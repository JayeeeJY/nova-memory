use nova_core::{compilation::*, intake::*, *};
use serde_json::{Value, json};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "test".into(),
    }
}
fn authority() -> IntakeAuthority {
    IntakeAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("n2b.sqlite");
    let key = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&path, key.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-not-an-llm".into(),
            endpoint: "http://127.0.0.1:11434/v1".into(),
        },
    )
    .unwrap();
    (d, path, key, s)
}
fn request(id: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC evidence","occurred_at":null})).unwrap()
}
fn output(value: &str, condition: Value, time: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:nova","predicate":"project:deployment","value":value,"polarity":"positive","modality":"asserted","conditions":condition},"occurred_at":null,"valid_time":time,"quotes":[{"quote":"SYNTHETIC evidence","occurrence":0}]}]})).unwrap()
}
fn prepared(s: &mut IntakeStore) -> CompilationAttempt {
    let job = s.claim_next(&scope(), 60).unwrap().unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 1000)
        .unwrap()
}
fn ingest(
    s: &mut IntakeStore,
    id: &str,
    value: &str,
    condition: Value,
    time: Value,
) -> (IntakeReceipt, CommittedCompilation) {
    let r = s.receive(&authority(), &request(id)).unwrap();
    let a = prepared(s);
    let c = s
        .commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            &output(value, condition, time),
        )
        .unwrap();
    (r, c)
}
fn pages(s: &IntakeStore) -> Vec<WikiPage> {
    s.wiki_pages(&scope(), None)
        .unwrap()
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}
#[test]
fn exact_duplicates_group_and_enrich_evidence_without_inventing_truth_or_overwriting_sources() {
    let (_d, p, k, mut s) = setup();
    let (a, _) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    s.consolidate_pending(&scope(), 4).unwrap();
    let first = pages(&s).remove(0);
    let (b, _) = ingest(&mut s, "b", "local", json!({}), json!({"kind":"unknown"}));
    assert!(pages(&s).is_empty());
    drop(s);
    let mut s = IntakeStore::open(&p, k, scope()).unwrap();
    assert_eq!(s.consolidation_queue(&scope()).unwrap().pending, 1);
    assert_eq!(s.consolidate_pending(&scope(), 4).unwrap(), 1);
    let page = pages(&s).remove(0);
    assert!(page.generation > first.generation);
    assert_eq!(page.memories.len(), 1);
    assert_eq!(page.memories[0].members.len(), 2);
    assert_eq!(page.memories[0].evidence.len(), 2);
    assert_eq!(page.inputs.len(), 4);
    assert!(page.semantic_support.contains("unverified"));
    assert!(s.source(&scope(), &a.source.object_id).is_ok());
    assert!(s.source(&scope(), &b.source.object_id).is_ok());
    assert_eq!(s.consolidate_pending(&scope(), 4).unwrap(), 0);
}
#[test]
fn conditions_explicit_temporal_change_and_candidate_conflict_are_distinct() {
    for (mode, expected) in [
        ("condition", "conditional_variant"),
        ("time", "temporal_transition"),
        ("unknown", "potential_conflict"),
    ] {
        let (_d, _p, _k, mut s) = setup();
        let a = if mode == "time" {
            json!({"kind":"interval","start":"2025-01-01T00:00:00Z","end":"2025-06-01T00:00:00Z"})
        } else {
            json!({"kind":"unknown"})
        };
        let b = if mode == "time" {
            json!({"kind":"interval","start":"2025-06-01T00:00:00Z","end":null})
        } else {
            json!({"kind":"unknown"})
        };
        ingest(&mut s, "a", "local", json!({}), a);
        ingest(
            &mut s,
            "b",
            "cloud",
            if mode == "condition" {
                json!({"scope:context":"work"})
            } else {
                json!({})
            },
            b,
        );
        s.consolidate_pending(&scope(), 4).unwrap();
        let page = pages(&s).remove(0);
        assert_eq!(page.memories.len(), 2);
        assert_eq!(page.associations[0].kind, expected);
        if mode == "time" {
            let from = page
                .memories
                .iter()
                .find(|m| m.id == page.associations[0].from)
                .unwrap();
            let to = page
                .memories
                .iter()
                .find(|m| m.id == page.associations[0].to)
                .unwrap();
            assert_eq!(from.content.value, "local");
            assert_eq!(to.content.value, "cloud");
        }
    }
}
#[test]
fn withdrawal_invalidates_immediately_rebuild_retains_independent_support_and_no_list_poisoning() {
    let (_d, p, k, mut s) = setup();
    let (a, c) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    ingest(&mut s, "b", "local", json!({}), json!({"kind":"unknown"}));
    s.consolidate_pending(&scope(), 4).unwrap();
    s.withdraw_source(&scope(), &a.source.object_id).unwrap();
    assert!(pages(&s).is_empty());
    assert!(s.source(&scope(), &a.source.object_id).is_err());
    assert!(s.compilation_commit(&scope(), &c.change.change_id).is_err());
    assert!(
        s.unit_history(&scope(), &c.output_versions[0].object_id)
            .is_err()
    );
    assert_eq!(s.compiled_units(&scope(), None).unwrap().len(), 1);
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1);
    drop(s);
    let mut s = IntakeStore::open(&p, k, scope()).unwrap();
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s)[0].memories[0].evidence.len(), 1);
}
#[test]
fn deletion_removes_live_envelopes_and_keys_refuses_replay_late_completion_and_survives_backup() {
    let (d, p, k, mut s) = setup();
    s.receive(&authority(), &request("a")).unwrap();
    let a = prepared(&mut s);
    let out = output("local", json!({}), json!({"kind":"unknown"}));
    let c = s
        .commit_compilation(&scope(), &a.job_id, &a.lease_id, &out)
        .unwrap();
    s.consolidate_pending(&scope(), 4).unwrap();
    let source = a.context.sources[0].source.object_id.clone();
    assert!(s.delete_source(&scope(), &source).unwrap() >= 6);
    assert!(s.receive(&authority(), &request("a")).is_err());
    assert!(
        s.commit_compilation(&scope(), &a.job_id, &a.lease_id, &out)
            .is_err()
    );
    assert!(
        s.compiled_unit(&scope(), &c.output_versions[0].object_id)
            .is_err()
    );
    assert!(pages(&s).is_empty());
    assert!(s.jobs(&scope(), None).unwrap().is_empty());
    let db = rusqlite::Connection::open(&p).unwrap();
    let n:i64=db.query_row("SELECT count(*) FROM intake_objects WHERE kind IN ('source','job','unit_revision','unit_head','compilation_attempt','compilation_commit','wiki','model_response_hash')",[],|r|r.get(0)).unwrap();
    // Response hash carries no content but is harmless retained metadata.
    assert!(n <= 1);
    let backup = d.path().join("after.sqlite");
    s.backup_to(&scope(), &backup).unwrap();
    drop(s);
    let restored = IntakeStore::open(&backup, k, scope()).unwrap();
    assert!(pages(&restored).is_empty());
    assert!(restored.source(&scope(), &source).is_err());
}
#[test]
fn correction_undo_invalidates_old_views_protects_meaning_and_rebuild_does_not_rewrite_units() {
    let (_d, _p, _k, mut s) = setup();
    let (_, c) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let target = c.output_versions[0].clone();
    s.consolidate_pending(&scope(), 4).unwrap();
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    s.request_correction(
        &owner,
        target.clone(),
        "correct".into(),
        "SYNTHETIC evidence".into(),
    )
    .unwrap();
    let a = prepared(&mut s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output("cloud", json!({}), json!({"kind":"unknown"})),
    )
    .unwrap();
    assert!(pages(&s).is_empty());
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s)[0].memories[0].protected_members.len(), 1);
    ingest(&mut s, "b", "local", json!({}), json!({"kind":"unknown"}));
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s)[0].memories.len(), 2);
    assert_eq!(
        s.compiled_unit(&scope(), &target.object_id)
            .unwrap()
            .unit
            .content
            .value,
        "cloud"
    );
    s.undo_correction(&owner, &target.object_id, 2).unwrap();
    assert!(pages(&s).is_empty());
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s)[0].memories.len(), 1);
    assert_eq!(pages(&s)[0].memories[0].members.len(), 2);
    assert_eq!(
        s.unit_history(&scope(), &target.object_id).unwrap().len(),
        3
    );
}
#[test]
fn fault_rolls_back_projection_and_queue_can_retry_without_duplicate_generation() {
    let (_d, p, _k, mut s) = setup();
    ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let db = rusqlite::Connection::open(p).unwrap();
    db.execute_batch("CREATE TRIGGER fixture BEFORE INSERT ON intake_objects WHEN NEW.kind='wiki' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(s.consolidate_pending(&scope(), 4).is_err());
    assert!(pages(&s).is_empty());
    assert_eq!(s.consolidation_queue(&scope()).unwrap().pending, 1);
    db.execute_batch("DROP TRIGGER fixture").unwrap();
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s)[0].generation, 1);
}
#[test]
fn budgets_partition_overflow_and_scope_fail_closed_without_claiming_partial_completion() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..65 {
        ingest(
            &mut s,
            &format!("r{i}"),
            "local",
            json!({}),
            json!({"kind":"unknown"}),
        );
    }
    assert!(s.consolidate_pending(&scope(), 5).is_err());
    assert_eq!(s.consolidate_pending(&scope(), 1).unwrap(), 0);
    assert_eq!(s.consolidation_queue(&scope()).unwrap().blocked, 1);
    assert!(pages(&s).is_empty());
    let mut foreign = scope();
    foreign.space = "other".into();
    assert!(s.wiki_pages(&foreign, None).is_err());
    assert!(s.delete_source(&foreign, "source_fake").is_err());
}
#[test]
fn withdrawn_source_cannot_be_undone_or_reintroduced_through_correction_and_can_be_deleted_later() {
    let (_d, _p, _k, mut s) = setup();
    let (r, c) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let target = c.output_versions[0].clone();
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    s.request_correction(
        &owner,
        target.clone(),
        "correct".into(),
        "SYNTHETIC evidence".into(),
    )
    .unwrap();
    let a = prepared(&mut s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output("cloud", json!({}), json!({"kind":"unknown"})),
    )
    .unwrap();
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    assert!(s.undo_correction(&owner, &target.object_id, 2).is_err());
    assert!(
        s.request_correction(
            &owner,
            RevisionRef {
                object_id: target.object_id,
                revision: 2
            },
            "resurrection".into(),
            "SYNTHETIC evidence".into()
        )
        .is_err()
    );
    assert_eq!(s.withdrawn_sources(&scope(), None).unwrap().len(), 1);
    s.delete_source(&scope(), &r.source.object_id).unwrap();
    assert!(s.withdrawn_sources(&scope(), None).unwrap().is_empty());
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1); // The separately authored correction source remains; retired unit does not.
}
#[test]
fn deletion_sql_failure_is_atomic_and_retryable() {
    let (_d, p, _k, mut s) = setup();
    let (r, _) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    s.consolidate_pending(&scope(), 4).unwrap();
    let db = rusqlite::Connection::open(p).unwrap();
    db.execute_batch("CREATE TRIGGER delete_fault BEFORE DELETE ON intake_objects WHEN OLD.kind='source' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(s.delete_source(&scope(), &r.source.object_id).is_err());
    assert!(s.source(&scope(), &r.source.object_id).is_ok());
    assert_eq!(pages(&s).len(), 1);
    db.execute_batch("DROP TRIGGER delete_fault").unwrap();
    s.delete_source(&scope(), &r.source.object_id).unwrap();
    assert!(s.source(&scope(), &r.source.object_id).is_err());
}
#[test]
fn equivalence_preserves_privacy_and_origin_and_rebuild_is_explicit_and_bounded() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let mut auth = authority();
    auth.privacy_level = PrivacyLevel::Shareable;
    s.receive(&auth, &request("b")).unwrap();
    let a = prepared(&mut s);
    let mut out: Value =
        serde_json::from_slice(&output("local", json!({}), json!({"kind":"unknown"}))).unwrap();
    out["units"][0]["origin"] = json!("inferred");
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &serde_json::to_vec(&out).unwrap(),
    )
    .unwrap();
    s.consolidate_pending(&scope(), 4).unwrap();
    let page = pages(&s).remove(0);
    assert_eq!(page.memories.len(), 2);
    s.rebuild_wiki_page(&scope(), &page.id).unwrap();
    assert!(pages(&s).is_empty());
    s.consolidate_pending(&scope(), 1).unwrap();
    assert!(pages(&s)[0].generation > page.generation);
    assert!(
        s.rebuild_wiki_page(&scope(), "untrusted-unknown-bucket")
            .is_err()
    );
}
#[test]
fn explicit_upgrade_backfills_dependencies_and_honors_pre_n2b_withdrawal_fence() {
    let (_d, p, _k, mut s) = setup();
    let (r, _) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    let db = rusqlite::Connection::open(p).unwrap();
    db.execute_batch("DROP TABLE intake_dependencies; DROP TABLE intake_revocations; DROP TABLE intake_retired_requests; DROP TABLE intake_related; DROP TABLE intake_dirty;").unwrap();
    s.enable_compilation(&scope()).unwrap();
    assert!(s.source(&scope(), &r.source.object_id).is_err());
    assert!(s.compiled_units(&scope(), None).unwrap().is_empty());
    assert_eq!(s.consolidate_pending(&scope(), 4).unwrap(), 1);
    assert!(pages(&s).is_empty());
}
#[test]
fn deletion_of_one_source_does_not_erase_another_withdrawn_original() {
    let (_d, p, _k, mut s) = setup();
    let (a, _) = ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let (b, _) = ingest(&mut s, "b", "cloud", json!({}), json!({"kind":"unknown"}));
    s.withdraw_source(&scope(), &a.source.object_id).unwrap();
    s.withdraw_source(&scope(), &b.source.object_id).unwrap();
    s.delete_source(&scope(), &b.source.object_id).unwrap();
    let withdrawn = s.withdrawn_sources(&scope(), None).unwrap();
    assert_eq!(withdrawn.len(), 1);
    assert_eq!(withdrawn[0].1, a.source.object_id);
    let db = rusqlite::Connection::open(p).unwrap();
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM intake_objects WHERE kind='source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
#[test]
fn user_and_assistant_evidence_with_matching_extracted_content_remain_distinct() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "a", "local", json!({}), json!({"kind":"unknown"}));
    let mut auth = authority();
    auth.role = SourceRole::Assistant;
    s.receive(&auth, &request("b")).unwrap();
    let a = prepared(&mut s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output("local", json!({}), json!({"kind":"unknown"})),
    )
    .unwrap();
    s.consolidate_pending(&scope(), 1).unwrap();
    let p = pages(&s).remove(0);
    assert_eq!(p.memories.len(), 2);
    assert_eq!(p.associations[0].kind, "parallel_evidence");
    assert!(p.memories.iter().any(|m| m.source_roles == ["assistant"]));
    assert!(p.memories.iter().any(|m| m.source_roles == ["user"]));
}
#[test]
fn revoked_rows_do_not_hide_older_valid_rows_after_page_filtering() {
    let (_d, _p, _k, mut s) = setup();
    ingest(
        &mut s,
        "kept",
        "local",
        json!({}),
        json!({"kind":"unknown"}),
    );
    for i in 0..52 {
        let (r, _) = ingest(
            &mut s,
            &format!("r{i}"),
            "local",
            json!({}),
            json!({"kind":"unknown"}),
        );
        s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    }
    assert_eq!(s.compiled_units(&scope(), None).unwrap().len(), 1);
    assert_eq!(s.jobs(&scope(), None).unwrap().len(), 1);
}
#[test]
fn opaque_semantic_json_cannot_forge_dependency_edges_or_invalidate_unrelated_heads() {
    let (_d, _p, _k, mut s) = setup();
    let (other, _) = ingest(
        &mut s,
        "other",
        "local",
        json!({}),
        json!({"kind":"unknown"}),
    );
    s.receive(&authority(), &request("kept")).unwrap();
    let a = prepared(&mut s);
    let mut out: Value =
        serde_json::from_slice(&output("data", json!({}), json!({"kind":"unknown"}))).unwrap();
    out["units"][0]["content"]["value"] = json!({"object_id":other.source.object_id,"revision":1});
    let c = s
        .commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            &serde_json::to_vec(&out).unwrap(),
        )
        .unwrap();
    s.withdraw_source(&scope(), &other.source.object_id)
        .unwrap();
    assert!(
        s.compiled_unit(&scope(), &c.output_versions[0].object_id)
            .is_ok()
    );
    assert_eq!(s.compiled_units(&scope(), None).unwrap().len(), 1);
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(pages(&s).len(), 1);
}
