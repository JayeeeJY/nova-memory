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
    ingest_output(s, id, &output(value, condition, time))
}
fn ingest_output(
    s: &mut IntakeStore,
    id: &str,
    output: &[u8],
) -> (IntakeReceipt, CommittedCompilation) {
    let r = s.receive(&authority(), &request(id)).unwrap();
    let a = prepared(s);
    let c = s
        .commit_compilation(&scope(), &a.job_id, &a.lease_id, output)
        .unwrap();
    (r, c)
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
fn basic(s: &mut IntakeStore, id: &str, value: &str) -> (IntakeReceipt, CommittedCompilation) {
    ingest(s, id, value, json!({}), json!({"kind":"unknown"}))
}

#[test]
fn neighborhood_is_bounded_to_current_granted_same_subject_occurrences() {
    let (_d, _p, _k, mut store) = connected_setup();
    let event = |store: &mut IntakeStore, id: &str, subject: &str, date: &str| {
        let mut candidate: Value =
            serde_json::from_slice(&output(id, json!({}), json!({"kind":"unknown"}))).unwrap();
        candidate["units"][0]["content"]["subject"] = json!(subject);
        candidate["units"][0]["occurred_at"] = json!(date);
        let (receipt, commit) = ingest_output(store, id, &serde_json::to_vec(&candidate).unwrap());
        (receipt, commit.output_versions[0].clone())
    };
    let subject = "project:starbridge";
    let (anchor_source, anchor) = event(&mut store, "anchor", subject, "2026-10-10T12:00:00Z");
    let (nearest_source, nearest) =
        event(&mut store, "nearest", subject, "2026-10-10T21:00:00+08:00");
    let (_, edge) = event(&mut store, "edge", subject, "2026-10-08T12:00:00Z");
    event(&mut store, "outside", subject, "2026-10-08T11:59:59Z");
    event(
        &mut store,
        "other-subject",
        "project:other",
        "2026-10-10T12:00:00Z",
    );
    let (_, unknown) = basic(&mut store, "unknown", "未知");
    let unknown = unknown.output_versions[0].clone();
    let connection = issue(&mut store, ConnectionPreset::ReadOnly, None, false);
    let (_, hidden) = event(&mut store, "ungranted", subject, "2026-10-10T12:00:01Z");
    let access = store
        .authenticate_connection(&scope(), &connection.token)
        .unwrap();
    let read = |store: &mut IntakeStore, id: &str, anchor: RevisionRef, days, limit| {
        store.query_connected_neighborhood(&scope(), &access, id.into(), anchor, days, limit)
    };
    let result = read(&mut store, "ordered", anchor.clone(), 2, 8).unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .map(|hit| &hit.memory)
            .collect::<Vec<_>>(),
        vec![&nearest, &edge]
    );
    assert_eq!(
        result.receipt.memory_versions,
        vec![anchor.clone(), nearest.clone(), edge]
    );
    assert_eq!(result.receipt.view, "compiled_unverified_neighborhood");
    assert!(!result.more_available);
    let limited = read(&mut store, "limited", anchor.clone(), 2, 1).unwrap();
    assert_eq!(limited.items[0].memory, nearest);
    assert!(limited.more_available);
    assert_eq!(
        read(&mut store, "limited", anchor.clone(), 2, 1)
            .unwrap()
            .receipt
            .receipt_id,
        limited.receipt.receipt_id
    );
    assert!(read(&mut store, "limited", anchor.clone(), 3, 1).is_err());
    assert!(read(&mut store, "invalid-window", anchor.clone(), 31, 1).is_err());
    let mismatched = RevisionRef {
        revision: 2,
        ..anchor.clone()
    };
    assert!(read(&mut store, "mismatched-version", mismatched, 2, 1).is_err());
    assert!(read(&mut store, "hidden", hidden, 2, 8).is_err());
    let undated = read(&mut store, "unknown-time", unknown.clone(), 2, 8).unwrap();
    assert_eq!(undated.receipt.status, "anchor_time_unknown");
    assert!(undated.items.is_empty() && !undated.more_available);

    store
        .withdraw_source(&scope(), &nearest_source.source.object_id)
        .unwrap();
    assert!(read(&mut store, "limited", anchor.clone(), 2, 1).is_err());
    let after = read(&mut store, "after", anchor.clone(), 2, 1).unwrap();
    assert_eq!(after.items.len(), 1);
    assert!(!after.more_available);
    store
        .withdraw_source(&scope(), &anchor_source.source.object_id)
        .unwrap();
    assert!(read(&mut store, "after", anchor.clone(), 2, 1).is_err());
    store
        .revoke_connection(&scope(), &connection.connection.id)
        .unwrap();
    assert!(read(&mut store, "unknown-time", unknown, 2, 8).is_err());
    let usage = store.connection_usage(&scope(), None).unwrap();
    assert_eq!(usage.len(), 4);
    assert!(
        usage
            .iter()
            .all(|(_, event)| event.operation == "read_index")
    );
}

#[test]
fn progressive_read_pins_current_revision_and_rechecks_cached_index_after_correction() {
    let (_d, _p, _k, mut store) = connected_setup();
    let (_, commit) = basic(&mut store, "original", "星桥内部周会最多25分钟");
    let connection = issue(&mut store, ConnectionPreset::ReadOnly, None, true);
    let access = store
        .authenticate_connection(&scope(), &connection.token)
        .unwrap();
    let index = store
        .query_connected_index(&scope(), &access, query("index-v1", "星桥"))
        .unwrap();
    let v1 = index.items[0].memory.clone();
    assert_eq!(v1, commit.output_versions[0]);
    assert_eq!(
        store
            .query_connected_index(&scope(), &access, query("index-v1", "星桥"))
            .unwrap()
            .receipt
            .receipt_id,
        index.receipt.receipt_id
    );
    let first = store
        .query_connected_current_revision(&scope(), &access, "detail-v1".into(), v1.clone())
        .unwrap();
    assert_eq!(first.items[0].content.value, "星桥内部周会最多25分钟");
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    store
        .request_correction(
            &owner,
            v1.clone(),
            "correction".into(),
            "SYNTHETIC evidence".into(),
        )
        .unwrap();
    let job = prepared(&mut store);
    store
        .commit_compilation(
            &scope(),
            &job.job_id,
            &job.lease_id,
            &output(
                "星桥内部周会最多20分钟",
                json!({}),
                json!({"kind":"unknown"}),
            ),
        )
        .unwrap();

    assert!(
        store
            .query_connected_index(&scope(), &access, query("index-v1", "星桥"))
            .is_err()
    );
    for request_id in ["detail-v1", "stale-new-request"] {
        assert!(
            store
                .query_connected_current_revision(&scope(), &access, request_id.into(), v1.clone())
                .is_err()
        );
    }
    assert_eq!(store.connection_usage(&scope(), None).unwrap().len(), 2);
    let refreshed = store
        .query_connected_index(&scope(), &access, query("index-v2", "星桥"))
        .unwrap();
    let v2 = refreshed.items[0].memory.clone();
    assert_eq!(v2.object_id, v1.object_id);
    assert_eq!(v2.revision, 2);
    let latest = store
        .query_connected_current_revision(&scope(), &access, "detail-v2".into(), v2.clone())
        .unwrap();
    assert_eq!(latest.items[0].content.value, "星桥内部周会最多20分钟");
    assert!(latest.items[0].protected);
    let report = store.construction_report(&scope()).unwrap();
    assert_eq!(report.usage.logical_calls, 4);
    assert_eq!(report.usage.delivered_items, 2);
    assert_eq!(report.usage.operations["read_index"], 2);

    store
        .revoke_connection(&scope(), &connection.connection.id)
        .unwrap();
    assert!(
        store
            .query_connected_current_revision(&scope(), &access, "detail-v2".into(), v2)
            .is_err()
    );
    assert_eq!(store.connection_usage(&scope(), None).unwrap().len(), 4);
}

#[test]
fn chinese_reordered_terms_rank_relevant_authorized_results_and_trace() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "工作会议最多30分钟");
    basic(&mut s, "b", "工作早餐");
    basic(&mut s, "c", "私人聚会不限时间");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let old = s
        .query_connected(&scope(), &a, query("same", "会议 工作"))
        .unwrap();
    assert!(old.items.is_empty());
    let ranked = s
        .query_connected_ranked(&scope(), &a, query("same", "会议 工作"))
        .unwrap();
    assert_eq!(ranked.delivery.items.len(), 2);
    assert_eq!(ranked.delivery.items[0].content.value, "工作会议最多30分钟");
    assert!(ranked.trace[0].lexical_score > ranked.trace[1].lexical_score);
    assert_eq!(ranked.trace[0].matched_terms.len(), 2);
    assert_eq!(ranked.algorithm, RANKING_VERSION);
    assert_eq!(ranked.delivery.receipt.view, "compiled_unverified_ranked");
    assert!(
        ranked
            .delivery
            .receipt
            .disclosed_fields
            .contains(&"lexical_trace".into())
    );
    assert_eq!(
        ranked.delivery.items[0].semantic_support,
        "model_output_unverified"
    );
}

#[test]
fn exact_text_first_and_query_repeat_does_not_charge_twice() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "Alpha Beta");
    basic(&mut s, "b", "Beta Alpha");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let first = s
        .query_connected_ranked(&scope(), &a, query("same", "alpha beta"))
        .unwrap();
    assert_eq!(first.delivery.items[0].content.value, "Alpha Beta");
    assert!(first.trace[0].exact_substring);
    let again = s
        .query_connected_ranked(&scope(), &a, query("same", "alpha beta"))
        .unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&again).unwrap()
    );
    assert_eq!(s.connection_usage(&scope(), None).unwrap().len(), 1);
    assert!(
        s.query_connected_ranked(&scope(), &a, query("same", "different"))
            .is_err()
    );
}

#[test]
fn unauthorized_future_matches_do_not_change_results_scores_or_counts() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "工作会议");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let first = s
        .query_connected_ranked(&scope(), &a, query("one", "工作 会议"))
        .unwrap();
    basic(&mut s, "hidden", "工作 会议 未授权秘密");
    let after = s
        .query_connected_ranked(&scope(), &a, query("two", "工作 会议"))
        .unwrap();
    assert_eq!(
        serde_json::to_value(&first.trace).unwrap(),
        serde_json::to_value(&after.trace).unwrap()
    );
    assert_eq!(first.delivery.items.len(), after.delivery.items.len());
    assert_eq!(first.delivery.more_available, after.delivery.more_available);
    assert!(
        !serde_json::to_string(&after)
            .unwrap()
            .contains("未授权秘密")
    );
}

#[test]
fn revocation_and_source_withdrawal_fence_cached_ranked_delivery() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (source, _) = basic(&mut s, "a", "工作会议");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    s.query_connected_ranked(&scope(), &a, query("one", "工作 会议"))
        .unwrap();
    s.withdraw_source(&scope(), &source.source.object_id)
        .unwrap();
    assert!(
        s.query_connected_ranked(&scope(), &a, query("one", "工作 会议"))
            .is_err()
    );
    assert!(
        s.query_connected_ranked(&scope(), &a, query("two", "工作 会议"))
            .unwrap()
            .delivery
            .items
            .is_empty()
    );
    s.revoke_connection(&scope(), &c.connection.id).unwrap();
    assert!(
        s.query_connected_ranked(&scope(), &a, query("two", "工作 会议"))
            .is_err()
    );
}

#[test]
fn structural_json_keys_are_not_search_terms_and_invalid_query_spends_nothing() {
    let (_d, _p, _k, mut s) = connected_setup();
    basic(&mut s, "a", "无关内容");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    for term in ["!!!", "工作\n会议", ""] {
        assert!(
            s.query_connected_ranked(&scope(), &a, query("bad", term))
                .is_err()
        );
    }
    assert!(s.connection_usage(&scope(), None).unwrap().is_empty());
    let result = s
        .query_connected_ranked(&scope(), &a, query("valid", "predicate"))
        .unwrap();
    assert!(result.delivery.items.is_empty());
    assert!(result.trace.is_empty());
}

#[test]
fn reopen_replay_uses_encrypted_cache_and_no_plaintext_side_index() {
    let (_d, path, key, mut s) = connected_setup();
    basic(&mut s, "a", "独特的中文工作会议秘密");
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let before = s
        .query_connected_ranked(&scope(), &a, query("one", "工作 会议"))
        .unwrap();
    drop(s);
    let mut reopened = IntakeStore::open(&path, key, scope()).unwrap();
    let access = reopened
        .authenticate_connection(&scope(), &c.token)
        .unwrap();
    let after = reopened
        .query_connected_ranked(&scope(), &access, query("one", "工作 会议"))
        .unwrap();
    assert_eq!(
        before.delivery.receipt.receipt_id,
        after.delivery.receipt.receipt_id
    );
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows("独特的中文工作会议秘密".len())
            .any(|bytes| bytes == "独特的中文工作会议秘密".as_bytes())
    );
}
