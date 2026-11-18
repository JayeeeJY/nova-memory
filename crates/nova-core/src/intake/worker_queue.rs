//! Bounded traversal of encrypted worker jobs. No plaintext state index, model or HTTP tool.
//! A cursor is progress in a sweep, never source permission or a dispatch permit.
use super::{storage::*, *};
use rusqlite::{Connection, TransactionBehavior, params};
use time::{Duration, OffsetDateTime};

pub const MAX_WORKER_QUEUE_ROWS: usize = 128;
const PAGE: &str =
    "SELECT rowid,job_id FROM intake_requests WHERE rowid>?1 ORDER BY rowid LIMIT ?2";

/// Opaque in-process position bound to the encrypted store identity and Vault key.
/// Reopening the same store is supported; another initialized store is refused.
#[derive(Clone)]
pub struct WorkerQueueCursor {
    binding: String,
    after: i64,
}
impl std::fmt::Debug for WorkerQueueCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerQueueCursor([REDACTED])")
    }
}

pub struct WorkerQueueBatch {
    pub claimed: Option<IntakeJob>,
    /// Expired leases failed in this transaction; retry remains an explicit action.
    pub recovered: usize,
    /// Jobs inspected, including unreadable/revoked rows; never exceeds the limit.
    pub scanned: usize,
    pub model_configured: bool,
    /// Eligible jobs left unchanged by a trusted admission gate in this page.
    pub admission_deferred: usize,
    /// End of this sweep, not proof of an empty queue or globally recovered leases.
    pub complete: bool,
    pub next_cursor: Option<WorkerQueueCursor>,
}
impl std::fmt::Debug for WorkerQueueBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerQueueBatch")
            .field("claimed", &self.claimed.is_some())
            .field("recovered", &self.recovered)
            .field("scanned", &self.scanned)
            .field("model_configured", &self.model_configured)
            .field("admission_deferred", &self.admission_deferred)
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

fn binding(c: &Connection, key: &crate::VaultKey) -> Result<String> {
    let envelope: (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) = c.query_row(
        "SELECT key_nonce,wrapped_key,nonce,ciphertext FROM intake_objects WHERE id='identity' AND kind='identity'",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    )?;
    Ok(key.authentication_tag(&[
        b"nova-worker-queue-cursor-v1",
        &serde_json::to_vec(&envelope)?,
    ]))
}

impl IntakeStore {
    /// Trusted worker step. Selects at most `limit` request IDs using the existing
    /// rowid index, decrypts no more than that many jobs, recovers visited expired
    /// leases, and claims at most one eligible job in a single IMMEDIATE transaction.
    /// Never calls the legacy whole-store recover_expired/claim_next paths.
    ///
    /// Retain next_cursor even when claimed is None. Once complete, a later None
    /// starts a new sweep and revisits explicit retries/new inserts behind its position.
    /// A completed job's ciphertext is inspected at most once per sweep, not per tick.
    pub fn claim_next_batch(
        &mut self,
        scope: &AssetScope,
        lease_seconds: u32,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
    ) -> Result<WorkerQueueBatch> {
        self.claim_next_batch_at(
            scope,
            lease_seconds,
            cursor,
            limit,
            OffsetDateTime::now_utc(),
        )
    }

    /// Recovery-only worker step. Even a configured model cannot make this method
    /// claim a received/retried job. Does not restore quarantined backup authority.
    pub fn recover_expired_batch(
        &mut self,
        scope: &AssetScope,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
    ) -> Result<WorkerQueueBatch> {
        self.worker_queue_step_at(scope, None, cursor, limit, OffsetDateTime::now_utc())
    }

    /// Trusted short, local admission callback inside the Core claim transaction.
    /// False leaves this candidate's attempt/retry/lease unchanged and continues
    /// this bounded page. Errors roll back this page, including lease recovery.
    /// No callback is invoked for inaccessible, expired, or ineligible jobs.
    /// The caller must not perform network I/O or recursively use this store.
    pub fn claim_next_batch_admitted(
        &mut self,
        scope: &AssetScope,
        lease_seconds: u32,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
        admit: impl FnMut(&IntakeJob) -> Result<bool>,
    ) -> Result<WorkerQueueBatch> {
        self.worker_queue_admitted_at(
            scope,
            Some(lease_seconds),
            cursor,
            limit,
            OffsetDateTime::now_utc(),
            admit,
        )
    }

    fn claim_next_batch_at(
        &mut self,
        scope: &AssetScope,
        lease_seconds: u32,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
        at: OffsetDateTime,
    ) -> Result<WorkerQueueBatch> {
        self.worker_queue_step_at(scope, Some(lease_seconds), cursor, limit, at)
    }

    fn worker_queue_step_at(
        &mut self,
        scope: &AssetScope,
        lease_seconds: Option<u32>,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
        at: OffsetDateTime,
    ) -> Result<WorkerQueueBatch> {
        self.worker_queue_admitted_at(scope, lease_seconds, cursor, limit, at, |_| Ok(true))
    }

    fn worker_queue_admitted_at(
        &mut self,
        scope: &AssetScope,
        lease_seconds: Option<u32>,
        cursor: Option<&WorkerQueueCursor>,
        limit: usize,
        at: OffsetDateTime,
        mut admit: impl FnMut(&IntakeJob) -> Result<bool>,
    ) -> Result<WorkerQueueBatch> {
        self.check(scope)?;
        if lease_seconds.is_some_and(|n| !(1..=900).contains(&n))
            || !(1..=MAX_WORKER_QUEUE_ROWS).contains(&limit)
        {
            return Err(invalid("worker_queue_bounds"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        let binding = binding(&tx, &self.key)?;
        if cursor.is_some_and(|c| c.binding != binding) {
            return Err(denied());
        }
        let config: ModelConfiguration = load(&tx, &self.key, "configuration", "configuration")?;
        config.validate()?;
        let model_configured = config.selection != ModelSelection::Unconfigured;
        // Request IDs and row positions only. No full encrypted-job collection or OFFSET.
        let ids = {
            let mut q = tx.prepare(PAGE)?;
            q.query_map(
                params![cursor.map_or(i64::MIN, |c| c.after), limit as i64],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let selected = ids.len();
        let mut scanned = 0;
        let mut recovered = 0;
        let mut admission_deferred = 0;
        let mut after = cursor.map_or(i64::MIN, |c| c.after);
        let mut claimed = None;
        for (position, job_id) in ids {
            scanned += 1;
            after = position;
            if !super::consolidation::readable(&tx, &job_id)? {
                continue;
            }
            let mut record: IntakeJob = load(&tx, &self.key, "job", &job_id)?;
            if record.job.state == JobState::Processing {
                let lease = record.lease.as_ref().ok_or_else(conflict)?;
                if parse_time(&lease.expires_at)? <= at {
                    fail(&tx, &self.key, &mut record, "worker_interrupted")?;
                    recovered += 1;
                }
                continue;
            }
            if lease_seconds.is_none()
                || !model_configured
                || !record.retry_requested
                || record.job.attempt >= MAX_ATTEMPTS
                || !matches!(record.job.state, JobState::Received | JobState::Failed)
            {
                continue;
            }
            let source: SourceSnapshot =
                load(&tx, &self.key, "source", &record.job.source.object_id)?;
            if !admit(&record)? {
                admission_deferred += 1;
                continue;
            }
            let mut next = record.job.clone();
            next.state = JobState::Processing;
            next.attempt += 1;
            next.error_code = None;
            record.job.validate_successor(&next, &snapshot(source))?;
            record.job = next;
            record.retry_requested = false;
            record.lease = Some(WorkerLease {
                lease_id: id("lease"),
                expires_at: format_time(
                    at + Duration::seconds(i64::from(lease_seconds.ok_or_else(conflict)?)),
                )?,
                configuration_revision: config.revision,
            });
            event(&mut record, "worker_claimed")?;
            update(&tx, &self.key, "job", &job_id, &record)?;
            claimed = Some(record);
            break;
        }
        // An early claim leaves any remaining selected IDs for the next page. A
        // full page can require a later empty step; do not inspect a 129th row.
        let complete = scanned == selected && selected < limit;
        tx.commit()?;
        Ok(WorkerQueueBatch {
            claimed,
            recovered,
            scanned,
            model_configured,
            admission_deferred,
            complete,
            next_cursor: (!complete).then_some(WorkerQueueCursor { binding, after }),
        })
    }
}

#[cfg(test)]
#[path = "worker_queue_tests.rs"]
mod tests;
