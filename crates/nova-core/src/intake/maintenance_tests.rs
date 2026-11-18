use super::*;
use serde_json::json;
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:maintenance-fixture".into(),
        space: "synthetic".into(),
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("intake.sqlite");
    let k = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&p, k.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-no-model".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )
    .unwrap();
    (d, p, k, s)
}
fn receive(s: &mut IntakeStore, id: &str) -> IntakeReceipt {
    s.receive(&IntakeAuthority{scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC maintenance source","occurred_at":null})).unwrap()).unwrap()
}
fn compile(s: &mut IntakeStore, id: &str, subject: &str) -> (IntakeReceipt, RevisionRef) {
    let receipt = receive(s, id);
    let j = s.claim_next(&scope(), 60).unwrap().unwrap();
    let l = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &l.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &j.job.job_id, &l.lease_id, 1000)
        .unwrap();
    let result=s.commit_compilation(&scope(),&j.job.job_id,&l.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":subject,"predicate":"test:constraint","value":"fixture","polarity":"negative","modality":"asserted","conditions":{"scope:context":"work"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC maintenance source","occurrence":0}]}]})).unwrap()).unwrap();
    (receipt, result.output_versions[0].clone())
}
fn policy() -> MaintenancePolicy {
    MaintenancePolicy::default()
}
fn old_page(s: &mut IntakeStore) -> String {
    s.consolidate_pending(&scope(), 4).unwrap();
    let (_, mut page) = s.wiki_pages(&scope(), None).unwrap().remove(0);
    page.updated_at = "2000-01-01T00:00:00Z".into();
    update(&s.connection, &s.key, "wiki", &page.id, &page).unwrap();
    page.id
}
#[test]
fn dry_run_is_read_only_and_execution_is_durable_idempotent() {
    let (_d, p, k, mut s) = setup();
    let (r, u) = compile(&mut s, "one", "project:one");
    let count = |s: &IntakeStore| {
        s.connection
            .query_row("SELECT count(*) FROM intake_objects", [], |r| {
                r.get::<_, u64>(0)
            })
            .unwrap()
    };
    let before = count(&s);
    let preview = s.preview_maintenance(&scope(), policy()).unwrap();
    assert_eq!(count(&s), before);
    assert_eq!(preview.rebuild.len(), 1);
    assert_eq!(preview.model_calls, 0);
    assert_eq!(preview.originals_deleted, 0);
    let job = s.schedule_maintenance(&scope(), "job", policy()).unwrap();
    assert_eq!(job.preview.rebuild[0].input_versions.len(), 2);
    assert!(!serde_json::to_string(&job).unwrap().contains("SYNTHETIC"));
    drop(s);
    let mut s = IntakeStore::open(&p, k, scope()).unwrap();
    let done = s.run_maintenance_when_idle(&scope(), &job.id).unwrap();
    assert_eq!(done.state, MaintenanceState::Completed);
    assert_eq!(done.pages_rebuilt, 1);
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &job.id)
            .unwrap()
            .updated_at,
        done.updated_at
    );
    assert_eq!(
        s.schedule_maintenance(&scope(), "job", policy())
            .unwrap()
            .state,
        MaintenanceState::Completed
    );
    assert_eq!(s.wiki_pages(&scope(), None).unwrap().len(), 1);
    assert_eq!(
        read_unit(&s.connection, &s.key, &scope(), &u.object_id)
            .unwrap()
            .unit
            .header
            .object,
        u
    );
    assert!(s.source(&scope(), &r.source.object_id).is_ok());
}
#[test]
fn changed_input_or_withdrawal_stales_frozen_plan() {
    for withdraw in [false, true] {
        let (_d, _p, _k, mut s) = setup();
        let (r, _) = compile(&mut s, "one", "project:one");
        let job = s.schedule_maintenance(&scope(), "job", policy()).unwrap();
        if withdraw {
            s.withdraw_source(&scope(), &r.source.object_id).unwrap();
        } else {
            compile(&mut s, "two", "project:one");
        }
        let out = s.run_maintenance_when_idle(&scope(), &job.id).unwrap();
        assert_eq!(out.state, MaintenanceState::Stale);
        assert_eq!(out.pages_rebuilt, 0);
        assert!(s.wiki_pages(&scope(), None).unwrap().is_empty());
        assert_eq!(
            s.maintenance_job(&scope(), &job.id).unwrap().state,
            MaintenanceState::Stale
        );
    }
}
#[test]
fn cancelled_or_foreign_plan_cannot_execute() {
    let (_d, _p, _k, mut s) = setup();
    compile(&mut s, "one", "project:one");
    let job = s.schedule_maintenance(&scope(), "job", policy()).unwrap();
    s.cancel_maintenance(&scope(), &job.id).unwrap();
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &job.id)
            .unwrap()
            .state,
        MaintenanceState::Cancelled
    );
    assert!(s.wiki_pages(&scope(), None).unwrap().is_empty());
    let foreign = AssetScope {
        owner_id: "owner:other".into(),
        space: "synthetic".into(),
    };
    assert!(s.run_maintenance_when_idle(&foreign, &job.id).is_err());
    assert!(s.preview_maintenance(&foreign, policy()).is_err());
    assert!(s.maintenance_jobs(&foreign, None).is_err());
}
#[test]
fn active_worker_defers_without_stealing_lease() {
    let (_d, _p, _k, mut s) = setup();
    compile(&mut s, "one", "project:one");
    let job = s.schedule_maintenance(&scope(), "job", policy()).unwrap();
    let r = receive(&mut s, "pending");
    let j = s.claim_next(&scope(), 60).unwrap().unwrap();
    let pending = s.run_maintenance_when_idle(&scope(), &job.id).unwrap();
    assert_eq!(pending.state, MaintenanceState::Pending);
    assert_eq!(pending.reason, "compiler_busy");
    assert_eq!(s.job(&scope(), &r.job_id).unwrap().lease, j.lease);
    assert!(s.wiki_pages(&scope(), None).unwrap().is_empty());
}
#[test]
fn cold_cache_eviction_preserves_sources_units_and_does_not_requeue() {
    let (_d, _p, _k, mut s) = setup();
    let (r, u) = compile(&mut s, "one", "project:one");
    s.enable_connections(&scope()).unwrap();
    let issued = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "client".into(),
                label: "Synthetic reader".into(),
                host: ConnectionHost::Other,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    let access = s.authenticate_connection(&scope(), &issued.token).unwrap();
    old_page(&mut s);
    let job = s.schedule_maintenance(&scope(), "prune", policy()).unwrap();
    assert_eq!(job.preview.evict.len(), 1);
    assert!(job.preview.rebuild.is_empty());
    let out = s.run_maintenance_when_idle(&scope(), &job.id).unwrap();
    assert_eq!(out.pages_pruned, 1);
    assert!(s.wiki_pages(&scope(), None).unwrap().is_empty());
    assert_eq!(s.consolidation_queue(&scope()).unwrap().pending, 0);
    assert!(s.source(&scope(), &r.source.object_id).is_ok());
    assert_eq!(
        read_unit(&s.connection, &s.key, &scope(), &u.object_id)
            .unwrap()
            .unit
            .header
            .object,
        u
    );
    assert_eq!(s.consolidation_queue(&scope()).unwrap().cold, 1);
    let report = s.construction_report(&scope()).unwrap();
    assert_eq!(report.construction.wiki_cold, 1);
    assert_eq!(report.construction.extraction_records, 1);
    assert_eq!(report.construction.sources, 1);
    let graph = s.owner_graph(&scope(), &GraphRequest::default()).unwrap();
    assert_eq!(graph.stats.memories, 1);
    assert!(
        graph
            .notices
            .iter()
            .any(|v| v == "wiki_cold_associations_omitted")
    );
    assert_eq!(
        s.query_connected(
            &scope(),
            &access,
            ConnectedQuery {
                request_id: "after-prune".into(),
                query: None,
                memory_id: Some(u.object_id.clone()),
                max_items: 1
            }
        )
        .unwrap()
        .items
        .len(),
        1
    );
    assert!(
        s.preview_maintenance(&scope(), policy())
            .unwrap()
            .rebuild
            .is_empty()
    );
    let new = s
        .schedule_maintenance(
            &scope(),
            "rebuild",
            MaintenancePolicy {
                include_cold: true,
                max_prune_pages: 0,
                ..policy()
            },
        )
        .unwrap();
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &new.id)
            .unwrap()
            .pages_rebuilt,
        1
    );
    assert_eq!(s.consolidation_queue(&scope()).unwrap().cold, 0);
}
#[test]
fn changed_cache_is_not_pruned_under_old_plan() {
    let (_d, _p, _k, mut s) = setup();
    compile(&mut s, "one", "project:one");
    let page = old_page(&mut s);
    let job = s.schedule_maintenance(&scope(), "prune", policy()).unwrap();
    s.rebuild_wiki_page(&scope(), &page).unwrap();
    s.consolidate_pending(&scope(), 4).unwrap();
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &job.id)
            .unwrap()
            .state,
        MaintenanceState::Stale
    );
    assert_eq!(s.wiki_pages(&scope(), None).unwrap().len(), 1);
}
#[test]
fn failed_commit_rolls_back_all_pages_and_receipt() {
    let (_d, _p, _k, mut s) = setup();
    compile(&mut s, "one", "project:one");
    old_page(&mut s);
    compile(&mut s, "two", "project:two");
    let job = s
        .schedule_maintenance(&scope(), "atomic", policy())
        .unwrap();
    assert_eq!(job.preview.rebuild.len(), 1);
    assert_eq!(job.preview.evict.len(), 1);
    s.connection.execute_batch("CREATE TRIGGER fixture_abort BEFORE DELETE ON intake_objects WHEN OLD.kind='wiki' BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    assert!(s.run_maintenance_when_idle(&scope(), &job.id).is_err());
    assert_eq!(
        s.maintenance_job(&scope(), &job.id).unwrap().state,
        MaintenanceState::Pending
    );
    assert_eq!(s.wiki_pages(&scope(), None).unwrap().len(), 1);
    assert_eq!(s.consolidation_queue(&scope()).unwrap().pending, 1);
    s.connection
        .execute_batch("DROP TRIGGER fixture_abort")
        .unwrap();
    let out = s.run_maintenance_when_idle(&scope(), &job.id).unwrap();
    assert_eq!((out.pages_rebuilt, out.pages_pruned), (1, 1));
}
#[test]
fn expired_plan_budget_and_request_reuse_fail_closed() {
    let (_d, _p, _k, mut s) = setup();
    compile(&mut s, "one", "project:one");
    let mut job = s.schedule_maintenance(&scope(), "job", policy()).unwrap();
    job.preview.expires_at = "2000-01-01T00:00:00Z".into();
    update(&s.connection, &s.key, "maintenance_job", &job.id, &job).unwrap();
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &job.id)
            .unwrap()
            .state,
        MaintenanceState::Stale
    );
    let mut other = policy();
    other.max_rebuild_pages = 1;
    assert!(s.schedule_maintenance(&scope(), "job", other).is_err());
    let mut invalid = policy();
    invalid.derived_ttl_seconds = 0;
    assert!(s.preview_maintenance(&scope(), invalid).is_err());
    let mut invalid = policy();
    invalid.max_rebuild_pages = 5;
    assert!(s.schedule_maintenance(&scope(), "bad", invalid).is_err());
}
#[test]
fn bounded_page_scan_exposes_cursor_and_does_not_loop_all_partitions() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..5 {
        compile(&mut s, &format!("source{i}"), &format!("project:p{i}"));
    }
    let first = s.schedule_maintenance(&scope(), "first", policy()).unwrap();
    assert_eq!(first.preview.rebuild.len(), 4);
    assert_eq!(
        s.run_maintenance_when_idle(&scope(), &first.id)
            .unwrap()
            .pages_rebuilt,
        4
    );
    assert_eq!(s.consolidation_queue(&scope()).unwrap().pending, 1);
    let p = MaintenancePolicy {
        max_rebuild_pages: 0,
        max_prune_pages: 1,
        ..policy()
    };
    let dry = s.preview_maintenance(&scope(), p.clone()).unwrap();
    assert!(dry.next_prune_before.is_some());
    let next = s
        .preview_maintenance(
            &scope(),
            MaintenancePolicy {
                prune_before: dry.next_prune_before,
                ..p
            },
        )
        .unwrap();
    assert_ne!(dry.next_prune_before, next.next_prune_before);
}
