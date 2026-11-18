//! Owner-only construction inventory. Counts describe records, never semantic or Agent quality.
use super::{compiler::*, consolidation::*, storage::*, *};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const CONSTRUCTION_REPORT_VERSION: &str = "nova.construction-report.v0.1";
pub const MAX_REPORT_SOURCES: usize = 2_048;
pub const MAX_REPORT_UNITS: usize = 2_048;
pub const MAX_REPORT_METADATA_ROWS: usize = 32_768;
pub const MAX_REPORT_ENCRYPTED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCompleteness {
    pub complete: bool,
    pub count_scope: String,
    pub max_sources: usize,
    pub max_current_units: usize,
    pub max_metadata_rows: usize,
    pub max_encrypted_bytes: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConstructionCounts {
    pub sources: usize,
    /// Ordinary sources without a Ready compilation job, including failed/processing jobs.
    pub uncompiled_sources: usize,
    pub sources_without_current_records: usize,
    pub extraction_records: usize,
    pub exact_memories: usize,
    pub topics: usize,
    /// Distinct exact-memory × source references, not independent evidence or quote counts.
    pub source_links: usize,
    pub current_revisions: usize,
    pub historical_revisions: usize,
    pub wiki_fresh: usize,
    pub wiki_pending: usize,
    pub wiki_blocked: usize,
    pub wiki_stale: usize,
    pub wiki_cold: usize,
    pub rule_associations: BTreeMap<String, usize>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConstructionMetadata {
    pub source_roles: BTreeMap<String, usize>,
    pub capture_fidelity: BTreeMap<String, usize>,
    pub semantic_origins: BTreeMap<String, usize>,
    pub privacy_levels: BTreeMap<String, usize>,
    pub job_states: BTreeMap<String, usize>,
    pub conditioned_records: usize,
    pub known_valid_time_records: usize,
    pub unknown_valid_time_records: usize,
    pub known_occurrence_records: usize,
    pub unknown_occurrence_records: usize,
    pub protected_records: usize,
    pub records_with_derivation: usize,
    pub records_with_modules: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionUsage {
    pub configured_connections: usize,
    pub active_connections: usize,
    pub revoked_connections: usize,
    pub expired_connections: usize,
    pub exhausted_connections: usize,
    pub logical_calls: usize,
    pub delivered_items: usize,
    pub operations: BTreeMap<String, usize>,
    pub evidence: String,
}
impl Default for ConstructionUsage {
    fn default() -> Self {
        Self {
            configured_connections: 0,
            active_connections: 0,
            revoked_connections: 0,
            expired_connections: 0,
            exhausted_connections: 0,
            logical_calls: 0,
            delivered_items: 0,
            operations: BTreeMap::new(),
            evidence: "server_recorded_only_not_host_acceptance".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmeasuredCapabilities {
    pub goal_coverage_denominator: Option<usize>,
    pub semantic_correctness: Option<f64>,
    pub actual_tokens: Option<u64>,
    pub actual_cost: Option<f64>,
    pub agent_task_benefit: Option<f64>,
    pub capability_level: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionReport {
    pub schema_version: String,
    pub scope: AssetScope,
    pub generated_at: String,
    /// Keyed content/rule/lifecycle identity. Excludes connection/evaluation/use records and
    /// report time. Persistent retirement fences prevent a deleted library returning to an
    /// earlier identity even if its current ordinary inventory happens to match again.
    pub content_snapshot_id: String,
    pub completeness: ReportCompleteness,
    pub construction: ConstructionCounts,
    pub metadata: ConstructionMetadata,
    pub usage: ConstructionUsage,
    pub unmeasured: UnmeasuredCapabilities,
    pub notices: Vec<String>,
}
#[derive(Deserialize)]
struct ReportConnection {
    policy: MemoryConnection,
}
struct Row {
    id: String,
    kind: String,
}
fn ordinary(p: PrivacyLevel) -> bool {
    matches!(p, PrivacyLevel::Private | PrivacyLevel::Shareable)
}
fn increment<T: Serialize>(counts: &mut BTreeMap<String, usize>, value: T) -> Result<()> {
    let value = serde_json::to_value(value)?;
    let key = value
        .as_str()
        .ok_or_else(|| invalid("report_metadata_enum"))?;
    *counts.entry(key.into()).or_default() += 1;
    Ok(())
}
fn logical(kind: &str, id: &str) -> String {
    id.strip_prefix(&physical_id(kind, "")).unwrap_or(id).into()
}
fn metadata_rows(c: &Connection) -> Result<Vec<Row>> {
    // Examine every relevant encrypted envelope before decrypting; no page limit is mistaken
    // for a library total. Bounds apply to excluded records too, preventing unbounded work.
    let mut q = c.prepare("SELECT id,kind,length(ciphertext) FROM intake_objects WHERE kind IN ('source','unit_head','unit_revision','wiki','job','connection','connection_usage') ORDER BY id LIMIT ?1")?;
    let rows = q
        .query_map([MAX_REPORT_METADATA_ROWS + 1], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, usize>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_REPORT_METADATA_ROWS {
        return Err(invalid("report_metadata_budget"));
    }
    let mut bytes = 0usize;
    let mut sources = 0;
    let mut heads = 0;
    for (_, kind, size) in &rows {
        bytes = bytes
            .checked_add(*size)
            .ok_or_else(|| invalid("report_byte_budget"))?;
        sources += usize::from(kind == "source");
        heads += usize::from(kind == "unit_head");
    }
    if sources > MAX_REPORT_SOURCES || heads > MAX_REPORT_UNITS {
        return Err(invalid("report_object_budget"));
    }
    if bytes > MAX_REPORT_ENCRYPTED_BYTES {
        return Err(invalid("report_byte_budget"));
    }
    Ok(rows
        .into_iter()
        .map(|(id, kind, _)| Row {
            id: logical(&kind, &id),
            kind,
        })
        .collect())
}

fn hash_lifecycle_fences(
    c: &Connection,
    snapshot: &mut Sha256,
    existing_metadata_rows: usize,
) -> Result<()> {
    if !available(c)? {
        return Ok(());
    }
    let remaining = MAX_REPORT_METADATA_ROWS
        .checked_sub(existing_metadata_rows)
        .ok_or_else(|| invalid("report_metadata_budget"))?;
    // These existing keyed tombstones survive content erasure and backups. Only source/unit/
    // dependent-job retirement writes them; connection revocation and evaluation imports do
    // not. Hashing them closes empty -> receive -> delete -> empty snapshot ABA without new
    // schema, report writes, retained source text, or returned deleted identifiers.
    let mut q = c.prepare(
        "SELECT 'content_revocation',dependency FROM intake_revocations
         UNION ALL SELECT 'retired_intake_request',binding FROM intake_retired_requests
         ORDER BY 1,2 LIMIT ?1",
    )?;
    let rows = q
        .query_map([remaining + 1], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > remaining {
        return Err(invalid("report_metadata_budget"));
    }
    for (kind, fence) in rows {
        if fence.len() > 256 {
            return Err(invalid("report_lifecycle_fence_bounds"));
        }
        snapshot.update(serde_json::to_vec(&(kind, fence))?);
    }
    Ok(())
}

impl IntakeStore {
    /// Read-only Owner aggregate in one SQLite snapshot. Overflow is an error, never a partial
    /// or extrapolated total. Sensitive/sealed sources and their records are filtered before
    /// grouping. No raw text, semantic labels, quoted evidence or bearer credentials leave here.
    pub fn construction_report(&self, scope: &AssetScope) -> Result<ConstructionReport> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        let report = construction_report_on(&tx, &self.key, scope)?;
        tx.commit()?;
        Ok(report)
    }
}
/// The caller must authenticate scope and hold a transaction. Reused by evaluation import under
/// its write transaction, so a concurrent content update cannot race snapshot validation.
pub(super) fn construction_report_on(
    connection: &Connection,
    key: &crate::VaultKey,
    scope: &AssetScope,
) -> Result<ConstructionReport> {
    let tx = connection;
    let rows = metadata_rows(tx)?;
    let mut counts = ConstructionCounts::default();
    let mut metadata = ConstructionMetadata::default();
    let mut usage = ConstructionUsage::default();
    let mut sources = BTreeMap::new();
    let mut units = BTreeMap::new();
    let mut memories: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut buckets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut snapshot = Sha256::new();
    snapshot.update(CONSTRUCTION_REPORT_VERSION);
    snapshot.update(CONSOLIDATION_RULES);
    snapshot.update(serde_json::to_vec(scope)?);
    hash_lifecycle_fences(tx, &mut snapshot, rows.len())?;
    for row in rows.iter().filter(|r| r.kind == "source") {
        if !readable(tx, &physical_id("source", &row.id))?
            || exists(tx, "source_withdrawal", &row.id)?
        {
            continue;
        }
        let source: SourceSnapshot = load(tx, key, "source", &row.id)?;
        if source.scope != *scope {
            return Err(denied());
        }
        if !ordinary(source.privacy_level) {
            continue;
        }
        increment(&mut metadata.source_roles, source.role)?;
        increment(&mut metadata.capture_fidelity, source.capture_fidelity)?;
        snapshot.update(serde_json::to_vec(&source)?);
        sources.insert(source.source.object_id.clone(), source);
    }
    let consolidated = available(tx)?;
    for row in rows.iter().filter(|r| r.kind == "unit_head") {
        if !readable(tx, &physical_id("unit_head", &row.id))? {
            continue;
        }
        let item = read_unit(tx, key, scope, &row.id)?;
        let u = &item.unit;
        if u.header.state != UnitState::Active
            || !ordinary(u.header.privacy_level)
            || u.evidence
                .iter()
                .any(|e| !sources.contains_key(&e.source.object_id))
        {
            continue;
        }
        let roles = u
            .evidence
            .iter()
            .map(|e| {
                serde_json::to_value(sources[&e.source.object_id].role)
                    .map(|v| v.as_str().unwrap_or_default().to_owned())
            })
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        let exact = exact_key(key, u, &roles)?;
        memories
            .entry(exact.clone())
            .or_default()
            .extend(u.evidence.iter().map(|e| e.source.object_id.clone()));
        let bucket = if consolidated {
            tx.query_row(
                "SELECT bucket FROM intake_related WHERE unit_id=?1",
                [&row.id],
                |r| r.get::<_, String>(0),
            )?
        } else {
            return Err(invalid("report_compilation_index_missing"));
        };
        buckets.entry(bucket).or_default().insert(exact);
        increment(&mut metadata.semantic_origins, u.origin)?;
        increment(&mut metadata.privacy_levels, u.header.privacy_level)?;
        metadata.conditioned_records += usize::from(!u.content.conditions.is_empty());
        match u.valid_time {
            ValidTime::Unknown => metadata.unknown_valid_time_records += 1,
            ValidTime::Interval { .. } => metadata.known_valid_time_records += 1,
        }
        metadata.known_occurrence_records += usize::from(u.occurred_at.is_some());
        metadata.unknown_occurrence_records += usize::from(u.occurred_at.is_none());
        metadata.protected_records += usize::from(item.protected);
        metadata.records_with_derivation += usize::from(!u.derived_from.is_empty());
        metadata.records_with_modules += usize::from(!u.modules.is_empty());
        snapshot.update(serde_json::to_vec(&item)?);
        units.insert(row.id.clone(), item);
    }
    counts.sources = sources.len();
    counts.extraction_records = units.len();
    counts.current_revisions = units.len();
    counts.exact_memories = memories.len();
    counts.source_links = memories.values().map(BTreeSet::len).sum();
    counts.topics = buckets.len();
    let linked = memories.values().flatten().collect::<BTreeSet<_>>();
    counts.sources_without_current_records =
        sources.keys().filter(|id| !linked.contains(id)).count();
    let mut ready_sources = BTreeSet::new();
    for row in rows.iter().filter(|r| r.kind == "job") {
        if !readable(tx, &physical_id("job", &row.id))? {
            continue;
        }
        let job: IntakeJob = load(tx, key, "job", &row.id)?;
        if job.job.scope != *scope {
            return Err(denied());
        }
        if !sources.contains_key(&job.job.source.object_id) {
            continue;
        }
        increment(&mut metadata.job_states, job.job.state)?;
        if job.job.state == JobState::Ready {
            ready_sources.insert(job.job.source.object_id);
        }
    }
    counts.uncompiled_sources = sources
        .keys()
        .filter(|id| !ready_sources.contains(*id))
        .count();
    for row in rows.iter().filter(|r| r.kind == "unit_revision") {
        // History contributes only to history inventory, never current memory growth.
        if !readable(tx, &physical_id("unit_revision", &row.id))? {
            continue;
        }
        let Some((id, _)) = row.id.rsplit_once(":revision:") else {
            return Err(invalid("report_revision_id"));
        };
        let Some(current) = units.get(id) else {
            continue;
        };
        let u: MemoryUnit = load(tx, key, "unit_revision", &row.id)?;
        if u.header.scope != *scope {
            return Err(denied());
        }
        if u.header.object.revision < current.unit.header.object.revision
            && ordinary(u.header.privacy_level)
            && u.evidence
                .iter()
                .all(|e| sources.contains_key(&e.source.object_id))
        {
            counts.historical_revisions += 1;
        }
    }
    for (bucket, eligible_groups) in &buckets {
        let (generation, state): (u64, u64) = tx.query_row(
            "SELECT generation,blocked FROM intake_dirty WHERE bucket=?1",
            [bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        snapshot.update(serde_json::to_vec(&(bucket, generation, state))?);
        if state == 0 {
            counts.wiki_pending += 1;
            continue;
        }
        if state == 1 {
            counts.wiki_blocked += 1;
            continue;
        }
        if state == 3 {
            counts.wiki_cold += 1;
            continue;
        }
        if state != 2 {
            return Err(invalid("report_wiki_state"));
        }
        if !exists(tx, "wiki", bucket)? || !readable(tx, &physical_id("wiki", bucket))? {
            counts.wiki_stale += 1;
            continue;
        }
        let page: WikiPage = load(tx, key, "wiki", bucket)?;
        if page.scope != *scope {
            return Err(denied());
        }
        let mut fresh = page.generation == generation && page.rules_version == CONSOLIDATION_RULES;
        for input in &page.inputs {
            if input.object_id.starts_with("source_") {
                load_source(tx, key, scope, input)?;
            } else if read_unit(tx, key, scope, &input.object_id)?
                .unit
                .header
                .object
                != *input
            {
                fresh = false;
            }
        }
        if !fresh {
            counts.wiki_stale += 1;
            continue;
        }
        counts.wiki_fresh += 1;
        // Only current ordinary endpoints can contribute an association count. These are
        // deterministic rule hints, not verified semantic relations or independent Gold.
        let mut associations = BTreeSet::new();
        for a in &page.associations {
            if !eligible_groups.contains(&a.from) || !eligible_groups.contains(&a.to) {
                continue;
            }
            if !matches!(
                a.kind.as_str(),
                "parallel_evidence"
                    | "conditional_variant"
                    | "temporal_transition"
                    | "potential_conflict"
            ) {
                return Err(invalid("report_unknown_association"));
            }
            if associations.insert((&a.from, &a.to, &a.kind)) {
                *counts.rule_associations.entry(a.kind.clone()).or_default() += 1;
            }
        }
        snapshot.update(serde_json::to_vec(&(
            &page.rules_version,
            &page.inputs,
            &page.memories,
            &page.associations,
        ))?);
    }
    let at = now()?;
    let current_time = parse_time(&at)?;
    for row in rows.iter().filter(|r| r.kind == "connection") {
        let record: ReportConnection = load(tx, key, "connection", &row.id)?;
        let policy = record.policy;
        if policy.scope != *scope {
            return Err(denied());
        }
        usage.configured_connections += 1;
        if policy.revoked {
            usage.revoked_connections += 1;
        } else if parse_time(&policy.expires_at)? <= current_time {
            usage.expired_connections += 1;
        } else if policy.used_calls >= policy.call_budget {
            usage.exhausted_connections += 1;
        } else {
            usage.active_connections += 1;
        }
    }
    for row in rows.iter().filter(|r| r.kind == "connection_usage") {
        let event: ConnectionUsage = load(tx, key, "connection_usage", &row.id)?;
        if !matches!(event.operation.as_str(), "read" | "read_index" | "capture") {
            return Err(invalid("report_usage_operation"));
        }
        usage.logical_calls += 1;
        if event.operation == "read" {
            usage.delivered_items += event.item_count;
        }
        *usage.operations.entry(event.operation).or_default() += 1;
    }
    let report = ConstructionReport {
        schema_version: CONSTRUCTION_REPORT_VERSION.into(),
        scope: scope.clone(),
        generated_at: at,
        content_snapshot_id: key
            .authentication_tag(&[b"n5:content-snapshot", &snapshot.finalize()]),
        completeness: ReportCompleteness {
            complete: true,
            count_scope: "eligible_current_library".into(),
            max_sources: MAX_REPORT_SOURCES,
            max_current_units: MAX_REPORT_UNITS,
            max_metadata_rows: MAX_REPORT_METADATA_ROWS,
            max_encrypted_bytes: MAX_REPORT_ENCRYPTED_BYTES,
        },
        construction: counts,
        metadata,
        usage,
        unmeasured: UnmeasuredCapabilities {
            goal_coverage_denominator: None,
            semantic_correctness: None,
            actual_tokens: None,
            actual_cost: None,
            agent_task_benefit: None,
            capability_level: None,
        },
        notices: vec![
            "ordinary_private_or_shareable_current_only_no_sensitive_or_sealed".into(),
            "full_bounded_inventory_not_graph_page_counts".into(),
            "exact_duplicates_not_independent_evidence_or_verified_knowledge".into(),
            "source_links_and_rule_associations_do_not_establish_semantic_correctness".into(),
            "unknown_cost_tokens_quality_goal_coverage_and_capability_are_null".into(),
            "connection_configuration_and_server_receipts_do_not_establish_real_host_acceptance"
                .into(),
            "no_security_pass_from_presence_and_no_intelligence_age_or_weighted_score".into(),
        ],
    };
    Ok(report)
}
