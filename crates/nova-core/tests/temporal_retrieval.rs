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
fn basic(s: &mut IntakeStore, id: &str, value: &str) -> (IntakeReceipt, CommittedCompilation) {
    ingest(s, id, value, json!({}), json!({"kind":"unknown"}))
}

fn history(s: &mut IntakeStore, c: &IssuedConnection) {
    s.set_connection_history_policy(
        &scope(),
        &c.connection.id,
        0,
        Some("2000-01-01T00:00:00Z".into()),
    )
    .unwrap();
}
fn historical(id: &str, memory: &str, known: &str, valid: Option<&str>) -> HistoricalQuery {
    HistoricalQuery {
        request_id: id.into(),
        query: None,
        memory_id: Some(memory.into()),
        max_items: 5,
        known_as_of: known.into(),
        valid_at: valid.map(Into::into),
        unknown_validity: UnknownValidity::Exclude,
    }
}
fn correction(s: &mut IntakeStore, target: RevisionRef, value: &str, valid: Value) -> CompiledUnit {
    s.request_correction(
        &CorrectionAuthority {
            scope: scope(),
            actor_id: "owner".into(),
            may_correct: true,
        },
        target.clone(),
        "correction".into(),
        "SYNTHETIC evidence".into(),
    )
    .unwrap();
    let a = prepared(s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output(value, json!({}), valid),
    )
    .unwrap();
    s.compiled_unit(&scope(), &target.object_id).unwrap()
}
fn interval(start: &str, end: Option<&str>) -> Value {
    json!({"kind":"interval","start":start,"end":end})
}

#[test]
fn existing_connection_never_implies_history_permission_or_spends_on_denial() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "old assertion");
    let unit = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    assert_eq!(
        s.connection_history_policy(&scope(), &c.connection.id)
            .unwrap()
            .revision,
        0
    );
    let q = historical(
        "h",
        &unit.header.object.object_id,
        &unit.header.recorded_at,
        None,
    );
    assert!(
        s.query_connected_historical(&scope(), &a, q.clone())
            .is_err()
    );
    assert!(s.connection_usage(&scope(), None).unwrap().is_empty());
    history(&mut s, &c);
    let r = s.query_connected_historical(&scope(), &a, q).unwrap();
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.items[0].temporal_match, "not_filtered");
    assert_eq!(r.receipt.view, "compiled_unverified_historical");
}

#[test]
fn late_arrival_record_time_and_stated_validity_are_separate() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = ingest(
        &mut s,
        "a",
        "retroactive event",
        json!({}),
        interval("2010-01-01T00:00:00Z", Some("2011-01-01T00:00:00Z")),
    );
    let u = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let before = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "before",
                &u.header.object.object_id,
                "2010-06-01T00:00:00Z",
                Some("2010-06-01T00:00:00Z"),
            ),
        )
        .unwrap();
    assert!(before.items.is_empty());
    let known = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "after",
                &u.header.object.object_id,
                &u.header.recorded_at,
                Some("2010-06-01T00:00:00Z"),
            ),
        )
        .unwrap();
    assert_eq!(known.items.len(), 1);
    assert_eq!(known.items[0].temporal_match, "matched_stated_interval");
    for (id, time) in [
        ("before-valid", "2009-12-31T23:59:59Z"),
        ("exclusive-end", "2011-01-01T00:00:00Z"),
    ] {
        assert!(
            s.query_connected_historical(
                &scope(),
                &a,
                historical(
                    id,
                    &u.header.object.object_id,
                    &u.header.recorded_at,
                    Some(time)
                )
            )
            .unwrap()
            .items
            .is_empty()
        );
    }
}

#[test]
fn unknown_time_is_explicit_never_filled_from_capture_or_recording() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "timeless assertion");
    let u = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let q = historical(
        "exclude",
        &u.header.object.object_id,
        &u.header.recorded_at,
        Some("2010-06-01T00:00:00Z"),
    );
    assert!(
        s.query_connected_historical(&scope(), &a, q.clone())
            .unwrap()
            .items
            .is_empty()
    );
    let mut q = q;
    q.request_id = "include".into();
    q.unknown_validity = UnknownValidity::IncludeAsUnknown;
    let r = s.query_connected_historical(&scope(), &a, q).unwrap();
    assert_eq!(r.items[0].temporal_match, "unknown_included");
    assert_eq!(r.items[0].valid_time, ValidTime::Unknown);
    assert!(r.items[0].occurred_at.is_none());
}

#[test]
fn historical_correction_selects_version_then_filters_never_falls_back() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = ingest(
        &mut s,
        "a",
        "old",
        json!({}),
        interval("2010-01-01T00:00:00Z", Some("2011-01-01T00:00:00Z")),
    );
    let old = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let q = historical(
        "old-cached",
        &old.header.object.object_id,
        &old.header.recorded_at,
        Some("2010-06-01T00:00:00Z"),
    );
    s.query_connected_historical(&scope(), &a, q.clone())
        .unwrap();
    let new = correction(
        &mut s,
        old.header.object.clone(),
        "corrected",
        interval("2012-01-01T00:00:00Z", None),
    )
    .unit;
    assert!(s.query_connected_historical(&scope(), &a, q).is_err());
    let r = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "new-request-old-time",
                &old.header.object.object_id,
                &old.header.recorded_at,
                Some("2010-06-01T00:00:00Z"),
            ),
        )
        .unwrap();
    assert_eq!(r.items[0].memory.revision, 1);
    assert_eq!(r.items[0].authorized_current_version.revision, 2);
    assert_eq!(r.items[0].content.value, "old");
    let r = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "new-record-old-valid",
                &old.header.object.object_id,
                &new.header.recorded_at,
                Some("2010-06-01T00:00:00Z"),
            ),
        )
        .unwrap();
    assert!(r.items.is_empty());
    let r = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "new-record-new-valid",
                &old.header.object.object_id,
                &new.header.recorded_at,
                Some("2012-06-01T00:00:00Z"),
            ),
        )
        .unwrap();
    assert_eq!(r.items[0].memory.revision, 2);
    assert_eq!(r.items[0].content.value, "corrected");
}

#[test]
fn history_policy_cas_revocation_and_fresh_grant_do_not_replay_old_cache() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "history");
    let u = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let q = historical(
        "cached",
        &u.header.object.object_id,
        &u.header.recorded_at,
        None,
    );
    let r = s
        .query_connected_historical(&scope(), &a, q.clone())
        .unwrap();
    assert_eq!(
        serde_json::to_value(r).unwrap(),
        serde_json::to_value(
            s.query_connected_historical(&scope(), &a, q.clone())
                .unwrap()
        )
        .unwrap()
    );
    assert_eq!(s.connection_usage(&scope(), None).unwrap().len(), 1);
    assert!(
        s.set_connection_history_policy(&scope(), &c.connection.id, 0, None)
            .is_err()
    );
    s.set_connection_history_policy(&scope(), &c.connection.id, 1, None)
        .unwrap();
    assert!(
        s.query_connected_historical(&scope(), &a, q.clone())
            .is_err()
    );
    s.set_connection_history_policy(
        &scope(),
        &c.connection.id,
        2,
        Some("2000-01-01T00:00:00Z".into()),
    )
    .unwrap();
    assert!(s.query_connected_historical(&scope(), &a, q).is_err());
    let fresh = historical(
        "fresh",
        &u.header.object.object_id,
        &u.header.recorded_at,
        None,
    );
    s.query_connected_historical(&scope(), &a, fresh.clone())
        .unwrap();
    s.revoke_connection(&scope(), &c.connection.id).unwrap();
    assert!(s.query_connected_historical(&scope(), &a, fresh).is_err());
    assert!(
        s.set_connection_history_policy(&scope(), &c.connection.id, 3, None)
            .is_err()
    );
}

#[test]
fn source_withdrawal_and_deletion_cannot_be_reversed_by_time_travel() {
    for delete in [false, true] {
        let (_d, _p, _k, mut s) = connected_setup();
        let (source, commit) = basic(&mut s, "a", "never resurrect");
        let u = s
            .compiled_unit(&scope(), &commit.output_versions[0].object_id)
            .unwrap()
            .unit;
        let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
        history(&mut s, &c);
        let a = s.authenticate_connection(&scope(), &c.token).unwrap();
        let q = historical(
            "before",
            &u.header.object.object_id,
            &u.header.recorded_at,
            None,
        );
        s.query_connected_historical(&scope(), &a, q.clone())
            .unwrap();
        if delete {
            s.delete_source(&scope(), &source.source.object_id).unwrap();
        } else {
            s.withdraw_source(&scope(), &source.source.object_id)
                .unwrap();
        }
        assert!(s.query_connected_historical(&scope(), &a, q).is_err());
        let r = s
            .query_connected_historical(
                &scope(),
                &a,
                historical(
                    "after",
                    &u.header.object.object_id,
                    &u.header.recorded_at,
                    None,
                ),
            )
            .unwrap();
        assert!(r.items.is_empty());
    }
}

#[test]
fn fixed_revision_grant_does_not_expand_to_corrected_head_or_previous_history() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "original");
    let old = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    correction(
        &mut s,
        old.header.object.clone(),
        "corrected",
        json!({"kind":"unknown"}),
    );
    let r = s
        .query_connected_historical(
            &scope(),
            &a,
            historical(
                "old",
                &old.header.object.object_id,
                &old.header.recorded_at,
                None,
            ),
        )
        .unwrap();
    assert!(r.items.is_empty());
}

#[test]
fn current_revision_grant_plus_explicit_history_permission_can_read_older_same_object() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "original");
    let old = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    correction(
        &mut s,
        old.header.object.clone(),
        "corrected",
        json!({"kind":"unknown"}),
    );
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    assert_eq!(c.connection.member_snapshot[0].revision, 2);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    let q = historical(
        "h",
        &old.header.object.object_id,
        &old.header.recorded_at,
        None,
    );
    assert!(
        s.query_connected_historical(&scope(), &a, q.clone())
            .is_err()
    );
    history(&mut s, &c);
    let r = s.query_connected_historical(&scope(), &a, q).unwrap();
    assert_eq!(r.items[0].memory.revision, 1);
    assert_eq!(r.items[0].authorized_current_version.revision, 2);
    assert_eq!(r.items[0].content.value, "original");
}

#[test]
fn bounded_authorized_lexical_search_no_hidden_counts_and_encrypted_reopen() {
    let (_d, p, k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "独特历史会议工作秘密");
    let u = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, false);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    basic(&mut s, "hidden", "未授权 工作会议历史");
    let mut q = historical(
        "search",
        &u.header.object.object_id,
        &u.header.recorded_at,
        None,
    );
    q.memory_id = None;
    q.query = Some("会议 工作".into());
    let r = s
        .query_connected_historical(&scope(), &a, q.clone())
        .unwrap();
    assert_eq!(r.items.len(), 1);
    assert!(!r.more_available);
    assert!(!serde_json::to_string(&r).unwrap().contains("未授权"));
    drop(s);
    let raw = std::fs::read(&p).unwrap();
    assert!(
        !raw.windows("独特历史".len())
            .any(|b| b == "独特历史".as_bytes())
    );
    let mut s = IntakeStore::open(&p, k, scope()).unwrap();
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    assert_eq!(
        serde_json::to_value(r).unwrap(),
        serde_json::to_value(s.query_connected_historical(&scope(), &a, q).unwrap()).unwrap()
    );
}

#[test]
fn invalid_times_cross_scope_and_changed_request_are_rejected_without_spend() {
    let (_d, _p, _k, mut s) = connected_setup();
    let (_, commit) = basic(&mut s, "a", "test");
    let u = s
        .compiled_unit(&scope(), &commit.output_versions[0].object_id)
        .unwrap()
        .unit;
    let c = issue(&mut s, ConnectionPreset::ReadOnly, None, true);
    history(&mut s, &c);
    let a = s.authenticate_connection(&scope(), &c.token).unwrap();
    for time in ["yesterday", "2099-01-01T00:00:00Z", "1990-01-01T00:00:00Z"] {
        assert!(
            s.query_connected_historical(
                &scope(),
                &a,
                historical("invalid", &u.header.object.object_id, time, None)
            )
            .is_err()
        );
    }
    let mut other = scope();
    other.owner_id = "other".into();
    let q = historical(
        "valid",
        &u.header.object.object_id,
        &u.header.recorded_at,
        None,
    );
    assert!(s.query_connected_historical(&other, &a, q.clone()).is_err());
    assert!(s.connection_usage(&scope(), None).unwrap().is_empty());
    s.query_connected_historical(&scope(), &a, q.clone())
        .unwrap();
    let mut q = q;
    q.valid_at = Some("2010-01-01T00:00:00Z".into());
    assert!(s.query_connected_historical(&scope(), &a, q).is_err());
    assert_eq!(s.connection_usage(&scope(), None).unwrap().len(), 1);
}
