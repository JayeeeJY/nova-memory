//! Isolated synthetic core tests: no model, network, charging, or existing Vault access.
use nova_core::{compilation::*, intake::*, *};
use serde_json::json;
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:meter-test".into(),
        space: "private".into(),
    }
}
fn setup() -> (tempfile::TempDir, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let mut store =
        IntakeStore::create(&d.path().join("intake.sqlite"), key.clone(), scope()).unwrap();
    store.enable_compilation(&scope()).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-meter-fixture".into(),
                endpoint: "http://127.0.0.1:19873/v1".into(),
            },
        )
        .unwrap();
    (d, key, store)
}
fn prepare(s: &mut IntakeStore, n: usize) -> CompilationAttempt {
    s.receive(&IntakeAuthority{scope:scope(),actor_id:"owner-ui".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},
        &serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":format!("fixture-{n}"),"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC_SECRET_SOURCE","occurred_at":null})).unwrap()).unwrap();
    claim(s)
}
fn claim(s: &mut IntakeStore) -> CompilationAttempt {
    let job = s.claim_next(&scope(), 60).unwrap().unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &job.lease.unwrap().lease_id)
        .unwrap()
}
fn dispatch(s: &mut IntakeStore, a: &CompilationAttempt) -> CompilationAttempt {
    s.authorize_metered_dispatch(
        &scope(),
        &a.job_id,
        &a.lease_id,
        1000,
        CompilationTransport::OpenaiCompatibleJson,
    )
    .unwrap()
}
fn observed() -> ProviderObservation {
    ProviderObservation {
        outcome: ProviderObservationOutcome::ResponseReceived,
        usage: Some(ProviderTokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: Some(3),
        }),
        elapsed_millis: Some(12),
        http_status: Some(200),
        curl_exit_code: Some(0),
    }
}
fn record(s: &mut IntakeStore, a: &CompilationAttempt) -> RecordedProviderObservation {
    s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, observed())
        .unwrap()
}
#[test]
fn dispatch_and_intent_are_atomic_and_restart_preserves_unknown() {
    let (d, key, mut s) = setup();
    let a = prepare(&mut s, 0);
    let db = rusqlite::Connection::open(d.path().join("intake.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER metering_fault BEFORE INSERT ON intake_objects WHEN NEW.kind='compilation_meter_intent' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(
        s.authorize_metered_dispatch(
            &scope(),
            &a.job_id,
            &a.lease_id,
            1,
            CompilationTransport::OpenaiCompatibleJson
        )
        .is_err()
    );
    assert!(
        s.compilation_usage(&scope(), None)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        s.prepare_compilation(&scope(), &a.job_id, &a.lease_id)
            .unwrap()
            .dispatched_at
            .is_none()
    );
    db.execute_batch("DROP TRIGGER metering_fault;").unwrap();
    s.authorize_metered_dispatch(
        &scope(),
        &a.job_id,
        &a.lease_id,
        1,
        CompilationTransport::OpenaiCompatibleJson,
    )
    .unwrap();
    assert!(
        s.authorize_metered_dispatch(
            &scope(),
            &a.job_id,
            &a.lease_id,
            1,
            CompilationTransport::OpenaiCompatibleJson
        )
        .is_err()
    );
    drop(s);
    let s = IntakeStore::open_read_only(&d.path().join("intake.sqlite"), key, scope()).unwrap();
    let page = s.compilation_usage(&scope(), None).unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].observed.is_none());
    assert!(page.monetary_cost.is_none());
    assert!(page.reporting_only);
    let bytes = serde_json::to_string(&page).unwrap();
    for forbidden in ["SYNTHETIC_SECRET_SOURCE", "127.0.0.1", &a.lease_id] {
        assert!(!bytes.contains(forbidden));
    }
}
#[test]
fn observation_is_idempotent_bound_and_immutable_after_restart() {
    let (d, key, mut s) = setup();
    let a = prepare(&mut s, 0);
    assert!(
        s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, observed())
            .is_err()
    );
    let a = dispatch(&mut s, &a);
    assert!(
        s.record_compilation_observation(&scope(), &a.job_id, a.attempt, "wrong-lease", observed())
            .is_err()
    );
    let other = AssetScope {
        owner_id: "owner:other".into(),
        space: "private".into(),
    };
    assert!(s.compilation_usage(&other, None).is_err());
    assert!(
        s.record_compilation_observation(&other, &a.job_id, a.attempt, &a.lease_id, observed())
            .is_err()
    );
    let first = record(&mut s, &a);
    drop(s);
    let mut s = IntakeStore::open(&d.path().join("intake.sqlite"), key, scope()).unwrap();
    assert_eq!(record(&mut s, &a), first);
    let mut changed = observed();
    changed.elapsed_millis = Some(13);
    assert!(
        s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, changed)
            .is_err()
    );
    assert_eq!(
        s.compilation_usage(&scope(), None).unwrap().items[0].observed,
        Some(first)
    );
}
#[test]
fn retry_keeps_each_attempt_and_late_observation_does_not_commit_or_reauthorize() {
    let (_, _, mut s) = setup();
    let a = prepare(&mut s, 0);
    let a = dispatch(&mut s, &a);
    s.fail_attempt(
        &scope(),
        &a.job_id,
        &a.lease_id,
        AttemptFailure::WorkerInterrupted,
    )
    .unwrap();
    s.retry(&scope(), &a.job_id).unwrap();
    let b = claim(&mut s);
    let b = dispatch(&mut s, &b);
    record(&mut s, &a);
    assert!(s.compiled_units(&scope(), None).unwrap().is_empty());
    let p = s.compilation_usage(&scope(), None).unwrap();
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.items[0].intent.attempt, 2);
    assert!(p.items[0].observed.is_none());
    assert!(p.items[1].observed.is_some());
    assert!(
        s.record_compilation_observation(&scope(), &b.job_id, b.attempt, &a.lease_id, observed())
            .is_err()
    );
    record(&mut s, &b);
    assert_eq!(
        s.job(&scope(), &a.job_id).unwrap().job.state,
        JobState::Processing
    );
}
#[test]
fn source_deletion_and_configuration_change_keep_content_free_cost_facts() {
    let (_, _, mut s) = setup();
    let a = prepare(&mut s, 0);
    let a = dispatch(&mut s, &a);
    s.configure_model(2, ModelSelection::Unconfigured).unwrap();
    let sid = &a.context.sources[0].source.object_id;
    s.delete_source(&scope(), sid).unwrap();
    record(&mut s, &a);
    assert!(s.source(&scope(), sid).is_err());
    assert!(
        s.commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            br#"{"schema_version":"nova.extraction.v0.1","units":[]}"#
        )
        .is_err()
    );
    let p = s.compilation_usage(&scope(), None).unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].intent.configuration_revision, 2);
    assert_eq!(
        p.items[0]
            .observed
            .as_ref()
            .unwrap()
            .observation
            .usage
            .as_ref()
            .unwrap()
            .total_tokens,
        15
    );
}
#[test]
fn invalid_usage_cannot_be_persisted_or_replaced_by_zero() {
    let (_, _, mut s) = setup();
    let a = prepare(&mut s, 0);
    let a = dispatch(&mut s, &a);
    for usage in [
        ProviderTokenUsage {
            input_tokens: u64::MAX,
            output_tokens: 1,
            total_tokens: 0,
            cached_input_tokens: None,
        },
        ProviderTokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 16,
            cached_input_tokens: None,
        },
        ProviderTokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cached_input_tokens: Some(11),
        },
    ] {
        let mut o = observed();
        o.usage = Some(usage);
        assert!(
            s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, o)
                .is_err()
        );
    }
    let mut o = observed();
    o.usage = None;
    o.outcome = ProviderObservationOutcome::TransportFailed;
    o.http_status = None;
    o.curl_exit_code = Some(28);
    s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, o)
        .unwrap();
    assert!(
        s.compilation_usage(&scope(), None).unwrap().items[0]
            .observed
            .as_ref()
            .unwrap()
            .observation
            .usage
            .is_none()
    );
}
#[test]
fn page_is_bounded_and_backup_copy_cannot_receive_new_observations() {
    let (d, key, mut s) = setup();
    let mut last = None;
    for n in 0..51 {
        let a = prepare(&mut s, n);
        let a = dispatch(&mut s, &a);
        s.fail_attempt(
            &scope(),
            &a.job_id,
            &a.lease_id,
            AttemptFailure::WorkerInterrupted,
        )
        .unwrap();
        last = Some(a);
    }
    let p = s.compilation_usage(&scope(), None).unwrap();
    assert_eq!(p.items.len(), 50);
    let tail = s.compilation_usage(&scope(), p.next_before).unwrap();
    assert_eq!(tail.items.len(), 1);
    assert!(tail.next_before.is_none());
    assert!(s.compilation_usage(&scope(), Some(0)).is_err());
    let copy = d.path().join("copy.sqlite");
    s.backup_to(&scope(), &copy).unwrap();
    let mut copy = IntakeStore::open(&copy, key, scope()).unwrap();
    let a = last.unwrap();
    assert!(
        copy.record_compilation_observation(
            &scope(),
            &a.job_id,
            a.attempt,
            &a.lease_id,
            observed()
        )
        .is_err()
    );
    assert_eq!(
        copy.compilation_usage(&scope(), None).unwrap().items.len(),
        50
    );
}
#[test]
fn observation_write_failure_keeps_intent_retryable_without_mutating_job() {
    let (d, _, mut s) = setup();
    let a = prepare(&mut s, 0);
    let a = dispatch(&mut s, &a);
    let db = rusqlite::Connection::open(d.path().join("intake.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER observation_fault BEFORE INSERT ON intake_objects WHEN NEW.kind='compilation_meter_observation' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(
        s.record_compilation_observation(&scope(), &a.job_id, a.attempt, &a.lease_id, observed())
            .is_err()
    );
    assert!(
        s.compilation_usage(&scope(), None).unwrap().items[0]
            .observed
            .is_none()
    );
    db.execute_batch("DROP TRIGGER observation_fault;").unwrap();
    record(&mut s, &a);
    assert_eq!(
        s.job(&scope(), &a.job_id).unwrap().job.state,
        JobState::Processing
    );
}
