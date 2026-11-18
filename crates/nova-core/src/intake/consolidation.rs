//! N2b: bounded, deterministic, rebuildable evidence views. No LLM or authority expansion.
use super::{compiler::*, storage::*, *};
use crate::VaultKey;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const CONSOLIDATION_RULES: &str = "n2b-exact-evidence-v1";
pub const MAX_RELATED_UNITS: usize = 64;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedMemory {
    pub id: String,
    pub content: SemanticContent,
    pub origin: SemanticOrigin,
    pub valid_time: ValidTime,
    pub occurred_at: Option<String>,
    pub source_roles: Vec<String>,
    pub privacy_level: PrivacyLevel,
    pub members: Vec<RevisionRef>,
    pub protected_members: Vec<RevisionRef>,
    pub evidence: Vec<EvidenceSpan>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAssociation {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub interpretation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    pub id: String,
    pub scope: AssetScope,
    pub generation: u64,
    pub rules_version: String,
    pub updated_at: String,
    pub subject: String,
    pub predicate: String,
    pub inputs: Vec<RevisionRef>,
    pub memories: Vec<ConsolidatedMemory>,
    pub associations: Vec<LocalAssociation>,
    pub semantic_support: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationQueue {
    pub pending: u64,
    pub blocked: u64,
    /// Rebuildable pages evicted by the explicit maintenance retention policy.
    pub cold: u64,
}
// HMAC indices contain no subjects, values or quotes. All view payloads remain envelope encrypted.
pub(super) fn available(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='intake_dependencies'",
        [],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}
pub(super) fn initialize(c: &Connection, key: &VaultKey) -> Result<()> {
    if available(c)? {
        return Ok(());
    }
    let count: i64 = c.query_row("SELECT count(*) FROM intake_objects", [], |r| r.get(0))?;
    if count > 4096 {
        return Err(invalid("n2b_explicit_batched_migration_required"));
    }
    c.execute_batch("CREATE TABLE intake_dependencies(record_id TEXT NOT NULL, dependency TEXT NOT NULL, PRIMARY KEY(record_id,dependency));
        CREATE INDEX intake_dependency_lookup ON intake_dependencies(dependency);
        CREATE TABLE intake_revocations(dependency TEXT PRIMARY KEY);
        CREATE TABLE intake_retired_requests(binding TEXT PRIMARY KEY);
        CREATE TABLE intake_related(unit_id TEXT PRIMARY KEY,bucket TEXT NOT NULL);
        CREATE INDEX intake_related_bucket ON intake_related(bucket);
        CREATE TABLE intake_dirty(bucket TEXT PRIMARY KEY,generation INTEGER NOT NULL,blocked INTEGER NOT NULL DEFAULT 0);")?;
    let rows = {
        let mut q = c.prepare("SELECT id,kind FROM intake_objects")?;
        q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let withdrawn = rows
        .iter()
        .filter(|(_, k)| k == "source_withdrawal")
        .map(|(p, k)| logical_id(k, p))
        .collect::<Vec<_>>();
    for (physical, kind) in rows {
        let logical = logical_id(&kind, &physical);
        let value: Value = load(c, key, &kind, &logical)?;
        track(c, key, &kind, &logical, &value)?;
        if kind == "unit_head" {
            let head: Value = load(c, key, "unit_head", &logical)?;
            let unit: MemoryUnit = load(
                c,
                key,
                "unit_revision",
                &revision_id(&RevisionRef {
                    object_id: logical,
                    revision: head["revision"].as_u64().ok_or_else(conflict)?,
                }),
            )?;
            register(c, key, &unit)?;
        }
    }
    for id in withdrawn {
        let _ = invalidate(c, key, &id)?;
    }
    Ok(())
}
fn logical_id(kind: &str, physical: &str) -> String {
    physical
        .strip_prefix(&physical_id(kind, ""))
        .unwrap_or(physical)
        .to_string()
}
fn token(key: &VaultKey, id: &str) -> String {
    key.authentication_tag(&[b"n2b:dependency", id.as_bytes()])
}
fn refs(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::Object(m) => {
            if m.len() == 2
                && m.get("revision").and_then(Value::as_u64).is_some()
                && let Some(Value::String(id)) = m.get("object_id")
            {
                ids.insert(id.clone());
            }
            // Arbitrary semantic JSON is data, never a dependency or control instruction.
            for (name, v) in m {
                if !matches!(name.as_str(), "content" | "extensions") {
                    refs(v, ids);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                refs(v, ids);
            }
        }
        _ => {}
    }
}
pub(super) fn track<T: Serialize>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    if !available(c)? || kind == "connection" {
        return Ok(());
    }
    let mut ids = BTreeSet::new();
    refs(&serde_json::to_value(value)?, &mut ids);
    if matches!(kind, "unit_head" | "job" | "model_response_hash") {
        ids.insert(id.into());
    }
    for id in &ids {
        if revoked(c, key, id)? {
            return Err(denied());
        }
    }
    let physical = physical_id(kind, id);
    c.execute(
        "DELETE FROM intake_dependencies WHERE record_id=?1",
        [&physical],
    )?;
    for id in ids {
        c.execute(
            "INSERT INTO intake_dependencies VALUES (?1,?2)",
            params![physical, token(key, &id)],
        )?;
    }
    Ok(())
}
pub(super) fn revoked(c: &Connection, key: &VaultKey, id: &str) -> Result<bool> {
    if !available(c)? {
        return Ok(false);
    }
    Ok(c.query_row(
        "SELECT 1 FROM intake_revocations WHERE dependency=?1",
        [token(key, id)],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}
pub(super) fn readable(c: &Connection, physical: &str) -> Result<bool> {
    if !available(c)? {
        return Ok(true);
    }
    Ok(!c.query_row("SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id=?1 LIMIT 1",[physical],|_|Ok(())).optional()?.is_some())
}
fn bucket(key: &VaultKey, u: &MemoryUnit) -> Result<String> {
    Ok(key.authentication_tag(&[
        b"n2b:bucket",
        &serde_json::to_vec(&(&u.header.scope, &u.content.subject, &u.content.predicate))?,
    ]))
}
pub(super) fn remove_record(c: &Connection, kind: &str, id: &str) -> Result<()> {
    let physical = physical_id(kind, id);
    c.execute(
        "DELETE FROM intake_dependencies WHERE record_id=?1",
        [&physical],
    )?;
    c.execute("DELETE FROM intake_objects WHERE id=?1", [&physical])?;
    Ok(())
}
fn dirty(c: &Connection, b: &str) -> Result<()> {
    // Never return an old cache while a replacement is queued.
    remove_record(c, "wiki", b)?;
    c.execute("INSERT INTO intake_dirty(bucket,generation) VALUES (?1,1) ON CONFLICT(bucket) DO UPDATE SET generation=generation+1,blocked=0",[b])?;
    Ok(())
}
pub(super) fn register(c: &Connection, key: &VaultKey, u: &MemoryUnit) -> Result<()> {
    if !available(c)? {
        return Ok(());
    }
    let id = &u.header.object.object_id;
    let old: Option<String> = c
        .query_row(
            "SELECT bucket FROM intake_related WHERE unit_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        dirty(c, &old)?;
    }
    let b = bucket(key, u)?;
    c.execute("INSERT INTO intake_related VALUES (?1,?2) ON CONFLICT(unit_id) DO UPDATE SET bucket=excluded.bucket",params![id,b])?;
    dirty(c, &b)
}
impl IntakeStore {
    /// Persistent deterministic queue; at most four partitions of 64 current units per call.
    pub fn consolidate_pending(
        &mut self,
        scope: &AssetScope,
        maximum_pages: usize,
    ) -> Result<usize> {
        self.check(scope)?;
        if self.backup_recovery_manifest(scope)?.is_some() { return Ok(0); }
        if !(1..=4).contains(&maximum_pages) {
            return Err(invalid("consolidation_budget"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !available(&tx)? {
            return Ok(0);
        }
        let buckets = {
            let mut q=tx.prepare("SELECT bucket,generation FROM intake_dirty WHERE blocked=0 ORDER BY rowid LIMIT ?1")?;
            q.query_map([maximum_pages], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut completed = 0;
        for (b, generation) in buckets {
            let ids = {
                let mut q = tx.prepare(
                    "SELECT unit_id FROM intake_related WHERE bucket=?1 ORDER BY unit_id LIMIT 65",
                )?;
                q.query_map([&b], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            if ids.len() > MAX_RELATED_UNITS {
                tx.execute("UPDATE intake_dirty SET blocked=1 WHERE bucket=?1", [&b])?;
                continue;
            }
            let units = ids
                .iter()
                .map(|id| read_unit(&tx, &self.key, scope, id))
                .collect::<Result<Vec<_>>>()?;
            if !units.is_empty() {
                let page = build_page(&tx, &self.key, scope, &b, generation, units)?;
                if serde_json::to_vec(&page)?.len() > 512 * 1024 {
                    tx.execute("UPDATE intake_dirty SET blocked=1 WHERE bucket=?1", [&b])?;
                    continue;
                }
                insert(&tx, &self.key, "wiki", &b, &page)?;
            }
            tx.execute("UPDATE intake_dirty SET blocked=2 WHERE bucket=?1", [&b])?;
            completed += 1;
        }
        tx.commit()?;
        Ok(completed)
    }
    pub fn rebuild_wiki_page(&mut self, scope: &AssetScope, page_id: &str) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !available(&tx)?
            || tx
                .query_row(
                    "SELECT 1 FROM intake_dirty WHERE bucket=?1",
                    [page_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_none()
        {
            return Err(denied());
        }
        dirty(&tx, page_id)?;
        tx.commit()?;
        Ok(())
    }
    pub fn consolidation_queue(&self, scope: &AssetScope) -> Result<ConsolidationQueue> {
        self.check(scope)?;
        if !available(&self.connection)? {
            return Ok(ConsolidationQueue {
                pending: 0,
                blocked: 0,
                cold: 0,
            });
        }
        Ok(ConsolidationQueue {
            pending: self.connection.query_row(
                "SELECT count(*) FROM intake_dirty WHERE blocked=0",
                [],
                |r| r.get(0),
            )?,
            blocked: self.connection.query_row(
                "SELECT count(*) FROM intake_dirty WHERE blocked=1",
                [],
                |r| r.get(0),
            )?,
            cold: self.connection.query_row(
                "SELECT count(*) FROM intake_dirty WHERE blocked=3",
                [],
                |r| r.get(0),
            )?,
        })
    }
    pub fn wiki_pages(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<Vec<(i64, WikiPage)>> {
        self.check(scope)?;
        let rows = {
            let mut q=self.connection.prepare("SELECT rowid,id FROM intake_objects WHERE kind='wiki' AND rowid<?1 ORDER BY rowid DESC LIMIT 50")?;
            q.query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut pages = Vec::new();
        for (seq, id) in rows {
            if !readable(&self.connection, &id)? {
                continue;
            }
            let page: WikiPage = load(
                &self.connection,
                &self.key,
                "wiki",
                &logical_id("wiki", &id),
            )?;
            if page.scope != *scope {
                return Err(denied());
            }
            // Revalidate every exact input revision; stale caches fail closed even after process restart.
            for r in &page.inputs {
                if r.object_id.starts_with("source_") {
                    let _ = load_source(&self.connection, &self.key, scope, r)?;
                } else if read_unit(&self.connection, &self.key, scope, &r.object_id)?
                    .unit
                    .header
                    .object
                    != *r
                {
                    return Err(conflict());
                }
            }
            pages.push((seq, page));
        }
        Ok(pages)
    }
    /// Withdraw immediately; deletion additionally removes all live-store envelopes containing the
    /// source or any version of a dependent unit. Old backups/snapshots require separate destruction.
    pub fn withdrawn_sources(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<Vec<(i64, String, String)>> {
        self.check(scope)?;
        let rows = {
            let mut q=self.connection.prepare("SELECT rowid,id FROM intake_objects WHERE kind='source_withdrawal' AND rowid<?1 AND EXISTS (SELECT 1 FROM intake_objects s WHERE s.kind='source' AND s.id=substr(intake_objects.id,length('n2a:source_withdrawal:')+1)) ORDER BY rowid DESC LIMIT 50")?;
            q.query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut items = vec![];
        for (seq, p) in rows {
            let id = logical_id("source_withdrawal", &p);
            if exists(&self.connection, "source", &id)? {
                let at = load(&self.connection, &self.key, "source_withdrawal", &id)?;
                items.push((seq, id, at));
            }
        }
        Ok(items)
    }
    pub fn delete_source(&mut self, scope: &AssetScope, source_id: &str) -> Result<usize> {
        self.retire_source(scope, source_id, true)
    }
    pub(super) fn retire_source(
        &mut self,
        scope: &AssetScope,
        source_id: &str,
        erase: bool,
    ) -> Result<usize> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        initialize(&tx, &self.key)?;
        if !revoked(&tx, &self.key, source_id)? {
            let _: SourceSnapshot = load(&tx, &self.key, "source", source_id)?;
        }
        let targets = serde_json::to_string(&invalidate(&tx, &self.key, source_id)?)?;
        super::compiler::storage_capacity::withdrawn(&tx, &self.key, scope, source_id)?;
        let mut deleted = 0;
        if erase {
            // Keep only keyed request tombstones, preventing identical request IDs from reimporting.
            tx.execute("INSERT OR IGNORE INTO intake_retired_requests SELECT binding FROM intake_requests WHERE job_id IN (SELECT record_id FROM intake_dependencies WHERE dependency IN (SELECT value FROM json_each(?1)))",[&targets])?;
            tx.execute("DELETE FROM intake_requests WHERE job_id IN (SELECT record_id FROM intake_dependencies WHERE dependency IN (SELECT value FROM json_each(?1)))",[&targets])?;
            deleted=tx.execute("DELETE FROM intake_objects WHERE id IN (SELECT record_id FROM intake_dependencies WHERE dependency IN (SELECT value FROM json_each(?1)))",[&targets])?;
            tx.execute("DELETE FROM intake_dependencies WHERE record_id NOT IN (SELECT id FROM intake_objects)",[])?;
        }
        tx.commit()?;
        if erase {
            let busy: i64 =
                self.connection
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
            if busy != 0 {
                return Err(invalid("erasure_checkpoint_busy_retry_required"));
            }
        }
        Ok(deleted)
    }
}
pub(super) fn exact_key(
    key: &VaultKey,
    u: &MemoryUnit,
    roles: &BTreeSet<String>,
) -> Result<String> {
    Ok(key.authentication_tag(&[
        b"n2b:exact",
        &serde_json::to_vec(&(
            &u.header.scope,
            roles,
            &u.content,
            &u.origin,
            &u.valid_time,
            &u.occurred_at,
            &u.header.privacy_level,
            &u.header.state,
        ))?,
    ]))
}
pub(super) fn build_page(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    b: &str,
    generation: u64,
    units: Vec<CompiledUnit>,
) -> Result<WikiPage> {
    let mut groups: BTreeMap<String, ConsolidatedMemory> = BTreeMap::new();
    let mut inputs = BTreeSet::new();
    let subject = units[0].unit.content.subject.clone();
    let predicate = units[0].unit.content.predicate.clone();
    for item in units {
        let u = item.unit;
        inputs.insert((u.header.object.object_id.clone(), u.header.object.revision));
        for e in &u.evidence {
            inputs.insert((e.source.object_id.clone(), e.source.revision));
        }
        let roles = u
            .evidence
            .iter()
            .map(|e| {
                let source = load_source(c, key, scope, &e.source)?;
                Ok(serde_json::to_value(source.role)?
                    .as_str()
                    .ok_or_else(conflict)?
                    .to_string())
            })
            .collect::<Result<BTreeSet<String>>>()?;
        let id = exact_key(key, &u, &roles)?;
        let group = groups
            .entry(id.clone())
            .or_insert_with(|| ConsolidatedMemory {
                id,
                content: u.content.clone(),
                origin: u.origin,
                valid_time: u.valid_time.clone(),
                occurred_at: u.occurred_at.clone(),
                source_roles: roles.into_iter().collect(),
                privacy_level: u.header.privacy_level,
                members: vec![],
                protected_members: vec![],
                evidence: vec![],
            });
        group.members.push(u.header.object.clone());
        if item.protected {
            group.protected_members.push(u.header.object);
        }
        for ev in u.evidence {
            if !group.evidence.contains(&ev) {
                group.evidence.push(ev);
            }
        }
    }
    let memories = groups.into_values().collect::<Vec<_>>();
    let mut associations = vec![];
    for (i, a) in memories.iter().enumerate() {
        for b in memories.iter().skip(i + 1) {
            let (kind, interpretation) = if a.content == b.content {
                (
                    "parallel_evidence",
                    "Matching content with distinct provenance, privacy or time; retained separately, not a contradiction.",
                )
            } else if a.content.conditions != b.content.conditions {
                (
                    "conditional_variant",
                    "Conditions differ; no global replacement is inferred.",
                )
            } else if disjoint(&a.valid_time, &b.valid_time) {
                (
                    "temporal_transition",
                    "Explicit validity intervals do not overlap; both versions are retained.",
                )
            } else {
                (
                    "potential_conflict",
                    "Structured claims differ; this is a candidate discrepancy, not a verified contradiction.",
                )
            };
            let (from, to) = if kind == "temporal_transition" {
                match (&a.valid_time, &b.valid_time) {
                    (
                        ValidTime::Interval { start: a_start, .. },
                        ValidTime::Interval { start: b_start, .. },
                    ) if parse_time(a_start)? > parse_time(b_start)? => (&b.id, &a.id),
                    _ => (&a.id, &b.id),
                }
            } else {
                (&a.id, &b.id)
            };
            associations.push(LocalAssociation {
                from: from.clone(),
                to: to.clone(),
                kind: kind.into(),
                interpretation: interpretation.into(),
            });
        }
    }
    Ok(WikiPage {
        id: b.into(),
        scope: scope.clone(),
        generation,
        rules_version: CONSOLIDATION_RULES.into(),
        updated_at: now()?,
        subject,
        predicate,
        inputs: inputs
            .into_iter()
            .map(|(object_id, revision)| RevisionRef {
                object_id,
                revision,
            })
            .collect(),
        memories,
        associations,
        semantic_support: "structural_associations_model_claims_unverified".into(),
    })
}
fn disjoint(a: &ValidTime, b: &ValidTime) -> bool {
    if let (ValidTime::Interval { start: a, end: ae }, ValidTime::Interval { start: b, end: be }) =
        (a, b)
    {
        return ae.as_ref().is_some_and(|e| {
            parse_time(e)
                .ok()
                .zip(parse_time(b).ok())
                .is_some_and(|(e, s)| e <= s)
        }) || be.as_ref().is_some_and(|e| {
            parse_time(e)
                .ok()
                .zip(parse_time(a).ok())
                .is_some_and(|(e, s)| e <= s)
        });
    }
    false
}

fn invalidate(c: &Connection, key: &VaultKey, source_id: &str) -> Result<Vec<String>> {
    // Any retained revision can contain the withdrawn source. Retire its entire unit lineage.
    let affected = {
        let mut q=c.prepare("SELECT o.id FROM intake_objects o JOIN intake_dependencies d ON o.id=d.record_id WHERE o.kind='unit_revision' AND d.dependency=?1")?;
        q.query_map([token(key, source_id)], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut revoked_ids = BTreeSet::from([source_id.to_string()]);
    for p in affected {
        let logical = logical_id("unit_revision", &p);
        let (dek, blob) = data(c, key, "unit_revision", &logical)?;
        let unit: MemoryUnit =
            serde_json::from_slice(&dek.decrypt(&blob, &aad("unit_revision", &logical))?)?;
        revoked_ids.insert(unit.header.object.object_id);
    }
    for id in &revoked_ids {
        let old: Option<String> = c
            .query_row(
                "SELECT bucket FROM intake_related WHERE unit_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(b) = old {
            c.execute("DELETE FROM intake_related WHERE unit_id=?1", [id])?;
            dirty(c, &b)?;
        }
        c.execute(
            "INSERT OR IGNORE INTO intake_revocations VALUES (?1)",
            [token(key, id)],
        )?;
    }
    if !exists(c, "source_withdrawal", source_id)? {
        insert(c, key, "source_withdrawal", source_id, &now()?)?;
    }
    let mut targets = revoked_ids
        .iter()
        .map(|id| token(key, id))
        .collect::<Vec<_>>();
    let serialized = serde_json::to_string(&targets)?;
    let jobs = {
        let mut q=c.prepare("SELECT o.id FROM intake_objects o JOIN intake_dependencies d ON o.id=d.record_id WHERE o.kind='job' AND d.dependency IN (SELECT value FROM json_each(?1))")?;
        q.query_map([serialized], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?
    };
    for job in jobs {
        targets.push(token(key, &job));
        c.execute(
            "INSERT OR IGNORE INTO intake_revocations VALUES (?1)",
            [token(key, &job)],
        )?;
    }
    Ok(targets)
}
