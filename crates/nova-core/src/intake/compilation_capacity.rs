//! Encrypted output-space holds. Admission and release use the same transaction as core state.
//! Counts additional allocated SQLite pages, not payload size, disk quota or billable storage.
use super::*;

const ACTIVE: &str = "compilation_storage_hold";
const RELEASED: &str = "compilation_storage_release";
const MAX_ACTIVE: usize = 256;
pub const COMPILATION_STORAGE_CAPACITY: &str = "compilation_storage_capacity";

/// Trusted worker configuration. Never decoded from a capture or a model response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompilationStorageBudget(u64);
impl CompilationStorageBudget {
    pub fn new(bytes: u64) -> Result<Self> {
        if !(65536..=(1_u64 << 30)).contains(&bytes) || !bytes.is_multiple_of(65536) {
            return Err(invalid("compilation_storage_budget"));
        }
        Ok(Self(bytes))
    }
    pub fn bytes(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CompilationStorageStatus {
    pub active_reservations: usize,
    pub reserved_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hold {
    scope: AssetScope,
    job_id: String,
    attempt: u32,
    lease_tag: String,
    configuration_revision: u64,
    bytes: u64,
    source_ids: Vec<String>,
    reserved_at: String,
}

fn exhausted() -> crate::NovaError {
    crate::NovaError::PolicyDenied(COMPILATION_STORAGE_CAPACITY.into())
}
fn allocated(c: &Connection) -> Result<u64> {
    let pages: u64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let size: u64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    pages.checked_mul(size).ok_or_else(exhausted)
}
fn lease_tag(key: &VaultKey, lease: &str) -> String {
    key.authentication_tag(&[b"compilation-storage-lease-v1", lease.as_bytes()])
}
fn active(c: &Connection, key: &VaultKey, scope: &AssetScope) -> Result<Vec<(String, Hold)>> {
    // Prefix range uses the existing primary-key index; terminal history is never scanned.
    let prefix = physical_id(ACTIVE, "");
    let mut q = c.prepare(
        "SELECT id FROM intake_objects WHERE id>=?1 AND id<?2 AND kind=?3 ORDER BY id LIMIT ?4",
    )?;
    let ids = q
        .query_map(
            rusqlite::params![
                prefix,
                format!("{prefix}\u{10ffff}"),
                ACTIVE,
                MAX_ACTIVE + 1
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.len() > MAX_ACTIVE {
        return Err(exhausted());
    }
    ids.into_iter()
        .map(|physical| {
            let id = physical
                .strip_prefix(&prefix)
                .ok_or_else(conflict)?
                .to_owned();
            let hold: Hold = load(c, key, ACTIVE, &id)?;
            if hold.scope != *scope || CompilationStorageBudget::new(hold.bytes).is_err() {
                return Err(denied());
            }
            Ok((id, hold))
        })
        .collect()
}
fn status(c: &Connection, key: &VaultKey, scope: &AssetScope) -> Result<CompilationStorageStatus> {
    let holds = active(c, key, scope)?;
    let reserved_bytes = holds.iter().try_fold(0_u64, |n, (_, h)| {
        n.checked_add(h.bytes).ok_or_else(exhausted)
    })?;
    Ok(CompilationStorageStatus {
        active_reservations: holds.len(),
        reserved_bytes,
    })
}
fn matching(
    c: &Connection,
    key: &VaultKey,
    record: &IntakeJob,
    lease: &str,
) -> Result<Option<Hold>> {
    let hold: Option<Hold> = optional(c, key, ACTIVE, &attempt_id(record))?;
    if let Some(h) = &hold
        && (h.job_id != record.job.job_id
            || h.attempt != record.job.attempt
            || h.lease_tag != lease_tag(key, lease))
    {
        return Err(conflict());
    }
    Ok(hold)
}
fn release(c: &Connection, key: &VaultKey, id: &str, hold: &Hold, reason: &str) -> Result<()> {
    insert(
        c,
        key,
        RELEASED,
        id,
        &json!({"reservation": hold, "reason": reason, "released_at": now()?}),
    )?;
    // Only this internal active index is removed; the encrypted terminal receipt remains.
    super::super::consolidation::remove_record(c, ACTIVE, id)
}

impl IntakeStore {
    pub fn compilation_storage_status(
        &self,
        scope: &AssetScope,
    ) -> Result<CompilationStorageStatus> {
        self.check(scope)?;
        status(&self.connection, &self.key, scope)
    }
    /// The host must hold its cross-process asset lock and supply its durable growth ceiling.
    /// No network, monetary reservation or dispatch authorization occurs here.
    pub fn reserve_compilation_storage(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease: &str,
        budget: CompilationStorageBudget,
        growth_ceiling: u64,
    ) -> Result<()> {
        self.check(scope)?;
        if growth_ceiling == 0
            || growth_ceiling > (1_u64 << 40)
            || !growth_ceiling.is_multiple_of(65536)
        {
            return Err(invalid("compilation_storage_ceiling"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::super::recovery::require_automation(&tx, &self.key)?;
        require_enabled(&tx, &self.key)?;
        let record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        check_lease(&tx, &self.key, &record, lease)?;
        let a: CompilationAttempt =
            load(&tx, &self.key, "compilation_attempt", &attempt_id(&record))?;
        if a.dispatched_at.is_some() || a.lease_id != lease {
            return Err(conflict());
        }
        recheck_context(&tx, &self.key, scope, &a)?;
        if let Some(old) = matching(&tx, &self.key, &record, lease)? {
            if old.bytes != budget.0 || old.configuration_revision != a.configuration.revision {
                return Err(conflict());
            }
        } else {
            if exists(&tx, RELEASED, &attempt_id(&record))?
                || status(&tx, &self.key, scope)?.active_reservations >= MAX_ACTIVE
            {
                return Err(exhausted());
            }
            insert(
                &tx,
                &self.key,
                ACTIVE,
                &attempt_id(&record),
                &Hold {
                    scope: scope.clone(),
                    job_id: job_id.into(),
                    attempt: a.attempt,
                    lease_tag: lease_tag(&self.key, lease),
                    configuration_revision: a.configuration.revision,
                    bytes: budget.0,
                    source_ids: a
                        .context
                        .sources
                        .iter()
                        .map(|s| s.source.object_id.clone())
                        .collect(),
                    reserved_at: now()?,
                },
            )?;
        }
        if allocated(&tx)?
            .checked_add(status(&tx, &self.key, scope)?.reserved_bytes)
            .is_none_or(|n| n > growth_ceiling)
        {
            return Err(exhausted());
        }
        tx.commit()?;
        Ok(())
    }
    /// Reserved commit may use its own hold, never another attempt's. Does not authorize commit.
    pub fn compilation_commit_capacity(
        &self,
        scope: &AssetScope,
        job_id: &str,
        lease: &str,
        growth_ceiling: u64,
    ) -> Result<u64> {
        self.check(scope)?;
        let record: IntakeJob = load(&self.connection, &self.key, "job", job_id)?;
        let held = matching(&self.connection, &self.key, &record, lease)?
            .map(|h| h.bytes)
            .unwrap_or(0);
        let others = status(&self.connection, &self.key, scope)?
            .reserved_bytes
            .checked_sub(held)
            .ok_or_else(exhausted)?;
        let ceiling = growth_ceiling.checked_sub(others).ok_or_else(exhausted)?;
        let current = allocated(&self.connection)?;
        let limit = if held == 0 {
            ceiling
        } else {
            ceiling.min(current.checked_add(held).ok_or_else(exhausted)?)
        };
        if current > limit {
            return Err(exhausted());
        }
        Ok(limit)
    }
}

pub(super) fn before_commit(
    c: &Connection,
    key: &VaultKey,
    record: &IntakeJob,
    lease: &str,
) -> Result<Option<u64>> {
    matching(c, key, record, lease)?
        .map(|h| allocated(c)?.checked_add(h.bytes).ok_or_else(exhausted))
        .transpose()
}
pub(super) fn committed(
    c: &Connection,
    key: &VaultKey,
    record: &IntakeJob,
    lease: &str,
    limit: Option<u64>,
) -> Result<()> {
    if let Some(hold) = matching(c, key, record, lease)? {
        release(c, key, &attempt_id(record), &hold, "committed")?;
        if allocated(c)? > limit.ok_or_else(conflict)? {
            return Err(exhausted());
        }
    }
    Ok(())
}
pub(crate) fn failed(c: &Connection, key: &VaultKey, record: &IntakeJob) -> Result<()> {
    if let Some(hold) = optional::<Hold>(c, key, ACTIVE, &attempt_id(record))? {
        release(c, key, &attempt_id(record), &hold, "failed_and_fenced")?;
    }
    Ok(())
}
pub(crate) fn withdrawn(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    source: &str,
) -> Result<()> {
    for (id, hold) in active(c, key, scope)? {
        if hold.source_ids.iter().any(|id| id == source) {
            release(c, key, &id, &hold, "source_withdrawn_and_fenced")?;
        }
    }
    Ok(())
}
