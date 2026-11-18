//! Pre-session encrypted spool. Contains no fabricated CaptureEnvelope or server session ID.
//! Transfer is at-least-once across two durable stores and reuses the prepared event/sequence.
use super::*;

pub const CAPTURE_BOOTSTRAP_VERSION: &str = "nova.capture-bootstrap.v0.1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBootstrapConfiguration {
    pub instance_id: String,
    pub target_id: String,
    pub grant_id: String,
    pub source_namespace: String,
    pub expires_at: String,
    pub policy: CaptureRelayPolicy,
    pub max_pending_events: usize,
    pub max_pending_bytes: usize,
    pub max_total_events: usize,
    pub ttl_seconds: u32,
    pub lease_seconds: u32,
    pub max_attempts: u32,
    pub retry_base_seconds: u32,
    pub retry_max_seconds: u32,
}
impl CaptureBootstrapConfiguration {
    pub(super) fn validate(&self) -> Result<()> {
        if [
            &self.instance_id,
            &self.target_id,
            &self.grant_id,
            &self.source_namespace,
            &self.policy.version,
        ]
        .iter()
        .any(|s| !identifier(s))
            || self.policy.allowed_events.is_empty()
            || self.policy.allowed_events.len() > 2
            || (self.policy.allowed_events.len() == 2
                && self.policy.allowed_events[0] == self.policy.allowed_events[1])
            || !(1..=32 * 1024).contains(&self.policy.max_content_bytes)
            || self.policy.redact_literals.len() > 16
            || self
                .policy
                .redact_literals
                .iter()
                .any(|s| s.is_empty() || s.len() > 256)
            || !(1..=10000).contains(&self.max_total_events)
            || !(1..=self.max_total_events).contains(&self.max_pending_events)
            || !(1..=32 * 1024 * 1024).contains(&self.max_pending_bytes)
            || !(1..=86400).contains(&self.ttl_seconds)
            || !(1..=300).contains(&self.lease_seconds)
            || !(1..=10).contains(&self.max_attempts)
            || !(1..=60).contains(&self.retry_base_seconds)
            || !(self.retry_base_seconds..=3600).contains(&self.retry_max_seconds)
        {
            return Err(invalid("bootstrap_configuration_bounds"));
        }
        parse_time(&self.expires_at)?;
        Ok(())
    }
    /// The downstream policy is explicitly bound to the upstream policy digest and applies no
    /// second literal replacement. This prevents recursively redacting replacement markers.
    pub fn relay_configuration(
        &self,
        session: &CaptureSession,
    ) -> Result<CaptureRelayConfiguration> {
        self.validate()?;
        if session.source_namespace != self.source_namespace
            || !identifier(&session.session_id)
            || parse_time(&session.expires_at)? > parse_time(&self.expires_at)?
        {
            return Err(denied());
        }
        Ok(CaptureRelayConfiguration {
            target: CaptureRelayTarget {
                instance_id: self.instance_id.clone(),
                target_id: self.target_id.clone(),
                grant_id: self.grant_id.clone(),
                source_namespace: self.source_namespace.clone(),
                session_id: session.session_id.clone(),
                expires_at: session.expires_at.clone(),
            },
            policy: CaptureRelayPolicy {
                version: format!(
                    "prepared-{}",
                    hex::encode(Sha256::digest(serde_json::to_vec(&self.policy)?))
                ),
                allowed_events: self.policy.allowed_events.clone(),
                redact_literals: vec![],
                max_content_bytes: self.policy.max_content_bytes,
            },
            max_pending_events: self.max_pending_events,
            max_pending_bytes: self.max_pending_bytes,
            max_total_events: self.max_total_events,
            ttl_seconds: self.ttl_seconds,
            lease_seconds: self.lease_seconds,
            max_attempts: self.max_attempts,
            retry_base_seconds: self.retry_base_seconds,
            retry_max_seconds: self.retry_max_seconds,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureBootstrapState {
    Queued,
    InFlight,
    RetryWait,
    Transferred,
    Rejected,
    Exhausted,
    Expired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureBootstrapTransfer {
    pub session_id: String,
    pub relay_event_key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureBootstrapStatus {
    pub event_key: String,
    pub origin_session_id: String,
    pub sequence: u64,
    pub state: CaptureBootstrapState,
    pub attempts: u32,
    pub expires_at: i64,
    pub next_attempt_at: i64,
    pub loss: CaptureLoss,
    pub transfer: Option<CaptureBootstrapTransfer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureBootstrapSnapshot {
    pub version: String,
    pub stopped: Option<CaptureRelayStop>,
    pub pending_events: usize,
    pub pending_bytes: usize,
    pub events: Vec<CaptureBootstrapStatus>,
}
#[derive(Clone)]
pub struct CaptureBootstrapClaim {
    binding: String,
    event_key: String,
    lease_id: String,
    origin_session_id: String,
}
impl CaptureBootstrapClaim {
    pub fn origin_session_id(&self) -> &str {
        &self.origin_session_id
    }
}
#[derive(Serialize, Deserialize)]
struct Identity {
    version: String,
    configuration: CaptureBootstrapConfiguration,
    binding: String,
}
#[derive(Serialize, Deserialize)]
struct Record {
    status: CaptureBootstrapStatus,
    fingerprint: String,
    event: Option<CaptureRelayEvent>,
    lease: Option<RelayLease>,
}
pub struct CaptureBootstrap {
    connection: Connection,
    key: VaultKey,
    configuration: CaptureBootstrapConfiguration,
    binding: String,
}
fn record_list(c: &Connection, key: &VaultKey, binding: &str, max: usize) -> Result<Vec<Record>> {
    let ids=c.prepare("SELECT id FROM relay_objects WHERE id NOT IN ('identity','control') ORDER BY rowid LIMIT ?1")?.query_map([max as i64+1],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if ids.len() > max {
        return Err(invalid("bootstrap_record_limit"));
    }
    ids.into_iter()
        .map(|id| {
            let r: Record = unseal(c, key, binding, &id)?;
            if r.status.event_key != id {
                return Err(denied());
            }
            Ok(r)
        })
        .collect()
}
fn wait(c: &CaptureBootstrapConfiguration, attempts: u32) -> i64 {
    i64::from(
        c.retry_base_seconds
            .saturating_mul(1u32 << attempts.saturating_sub(1).min(10))
            .min(c.retry_max_seconds),
    )
}
fn sweep(
    c: &Connection,
    key: &VaultKey,
    binding: &str,
    cfg: &CaptureBootstrapConfiguration,
    rs: &mut [Record],
    at: i64,
) -> Result<()> {
    for r in rs {
        let changed = if r.event.is_some() && r.status.expires_at <= at {
            r.event = None;
            r.lease = None;
            r.status.state = CaptureBootstrapState::Expired;
            true
        } else if r.status.state == CaptureBootstrapState::InFlight
            && r.lease.as_ref().is_some_and(|l| l.until <= at)
        {
            r.lease = None;
            r.status.state = if r.status.attempts >= cfg.max_attempts {
                CaptureBootstrapState::Exhausted
            } else {
                CaptureBootstrapState::RetryWait
            };
            r.status.next_attempt_at = at + wait(cfg, r.status.attempts);
            true
        } else {
            false
        };
        if changed {
            seal(c, key, binding, &r.status.event_key, r)?;
        }
    }
    Ok(())
}
pub(super) fn prepared(
    mut event: CaptureRelayEvent,
    policy: &CaptureRelayPolicy,
) -> Result<CaptureRelayEvent> {
    if !identifier(&event.event_id)
        || event.content.trim().is_empty()
        || event.content.len() > MAX_INPUT_BYTES
        || !policy.allowed_events.contains(&event.kind)
    {
        return Err(invalid("bootstrap_visible_event_bounds"));
    }
    if let Some(t) = &event.occurred_at {
        if t.len() > 64 {
            return Err(invalid("bootstrap_event_time_bounds"));
        }
        parse_time(t)?;
    }
    let (content, redacted) = redact(&event.content, &policy.redact_literals);
    event.content = content;
    event.loss.redacted |= redacted;
    event.loss.partial_history = true;
    let max = policy.max_content_bytes;
    if event.content.len() > max {
        let mut end = max;
        while !event.content.is_char_boundary(end) {
            end -= 1;
        }
        event.content.truncate(end);
        event.loss.truncated = true;
    }
    // Reserve >4 KiB for the future real envelope's identifiers/metadata. No placeholder session exists.
    if serde_json::to_vec(&event)?.len() > 90 * 1024 {
        event.loss.truncated = true;
        let text = std::mem::take(&mut event.content);
        let overhead = serde_json::to_vec(&event)?.len();
        let budget = (90 * 1024usize).saturating_sub(overhead);
        let mut used = 0;
        for c in text.chars() {
            let n = match c {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                '\u{0}'..='\u{1f}' => 6,
                _ => c.len_utf8(),
            };
            if used + n > budget {
                break;
            }
            used += n;
            event.content.push(c);
        }
    }
    if event.content.trim().is_empty() {
        return Err(invalid("bootstrap_empty_after_policy"));
    }
    Ok(event)
}
impl CaptureBootstrap {
    /// Persist a terminal transport/credential decision across every origin session in this grant.
    pub fn stop(&mut self, reason: CaptureRelayStop, at: i64) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = advance(&tx, &self.key, &self.binding, at)?;
        control.stopped = Some(reason);
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(())
    }
    pub fn create(
        path: &Path,
        key: VaultKey,
        configuration: CaptureBootstrapConfiguration,
        at: i64,
    ) -> Result<Self> {
        configuration.validate()?;
        if at < 0 || parse_time(&configuration.expires_at)?.unix_timestamp() <= at {
            return Err(invalid("bootstrap_target_expired"));
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
        configure_relay(
            &connection,
            (configuration.max_pending_bytes * 6
                + configuration.max_total_events * 16 * 1024
                + 1024 * 1024)
                .div_ceil(4096),
        )?;
        connection.execute_batch("CREATE TABLE relay_objects(id TEXT PRIMARY KEY,nonce BLOB NOT NULL,ciphertext BLOB NOT NULL);")?;
        let binding = key.authentication_tag(&[
            b"capture-bootstrap:v1",
            &serde_json::to_vec(&configuration)?,
        ]);
        let mut s = Self {
            connection,
            key,
            configuration,
            binding,
        };
        let tx = s
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        seal(
            &tx,
            &s.key,
            "",
            "identity",
            &Identity {
                version: CAPTURE_BOOTSTRAP_VERSION.into(),
                configuration: s.configuration.clone(),
                binding: s.binding.clone(),
            },
        )?;
        seal(
            &tx,
            &s.key,
            &s.binding,
            "control",
            &RelayControl {
                clock: at,
                stopped: None,
            },
        )?;
        tx.commit()?;
        Ok(s)
    }
    pub fn open(
        path: &Path,
        key: VaultKey,
        expected: CaptureBootstrapConfiguration,
    ) -> Result<Self> {
        expected.validate()?;
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let i: Identity = unseal(&connection, &key, "", "identity")?;
        let binding =
            key.authentication_tag(&[b"capture-bootstrap:v1", &serde_json::to_vec(&expected)?]);
        if i.version != CAPTURE_BOOTSTRAP_VERSION
            || i.configuration != expected
            || i.binding != binding
        {
            return Err(denied());
        }
        let _: RelayControl = unseal(&connection, &key, &binding, "control")?;
        configure_relay(
            &connection,
            (expected.max_pending_bytes * 6 + expected.max_total_events * 16 * 1024 + 1024 * 1024)
                .div_ceil(4096),
        )?;
        Ok(Self {
            connection,
            key,
            configuration: expected,
            binding,
        })
    }
    pub fn enqueue(
        &mut self,
        origin: &str,
        mut event: CaptureRelayEvent,
        at: i64,
    ) -> Result<CaptureBootstrapStatus> {
        if !identifier(origin) {
            return Err(invalid("bootstrap_origin_required"));
        }
        event.sequence = 0;
        let fingerprint = self
            .key
            .authentication_tag(&[b"bootstrap-input:v1", &serde_json::to_vec(&event)?]);
        let mut event = prepared(event, &self.configuration.policy)?;
        let id = self.key.authentication_tag(&[
            b"bootstrap-event:v1",
            self.binding.as_bytes(),
            origin.as_bytes(),
            event.event_id.as_bytes(),
            &serde_json::to_vec(&event.kind)?,
        ]);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = record_list(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        sweep(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            &mut rs,
            at,
        )?;
        if let Some(r) = rs.iter().find(|r| r.status.event_key == id) {
            if r.fingerprint != fingerprint {
                return Err(conflict());
            }
            let result = r.status.clone();
            seal(&tx, &self.key, &self.binding, "control", &control)?;
            tx.commit()?;
            return Ok(result);
        }
        if control.stopped.is_some() {
            return Err(denied());
        }
        let expiry = parse_time(&self.configuration.expires_at)?
            .unix_timestamp()
            .min(at + i64::from(self.configuration.ttl_seconds));
        if expiry <= at {
            return Err(invalid("bootstrap_target_expired"));
        }
        let payloads: Vec<_> = rs.iter().filter_map(|r| r.event.as_ref()).collect();
        if rs.len() >= self.configuration.max_total_events
            || payloads.len() >= self.configuration.max_pending_events
            || payloads.iter().map(|e| e.content.len()).sum::<usize>() + event.content.len()
                > self.configuration.max_pending_bytes
        {
            return Err(invalid("bootstrap_capacity_full_no_ack"));
        }
        let origins: std::collections::BTreeSet<_> = rs
            .iter()
            .map(|r| r.status.origin_session_id.as_str())
            .collect();
        if !origins.contains(origin) && origins.len() >= 64 {
            return Err(invalid("bootstrap_session_capacity"));
        }
        event.sequence = rs
            .iter()
            .filter(|r| r.status.origin_session_id == origin)
            .map(|r| r.status.sequence)
            .max()
            .unwrap_or(0)
            + 1;
        let status = CaptureBootstrapStatus {
            event_key: id.clone(),
            origin_session_id: origin.into(),
            sequence: event.sequence,
            state: CaptureBootstrapState::Queued,
            attempts: 0,
            expires_at: expiry,
            next_attempt_at: at,
            loss: event.loss.clone(),
            transfer: None,
        };
        seal(
            &tx,
            &self.key,
            &self.binding,
            &id,
            &Record {
                status: status.clone(),
                fingerprint,
                event: Some(event),
                lease: None,
            },
        )?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(status)
    }
    pub fn claim(
        &mut self,
        origin: Option<&str>,
        at: i64,
    ) -> Result<Option<CaptureBootstrapClaim>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = record_list(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        sweep(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            &mut rs,
            at,
        )?;
        let mut result = None;
        if control.stopped.is_none()
            && let Some(r) = rs.iter_mut().find(|r| {
                origin.is_none_or(|o| o == r.status.origin_session_id)
                    && matches!(
                        r.status.state,
                        CaptureBootstrapState::Queued | CaptureBootstrapState::RetryWait
                    )
                    && r.status.next_attempt_at <= at
            })
        {
            let id = uuid::Uuid::now_v7().to_string();
            r.status.state = CaptureBootstrapState::InFlight;
            r.status.attempts += 1;
            r.lease = Some(RelayLease {
                id: id.clone(),
                until: (at + i64::from(self.configuration.lease_seconds)).min(r.status.expires_at),
            });
            result = Some(CaptureBootstrapClaim {
                binding: self.binding.clone(),
                event_key: r.status.event_key.clone(),
                lease_id: id,
                origin_session_id: r.status.origin_session_id.clone(),
            });
            seal(&tx, &self.key, &self.binding, &r.status.event_key, r)?;
        }
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(result)
    }
    fn leased(
        c: &Connection,
        key: &VaultKey,
        binding: &str,
        claim: &CaptureBootstrapClaim,
        at: i64,
    ) -> Result<Record> {
        if binding != claim.binding {
            return Err(denied());
        }
        let r: Record = unseal(c, key, binding, &claim.event_key)?;
        if r.status.event_key != claim.event_key
            || r.status.origin_session_id != claim.origin_session_id
            || r.status.state != CaptureBootstrapState::InFlight
            || r.status.expires_at <= at
            || !r
                .lease
                .as_ref()
                .is_some_and(|l| l.id == claim.lease_id && l.until > at)
        {
            return Err(conflict());
        }
        Ok(r)
    }
    /// Durable local relay commit happens before the bootstrap payload is removed. A crash between
    /// commits replays identical event bytes/sequence into the relay. Transferred is never a Source ACK.
    pub fn transfer(
        &mut self,
        claim: &CaptureBootstrapClaim,
        session: &CaptureSession,
        relay: &mut CaptureRelay,
        at: i64,
    ) -> Result<CaptureBootstrapStatus> {
        if session.origin_session_id != claim.origin_session_id
            || relay.configuration != self.configuration.relay_configuration(session)?
        {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut r = Self::leased(&tx, &self.key, &self.binding, claim, at)?;
        let enqueued = relay.enqueue_inner(
            r.event
                .clone()
                .ok_or_else(|| invalid("bootstrap_payload_missing"))?,
            at,
            false,
            Some(r.status.expires_at),
        )?;
        r.status.transfer = Some(CaptureBootstrapTransfer {
            session_id: session.session_id.clone(),
            relay_event_key: enqueued.event_key,
        });
        r.status.state = CaptureBootstrapState::Transferred;
        r.event = None;
        r.lease = None;
        seal(&tx, &self.key, &self.binding, &r.status.event_key, &r)?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(r.status)
    }
    pub fn retry(
        &mut self,
        claim: &CaptureBootstrapClaim,
        stop: Option<CaptureRelayStop>,
        retry_after: Option<u32>,
        at: i64,
    ) -> Result<CaptureBootstrapStatus> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut control = advance(&tx, &self.key, &self.binding, at)?;
        let mut r = Self::leased(&tx, &self.key, &self.binding, claim, at)?;
        r.lease = None;
        if let Some(reason) = stop {
            r.status.state = CaptureBootstrapState::Rejected;
            control.stopped = Some(reason);
        } else {
            let wait = wait(&self.configuration, r.status.attempts)
                .max(i64::from(retry_after.unwrap_or(0)));
            r.status.next_attempt_at = at + wait;
            r.status.state = if r.status.attempts >= self.configuration.max_attempts
                || wait > i64::from(self.configuration.retry_max_seconds)
                || r.status.next_attempt_at >= r.status.expires_at
            {
                CaptureBootstrapState::Exhausted
            } else {
                CaptureBootstrapState::RetryWait
            };
        }
        seal(&tx, &self.key, &self.binding, &r.status.event_key, &r)?;
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(r.status)
    }
    pub fn snapshot(&mut self, at: i64) -> Result<CaptureBootstrapSnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let control = advance(&tx, &self.key, &self.binding, at)?;
        let mut rs = record_list(
            &tx,
            &self.key,
            &self.binding,
            self.configuration.max_total_events,
        )?;
        sweep(
            &tx,
            &self.key,
            &self.binding,
            &self.configuration,
            &mut rs,
            at,
        )?;
        let result = CaptureBootstrapSnapshot {
            version: CAPTURE_BOOTSTRAP_VERSION.into(),
            stopped: control.stopped,
            pending_events: rs.iter().filter(|r| r.event.is_some()).count(),
            pending_bytes: rs
                .iter()
                .filter_map(|r| r.event.as_ref())
                .map(|e| e.content.len())
                .sum(),
            events: rs.into_iter().map(|r| r.status).collect(),
        };
        seal(&tx, &self.key, &self.binding, "control", &control)?;
        tx.commit()?;
        Ok(result)
    }
}
