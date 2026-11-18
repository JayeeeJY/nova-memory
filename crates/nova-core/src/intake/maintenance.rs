//! Bounded, local-only maintenance. Plans are persisted separately from model jobs.
//! Originals, revisions, corrections, audit records and authority are never eviction targets.
use super::{compiler::read_unit, consolidation, storage::*, *};
use crate::VaultKey;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use time::{Duration, OffsetDateTime};

pub const MAINTENANCE_VERSION: &str = "nova.maintenance.v0.1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePolicy {
    pub max_rebuild_pages: usize,
    pub max_prune_pages: usize,
    /// Applies only to rebuildable Wiki pages, never Sources or semantic revisions.
    pub derived_ttl_seconds: u32,
    /// Opaque Owner pagination cursor; no unbounded background scan.
    pub prune_before: Option<i64>,
    #[serde(default)]
    pub include_cold: bool,
}
impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            max_rebuild_pages: 4,
            max_prune_pages: 16,
            derived_ttl_seconds: 604800,
            prune_before: None,
            include_cold: false,
        }
    }
}
impl MaintenancePolicy {
    fn validate(&self) -> Result<()> {
        if self.max_rebuild_pages > 4
            || self.max_prune_pages > 16
            || self.max_rebuild_pages + self.max_prune_pages == 0
            || !(86400..=31536000).contains(&self.derived_ttl_seconds)
            || self.prune_before.is_some_and(|n| n <= 0)
        {
            return Err(invalid("maintenance_budget"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceBucket {
    pub id: String,
    pub generation: u64,
    #[serde(default)]
    pub queue_state: u8,
    pub input_fingerprint: String,
    /// Tuples are metadata, not Source-dependent cached content. Stale/cancel receipts remain visible.
    pub input_versions: Vec<(String, u64)>,
    pub blocked: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceEviction {
    pub id: String,
    pub generation: u64,
    pub updated_at: String,
    pub fingerprint: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenancePreview {
    pub schema_version: String,
    pub rules_version: String,
    pub policy: MaintenancePolicy,
    pub created_at: String,
    pub expires_at: String,
    pub rebuild: Vec<MaintenanceBucket>,
    pub evict: Vec<MaintenanceEviction>,
    pub next_prune_before: Option<i64>,
    pub model_calls: u32,
    pub originals_deleted: u32,
    pub interpretation: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceState {
    Pending,
    Cancelled,
    Stale,
    Completed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceJob {
    pub id: String,
    pub scope: AssetScope,
    pub state: MaintenanceState,
    pub preview: MaintenancePreview,
    pub updated_at: String,
    pub reason: String,
    pub pages_rebuilt: usize,
    pub pages_pruned: usize,
    pub pages_blocked: usize,
}
fn bucket(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    id: &str,
) -> Result<Option<MaintenanceBucket>> {
    let row: Option<(u64, u8)> = c
        .query_row(
            "SELECT generation,blocked FROM intake_dirty WHERE bucket=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((generation, queue_state @ (0 | 3))) = row else {
        return Ok(None);
    };
    let ids = {
        let mut q = c.prepare(
            "SELECT unit_id FROM intake_related WHERE bucket=?1 ORDER BY unit_id LIMIT 65",
        )?;
        q.query_map([id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let blocked = ids.len() > consolidation::MAX_RELATED_UNITS;
    let mut versions = Vec::new();
    let mut exact = Vec::new();
    if !blocked {
        for id in &ids {
            let u = read_unit(c, key, scope, id)?;
            versions.push((
                u.unit.header.object.object_id.clone(),
                u.unit.header.object.revision,
            ));
            versions.extend(
                u.unit
                    .evidence
                    .iter()
                    .map(|e| (e.source.object_id.clone(), e.source.revision)),
            );
            exact.push(u);
        }
    }
    versions.sort();
    versions.dedup();
    let input_fingerprint = key.authentication_tag(&[
        b"maintenance:bucket:v1",
        &serde_json::to_vec(&(
            id,
            generation,
            queue_state,
            &ids,
            &exact,
            consolidation::CONSOLIDATION_RULES,
        ))?,
    ]);
    Ok(Some(MaintenanceBucket {
        id: id.into(),
        generation,
        queue_state,
        input_fingerprint,
        input_versions: versions,
        blocked,
    }))
}
fn eviction(key: &VaultKey, page: &WikiPage) -> Result<MaintenanceEviction> {
    Ok(MaintenanceEviction {
        id: page.id.clone(),
        generation: page.generation,
        updated_at: page.updated_at.clone(),
        fingerprint: key.authentication_tag(&[b"maintenance:wiki:v1", &serde_json::to_vec(page)?]),
    })
}
fn preview(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    policy: MaintenancePolicy,
) -> Result<MaintenancePreview> {
    policy.validate()?;
    let at = OffsetDateTime::now_utc();
    let mut out = MaintenancePreview {
        schema_version: MAINTENANCE_VERSION.into(),
        rules_version: consolidation::CONSOLIDATION_RULES.into(),
        policy: policy.clone(),
        created_at: format_time(at)?,
        expires_at: format_time(at + Duration::hours(1))?,
        rebuild: vec![],
        evict: vec![],
        next_prune_before: None,
        model_calls: 0,
        originals_deleted: 0,
        interpretation:
            "deterministic_local_derived_pages_only; no_semantic_quality_or_source_retention_change"
                .into(),
    };
    if !consolidation::available(c)? {
        return Ok(out);
    }
    let ids = {
        let mut q = c.prepare("SELECT bucket FROM intake_dirty WHERE blocked=0 OR (?2=1 AND blocked=3) ORDER BY blocked,rowid LIMIT ?1")?;
        q.query_map(
            rusqlite::params![policy.max_rebuild_pages, policy.include_cold],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in ids {
        if let Some(b) = bucket(c, key, scope, &id)? {
            out.rebuild.push(b);
        }
    }
    let rows = {
        let mut q=c.prepare("SELECT rowid,id FROM intake_objects WHERE kind='wiki' AND rowid<?1 ORDER BY rowid DESC LIMIT ?2")?;
        q.query_map(
            rusqlite::params![
                policy.prune_before.unwrap_or(i64::MAX),
                policy.max_prune_pages
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if policy.max_prune_pages > 0 && rows.len() == policy.max_prune_pages {
        out.next_prune_before = rows.last().map(|r| r.0);
    }
    for (_, physical) in rows {
        if !consolidation::readable(c, &physical)? {
            continue;
        }
        let id = physical
            .strip_prefix(&physical_id("wiki", ""))
            .ok_or_else(conflict)?;
        let page: WikiPage = load(c, key, "wiki", id)?;
        if page.scope != *scope {
            return Err(denied());
        }
        if parse_time(&page.updated_at)? + Duration::seconds(policy.derived_ttl_seconds.into())
            <= at
        {
            out.evict.push(eviction(key, &page)?);
        }
    }
    Ok(out)
}
/// An unbounded full-store scan is never used to declare idle. Large stores require a future
/// indexed job-state implementation and return an explicit unknown/deferred status here.
fn idle(c: &Connection, key: &VaultKey) -> Result<Option<&'static str>> {
    let ids = {
        let mut q = c.prepare(
            "SELECT id FROM intake_objects WHERE kind='job' ORDER BY rowid DESC LIMIT 129",
        )?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if ids.len() > 128 {
        return Ok(Some("idle_unknown_scan_limit"));
    }
    for physical in ids {
        if !consolidation::readable(c, &physical)? {
            continue;
        }
        let id = physical
            .strip_prefix(&physical_id("job", ""))
            .ok_or_else(conflict)?;
        let j: IntakeJob = load(c, key, "job", id)?;
        if j.job.state == JobState::Processing {
            return Ok(Some("compiler_busy"));
        }
    }
    Ok(None)
}
impl IntakeStore {
    /// Read-only dry-run. No table, job, model dispatch or authority is created.
    pub fn preview_maintenance(
        &self,
        scope: &AssetScope,
        policy: MaintenancePolicy,
    ) -> Result<MaintenancePreview> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        preview(&tx, &self.key, scope, policy)
    }
    pub fn schedule_maintenance(
        &mut self,
        scope: &AssetScope,
        request_id: &str,
        policy: MaintenancePolicy,
    ) -> Result<MaintenanceJob> {
        self.check(scope)?;
        policy.validate()?;
        if !identifier(request_id) {
            return Err(invalid("maintenance_request"));
        }
        let id = self
            .key
            .authentication_tag(&[b"maintenance:request:v1", request_id.as_bytes()]);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = optional::<MaintenanceJob>(&tx, &self.key, "maintenance_job", &id)?
        {
            if previous.preview.policy != policy {
                return Err(conflict());
            }
            return Ok(previous);
        }
        let count: u64 = tx.query_row(
            "SELECT count(*) FROM intake_objects WHERE kind='maintenance_job'",
            [],
            |r| r.get(0),
        )?;
        if count >= 256 {
            return Err(invalid("maintenance_job_limit"));
        }
        let preview = preview(&tx, &self.key, scope, policy)?;
        let out = MaintenanceJob {
            id: id.clone(),
            scope: scope.clone(),
            state: MaintenanceState::Pending,
            updated_at: preview.created_at.clone(),
            preview,
            reason: "scheduled_explicitly".into(),
            pages_rebuilt: 0,
            pages_pruned: 0,
            pages_blocked: 0,
        };
        insert(&tx, &self.key, "maintenance_job", &id, &out)?;
        tx.commit()?;
        Ok(out)
    }
    pub fn maintenance_job(&self, scope: &AssetScope, id: &str) -> Result<MaintenanceJob> {
        self.check(scope)?;
        load(&self.connection, &self.key, "maintenance_job", id)
    }
    pub fn maintenance_jobs(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<Vec<(i64, MaintenanceJob)>> {
        self.check(scope)?;
        let mut q=self.connection.prepare("SELECT rowid,id FROM intake_objects WHERE kind='maintenance_job' AND rowid<?1 ORDER BY rowid DESC LIMIT 50")?;
        let rows = q
            .query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(seq, id)| {
                Ok((
                    seq,
                    load(
                        &self.connection,
                        &self.key,
                        "maintenance_job",
                        id.strip_prefix(&physical_id("maintenance_job", ""))
                            .ok_or_else(conflict)?,
                    )?,
                ))
            })
            .collect()
    }
    pub fn cancel_maintenance(&mut self, scope: &AssetScope, id: &str) -> Result<MaintenanceJob> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job: MaintenanceJob = load(&tx, &self.key, "maintenance_job", id)?;
        if job.state == MaintenanceState::Pending {
            job.state = MaintenanceState::Cancelled;
            job.reason = "owner_cancelled".into();
            job.updated_at = now()?;
            update(&tx, &self.key, "maintenance_job", id, &job)?;
        }
        tx.commit()?;
        Ok(job)
    }
    /// One bounded atomic transaction, only while the compiler is known idle. Repeated calls return
    /// the same terminal receipt. Cancellation races serialize before/after commit; no half result.
    pub fn run_maintenance_when_idle(
        &mut self,
        scope: &AssetScope,
        id: &str,
    ) -> Result<MaintenanceJob> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job: MaintenanceJob = load(&tx, &self.key, "maintenance_job", id)?;
        if job.state != MaintenanceState::Pending {
            return Ok(job);
        }
        let at = OffsetDateTime::now_utc();
        if parse_time(&job.preview.expires_at)? <= at
            || parse_time(&job.preview.created_at)? > at
            || job.preview.rules_version != consolidation::CONSOLIDATION_RULES
        {
            job.state = MaintenanceState::Stale;
            job.reason = "plan_expired_or_rules_changed".into();
        } else if let Some(reason) = idle(&tx, &self.key)? {
            job.reason = reason.into();
        } else {
            let mut valid = true;
            for b in &job.preview.rebuild {
                if bucket(&tx, &self.key, scope, &b.id)?.as_ref() != Some(b) {
                    valid = false;
                    break;
                }
            }
            for e in &job.preview.evict {
                let page = optional::<WikiPage>(&tx, &self.key, "wiki", &e.id)?;
                if page
                    .as_ref()
                    .map(|p| eviction(&self.key, p))
                    .transpose()?
                    .as_ref()
                    != Some(e)
                {
                    valid = false;
                    break;
                }
            }
            if !valid {
                job.state = MaintenanceState::Stale;
                job.reason = "input_versions_changed".into();
            } else {
                for b in &job.preview.rebuild {
                    if b.blocked {
                        tx.execute("UPDATE intake_dirty SET blocked=1 WHERE bucket=?1", [&b.id])?;
                        job.pages_blocked += 1;
                        continue;
                    }
                    let units = b
                        .input_versions
                        .iter()
                        .filter(|(id, _)| id.starts_with("unit_"))
                        .map(|(id, _)| read_unit(&tx, &self.key, scope, id))
                        .collect::<Result<Vec<_>>>()?;
                    if !units.is_empty() {
                        let page = consolidation::build_page(
                            &tx,
                            &self.key,
                            scope,
                            &b.id,
                            b.generation,
                            units,
                        )?;
                        if serde_json::to_vec(&page)?.len() > 512 * 1024 {
                            tx.execute(
                                "UPDATE intake_dirty SET blocked=1 WHERE bucket=?1",
                                [&b.id],
                            )?;
                            job.pages_blocked += 1;
                            continue;
                        }
                        insert(&tx, &self.key, "wiki", &b.id, &page)?;
                    }
                    tx.execute("UPDATE intake_dirty SET blocked=2 WHERE bucket=?1", [&b.id])?;
                    job.pages_rebuilt += 1;
                }
                for e in &job.preview.evict {
                    consolidation::remove_record(&tx, "wiki", &e.id)?;
                    // Cold derived pages are not immediately requeued. Explicit rebuild or new
                    // input dirties them normally; original units remain directly retrievable.
                    tx.execute("UPDATE intake_dirty SET blocked=3 WHERE bucket=?1", [&e.id])?;
                    job.pages_pruned += 1;
                }
                job.state = MaintenanceState::Completed;
                job.reason = "bounded_local_commit".into();
            }
        }
        job.updated_at = now()?;
        update(&tx, &self.key, "maintenance_job", id, &job)?;
        tx.commit()?;
        Ok(job)
    }
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
