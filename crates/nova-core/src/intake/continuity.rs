//! Explicit, encrypted task continuity. These objects are neither memory truth nor instructions.
use super::{compiler::*, connections::*, storage::*, *};
use crate::VaultKey;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::{Duration, OffsetDateTime};

pub const CONTINUITY_VERSION: &str = "nova.continuity.v0.1";
const MAX_RECORDS: i64 = 256;
const MAX_CONTEXT_BYTES: usize = 8192;
mod tasks;
pub use tasks::*;

/// A server-issued session bound to an existing authenticated N3 connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub session_id: String,
    pub revision: u64,
    pub connection_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRegistration {
    pub request_id: String,
    pub connection_id: String,
    pub lifetime_seconds: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    scope: AssetScope,
    session: SessionRef,
    fingerprint: String,
    expires_at: String,
    closed: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffContent {
    pub summary: String,
    pub completed: Vec<String>,
    pub open_tasks: Vec<String>,
    pub decisions: Vec<String>,
    pub failed_attempts: Vec<String>,
    pub constraints: Vec<String>,
    pub next_steps: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateHandoff {
    pub request_id: String,
    pub from: SessionRef,
    pub recipient: SessionRef,
    pub lifetime_seconds: u32,
    pub memories: Vec<RevisionRef>,
    pub content: HandoffContent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    Pending,
    Claimed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffMetadata {
    pub schema_version: String,
    pub handoff_id: String,
    pub revision: u64,
    pub scope: AssetScope,
    pub from: SessionRef,
    pub recipient: SessionRef,
    pub created_at: String,
    pub expires_at: String,
    pub state: HandoffState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffEnvelope {
    pub metadata: HandoffMetadata,
    pub memories: Vec<RevisionRef>,
    pub content: HandoffContent,
    pub interpretation: String,
    pub confers_execution_authority: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_notes: Vec<OwnerTaskNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffClaimReceipt {
    pub receipt_id: String,
    pub handoff_id: String,
    pub recipient: SessionRef,
    pub claimed_at: String,
    pub status: String,
    pub remaining_calls: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffDelivery {
    pub envelope: HandoffEnvelope,
    pub receipt: HandoffClaimReceipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRecord {
    request_id: String,
    receipt: HandoffClaimReceipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffRecord {
    envelope: HandoffEnvelope,
    fingerprint: String,
    claim: Option<ClaimRecord>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPage {
    pub items: Vec<(i64, HandoffMetadata)>,
    pub next_before: Option<i64>,
}

fn tagged(key: &VaultKey, domain: &[u8], value: &[u8]) -> String {
    key.authentication_tag(&[b"continuity:v1", domain, value])
}
fn require(c: &Connection) -> Result<()> {
    if c.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='intake_continuity_handoffs'",
        [],
        |_| Ok(()),
    )
    .optional()?
    .is_none()
    {
        return Err(invalid("continuity_not_enabled"));
    }
    Ok(())
}
fn lifetime(seconds: u32) -> Result<()> {
    if !(1..=86400).contains(&seconds) {
        return Err(invalid("continuity_lifetime"));
    }
    Ok(())
}
fn session(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    r: &SessionRef,
) -> Result<SessionRecord> {
    require(c)?;
    let s: SessionRecord = load(c, key, "continuity_session", &r.session_id)?;
    if s.session.revision != 1
        || s.scope != *scope
        || s.session != *r
        || s.closed
        || parse_time(&s.expires_at)? <= OffsetDateTime::now_utc()
    {
        return Err(denied());
    }
    // The session itself never extends or restores the underlying connection grant.
    continuity_recipient(c, key, scope, &r.connection_id)?;
    Ok(s)
}
fn recipient(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    access: &ConnectionAccess,
    r: &SessionRef,
) -> Result<Vec<CompiledUnit>> {
    let (policy, allowed) = continuity_connection(c, key, scope, access)?;
    if policy.id != r.connection_id {
        return Err(denied());
    }
    session(c, key, scope, r)?;
    Ok(allowed)
}

/// Owner-only metadata association uses the same live session gate, never a new read grant.
pub(super) fn validate_owner_session(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    r: &SessionRef,
) -> Result<()> {
    session(c, key, scope, r).map(|_| ())
}
fn refs_allowed(refs: &[RevisionRef], allowed: &[CompiledUnit]) -> Result<()> {
    if refs.is_empty() || refs.len() > 16 {
        return Err(invalid("continuity_evidence_limit"));
    }
    for (i, r) in refs.iter().enumerate() {
        if refs[..i].contains(r) || !allowed.iter().any(|u| u.unit.header.object == *r) {
            return Err(denied());
        }
    }
    Ok(())
}
fn context(input: &HandoffContent) -> Result<()> {
    let groups = [
        &input.completed,
        &input.open_tasks,
        &input.decisions,
        &input.failed_attempts,
        &input.constraints,
        &input.next_steps,
    ];
    if input.summary.trim().is_empty()
        || input.summary.len() > 2048
        || groups
            .iter()
            .any(|g| g.len() > 8 || g.iter().any(|s| s.trim().is_empty() || s.len() > 1024))
        || serde_json::to_vec(input)?.len() > MAX_CONTEXT_BYTES
    {
        return Err(invalid("continuity_context_limit"));
    }
    Ok(())
}
fn live_handoff(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    record: &HandoffRecord,
    allowed: &[CompiledUnit],
) -> Result<()> {
    let m = &record.envelope.metadata;
    if m.schema_version != CONTINUITY_VERSION
        || m.revision != 1
        || m.scope != *scope
        || m.state == HandoffState::Cancelled
        || parse_time(&m.expires_at)? <= OffsetDateTime::now_utc()
    {
        return Err(denied());
    }
    session(c, key, scope, &m.from)?;
    session(c, key, scope, &m.recipient)?;
    refs_allowed(&record.envelope.memories, allowed)
}

impl IntakeStore {
    /// Owner/runtime opt-in only. Does not expose new tools or enable automatic capture.
    pub fn enable_continuity(&mut self, scope: &AssetScope) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Connections must already be explicitly enabled by their existing owner API.
        if tx
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name='intake_connection_tokens'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_none()
        {
            return Err(invalid("connections_not_enabled"));
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS intake_continuity_handoffs (handoff_id TEXT PRIMARY KEY, recipient_tag TEXT NOT NULL); CREATE INDEX IF NOT EXISTS intake_continuity_recipient ON intake_continuity_handoffs(recipient_tag); CREATE TABLE IF NOT EXISTS intake_continuity_events(event_id TEXT PRIMARY KEY, session_tag TEXT NOT NULL, sequence INTEGER NOT NULL, source_tag TEXT NOT NULL UNIQUE, UNIQUE(session_tag,sequence));")?;
        tx.commit()?;
        Ok(())
    }
    /// Trusted owner registration; adapters must bind their real host session before calling.
    pub fn register_continuity_session(
        &mut self,
        scope: &AssetScope,
        input: SessionRegistration,
    ) -> Result<SessionRef> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = register_session_in(&tx, &self.key, scope, input)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn close_continuity_session(
        &mut self,
        scope: &AssetScope,
        reference: &SessionRef,
    ) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let mut record: SessionRecord =
            load(&tx, &self.key, "continuity_session", &reference.session_id)?;
        if record.scope != *scope || record.session != *reference {
            return Err(denied());
        }
        record.closed = true;
        update(
            &tx,
            &self.key,
            "continuity_session",
            &reference.session_id,
            &record,
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn create_handoff(
        &mut self,
        scope: &AssetScope,
        input: CreateHandoff,
    ) -> Result<HandoffMetadata> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = create_handoff_in(&tx, &self.key, scope, input)?;
        tx.commit()?;
        Ok(result)
    }
    /// Owner metadata only; source withdrawal, deletion and version changes hide stale objects.
    pub fn list_handoffs(
        &self,
        scope: &AssetScope,
        target: &SessionRef,
        before: Option<i64>,
    ) -> Result<HandoffPage> {
        self.check(scope)?;
        list_page(&self.connection, &self.key, scope, target, before)
    }
    pub fn cancel_handoff(&mut self, scope: &AssetScope, handoff_id: &str) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let mut record: HandoffRecord = load(&tx, &self.key, "continuity_handoff", handoff_id)?;
        if record.envelope.metadata.scope != *scope {
            return Err(denied());
        }
        record.envelope.metadata.state = HandoffState::Cancelled;
        update(&tx, &self.key, "continuity_handoff", handoff_id, &record)?;
        tx.commit()?;
        Ok(())
    }
    /// Serializes all contenders, and rechecks authority/evidence even for a lost-response replay.
    pub fn claim_handoff(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        target: &SessionRef,
        handoff_id: &str,
        request_id: &str,
    ) -> Result<HandoffDelivery> {
        self.check(scope)?;
        if !identifier(handoff_id) || !identifier(request_id) {
            return Err(invalid("continuity_claim_input"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let allowed = recipient(&tx, &self.key, scope, access, target)?;
        let mut record: HandoffRecord = load(&tx, &self.key, "continuity_handoff", handoff_id)?;
        if record.envelope.metadata.recipient != *target {
            return Err(denied());
        }
        live_handoff(&tx, &self.key, scope, &record, &allowed)?;
        if let Some(claim) = record.claim {
            if claim.request_id != request_id {
                return Err(conflict());
            }
            return Ok(HandoffDelivery {
                envelope: record.envelope,
                receipt: claim.receipt,
            });
        }
        let receipt_id = id("handoff_claim");
        let remaining_calls = continuity_charge(
            &tx,
            &self.key,
            scope,
            access,
            "handoff_claim",
            &receipt_id,
            record.envelope.memories.len(),
        )?;
        let receipt = HandoffClaimReceipt {
            receipt_id,
            handoff_id: handoff_id.into(),
            recipient: target.clone(),
            claimed_at: now()?,
            status: "claimed_not_confirmed_used".into(),
            remaining_calls,
        };
        record.envelope.metadata.state = HandoffState::Claimed;
        record.claim = Some(ClaimRecord {
            request_id: request_id.into(),
            receipt: receipt.clone(),
        });
        update(&tx, &self.key, "continuity_handoff", handoff_id, &record)?;
        tx.commit()?;
        Ok(HandoffDelivery {
            envelope: record.envelope,
            receipt,
        })
    }
}

/// A minimal N3-approved projection; never includes source text, model reasoning or hidden prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityMemoryView {
    pub memory: RevisionRef,
    pub content: SemanticContent,
    pub origin: SemanticOrigin,
    pub valid_time: ValidTime,
    pub source_versions: Vec<RevisionRef>,
    pub semantic_support: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupBriefRequest {
    pub session: SessionRef,
    pub pinned: Vec<RevisionRef>,
    /// Explicitly selected constraints; this projection does not classify facts as constraints.
    pub constraints: Vec<RevisionRef>,
    pub max_items: usize,
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityReadReceipt {
    pub receipt_id: String,
    pub connection_id: String,
    pub session: SessionRef,
    pub created_at: String,
    pub operation: String,
    pub memory_versions: Vec<RevisionRef>,
    pub remaining_calls: u64,
    pub status: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupBrief {
    pub schema_version: String,
    pub pinned: Vec<ContinuityMemoryView>,
    pub constraints: Vec<ContinuityMemoryView>,
    pub recent: Vec<ContinuityMemoryView>,
    pub pending_handoffs: Vec<HandoffMetadata>,
    pub truncated: bool,
    pub handoffs_claimed: usize,
    pub receipt: ContinuityReadReceipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventRegistration {
    pub session: SessionRef,
    pub sequence: u64,
    pub source: RevisionRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionEventRecord {
    scope: AssetScope,
    event: SessionEventRegistration,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionRecapRequest {
    pub session: SessionRef,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapEvent {
    pub sequence: u64,
    pub source: RevisionRef,
    pub capture_fidelity: crate::CaptureFidelity,
    pub memories: Vec<ContinuityMemoryView>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionRecap {
    pub schema_version: String,
    pub session: SessionRef,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub events: Vec<RecapEvent>,
    pub unavailable_sequences: Vec<u64>,
    pub incomplete: bool,
    pub incomplete_reasons: Vec<String>,
    pub view: String,
    pub receipt: ContinuityReadReceipt,
}
fn view(u: &CompiledUnit) -> ContinuityMemoryView {
    let mut sources = vec![];
    for e in &u.unit.evidence {
        if !sources.contains(&e.source) {
            sources.push(e.source.clone());
        }
    }
    ContinuityMemoryView {
        memory: u.unit.header.object.clone(),
        content: u.unit.content.clone(),
        origin: u.unit.origin,
        valid_time: u.unit.valid_time.clone(),
        source_versions: sources,
        semantic_support: "model_output_unverified".into(),
    }
}
fn selected(refs: &[RevisionRef], allowed: &[CompiledUnit]) -> Result<Vec<ContinuityMemoryView>> {
    if refs.len() > 8 {
        return Err(invalid("continuity_brief_limit"));
    }
    let mut result = vec![];
    for (i, r) in refs.iter().enumerate() {
        if refs[..i].contains(r) {
            return Err(invalid("continuity_duplicate_selection"));
        }
        let u = allowed
            .iter()
            .find(|u| u.unit.header.object == *r)
            .ok_or_else(denied)?;
        result.push(view(u));
    }
    Ok(result)
}
fn read_receipt(
    policy: &MemoryConnection,
    session: &SessionRef,
    operation: &str,
) -> Result<ContinuityReadReceipt> {
    Ok(ContinuityReadReceipt {
        receipt_id: id("continuity_read"),
        connection_id: policy.id.clone(),
        session: session.clone(),
        created_at: format_time(
            OffsetDateTime::now_utc()
                .replace_nanosecond(0)
                .map_err(|_| invalid("continuity_clock"))?,
        )?,
        operation: operation.into(),
        memory_versions: vec![],
        remaining_calls: policy.call_budget,
        status: "delivered_unverified_context".into(),
    })
}
fn byte_budget(bytes: usize) -> Result<()> {
    if !(1024..=32768).contains(&bytes) {
        return Err(invalid("continuity_byte_budget"));
    }
    Ok(())
}
impl IntakeStore {
    /// Rebuilds a bounded, read-only projection. Metering is recorded, but no handoff is claimed.
    pub fn startup_brief(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: StartupBriefRequest,
    ) -> Result<StartupBrief> {
        self.check(scope)?;
        byte_budget(input.max_bytes)?;
        if !(1..=8).contains(&input.max_items)
            || input.pinned.len() + input.constraints.len() > input.max_items
            || input.pinned.iter().any(|r| input.constraints.contains(r))
        {
            return Err(invalid("continuity_brief_limit"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let allowed = recipient(&tx, &self.key, scope, access, &input.session)?;
        let (policy, _) = continuity_connection(&tx, &self.key, scope, access)?;
        let mut dated = allowed
            .into_iter()
            .map(|unit| Ok((parse_time(&unit.unit.header.recorded_at)?, unit)))
            .collect::<Result<Vec<_>>>()?;
        dated.sort_by(|a, b| {
            b.0.cmp(&a.0).then(
                a.1.unit
                    .header
                    .object
                    .object_id
                    .cmp(&b.1.unit.header.object.object_id),
            )
        });
        let allowed = dated.into_iter().map(|(_, unit)| unit).collect::<Vec<_>>();
        let mut result = StartupBrief {
            schema_version: CONTINUITY_VERSION.into(),
            pinned: selected(&input.pinned, &allowed)?,
            constraints: selected(&input.constraints, &allowed)?,
            recent: vec![],
            pending_handoffs: vec![],
            truncated: false,
            handoffs_claimed: 0,
            receipt: read_receipt(&policy, &input.session, "startup_brief")?,
        };
        result.receipt.memory_versions = input
            .pinned
            .iter()
            .chain(&input.constraints)
            .cloned()
            .collect();
        // Explicit pins/constraints are indivisible. An insufficient budget is an error, not silent truncation.
        if serde_json::to_vec(&result)?.len() > input.max_bytes {
            return Err(invalid("continuity_byte_budget"));
        }
        for u in &allowed {
            if input.pinned.contains(&u.unit.header.object)
                || input.constraints.contains(&u.unit.header.object)
            {
                continue;
            }
            if result.pinned.len() + result.constraints.len() + result.recent.len()
                >= input.max_items
            {
                result.truncated = true;
                break;
            }
            result.recent.push(view(u));
            result
                .receipt
                .memory_versions
                .push(u.unit.header.object.clone());
            if serde_json::to_vec(&result)?.len() > input.max_bytes {
                result.recent.pop();
                result.receipt.memory_versions.pop();
                result.truncated = true;
                break;
            }
        }
        let rows = {
            let mut stmt=tx.prepare("SELECT handoff_id FROM intake_continuity_handoffs WHERE recipient_tag=?1 ORDER BY rowid DESC LIMIT 33")?;
            stmt.query_map(
                [tagged(
                    &self.key,
                    b"recipient",
                    input.session.session_id.as_bytes(),
                )],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if rows.len() > 32 {
            result.truncated = true;
        }
        for id in rows.into_iter().take(32) {
            if !exists(&tx, "continuity_handoff", &id)?
                || !consolidation::readable(&tx, &physical_id("continuity_handoff", &id))?
            {
                continue;
            }
            let record: HandoffRecord = load(&tx, &self.key, "continuity_handoff", &id)?;
            if record.envelope.metadata.recipient != input.session {
                return Err(denied());
            }
            if record.envelope.metadata.state != HandoffState::Pending
                || live_handoff(&tx, &self.key, scope, &record, &allowed).is_err()
            {
                continue;
            }
            result.pending_handoffs.push(record.envelope.metadata);
            if serde_json::to_vec(&result)?.len() > input.max_bytes {
                result.pending_handoffs.pop();
                result.truncated = true;
                break;
            }
        }
        // Include receipt overhead in the byte budget and fail atomically before charging.
        if serde_json::to_vec(&result)?.len() > input.max_bytes {
            return Err(invalid("continuity_byte_budget"));
        }
        result.receipt.remaining_calls = continuity_charge(
            &tx,
            &self.key,
            scope,
            access,
            "startup_brief",
            &result.receipt.receipt_id,
            result.receipt.memory_versions.len(),
        )?;
        insert(
            &tx,
            &self.key,
            "continuity_read",
            &result.receipt.receipt_id,
            &result.receipt,
        )?;
        tx.commit()?;
        Ok(result)
    }
    /// Trusted adapter/owner event binding. The durable source must have been captured by this N3
    /// connection. A source can belong to only one session and sequence; tombstones prevent reuse.
    pub fn record_continuity_event(
        &mut self,
        scope: &AssetScope,
        input: SessionEventRegistration,
    ) -> Result<()> {
        self.check(scope)?;
        if input.sequence == 0 || input.sequence > i64::MAX as u64 {
            return Err(invalid("continuity_event_sequence"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        session(&tx, &self.key, scope, &input.session)?;
        let _ = load_source(&tx, &self.key, scope, &input.source)?;
        let job_id: String = tx
            .query_row(
                "SELECT job_id FROM intake_requests WHERE source_id=?1",
                [&input.source.object_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(denied)?;
        let job: IntakeJob = load(&tx, &self.key, "job", &job_id)?;
        if job.actor_id != input.session.connection_id || job.job.source != input.source {
            return Err(denied());
        }
        let session_tag = tagged(
            &self.key,
            b"event-session",
            input.session.session_id.as_bytes(),
        );
        let event_id = tagged(
            &self.key,
            b"event-slot",
            &serde_json::to_vec(&(&input.session, input.sequence))?,
        );
        let source_tag = tagged(
            &self.key,
            b"event-source",
            input.source.object_id.as_bytes(),
        );
        if let Some(existing) = tx
            .query_row(
                "SELECT event_id FROM intake_continuity_events WHERE source_tag=?1",
                [&source_tag],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            if existing != event_id {
                return Err(conflict());
            }
            let previous: SessionEventRecord = load(&tx, &self.key, "continuity_event", &event_id)?;
            if previous.scope != *scope || previous.event.source != input.source {
                return Err(denied());
            }
            return Ok(());
        }
        let count: i64 =
            tx.query_row("SELECT count(*) FROM intake_continuity_events", [], |r| {
                r.get(0)
            })?;
        if count >= 4096 {
            return Err(invalid("continuity_event_limit"));
        }
        tx.execute("INSERT INTO intake_continuity_events(event_id,session_tag,sequence,source_tag) VALUES(?1,?2,?3,?4)",params![event_id,session_tag,input.sequence,source_tag])?;
        insert(
            &tx,
            &self.key,
            "continuity_event",
            &event_id,
            &SessionEventRecord {
                scope: scope.clone(),
                event: input,
            },
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Returns approved memory projections for a known session window. This does not authorize raw
    /// transcript disclosure; missing/uncompiled/withdrawn data remains visibly incomplete.
    pub fn compaction_recap(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: CompactionRecapRequest,
    ) -> Result<CompactionRecap> {
        self.check(scope)?;
        byte_budget(input.max_bytes)?;
        if input.first_sequence == 0
            || input.last_sequence < input.first_sequence
            || input.last_sequence - input.first_sequence >= 64
            || input.last_sequence > i64::MAX as u64
        {
            return Err(invalid("continuity_recap_window"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let allowed = recipient(&tx, &self.key, scope, access, &input.session)?;
        let (policy, _) = continuity_connection(&tx, &self.key, scope, access)?;
        let mut result = CompactionRecap {
            schema_version: CONTINUITY_VERSION.into(),
            session: input.session.clone(),
            first_sequence: input.first_sequence,
            last_sequence: input.last_sequence,
            events: vec![],
            unavailable_sequences: vec![],
            incomplete: true,
            incomplete_reasons: vec!["raw_source_delivery_not_authorized".into()],
            view: "approved_memory_projection_only".into(),
            receipt: read_receipt(&policy, &input.session, "compaction_recap")?,
        };
        let session_tag = tagged(
            &self.key,
            b"event-session",
            input.session.session_id.as_bytes(),
        );
        for seq in input.first_sequence..=input.last_sequence {
            let event_id=tx.query_row("SELECT event_id FROM intake_continuity_events WHERE session_tag=?1 AND sequence=?2",params![session_tag,seq],|r|r.get::<_,String>(0)).optional()?;
            let Some(event_id) = event_id else {
                result.unavailable_sequences.push(seq);
                continue;
            };
            if !exists(&tx, "continuity_event", &event_id)?
                || !consolidation::readable(&tx, &physical_id("continuity_event", &event_id))?
            {
                result.unavailable_sequences.push(seq);
                continue;
            }
            let event: SessionEventRecord = load(&tx, &self.key, "continuity_event", &event_id)?;
            if event.scope != *scope
                || event.event.session != input.session
                || event.event.sequence != seq
            {
                return Err(denied());
            }
            let source = load_source(&tx, &self.key, scope, &event.event.source)?;
            let memories = allowed
                .iter()
                .filter(|u| u.unit.evidence.iter().any(|e| e.source == source.source))
                .map(view)
                .collect::<Vec<_>>();
            if memories.is_empty() {
                result.unavailable_sequences.push(seq);
                continue;
            }
            if source.capture_fidelity == crate::CaptureFidelity::F1AiDigest
                && !result
                    .incomplete_reasons
                    .iter()
                    .any(|r| r == "source_is_ai_digest")
            {
                result.incomplete_reasons.push("source_is_ai_digest".into());
            }
            for m in &memories {
                if !result.receipt.memory_versions.contains(&m.memory) {
                    result.receipt.memory_versions.push(m.memory.clone());
                }
            }
            result.events.push(RecapEvent {
                sequence: seq,
                source: source.source,
                capture_fidelity: source.capture_fidelity,
                memories,
            });
        }
        if !result.unavailable_sequences.is_empty() {
            result
                .incomplete_reasons
                .push("events_missing_or_not_disclosable".into());
        }
        result.receipt.status = "incomplete_projection".into();
        if serde_json::to_vec(&result)?.len() > input.max_bytes {
            return Err(invalid("continuity_byte_budget"));
        }
        result.receipt.remaining_calls = continuity_charge(
            &tx,
            &self.key,
            scope,
            access,
            "compaction_recap",
            &result.receipt.receipt_id,
            result.receipt.memory_versions.len(),
        )?;
        insert(
            &tx,
            &self.key,
            "continuity_read",
            &result.receipt.receipt_id,
            &result.receipt,
        )?;
        tx.commit()?;
        Ok(result)
    }
}

fn list_page(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    target: &SessionRef,
    before: Option<i64>,
) -> Result<HandoffPage> {
    session(c, key, scope, target)?;
    let (_, allowed) = continuity_recipient(c, key, scope, &target.connection_id)?;
    let mut stmt = c.prepare("SELECT rowid,handoff_id FROM intake_continuity_handoffs WHERE recipient_tag=?1 AND rowid<?2 ORDER BY rowid DESC LIMIT 32")?;
    let rows = stmt
        .query_map(
            params![
                tagged(key, b"recipient", target.session_id.as_bytes()),
                before.unwrap_or(i64::MAX)
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let next_before = if rows.len() == 32 {
        rows.last().map(|r| r.0)
    } else {
        None
    };
    let mut items = vec![];
    for (row, id) in rows {
        if !exists(c, "continuity_handoff", &id)?
            || !consolidation::readable(c, &physical_id("continuity_handoff", &id))?
        {
            continue;
        }
        let record: HandoffRecord = load(c, key, "continuity_handoff", &id)?;
        if record.envelope.metadata.recipient != *target {
            return Err(denied());
        }
        if live_handoff(c, key, scope, &record, &allowed).is_ok() {
            items.push((row, record.envelope.metadata));
        }
    }
    Ok(HandoffPage { items, next_before })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedHandoffPage {
    pub page: HandoffPage,
    pub receipt: ContinuityReadReceipt,
}
impl IntakeStore {
    /// Resolve only a session already registered by the trusted owner/adapter. Client input cannot
    /// select an owner or impersonate another connection by constructing a SessionRef.
    pub fn continuity_session_for_access(
        &self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        session_id: &str,
    ) -> Result<SessionRef> {
        self.check(scope)?;
        if !identifier(session_id) {
            return Err(denied());
        }
        continuity_connection(&self.connection, &self.key, scope, access)?;
        let record: SessionRecord = load(
            &self.connection,
            &self.key,
            "continuity_session",
            session_id,
        )?;
        recipient(&self.connection, &self.key, scope, access, &record.session)?;
        Ok(record.session)
    }
    pub fn list_handoffs_connected(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        target: &SessionRef,
        before: Option<i64>,
    ) -> Result<ConnectedHandoffPage> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recipient(&tx, &self.key, scope, access, target)?;
        let (policy, _) = continuity_connection(&tx, &self.key, scope, access)?;
        let page = list_page(&tx, &self.key, scope, target, before)?;
        let mut receipt = read_receipt(&policy, target, "list_handoffs")?;
        receipt.status = "metadata_only".into();
        receipt.remaining_calls = continuity_charge(
            &tx,
            &self.key,
            scope,
            access,
            "list_handoffs",
            &receipt.receipt_id,
            page.items.len(),
        )?;
        insert(
            &tx,
            &self.key,
            "continuity_read",
            &receipt.receipt_id,
            &receipt,
        )?;
        tx.commit()?;
        Ok(ConnectedHandoffPage { page, receipt })
    }
}

/// Owner inventory includes closed/expired registrations without restoring their authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuitySessionInfo {
    pub session: SessionRef,
    pub scope: AssetScope,
    pub expires_at: String,
    pub closed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuitySessionPage {
    pub items: Vec<(i64, ContinuitySessionInfo)>,
    pub next_before: Option<i64>,
}
impl IntakeStore {
    pub fn continuity_session_info(
        &self,
        scope: &AssetScope,
        session_id: &str,
    ) -> Result<ContinuitySessionInfo> {
        self.check(scope)?;
        require(&self.connection)?;
        if !identifier(session_id) {
            return Err(denied());
        }
        let record: SessionRecord = load(
            &self.connection,
            &self.key,
            "continuity_session",
            session_id,
        )?;
        if record.scope != *scope || record.session.revision != 1 {
            return Err(denied());
        }
        Ok(ContinuitySessionInfo {
            session: record.session,
            scope: record.scope,
            expires_at: record.expires_at,
            closed: record.closed,
        })
    }
    pub fn list_continuity_sessions(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<ContinuitySessionPage> {
        self.check(scope)?;
        require(&self.connection)?;
        let mut stmt=self.connection.prepare("SELECT rowid,substr(id,length('n2a:continuity_session:')+1) FROM intake_objects WHERE kind='continuity_session' AND rowid<?1 ORDER BY rowid DESC LIMIT 32")?;
        let rows = stmt
            .query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next_before = if rows.len() == 32 {
            rows.last().map(|r| r.0)
        } else {
            None
        };
        let mut items = vec![];
        for (row, id) in rows {
            items.push((row, self.continuity_session_info(scope, &id)?));
        }
        Ok(ContinuitySessionPage { items, next_before })
    }
}

fn register_session_in(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    input: SessionRegistration,
) -> Result<SessionRef> {
    lifetime(input.lifetime_seconds)?;
    if !identifier(&input.request_id) || !identifier(&input.connection_id) {
        return Err(invalid("continuity_session_input"));
    }
    require(c)?;
    let (policy, _) = continuity_recipient(c, key, scope, &input.connection_id)?;
    let sid = tagged(key, b"session", input.request_id.as_bytes());
    let fingerprint = tagged(key, b"session-request", &serde_json::to_vec(&input)?);
    if exists(c, "continuity_session", &sid)? {
        let prior: SessionRecord = load(c, key, "continuity_session", &sid)?;
        if prior.fingerprint != fingerprint {
            return Err(conflict());
        }
        session(c, key, scope, &prior.session)?;
        return Ok(prior.session);
    }
    let count: i64 = c.query_row(
        "SELECT count(*) FROM intake_objects WHERE kind='continuity_session'",
        [],
        |r| r.get(0),
    )?;
    if count >= MAX_RECORDS {
        return Err(invalid("continuity_session_limit"));
    }
    let expires = (OffsetDateTime::now_utc() + Duration::seconds(input.lifetime_seconds.into()))
        .min(parse_time(&policy.expires_at)?);
    let record = SessionRecord {
        scope: scope.clone(),
        session: SessionRef {
            session_id: sid,
            revision: 1,
            connection_id: policy.id,
        },
        fingerprint,
        expires_at: format_time(expires)?,
        closed: false,
    };
    insert(
        c,
        key,
        "continuity_session",
        &record.session.session_id,
        &record,
    )?;
    Ok(record.session)
}

fn create_handoff_in(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    input: CreateHandoff,
) -> Result<HandoffMetadata> {
    lifetime(input.lifetime_seconds)?;
    context(&input.content)?;
    if !identifier(&input.request_id) || input.from == input.recipient {
        return Err(invalid("continuity_handoff_input"));
    }
    let from = session(c, key, scope, &input.from)?;
    let to = session(c, key, scope, &input.recipient)?;
    let (_, allowed) = continuity_recipient(c, key, scope, &input.recipient.connection_id)?;
    refs_allowed(&input.memories, &allowed)?;
    let hid = tagged(key, b"handoff", input.request_id.as_bytes());
    let fingerprint = tagged(key, b"handoff-request", &serde_json::to_vec(&input)?);
    if exists(c, "continuity_handoff", &hid)? {
        let previous: HandoffRecord = load(c, key, "continuity_handoff", &hid)?;
        if previous.fingerprint != fingerprint {
            return Err(conflict());
        }
        live_handoff(c, key, scope, &previous, &allowed)?;
        return Ok(previous.envelope.metadata);
    }
    // A retained index is a tombstone: deletion cannot resurrect the same request.
    if c.query_row(
        "SELECT 1 FROM intake_continuity_handoffs WHERE handoff_id=?1",
        [&hid],
        |_| Ok(()),
    )
    .optional()?
    .is_some()
    {
        return Err(denied());
    }
    let count: i64 = c.query_row("SELECT count(*) FROM intake_continuity_handoffs", [], |r| {
        r.get(0)
    })?;
    if count >= MAX_RECORDS {
        return Err(invalid("continuity_handoff_limit"));
    }
    let at = OffsetDateTime::now_utc();
    let expires = (at + Duration::seconds(input.lifetime_seconds.into()))
        .min(parse_time(&from.expires_at)?)
        .min(parse_time(&to.expires_at)?);
    let metadata = HandoffMetadata {
        schema_version: CONTINUITY_VERSION.into(),
        handoff_id: hid,
        revision: 1,
        scope: scope.clone(),
        from: input.from,
        recipient: input.recipient,
        created_at: format_time(at)?,
        expires_at: format_time(expires)?,
        state: HandoffState::Pending,
    };
    let record = HandoffRecord {
        envelope: HandoffEnvelope {
            metadata: metadata.clone(),
            memories: input.memories,
            content: input.content,
            interpretation: "task_context_unverified".into(),
            confers_execution_authority: false,
            owner_notes: vec![],
            supersedes: None,
        },
        fingerprint,
        claim: None,
    };
    insert(c, key, "continuity_handoff", &metadata.handoff_id, &record)?;
    c.execute(
        "INSERT INTO intake_continuity_handoffs VALUES (?1,?2)",
        params![
            metadata.handoff_id,
            tagged(key, b"recipient", metadata.recipient.session_id.as_bytes())
        ],
    )?;
    Ok(metadata)
}
