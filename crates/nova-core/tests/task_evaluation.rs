use nova_core::{compilation::*, intake::*, *};
use serde_json::json;
fn fixture() -> TaskEvaluationSubmission {
    serde_json::from_str(include_str!("../../../eval/n5/fixture-submission.json")).unwrap()
}
fn negative() -> TaskEvaluationSubmission {
    serde_json::from_str(include_str!("../../../eval/n5/negative-submission.json")).unwrap()
}
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "n5-test".into(),
    }
}
fn setup() -> (tempfile::TempDir, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let mut s = IntakeStore::create(
        &d.path().join("n5.sqlite"),
        VaultKey::generate().unwrap(),
        scope(),
    )
    .unwrap();
    s.enable_compilation(&scope()).unwrap();
    (d, s)
}
fn bind(s: &IntakeStore, submission: &mut TaskEvaluationSubmission) {
    submission.manifest.asset_snapshot_id =
        s.construction_report(&scope()).unwrap().content_snapshot_id;
    let hash = n5_configuration_hash(&submission.manifest).unwrap();
    for p in &mut submission.predictions {
        p.configuration_hash = hash.clone();
    }
}
fn receive(s: &mut IntakeStore, id: &str) -> IntakeReceipt {
    let authority = IntakeAuthority {
        scope: scope(),
        actor_id: "synthetic-owner".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    };
    s.receive(&authority,&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC extra source mutation","occurred_at":null})).unwrap()).unwrap()
}
#[test]
fn public_fixture_is_sixty_chain_disjoint_tasks_and_never_a_quality_grade() {
    let s = fixture();
    validate_n5(&s.dataset).unwrap();
    let r = score_n5(&s).unwrap();
    assert_eq!(r.task_count, 60);
    assert_eq!(r.metrics.len(), 4);
    assert_eq!(r.arm_runs.len(), 4);
    assert_eq!(r.case_ids.len(), 60);
    assert_eq!(r.asset_snapshot_id, s.manifest.asset_snapshot_id);
    assert_eq!(r.pairs.len(), 3);
    assert_eq!(r.structure_evolution_tasks, 6);
    assert_eq!(r.quality_status, "independent_quality_uncalibrated");
    assert_eq!(r.level_status, "uncalibrated");
    assert_eq!(r.evaluation_scope, "synthetic_fixed_tasks");
    assert_eq!(r.run_kind, N5RunKind::FixtureContract);
    assert!(r.safety_gate);
    assert!(r.metrics.iter().all(|m| m.categories.len() == 6));
    assert_eq!(r.metrics[0].success_count, 10);
    assert_eq!(r.metrics[1].success_count, 60);
    assert_eq!(r.pairs[0].improved, 50);
    assert_eq!(r.pairs[0].regressed, 0);
}
#[test]
fn unknown_observations_stay_null_and_maintenance_is_not_inference_cost() {
    let mut s = fixture();
    s.manifest.costs.ingest_usd = Some(2.0);
    s.manifest.costs.maintenance_usd = Some(3.0);
    let r = score_n5(&s).unwrap();
    let m = &r.metrics[1];
    assert_eq!(m.cost_usd.value, None);
    assert_eq!(m.cost_usd.observed, 0);
    assert_eq!(m.cost_usd.total, 60);
    assert_eq!(m.latency_ms.value, None);
    assert_eq!(r.costs.ingest_usd, Some(2.0));
    assert_eq!(r.costs.maintenance_usd, Some(3.0));
    s.predictions[60].observed.cost_usd = Some(1.25);
    s.predictions[60].observed.latency_ms = Some(20);
    s.predictions[61].observed.latency_ms = Some(40);
    let r = score_n5(&s).unwrap();
    assert_eq!(r.metrics[1].cost_usd.value, Some(1.25));
    assert_eq!(r.metrics[1].cost_usd.observed, 1);
    assert_eq!(r.metrics[1].latency_ms.value, Some(30.0));
}
#[test]
fn source_forgery_and_privacy_fail_separate_gate_despite_high_average() {
    let r = score_n5(&negative()).unwrap();
    assert!(!r.safety_gate);
    assert_eq!(r.source_forgery_count, 1);
    assert!(r.severe_boundary_count >= 10);
    assert!(r.metrics[1].success_count > r.metrics[0].success_count);
    assert!(
        r.metrics[3]
            .categories
            .iter()
            .find(|c| c.category == N5Category::Privacy)
            .unwrap()
            .privacy_violation_count
            == 10
    );
}
#[test]
fn duplicate_missing_and_cross_arm_configuration_drift_are_rejected() {
    let mut s = fixture();
    s.predictions.pop();
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.predictions[1] = s.predictions[0].clone();
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.predictions[61].configuration_hash = "f".repeat(64);
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.manifest.model_version = "latest".into();
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.manifest.case_ids.pop();
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.manifest.arm_runs[1].run_id = s.manifest.arm_runs[0].run_id.clone();
    assert!(score_n5(&s).is_err());
}
#[test]
fn chain_leakage_future_clocks_bounds_and_forged_metrics_reject() {
    let mut s = fixture();
    s.dataset.tasks[0].split = N5Split::Test;
    assert!(validate_n5(&s.dataset).is_err());
    let mut s = fixture();
    s.predictions[0].completed_at = "2999-01-01T00:00:00Z".into();
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.predictions[0].observed.input_tokens = Some(u64::MAX);
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.predictions[0].observed.cost_usd = Some(f64::NAN);
    assert!(score_n5(&s).is_err());
    let mut s = fixture();
    s.predictions[0].observed.cost_usd = Some(-1.0);
    assert!(score_n5(&s).is_err());
    let mut s = serde_json::to_value(fixture()).unwrap();
    s["independently_verified"] = json!(true);
    assert!(TaskEvaluationSubmission::parse(&serde_json::to_vec(&s).unwrap()).is_err());
    let mut s = serde_json::to_value(fixture()).unwrap();
    s["predictions"][0]["success"] = json!(true);
    assert!(TaskEvaluationSubmission::parse(&serde_json::to_vec(&s).unwrap()).is_err());
    assert!(TaskEvaluationSubmission::parse(&vec![b' '; MAX_TASK_EVALUATION_BYTES + 1]).is_err());
}
#[test]
fn encrypted_summary_import_is_idempotent_scope_bound_and_stale_after_mutation() {
    let (d, mut s) = setup();
    let mut submission = fixture();
    bind(&s, &mut submission);
    let first = s.record_task_evaluation(&scope(), &submission).unwrap();
    assert!(!first.stale);
    assert_eq!(
        s.record_task_evaluation(&scope(), &submission)
            .unwrap()
            .evaluation_id,
        first.evaluation_id
    );
    let mut changed = submission.clone();
    changed.predictions[60].answer = "changed".into();
    assert!(s.record_task_evaluation(&scope(), &changed).is_err());
    let mut wrong = scope();
    wrong.owner_id = "owner:other".into();
    assert!(s.record_task_evaluation(&wrong, &submission).is_err());
    receive(&mut s, "mutate");
    let current = s.construction_report(&scope()).unwrap().content_snapshot_id;
    assert!(s.task_evaluations(&scope(), &current).unwrap()[0].stale);
    assert!(s.record_task_evaluation(&scope(), &submission).is_err());
    let db = std::fs::read(d.path().join("n5.sqlite")).unwrap();
    assert!(
        !db.windows(b"Project Cedar".len())
            .any(|w| w == b"Project Cedar")
    );
    assert!(!db.windows(b"Region-1".len()).any(|w| w == b"Region-1"));
    let stored = serde_json::to_string(&first).unwrap();
    assert!(!stored.contains("answer_exact"));
    assert!(!stored.contains("predictions"));
}
#[test]
fn bounded_history_prunes_aggregates_without_reviving_old_requests() {
    let (_d, mut s) = setup();
    let mut submission = fixture();
    bind(&s, &mut submission);
    for i in 0..21 {
        submission.request_id = format!("history-{i}");
        s.record_task_evaluation(&scope(), &submission).unwrap();
    }
    let snapshot = s.construction_report(&scope()).unwrap().content_snapshot_id;
    assert_eq!(s.task_evaluations(&scope(), &snapshot).unwrap().len(), 20);
    submission.request_id = "history-0".into();
    assert!(s.record_task_evaluation(&scope(), &submission).is_err());
}
#[test]
fn reported_observations_never_self_attest_and_old_answers_are_explicit_errors() {
    let mut s = fixture();
    s.manifest.run_kind = N5RunKind::OwnerReportedObservations;
    let config = n5_configuration_hash(&s.manifest).unwrap();
    for p in &mut s.predictions {
        p.configuration_hash = config.clone();
    }
    let row = s
        .predictions
        .iter_mut()
        .find(|p| p.arm == N5Arm::Compiled && p.case_id == "updates-01")
        .unwrap();
    row.answer = "Team-Old-1".into();
    let report = score_n5(&s).unwrap();
    assert_eq!(report.run_kind, N5RunKind::OwnerReportedObservations);
    assert_eq!(report.quality_status, "independent_quality_uncalibrated");
    assert_eq!(report.level_status, "uncalibrated");
    assert_eq!(report.metrics[2].outdated_count, 1);
    assert_eq!(report.metrics[2].error_count, 1);
}
#[test]
fn withdrawal_and_deletion_leave_only_stale_noncontent_summary() {
    let (_d, mut s) = setup();
    let source = receive(&mut s, "erase-source");
    let mut submission = fixture();
    bind(&s, &mut submission);
    s.record_task_evaluation(&scope(), &submission).unwrap();
    s.withdraw_source(&scope(), &source.source.object_id)
        .unwrap();
    let current = s.construction_report(&scope()).unwrap().content_snapshot_id;
    let reports = s.task_evaluations(&scope(), &current).unwrap();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].stale);
    s.delete_source(&scope(), &source.source.object_id).unwrap();
    let current = s.construction_report(&scope()).unwrap().content_snapshot_id;
    assert!(s.task_evaluations(&scope(), &current).unwrap()[0].stale);
    let stored = serde_json::to_string(&s.task_evaluations(&scope(), &current).unwrap()).unwrap();
    assert!(!stored.contains(&source.source.object_id));
    assert!(!stored.contains("SYNTHETIC extra source mutation"));
}
#[test]
fn extra_authorized_but_unsupported_citation_fails_exact_source_set_without_restricting_disclosure()
{
    let mut s = fixture();
    let prediction = s
        .predictions
        .iter_mut()
        .find(|p| p.arm == N5Arm::RawRetrieval && p.case_id == "facts-01")
        .unwrap();
    // This source is authorized, but states the owner; it does not support the requested region.
    prediction.evidence_ids.push("source-facts-01-b".into());
    prediction
        .disclosed_source_ids
        .push("source-facts-01-b".into());
    let report = score_n5(&s).unwrap();
    assert_eq!(report.metrics[1].success_count, 59);
    assert_eq!(report.metrics[1].source_correct_count, 59);
    assert_eq!(report.metrics[1].error_count, 1);
    assert_eq!(report.source_forgery_count, 0);
    assert_eq!(report.severe_boundary_count, 0);
    assert!(report.safety_gate);
    let prediction = s
        .predictions
        .iter_mut()
        .find(|p| p.arm == N5Arm::RawRetrieval && p.case_id == "facts-01")
        .unwrap();
    prediction.evidence_ids.pop();
    let report = score_n5(&s).unwrap();
    assert_eq!(report.metrics[1].success_count, 60);
    assert_eq!(report.metrics[1].source_correct_count, 60);
    assert!(report.safety_gate);
    let mut changed = fixture();
    changed.manifest.rules_version = "exact-reference-v1".into();
    let config = n5_configuration_hash(&changed.manifest).unwrap();
    for p in &mut changed.predictions {
        p.configuration_hash = config.clone();
    }
    assert!(score_n5(&changed).is_err());
}
