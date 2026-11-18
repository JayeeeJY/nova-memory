//! N5 HTTP boundaries and lifecycle; only independently created synthetic stores.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::*, intake::*, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31447";
const ROOT: &str = "/console/api/owner/intake";
struct Fixture {
    _dir: tempfile::TempDir,
    router: axum::Router,
    token: String,
    store: IntakeStore,
    scope: AssetScope,
}
fn setup(native: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let kp = dir.path().join("vault.key");
    key.write_new_file(&kp).unwrap();
    let legacy = dir.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone()).unwrap());
    let op = dir.path().join("owner.key");
    VaultKey::generate().unwrap().write_new_file(&op).unwrap();
    let token = std::fs::read_to_string(&op).unwrap().trim().to_owned();
    let scope = AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "n5-http".into(),
    };
    let ip = dir.path().join("intake.sqlite");
    let mut store = IntakeStore::create(&ip, key, scope.clone()).unwrap();
    store.enable_compilation(&scope).unwrap();
    let mut server = ReviewServer::new(legacy, kp, HOST.into(), true)
        .with_owner(scope.owner_id.clone(), scope.space.clone())
        .with_owner_token(&op)
        .unwrap()
        .with_intake_store(&ip)
        .unwrap();
    if native {
        server = server.with_native_workspace().unwrap();
    }
    Fixture {
        _dir: dir,
        router: server.router(),
        token,
        store,
        scope,
    }
}
async fn request(
    f: &Fixture,
    path: &str,
    body: Option<Vec<u8>>,
    token: Option<&str>,
    host: &str,
    origin: Option<&str>,
    review: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(format!("{ROOT}{path}"))
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Host", host);
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    if review {
        req = req.header("X-Nova-Review", "owner-review");
    }
    if let Some(origin) = origin {
        req = req.header("Origin", origin);
    }
    if body.is_some() {
        req = req.header("Content-Type", "application/json");
    }
    let result = f
        .router
        .clone()
        .oneshot(req.body(Body::from(body.unwrap_or_default())).unwrap())
        .await
        .unwrap();
    assert_eq!(result.headers()["cache-control"], "no-store");
    let status = result.status();
    let bytes = to_bytes(result.into_body(), 2_000_000).await.unwrap();
    let data = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({"plain_response":true}));
    (status, data)
}
async fn get(f: &Fixture, path: &str) -> (StatusCode, Value) {
    request(f, path, None, Some(&f.token), HOST, None, true).await
}
async fn post(f: &Fixture, path: &str, body: Value) -> (StatusCode, Value) {
    request(
        f,
        path,
        Some(body.to_string().into_bytes()),
        Some(&f.token),
        HOST,
        Some(&format!("http://{HOST}")),
        true,
    )
    .await
}
fn receive(f: &mut Fixture, id: &str, privacy_level: PrivacyLevel) -> IntakeReceipt {
    let auth = IntakeAuthority {
        scope: f.scope.clone(),
        actor_id: "synthetic-owner".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level,
    };
    f.store.receive(&auth,&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"synthetic-n5-http","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC private original. Never include body in report.","occurred_at":null})).unwrap()).unwrap()
}
#[tokio::test]
async fn report_is_explicit_owner_only_not_a_client_graph_or_policy_extension() {
    let mut f = setup(true);
    let (status, out) = get(&f, "/report").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["task_evaluations"]["items"], json!([]));
    assert_eq!(
        out["construction"]["unmeasured"]["semantic_correctness"],
        Value::Null
    );
    assert_eq!(
        out["construction"]["unmeasured"]["capability_level"],
        Value::Null
    );
    assert!(!out.to_string().contains(&f.token));
    for (token, host, origin, review) in [
        (None, HOST, None, true),
        (Some(f.token.as_str()), "evil.invalid", None, true),
        (
            Some(f.token.as_str()),
            HOST,
            Some("http://evil.invalid"),
            true,
        ),
        (Some(f.token.as_str()), HOST, None, false),
    ] {
        assert_eq!(
            request(&f, "/report", None, token, host, origin, review)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let client = f
        .store
        .create_connection(
            &f.scope,
            ConnectionRequest {
                request_id: "synthetic-client".into(),
                label: "Synthetic client".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap();
    assert_eq!(
        request(&f, "/report", None, Some(&client.token), HOST, None, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f,
            "/task-evaluations",
            Some(b"{}".to_vec()),
            Some(&client.token),
            HOST,
            Some(&format!("http://{HOST}")),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let old = setup(false);
    assert_eq!(get(&old, "/report").await.0, StatusCode::FORBIDDEN);
}
#[tokio::test]
async fn strict_query_and_import_bounds_do_not_accept_scope_or_verification_overrides() {
    let f = setup(true);
    for suffix in [
        "?owner_id=forged",
        "?space=other",
        "?include_sensitive=true",
        "?limit=2",
    ] {
        assert_eq!(
            get(&f, &format!("/report{suffix}")).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    for body in [
        json!({}),
        json!({"verified":true,"capability_level":"L4"}),
        json!({"request_id":"bad","model_api_key":"DO_NOT_STORE"}),
    ] {
        assert_eq!(
            post(&f, "/task-evaluations", body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (_, out) = get(&f, "/report").await;
    assert_eq!(out["task_evaluations"]["items"], json!([]));
    // This route explicitly permits the bounded full artifact beyond the old 96KiB cap;
    // schema rejection must occur instead of incorrectly inheriting that smaller limit.
    let medium = json!({"unknown":"x".repeat(100*1024)})
        .to_string()
        .into_bytes();
    assert_eq!(
        request(
            &f,
            "/task-evaluations",
            Some(medium),
            Some(&f.token),
            HOST,
            Some(&format!("http://{HOST}")),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &f,
            "/task-evaluations",
            Some(vec![b' '; 512 * 1024 + 1]),
            Some(&f.token),
            HOST,
            Some(&format!("http://{HOST}")),
            true
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
#[tokio::test]
async fn ordinary_pending_sources_are_counted_and_withdraw_delete_clear_them_without_exposing_bodies()
 {
    let mut f = setup(true);
    let before = get(&f, "/report").await.1;
    let p = receive(&mut f, "ordinary", PrivacyLevel::Private);
    let _s = receive(&mut f, "sensitive", PrivacyLevel::Sensitive);
    let _z = receive(&mut f, "sealed", PrivacyLevel::Sealed);
    let (status, out) = get(&f, "/report").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["construction"]["construction"]["sources"], 1);
    assert_eq!(out["construction"]["construction"]["uncompiled_sources"], 1);
    assert_eq!(out["construction"]["construction"]["exact_memories"], 0);
    assert_ne!(
        out["construction"]["content_snapshot_id"],
        before["construction"]["content_snapshot_id"]
    );
    assert!(!out.to_string().contains("SYNTHETIC private original"));
    assert!(!out.to_string().contains(&p.source.object_id));
    assert_eq!(
        post(
            &f,
            &format!("/sources/{}/withdraw", p.source.object_id),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&f, "/report").await.1["construction"]["construction"]["sources"],
        0
    );
    assert_eq!(
        post(
            &f,
            &format!("/sources/{}/delete", p.source.object_id),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let out = get(&f, "/report").await.1;
    assert_eq!(out["construction"]["construction"]["sources"], 0);
    assert_eq!(
        out["construction"]["unmeasured"]["actual_cost"],
        Value::Null
    );
}

fn artifact(f: &Fixture, request_id: &str) -> TaskEvaluationSubmission {
    let mut s: TaskEvaluationSubmission =
        serde_json::from_str(include_str!("../../../eval/n5/fixture-submission.json")).unwrap();
    s.request_id = request_id.into();
    s.manifest.asset_snapshot_id = f
        .store
        .construction_report(&f.scope)
        .unwrap()
        .content_snapshot_id;
    let fingerprint = n5_configuration_hash(&s.manifest).unwrap();
    for p in &mut s.predictions {
        p.configuration_hash = fingerprint.clone();
    }
    s
}
#[tokio::test]
async fn full_artifact_is_scored_then_only_aggregate_history_is_returned_and_marked_stale() {
    let mut f = setup(true);
    let submission = artifact(&f, "http-import-control");
    let payload = serde_json::to_value(&submission).unwrap();
    assert!(payload.to_string().len() > 96 * 1024);
    let (status, out) = post(&f, "/task-evaluations", payload.clone()).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["evaluation"]["report"]["run_kind"], "fixture_contract");
    assert_eq!(out["evaluation"]["stale"], false);
    let (_, replay) = post(&f, "/task-evaluations", payload).await;
    assert_eq!(
        replay["evaluation"]["evaluation_id"],
        out["evaluation"]["evaluation_id"]
    );
    let (_, report) = get(&f, "/report").await;
    let items = report["task_evaluations"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["report"]["task_count"], 60);
    assert_eq!(items[0]["report"]["level_status"], "uncalibrated");
    let encoded = report.to_string();
    assert!(!encoded.contains("\"predictions\""));
    assert!(!encoded.contains("\"reference_checks\""));
    assert!(!encoded.contains("\"materials\""));
    assert!(!encoded.contains("\"answer\""));
    assert!(!encoded.contains(&f.token));
    receive(&mut f, "changed-material", PrivacyLevel::Private);
    let report = get(&f, "/report").await.1;
    assert_eq!(report["task_evaluations"]["items"][0]["stale"], true);
    assert_eq!(
        report["construction"]["unmeasured"]["agent_task_benefit"],
        Value::Null
    );
    let mut old = submission;
    old.request_id = "http-old-snapshot".into();
    assert_ne!(
        post(&f, "/task-evaluations", serde_json::to_value(old).unwrap())
            .await
            .0,
        StatusCode::OK
    );
}
