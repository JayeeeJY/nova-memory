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
fn connected_setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let (d, p, k, mut s) = setup();
    s.enable_connections(&scope()).unwrap();
    (d, p, k, s)
}
fn issue(
    s: &mut IntakeStore,
    preset: ConnectionPreset,
    topic: Option<String>,
    future: bool,
) -> IssuedConnection {
    s.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: VaultKey::generate().unwrap().to_hex(),
            label: "Synthetic client".into(),
            host: ConnectionHost::Codex,
            preset,
            topic_id: topic,
            include_future: future,
        },
    )
    .unwrap()
}
fn query(id: &str, term: &str) -> ConnectedQuery {
    ConnectedQuery {
        request_id: id.into(),
        query: Some(term.into()),
        memory_id: None,
        max_items: 5,
    }
}
#[test]
fn owner_lifetime_expires_even_with_an_already_authenticated_client() {
    let (_d, _p, _k, mut s) = connected_setup();
    let input = |id: &str| ConnectionRequest {
        request_id: id.into(),
        label: "Short synthetic connection".into(),
        host: ConnectionHost::Cursor,
        preset: ConnectionPreset::ReadOnly,
        topic_id: None,
        include_future: true,
    };
    for ttl in [0, 86401, u32::MAX] {
        assert!(
            s.create_connection_with_lifetime(&scope(), input("invalid"), ttl)
                .is_err()
        );
    }
    assert!(s.connections(&scope()).unwrap().is_empty());
    let short = s
        .create_connection_with_lifetime(&scope(), input("short"), 2)
        .unwrap();
    let parse = |text: &str| {
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).unwrap()
    };
    assert_eq!(
        (parse(&short.connection.expires_at) - parse(&short.connection.created_at)).whole_seconds(),
        2
    );
    let access = s.authenticate_connection(&scope(), &short.token).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert!(s.authenticate_connection(&scope(), &short.token).is_err());
    assert!(
        s.query_connected(&scope(), &access, query("expired", "synthetic"))
            .is_err()
    );
    assert!(s.connection_usage(&scope(), None).unwrap().is_empty());
    let default = s.create_connection(&scope(), input("default")).unwrap();
    assert_eq!(
        (parse(&default.connection.expires_at) - parse(&default.connection.created_at))
            .whole_seconds(),
        86400
    );
}
fn basic(s: &mut IntakeStore, id: &str, value: &str) -> (IntakeReceipt, CommittedCompilation) {
    ingest(s, id, value, json!({}), json!({"kind":"unknown"}))
}
#[test]
fn scoped_minimal_view_receipt_exact_dedup_and_retry_are_not_confirmation() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    basic(&mut s, "b", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let out = s
        .query_connected(&scope(), &a, query("one", "local"))
        .unwrap();
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0].members.len(), 2);
    assert_eq!(out.items[0].sources.len(), 2);
    assert_eq!(out.receipt.view, "compiled_unverified");
    assert_eq!(out.items[0].semantic_support, "model_output_unverified");
    let raw = serde_json::to_string(&out).unwrap();
    assert!(!raw.contains("SYNTHETIC evidence"));
    assert!(!raw.contains("quote"));
    assert!(!raw.contains(&c.token));
    let repeat = s
        .query_connected(&scope(), &a, query("one", "local"))
        .unwrap();
    assert_eq!(repeat.receipt.receipt_id, out.receipt.receipt_id);
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 1);
    assert_eq!(s.connection_usage(&scope(), None).unwrap().len(), 1);
    assert!(
        s.query_connected(&scope(), &a, query("one", "changed"))
            .is_err()
    );
}
#[test]
fn snapshot_excludes_future_members_while_explicit_future_includes_them() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let old = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let future = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    basic(&mut s, "b", "cloud");
    let a = s.authenticate_connection(&scope(), &old.token).unwrap();
    let b = s.authenticate_connection(&scope(), &future.token).unwrap();
    assert!(
        s.query_connected(&scope(), &a, query("one", "cloud"))
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        s.query_connected(&scope(), &b, query("one", "cloud"))
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn topic_is_filtered_before_search_and_future_does_not_expand_to_other_topics() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    s.consolidate_pending(&scope(), 4).unwrap();
    let page = pages(&s).remove(0);
    let c = issue(&mut s, ConnectionPreset::Topic, Some(page.id), false);
    s.receive(&authority(), &request("other")).unwrap();
    let attempt = prepared(&mut s);
    let mut out: Value = serde_json::from_slice(&output(
        "private other",
        json!({}),
        json!({"kind":"unknown"}),
    ))
    .unwrap();
    out["units"][0]["content"]["subject"] = json!("project:other");
    let other = s
        .commit_compilation(
            &scope(),
            &attempt.job_id,
            &attempt.lease_id,
            &serde_json::to_vec(&out).unwrap(),
        )
        .unwrap();
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    assert!(
        s.query_connected(&scope(), &a, query("s", "other"))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        s.query_connected(
            &scope(),
            &a,
            ConnectedQuery {
                request_id: "g".into(),
                query: None,
                memory_id: Some(other.output_versions[0].object_id.clone()),
                max_items: 1
            }
        )
        .unwrap()
        .items
        .is_empty()
    );
    assert_eq!(
        s.query_connected(&scope(), &a, query("ok", "local"))
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn capture_permission_explicit_request_role_and_request_namespaces_are_bound() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let read = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let c = issue(&mut s, ConnectionPreset::CaptureRead, None, false);
    let d = issue(&mut s, ConnectionPreset::CaptureRead, None, false);
    let a = s.authenticate_connection(&scope(), &read.token).unwrap();
    assert!(
        s.capture_connected(&scope(), &a, "save", "synthetic", true)
            .is_err()
    );
    let b = s.authenticate_connection(&scope(), &c.token).unwrap();
    assert!(
        s.capture_connected(&scope(), &b, "save", "synthetic", false)
            .is_err()
    );
    let one = s
        .capture_connected(&scope(), &b, "save", "synthetic", true)
        .unwrap();
    let repeat = s
        .capture_connected(&scope(), &b, "save", "synthetic", true)
        .unwrap();
    assert_eq!(one.source, repeat.source);
    let source = s.source(&scope(), &one.source.object_id).unwrap();
    assert_eq!(source.role, SourceRole::Mixed);
    assert_eq!(source.capture_fidelity, CaptureFidelity::F1AiDigest);
    assert_eq!(
        s.job(&scope(), &one.job_id).unwrap().job.state,
        JobState::Received
    );
    let a = s.authenticate_connection(&scope(), &d.token).unwrap();
    let two = s
        .capture_connected(&scope(), &a, "save", "synthetic", true)
        .unwrap();
    assert_ne!(one.source, two.source);
    assert!(
        s.capture_connected(&scope(), &b, "save", "changed", true)
            .is_err()
    );
}
#[test]
fn revocation_fences_cached_results_and_an_already_authenticated_access() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    s.query_connected(&scope(), &a, query("one", "local"))
        .unwrap();
    s.revoke_connection(&scope(), &c.connection.id).unwrap();
    assert!(s.authenticate_connection(&scope(), &c.token).is_err());
    assert!(
        s.query_connected(&scope(), &a, query("one", "local"))
            .is_err()
    );
    assert!(
        s.query_connected(&scope(), &a, query("two", "local"))
            .is_err()
    );
}
#[test]
fn source_deletion_invalidates_cached_content_but_not_independent_policy_and_no_reexecution() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (r, _) = basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    s.query_connected(&scope(), &a, query("one", "local"))
        .unwrap();
    s.delete_source(&scope(), &r.source.object_id).unwrap();
    assert!(s.authenticate_connection(&scope(), &c.token).is_ok());
    assert!(
        s.query_connected(&scope(), &a, query("one", "local"))
            .is_err()
    );
    basic(&mut s, "b", "local");
    assert!(
        s.query_connected(&scope(), &a, query("one", "local"))
            .is_err()
    );
    assert_eq!(
        s.query_connected(&scope(), &a, query("fresh", "local"))
            .unwrap()
            .items
            .len(),
        1
    );
    s.revoke_connection(&scope(), &c.connection.id).unwrap();
}
#[test]
fn snapshot_correction_requires_new_authorization_and_cannot_replay_old_meaning() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let access = s.authenticate_connection(&scope(), &c.token).unwrap();
    s.query_connected(&scope(), &access, query("old", "local"))
        .unwrap();
    let target = commit.output_versions[0].clone();
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    s.request_correction(
        &owner,
        target,
        "correction".into(),
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
    assert!(
        s.query_connected(&scope(), &access, query("old", "local"))
            .is_err()
    );
    assert!(
        s.query_connected(&scope(), &access, query("new", "cloud"))
            .unwrap()
            .items
            .is_empty()
    );
}
#[test]
fn backup_preserves_assets_but_drops_client_authority_without_changing_live_connection() {
    let (d, _p, k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let p = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &p).unwrap();
    let mut restored = IntakeStore::open(&p, k, scope()).unwrap();
    restored.enable_connections(&scope()).unwrap();
    assert!(
        restored
            .authenticate_connection(&scope(), &c.token)
            .is_err()
    );
    assert!(restored.connections(&scope()).unwrap().is_empty());
    assert_eq!(restored.compiled_units(&scope(), None).unwrap().len(), 1);
    assert!(s.authenticate_connection(&scope(), &c.token).is_ok());
}
#[test]
fn failed_final_policy_write_rolls_back_receipt_cache_key_usage_and_budget() {
    let (_d, p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let db = rusqlite::Connection::open(p).unwrap();
    db.execute_batch("CREATE TRIGGER n3_fault BEFORE UPDATE ON intake_objects WHEN OLD.kind='connection' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    assert!(
        s.query_connected(&scope(), &a, query("once", "local"))
            .is_err()
    );
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 0);
    assert!(s.connection_usage(&scope(), None).unwrap().is_empty());
    let count: i64 = db
        .query_row("SELECT count(*) FROM intake_connection_calls", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    db.execute_batch("DROP TRIGGER n3_fault").unwrap();
    assert!(
        s.query_connected(&scope(), &a, query("once", "local"))
            .is_ok()
    );
}
#[test]
fn concurrent_identical_queries_commit_one_logical_receipt() {
    let (_d, p, k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let tasks = (0..2)
        .map(|_| {
            let p = p.clone();
            let k = k.clone();
            let a = a.clone();
            std::thread::spawn(move || {
                let mut s = IntakeStore::open(&p, k, scope()).unwrap();
                s.query_connected(&scope(), &a, query("race", "local"))
                    .unwrap()
                    .receipt
                    .receipt_id
            })
        })
        .collect::<Vec<_>>();
    let ids = tasks
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 1);
}
#[test]
fn rate_scope_validation_search_and_creation_retry_fail_closed() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    for i in 0..30 {
        s.query_connected(&scope(), &a, query(&format!("call-{i}"), "local"))
            .unwrap();
    }
    assert!(
        s.query_connected(&scope(), &a, query("over", "local"))
            .is_err()
    );
    let mut foreign = scope();
    foreign.space = "other".into();
    assert!(
        s.query_connected(&foreign, &a, query("foreign", "local"))
            .is_err()
    );
    assert!(s.authenticate_connection(&scope(), "bad").is_err());
    assert!(
        s.search_owner_units(&scope(), "SYNTHETIC evidence", None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.search_owner_units(&scope(), "local", None).unwrap().len(),
        1
    );
    let input = ConnectionRequest {
        request_id: "unique-issue".into(),
        label: "synthetic".into(),
        host: ConnectionHost::Codex,
        preset: ConnectionPreset::ReadOnly,
        topic_id: None,
        include_future: false,
    };
    s.create_connection(&scope(), input.clone()).unwrap();
    assert!(s.create_connection(&scope(), input).is_err());
    assert_eq!(s.connections(&scope()).unwrap().len(), 2);
}

#[test]
fn semantic_topic_tags_cannot_authorize_new_members_without_trusted_membership() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "local");
    s.consolidate_pending(&scope(), 4).unwrap();
    let page = pages(&s).remove(0);
    let input = ConnectionRequest {
        request_id: "unsafe-future".into(),
        label: "test".into(),
        host: ConnectionHost::Codex,
        preset: ConnectionPreset::Topic,
        topic_id: Some(page.id.clone()),
        include_future: true,
    };
    assert!(s.create_connection(&scope(), input).is_err());
    let c = issue(&mut s, ConnectionPreset::Topic, Some(page.id), false);
    basic(&mut s, "b", "new matching model tag");
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    assert!(
        s.query_connected(&scope(), &a, query("bounded", "new matching"))
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn six_recipient_identities_are_bound_by_owner_and_keep_legacy_other_compatible() {
    let (_d, _p, _k, mut s) = connected_setup();
    for (host, slug) in [
        (ConnectionHost::Chatgpt, "chatgpt"),
        (ConnectionHost::Codex, "codex"),
        (ConnectionHost::Claude, "claude"),
        (ConnectionHost::Workbuddy, "workbuddy"),
        (ConnectionHost::Grok, "grok"),
        (ConnectionHost::Cursor, "cursor"),
        (ConnectionHost::Other, "other"),
    ] {
        let issued = s
            .create_connection(
                &scope(),
                ConnectionRequest {
                    request_id: format!("issue-{slug}"),
                    label: format!("Synthetic {slug}"),
                    host,
                    preset: ConnectionPreset::CaptureRead,
                    topic_id: None,
                    include_future: true,
                },
            )
            .unwrap();
        let access = s.authenticate_connection(&scope(), &issued.token).unwrap();
        let saved = s
            .capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
            .unwrap();
        assert_eq!(saved.processing, JobState::Received);
        assert!(!saved.replayed);
        let source = s.source(&scope(), &saved.receipt.source.object_id).unwrap();
        assert_eq!(source.source_platform, format!("mcp-{slug}"));
        assert_eq!(source.role, SourceRole::Mixed);
        assert_eq!(source.capture_fidelity, CaptureFidelity::F1AiDigest);
        assert_eq!(serde_json::to_value(host).unwrap(), slug);
    }
    assert_eq!(s.connections(&scope()).unwrap().len(), 7);
    assert_eq!(
        serde_json::from_str::<ConnectionHost>("\"other\"")
            .unwrap()
            .as_str(),
        "other"
    );
}

#[test]
fn save_replay_observes_current_job_without_duplicate_or_withdrawal_revival() {
    let (_d, _p, _k, mut s) = connected_setup();
    let issued = issue(&mut s, ConnectionPreset::CaptureRead, None, true);
    let access = s.authenticate_connection(&scope(), &issued.token).unwrap();
    assert!(
        s.capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", false)
            .is_err()
    );
    let saved = s
        .capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
        .unwrap();
    let replay = s
        .capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
        .unwrap();
    assert_eq!(replay.receipt, saved.receipt);
    assert!(replay.replayed);
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 1);
    assert!(
        s.capture_connected_status(&scope(), &access, "save", "changed", true)
            .is_err()
    );
    let attempt = prepared(&mut s);
    assert_eq!(
        s.capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
            .unwrap()
            .processing,
        JobState::Processing
    );
    s.commit_compilation(
        &scope(),
        &attempt.job_id,
        &attempt.lease_id,
        &output("local", json!({}), json!({"kind":"unknown"})),
    )
    .unwrap();
    let ready = s
        .capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
        .unwrap();
    assert_eq!(ready.processing, JobState::Ready);
    assert_eq!(ready.receipt, saved.receipt);
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 1);
    s.withdraw_source(&scope(), &saved.receipt.source.object_id)
        .unwrap();
    assert!(
        s.capture_connected_status(&scope(), &access, "save", "SYNTHETIC evidence", true)
            .is_err()
    );
    assert_eq!(s.connections(&scope()).unwrap()[0].used_calls, 1);
}

#[test]
fn first_save_needs_no_future_read_consent_and_does_not_expand_empty_snapshot() {
    let (_d, _p, _k, mut s) = connected_setup();
    let issued = issue(&mut s, ConnectionPreset::CaptureRead, None, false);
    assert!(issued.connection.member_snapshot.is_empty());
    assert!(!issued.connection.include_future);
    let access = s.authenticate_connection(&scope(), &issued.token).unwrap();
    let receipt = s
        .capture_connected(&scope(), &access, "first-save", "SYNTHETIC evidence", true)
        .unwrap();
    let attempt = prepared(&mut s);
    let committed = s
        .commit_compilation(
            &scope(),
            &attempt.job_id,
            &attempt.lease_id,
            &output("new private memory", json!({}), json!({"kind":"unknown"})),
        )
        .unwrap();
    assert_eq!(receipt.job_id, attempt.job_id);
    let read = s
        .query_connected(
            &scope(),
            &access,
            ConnectedQuery {
                request_id: "no-future-search".into(),
                query: Some("new".into()),
                memory_id: None,
                max_items: 5,
            },
        )
        .unwrap();
    assert!(read.items.is_empty());
    let direct = s
        .query_connected(
            &scope(),
            &access,
            ConnectedQuery {
                request_id: "no-future-get".into(),
                query: None,
                memory_id: Some(committed.output_versions[0].object_id.clone()),
                max_items: 1,
            },
        )
        .unwrap();
    assert!(direct.items.is_empty());
}
