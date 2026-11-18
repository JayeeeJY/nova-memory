use nova_core::{VaultKey, compilation::*, intake::*};
use serde_json::json;
use time::OffsetDateTime;

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:relay-fixture".into(),
        space: "synthetic".into(),
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    path: std::path::PathBuf,
    key: VaultKey,
    config: CaptureRelayConfiguration,
    at: i64,
    token: String,
    access: CaptureAccess,
    store: IntakeStore,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.sqlite");
        let mut store = IntakeStore::create(
            &dir.path().join("source.sqlite"),
            VaultKey::generate().unwrap(),
            scope(),
        )
        .unwrap();
        store.enable_capture(&scope()).unwrap();
        let issued = store
            .create_capture_grant(
                &scope(),
                CaptureGrantRequest {
                    request_id: "grant-fixture".into(),
                    source_namespace: "adapter:fixture".into(),
                    host: "codex".into(),
                    surface: "visible_hook".into(),
                    host_version: "test-v01".into(),
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
        let access = store.authenticate_capture(&scope(), &issued.token).unwrap();
        let session = store
            .register_capture_session(&scope(), &access, "origin-session-1")
            .unwrap();
        let config = CaptureRelayConfiguration {
            target: CaptureRelayTarget {
                instance_id: issued.grant.instance_id,
                target_id: "loopback-fixture".into(),
                grant_id: issued.grant.grant_id,
                source_namespace: issued.grant.source_namespace,
                session_id: session.session_id,
                expires_at: session.expires_at,
            },
            policy: CaptureRelayPolicy {
                version: "fixture-policy-v1".into(),
                allowed_events: vec![
                    CaptureEventKind::UserPrompt,
                    CaptureEventKind::AssistantResponse,
                ],
                redact_literals: vec!["SECRET-MARKER".into()],
                max_content_bytes: 32 * 1024,
            },
            max_pending_events: 8,
            max_pending_bytes: 65536,
            max_total_events: 16,
            ttl_seconds: 300,
            lease_seconds: 10,
            max_attempts: 3,
            retry_base_seconds: 2,
            retry_max_seconds: 60,
        };
        Self {
            dir,
            path,
            key: VaultKey::generate().unwrap(),
            config,
            at: OffsetDateTime::now_utc().unix_timestamp(),
            token: issued.token,
            access,
            store,
        }
    }
    fn relay(&self) -> CaptureRelay {
        CaptureRelay::create(&self.path, self.key.clone(), self.config.clone(), self.at).unwrap()
    }
    fn reopen(&self) -> CaptureRelay {
        CaptureRelay::open(&self.path, self.key.clone(), self.config.clone()).unwrap()
    }
    fn ack(&mut self, claim: &CaptureRelayClaim) -> CaptureAck {
        self.store
            .capture_event(
                &scope(),
                &self.access,
                &serde_json::to_vec(claim.envelope()).unwrap(),
            )
            .unwrap()
    }
}
fn event(id: &str, seq: u64) -> CaptureRelayEvent {
    CaptureRelayEvent {
        event_id: id.into(),
        sequence: seq,
        kind: CaptureEventKind::UserPrompt,
        content: "SYNTHETIC-VISIBLE-PAYLOAD SECRET-MARKER 你好世界".into(),
        occurred_at: None,
        loss: CaptureLoss {
            truncated: false,
            redacted: false,
            partial_history: false,
        },
    }
}
fn send_retry(
    r: &mut CaptureRelay,
    c: &CaptureRelayClaim,
    at: i64,
    after: Option<u32>,
) -> CaptureRelayStatus {
    r.complete(
        c,
        CaptureRelayDelivery::Retry {
            retry_after_seconds: after,
        },
        at,
    )
    .unwrap()
}

#[test]
fn visible_policy_is_applied_before_encrypted_persistence_and_never_recursive() {
    let mut f = Fixture::new();
    f.config.policy.max_content_bytes = 42;
    f.config.policy.redact_literals.push("E".into());
    let mut r = f.relay();
    let queued = r.enqueue(event("event-one", 1), f.at).unwrap();
    assert!(!queued.source_acknowledged);
    assert!(queued.loss.redacted && queued.loss.truncated && queued.loss.partial_history);
    let claim = r.claim(1, f.at).unwrap().pop().unwrap();
    assert!(claim.envelope().content.len() <= 42);
    assert!(!claim.envelope().content.contains("SECRET-MARKER"));
    assert!(claim.envelope().content.contains("[REDACTED]"));
    drop(r);
    let bytes = std::fs::read(&f.path).unwrap();
    for marker in [
        "SYNTHETIC-VISIBLE-PAYLOAD",
        "SECRET-MARKER",
        "[REDACTED]",
        &f.config.target.instance_id,
        &f.token,
    ] {
        assert!(!bytes.windows(marker.len()).any(|w| w == marker.as_bytes()));
    }
    let r = f.reopen();
    assert_eq!(r.snapshot().unwrap().pending_events, 1);
}

#[test]
fn exact_source_ack_removes_payload_and_duplicate_returns_original_identity() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    let queued = r.enqueue(event("event-one", 1), f.at).unwrap();
    let claim = r.claim(1, f.at).unwrap().pop().unwrap();
    let ack = f.ack(&claim);
    let receipt = ack.receipt.clone();
    let status = r
        .complete(&claim, CaptureRelayDelivery::SourceAck(Box::new(ack)), f.at)
        .unwrap();
    assert_eq!(status.state, CaptureRelayState::Acknowledged);
    assert_eq!(status.receipt, Some(receipt.clone()));
    assert_eq!(r.snapshot().unwrap().pending_bytes, 0);
    drop(r);
    let mut r = f.reopen();
    let repeated = r.enqueue(event("event-one", 1), f.at).unwrap();
    assert!(repeated.duplicate && repeated.source_acknowledged);
    assert_eq!(repeated.event_key, queued.event_key);
    assert!(r.claim(1, f.at).unwrap().is_empty());
    assert_eq!(
        f.store
            .source(&scope(), &receipt.source.object_id)
            .unwrap()
            .content,
        "SYNTHETIC-VISIBLE-PAYLOAD [REDACTED] 你好世界"
    );
}

#[test]
fn partial_ack_lost_response_and_restart_reuse_same_event_bytes_and_source() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    r.enqueue(event("b", 2), f.at).unwrap();
    let claims = r.claim(2, f.at).unwrap();
    let lost = f.ack(&claims[0]);
    let second = f.ack(&claims[1]);
    let first_bytes = serde_json::to_vec(claims[0].envelope()).unwrap();
    r.complete(
        &claims[1],
        CaptureRelayDelivery::SourceAck(Box::new(second)),
        f.at,
    )
    .unwrap();
    drop(r);
    let mut r = f.reopen();
    assert!(r.claim(2, f.at + 10).unwrap().is_empty());
    assert_eq!(r.snapshot().unwrap().pending_events, 1);
    let retry = r.claim(2, f.at + 12).unwrap().pop().unwrap();
    assert_eq!(serde_json::to_vec(retry.envelope()).unwrap(), first_bytes);
    assert_eq!(retry.event_key(), claims[0].event_key());
    let again = f.ack(&retry);
    assert_eq!(again.receipt, lost.receipt);
    r.complete(
        &retry,
        CaptureRelayDelivery::SourceAck(Box::new(again)),
        f.at + 12,
    )
    .unwrap();
    assert_eq!(r.snapshot().unwrap().pending_events, 0);
    assert_eq!(
        f.store
            .capture_grant(&scope(), &f.config.target.grant_id)
            .unwrap()
            .received_events,
        2
    );
}

#[test]
fn concurrent_handles_cannot_claim_same_event_and_stale_lease_cannot_ack() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let mut other = f.reopen();
    let stale = r.claim(1, f.at).unwrap().pop().unwrap();
    assert!(other.claim(1, f.at).unwrap().is_empty());
    let ack = f.ack(&stale);
    other.sweep(f.at + 10).unwrap();
    let fresh = other.claim(1, f.at + 12).unwrap().pop().unwrap();
    assert!(
        r.complete(
            &stale,
            CaptureRelayDelivery::SourceAck(Box::new(ack.clone())),
            f.at + 12
        )
        .is_err()
    );
    other
        .complete(
            &fresh,
            CaptureRelayDelivery::SourceAck(Box::new(ack)),
            f.at + 12,
        )
        .unwrap();
    assert_eq!(r.snapshot().unwrap().pending_events, 0);
}

#[test]
fn queue_capacity_and_lifetime_tombstones_never_claim_source_ack() {
    let mut f = Fixture::new();
    f.config.max_pending_events = 1;
    f.config.max_total_events = 2;
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    assert!(
        r.enqueue(event("b", 2), f.at)
            .unwrap_err()
            .to_string()
            .contains("capacity_full_no_ack")
    );
    assert_eq!(r.snapshot().unwrap().events.len(), 1);
    r.sweep(f.at + 300).unwrap();
    let expired = r.enqueue(event("a", 1), f.at + 300).unwrap();
    assert_eq!(expired.state, CaptureRelayState::Expired);
    assert!(!expired.source_acknowledged);
    r.enqueue(event("b", 2), f.at + 300).unwrap();
    r.sweep(f.at + 600).unwrap();
    assert!(r.enqueue(event("c", 3), f.at + 600).is_err());
    assert_eq!(r.snapshot().unwrap().pending_events, 0);
    assert_eq!(r.snapshot().unwrap().events.len(), 2);
}

#[test]
fn byte_limit_is_applied_to_processed_content_without_partial_enqueue() {
    let mut f = Fixture::new();
    f.config.max_pending_bytes = 4;
    let mut r = f.relay();
    let mut e = event("a", 1);
    e.content = "你".into();
    r.enqueue(e.clone(), f.at).unwrap();
    e.event_id = "b".into();
    e.sequence = 2;
    assert!(r.enqueue(e, f.at).is_err());
    assert_eq!(r.snapshot().unwrap().pending_bytes, 3);
    assert_eq!(r.snapshot().unwrap().events.len(), 1);
}

#[test]
fn ack_mismatches_and_arbitrary_success_do_not_remove_payload() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let claim = r.claim(1, f.at).unwrap().pop().unwrap();
    let valid = f.ack(&claim);
    for field in [
        "instance_id",
        "session_id",
        "event_id",
        "sequence",
        "kind",
        "loss",
        "content",
        "request",
        "source",
        "schema",
        "size",
    ] {
        let mut ack = valid.clone();
        match field {
            "instance_id" => ack.instance_id = "other-instance".into(),
            "session_id" => ack.session_id = "other-session".into(),
            "event_id" => ack.event_id = "other-event".into(),
            "sequence" => ack.sequence += 1,
            "kind" => ack.kind = CaptureEventKind::AssistantResponse,
            "loss" => ack.loss.redacted = false,
            "content" => ack.receipt.content_sha256 = "0".repeat(64),
            "request" => ack.receipt.request_id = "wrong-event".into(),
            "source" => ack.receipt.source.revision = 0,
            "schema" => ack.schema_version = "other".into(),
            "size" => ack.interpretation = "x".repeat(8192),
            _ => unreachable!(),
        }
        assert!(
            r.complete(&claim, CaptureRelayDelivery::SourceAck(Box::new(ack)), f.at)
                .is_err(),
            "{field}"
        );
        assert_eq!(r.snapshot().unwrap().pending_events, 1);
    }
    r.complete(
        &claim,
        CaptureRelayDelivery::SourceAck(Box::new(valid)),
        f.at,
    )
    .unwrap();
}

#[test]
fn retry_after_backoff_attempt_limit_and_ttl_are_finite() {
    let f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    let status = send_retry(&mut r, &c, f.at, Some(20));
    assert_eq!(status.next_attempt_at, f.at + 20);
    assert!(r.claim(1, f.at + 19).unwrap().is_empty());
    let c = r.claim(1, f.at + 20).unwrap().pop().unwrap();
    send_retry(&mut r, &c, f.at + 20, None);
    assert!(r.claim(1, f.at + 23).unwrap().is_empty());
    let c = r.claim(1, f.at + 24).unwrap().pop().unwrap();
    assert_eq!(
        send_retry(&mut r, &c, f.at + 24, None).state,
        CaptureRelayState::Exhausted
    );
    assert!(r.claim(1, f.at + 100).unwrap().is_empty());
    assert_eq!(
        r.sweep(f.at + 300).unwrap().events[0].state,
        CaptureRelayState::Expired
    );
}

#[test]
fn excessive_retry_after_stops_instead_of_violating_server_backoff() {
    let f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    assert_eq!(
        send_retry(&mut r, &c, f.at, Some(u32::MAX)).state,
        CaptureRelayState::Exhausted
    );
    assert!(r.claim(1, f.at + 100).unwrap().is_empty());
}

#[test]
fn revoked_unauthorized_target_mismatch_stop_entire_queue_and_survive_restart() {
    for reason in [
        CaptureRelayStop::Revoked,
        CaptureRelayStop::Unauthorized,
        CaptureRelayStop::TargetMismatch,
    ] {
        let f = Fixture::new();
        let mut r = f.relay();
        r.enqueue(event("a", 1), f.at).unwrap();
        r.enqueue(event("b", 2), f.at).unwrap();
        let c = r.claim(1, f.at).unwrap().pop().unwrap();
        r.complete(&c, CaptureRelayDelivery::Reject(reason), f.at)
            .unwrap();
        drop(r);
        let mut r = f.reopen();
        assert_eq!(r.snapshot().unwrap().stopped, Some(reason));
        assert!(r.claim(1, f.at).unwrap().is_empty());
        assert!(r.enqueue(event("c", 3), f.at).is_err());
        assert_eq!(r.sweep(f.at + 300).unwrap().pending_events, 0);
    }
}

#[test]
fn permanent_event_rejection_is_visible_and_not_retried() {
    let f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    r.enqueue(event("b", 2), f.at).unwrap();
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    r.complete(
        &c,
        CaptureRelayDelivery::Reject(CaptureRelayStop::InvalidEvent),
        f.at,
    )
    .unwrap();
    let next = r.claim(1, f.at).unwrap().pop().unwrap();
    assert_eq!(next.envelope().event_id, "b");
    assert_eq!(
        r.snapshot().unwrap().events[0].state,
        CaptureRelayState::Rejected
    );
}

#[test]
fn binding_policy_payload_sequence_and_clock_changes_fail_closed() {
    let f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let mut changed = event("a", 1);
    changed.content.push('!');
    assert!(r.enqueue(changed, f.at).is_err());
    assert!(r.enqueue(event("b", 1), f.at).is_err());
    assert!(r.claim(1, f.at - 1).is_err());
    assert!(CaptureRelay::open(&f.path, VaultKey::generate().unwrap(), f.config.clone()).is_err());
    for part in [
        "target",
        "instance",
        "grant",
        "namespace",
        "session",
        "policy",
    ] {
        let mut cfg = f.config.clone();
        match part {
            "target" => cfg.target.target_id = "elsewhere".into(),
            "instance" => cfg.target.instance_id = "elsewhere".into(),
            "grant" => cfg.target.grant_id = "elsewhere".into(),
            "namespace" => cfg.target.source_namespace = "elsewhere".into(),
            "session" => cfg.target.session_id = "elsewhere".into(),
            "policy" => cfg.policy.version = "v2".into(),
            _ => unreachable!(),
        }
        assert!(CaptureRelay::open(&f.path, f.key.clone(), cfg).is_err());
    }
}

#[test]
fn only_visible_typed_events_and_explicit_bounds_are_accepted() {
    let f = Fixture::new();
    let mut r = f.relay();
    let original = serde_json::to_value(event("a", 1)).unwrap();
    for kind in [
        "system_prompt",
        "thinking",
        "tool_result",
        "tool_secret",
        "hidden",
    ] {
        let mut v = original.clone();
        v["kind"] = json!(kind);
        assert!(
            r.enqueue_json(&serde_json::to_vec(&v).unwrap(), f.at)
                .is_err()
        );
    }
    for field in [
        "target",
        "instance_id",
        "session_id",
        "grant_id",
        "scope",
        "token",
        "policy",
    ] {
        let mut v = original.clone();
        v[field] = json!("override");
        assert!(
            r.enqueue_json(&serde_json::to_vec(&v).unwrap(), f.at)
                .is_err()
        );
    }
    let mut e = event("a", 1);
    e.content = "x".repeat(128 * 1024 + 1);
    assert!(r.enqueue(e, f.at).is_err());
    let bytes = serde_json::to_string(&original).unwrap();
    let duplicate = bytes.replacen("{", "{\"sequence\":99,", 1);
    assert!(r.enqueue_json(duplicate.as_bytes(), f.at).is_err());
    assert!(r.snapshot().unwrap().events.is_empty());
}

#[test]
fn corrupted_ciphertext_and_source_ack_commit_failure_leave_no_false_success() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    let queued = r.enqueue(event("a", 1), f.at).unwrap();
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    let ack = f.ack(&c);
    let db = rusqlite::Connection::open(&f.path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE ON relay_objects WHEN OLD.id!='control' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        r.complete(
            &c,
            CaptureRelayDelivery::SourceAck(Box::new(ack.clone())),
            f.at
        )
        .is_err()
    );
    assert_eq!(r.snapshot().unwrap().pending_events, 1);
    db.execute_batch("DROP TRIGGER fail_ack;").unwrap();
    db.execute(
        "UPDATE relay_objects SET ciphertext=zeroblob(length(ciphertext)) WHERE id=?1",
        [queued.event_key],
    )
    .unwrap();
    assert!(r.snapshot().is_err());
    assert!(
        r.complete(&c, CaptureRelayDelivery::SourceAck(Box::new(ack)), f.at)
            .is_err()
    );
}

#[test]
fn dispatch_borrows_one_capture_token_for_one_fixed_target_and_keeps_no_token() {
    let mut f = Fixture::new();
    let mut r = f.relay();
    r.enqueue(event("a", 1), f.at).unwrap();
    let target = f.config.target.clone();
    let token = f.token.clone();
    let mut calls = 0;
    let status = r
        .dispatch_one(&token, |t, bearer, envelope| {
            calls += 1;
            assert_eq!(t, &target);
            assert_eq!(bearer, token);
            let a = f.store.authenticate_capture(&scope(), bearer).unwrap();
            CaptureRelayDelivery::SourceAck(Box::new(
                f.store
                    .capture_event(&scope(), &a, &serde_json::to_vec(envelope).unwrap())
                    .unwrap(),
            ))
        })
        .unwrap()
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(status.state, CaptureRelayState::Acknowledged);
    assert!(
        r.dispatch_one("root-bearer", |_, _, _| panic!("must not dispatch"))
            .is_err()
    );
    drop(r);
    for entry in std::fs::read_dir(f.dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()));
    }
}

#[test]
fn utf8_truncation_and_json_escaping_fit_the_durable_encrypted_row_bounds() {
    let mut f = Fixture::new();
    f.config.policy.max_content_bytes = 4;
    let mut r = f.relay();
    let mut e = event("a", 1);
    e.content = "你好".into();
    r.enqueue(e, f.at).unwrap();
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    assert_eq!(c.envelope().content, "你");
    assert!(c.envelope().loss.truncated);
    let mut f = Fixture::new();
    let mut r = f.relay();
    let mut e = event("b", 1);
    e.content = "\u{1}".repeat(32768);
    let queued = r.enqueue(e, f.at).unwrap();
    assert!(queued.loss.truncated);
    drop(r);
    let mut r = f.reopen();
    assert!(r.snapshot().unwrap().pending_bytes < 32768);
    let c = r.claim(1, f.at).unwrap().pop().unwrap();
    assert!(serde_json::to_vec(c.envelope()).unwrap().len() <= 96 * 1024);
    let ack = f.ack(&c);
    r.complete(&c, CaptureRelayDelivery::SourceAck(Box::new(ack)), f.at)
        .unwrap();
}

#[test]
fn native_adapter_sequence_is_atomic_persistent_and_never_reassigned() {
    let f = Fixture::new();
    let mut first = f.relay();
    let mut second = f.reopen();
    let a = first
        .enqueue_sequenced(event("stable-a", 999), f.at)
        .unwrap();
    let b = second
        .enqueue_sequenced(event("stable-b", 999), f.at)
        .unwrap();
    assert_eq!(a.sequence, 1);
    assert_eq!(b.sequence, 2);
    drop(first);
    let mut reopened = f.reopen();
    let repeated = reopened
        .enqueue_sequenced(event("stable-a", 0), f.at)
        .unwrap();
    assert_eq!(repeated.sequence, 1);
    assert!(repeated.duplicate);
    let mut changed = event("stable-a", 0);
    changed.content.push('!');
    assert!(reopened.enqueue_sequenced(changed, f.at).is_err());
    assert_eq!(reopened.snapshot().unwrap().events.len(), 2);
}
