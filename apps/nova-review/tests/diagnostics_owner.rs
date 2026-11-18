use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31555";
const ROOT: &str = "/console/api/owner/intake/diagnostics";
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
        .unwrap();
    if enabled {
        server = server.with_native_workspace().unwrap();
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
async fn diagnostic_requires_native_owner_and_returns_redacted_observations_only() {
    let (_d, r, t, k) = setup(true);
    let (code, v) = call(&r, ROOT, None, &t, true).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(v["schema_version"], "nova.local-doctor.v0.1");
    assert_eq!(v["store"]["model_location"], "unconfigured");
    assert_eq!(v["runtime"]["worker_liveness"], "not_probed");
    assert_eq!(v["runtime"]["model_network_probe"], false);
    assert_eq!(v["repairs_performed"], false);
    assert!(!v.to_string().contains(&t));
    assert!(!v.to_string().contains(&k));
    assert_ne!(call(&r, ROOT, None, "wrong", true).await.0, StatusCode::OK);
    assert_ne!(call(&r, ROOT, None, &k, true).await.0, StatusCode::OK);
    assert_ne!(call(&r, ROOT, None, &t, false).await.0, StatusCode::OK);
    assert_eq!(
        call(&r, ROOT, Some(json!({})), &t, true).await.0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    let (_d, r, t, _k) = setup(false);
    assert_ne!(call(&r, ROOT, None, &t, true).await.0, StatusCode::OK);
}
