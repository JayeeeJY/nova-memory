//! Opt-in capture-only grants. Host event bodies cannot choose authority or impersonate a save.
use super::{storage::*, *};
use crate::{CaptureFidelity, VaultKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::{Duration, OffsetDateTime};

pub const CAPTURE_VERSION: &str = "nova.capture.v0.1";
const MAX_EVENTS: u64 = 10_000;
const MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEventKind {
    UserPrompt,
    AssistantResponse,
}
impl CaptureEventKind {
    fn role(self) -> SourceRole {
        match self {
            Self::UserPrompt => SourceRole::User,
            Self::AssistantResponse => SourceRole::Assistant,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureGrantRequest {
    pub request_id: String,
    pub source_namespace: String,
    pub host: String,
    pub surface: String,
    pub host_version: String,
    pub events: Vec<CaptureEventKind>,
    pub lifetime_seconds: u32,
    pub event_budget: u64,
    pub byte_budget: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureGrant {
    pub grant_id: String,
    pub instance_id: String,
    pub scope: AssetScope,
    pub source_namespace: String,
    pub host: String,
    pub surface: String,
    pub host_version: String,
    pub events: Vec<CaptureEventKind>,
    pub expires_at: String,
    pub revoked: bool,
    pub event_budget: u64,
    pub byte_budget: u64,
    pub received_events: u64,
    pub received_bytes: u64,
    pub window_started_at: String,
    pub window_events: u32,
}
#[derive(Serialize, Deserialize)]
struct GrantRecord {
    grant: CaptureGrant,
    token_digest: String,
}
pub struct IssuedCaptureGrant {
    pub grant: CaptureGrant,
    pub token: String,
}
#[derive(Clone)]
pub struct CaptureAccess {
    scope: AssetScope,
    grant_id: String,
    digest: String,
}
impl std::fmt::Debug for CaptureAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptureAccess([REDACTED])")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSession {
    pub session_id: String,
    pub source_namespace: String,
    pub origin_session_id: String,
    pub expires_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureLoss {
    pub truncated: bool,
    pub redacted: bool,
    /// True means this event omits other turns/messages, not a complete conversation export.
    pub partial_history: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEnvelope {
    pub schema_version: String,
    pub session_id: String,
    pub event_id: String,
    pub sequence: u64,
    pub kind: CaptureEventKind,
    pub content: String,
    pub occurred_at: Option<String>,
    pub loss: CaptureLoss,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAck {
    pub schema_version: String,
    pub instance_id: String,
    pub session_id: String,
    pub event_id: String,
    pub event_key: String,
    pub sequence: u64,
    pub kind: CaptureEventKind,
    pub loss: CaptureLoss,
    pub receipt: IntakeReceipt,
    pub interpretation: String,
}
#[derive(Serialize, Deserialize)]
struct EventRecord {
    fingerprint: String,
    ack: CaptureAck,
}

fn tag(key: &VaultKey, domain: &[u8], bytes: &[u8]) -> String {
    key.authentication_tag(&[b"capture:v1", domain, bytes])
}
fn require(c: &Connection) -> Result<()> {
    if c.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='intake_capture_tokens'",
        [],
        |_| Ok(()),
    )
    .optional()?
    .is_none()
    {
        return Err(invalid("capture_not_enabled"));
    }
    Ok(())
}
fn live(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    access: &CaptureAccess,
) -> Result<GrantRecord> {
    super::recovery::require_automation(c, key)?;
    require(c)?;
    if access.scope != *scope {
        return Err(denied());
    }
    let r: GrantRecord = load(c, key, "capture_grant", &access.grant_id)?;
    let instance: String = load(c, key, "capture_instance", "current")?;
    if r.grant.instance_id != instance
        || r.grant.scope != *scope
        || r.token_digest != access.digest
        || r.grant.revoked
        || parse_time(&r.grant.expires_at)? <= OffsetDateTime::now_utc()
    {
        return Err(denied());
    }
    Ok(r)
}
fn read_session(
    c: &Connection,
    key: &VaultKey,
    g: &CaptureGrant,
    id: &str,
) -> Result<CaptureSession> {
    let s: CaptureSession = load(c, key, "capture_session", id)?;
    if s.source_namespace != g.source_namespace
        || s.session_id != id
        || parse_time(&s.expires_at)? <= OffsetDateTime::now_utc()
    {
        return Err(denied());
    }
    Ok(s)
}

/// Scope/token/session verification for metadata-only provenance. Does not change canonical bytes.
pub(super) fn received_for_provenance(c:&Connection,key:&VaultKey,scope:&AssetScope,access:&CaptureAccess,session_id:&str,event_id:&str,kind:CaptureEventKind)->Result<(CaptureGrant,CaptureSession,CaptureAck)> {
    if !identifier(session_id)||!identifier(event_id){return Err(denied());}
    let grant=live(c,key,scope,access)?.grant;
    if !grant.events.contains(&kind){return Err(denied());}
    let session=read_session(c,key,&grant,session_id)?;
    let event_key=tag(key,b"event",&serde_json::to_vec(&(scope,&session.source_namespace,&session.origin_session_id,event_id,kind))?);
    let record:EventRecord=load(c,key,"capture_event",&event_key)?;
    if record.ack.instance_id!=grant.instance_id||record.ack.session_id!=session_id||record.ack.event_id!=event_id||record.ack.kind!=kind{return Err(denied());}
    compiler::load_source(c,key,scope,&record.ack.receipt.source)?;
    Ok((grant,session,record.ack))
}
impl IntakeStore {
    /// Owner operation; never implicitly enabled by a client or a restored backup.
    pub fn enable_capture(&mut self, scope: &AssetScope) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS intake_capture_tokens(digest TEXT PRIMARY KEY, grant_id TEXT NOT NULL UNIQUE); CREATE TABLE IF NOT EXISTS intake_capture_owners(namespace TEXT PRIMARY KEY, grant_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS intake_capture_slots(slot TEXT PRIMARY KEY, event_key TEXT NOT NULL);")?;
        if tx
            .query_row(
                "SELECT 1 FROM intake_objects WHERE id=?1",
                [physical_id("capture_instance", "current")],
                |_| Ok(()),
            )
            .optional()?
            .is_none()
        {
            insert(
                &tx,
                &self.key,
                "capture_instance",
                "current",
                &id("capture_instance"),
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Selects the canonical capture owner per origin namespace. Rotation requires revocation.
    /// The one-time token is returned once and is never persisted in plaintext.
    pub fn create_capture_grant(
        &mut self,
        scope: &AssetScope,
        i: CaptureGrantRequest,
    ) -> Result<IssuedCaptureGrant> {
        self.check(scope)?;
        super::recovery::require_automation(&self.connection, &self.key)?;
        if !identifier(&i.request_id)
            || !identifier(&i.source_namespace)
            || !identifier(&i.host)
            || !identifier(&i.surface)
            || !identifier(&i.host_version)
            || i.host.len() + i.surface.len() + 1 > 128
            || i.events.is_empty()
            || i.events.len() > 2
            || (i.events.len() == 2 && i.events[0] == i.events[1])
            || !(1..=86400).contains(&i.lifetime_seconds)
            || !(1..=MAX_EVENTS).contains(&i.event_budget)
            || !(1..=MAX_BYTES).contains(&i.byte_budget)
        {
            return Err(invalid("capture_grant_bounds"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let namespace = tag(
            &self.key,
            b"owner",
            &serde_json::to_vec(&(scope, &i.source_namespace))?,
        );
        if let Some(old) = tx
            .query_row(
                "SELECT grant_id FROM intake_capture_owners WHERE namespace=?1",
                [&namespace],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let r: GrantRecord = load(&tx, &self.key, "capture_grant", &old)?;
            if !r.grant.revoked && parse_time(&r.grant.expires_at)? > OffsetDateTime::now_utc() {
                return Err(conflict());
            }
            // An origin namespace cannot silently change platform identity during rotation.
            if r.grant.host != i.host || r.grant.surface != i.surface {
                return Err(denied());
            }
        }
        let grant_id = tag(
            &self.key,
            b"grant",
            &serde_json::to_vec(&(scope, &i.request_id))?,
        );
        let at = now()?;
        let grant = CaptureGrant {
            grant_id: grant_id.clone(),
            instance_id: load(&tx, &self.key, "capture_instance", "current")?,
            scope: scope.clone(),
            source_namespace: i.source_namespace,
            host: i.host,
            surface: i.surface,
            host_version: i.host_version,
            events: i.events,
            expires_at: format_time(
                OffsetDateTime::now_utc() + Duration::seconds(i64::from(i.lifetime_seconds)),
            )?,
            revoked: false,
            event_budget: i.event_budget,
            byte_budget: i.byte_budget,
            received_events: 0,
            received_bytes: 0,
            window_started_at: at,
            window_events: 0,
        };
        let token = VaultKey::generate()?.to_hex();
        let digest = tag(&self.key, b"token", token.as_bytes());
        insert(
            &tx,
            &self.key,
            "capture_grant",
            &grant_id,
            &GrantRecord {
                grant: grant.clone(),
                token_digest: digest.clone(),
            },
        )?;
        tx.execute(
            "INSERT INTO intake_capture_tokens VALUES (?1,?2)",
            params![digest, grant_id],
        )?;
        tx.execute("INSERT INTO intake_capture_owners VALUES (?1,?2) ON CONFLICT(namespace) DO UPDATE SET grant_id=excluded.grant_id",params![namespace,grant_id])?;
        tx.commit()?;
        Ok(IssuedCaptureGrant { grant, token })
    }
    pub fn authenticate_capture(&self, scope: &AssetScope, token: &str) -> Result<CaptureAccess> {
        self.check(scope)?;
        require(&self.connection)?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(denied());
        }
        let digest = tag(&self.key, b"token", token.as_bytes());
        let grant_id = self
            .connection
            .query_row(
                "SELECT grant_id FROM intake_capture_tokens WHERE digest=?1",
                [&digest],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(denied)?;
        let a = CaptureAccess {
            scope: scope.clone(),
            grant_id,
            digest,
        };
        live(&self.connection, &self.key, scope, &a)?;
        Ok(a)
    }
    pub fn capture_grant(&self, scope: &AssetScope, id: &str) -> Result<CaptureGrant> {
        self.check(scope)?;
        let r: GrantRecord = load(&self.connection, &self.key, "capture_grant", id)?;
        if r.grant.scope != *scope {
            return Err(denied());
        }
        Ok(r.grant)
    }
    /// Owner-only bounded inventory; no bearer material is returned.
    pub fn capture_grants(&self, scope: &AssetScope) -> Result<Vec<CaptureGrant>> {
        self.check(scope)?;
        require(&self.connection)?;
        let mut q=self.connection.prepare("SELECT id FROM intake_objects WHERE kind='capture_grant' ORDER BY rowid DESC LIMIT 128")?;
        let ids = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                self.capture_grant(
                    scope,
                    id.strip_prefix("n2a:capture_grant:")
                        .ok_or_else(|| invalid("capture_grant_index"))?,
                )
            })
            .collect()
    }
    pub fn revoke_capture(&mut self, scope: &AssetScope, id: &str) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut r: GrantRecord = load(&tx, &self.key, "capture_grant", id)?;
        if r.grant.scope != *scope {
            return Err(denied());
        }
        r.grant.revoked = true;
        update(&tx, &self.key, "capture_grant", id, &r)?;
        tx.execute("DELETE FROM intake_capture_tokens WHERE grant_id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }
    /// A client can register its own origin session only within its server-fixed namespace.
    pub fn register_capture_session(
        &mut self,
        scope: &AssetScope,
        access: &CaptureAccess,
        origin: &str,
    ) -> Result<CaptureSession> {
        self.check(scope)?;
        if !identifier(origin) {
            return Err(invalid("capture_origin_session"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let r = live(&tx, &self.key, scope, access)?;
        let id = tag(
            &self.key,
            b"session",
            &serde_json::to_vec(&(scope, &r.grant.source_namespace, origin))?,
        );
        if tx
            .query_row(
                "SELECT 1 FROM intake_objects WHERE id=?1",
                [physical_id("capture_session", &id)],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return read_session(&tx, &self.key, &r.grant, &id);
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM intake_objects WHERE kind='capture_session'",
            [],
            |r| r.get(0),
        )?;
        if count >= 1024 {
            return Err(invalid("capture_session_capacity"));
        }
        let s = CaptureSession {
            session_id: id.clone(),
            source_namespace: r.grant.source_namespace,
            origin_session_id: origin.into(),
            expires_at: r.grant.expires_at,
        };
        insert(&tx, &self.key, "capture_session", &id, &s)?;
        tx.commit()?;
        Ok(s)
    }
    /// Strict envelope -> atomic original Source/job/ACK, never a compiled fact or action.
    pub fn capture_event(
        &mut self,
        scope: &AssetScope,
        access: &CaptureAccess,
        bytes: &[u8],
    ) -> Result<CaptureAck> {
        self.check(scope)?;
        if bytes.len() > 96 * 1024 {
            return Err(invalid("capture_document_limit"));
        }
        let i: CaptureEnvelope = crate::compilation::decode_untrusted_json(bytes)?;
        if i.schema_version != CAPTURE_VERSION
            || !identifier(&i.session_id)
            || !identifier(&i.event_id)
            || !(1..=1_000_000).contains(&i.sequence)
            || i.content.trim().is_empty()
            || i.content.len() > 32 * 1024
        {
            return Err(invalid("capture_envelope_bounds"));
        }
        if let Some(at) = &i.occurred_at {
            parse_time(at)?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut r = live(&tx, &self.key, scope, access)?;
        if !r.grant.events.contains(&i.kind) {
            return Err(denied());
        }
        let s = read_session(&tx, &self.key, &r.grant, &i.session_id)?;
        // Namespace/session/event identity is independent of delivery channel or rotated token.
        let event_key = tag(
            &self.key,
            b"event",
            &serde_json::to_vec(&(
                scope,
                &s.source_namespace,
                &s.origin_session_id,
                &i.event_id,
                i.kind,
            ))?,
        );
        let fingerprint = tag(&self.key, b"payload", &serde_json::to_vec(&i)?);
        if tx
            .query_row(
                "SELECT 1 FROM intake_objects WHERE id=?1",
                [physical_id("capture_event", &event_key)],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            let old: EventRecord = load(&tx, &self.key, "capture_event", &event_key)?;
            if old.fingerprint != fingerprint {
                return Err(conflict());
            }
            // Never recreate a withdrawn/deleted original, even when delivery is retried.
            let _: SourceSnapshot =
                load(&tx, &self.key, "source", &old.ack.receipt.source.object_id)?;
            return Ok(old.ack);
        }
        let slot = tag(
            &self.key,
            b"sequence",
            &serde_json::to_vec(&(scope, &s.source_namespace, &s.origin_session_id, i.sequence))?,
        );
        if tx
            .query_row(
                "SELECT 1 FROM intake_capture_slots WHERE slot=?1",
                [&slot],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(conflict());
        }
        if r.grant.received_events >= r.grant.event_budget
            || r.grant
                .received_bytes
                .checked_add(i.content.len() as u64)
                .is_none_or(|n| n > r.grant.byte_budget)
        {
            return Err(invalid("capture_budget_exhausted"));
        }
        let at = OffsetDateTime::now_utc();
        if at - parse_time(&r.grant.window_started_at)? >= Duration::minutes(1) {
            r.grant.window_started_at = format_time(at)?;
            r.grant.window_events = 0;
        }
        if r.grant.window_events >= 30 {
            return Err(invalid("capture_rate_limited"));
        }
        let request = IntakeRequest {
            schema_version: INTAKE_VERSION.into(),
            request_id: event_key.clone(),
            source_platform: format!("{}:{}", r.grant.host, r.grant.surface),
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content: i.content.clone(),
            occurred_at: i.occurred_at,
        };
        request.validate()?;
        let authority = IntakeAuthority {
            scope: scope.clone(),
            actor_id: tag(&self.key, b"actor", s.source_namespace.as_bytes()),
            capture_allowed: true,
            role: i.kind.role(),
            privacy_level: PrivacyLevel::Private,
        };
        let receipt = receive_tx(&tx, &self.key, scope, &authority, &request, None)?;
        let ack=CaptureAck{schema_version:CAPTURE_VERSION.into(),instance_id:r.grant.instance_id.clone(),session_id:i.session_id,event_id:i.event_id,event_key:event_key.clone(),sequence:i.sequence,kind:i.kind,loss:i.loss,receipt,interpretation:"captured_visible_event_unverified; role_from_configured_adapter; not_full_history; no_execution_authority".into()};
        insert(
            &tx,
            &self.key,
            "capture_event",
            &event_key,
            &EventRecord {
                fingerprint,
                ack: ack.clone(),
            },
        )?;
        tx.execute(
            "INSERT INTO intake_capture_slots VALUES (?1,?2)",
            params![slot, event_key],
        )?;
        r.grant.received_events += 1;
        r.grant.received_bytes += i.content.len() as u64;
        r.grant.window_events += 1;
        update(&tx, &self.key, "capture_grant", &r.grant.grant_id, &r)?;
        tx.commit()?;
        Ok(ack)
    }
}

/// A backup carries assets, never a live capture credential. Called only on the new copy.
pub(super) fn strip_backup_capture(path: &std::path::Path, key: &VaultKey) -> Result<()> {
    let mut c = Connection::open(path)?;
    if c.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='intake_capture_tokens'",
        [],
        |_| Ok(()),
    )
    .optional()?
    .is_none()
    {
        return Ok(());
    }
    c.execute_batch("PRAGMA secure_delete=ON")?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = {
        let mut q = tx.prepare("SELECT grant_id FROM intake_capture_tokens")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in ids {
        let mut r: GrantRecord = load(&tx, key, "capture_grant", &id)?;
        r.grant.revoked = true;
        update(&tx, key, "capture_grant", &id, &r)?;
    }
    tx.execute("DELETE FROM intake_capture_tokens", [])?;
    tx.commit()?;
    Ok(())
}
