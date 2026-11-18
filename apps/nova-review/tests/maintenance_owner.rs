use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31555";
const ROOT: &str = "/console/api/owner/intake/maintenance";
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
async fn owner_maintenance_is_opt_in_authenticated_bounded_and_cancelable() {
    let (_d, r, t, _k) = setup(false);
    assert_ne!(
        call(&r, &format!("{ROOT}/preview"), Some(json!({})), &t, true)
            .await
            .0,
        StatusCode::OK
    );
    let (_d, r, t, _k) = setup(true);
    let preview = format!("{ROOT}/preview");
    let jobs = format!("{ROOT}/jobs");
    assert_eq!(
        call(&r, &preview, Some(json!({})), &t, true).await.0,
        StatusCode::OK
    );
    assert!(
        call(&r, &jobs, None, &t, true).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_ne!(
        call(&r, &preview, Some(json!({})), "wrong", true).await.0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, &preview, Some(json!({})), &t, false).await.0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, &preview, Some(json!({"delete_sources":true})), &t, true)
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(call(&r,&preview,Some(json!({"policy":{"max_rebuild_pages":1000,"max_prune_pages":16,"derived_ttl_seconds":86400,"prune_before":null}})),&t,true).await.0,StatusCode::OK);
    let (status, job) = call(
        &r,
        &jobs,
        Some(json!({"request_id":"cancel-one"})),
        &t,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = job["id"].as_str().unwrap();
    assert_eq!(
        call(
            &r,
            &format!("{jobs}/{id}/cancel"),
            Some(json!({})),
            &t,
            true
        )
        .await
        .1["state"],
        "cancelled"
    );
    assert_eq!(
        call(&r, &format!("{jobs}/{id}/run"), Some(json!({})), &t, true)
            .await
            .1["state"],
        "cancelled"
    );
    let job = call(&r, &jobs, Some(json!({"request_id":"run-one"})), &t, true)
        .await
        .1;
    let id = job["id"].as_str().unwrap();
    assert_ne!(
        call(
            &r,
            &format!("{jobs}/{id}/run"),
            Some(json!({"preview":{"rebuild":[]}})),
            &t,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    let done = call(&r, &format!("{jobs}/{id}/run"), Some(json!({})), &t, true)
        .await
        .1;
    assert_eq!(done["state"], "completed");
    assert_eq!(done["preview"]["model_calls"], 0);
    assert_eq!(done["preview"]["originals_deleted"], 0);
    let again = call(&r, &format!("{jobs}/{id}/run"), Some(json!({})), &t, true)
        .await
        .1;
    assert_eq!(again, done);
    assert_eq!(
        call(&r, &jobs, None, &t, true).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
