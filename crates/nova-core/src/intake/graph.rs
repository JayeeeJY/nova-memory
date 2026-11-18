//! Owner-only, read-only graph projections of current evidence. No model, new ACL or graph cache.
use super::{compiler::read_unit, consolidation::*, storage::*, *};
use crate::VaultKey;
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MEMORY_GRAPH_VERSION: &str = "nova.memory-graph.v0.1";
pub const MAX_GRAPH_METADATA_ROWS: usize = 32_768;
pub const MAX_GRAPH_NODES: usize = 512;
pub const MAX_GRAPH_EDGES: usize = 1_024;
pub const MAX_GRAPH_BYTES: usize = 1024 * 1024;
fn default_limit() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphRequest {
    pub cursor: Option<String>,
    pub query: Option<String>,
    pub limit: usize,
}
impl Default for GraphRequest {
    fn default() -> Self {
        Self {
            cursor: None,
            query: None,
            limit: default_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub summary: Option<String>,
    pub semantic_support: String,
    pub object_ref: Option<RevisionRef>,
    pub members: Vec<RevisionRef>,
    pub source_refs: Vec<RevisionRef>,
    pub privacy_level: Option<PrivacyLevel>,
    pub origin: Option<SemanticOrigin>,
    pub polarity: Option<Polarity>,
    pub modality: Option<Modality>,
    pub valid_time: Option<ValidTime>,
    pub occurred_at: Option<String>,
    pub conditions: Option<BTreeMap<String, Value>>,
    pub member_count: usize,
    pub protected_count: usize,
    pub source_count: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub explanation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub count_scope: String,
    pub topics: usize,
    pub memories: usize,
    pub sources: usize,
    pub extraction_records: usize,
    pub relationships: usize,
    pub metadata_only: bool,
    pub source_text_included: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphProjection {
    pub schema_version: String,
    pub generation: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub stats: GraphStats,
    pub next_cursor: Option<String>,
    pub partial: bool,
    pub notices: Vec<String>,
}

fn ordinary(p: PrivacyLevel) -> bool {
    matches!(p, PrivacyLevel::Shareable | PrivacyLevel::Private)
}
fn summary(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let mut result = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}
fn node(id: String, kind: &str, label: String) -> GraphNode {
    GraphNode {
        id,
        kind: kind.into(),
        label,
        summary: None,
        semantic_support: "structural_metadata_only".into(),
        object_ref: None,
        members: vec![],
        source_refs: vec![],
        privacy_level: None,
        origin: None,
        polarity: None,
        modality: None,
        valid_time: None,
        occurred_at: None,
        conditions: None,
        member_count: 0,
        protected_count: 0,
        source_count: 0,
    }
}
fn edge(key: &VaultKey, from: &str, to: &str, kind: &str, explanation: &str) -> GraphEdge {
    GraphEdge {
        id: key.authentication_tag(&[b"n4:edge", from.as_bytes(), to.as_bytes(), kind.as_bytes()]),
        from: from.into(),
        to: to.into(),
        kind: kind.into(),
        explanation: explanation.into(),
    }
}
/// A deterministic digest of bounded metadata, not a global count or a retained content snapshot.
/// Source IDs are immutable; head ciphertext changes on every correction/undo. Dirty generations
/// include consolidation state, so no page mixes pending and rebuilt Wiki relationships.
fn generation(c: &Connection, key: &VaultKey, scope: &AssetScope) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(MEMORY_GRAPH_VERSION);
    digest.update(serde_json::to_vec(scope)?);
    let mut rows_seen = 0;
    let mut q = c.prepare("SELECT id,kind,CASE WHEN kind='unit_head' THEN nonce ELSE x'' END,CASE WHEN kind='unit_head' THEN ciphertext ELSE x'' END FROM intake_objects WHERE kind IN ('source','unit_head','source_withdrawal') ORDER BY id LIMIT ?1")?;
    let mut rows = q.query([MAX_GRAPH_METADATA_ROWS + 1])?;
    while let Some(row) = rows.next()? {
        rows_seen += 1;
        if rows_seen > MAX_GRAPH_METADATA_ROWS {
            return Err(invalid("graph_metadata_budget"));
        }
        let record = (
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        );
        digest.update(serde_json::to_vec(&record)?);
    }
    if available(c)? {
        let mut q = c.prepare(
            "SELECT bucket,generation,blocked FROM intake_dirty ORDER BY bucket LIMIT ?1",
        )?;
        let mut rows = q.query([MAX_GRAPH_METADATA_ROWS + 1])?;
        while let Some(row) = rows.next()? {
            rows_seen += 1;
            if rows_seen > MAX_GRAPH_METADATA_ROWS {
                return Err(invalid("graph_metadata_budget"));
            }
            let record = (
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
            );
            digest.update(serde_json::to_vec(&record)?);
        }
    }
    Ok(key.authentication_tag(&[b"n4:generation", &digest.finalize()]))
}
fn request_binding(key: &VaultKey, query: &str, limit: usize) -> String {
    key.authentication_tag(&[
        b"n4:request",
        query.as_bytes(),
        limit.to_string().as_bytes(),
    ])
}
fn cursor(key: &VaultKey, generation: &str, before: i64, binding: &str) -> String {
    // Hide row sequence metadata as well as authenticating it; default graph responses should
    // not expose the number/position of partitions suppressed by the privacy filter.
    let encoded = (before as u64) ^ cursor_mask(key, generation, binding);
    let value = format!("{generation}.{encoded:016x}.{binding}");
    let signature = key.authentication_tag(&[b"n4:cursor", value.as_bytes()]);
    format!("{value}.{signature}")
}
fn cursor_mask(key: &VaultKey, generation: &str, binding: &str) -> u64 {
    let tag =
        key.authentication_tag(&[b"n4:cursor-mask", generation.as_bytes(), binding.as_bytes()]);
    let digest = Sha256::digest(tag.as_bytes());
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    )
}
fn before(key: &VaultKey, value: Option<&str>, generation: &str, binding: &str) -> Result<i64> {
    let Some(value) = value else {
        return Ok(i64::MAX);
    };
    if value.len() > 512 {
        return Err(invalid("graph_cursor"));
    }
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(invalid("graph_cursor"));
    }
    let signed = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
    if key.authentication_tag(&[b"n4:cursor", signed.as_bytes()]) != parts[3] || parts[2] != binding
    {
        return Err(invalid("graph_cursor"));
    }
    if parts[0] != generation {
        return Err(invalid("graph_stale_cursor"));
    }
    let encoded = u64::from_str_radix(parts[1], 16).map_err(|_| invalid("graph_cursor"))?;
    let decoded = encoded ^ cursor_mask(key, generation, binding);
    if decoded == 0 || decoded > i64::MAX as u64 {
        return Err(invalid("graph_cursor"));
    }
    Ok(decoded as i64)
}

impl IntakeStore {
    /// Owner metadata projection only. Authorize first, then use one consistent read transaction.
    /// Counts cover displayed ordinary current records only; no global sensitive inventory leaks.
    /// A page scans at most four topic partitions and 64 current records per partition. A larger
    /// partition is explicitly omitted until a separate bounded detail workflow exists.
    pub fn owner_graph(
        &self,
        scope: &AssetScope,
        request: &GraphRequest,
    ) -> Result<GraphProjection> {
        self.check(scope)?;
        let raw_query = request.query.as_deref().unwrap_or("");
        if !(1..=4).contains(&request.limit)
            || raw_query.chars().count() > 128
            || raw_query.chars().any(char::is_control)
        {
            return Err(invalid("graph_request_bounds"));
        }
        let query = raw_query.trim().to_lowercase();
        let tx = self.connection.unchecked_transaction()?;
        let generation = generation(&tx, &self.key, scope)?;
        let binding = request_binding(&self.key, &query, request.limit);
        let before = before(&self.key, request.cursor.as_deref(), &generation, &binding)?;
        let mut nodes = BTreeMap::new();
        let mut edges = vec![];
        let mut notices = BTreeSet::from([
            "ordinary_current_only_no_sensitive_or_sealed".to_string(),
            "page_counts_not_global_inventory".to_string(),
            "uncompiled_sources_remain_in_intake_not_graph".to_string(),
            "structural_associations_not_verified_semantic_truth".to_string(),
        ]);
        let mut incomplete = false;
        let mut next_cursor = None;
        if available(&tx)? {
            let partitions = {
                // Retired partitions remain as invalidation metadata. Skip those with no current,
                // readable head before paging, so deletions do not bury older live memories behind
                // empty pages. Privacy filtering still occurs after bounded decryption below.
                let mut q = tx.prepare("SELECT d.rowid,d.bucket,d.blocked FROM intake_dirty d WHERE d.rowid<?1 AND EXISTS(SELECT 1 FROM intake_related r JOIN intake_objects h ON h.id='n2a:unit_head:'||r.unit_id AND h.kind='unit_head' WHERE r.bucket=d.bucket AND NOT EXISTS(SELECT 1 FROM intake_dependencies dep JOIN intake_revocations rev ON dep.dependency=rev.dependency WHERE dep.record_id=h.id)) ORDER BY d.rowid DESC LIMIT ?2")?;
                q.query_map(params![before, request.limit + 1], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
            };
            if partitions.len() > request.limit {
                next_cursor = Some(cursor(
                    &self.key,
                    &generation,
                    partitions[request.limit - 1].0,
                    &binding,
                ));
            }
            for (_, bucket, state) in partitions.into_iter().take(request.limit) {
                let ids = {
                    let mut q = tx.prepare("SELECT unit_id FROM intake_related WHERE bucket=?1 AND NOT EXISTS(SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id='n2a:unit_head:'||intake_related.unit_id) ORDER BY unit_id LIMIT 65")?;
                    q.query_map([&bucket], |r| r.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                };
                if ids.len() > MAX_RELATED_UNITS {
                    incomplete = true;
                    notices
                        .insert("partition_over_64_records_omitted_no_privacy_assumptions".into());
                    continue;
                }
                let mut groups: BTreeMap<String, GraphNode> = BTreeMap::new();
                let mut sources: BTreeMap<String, GraphNode> = BTreeMap::new();
                for id in ids {
                    let item = read_unit(&tx, &self.key, scope, &id)?;
                    let u = item.unit;
                    if u.header.state != UnitState::Active || !ordinary(u.header.privacy_level) {
                        continue;
                    }
                    let evidence_sources = u
                        .evidence
                        .iter()
                        .map(|e| load_source(&tx, &self.key, scope, &e.source))
                        .collect::<Result<Vec<_>>>()?;
                    if evidence_sources.iter().any(|s| !ordinary(s.privacy_level)) {
                        continue;
                    }
                    if !query.is_empty()
                        && !serde_json::to_string(&u.content)?
                            .to_lowercase()
                            .contains(&query)
                    {
                        continue;
                    }
                    let roles = evidence_sources
                        .iter()
                        .map(|s| {
                            serde_json::to_value(s.role)
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                        })
                        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
                    let id = exact_key(&self.key, &u, &roles)?;
                    let memory = groups.entry(id.clone()).or_insert_with(|| {
                        let mut n = node(
                            id,
                            "memory",
                            summary(
                                &format!("{} · {}", u.content.subject, u.content.predicate),
                                160,
                            ),
                        );
                        n.summary = Some(summary(&u.content.value.to_string(), 320));
                        n.semantic_support = "compiled_model_claim_unverified".into();
                        n.privacy_level = Some(u.header.privacy_level);
                        n.origin = Some(u.origin);
                        n.valid_time = Some(u.valid_time.clone());
                        n.polarity = Some(u.content.polarity);
                        n.modality = Some(u.content.modality);
                        n.occurred_at = u.occurred_at.clone();
                        n.conditions = Some(u.content.conditions.clone());
                        n
                    });
                    memory.members.push(u.header.object);
                    memory.member_count += 1;
                    memory.protected_count += usize::from(item.protected);
                    for s in evidence_sources {
                        if !memory.source_refs.contains(&s.source) {
                            memory.source_refs.push(s.source.clone());
                        }
                        sources
                            .entry(s.source.object_id.clone())
                            .or_insert_with(|| {
                                let mut n = node(
                                    s.source.object_id.clone(),
                                    "source",
                                    summary(&s.source_platform, 128),
                                );
                                n.summary = Some(format!(
                                    "{} · {}",
                                    serde_json::to_value(s.role).unwrap_or_default(),
                                    s.recorded_at
                                ));
                                n.object_ref = Some(s.source.clone());
                                n.privacy_level = Some(s.privacy_level);
                                n.occurred_at = s.occurred_at;
                                n
                            });
                    }
                    memory.source_count = memory.source_refs.len();
                }
                if groups.is_empty() {
                    continue;
                }
                let mut local_edges = vec![];
                let topic_id = format!("topic:{bucket}");
                for memory in groups.values() {
                    local_edges.push(edge(&self.key, &memory.id, &topic_id, "in_topic", "Exact subject/predicate partition; an organizational grouping, not trusted project membership."));
                    for source in &memory.source_refs {
                        local_edges.push(edge(&self.key, &memory.id, &source.object_id, "supported_by", "Current evidence locator; the original remains separate. Model claims are not verified facts."));
                    }
                }
                if state == 2
                    && exists(&tx, "wiki", &bucket)?
                    && readable(&tx, &physical_id("wiki", &bucket))?
                {
                    let page: WikiPage = load(&tx, &self.key, "wiki", &bucket)?;
                    if page.scope != *scope {
                        return Err(denied());
                    }
                    // The dirty generation is the authoritative invalidation fence; validate the
                    // exact input heads again before reusing any persisted rule association.
                    let mut fresh = true;
                    for input in &page.inputs {
                        if input.object_id.starts_with("source_") {
                            load_source(&tx, &self.key, scope, input)?;
                        } else if read_unit(&tx, &self.key, scope, &input.object_id)?
                            .unit
                            .header
                            .object
                            != *input
                        {
                            fresh = false;
                        }
                    }
                    if fresh {
                        for association in page.associations {
                            if !groups.contains_key(&association.from)
                                || !groups.contains_key(&association.to)
                            {
                                continue;
                            }
                            if !matches!(
                                association.kind.as_str(),
                                "parallel_evidence"
                                    | "conditional_variant"
                                    | "temporal_transition"
                                    | "potential_conflict"
                            ) {
                                return Err(invalid("graph_unknown_association"));
                            }
                            if edges.len() + local_edges.len() >= MAX_GRAPH_EDGES {
                                incomplete = true;
                                notices.insert("relationship_budget_partial".into());
                                break;
                            }
                            local_edges.push(edge(
                                &self.key,
                                &association.from,
                                &association.to,
                                &association.kind,
                                &association.interpretation,
                            ));
                        }
                    } else {
                        incomplete = true;
                        notices.insert("wiki_not_current_associations_omitted".into());
                    }
                } else {
                    incomplete = true;
                    notices.insert(if state == 3 {
                        "wiki_cold_associations_omitted".into()
                    } else {
                        "wiki_pending_or_blocked_associations_omitted".into()
                    });
                }
                let additional_sources =
                    sources.keys().filter(|id| !nodes.contains_key(*id)).count();
                if nodes.len() + groups.len() + additional_sources + 1 > MAX_GRAPH_NODES
                    || edges.len() + local_edges.len() > MAX_GRAPH_EDGES
                {
                    incomplete = true;
                    notices.insert("projection_budget_partition_omitted".into());
                    continue;
                }
                let label = groups
                    .values()
                    .next()
                    .map(|g| g.label.clone())
                    .unwrap_or_default();
                let mut topic = node(topic_id, "topic", label);
                topic.member_count = groups.len();
                topic.source_count = sources.len();
                topic.summary = Some("Exact subject/predicate partition; not a project ACL or semantic clustering claim.".into());
                nodes.insert(topic.id.clone(), topic);
                nodes.extend(groups);
                nodes.extend(sources);
                edges.extend(local_edges);
            }
        }
        let nodes = nodes.into_values().collect::<Vec<_>>();
        let stats = GraphStats {
            count_scope: "page".into(),
            topics: nodes.iter().filter(|n| n.kind == "topic").count(),
            memories: nodes.iter().filter(|n| n.kind == "memory").count(),
            sources: nodes.iter().filter(|n| n.kind == "source").count(),
            extraction_records: nodes
                .iter()
                .filter(|n| n.kind == "memory")
                .map(|n| n.member_count)
                .sum(),
            relationships: edges.len(),
            metadata_only: false,
            source_text_included: false,
        };
        let projection = GraphProjection {
            schema_version: MEMORY_GRAPH_VERSION.into(),
            generation,
            nodes,
            edges,
            stats,
            partial: incomplete || next_cursor.is_some(),
            next_cursor,
            notices: notices.into_iter().collect(),
        };
        if serde_json::to_vec(&projection)?.len() > MAX_GRAPH_BYTES {
            return Err(invalid("graph_output_budget"));
        }
        tx.commit()?;
        Ok(projection)
    }
}
