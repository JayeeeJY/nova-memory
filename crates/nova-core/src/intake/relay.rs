//! Opt-in, transport-independent encrypted outbox for visible capture events.
//! The trusted adapter selects the fixed destination and supplies a capture-only token per send.
use super::{
    storage::{conflict, denied, parse_time},
    *,
};
use crate::{VaultKey, crypto::EncryptedBlob};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::path::Path;
use time::OffsetDateTime;
#[path = "capture_bootstrap.rs"]
mod capture_bootstrap;
pub use capture_bootstrap::*;
#[path = "capture_history.rs"]
mod capture_history;
pub use capture_history::*;

pub const CAPTURE_RELAY_VERSION: &str = "nova.capture-relay.v0.1";
const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_ACK_BYTES: usize = 8 * 1024;
const MAX_ENVELOPE_BYTES: usize = 96 * 1024;

/// All identifiers are installed by the owner/adapter, never taken from an event body.
/// `target_id` is an adapter-owned transport binding, not a caller-provided URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRelayTarget {
    pub instance_id: String,
    pub target_id: String,
    pub grant_id: String,
    pub source_namespace: String,
    pub session_id: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRelayPolicy {
    pub version: String,
    pub allowed_events: Vec<CaptureEventKind>,
    /// Exact, case-sensitive literal matches; no regex, inference or external processing.
    pub redact_literals: Vec<String>,
    pub max_content_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRelayConfiguration {
    pub target: CaptureRelayTarget,
    pub policy: CaptureRelayPolicy,
    pub max_pending_events: usize,
    pub max_pending_bytes: usize,
    /// Includes ACK/expired/rejected tombstones. Never silently drops idempotency history.
    pub max_total_events: usize,
    pub ttl_seconds: u32,
    pub lease_seconds: u32,
    pub max_attempts: u32,
    pub retry_base_seconds: u32,
    pub retry_max_seconds: u32,
}
impl CaptureRelayConfiguration {
    fn validate(&self) -> Result<()> {
        let t = &self.target;
        let p = &self.policy;
        if [
            &t.instance_id,
            &t.target_id,
            &t.grant_id,
            &t.source_namespace,
            &t.session_id,
            &p.version,
        ]
        .iter()
        .any(|v| !identifier(v))
            || p.allowed_events.is_empty()
            || p.allowed_events.len() > 2
            || (p.allowed_events.len() == 2 && p.allowed_events[0] == p.allowed_events[1])
            || !(1..=32 * 1024).contains(&p.max_content_bytes)
            || p.redact_literals.len() > 16
            || p.redact_literals
                .iter()
                .any(|v| v.is_empty() || v.len() > 256)
            || !(1..=10_000).contains(&self.max_total_events)
            || !(1..=self.max_total_events).contains(&self.max_pending_events)
            || !(1..=32 * 1024 * 1024).contains(&self.max_pending_bytes)
            || !(1..=86400).contains(&self.ttl_seconds)
            || !(1..=300).contains(&self.lease_seconds)
            || !(1..=10).contains(&self.max_attempts)
            || !(1..=60).contains(&self.retry_base_seconds)
            || !(self.retry_base_seconds..=3600).contains(&self.retry_max_seconds)
        {
            return Err(invalid("relay_configuration_bounds"));
        }
        parse_time(&t.expires_at)?;
        Ok(())
    }
}

/// Typed visible-only ingress. Unknown fields, hidden/thinking/system/tool event kinds are rejected.
/// A trusted host adapter must actually select the user-visible content; this is not a DLP oracle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRelayEvent {
    pub event_id: String,
    /// The adapter's stable sequence. Retries must reuse it; the relay never renumbers.
    pub sequence: u64,
    pub kind: CaptureEventKind,
    pub content: String,
    pub occurred_at: Option<String>,
    pub loss: CaptureLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRelayState {
    Queued,
    InFlight,
    RetryWait,
    Acknowledged,
    Rejected,
    Exhausted,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRelayStop {
    Revoked,
    Unauthorized,
    InvalidEvent,
    Conflict,
    TargetMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRelayStatus {
    pub event_key: String,
    pub sequence: u64,
    pub state: CaptureRelayState,
    pub attempts: u32,
    pub next_attempt_at: i64,
    pub expires_at: i64,
    pub stop_reason: Option<CaptureRelayStop>,
    pub receipt: Option<IntakeReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRelaySnapshot {
    pub version: String,
    pub stopped: Option<CaptureRelayStop>,
    pub pending_events: usize,
    pub pending_bytes: usize,
    pub events: Vec<CaptureRelayStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRelayEnqueued {
    pub event_key: String,
    pub sequence: u64,
    pub state: CaptureRelayState,
    /// A durable local outbox entry is not a durable Source ACK.
    pub source_acknowledged: bool,
    pub duplicate: bool,
    pub loss: CaptureLoss,
}

/// Opaque lease; its destination and envelope cannot be replaced through the public API.
#[derive(Clone)]
pub struct CaptureRelayClaim {
    binding: String,
    event_key: String,
    lease_id: String,
    target: CaptureRelayTarget,
    envelope: CaptureEnvelope,
}
impl std::fmt::Debug for CaptureRelayClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureRelayClaim")
            .field("event_key", &self.event_key)
            .field("sequence", &self.envelope.sequence)
            .finish_non_exhaustive()
    }
}
impl CaptureRelayClaim {
    pub fn target(&self) -> &CaptureRelayTarget {
        &self.target
    }
    pub fn envelope(&self) -> &CaptureEnvelope {
        &self.envelope
    }
    pub fn event_key(&self) -> &str {
        &self.event_key
    }
}

#[derive(Debug)]
pub enum CaptureRelayDelivery {
    SourceAck(Box<CaptureAck>),
    /// For timeout/transport failure/429/5xx. No remote error body or bearer is persisted.
    Retry {
        retry_after_seconds: Option<u32>,
    },
    Reject(CaptureRelayStop),
}

#[derive(Serialize, Deserialize)]
struct RelayIdentity {
    version: String,
    configuration: CaptureRelayConfiguration,
    binding: String,
}
#[derive(Serialize, Deserialize)]
struct RelayControl {
    clock: i64,
    stopped: Option<CaptureRelayStop>,
}
#[derive(Serialize, Deserialize)]
struct RelayLease {
    id: String,
    until: i64,
}
#[derive(Serialize, Deserialize)]
struct RelayRecord {
    event_key: String,
    fingerprint: String,
    sequence: u64,
    kind: CaptureEventKind,
    loss: CaptureLoss,
    content_sha256: String,
    envelope: Option<CaptureEnvelope>,
    state: CaptureRelayState,
    attempts: u32,
    next_attempt_at: i64,
    expires_at: i64,
    lease: Option<RelayLease>,
    stop_reason: Option<CaptureRelayStop>,
    receipt: Option<IntakeReceipt>,
}
impl RelayRecord {
    fn status(&self) -> CaptureRelayStatus {
        CaptureRelayStatus {
            event_key: self.event_key.clone(),
            sequence: self.sequence,
            state: self.state,
            attempts: self.attempts,
            next_attempt_at: self.next_attempt_at,
            expires_at: self.expires_at,
            stop_reason: self.stop_reason,
            receipt: self.receipt.clone(),
        }
    }
    fn enqueued(&self, duplicate: bool) -> CaptureRelayEnqueued {
        CaptureRelayEnqueued {
            event_key: self.event_key.clone(),
            sequence: self.sequence,
            state: self.state,
            source_acknowledged: self.state == CaptureRelayState::Acknowledged,
            duplicate,
            loss: self.loss.clone(),
        }
    }
}

/// A separate encrypted SQLite file; opening a legacy Vault or changing any binding is refused.
/// Timestamps are supplied by trusted runtime code for deterministic state-machine testing.
/// Remote event bodies never select time. Clock rollback fails closed.
pub struct CaptureRelay {
    connection: Connection,
    key: VaultKey,
    configuration: CaptureRelayConfiguration,
    binding: String,
}

fn seal<T: Serialize>(
    c: &Connection,
    key: &VaultKey,
    binding: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    let aad = serde_json::to_vec(&(CAPTURE_RELAY_VERSION, binding, id))?;
    let b = key.encrypt(&serde_json::to_vec(value)?, &aad)?;
    c.execute("INSERT INTO relay_objects(id,nonce,ciphertext) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET nonce=excluded.nonce,ciphertext=excluded.ciphertext", params![id,b.nonce,b.ciphertext])?;
    Ok(())
}
fn unseal<T: DeserializeOwned>(
    c: &Connection,
    key: &VaultKey,
    binding: &str,
    id: &str,
) -> Result<T> {
    let b = c.query_row("SELECT nonce,ciphertext FROM relay_objects WHERE id=?1 AND length(nonce)=24 AND length(ciphertext)<=262144", [id], |r| Ok(EncryptedBlob {nonce:r.get(0)?, ciphertext:r.get(1)?}))?;
    let aad = serde_json::to_vec(&(CAPTURE_RELAY_VERSION, binding, id))?;
    Ok(serde_json::from_slice(&key.decrypt(&b, &aad)?)?)
}
fn records(c: &Connection, key: &VaultKey, binding: &str, max: usize) -> Result<Vec<RelayRecord>> {
    let ids = c.prepare("SELECT id FROM relay_objects WHERE id NOT IN ('identity','control') ORDER BY id LIMIT ?1")?
        .query_map([max as i64 + 1], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if ids.len() > max {
        return Err(invalid("relay_record_limit"));
    }
    let mut result = Vec::with_capacity(ids.len());
    for id in ids {
        let r: RelayRecord = unseal(c, key, binding, &id)?;
        if r.event_key != id {
            return Err(denied());
        }
        result.push(r);
    }
    result.sort_by_key(|r| r.sequence);
    Ok(result)
}
fn configure_relay(c: &Connection, max_pages: usize) -> Result<()> {
    let page_size: usize = c.pragma_query_value(None, "page_size", |r| r.get(0))?;
    if page_size != 4096 {
        return Err(invalid("relay_page_size"));
    }
    c.execute_batch("PRAGMA secure_delete=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;")?;
    c.pragma_update(None, "max_page_count", max_pages)?;
    Ok(())
}
fn pages(config: &CaptureRelayConfiguration) -> usize {
    // Bounded ciphertext payload + per-record receipt/index/AEAD overhead; DELETE journal is bounded too.
    (config.max_pending_bytes * 6 + config.max_total_events * 16 * 1024 + 1024 * 1024)
        .div_ceil(4096)
}
fn advance(c: &Connection, key: &VaultKey, binding: &str, at: i64) -> Result<RelayControl> {
    let mut control: RelayControl = unseal(c, key, binding, "control")?;
    if at < control.clock || OffsetDateTime::from_unix_timestamp(at).is_err() {
        return Err(invalid("relay_clock_rollback"));
    }
    control.clock = at;
    Ok(control)
}
fn delay(config: &CaptureRelayConfiguration, attempts: u32) -> i64 {
    i64::from(
        config
            .retry_base_seconds
            .saturating_mul(1u32 << attempts.saturating_sub(1).min(10))
            .min(config.retry_max_seconds),
    )
}
fn redact(content: &str, literals: &[String]) -> (String, bool) {
    // One pass over original bytes: replacement markers are never recursively re-redacted.
    let mut result = String::new();
    let mut offset = 0;
    let mut redacted = false;
    while offset < content.len() {
        let remaining = &content[offset..];
        if let Some(needle) = literals
            .iter()
            .filter(|n| remaining.starts_with(n.as_str()))
            .max_by_key(|n| n.len())
        {
            result.push_str("[REDACTED]");
            offset += needle.len();
            redacted = true;
        } else {
            let c = remaining.chars().next().expect("nonempty suffix");
            result.push(c);
            offset += c.len_utf8();
        }
    }
    (result, redacted)
}
fn fit_envelope(envelope: &mut CaptureEnvelope) -> Result<()> {
    if serde_json::to_vec(envelope)?.len() <= MAX_ENVELOPE_BYTES {
        return Ok(());
    }
    envelope.loss.truncated = true;
    let content = std::mem::take(&mut envelope.content);
    let overhead = serde_json::to_vec(envelope)?.len();
    let budget = MAX_ENVELOPE_BYTES.saturating_sub(overhead);
    let mut used = 0;
    for c in content.chars() {
        let escaped = match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => c.len_utf8(),
        };
        if used + escaped > budget {
            break;
        }
        used += escaped;
        envelope.content.push(c);
    }
    if envelope.content.trim().is_empty()
        || serde_json::to_vec(envelope)?.len() > MAX_ENVELOPE_BYTES
    {
        return Err(invalid("relay_envelope_bounds"));
    }
    Ok(())
}
fn maintain(
    c: &Connection,
    key: &VaultKey,
    binding: &str,
    config: &CaptureRelayConfiguration,
    at: i64,
    rs: &mut [RelayRecord],
) -> Result<()> {
    for r in rs {
        let mut changed = false;
        if r.envelope.is_some() && r.expires_at <= at {
            r.envelope = None;
            r.lease = None;
            r.state = CaptureRelayState::Expired;
            changed = true;
        } else if r.state == CaptureRelayState::InFlight
            && r.lease.as_ref().is_some_and(|l| l.until <= at)
        {
            r.lease = None;
            r.state = if r.attempts >= config.max_attempts {
                CaptureRelayState::Exhausted
            } else {
                CaptureRelayState::RetryWait
            };
            r.next_attempt_at = at + delay(config, r.attempts);
            changed = true;
        }
        if changed {
            seal(c, key, binding, &r.event_key, r)?;
        }
    }
    Ok(())
}

impl CaptureRelay {
    pub fn create(
        path: &Path,
        key: VaultKey,
        configuration: CaptureRelayConfiguration,
        at: i64,
    ) -> Result<Self> {
        configuration.validate()?;
        if at < 0 || parse_time(&configuration.target.expires_at)?.unix_timestamp() <= at {
            return Err(invalid("relay_target_expired"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?.sync_all()?;
        let connection = Connection::open(path)?;
        configure_relay(&connection, pages(&configuration))?;
        connection.execute_batch("CREATE TABLE relay_objects(id TEXT PRIMARY KEY,nonce BLOB NOT NULL,ciphertext BLOB NOT NULL);")?;
        let binding =
            key.authentication_tag(&[b"relay-binding:v1", &serde_json::to_vec(&configuration)?]);
        let mut relay = Self {
            connection,
            key,
            configuration,
            binding,
        };
        let tx = relay
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        seal(
            &tx,
            &relay.key,
            "",
            "identity",
            &RelayIdentity {
                version: CAPTURE_RELAY_VERSION.into(),
                configuration: relay.configuration.clone(),
                binding: relay.binding.clone(),
            },
        )?;
        seal(
            &tx,
            &relay.key,
            &relay.binding,
            "control",
            &RelayControl {
                clock: at,
                stopped: None,
            },
        )?;
        tx.commit()?;
        Ok(relay)
    }
    pub fn open(path: &Path, key: VaultKey, expected: CaptureRelayConfiguration) -> Result<Self> {
        expected.validate()?;
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let i: RelayIdentity = unseal(&connection, &key, "", "identity")?;
        let binding =
            key.authentication_tag(&[b"relay-binding:v1", &serde_json::to_vec(&expected)?]);
        if i.version != CAPTURE_RELAY_VERSION || i.configuration != expected || i.binding != binding
        {
            return Err(denied());
        }
        let _: RelayControl = unseal(&connection, &key, &binding, "control")?;
        configure_relay(&connection, pages(&expected))?;
        Ok(Self {
            connection,
            key,
            configuration: expected,
            binding,
        })
    }
    pub fn enqueue_json(&mut self, bytes: &[u8], at: i64) -> Result<CaptureRelayEnqueued> {
        if bytes.len() > MAX_INPUT_BYTES * 6 + 4096 {
            return Err(invalid("relay_input_bounds"));
        }
        let event = crate::compilation::decode_untrusted_json(bytes)?;
        self.enqueue(event, at)
    }
    pub fn enqueue(&mut self, event: CaptureRelayEvent, at: i64) -> Result<CaptureRelayEnqueued> {
        self.enqueue_inner(event, at, false, None)
    }
    /// Atomically allocates a sequence for a stable adapter event ID on first receipt.
    /// Duplicate IDs reuse their original sequence; caller's placeholder sequence is ignored.
    pub fn enqueue_sequenced(
        &mut self,
        event: CaptureRelayEvent,
        at: i64,
    ) -> Result<CaptureRelayEnqueued> {
        self.enqueue_inner(event, at, true, None)
    }
    fn enqueue_inner(
        &mut self,
        mut event: CaptureRelayEvent,
        at: i64,
        sequence_locally: bool,
        original_expiry: Option<i64>,
    ) -> Result<CaptureRelayEnqueued> {
        if !identifier(&event.event_id)
            || (!sequence_locally && !(1..=1_000_000).contains(&event.sequence))
            || event.content.trim().is_empty()
            || event.content.len() > MAX_INPUT_BYTES
            || !self
                .configuration
                .policy
                .allowed_events
                .contains(&event.kind)
        {
            return Err(invalid("relay_visible_event_bounds"));
        }
        if let Some(t) = &event.occurred_at {
            if t.len() > 64 {
                return Err(invalid("relay_event_time_bounds"));
            }
            parse_time(t)?;
        }
        let event_key = self.key.authentication_tag(&[
            b"relay-event:v1",
            self.binding.as_bytes(),
            event.event_id.as_bytes(),
            &serde_json::to_vec(&event.kind)?,
        ]);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = records(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        maintain(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            at,
            &mut rs,
        )?;
        if sequence_locally {
            event.sequence = rs
                .iter()
                .find(|r| r.event_key == event_key)
                .map(|r| r.sequence)
                .unwrap_or_else(|| rs.iter().map(|r| r.sequence).max().unwrap_or(0) + 1);
            if event.sequence > 1_000_000 {
                return Err(invalid("relay_sequence_exhausted"));
            }
        }
        let fingerprint = self
            .key
            .authentication_tag(&[b"relay-input:v1", &serde_json::to_vec(&event)?]);
        if let Some(r) = rs.iter().find(|r| r.event_key == event_key) {
            if r.fingerprint != fingerprint {
                return Err(conflict());
            }
            let result = r.enqueued(true);
            seal(&tx, &self.key, &self.binding, "control", &control)?;
            tx.commit()?;
            return Ok(result);
        }
        if control.stopped.is_some() {
            return Err(denied());
        }
        if rs.iter().any(|r| r.sequence == event.sequence) {
            return Err(conflict());
        }
        let target_expiry = parse_time(&self.configuration.target.expires_at)?.unix_timestamp();
        if at >= target_expiry {
            return Err(invalid("relay_target_expired"));
        }
        let (mut content, redacted) =
            redact(&event.content, &self.configuration.policy.redact_literals);
        let mut loss = event.loss;
        loss.partial_history = true;
        loss.redacted |= redacted;
        let max = self.configuration.policy.max_content_bytes;
        if content.len() > max {
            let mut end = max;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
            loss.truncated = true;
        }
        if content.trim().is_empty() {
            return Err(invalid("relay_empty_after_policy"));
        }
        let mut envelope = CaptureEnvelope {
            schema_version: CAPTURE_VERSION.into(),
            session_id: self.configuration.target.session_id.clone(),
            event_id: event.event_id,
            sequence: event.sequence,
            kind: event.kind,
            content,
            occurred_at: event.occurred_at,
            loss,
        };
        fit_envelope(&mut envelope)?;
        let payloads: Vec<_> = rs.iter().filter_map(|r| r.envelope.as_ref()).collect();
        if rs.len() >= self.configuration.max_total_events
            || payloads.len() >= self.configuration.max_pending_events
            || payloads.iter().map(|e| e.content.len()).sum::<usize>() + envelope.content.len()
                > self.configuration.max_pending_bytes
        {
            return Err(invalid("relay_capacity_full_no_ack"));
        }
        let r = RelayRecord {
            event_key: event_key.clone(),
            fingerprint,
            sequence: event.sequence,
            kind: event.kind,
            content_sha256: hex::encode(Sha256::digest(envelope.content.as_bytes())),
            loss: envelope.loss.clone(),
            envelope: Some(envelope),
            state: CaptureRelayState::Queued,
            attempts: 0,
            next_attempt_at: at,
            expires_at: (at + i64::from(self.configuration.ttl_seconds))
                .min(target_expiry)
                .min(original_expiry.unwrap_or(i64::MAX)),
            lease: None,
            stop_reason: None,
            receipt: None,
        };
        let result = r.enqueued(false);
        seal(&tx, &self.key, &self.binding, &event_key, &r)?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(result)
    }
    /// At most 16 events per lease batch; unacknowledged members recover individually.
    pub fn claim(&mut self, limit: usize, at: i64) -> Result<Vec<CaptureRelayClaim>> {
        if !(1..=16).contains(&limit) {
            return Err(invalid("relay_claim_limit"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = records(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        maintain(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            at,
            &mut rs,
        )?;
        let mut result = Vec::new();
        if control.stopped.is_none() {
            for r in &mut rs {
                if result.len() == limit {
                    break;
                }
                if !matches!(
                    r.state,
                    CaptureRelayState::Queued | CaptureRelayState::RetryWait
                ) || r.next_attempt_at > at
                {
                    continue;
                }
                let envelope = r
                    .envelope
                    .clone()
                    .ok_or_else(|| invalid("relay_missing_payload"))?;
                let lease_id = uuid::Uuid::now_v7().to_string();
                r.attempts += 1;
                r.state = CaptureRelayState::InFlight;
                r.lease = Some(RelayLease {
                    id: lease_id.clone(),
                    until: (at + i64::from(self.configuration.lease_seconds)).min(r.expires_at),
                });
                result.push(CaptureRelayClaim {
                    binding: self.binding.clone(),
                    event_key: r.event_key.clone(),
                    lease_id,
                    target: self.configuration.target.clone(),
                    envelope,
                });
                seal(&tx, &self.key, &self.binding, &r.event_key, r)?;
            }
        }
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn complete(
        &mut self,
        claim: &CaptureRelayClaim,
        delivery: CaptureRelayDelivery,
        at: i64,
    ) -> Result<CaptureRelayStatus> {
        if claim.binding != self.binding || claim.target != self.configuration.target {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = advance(&tx, &self.key, &self.binding, at)?;
        let mut r: RelayRecord = unseal(&tx, &self.key, &self.binding, &claim.event_key)?;
        if r.event_key != claim.event_key
            || r.state != CaptureRelayState::InFlight
            || r.expires_at <= at
            || !r
                .lease
                .as_ref()
                .is_some_and(|l| l.id == claim.lease_id && l.until > at)
        {
            return Err(conflict());
        }
        match delivery {
            CaptureRelayDelivery::SourceAck(ack) => {
                if serde_json::to_vec(&ack)?.len() > MAX_ACK_BYTES
                    || ack.schema_version != CAPTURE_VERSION
                    || ack.instance_id != self.configuration.target.instance_id
                    || ack.session_id != claim.envelope.session_id
                    || ack.event_id != claim.envelope.event_id
                    || ack.sequence != r.sequence
                    || ack.kind != r.kind
                    || ack.loss != r.loss
                    || ack.receipt.schema_version != INTAKE_VERSION
                    || ack.receipt.status != IntakeAcceptance::Received
                    || ack.receipt.content_sha256 != r.content_sha256
                    || ack.event_key != ack.receipt.request_id
                    || !identifier(&ack.event_key)
                    || !identifier(&ack.receipt.source.object_id)
                    || ack.receipt.source.revision == 0
                    || !identifier(&ack.receipt.job_id)
                    || parse_time(&ack.receipt.saved_at).is_err()
                {
                    return Err(invalid("relay_ack_binding_mismatch"));
                }
                r.receipt = Some(ack.receipt);
                r.envelope = None;
                r.state = CaptureRelayState::Acknowledged;
            }
            CaptureRelayDelivery::Retry {
                retry_after_seconds,
            } => {
                r.state = if r.attempts >= self.configuration.max_attempts {
                    CaptureRelayState::Exhausted
                } else {
                    CaptureRelayState::RetryWait
                };
                // Never retry earlier than Retry-After. If it exceeds local bounds/TTL, stop visibly.
                let wait = delay(&self.configuration, r.attempts)
                    .max(i64::from(retry_after_seconds.unwrap_or(0)));
                r.next_attempt_at = at + wait;
                if wait > i64::from(self.configuration.retry_max_seconds)
                    || r.next_attempt_at >= r.expires_at
                {
                    r.state = CaptureRelayState::Exhausted;
                }
            }
            CaptureRelayDelivery::Reject(reason) => {
                r.state = CaptureRelayState::Rejected;
                r.stop_reason = Some(reason);
                if matches!(
                    reason,
                    CaptureRelayStop::Revoked
                        | CaptureRelayStop::Unauthorized
                        | CaptureRelayStop::TargetMismatch
                ) {
                    control.stopped = Some(reason);
                }
            }
        }
        r.lease = None;
        let result = r.status();
        seal(&tx, &self.key, &self.binding, &claim.event_key, &r)?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(result)
    }
    /// Transport seam only: one capture-only bearer borrowed for exactly one callback invocation.
    /// No HTTP client, URL redirects, token lookup, token persistence, or root-bearer fallback.
    pub fn dispatch_one<F>(&mut self, token: &str, send: F) -> Result<Option<CaptureRelayStatus>>
    where
        F: FnOnce(&CaptureRelayTarget, &str, &CaptureEnvelope) -> CaptureRelayDelivery,
    {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(denied());
        }
        let at = OffsetDateTime::now_utc().unix_timestamp();
        let Some(claim) = self.claim(1, at)?.pop() else {
            return Ok(None);
        };
        let result = send(claim.target(), token, claim.envelope());
        self.complete(&claim, result, OffsetDateTime::now_utc().unix_timestamp())
            .map(Some)
    }
    /// Sweep expires encrypted payloads, including rejected/exhausted records. Tombstones remain.
    pub fn sweep(&mut self, at: i64) -> Result<CaptureRelaySnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = records(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        maintain(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            at,
            &mut rs,
        )?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        self.snapshot()
    }
    pub fn snapshot(&self) -> Result<CaptureRelaySnapshot> {
        let control: RelayControl = unseal(&self.connection, &self.key, &self.binding, "control")?;
        let rs = records(
            &self.connection,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        Ok(CaptureRelaySnapshot {
            version: CAPTURE_RELAY_VERSION.into(),
            stopped: control.stopped,
            pending_events: rs.iter().filter(|r| r.envelope.is_some()).count(),
            pending_bytes: rs
                .iter()
                .filter_map(|r| r.envelope.as_ref())
                .map(|e| e.content.len())
                .sum(),
            events: rs.iter().map(RelayRecord::status).collect(),
        })
    }
}
