//! N3 connection policy and disclosure boundary for compiled, explicitly unverified memories.
use super::{compiler::*, consolidation, storage::*, *};
use crate::{CaptureFidelity, VaultKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use time::{Duration, OffsetDateTime};

const MAX_MEMBERS: usize = 128;
const LIFETIME_SECONDS: i64 = 86400;
const CALL_BUDGET: u64 = 1000;
const PER_MINUTE: u32 = 30;
#[path = "temporal.rs"]
mod temporal;
pub use temporal::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPreset {
    ReadOnly,
    Topic,
    CaptureRead,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionHost {
    Chatgpt,
    Codex,
    Claude,
    Workbuddy,
    Grok,
    Cursor,
    Other,
}
impl ConnectionHost {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chatgpt => "chatgpt",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Workbuddy => "workbuddy",
            Self::Grok => "grok",
            Self::Cursor => "cursor",
            Self::Other => "other",
        }
    }
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT",
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Workbuddy => "WorkBuddy",
            Self::Grok => "Grok",
            Self::Cursor => "Cursor",
            Self::Other => "MCP 客户端",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRequest {
    pub request_id: String,
    pub label: String,
    pub host: ConnectionHost,
    pub preset: ConnectionPreset,
    pub topic_id: Option<String>,
    pub include_future: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConnection {
    pub id: String,
    pub scope: AssetScope,
    pub label: String,
    pub host: ConnectionHost,
    pub preset: ConnectionPreset,
    pub topic_id: Option<String>,
    pub include_future: bool,
    pub member_snapshot: Vec<RevisionRef>,
    pub created_at: String,
    pub expires_at: String,
    pub revoked: bool,
    pub used_calls: u64,
    pub call_budget: u64,
    pub last_used_at: Option<String>,
    pub window_started_at: String,
    pub window_calls: u32,
}
#[derive(Serialize, Deserialize)]
struct ConnectionRecord {
    policy: MemoryConnection,
    token_digest: String,
}
/// Authenticated by the store, never constructed from MCP arguments or clientInfo.
#[derive(Clone)]
pub struct ConnectionAccess {
    scope: AssetScope,
    connection_id: String,
    token_digest: String,
}
impl ConnectionAccess {
    /// Store-authenticated identity, for binding private local setup to its selected connection.
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }
}
impl std::fmt::Debug for ConnectionAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionAccess([REDACTED])")
    }
}
pub struct IssuedConnection {
    pub connection: MemoryConnection,
    pub token: String,
}
/// A durable acceptance receipt plus the observed job state, never a semantic-quality claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedCapture {
    pub receipt: IntakeReceipt,
    pub processing: JobState,
    pub replayed: bool,
}

fn capture_delivery(
    c: &Connection,
    key: &VaultKey,
    receipt: IntakeReceipt,
    actor_id: &str,
    replayed: bool,
) -> Result<ConnectedCapture> {
    // An old saved receipt cannot revive a withdrawn source or claim it is queued again.
    if exists(c, "source_withdrawal", &receipt.source.object_id)? {
        return Err(denied());
    }
    let job: IntakeJob = load(c, key, "job", &receipt.job_id)?;
    if job.actor_id != actor_id || job.job.source != receipt.source {
        return Err(denied());
    }
    Ok(ConnectedCapture {
        receipt,
        processing: job.job.state,
        replayed,
    })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectedQuery {
    pub request_id: String,
    pub query: Option<String>,
    pub memory_id: Option<String>,
    pub max_items: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMetadata {
    pub source: RevisionRef,
    pub platform: String,
    pub role: SourceRole,
    pub capture_fidelity: CaptureFidelity,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveredMemory {
    pub memory: RevisionRef,
    pub members: Vec<RevisionRef>,
    pub content: SemanticContent,
    pub origin: SemanticOrigin,
    pub valid_time: ValidTime,
    pub occurred_at: Option<String>,
    pub protected: bool,
    pub sources: Vec<SourceMetadata>,
    pub semantic_support: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionReceipt {
    pub receipt_id: String,
    pub connection_id: String,
    pub created_at: String,
    pub purpose: String,
    pub view: String,
    pub memory_versions: Vec<RevisionRef>,
    pub disclosed_fields: Vec<String>,
    pub status: String,
    pub remaining_calls: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedDelivery {
    pub items: Vec<DeliveredMemory>,
    pub more_available: bool,
    pub receipt: ConnectionReceipt,
}
#[derive(Serialize, Deserialize)]
struct CachedCall {
    fingerprint: String,
    result: Value,
}

enum QueryView {
    Full,
    CurrentRevision(u64),
    Ranked,
    Index,
    Neighborhood { revision: u64, window_days: u8 },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionUsage {
    pub event_id: String,
    pub connection_id: String,
    pub operation: String,
    pub at: String,
    pub item_count: usize,
    pub receipt_id: String,
}
fn enabled(c: &Connection) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='intake_connection_tokens'",
        [],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}
fn require(c: &Connection) -> Result<()> {
    if enabled(c)? {
        Ok(())
    } else {
        Err(invalid("connections_not_enabled"))
    }
}
fn digest(key: &VaultKey, scope: &AssetScope, token: &str) -> String {
    key.authentication_tag(&[
        b"n3:client",
        scope.owner_id.as_bytes(),
        scope.space.as_bytes(),
        token.as_bytes(),
    ])
}
fn candidate_ids(c: &Connection, topic: Option<&str>) -> Result<Vec<String>> {
    let mut q=c.prepare("SELECT unit_id FROM intake_related WHERE (?1 IS NULL OR bucket=?1) AND NOT EXISTS (SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id='n2a:unit_head:'||intake_related.unit_id) ORDER BY rowid DESC LIMIT 129")?;
    Ok(q.query_map([topic], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}
fn allowed(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    p: &MemoryConnection,
) -> Result<Vec<CompiledUnit>> {
    let candidates = if p.include_future {
        candidate_ids(c, p.topic_id.as_deref())?
    } else {
        p.member_snapshot
            .iter()
            .map(|r| r.object_id.clone())
            .collect()
    };
    if candidates.len() > MAX_MEMBERS {
        return Err(invalid("connection_scope_limit"));
    }
    let mut result = vec![];
    for id in candidates {
        if !consolidation::readable(c, &physical_id("unit_head", &id))? {
            continue;
        }
        let u = read_unit(c, key, scope, &id)?;
        if u.unit.header.state != UnitState::Active
            || !matches!(
                u.unit.header.privacy_level,
                PrivacyLevel::Private | PrivacyLevel::Shareable
            )
        {
            continue;
        }
        if !p.include_future && !p.member_snapshot.contains(&u.unit.header.object) {
            continue;
        }
        result.push(u);
    }
    Ok(result)
}
fn current(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    access: &ConnectionAccess,
) -> Result<ConnectionRecord> {
    super::recovery::require_automation(c, key)?;
    if access.scope != *scope {
        return Err(denied());
    }
    require(c)?;
    let record: ConnectionRecord = load(c, key, "connection", &access.connection_id)?;
    if record.policy.scope != *scope
        || record.token_digest != access.token_digest
        || record.policy.revoked
        || (record.policy.include_future && record.policy.topic_id.is_some())
        || parse_time(&record.policy.expires_at)? <= OffsetDateTime::now_utc()
    {
        return Err(denied());
    }
    Ok(record)
}
/// Continuity uses the same live authority and approved revision set as N3 queries.
pub(super) fn continuity_connection(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    access: &ConnectionAccess,
) -> Result<(MemoryConnection, Vec<CompiledUnit>)> {
    let record = current(c, key, scope, access)?;
    let units = allowed(c, key, scope, &record.policy)?;
    Ok((record.policy, units))
}

/// Owner-only creation checks the explicitly selected recipient, without minting client authority.
pub(super) fn continuity_recipient(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    connection_id: &str,
) -> Result<(MemoryConnection, Vec<CompiledUnit>)> {
    require(c)?;
    let record: ConnectionRecord = load(c, key, "connection", connection_id)?;
    let access = ConnectionAccess {
        scope: scope.clone(),
        connection_id: connection_id.into(),
        token_digest: record.token_digest,
    };
    continuity_connection(c, key, scope, &access)
}

/// Charge only a new successful continuity delivery, inside the caller's transaction.
pub(super) fn continuity_charge(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    access: &ConnectionAccess,
    operation: &str,
    receipt_id: &str,
    item_count: usize,
) -> Result<u64> {
    let mut record = current(c, key, scope, access)?;
    charge(&mut record)?;
    let event = ConnectionUsage {
        event_id: id("usage"),
        connection_id: record.policy.id.clone(),
        operation: operation.into(),
        at: now()?,
        item_count,
        receipt_id: receipt_id.into(),
    };
    insert(c, key, "connection_usage", &event.event_id, &event)?;
    update(c, key, "connection", &record.policy.id, &record)?;
    Ok(record.policy.call_budget - record.policy.used_calls)
}

fn charge(record: &mut ConnectionRecord) -> Result<()> {
    let now = OffsetDateTime::now_utc();
    if now - parse_time(&record.policy.window_started_at)? >= Duration::minutes(1) {
        record.policy.window_started_at = super::storage::now()?;
        record.policy.window_calls = 0;
    }
    if record.policy.used_calls >= record.policy.call_budget
        || record.policy.window_calls >= PER_MINUTE
    {
        return Err(invalid("connection_call_budget"));
    }
    record.policy.used_calls += 1;
    record.policy.window_calls += 1;
    record.policy.last_used_at = Some(super::storage::now()?);
    Ok(())
}
fn call_key(key: &VaultKey, connection: &str, operation: &str, request: &str) -> String {
    key.authentication_tag(&[
        b"n3:call",
        connection.as_bytes(),
        operation.as_bytes(),
        request.as_bytes(),
    ])
}
fn saved_call(
    c: &Connection,
    key: &VaultKey,
    cache: &str,
    fingerprint: &str,
) -> Result<Option<Value>> {
    let existing: Option<String> = c
        .query_row(
            "SELECT fingerprint FROM intake_connection_calls WHERE call_key=?1",
            [cache],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = &existing
        && old != fingerprint
    {
        return Err(conflict());
    }
    let previous: Option<CachedCall> = optional(c, key, "connection_call", cache)?;
    if existing.is_some() && previous.is_none() {
        return Err(denied());
    }
    if let Some(old) = previous {
        if old.fingerprint != fingerprint {
            return Err(conflict());
        }
        Ok(Some(old.result))
    } else {
        Ok(None)
    }
}
struct CallUsage {
    operation: &'static str,
    count: usize,
    receipt_id: String,
}
fn store_call(
    c: &Connection,
    key: &VaultKey,
    record: &ConnectionRecord,
    cache: &str,
    fingerprint: String,
    result: Value,
    usage: CallUsage,
) -> Result<()> {
    let CallUsage {
        operation,
        count,
        receipt_id,
    } = usage;
    c.execute(
        "INSERT INTO intake_connection_calls VALUES (?1,?2)",
        params![cache, fingerprint],
    )?;
    insert(
        c,
        key,
        "connection_call",
        cache,
        &CachedCall {
            fingerprint,
            result,
        },
    )?;
    let event = ConnectionUsage {
        event_id: id("usage"),
        connection_id: record.policy.id.clone(),
        operation: operation.into(),
        at: now()?,
        item_count: count,
        receipt_id,
    };
    insert(c, key, "connection_usage", &event.event_id, &event)?;
    update(c, key, "connection", &record.policy.id, record)
}
impl IntakeStore {
    /// Explicit runtime opt-in. Never processes or migrates a legacy Vault.
    pub fn enable_connections(&mut self, scope: &AssetScope) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        consolidation::initialize(&tx, &self.key)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS intake_connection_tokens(digest TEXT PRIMARY KEY,connection_id TEXT NOT NULL UNIQUE,issue_key TEXT NOT NULL UNIQUE); CREATE TABLE IF NOT EXISTS intake_connection_calls(call_key TEXT PRIMARY KEY,fingerprint TEXT NOT NULL)")?;
        tx.commit()?;
        Ok(())
    }
    pub fn create_connection(
        &mut self,
        scope: &AssetScope,
        input: ConnectionRequest,
    ) -> Result<IssuedConnection> {
        self.create_connection_with_lifetime(scope, input, LIFETIME_SECONDS as u32)
    }
    /// Owner-selected lifetime may shorten, but never extend, the default policy.
    /// Expiry is enforced by the store even if the issuing UI or harness exits.
    pub fn create_connection_with_lifetime(
        &mut self,
        scope: &AssetScope,
        input: ConnectionRequest,
        lifetime_seconds: u32,
    ) -> Result<IssuedConnection> {
        self.check(scope)?;
        if !(1..=LIFETIME_SECONDS as u32).contains(&lifetime_seconds) {
            return Err(invalid("connection_lifetime"));
        }
        if !identifier(&input.request_id)
            || input.label.trim().is_empty()
            || input.label.len() > 120
            || input.label.chars().any(char::is_control)
            || (input.preset == ConnectionPreset::Topic && input.topic_id.is_none())
        {
            return Err(invalid("connection_selection"));
        }
        if input.include_future && input.topic_id.is_some() {
            return Err(invalid("future_topic_requires_trusted_membership"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        require(&tx)?;
        let issue_key = self
            .key
            .authentication_tag(&[b"n3:issue", input.request_id.as_bytes()]);
        if tx
            .query_row(
                "SELECT 1 FROM intake_connection_tokens WHERE issue_key=?1",
                [&issue_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(invalid(
                "connection_already_created_credential_not_retained",
            ));
        }
        let count: i64 =
            tx.query_row("SELECT count(*) FROM intake_connection_tokens", [], |r| {
                r.get(0)
            })?;
        if count >= 64 {
            return Err(invalid("connection_count_limit"));
        }
        if let Some(topic) = &input.topic_id
            && tx
                .query_row(
                    "SELECT 1 FROM intake_dirty WHERE bucket=?1",
                    [topic],
                    |_| Ok(()),
                )
                .optional()?
                .is_none()
        {
            return Err(denied());
        }
        let ids = candidate_ids(&tx, input.topic_id.as_deref())?;
        if ids.len() > MAX_MEMBERS {
            return Err(invalid("connection_scope_limit"));
        }
        let mut members = vec![];
        for id in ids {
            let u = read_unit(&tx, &self.key, scope, &id)?;
            if u.unit.header.state == UnitState::Active
                && matches!(
                    u.unit.header.privacy_level,
                    PrivacyLevel::Private | PrivacyLevel::Shareable
                )
            {
                members.push(u.unit.header.object);
            }
        }
        // First save must not require blanket future-read consent. An empty capture/read
        // snapshot permits capture only in practice; newly compiled units remain unreadable.
        if members.is_empty()
            && !input.include_future
            && input.preset != ConnectionPreset::CaptureRead
        {
            return Err(invalid("connection_empty_snapshot"));
        }
        let at = now()?;
        let token = VaultKey::generate()?.to_hex();
        let tag = digest(&self.key, scope, &token);
        let policy = MemoryConnection {
            id: id("connection"),
            scope: scope.clone(),
            label: input.label.trim().into(),
            host: input.host,
            preset: input.preset,
            topic_id: input.topic_id,
            include_future: input.include_future,
            member_snapshot: members,
            created_at: at.clone(),
            expires_at: (parse_time(&at)? + Duration::seconds(i64::from(lifetime_seconds)))
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid("connection_time"))?,
            revoked: false,
            used_calls: 0,
            call_budget: CALL_BUDGET,
            last_used_at: None,
            window_started_at: at,
            window_calls: 0,
        };
        insert(
            &tx,
            &self.key,
            "connection",
            &policy.id,
            &ConnectionRecord {
                policy: policy.clone(),
                token_digest: tag.clone(),
            },
        )?;
        tx.execute(
            "INSERT INTO intake_connection_tokens VALUES (?1,?2,?3)",
            params![tag, policy.id, issue_key],
        )?;
        tx.commit()?;
        Ok(IssuedConnection {
            connection: policy,
            token,
        })
    }
    pub fn connections(&self, scope: &AssetScope) -> Result<Vec<MemoryConnection>> {
        self.check(scope)?;
        if !enabled(&self.connection)? {
            return Ok(vec![]);
        }
        let ids = {
            let mut q = self.connection.prepare(
                "SELECT connection_id FROM intake_connection_tokens ORDER BY rowid DESC LIMIT 64",
            )?;
            q.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        ids.iter()
            .map(|id| {
                let r: ConnectionRecord = load(&self.connection, &self.key, "connection", id)?;
                if r.policy.scope != *scope {
                    return Err(denied());
                }
                Ok(r.policy)
            })
            .collect()
    }
    /// Runtime credential separation also recognizes revoked credentials; this is not an auth grant.
    pub fn contains_connection_credential(&self, scope: &AssetScope, value: &str) -> Result<bool> {
        self.check(scope)?;
        if !enabled(&self.connection)? || value.len() != 64 {
            return Ok(false);
        }
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM intake_connection_tokens WHERE digest=?1",
                [digest(&self.key, scope, &value.to_ascii_lowercase())],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub fn authenticate_connection(
        &self,
        scope: &AssetScope,
        token: &str,
    ) -> Result<ConnectionAccess> {
        self.check(scope)?;
        require(&self.connection)?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(denied());
        }
        let tag = digest(&self.key, scope, token);
        let id: String = self
            .connection
            .query_row(
                "SELECT connection_id FROM intake_connection_tokens WHERE digest=?1",
                [&tag],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(denied)?;
        let access = ConnectionAccess {
            scope: scope.clone(),
            connection_id: id,
            token_digest: tag,
        };
        current(&self.connection, &self.key, scope, &access)?;
        Ok(access)
    }
    pub fn revoke_connection(&mut self, scope: &AssetScope, id: &str) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let mut record: ConnectionRecord = load(&tx, &self.key, "connection", id)?;
        if record.policy.scope != *scope {
            return Err(denied());
        }
        record.policy.revoked = true;
        update(&tx, &self.key, "connection", id, &record)?;
        tx.commit()?;
        Ok(())
    }
    pub fn connection_usage(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<Vec<(i64, ConnectionUsage)>> {
        self.check(scope)?;
        let mut q=self.connection.prepare("SELECT rowid,substr(id,length('n2a:connection_usage:')+1) FROM intake_objects WHERE kind='connection_usage' AND rowid<?1 ORDER BY rowid DESC LIMIT 50")?;
        let ids = q
            .query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|(seq, id)| {
                Ok((
                    seq,
                    load(&self.connection, &self.key, "connection_usage", &id)?,
                ))
            })
            .collect()
    }
    pub fn query_connected(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: ConnectedQuery,
    ) -> Result<ConnectedDelivery> {
        self.query_connected_internal(
            scope,
            access,
            input,
            QueryView::Full,
            std::convert::identity,
        )
    }

    /// Pin a detail read to the current permitted revision. A stale pin is denied in the read transaction.
    pub fn query_connected_current_revision(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        request_id: String,
        memory: RevisionRef,
    ) -> Result<ConnectedDelivery> {
        let input = ConnectedQuery {
            request_id,
            query: None,
            memory_id: Some(memory.object_id),
            max_items: 1,
        };
        self.query_connected_internal(
            scope,
            access,
            input,
            QueryView::CurrentRevision(memory.revision),
            std::convert::identity,
        )
    }

    pub fn query_connected_index(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: ConnectedQuery,
    ) -> Result<ConnectedIndexDelivery> {
        self.query_connected_internal(
            scope,
            access,
            input,
            QueryView::Index,
            super::retrieval::index,
        )
    }

    /// Locate nearby current memories with the same subject and an explicit occurrence time.
    pub fn query_connected_neighborhood(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        request_id: String,
        anchor: RevisionRef,
        window_days: u8,
        max_items: usize,
    ) -> Result<ConnectedIndexDelivery> {
        if !(1..=30).contains(&window_days) {
            return Err(invalid("neighborhood_window_days"));
        }
        let input = ConnectedQuery {
            request_id,
            query: None,
            memory_id: Some(anchor.object_id),
            max_items,
        };
        self.query_connected_internal(
            scope,
            access,
            input,
            QueryView::Neighborhood {
                revision: anchor.revision,
                window_days,
            },
            |delivery| {
                let mut index = super::retrieval::index(delivery);
                index.receipt.view = "compiled_unverified_neighborhood".into();
                index
            },
        )
    }

    /// Opt-in lexical relevance. Existing query_connected keeps substring behavior.
    pub fn query_connected_ranked(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: ConnectedQuery,
    ) -> Result<RankedConnectedDelivery> {
        let query = input
            .query
            .clone()
            .ok_or_else(|| invalid("ranked_query_required"))?;
        let delivery = self.query_connected_internal(
            scope,
            access,
            input,
            QueryView::Ranked,
            std::convert::identity,
        )?;
        super::retrieval::explain(&query, delivery)
    }

    fn query_connected_internal<T: Serialize + serde::de::DeserializeOwned>(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: ConnectedQuery,
        view: QueryView,
        project: impl FnOnce(ConnectedDelivery) -> T,
    ) -> Result<T> {
        self.check(scope)?;
        if !identifier(&input.request_id)
            || !(1..=8).contains(&input.max_items)
            || input
                .query
                .as_ref()
                .is_some_and(|q| q.trim().is_empty() || q.len() > 1000)
            || input.memory_id.as_ref().is_some_and(|id| !identifier(id))
            || input.query.is_some() == input.memory_id.is_some()
        {
            return Err(invalid("connected_query"));
        }
        let ranking = if matches!(view, QueryView::Ranked | QueryView::Index) {
            Some(super::retrieval::LexicalQuery::new(
                input
                    .query
                    .as_deref()
                    .ok_or_else(|| invalid("ranked_query_required"))?,
            )?)
        } else {
            None
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = current(&tx, &self.key, scope, access)?;
        let (operation, domain): (&str, &[u8]) = match view {
            QueryView::Full => ("query", b"n3:query"),
            QueryView::CurrentRevision(_) => {
                ("query_current_revision_v01", b"n3:current_revision:v0.1")
            }
            QueryView::Ranked => ("query_ranked_v01", b"n3:ranked_query:lexical-cjk:v0.1"),
            QueryView::Index => ("query_index_v01", b"n3:index:lexical-cjk:v0.1"),
            QueryView::Neighborhood { .. } => ("query_neighborhood_v01", b"n3:neighborhood:v0.1"),
        };
        let permitted = allowed(&tx, &self.key, scope, &record.policy)?;
        let mut fingerprint_input = serde_json::to_vec(&input)?;
        let pinned = match view {
            QueryView::CurrentRevision(revision) | QueryView::Neighborhood { revision, .. } => {
                let selected = permitted
                    .iter()
                    .find(|u| {
                        let memory = &u.unit.header.object;
                        Some(&memory.object_id) == input.memory_id.as_ref()
                            && memory.revision == revision
                            && revision != 0
                    })
                    .ok_or_else(denied)?;
                fingerprint_input.extend_from_slice(&revision.to_be_bytes());
                Some(&selected.unit)
            }
            _ => None,
        };
        let neighborhood = match view {
            QueryView::Neighborhood { window_days, .. } => {
                fingerprint_input.push(window_days);
                let anchor = pinned.ok_or_else(conflict)?;
                Some(super::retrieval::Neighborhood::new(anchor, window_days)?)
            }
            _ => None,
        };
        let fingerprint = self.key.authentication_tag(&[domain, &fingerprint_input]);
        let cache = call_key(&self.key, &record.policy.id, operation, &input.request_id);
        if let Some(old) = saved_call(&tx, &self.key, &cache, &fingerprint)? {
            let receipt: ConnectionReceipt =
                serde_json::from_value(old.get("receipt").ok_or_else(conflict)?.clone())?;
            for r in &receipt.memory_versions {
                if !permitted.iter().any(|u| u.unit.header.object == *r) {
                    return Err(denied());
                }
            }
            return Ok(serde_json::from_value(old)?);
        }
        charge(&mut record)?;
        let mut groups: BTreeMap<String, DeliveredMemory> = BTreeMap::new();
        for u in permitted {
            if let Some(neighborhood) = &neighborhood {
                if !neighborhood.includes(&u.unit)? {
                    continue;
                }
            } else if input
                .memory_id
                .as_ref()
                .is_some_and(|id| u.unit.header.object.object_id != *id)
            {
                continue;
            }
            if let Some(ranking) = &ranking {
                if ranking.score(&u.unit.content)?.0 == 0 {
                    continue;
                }
            } else if let Some(q) = &input.query {
                let haystack = serde_json::to_string(&u.unit.content)?.to_lowercase();
                if !haystack.contains(&q.to_lowercase()) {
                    continue;
                }
            }
            let mut roles = BTreeSet::new();
            let mut sources = vec![];
            for e in &u.unit.evidence {
                let source = load_source(&tx, &self.key, scope, &e.source)?;
                roles.insert(
                    serde_json::to_value(source.role)?
                        .as_str()
                        .ok_or_else(conflict)?
                        .to_string(),
                );
                if !sources
                    .iter()
                    .any(|s: &SourceMetadata| s.source == source.source)
                {
                    sources.push(SourceMetadata {
                        source: source.source,
                        platform: source.source_platform,
                        role: source.role,
                        capture_fidelity: source.capture_fidelity,
                    });
                }
            }
            let group = consolidation::exact_key(&self.key, &u.unit, &roles)?;
            if let Some(item) = groups.get_mut(&group) {
                item.members.push(u.unit.header.object);
                item.protected |= u.protected;
                for source in sources {
                    if !item.sources.iter().any(|s| s.source == source.source) {
                        item.sources.push(source);
                    }
                }
            } else {
                groups.insert(
                    group,
                    DeliveredMemory {
                        memory: u.unit.header.object.clone(),
                        members: vec![u.unit.header.object],
                        content: u.unit.content,
                        origin: u.unit.origin,
                        valid_time: u.unit.valid_time,
                        occurred_at: u.unit.occurred_at,
                        protected: u.protected,
                        sources,
                        semantic_support: "model_output_unverified".into(),
                    },
                );
            }
        }
        let more_available = groups.len() > input.max_items;
        let items = if let Some(ranking) = &ranking {
            let mut scored = groups
                .into_iter()
                .map(|(key, item)| Ok((ranking.score(&item.content)?.0, key, item)))
                .collect::<Result<Vec<_>>>()?;
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            scored
                .into_iter()
                .take(input.max_items)
                .map(|(_, _, item)| item)
                .collect::<Vec<_>>()
        } else if let Some(neighborhood) = &neighborhood {
            neighborhood.nearest(groups, input.max_items)?
        } else {
            groups
                .into_values()
                .take(input.max_items)
                .collect::<Vec<_>>()
        };
        let mut receipt = ConnectionReceipt {
            receipt_id: id("disclosure"),
            connection_id: record.policy.id.clone(),
            created_at: now()?,
            purpose: "personal_memory_continuity".into(),
            view: "compiled_unverified".into(),
            memory_versions: items.iter().flat_map(|i| i.members.clone()).collect(),
            disclosed_fields: vec![
                "content".into(),
                "origin".into(),
                "time".into(),
                "source_metadata".into(),
                "protected".into(),
            ],
            status: if items.is_empty() {
                "empty"
            } else {
                "delivered"
            }
            .into(),
            remaining_calls: record.policy.call_budget - record.policy.used_calls,
        };
        if matches!(view, QueryView::Ranked) {
            receipt.view = "compiled_unverified_ranked".into();
            receipt.disclosed_fields.push("lexical_trace".into());
        }
        if let Some(neighborhood) = &neighborhood {
            // The anchor is a dependency of the projection even when the result is empty.
            receipt
                .memory_versions
                .insert(0, neighborhood.anchor.clone());
            if neighborhood.time_unknown() {
                receipt.status = "anchor_time_unknown".into();
            }
        }
        let result = ConnectedDelivery {
            items,
            more_available,
            receipt,
        };
        let count = result.items.len();
        let receipt_id = result.receipt.receipt_id.clone();
        if matches!(view, QueryView::Ranked)
            && serde_json::to_vec(&super::retrieval::explain(
                input
                    .query
                    .as_deref()
                    .ok_or_else(|| invalid("ranked_query_required"))?,
                result.clone(),
            )?)?
            .len()
                > 32 * 1024
        {
            return Err(invalid("context_byte_budget"));
        }
        let result = project(result);
        let value = serde_json::to_value(&result)?;
        let index_only = matches!(view, QueryView::Index | QueryView::Neighborhood { .. });
        let max_bytes = if index_only { 8 * 1024 } else { 32 * 1024 };
        if serde_json::to_vec(&value)?.len() > max_bytes {
            return Err(invalid("context_byte_budget"));
        }
        store_call(
            &tx,
            &self.key,
            &record,
            &cache,
            fingerprint,
            value,
            CallUsage {
                operation: if index_only { "read_index" } else { "read" },
                count,
                receipt_id,
            },
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub fn capture_connected(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        request_id: &str,
        content: &str,
        user_requested_save: bool,
    ) -> Result<IntakeReceipt> {
        Ok(self
            .capture_connected_status(scope, access, request_id, content, user_requested_save)?
            .receipt)
    }
    /// Rechecks live connection authority on retries and observes status in the same transaction.
    pub fn capture_connected_status(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        request_id: &str,
        content: &str,
        user_requested_save: bool,
    ) -> Result<ConnectedCapture> {
        self.check(scope)?;
        if !user_requested_save || !identifier(request_id) {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = current(&tx, &self.key, scope, access)?;
        if record.policy.preset != ConnectionPreset::CaptureRead {
            return Err(denied());
        }
        let fingerprint = self
            .key
            .authentication_tag(&[b"n3:save", content.as_bytes()]);
        let cache = call_key(&self.key, &record.policy.id, "save", request_id);
        if let Some(old) = saved_call(&tx, &self.key, &cache, &fingerprint)? {
            return capture_delivery(
                &tx,
                &self.key,
                serde_json::from_value(old)?,
                &record.policy.id,
                true,
            );
        }
        charge(&mut record)?;
        let request = IntakeRequest {
            schema_version: INTAKE_VERSION.into(),
            request_id: cache.clone(),
            source_platform: format!("mcp-{}", record.policy.host.as_str()),
            capture_fidelity: CaptureFidelity::F1AiDigest,
            content: content.into(),
            occurred_at: None,
        };
        request.validate()?;
        let authority = IntakeAuthority {
            scope: scope.clone(),
            actor_id: record.policy.id.clone(),
            capture_allowed: true,
            role: SourceRole::Mixed,
            privacy_level: PrivacyLevel::Private,
        };
        let receipt = receive_tx(&tx, &self.key, scope, &authority, &request, None)?;
        store_call(
            &tx,
            &self.key,
            &record,
            &cache,
            fingerprint,
            json!(receipt),
            CallUsage {
                operation: "capture",
                count: 0,
                receipt_id: receipt.job_id.clone(),
            },
        )?;
        let delivery = capture_delivery(&tx, &self.key, receipt, &record.policy.id, false)?;
        tx.commit()?;
        Ok(delivery)
    }
}

impl IntakeStore {
    /// Owner inventory reports actual live records, not model quality or an intelligence score.
    pub fn owner_inventory(&self, scope: &AssetScope) -> Result<Value> {
        self.check(scope)?;
        require(&self.connection)?;
        let count = |kind: &str| -> Result<u64> {
            Ok(self.connection.query_row("SELECT count(*) FROM intake_objects o WHERE o.kind=?1 AND NOT EXISTS(SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id=o.id)",[kind],|r|r.get(0))?)
        };
        Ok(
            json!({"sources":count("source")?,"extraction_records":count("unit_head")?,"wiki_pages":count("wiki")?,"configured_connections":self.connections(scope)?.len(),"consolidation":self.consolidation_queue(scope)?,"evaluation":"not_measured"}),
        )
    }
}

/// An asset backup must not restore old client authority. No change to the live store.
pub(super) fn strip_backup_connections(path: &std::path::Path, key: &VaultKey) -> Result<()> {
    let mut c = Connection::open(path)?;
    if !enabled(&c)? {
        return Ok(());
    }
    c.execute_batch("PRAGMA secure_delete=ON")?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = {
        let mut q = tx.prepare("SELECT connection_id FROM intake_connection_tokens")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in ids {
        let mut r: ConnectionRecord = load(&tx, key, "connection", &id)?;
        r.policy.revoked = true;
        update(&tx, key, "connection", &id, &r)?;
    }
    tx.execute("DELETE FROM intake_connection_tokens", [])?;
    tx.execute("DELETE FROM intake_connection_calls", [])?;
    tx.execute("DELETE FROM intake_dependencies WHERE record_id IN (SELECT id FROM intake_objects WHERE kind='connection_call')",[])?;
    tx.execute(
        "DELETE FROM intake_objects WHERE kind='connection_call'",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

impl IntakeStore {
    /// Bounded Owner keyword search of structured content. Raw source text is not searched.
    pub fn search_owner_units(
        &self,
        scope: &AssetScope,
        query: &str,
        before: Option<i64>,
    ) -> Result<Vec<(i64, CompiledUnit)>> {
        self.check(scope)?;
        if query.trim().is_empty() || query.len() > 1000 {
            return Err(invalid("owner_search_bounds"));
        }
        let ids = candidate_ids(&self.connection, None)?;
        if ids.len() > MAX_MEMBERS {
            return Err(invalid("owner_search_scope_limit"));
        }
        let query = query.to_lowercase();
        let mut rows = vec![];
        for id in ids {
            let seq: i64 = self.connection.query_row(
                "SELECT rowid FROM intake_objects WHERE id=?1",
                [physical_id("unit_head", &id)],
                |r| r.get(0),
            )?;
            if seq >= before.unwrap_or(i64::MAX) {
                continue;
            }
            let u = read_unit(&self.connection, &self.key, scope, &id)?;
            if serde_json::to_string(&u.unit.content)?
                .to_lowercase()
                .contains(&query)
            {
                rows.push((seq, u));
            }
        }
        rows.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        rows.truncate(50);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> (tempfile::TempDir, IntakeStore, AssetScope) {
        let dir = tempfile::tempdir().unwrap();
        let scope = AssetScope {
            owner_id: "owner:synthetic".into(),
            space: "test".into(),
        };
        let mut s = IntakeStore::create(
            &dir.path().join("n3.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        s.enable_connections(&scope).unwrap();
        (dir, s, scope)
    }
    fn issue(s: &mut IntakeStore, scope: &AssetScope) -> IssuedConnection {
        s.create_connection(
            scope,
            ConnectionRequest {
                request_id: "synthetic".into(),
                label: "test".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap()
    }
    #[test]
    fn expiry_and_total_budget_rechecked_inside_the_transaction() {
        let (_d, mut s, scope) = store();
        let issued = issue(&mut s, &scope);
        let access = s.authenticate_connection(&scope, &issued.token).unwrap();
        let mut record: ConnectionRecord =
            load(&s.connection, &s.key, "connection", &issued.connection.id).unwrap();
        record.policy.used_calls = CALL_BUDGET - 1;
        update(
            &s.connection,
            &s.key,
            "connection",
            &record.policy.id,
            &record,
        )
        .unwrap();
        let q = |id: &str| ConnectedQuery {
            request_id: id.into(),
            query: Some("test".into()),
            memory_id: None,
            max_items: 1,
        };
        assert_eq!(
            s.query_connected(&scope, &access, q("last"))
                .unwrap()
                .receipt
                .remaining_calls,
            0
        );
        assert!(s.query_connected(&scope, &access, q("over")).is_err());
        record.policy.expires_at = "2000-01-01T00:00:00Z".into();
        update(
            &s.connection,
            &s.key,
            "connection",
            &record.policy.id,
            &record,
        )
        .unwrap();
        assert!(s.authenticate_connection(&scope, &issued.token).is_err());
        assert!(s.query_connected(&scope, &access, q("last")).is_err());
    }
    #[test]
    fn sensitive_units_never_enter_default_connection_scope_or_results() {
        let (_d, mut s, scope) = store();
        let request = IntakeRequest {
            schema_version: INTAKE_VERSION.into(),
            request_id: "sensitive-source".into(),
            source_platform: "synthetic".into(),
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content: "sensitive synthetic".into(),
            occurred_at: None,
        };
        let receipt = s
            .receive(
                &IntakeAuthority {
                    scope: scope.clone(),
                    actor_id: "owner".into(),
                    capture_allowed: true,
                    role: SourceRole::User,
                    privacy_level: PrivacyLevel::Sensitive,
                },
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        let unit:MemoryUnit=serde_json::from_value(json!({"header":{"object":{"object_id":"unit_sensitive","revision":1},"scope":scope,"recorded_at":now().unwrap(),"privacy_level":"sensitive","state":"active"},"origin":"extracted","content":{"subject":"user","predicate":"test:sensitive","value":"sensitive synthetic","conditions":{},"polarity":"positive","modality":"asserted"},"occurred_at":null,"valid_time":{"kind":"unknown"},"evidence":[{"source":receipt.source,"start_char":0,"end_char":19,"quote":"sensitive synthetic"}],"derived_from":[],"modules":[],"extensions":{}})).unwrap();
        insert(
            &s.connection,
            &s.key,
            "unit_revision",
            &revision_id(&unit.header.object),
            &unit,
        )
        .unwrap();
        insert(
            &s.connection,
            &s.key,
            "unit_head",
            "unit_sensitive",
            &json!({"revision":1,"protected":false,"change_id":"synthetic"}),
        )
        .unwrap();
        consolidation::register(&s.connection, &s.key, &unit).unwrap();
        let c = issue(&mut s, &scope);
        assert!(c.connection.member_snapshot.is_empty());
        let a = s.authenticate_connection(&scope, &c.token).unwrap();
        let out = s
            .query_connected(
                &scope,
                &a,
                ConnectedQuery {
                    request_id: "read".into(),
                    query: Some("sensitive".into()),
                    memory_id: None,
                    max_items: 8,
                },
            )
            .unwrap();
        assert!(out.items.is_empty());
    }
}
