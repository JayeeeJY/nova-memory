use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::*;
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
fn setup() -> (tempfile::TempDir, axum::Router, String) {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("vault.sqlite");
    let key = d.path().join("vault.key");
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&key).unwrap();
    drop(Vault::create(&db, k).unwrap());
    let owner = d.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner)
        .unwrap();
    let registry = d.path().join("clients.json");
    std::fs::write(
        &registry,
        json!({"schema_version":"nova.clients.v0.1","clients":[]}).to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let router = ReviewServer::new(db, key, "127.0.0.1:31410".into(), false)
        .with_owner("owner:alice".into(), "work".into())
        .with_registry(registry)
        .with_owner_token(&owner)
        .unwrap()
        .router();
    let token = std::fs::read_to_string(owner).unwrap();
    (d, router, token.trim().into())
}
async fn call(
    r: &axum::Router,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("host", "127.0.0.1:31410")
        .header("x-nova-review", "owner-review")
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        b = b
            .header("origin", "http://127.0.0.1:31410")
            .header("content-type", "application/json");
    }
    let out = r
        .clone()
        .oneshot(
            b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = out.status();
    let v = serde_json::from_slice(&to_bytes(out.into_body(), 1_000_000).await.unwrap()).unwrap();
    (status, v)
}
#[tokio::test]
async fn owner_only_web_capture_review_registry_collection_and_recovery() {
    let (d, r, t) = setup();
    assert_eq!(
        call(&r, "/api/assets", None, "client-not-owner").await.0,
        StatusCode::FORBIDDEN
    );
    let capture = json!({"request_id":"web-save","title":"Synthetic M1","content":"Synthetic web source","privacy":"private","acknowledge":true});
    let (s, a) = call(&r, "/api/capture", Some(capture.clone()), &t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["source"]["capture_channel"], "web_adapter");
    assert_eq!(a["source"]["capture_fidelity"], "f2_visible_transcript");
    assert_eq!(
        call(&r, "/api/capture", Some(capture), &t).await.1["receipt"],
        a["receipt"]
    );
    assert!(
        call(&r, "/api/assets", None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let rows = call(&r, "/api/inbox", None, &t).await.1;
    let p = &rows["items"][0];
    assert_eq!(call(&r,"/api/review",Some(json!({"request_id":"owner-confirm","proposal_id":p["proposal"]["proposal_id"],"expected_snapshot":p["snapshot"],"action":{"kind":"confirm"}})),&t).await.0,StatusCode::OK);
    let data = call(&r, "/api/assets", None, &t).await.1;
    let mid = data["items"][0]["memory_id"].as_str().unwrap();
    assert_eq!(
        call(&r, &format!("/api/assets/{mid}"), None, &t).await.1["sources"][0]["content"],
        "Synthetic web source"
    );
    let(s,client)=call(&r,"/api/clients",Some(json!({"client_id":"client-b","operation":"register","capabilities":["query"],"ttl_seconds":600,"acknowledge":true})),&t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        call(
            &r,
            "/api/review",
            Some(json!({})),
            client["token"].as_str().unwrap()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let(s,c)=call(&r,"/api/collections",Some(json!({"collection_id":null,"expected_revision":null,"title":"Work","member_ids":[mid],"acknowledge":true})),&t).await;
    assert_eq!(s, StatusCode::OK);
    let(s,g)=call(&r,"/api/collection-grants",Some(json!({"acknowledge":true,"request":{"collection_id":c["collection_id"],"client":"client-b","purpose":"personal_memory_continuity","ttl_seconds":600,"per_call_limit":2,"call_quota":2,"maximum_privacy":"private"}})),&t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        call(
            &r,
            "/api/access/revoke",
            Some(json!({"approval_id":g["approval_id"],"acknowledge":true})),
            &t
        )
        .await
        .0,
        StatusCode::OK
    );
    let backup = d.path().join("backup");
    let (s, b) = call(
        &r,
        "/api/backup",
        Some(json!({"directory":backup,"acknowledge":true})),
        &t,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let target = d.path().join("restored");
    let payload = json!({"package":b["package"],"package_key_file":b["package_key_file"],"target_directory":target,"acknowledge":true});
    assert_eq!(
        call(&r, "/api/restore", Some(payload.clone()), &t).await.0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, "/api/restore", Some(payload), &t).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&r, "/api/assets", None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
