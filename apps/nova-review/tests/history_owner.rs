use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31555";
const ROOT: &str = "/console/api/owner/intake/connections";
fn setup(enabled: bool) -> (tempfile::TempDir, axum::Router, String, String) {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("legacy.sqlite");
    let keyfile = d.path().join("vault.key");
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&keyfile).unwrap();
    drop(Vault::create(&db, k.clone()).unwrap());
    let owner = d.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner)
        .unwrap();
    let t = std::fs::read_to_string(&owner).unwrap().trim().to_owned();
    let intake = d.path().join("intake.sqlite");
    drop(
        IntakeStore::create(
            &intake,
            k.clone(),
            AssetScope {
                owner_id: "owner:fixture".into(),
                space: "test".into(),
            },
        )
        .unwrap(),
    );
    let mut server = ReviewServer::new(db, keyfile, HOST.into(), true)
        .with_owner("owner:fixture".into(), "test".into())
        .with_owner_token(&owner)
        .unwrap()
        .with_intake_store(&intake)
        .unwrap()
        .with_native_workspace()
        .unwrap();
    if enabled {
        server = server.with_historical_retrieval().unwrap();
    }
    (d, server.router(), t, k.to_hex())
}
async fn call(
    r: &axum::Router,
    path: &str,
    body: Option<Value>,
    token: &str,
    owner: bool,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Host", HOST)
        .header("Authorization", format!("Bearer {token}"));
    if owner {
        b = b.header("X-Nova-Review", "owner-review");
        if body.is_some() {
            b = b.header("Origin", format!("http://{HOST}"));
        }
    }
    let res = r
        .clone()
        .oneshot(
            b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    assert_eq!(res.headers()["cache-control"], "no-store");
    let bytes = to_bytes(res.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(json!({})))
}
#[tokio::test]
async fn history_permission_is_owner_only_cas_and_opt_in() {
    let request = json!({"request_id":"c","label":"Fixture","host":"codex","preset":"read_only","topic_id":null,"include_future":true});
    let (_d, r, t, _k) = setup(false);
    let issued = call(&r, ROOT, Some(request.clone()), &t, true).await.1;
    let path = format!(
        "{ROOT}/{}/history-policy",
        issued["connection"]["id"].as_str().unwrap()
    );
    assert_ne!(call(&r, &path, None, &t, true).await.0, StatusCode::OK);
    let (_d, r, t, _k) = setup(true);
    let issued = call(&r, ROOT, Some(request), &t, true).await.1;
    let client = issued["configuration"]["http"]["headers"]["Authorization"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    let path = format!(
        "{ROOT}/{}/history-policy",
        issued["connection"]["id"].as_str().unwrap()
    );
    assert!(call(&r, &path, None, &t, true).await.1["earliest_recorded_at"].is_null());
    let policy = json!({"expected_revision":0,"earliest_recorded_at":"2000-01-01T00:00:00Z"});
    assert_eq!(
        call(&r, &path, Some(policy.clone()), client, true).await.0,
        StatusCode::FORBIDDEN
    );
    let result = call(&r, &path, Some(policy.clone()), &t, true).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["revision"], 1);
    assert_ne!(
        call(&r, &path, Some(policy), &t, true).await.0,
        StatusCode::OK
    );
    let result = call(
        &r,
        &path,
        Some(json!({"expected_revision":1,"earliest_recorded_at":null})),
        &t,
        true,
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert!(result.1["earliest_recorded_at"].is_null());
}
