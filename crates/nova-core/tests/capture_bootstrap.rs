use nova_core::{VaultKey, compilation::AssetScope, intake::*};
use time::OffsetDateTime;
struct Fixture {
    dir: tempfile::TempDir,
    key: VaultKey,
    cfg: CaptureBootstrapConfiguration,
    store: IntakeStore,
    access: CaptureAccess,
    scope: AssetScope,
    at: i64,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let scope = AssetScope {
            owner_id: "owner:bootstrap-fixture".into(),
            space: "test".into(),
        };
        let mut store = IntakeStore::create(
            &dir.path().join("source.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        store.enable_capture(&scope).unwrap();
        let grant = store
            .create_capture_grant(
                &scope,
                CaptureGrantRequest {
                    request_id: "bootstrap-grant".into(),
                    source_namespace: "bootstrap-fixture".into(),
                    host: "codex".into(),
                    surface: "cli_hooks".into(),
                    host_version: "fixture".into(),
                    events: vec![
                        CaptureEventKind::UserPrompt,
                        CaptureEventKind::AssistantResponse,
                    ],
                    lifetime_seconds: 3600,
                    event_budget: 100,
                    byte_budget: 1024 * 1024,
                },
            )
            .unwrap();
        let access = store.authenticate_capture(&scope, &grant.token).unwrap();
        let cfg = CaptureBootstrapConfiguration {
            instance_id: grant.grant.instance_id,
            target_id: "loopback-fixture".into(),
            grant_id: grant.grant.grant_id,
            source_namespace: grant.grant.source_namespace,
            expires_at: grant.grant.expires_at,
            policy: CaptureRelayPolicy {
                version: "first-policy".into(),
                allowed_events: grant.grant.events,
                redact_literals: vec!["SECRET".into(), "REDACTED".into()],
                max_content_bytes: 64,
            },
            max_pending_events: 4,
            max_pending_bytes: 256,
            max_total_events: 8,
            ttl_seconds: 300,
            lease_seconds: 10,
            max_attempts: 3,
            retry_base_seconds: 2,
            retry_max_seconds: 60,
        };
        Self {
            dir,
            key,
            cfg,
            store,
            access,
            scope,
            at: OffsetDateTime::now_utc().unix_timestamp(),
        }
    }
    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("bootstrap.sqlite")
    }
    fn spool(&self) -> CaptureBootstrap {
        CaptureBootstrap::create(&self.path(), self.key.clone(), self.cfg.clone(), self.at).unwrap()
    }
    fn open(&self) -> CaptureBootstrap {
        CaptureBootstrap::open(&self.path(), self.key.clone(), self.cfg.clone()).unwrap()
    }
    fn session(&mut self) -> CaptureSession {
        self.store
            .register_capture_session(&self.scope, &self.access, "origin-1")
            .unwrap()
    }
    fn relay(&self, s: &CaptureSession) -> CaptureRelay {
        CaptureRelay::create(
            &self.dir.path().join("relay.sqlite"),
            self.key.clone(),
            self.cfg.relay_configuration(s).unwrap(),
            self.at,
        )
        .unwrap()
    }
}
fn event(id: &str) -> CaptureRelayEvent {
    CaptureRelayEvent {
        event_id: id.into(),
        sequence: 999,
        kind: CaptureEventKind::UserPrompt,
        content: "VISIBLE SECRET MARKER".into(),
        occurred_at: None,
        loss: CaptureLoss {
            truncated: false,
            redacted: false,
            partial_history: false,
        },
    }
}
#[test]
fn offline_enqueue_encrypted_stable_sequence_redacted_before_session_exists() {
    let f = Fixture::new();
    let mut s = f.spool();
    let a = s.enqueue("origin-1", event("e1"), f.at).unwrap();
    assert_eq!(a.sequence, 1);
    assert!(a.loss.redacted && a.loss.partial_history);
    assert!(a.transfer.is_none());
    drop(s);
    let bytes = std::fs::read(f.path()).unwrap();
    for needle in ["VISIBLE", "SECRET", "origin-1"] {
        assert!(!bytes.windows(needle.len()).any(|b| b == needle.as_bytes()));
    }
    let mut s = f.open();
    assert_eq!(
        s.enqueue("origin-1", event("e1"), f.at).unwrap().sequence,
        1
    );
    assert_eq!(
        s.enqueue("origin-1", event("e2"), f.at).unwrap().sequence,
        2
    );
    assert_eq!(
        s.enqueue("origin-2", event("e1"), f.at).unwrap().sequence,
        1
    );
    let mut changed = event("e1");
    changed.content.push('!');
    assert!(s.enqueue("origin-1", changed, f.at).is_err());
    let mut cfg = f.cfg.clone();
    cfg.policy.version = "changed".into();
    assert!(CaptureBootstrap::open(&f.path(), f.key.clone(), cfg).is_err());
}
#[test]
fn registration_transfer_is_local_only_then_exact_source_ack() {
    let mut f = Fixture::new();
    let mut spool = f.spool();
    let e = spool.enqueue("origin-1", event("e1"), f.at).unwrap();
    let claim = spool.claim(None, f.at + 1).unwrap().unwrap();
    let session = f.session();
    let mut relay = f.relay(&session);
    let status = spool
        .transfer(&claim, &session, &mut relay, f.at + 2)
        .unwrap();
    assert_eq!(status.state, CaptureBootstrapState::Transferred);
    assert_eq!(spool.snapshot(f.at + 2).unwrap().pending_events, 0);
    let snap = relay.snapshot().unwrap();
    assert_eq!(snap.events[0].state, CaptureRelayState::Queued);
    assert_eq!(snap.events[0].expires_at, e.expires_at);
    let c = relay.claim(1, f.at + 2).unwrap().remove(0);
    assert_eq!(c.envelope().content, "VISIBLE [REDACTED] MARKER");
    assert_eq!(c.envelope().session_id, session.session_id);
    assert_eq!(c.envelope().sequence, 1);
    let ack = f
        .store
        .capture_event(
            &f.scope,
            &f.access,
            &serde_json::to_vec(c.envelope()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        relay
            .complete(&c, CaptureRelayDelivery::SourceAck(Box::new(ack)), f.at + 2)
            .unwrap()
            .state,
        CaptureRelayState::Acknowledged
    );
}
#[test]
fn crash_after_relay_commit_replays_without_new_sequence_or_payload() {
    let mut f = Fixture::new();
    let mut spool = f.spool();
    spool.enqueue("origin-1", event("e1"), f.at).unwrap();
    let claim = spool.claim(None, f.at).unwrap().unwrap();
    drop(spool);
    let before = std::fs::read(f.path()).unwrap();
    let session = f.session();
    let mut relay = f.relay(&session);
    let mut spool = f.open();
    let a = spool
        .transfer(&claim, &session, &mut relay, f.at + 1)
        .unwrap();
    drop(spool);
    // Model process/power loss after durable relay enqueue but before bootstrap's durable transfer marker.
    std::fs::write(f.path(), before).unwrap();
    let mut spool = f.open();
    assert_eq!(
        spool.snapshot(f.at + 11).unwrap().events[0].state,
        CaptureBootstrapState::RetryWait
    );
    let claim = spool.claim(None, f.at + 13).unwrap().unwrap();
    let b = spool
        .transfer(&claim, &session, &mut relay, f.at + 13)
        .unwrap();
    assert_eq!(
        a.transfer.unwrap().relay_event_key,
        b.transfer.unwrap().relay_event_key
    );
    assert_eq!(relay.snapshot().unwrap().events.len(), 1);
    assert_eq!(relay.snapshot().unwrap().events[0].sequence, 1);
}
#[test]
fn bounded_retry_lease_ttl_revocation_and_capacity_never_ack() {
    let f = Fixture::new();
    let mut spool = f.spool();
    for id in ["a", "b", "c", "d"] {
        spool.enqueue("origin-1", event(id), f.at).unwrap();
    }
    assert!(spool.enqueue("origin-1", event("full"), f.at).is_err());
    let c = spool.claim(None, f.at).unwrap().unwrap();
    assert_eq!(
        spool.retry(&c, None, Some(100), f.at).unwrap().state,
        CaptureBootstrapState::Exhausted
    );
    let c = spool.claim(None, f.at).unwrap().unwrap();
    spool
        .retry(&c, Some(CaptureRelayStop::Revoked), None, f.at)
        .unwrap();
    assert!(spool.claim(None, f.at).unwrap().is_none());
    assert!(
        spool
            .enqueue("origin-1", event("after-revoke"), f.at)
            .is_err()
    );
    let s = spool.snapshot(f.at + 301).unwrap();
    assert_eq!(s.pending_events, 0);
    assert!(
        s.events
            .iter()
            .all(|e| e.state == CaptureBootstrapState::Expired)
    );
    assert!(spool.snapshot(f.at + 300).is_err());
}
#[test]
fn transfer_rejects_other_origin_or_fixed_target_before_enqueue() {
    let mut f = Fixture::new();
    let mut spool = f.spool();
    spool.enqueue("origin-1", event("e1"), f.at).unwrap();
    let c = spool.claim(None, f.at).unwrap().unwrap();
    let mut session = f.session();
    let mut relay = f.relay(&session);
    session.origin_session_id = "other".into();
    assert!(spool.transfer(&c, &session, &mut relay, f.at).is_err());
    assert_eq!(relay.snapshot().unwrap().events.len(), 0);
}
#[test]
fn bootstrap_truncates_utf8_and_json_escape_expansion_before_storage() {
    let mut f = Fixture::new();
    f.cfg.policy.max_content_bytes = 32 * 1024;
    f.cfg.max_pending_bytes = 128 * 1024;
    let mut spool = f.spool();
    let mut e = event("large");
    e.content = "x\u{1}".repeat(16 * 1024);
    let result = spool.enqueue("origin-1", e, f.at).unwrap();
    assert!(result.loss.truncated);
    let c = spool.claim(None, f.at).unwrap().unwrap();
    let session = f.session();
    let mut relay = f.relay(&session);
    spool.transfer(&c, &session, &mut relay, f.at).unwrap();
    let c = relay.claim(1, f.at).unwrap().remove(0);
    assert!(serde_json::to_vec(c.envelope()).unwrap().len() <= 96 * 1024);
}
