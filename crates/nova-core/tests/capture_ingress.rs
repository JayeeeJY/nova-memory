use nova_core::{compilation::*, intake::*, *};
use serde_json::{Value, json};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:fixture".into(),
        space: "synthetic".into(),
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("capture.sqlite");
    let k = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&p, k.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.enable_capture(&scope()).unwrap();
    (d, p, k, s)
}
fn request(id: &str, namespace: &str) -> CaptureGrantRequest {
    CaptureGrantRequest {
        request_id: id.into(),
        source_namespace: namespace.into(),
        host: "codex".into(),
        surface: "cli_hooks".into(),
        host_version: "fixture-0.1".into(),
        events: vec![
            CaptureEventKind::UserPrompt,
            CaptureEventKind::AssistantResponse,
        ],
        lifetime_seconds: 3600,
        event_budget: 100,
        byte_budget: 1_000_000,
    }
}
fn connected(s: &mut IntakeStore) -> (IssuedCaptureGrant, CaptureAccess, CaptureSession) {
    let g = s
        .create_capture_grant(&scope(), request("g1", "local:test"))
        .unwrap();
    let a = s.authenticate_capture(&scope(), &g.token).unwrap();
    let session = s
        .register_capture_session(&scope(), &a, "session-one")
        .unwrap();
    (g, a, session)
}
fn event(s: &CaptureSession, id: &str, seq: u64) -> Value {
    json!({"schema_version":CAPTURE_VERSION,"session_id":s.session_id,"event_id":id,"sequence":seq,"kind":"user_prompt","content":"SYNTHETIC-CAPTURE-SECRET 用户输入","occurred_at":null,"loss":{"truncated":false,"redacted":false,"partial_history":true}})
}
fn send(s: &mut IntakeStore, a: &CaptureAccess, v: &Value) -> Result<CaptureAck> {
    s.capture_event(&scope(), a, &serde_json::to_vec(v).unwrap())
}
#[test]
fn durable_source_and_role_are_separate_from_compilation_or_read_authority() {
    let (_d, _p, _k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    let ack = send(&mut s, &a, &e).unwrap();
    let src = s.source(&scope(), &ack.receipt.source.object_id).unwrap();
    assert_eq!(src.role, SourceRole::User);
    assert_eq!(src.source_platform, "codex:cli_hooks");
    assert_eq!(src.capture_fidelity, CaptureFidelity::F2VisibleTranscript);
    assert!(ack.loss.partial_history);
    assert_eq!(
        s.job(&scope(), &ack.receipt.job_id).unwrap().job.state,
        JobState::Received
    );
    assert!(s.claim_next(&scope(), 30).unwrap().is_none());
    s.enable_connections(&scope()).unwrap();
    assert!(s.authenticate_connection(&scope(), &g.token).is_err());
    let mut assistant = event(&session, "e2", 2);
    assistant["kind"] = json!("assistant_response");
    let second = send(&mut s, &a, &assistant).unwrap();
    assert_eq!(
        s.source(&scope(), &second.receipt.source.object_id)
            .unwrap()
            .role,
        SourceRole::Assistant
    );
}
#[test]
fn duplicate_across_reopen_and_token_rotation_has_one_source_and_charge() {
    let (_d, p, k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    let first = send(&mut s, &a, &e).unwrap();
    drop(s);
    let mut s = IntakeStore::open(&p, k, scope()).unwrap();
    let again = send(&mut s, &a, &e).unwrap();
    assert_eq!(first.receipt, again.receipt);
    assert_eq!(
        s.capture_grant(&scope(), &g.grant.grant_id)
            .unwrap()
            .received_events,
        1
    );
    s.revoke_capture(&scope(), &g.grant.grant_id).unwrap();
    assert!(send(&mut s, &a, &e).is_err());
    let rotated = s
        .create_capture_grant(&scope(), request("g2", "local:test"))
        .unwrap();
    let next = s.authenticate_capture(&scope(), &rotated.token).unwrap();
    let session2 = s
        .register_capture_session(&scope(), &next, "session-one")
        .unwrap();
    assert_eq!(session.session_id, session2.session_id);
    assert_eq!(send(&mut s, &next, &e).unwrap().receipt, first.receipt);
    assert_eq!(
        s.capture_grant(&scope(), &rotated.grant.grant_id)
            .unwrap()
            .received_events,
        0
    );
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1);
}
#[test]
fn stable_identity_rejects_changed_payload_sequence_collision_and_second_owner() {
    let (_d, _p, _k, mut s) = setup();
    let (_g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    send(&mut s, &a, &e).unwrap();
    let mut changed = e.clone();
    changed["content"] = json!("different");
    assert!(send(&mut s, &a, &changed).is_err());
    assert!(send(&mut s, &a, &event(&session, "e2", 1)).is_err());
    assert!(
        s.create_capture_grant(&scope(), request("g2", "local:test"))
            .is_err()
    );
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1);
}
#[test]
fn untrusted_envelope_cannot_select_identity_scope_privileges_or_hidden_events() {
    let (_d, _p, _k, mut s) = setup();
    let (_g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    for field in [
        "scope",
        "host",
        "role",
        "user_requested_save",
        "grant_id",
        "owner_id",
    ] {
        let mut bad = e.clone();
        bad[field] = json!("spoof");
        assert!(send(&mut s, &a, &bad).is_err());
    }
    let mut bad = e.clone();
    bad["kind"] = json!("system_prompt");
    assert!(send(&mut s, &a, &bad).is_err());
    let bytes = serde_json::to_string(&e).unwrap();
    let duplicate = bytes.replacen("{", "{\"sequence\":1,", 1);
    assert!(s.capture_event(&scope(), &a, duplicate.as_bytes()).is_err());
    let mut foreign = scope();
    foreign.owner_id = "someone:else".into();
    assert!(s.capture_event(&foreign, &a, bytes.as_bytes()).is_err());
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 0);
}
#[test]
fn source_withdrawal_rejects_replay_even_after_credential_rotation() {
    let (_d, _p, _k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    let ack = send(&mut s, &a, &e).unwrap();
    s.withdraw_source(&scope(), &ack.receipt.source.object_id)
        .unwrap();
    assert!(send(&mut s, &a, &e).is_err());
    s.revoke_capture(&scope(), &g.grant.grant_id).unwrap();
    let new = s
        .create_capture_grant(&scope(), request("g2", "local:test"))
        .unwrap();
    let a2 = s.authenticate_capture(&scope(), &new.token).unwrap();
    assert!(send(&mut s, &a2, &e).is_err());
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 0);
}
#[test]
fn budgets_and_rate_are_transactional_replay_does_not_spend() {
    let (_d, _p, _k, mut s) = setup();
    let mut req = request("budget", "budget");
    req.event_budget = 1;
    let g = s.create_capture_grant(&scope(), req).unwrap();
    let a = s.authenticate_capture(&scope(), &g.token).unwrap();
    let session = s.register_capture_session(&scope(), &a, "session").unwrap();
    let e = event(&session, "e1", 1);
    send(&mut s, &a, &e).unwrap();
    send(&mut s, &a, &e).unwrap();
    assert!(send(&mut s, &a, &event(&session, "e2", 2)).is_err());
    assert_eq!(
        s.capture_grant(&scope(), &g.grant.grant_id)
            .unwrap()
            .received_events,
        1
    );
    let mut req = request("bytes", "bytes");
    req.byte_budget = 1;
    let g = s.create_capture_grant(&scope(), req).unwrap();
    let a = s.authenticate_capture(&scope(), &g.token).unwrap();
    let session = s.register_capture_session(&scope(), &a, "session").unwrap();
    assert!(send(&mut s, &a, &event(&session, "e1", 1)).is_err());
    assert_eq!(
        s.capture_grant(&scope(), &g.grant.grant_id)
            .unwrap()
            .received_events,
        0
    );
}
#[test]
fn backup_keeps_originals_but_revokes_capture_authority_without_touching_live() {
    let (d, _p, k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    let ack = send(&mut s, &a, &e).unwrap();
    let path = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &path).unwrap();
    let mut copy = IntakeStore::open(&path, k, scope()).unwrap();
    assert!(copy.authenticate_capture(&scope(), &g.token).is_err());
    assert!(send(&mut copy, &a, &e).is_err());
    assert!(copy.source(&scope(), &ack.receipt.source.object_id).is_ok());
    assert!(send(&mut s, &a, &e).is_ok());
}
#[test]
fn authenticated_access_rechecked_at_delivery_and_sessions_cannot_cross_namespaces() {
    let (_d, _p, _k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let g2 = s
        .create_capture_grant(&scope(), request("other", "other"))
        .unwrap();
    let other = s.authenticate_capture(&scope(), &g2.token).unwrap();
    assert!(send(&mut s, &other, &event(&session, "e1", 1)).is_err());
    s.revoke_capture(&scope(), &g.grant.grant_id).unwrap();
    assert!(s.register_capture_session(&scope(), &a, "new").is_err());
    assert!(send(&mut s, &a, &event(&session, "e1", 1)).is_err());
}
#[test]
fn atomic_failure_leaves_no_source_receipt_or_charge_and_retry_is_possible() {
    let (_d, p, _k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch("CREATE TRIGGER fail_capture BEFORE INSERT ON intake_capture_slots BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let e = event(&session, "e1", 1);
    assert!(send(&mut s, &a, &e).is_err());
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 0);
    assert_eq!(
        s.capture_grant(&scope(), &g.grant.grant_id)
            .unwrap()
            .received_events,
        0
    );
    c.execute_batch("DROP TRIGGER fail_capture").unwrap();
    send(&mut s, &a, &e).unwrap();
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1);
}
#[test]
fn concurrent_delivery_commits_one_source_and_captured_content_is_encrypted() {
    let (_d, p, k, mut s) = setup();
    let (g, a, session) = connected(&mut s);
    let e = event(&session, "e1", 1);
    let mut other = IntakeStore::open(&p, k, scope()).unwrap();
    let ac = a.clone();
    let ec = e.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let b = barrier.clone();
    let t = std::thread::spawn(move || {
        b.wait();
        send(&mut other, &ac, &ec).unwrap()
    });
    barrier.wait();
    let one = send(&mut s, &a, &e).unwrap();
    assert_eq!(one.receipt, t.join().unwrap().receipt);
    assert_eq!(
        s.capture_grant(&scope(), &g.grant.grant_id)
            .unwrap()
            .received_events,
        1
    );
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(c);
    let bytes = std::fs::read(&p).unwrap();
    assert!(
        !bytes
            .windows(b"SYNTHETIC-CAPTURE-SECRET".len())
            .any(|w| w == b"SYNTHETIC-CAPTURE-SECRET")
    );
    assert!(
        !bytes
            .windows(g.token.len())
            .any(|w| w == g.token.as_bytes())
    );
}
