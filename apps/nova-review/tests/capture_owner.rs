use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31555";
const ROOT: &str = "/console/api/owner/intake/capture-grants";
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
        server = server.with_capture_ingress().unwrap();
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
fn request() -> Value {
    json!({"request_id":"new-fixture","source_namespace":"owner-fixture","host":"codex","surface":"cli_hooks","host_version":"synthetic","events":["user_prompt","assistant_response"],"lifetime_seconds":600,"event_budget":10,"byte_budget":4096})
}
#[tokio::test]
async fn owner_issues_capture_only_credential_and_default_mode_stays_disabled() {
    let (_d, r, t, _k) = setup(false);
    assert_eq!(call(&r, "/console/api/owner/intake/connections", None, &t, true).await.1["capabilities"]["capture_ingress"], false);
    assert_ne!(
        call(&r, ROOT, Some(request()), &t, true).await.0,
        StatusCode::OK
    );
    let (_d, r, t, k) = setup(true);
    assert_eq!(call(&r, "/console/api/owner/intake/connections", None, &t, true).await.1["capabilities"]["capture_ingress"], true);
    let (status, issued) = call(&r, ROOT, Some(request()), &t, true).await;
    assert_eq!(status, StatusCode::OK);
    let token = issued["credential"].as_str().unwrap();
    assert_ne!(token, t);
    assert_ne!(token, k);
    assert_eq!(issued["capabilities"]["read"], false);
    assert_eq!(issued["installed"], false);
    let listing = call(&r, ROOT, None, &t, true).await;
    assert_eq!(listing.0, StatusCode::OK);
    assert!(!listing.1.to_string().contains(token));
    assert_eq!(listing.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(&r, ROOT, Some(request()), token, true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_ne!(
        call(&r, ROOT, Some(request()), &t, true).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &r,
            "/capture/v01/session",
            Some(json!({"origin_session_id":"s1"})),
            &t,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let session = call(
        &r,
        "/capture/v01/session",
        Some(json!({"origin_session_id":"s1"})),
        token,
        false,
    )
    .await;
    assert_eq!(session.0, StatusCode::OK);
    let ack=call(&r,"/capture/v01/events",Some(json!({"schema_version":"nova.capture.v0.1","session_id":session.1["session_id"],"event_id":"e1","sequence":1,"kind":"assistant_response","content":"SYNTHETIC assistant suggestion","occurred_at":null,"loss":{"truncated":false,"redacted":false,"partial_history":true}})),token,false).await;
    assert_eq!(ack.0, StatusCode::OK);
    assert_eq!(ack.1["receipt"]["status"], "received");
    let path = format!(
        "{ROOT}/{}/revoke",
        issued["grant"]["grant_id"].as_str().unwrap()
    );
    assert_eq!(
        call(&r, &path, Some(json!({})), &t, true).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &r,
            "/capture/v01/session",
            Some(json!({"origin_session_id":"s1"})),
            token,
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}
