use super::*;
use serde_json::json;
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:diagnostics".into(),
        space: "fixture".into(),
    }
}
fn setup() -> (tempfile::TempDir, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let mut s = IntakeStore::create(
        &d.path().join("db"),
        crate::VaultKey::generate().unwrap(),
        scope(),
    )
    .unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.enable_connections(&scope()).unwrap();
    s.enable_capture(&scope()).unwrap();
    (d, s)
}
fn receive(s: &mut IntakeStore, id: &str) -> IntakeReceipt {
    s.receive(&IntakeAuthority{scope:scope(),actor_id:"actor-secret".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"platform-secret","capture_fidelity":"f2_visible_transcript","content":"PRIVATE-SOURCE-SENTINEL","occurred_at":null})).unwrap()).unwrap()
}
fn configure(s: &mut IntakeStore) {
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "private-model-name".into(),
            endpoint: "http://127.0.0.1:9/private-path".into(),
        },
    )
    .unwrap();
}
#[test]
fn readonly_redacted_and_scope_bound() {
    let (_d, mut s) = setup();
    let r = receive(&mut s, "request-secret");
    let before = s.connection.total_changes();
    let d = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(d.layers["compiler_queue"].counts["waiting"], 1);
    assert_eq!(d.model_location, "unconfigured");
    assert!(d.layers["compiler_queue"].complete);
    assert_eq!(before, s.connection.total_changes());
    let out = serde_json::to_string(&d).unwrap();
    for secret in [
        "PRIVATE-SOURCE-SENTINEL",
        "actor-secret",
        "platform-secret",
        "request-secret",
        &r.source.object_id,
        &r.job_id,
        &scope().owner_id,
    ] {
        assert!(!out.contains(secret));
    }
    assert!(
        s.operational_diagnostics(&AssetScope {
            space: "other".into(),
            ..scope()
        })
        .is_err()
    );
}
#[test]
fn lease_failure_budget_and_ready_remain_distinct() {
    let (_d, mut s) = setup();
    configure(&mut s);
    let receipt = receive(&mut s, "one");
    let mut j = s.claim_next(&scope(), 60).unwrap().unwrap();
    assert_eq!(
        s.operational_diagnostics(&scope()).unwrap().layers["compiler_queue"].counts["processing_lease_observed"],
        1
    );
    j.lease.as_mut().unwrap().expires_at = "2000-01-01T00:00:00Z".into();
    update(&s.connection, &s.key, "job", &j.job.job_id, &j).unwrap();
    let before = s.connection.total_changes();
    assert_eq!(
        s.operational_diagnostics(&scope()).unwrap().layers["compiler_queue"].counts["expired_lease"],
        1
    );
    assert_eq!(before, s.connection.total_changes());
    j.lease.as_mut().unwrap().expires_at =
        format_time(OffsetDateTime::now_utc() + time::Duration::seconds(60)).unwrap();
    update(&s.connection, &s.key, "job", &j.job.job_id, &j).unwrap();
    s.fail_attempt(
        &scope(),
        &receipt.job_id,
        &j.lease.unwrap().lease_id,
        AttemptFailure::BudgetExceeded,
    )
    .unwrap();
    let d = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(d.layers["compiler_queue"].counts["failed"], 1);
    assert_eq!(d.layers["compiler_queue"].counts["budget_exceeded"], 1);
    assert!(d.layers["compiler_queue"].last_success_at.is_none());
    // Corruption of a monitored record is an error, never a healthy empty queue.
    s.connection
        .execute(
            "UPDATE intake_objects SET ciphertext=x'00' WHERE id=?1",
            [&receipt.job_id],
        )
        .unwrap();
    assert!(s.operational_diagnostics(&scope()).is_err());
}
#[test]
fn latest_window_limit_is_explicit_and_never_changes_jobs() {
    let (_d, mut s) = setup();
    for n in 0..130 {
        receive(&mut s, &format!("r{n}"));
    }
    let before = s.connection.total_changes();
    let d = s.operational_diagnostics(&scope()).unwrap();
    let q = &d.layers["compiler_queue"];
    assert!(!q.complete);
    assert_eq!(q.observed, 128);
    assert_eq!(q.counts["waiting"], 128);
    assert!(q.notices.iter().any(|n| n == "latest_window_only"));
    assert_eq!(before, s.connection.total_changes());
}
#[test]
fn byte_budget_stops_before_decrypting_oversized_records() {
    let (_d, mut s) = setup();
    let r = receive(&mut s, "r");
    s.connection
        .execute(
            "UPDATE intake_objects SET ciphertext=zeroblob(?1) WHERE id=?2",
            rusqlite::params![DOCTOR_ENCRYPTED_BYTES + 1, r.job_id],
        )
        .unwrap();
    let d = s.operational_diagnostics(&scope()).unwrap();
    assert!(!d.layers["compiler_queue"].complete);
    assert_eq!(d.layers["compiler_queue"].observed, 0);
    assert_eq!(d.encrypted_bytes_examined, 0);
}
#[test]
fn source_ack_loss_revocation_and_disclosure_are_separate() {
    let (_d, mut s) = setup();
    let g = s
        .create_capture_grant(
            &scope(),
            CaptureGrantRequest {
                request_id: "grant-secret".into(),
                source_namespace: "private-namespace".into(),
                host: "codex".into(),
                surface: "cli_hooks".into(),
                host_version: "fixture".into(),
                events: vec![CaptureEventKind::UserPrompt],
                lifetime_seconds: 3600,
                event_budget: 2,
                byte_budget: 4096,
            },
        )
        .unwrap();
    let access = s.authenticate_capture(&scope(), &g.token).unwrap();
    let session = s
        .register_capture_session(&scope(), &access, "private-session")
        .unwrap();
    let ack=s.capture_event(&scope(),&access,&serde_json::to_vec(&json!({"schema_version":CAPTURE_VERSION,"session_id":session.session_id,"event_id":"private-event","sequence":1,"kind":"user_prompt","content":"PRIVATE-SOURCE-SENTINEL","occurred_at":null,"loss":{"truncated":true,"redacted":false,"partial_history":true}})).unwrap()).unwrap();
    let c = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "connection-secret".into(),
                label: "PRIVATE-CONNECTION-LABEL".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap();
    let ca = s.authenticate_connection(&scope(), &c.token).unwrap();
    s.query_connected(
        &scope(),
        &ca,
        ConnectedQuery {
            request_id: "read-secret".into(),
            query: Some("missing".into()),
            memory_id: None,
            max_items: 1,
        },
    )
    .unwrap();
    let d = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(d.layers["capture_receipts"].counts["source_acked"], 1);
    assert_eq!(d.layers["capture_receipts"].counts["truncated"], 1);
    assert_eq!(d.layers["disclosure"].counts["empty_read"], 1);
    assert!(d.layers["disclosure"].last_success_at.is_some());
    assert!(d.layers["compiler_queue"].last_success_at.is_none());
    let out = serde_json::to_string(&d).unwrap();
    for secret in [
        &g.token,
        &c.token,
        &g.grant.grant_id,
        &ack.receipt.source.object_id,
        "PRIVATE-CONNECTION-LABEL",
        "PRIVATE-SOURCE-SENTINEL",
        "private-session",
    ] {
        assert!(!out.contains(secret));
    }
    s.revoke_capture(&scope(), &g.grant.grant_id).unwrap();
    s.revoke_connection(&scope(), &c.connection.id).unwrap();
    let d = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(d.layers["capture_grants"].counts["revoked"], 1);
    assert_eq!(d.layers["connections"].counts["revoked"], 1);
}

#[test]
fn successful_commit_has_timestamp_and_withdrawal_removes_dependent_observations() {
    let (_d, mut s) = setup();
    configure(&mut s);
    let r = receive(&mut s, "compile");
    let j = s.claim_next(&scope(), 60).unwrap().unwrap();
    let lease = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &lease.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &j.job.job_id, &lease.lease_id, 1)
        .unwrap();
    s.commit_compilation(&scope(),&j.job.job_id,&lease.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:fixture","predicate":"test:note","value":"fixture","polarity":"positive","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"PRIVATE-SOURCE-SENTINEL","occurrence":0}]}]})).unwrap()).unwrap();
    let report = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(report.layers["compiler_queue"].counts["ready"], 1);
    assert!(report.layers["compiler_queue"].last_success_at.is_some());
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    let report = s.operational_diagnostics(&scope()).unwrap();
    assert_eq!(report.layers["compiler_queue"].observed, 0);
    assert_eq!(report.layers["compiler_queue"].omitted_unreadable, 1);
    assert!(report.layers["compiler_queue"].last_success_at.is_none());
}
