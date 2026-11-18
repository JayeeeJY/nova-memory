use nova_core::{compilation::*, intake::*, *};
use serde_json::{Value, json};

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:report-fixture".into(),
        space: "test".into(),
    }
}
fn authority(role: SourceRole, privacy_level: PrivacyLevel) -> IntakeAuthority {
    IntakeAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        capture_allowed: true,
        role,
        privacy_level,
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("report.sqlite");
    let key = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&path, key.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-not-a-real-model".into(),
            endpoint: "http://127.0.0.1:11434/v1".into(),
        },
    )
    .unwrap();
    (d, path, key, s)
}
fn receive(
    s: &mut IntakeStore,
    id: &str,
    role: SourceRole,
    privacy: PrivacyLevel,
) -> IntakeReceipt {
    s.receive(&authority(role,privacy),&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"report-private-label","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC report evidence. Never reveal this body.","occurred_at":null})).unwrap()).unwrap()
}
fn prepared(s: &mut IntakeStore) -> CompilationAttempt {
    let job = s.claim_next(&scope(), 60).unwrap().unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 10_000)
        .unwrap()
}
fn unit(topic: &str, value: &str) -> Value {
    json!({"origin":"extracted","content":{"subject":topic,"predicate":"project:deployment","value":value,"polarity":"positive","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC report evidence","occurrence":0}]})
}
fn commit(s: &mut IntakeStore, units: Vec<Value>) -> CommittedCompilation {
    let a = prepared(s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":units})).unwrap(),
    )
    .unwrap()
}
fn ingest(
    s: &mut IntakeStore,
    id: &str,
    topic: &str,
    value: &str,
) -> (IntakeReceipt, CommittedCompilation) {
    let r = receive(s, id, SourceRole::User, PrivacyLevel::Private);
    let c = commit(s, vec![unit(topic, value)]);
    (r, c)
}
fn report(s: &IntakeStore) -> ConstructionReport {
    s.construction_report(&scope()).unwrap()
}
fn consolidate(s: &mut IntakeStore) {
    while s.consolidation_queue(&scope()).unwrap().pending > 0 {
        s.consolidate_pending(&scope(), 4).unwrap();
    }
}

#[test]
fn empty_library_has_actual_zero_inventory_but_quality_cost_coverage_and_levels_are_unknown() {
    let (_d, _p, _k, s) = setup();
    let r = report(&s);
    assert_eq!(r.construction.sources, 0);
    assert_eq!(r.construction.exact_memories, 0);
    assert!(r.completeness.complete);
    assert_eq!(r.completeness.count_scope, "eligible_current_library");
    let v = serde_json::to_value(&r).unwrap();
    for field in [
        "goal_coverage_denominator",
        "semantic_correctness",
        "actual_tokens",
        "actual_cost",
        "agent_task_benefit",
        "capability_level",
    ] {
        assert!(v["unmeasured"][field].is_null());
    }
    assert!(!r.content_snapshot_id.is_empty());
    assert_eq!(r.content_snapshot_id, report(&s).content_snapshot_id);
}
#[test]
fn pending_and_zero_output_compilation_are_not_misrepresented_as_memory_or_quality() {
    let (_d, _p, _k, mut s) = setup();
    receive(&mut s, "pending", SourceRole::User, PrivacyLevel::Private);
    let before = report(&s);
    assert_eq!(before.construction.sources, 1);
    assert_eq!(before.construction.uncompiled_sources, 1);
    assert_eq!(before.construction.sources_without_current_records, 1);
    commit(&mut s, vec![]);
    let after = report(&s);
    assert_eq!(after.construction.uncompiled_sources, 0);
    assert_eq!(after.construction.sources_without_current_records, 1);
    assert_eq!(after.construction.exact_memories, 0);
    assert_eq!(after.metadata.job_states["ready"], 1);
    assert!(after.unmeasured.semantic_correctness.is_none());
}
#[test]
fn exact_duplicates_count_one_memory_without_claiming_independent_evidence() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "one", "project:nova", "local");
    ingest(&mut s, "two", "project:nova", "local");
    consolidate(&mut s);
    let r = report(&s);
    assert_eq!(
        (
            r.construction.sources,
            r.construction.extraction_records,
            r.construction.exact_memories,
            r.construction.source_links
        ),
        (2, 2, 1, 2)
    );
    assert_eq!(r.construction.wiki_fresh, 1);
    assert_eq!(r.metadata.semantic_origins["extracted"], 2);
    assert!(r.unmeasured.semantic_correctness.is_none());
    assert!(r.notices.iter().any(|s| s.contains("not_independent")));
}
#[test]
fn source_roles_and_privacy_filter_precede_exact_grouping_and_inventory() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "user", "project:nova", "local");
    receive(
        &mut s,
        "assistant",
        SourceRole::Assistant,
        PrivacyLevel::Private,
    );
    commit(&mut s, vec![unit("project:nova", "local")]);
    // Sensitive/sealed dispatch is already prohibited. Pending private sources must not enter
    // construction counts merely because no compilation has run yet.
    receive(
        &mut s,
        "sensitive",
        SourceRole::User,
        PrivacyLevel::Sensitive,
    );
    receive(&mut s, "sealed", SourceRole::User, PrivacyLevel::Sealed);
    consolidate(&mut s);
    let r = report(&s);
    assert_eq!(
        (
            r.construction.sources,
            r.construction.extraction_records,
            r.construction.exact_memories,
            r.construction.topics
        ),
        (2, 2, 2, 1)
    );
    assert_eq!(r.metadata.source_roles["user"], 1);
    assert_eq!(r.metadata.source_roles["assistant"], 1);
    assert_eq!(r.metadata.capture_fidelity["f2_visible_transcript"], 2);
    assert_eq!(r.metadata.privacy_levels.len(), 1);
}
#[test]
fn conditions_time_unknown_and_rule_relationships_are_separate_factual_counts() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "old", "project:nova", "local");
    receive(&mut s, "new", SourceRole::User, PrivacyLevel::Private);
    let mut u = unit("project:nova", "cloud");
    u["content"]["conditions"] = json!({"scope:context":"work"});
    u["valid_time"] = json!({"kind":"interval","start":"2026-01-01T00:00:00Z","end":null});
    u["occurred_at"] = json!("2026-01-01T00:00:00Z");
    commit(&mut s, vec![u]);
    let pending = report(&s);
    assert_eq!(pending.construction.wiki_pending, 1);
    assert!(pending.construction.rule_associations.is_empty());
    consolidate(&mut s);
    let r = report(&s);
    assert_eq!(r.construction.rule_associations["conditional_variant"], 1);
    assert_eq!(r.metadata.conditioned_records, 1);
    assert_eq!(
        (
            r.metadata.known_valid_time_records,
            r.metadata.unknown_valid_time_records
        ),
        (1, 1)
    );
    assert_eq!(
        (
            r.metadata.known_occurrence_records,
            r.metadata.unknown_occurrence_records
        ),
        (1, 1)
    );
    assert!(r.unmeasured.capability_level.is_none());
    assert_ne!(pending.content_snapshot_id, r.content_snapshot_id);
}
#[test]
fn correcting_undoing_withdrawing_deleting_and_reopening_fence_current_counts_and_snapshots() {
    let (_d, p, k, mut s) = setup();
    let (source, c) = ingest(&mut s, "base", "project:nova", "old");
    consolidate(&mut s);
    let first = report(&s);
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    let target = c.output_versions[0].clone();
    s.request_correction(
        &owner,
        target.clone(),
        "correct".into(),
        "SYNTHETIC report evidence".into(),
    )
    .unwrap();
    commit(&mut s, vec![unit("project:nova", "new")]);
    let corrected = report(&s);
    assert_eq!(corrected.construction.current_revisions, 1);
    assert_eq!(corrected.construction.historical_revisions, 1);
    assert_eq!(corrected.metadata.protected_records, 1);
    assert_ne!(first.content_snapshot_id, corrected.content_snapshot_id);
    s.undo_correction(&owner, &target.object_id, 2).unwrap();
    let undone = report(&s);
    assert_eq!(undone.construction.historical_revisions, 2);
    assert_ne!(corrected.content_snapshot_id, undone.content_snapshot_id);
    s.withdraw_source(&scope(), &source.source.object_id)
        .unwrap();
    let withdrawn = report(&s);
    assert_eq!(withdrawn.construction.exact_memories, 0);
    assert_eq!(withdrawn.construction.sources, 1);
    assert_eq!(withdrawn.construction.sources_without_current_records, 1);
    assert_ne!(undone.content_snapshot_id, withdrawn.content_snapshot_id);
    s.delete_source(&scope(), &source.source.object_id).unwrap();
    let deleted = report(&s);
    assert_eq!(deleted.construction.exact_memories, 0);
    assert_eq!(deleted.construction.historical_revisions, 0);
    drop(s);
    let s = IntakeStore::open(&p, k, scope()).unwrap();
    assert_eq!(deleted.content_snapshot_id, report(&s).content_snapshot_id);
}
#[test]
fn two_hundred_actual_records_aggregate_across_all_library_pages() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..200 {
        ingest(&mut s, &format!("r{i}"), &format!("project:t{i}"), "local");
    }
    consolidate(&mut s);
    assert_eq!(s.compiled_units(&scope(), None).unwrap().len(), 50);
    let r = report(&s);
    assert_eq!(
        (
            r.construction.sources,
            r.construction.extraction_records,
            r.construction.exact_memories,
            r.construction.topics,
            r.construction.wiki_fresh
        ),
        (200, 200, 200, 200, 200)
    );
    assert_eq!(r.construction.source_links, 200);
    assert!(r.completeness.complete);
}
#[test]
fn blocked_wiki_is_reported_without_losing_actual_record_inventory() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..65 {
        ingest(&mut s, &format!("r{i}"), "project:same", "local");
    }
    s.consolidate_pending(&scope(), 4).unwrap();
    let r = report(&s);
    assert_eq!(
        (
            r.construction.extraction_records,
            r.construction.exact_memories,
            r.construction.wiki_blocked,
            r.construction.wiki_fresh
        ),
        (65, 1, 1, 0)
    );
    assert!(r.construction.rule_associations.is_empty());
}
#[test]
fn scope_and_object_byte_and_metadata_caps_refuse_rather_than_return_partial_counts() {
    let (_d, p, _k, s) = setup();
    let mut other = scope();
    other.space = "other".into();
    assert!(s.construction_report(&other).is_err());
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<2049) INSERT INTO intake_objects SELECT 'source_fake_'||x,'source',x'',x'',x'',x'' FROM n",[]).unwrap();
    assert!(
        s.construction_report(&scope())
            .unwrap_err()
            .to_string()
            .contains("report_object_budget")
    );
    c.execute("DELETE FROM intake_objects WHERE kind='source'", [])
        .unwrap();
    c.execute(
        "INSERT INTO intake_objects VALUES ('source_fake','source',x'',x'',x'',zeroblob(33554433))",
        [],
    )
    .unwrap();
    assert!(
        s.construction_report(&scope())
            .unwrap_err()
            .to_string()
            .contains("report_byte_budget")
    );
    c.execute("DELETE FROM intake_objects WHERE kind='source'", [])
        .unwrap();
    c.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<32769) INSERT INTO intake_objects SELECT 'n2a:connection_usage:fake_'||x,'connection_usage',x'',x'',x'',x'' FROM n",[]).unwrap();
    assert!(
        s.construction_report(&scope())
            .unwrap_err()
            .to_string()
            .contains("report_metadata_budget")
    );
}
#[test]
fn report_contains_no_source_body_semantic_labels_quotes_or_connection_credentials() {
    let (_d, _p, _k, mut s) = setup();
    ingest(
        &mut s,
        "one",
        "project:private-topic-label",
        "private-value-label",
    );
    s.enable_connections(&scope()).unwrap();
    let issued = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "private-client".into(),
                label: "private-client-label".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    let text = serde_json::to_string(&report(&s)).unwrap();
    for forbidden in [
        "Never reveal this body",
        "report-private-label",
        "private-topic-label",
        "private-value-label",
        "private-client-label",
        &issued.token,
        "quote",
    ] {
        assert!(
            !text.contains(forbidden),
            "unexpected report field: {forbidden}"
        );
    }
}
#[test]
fn connection_use_is_server_observation_not_real_host_proof_and_does_not_change_content_binding() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "one", "project:nova", "local");
    s.enable_connections(&scope()).unwrap();
    let first = report(&s);
    let issued = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "client".into(),
                label: "configured-not-attested".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    let access = s.authenticate_connection(&scope(), &issued.token).unwrap();
    let query = ConnectedQuery {
        request_id: "query".into(),
        query: Some("local".into()),
        memory_id: None,
        max_items: 1,
    };
    s.query_connected(&scope(), &access, query.clone()).unwrap();
    s.query_connected(&scope(), &access, query).unwrap();
    let used = report(&s);
    assert_eq!(
        (
            used.usage.configured_connections,
            used.usage.active_connections,
            used.usage.logical_calls,
            used.usage.delivered_items
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(used.usage.operations["read"], 1);
    assert!(used.usage.evidence.contains("not_host_acceptance"));
    assert_eq!(first.content_snapshot_id, used.content_snapshot_id);
    s.revoke_connection(&scope(), &issued.connection.id)
        .unwrap();
    let revoked = report(&s);
    assert_eq!(revoked.usage.active_connections, 0);
    assert_eq!(revoked.usage.revoked_connections, 1);
    assert_eq!(used.content_snapshot_id, revoked.content_snapshot_id);
}
#[test]
fn backups_keep_content_binding_and_observations_but_never_restore_active_client_authority() {
    let (d, _p, k, mut s) = setup();
    ingest(&mut s, "one", "project:nova", "local");
    consolidate(&mut s);
    s.enable_connections(&scope()).unwrap();
    let issued = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "client".into(),
                label: "test".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    let before = report(&s);
    let path = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &path).unwrap();
    let restored = IntakeStore::open(&path, k, scope()).unwrap();
    let r = report(&restored);
    assert_eq!(before.content_snapshot_id, r.content_snapshot_id);
    assert_eq!(r.usage.configured_connections, 1);
    assert_eq!(r.usage.active_connections, 0);
    assert_eq!(r.usage.revoked_connections, 1);
    assert!(
        restored
            .authenticate_connection(&scope(), &issued.token)
            .is_err()
    );
    assert!(s.authenticate_connection(&scope(), &issued.token).is_ok());
}

#[test]
fn deleting_uncompiled_content_cannot_resurrect_an_earlier_empty_snapshot() {
    let (d, path, key, mut s) = setup();
    let initial = report(&s);
    let source = receive(
        &mut s,
        "aba-pending",
        SourceRole::User,
        PrivacyLevel::Private,
    );
    let pending = report(&s);
    assert_ne!(initial.content_snapshot_id, pending.content_snapshot_id);
    s.delete_source(&scope(), &source.source.object_id).unwrap();
    let deleted = report(&s);
    assert_eq!(deleted.construction.sources, 0);
    assert_eq!(deleted.construction.exact_memories, 0);
    assert_ne!(initial.content_snapshot_id, deleted.content_snapshot_id);
    assert_ne!(pending.content_snapshot_id, deleted.content_snapshot_id);
    let encoded = serde_json::to_string(&deleted).unwrap();
    assert!(!encoded.contains(&source.source.object_id));
    assert!(!encoded.contains("Never reveal this body"));
    drop(s);
    let s = IntakeStore::open(&path, key.clone(), scope()).unwrap();
    assert_eq!(deleted.content_snapshot_id, report(&s).content_snapshot_id);
    let backup = d.path().join("aba-backup.sqlite");
    s.backup_to(&scope(), &backup).unwrap();
    let restored = IntakeStore::open(&backup, key, scope()).unwrap();
    assert_eq!(
        deleted.content_snapshot_id,
        report(&restored).content_snapshot_id
    );
}

#[test]
fn evaluation_history_cannot_change_content_binding_or_revive_a_retired_snapshot() {
    let (_d, _p, _k, mut s) = setup();
    let initial = report(&s);
    let mut submission: TaskEvaluationSubmission =
        serde_json::from_str(include_str!("../../../eval/n5/fixture-submission.json")).unwrap();
    submission.manifest.asset_snapshot_id = initial.content_snapshot_id.clone();
    let configuration_hash = n5_configuration_hash(&submission.manifest).unwrap();
    for prediction in &mut submission.predictions {
        prediction.configuration_hash = configuration_hash.clone();
    }
    s.record_task_evaluation(&scope(), &submission).unwrap();
    assert_eq!(initial.content_snapshot_id, report(&s).content_snapshot_id);
    let source = receive(
        &mut s,
        "aba-after-evaluation",
        SourceRole::User,
        PrivacyLevel::Private,
    );
    let current = report(&s).content_snapshot_id;
    assert!(s.task_evaluations(&scope(), &current).unwrap()[0].stale);
    s.delete_source(&scope(), &source.source.object_id).unwrap();
    let current = report(&s).content_snapshot_id;
    assert_ne!(initial.content_snapshot_id, current);
    assert!(s.task_evaluations(&scope(), &current).unwrap()[0].stale);
    assert!(s.record_task_evaluation(&scope(), &submission).is_err());
    s.enable_connections(&scope()).unwrap();
    let connection = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "aba-control-only".into(),
                label: "control".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap();
    assert_eq!(current, report(&s).content_snapshot_id);
    s.revoke_connection(&scope(), &connection.connection.id)
        .unwrap();
    assert_eq!(current, report(&s).content_snapshot_id);
    assert!(s.task_evaluations(&scope(), &current).unwrap()[0].stale);
}

#[test]
fn retained_lifecycle_fences_share_the_global_metadata_budget() {
    let (_d, path, _key, s) = setup();
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<32769) INSERT INTO intake_revocations SELECT 'synthetic-fence-'||x FROM n", []).unwrap();
    assert!(
        s.construction_report(&scope())
            .unwrap_err()
            .to_string()
            .contains("report_metadata_budget")
    );
}
