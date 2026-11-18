use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::*, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31411";
fn setup(enabled: bool) -> (tempfile::TempDir, axum::Router, String) {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("legacy.sqlite");
    let key = d.path().join("vault.key");
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&key).unwrap();
    drop(Vault::create(&db, k.clone()).unwrap());
    let owner = d.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner)
        .unwrap();
    let token = std::fs::read_to_string(&owner).unwrap().trim().to_string();
    let mut server = ReviewServer::new(db, key, HOST.into(), false)
        .with_owner("owner:test".into(), "private".into())
        .with_owner_token(&owner)
        .unwrap();
    if enabled {
        let path = d.path().join("intake.sqlite");
        drop(
            IntakeStore::create(
                &path,
                k,
                AssetScope {
                    owner_id: "owner:test".into(),
                    space: "private".into(),
                },
            )
            .unwrap(),
        );
        server = server.with_intake_store(&path).unwrap();
    }
    (d, server.router(), token)
}
async fn raw(
    r: &axum::Router,
    path: &str,
    body: Option<String>,
    token: &str,
    origin: &str,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("host", HOST)
        .header("x-nova-review", "owner-review")
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        b = b
            .header("origin", origin)
            .header("content-type", "application/json");
    }
    let out = r
        .clone()
        .oneshot(b.body(Body::from(body.unwrap_or_default())).unwrap())
        .await
        .unwrap();
    (
        out.status(),
        to_bytes(out.into_body(), 2_000_000).await.unwrap().to_vec(),
    )
}
async fn call(
    r: &axum::Router,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, Value) {
    let (s, b) = raw(
        r,
        path,
        body.map(|b| b.to_string()),
        token,
        &format!("http://{HOST}"),
    )
    .await;
    (s, serde_json::from_slice(&b).unwrap())
}
fn input() -> Value {
    json!({"schema_version":"nova.intake.v0.1","request_id":"web-test","source_platform":"web-console","capture_fidelity":"f2_visible_transcript","content":"synthetic UI original 中文😀","occurred_at":null})
}
const ROOT: &str = "/console/api/owner/intake";
#[tokio::test]
async fn backup_copy_is_inspectable_but_cannot_restore_ai_authority() {
    let (d, r, t) = setup(true);
    call(&r, ROOT, Some(input()), &t).await;
    let (status, bytes) = raw(
        &r,
        &format!("{ROOT}/backup"),
        Some("{}".into()),
        &t,
        &format!("http://{HOST}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let generation = call(&r, &format!("{ROOT}/backup/status"), None, &t).await.1;
    assert!(
        !generation["last_generation"]["backup_id"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert!(generation["recovery"].is_null());
    assert_eq!(generation["external_copy_verified"], false);
    let path = d.path().join("backup.sqlite");
    std::fs::write(&path, bytes).unwrap();
    // Start inspection with the same synthetic key, without touching the original store.
    let copy = ReviewServer::new(
        d.path().join("legacy.sqlite"),
        d.path().join("vault.key"),
        HOST.into(),
        false,
    )
    .with_owner("owner:test".into(), "private".into())
    .with_owner_token(&d.path().join("owner.key"))
    .unwrap()
    .with_intake_store(&path)
    .unwrap()
    .with_native_workspace()
    .unwrap()
    .router();
    let cfg = call(&copy, &format!("{ROOT}/configuration"), None, &t)
        .await
        .1;
    assert_eq!(cfg["backup_quarantined"], true);
    assert_eq!(cfg["compiler_available"], false);
    assert_eq!(
        call(&copy, &format!("{ROOT}/jobs"), None, &t).await.1["processing_blocker"],
        "backup_quarantined"
    );
    let svc = call(&copy, &format!("{ROOT}/connections"), None, &t)
        .await
        .1;
    assert_eq!(svc["service_ready"], false);
    assert_eq!(svc["capabilities"]["capture_ingress"], false);
    let new_connection = json!({"request_id":"reopen","label":"test","host":"codex","preset":"read_only","topic_id":null,"include_future":true});
    assert_ne!(
        call(
            &copy,
            &format!("{ROOT}/connections"),
            Some(new_connection),
            &t
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &copy,
            &format!("{ROOT}/backup/verify"),
            Some(json!({})),
            "wrong"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw(
            &copy,
            &format!("{ROOT}/backup/verify"),
            Some("{}".into()),
            &t,
            "https://elsewhere.test"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &copy,
            &format!("{ROOT}/backup/verify"),
            Some(json!({"activate":true})),
            &t
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // This N1 backup lacked native tables. Native inspection must not add encrypted state.
    let (status, checked) =
        call(&copy, &format!("{ROOT}/backup/verify"), Some(json!({})), &t).await;
    assert_eq!(status, StatusCode::OK, "{checked}");
    assert_eq!(checked["may_automate"], false);
    assert_eq!(checked["off_device_restore_verified"], false);
    assert_eq!(
        call(&copy, &format!("{ROOT}/backup/status"), None, &t)
            .await
            .1["recovery"]["quarantined"],
        true
    );
}
#[tokio::test]
async fn owner_one_click_receipt_status_and_backup_without_legacy_confirmation() {
    let (d, r, t) = setup(true);
    let (s, a) = call(&r, ROOT, Some(input()), &t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["saved"], true);
    assert_eq!(a["receipt"]["status"], "received");
    assert_eq!(a, call(&r, ROOT, Some(input()), &t).await.1);
    let rows = call(&r, &format!("{ROOT}/jobs"), None, &t).await.1;
    assert_eq!(rows["items"].as_array().unwrap().len(), 1);
    assert_eq!(rows["items"][0]["job"]["state"], "received");
    assert_eq!(rows["processing_blocker"], "model_unconfigured");
    let sid = a["receipt"]["source"]["object_id"].as_str().unwrap();
    let source = call(&r, &format!("{ROOT}/sources/{sid}"), None, &t).await.1;
    assert_eq!(source["content"], input()["content"]);
    assert_eq!(source["scope"]["owner_id"], "owner:test");
    assert_eq!(source["privacy_level"], "private");
    assert_eq!(
        call(&r, "/api/inbox", None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        call(&r, "/api/assets", None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let (s, backup) = raw(
        &r,
        &format!("{ROOT}/backup"),
        Some("{}".into()),
        &t,
        &format!("http://{HOST}"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let path = d.path().join("restored.sqlite");
    std::fs::write(&path, backup).unwrap();
    let store = IntakeStore::open(
        &path,
        VaultKey::read_file(&d.path().join("vault.key")).unwrap(),
        AssetScope {
            owner_id: "owner:test".into(),
            space: "private".into(),
        },
    )
    .unwrap();
    assert_eq!(
        store
            .jobs(
                &AssetScope {
                    owner_id: "owner:test".into(),
                    space: "private".into()
                },
                None
            )
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn wrong_credentials_origin_untrusted_authority_and_duplicate_keys_are_denied() {
    let (_d, r, t) = setup(true);
    assert_eq!(
        call(&r, ROOT, Some(input()), "client-token").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw(
            &r,
            ROOT,
            Some(input().to_string()),
            &t,
            "https://evil.example"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let duplicate = input()
        .to_string()
        .replacen("{", "{\"content\":\"SENSITIVE_MARKER\",", 1);
    let (s, b) = raw(&r, ROOT, Some(duplicate), &t, &format!("http://{HOST}")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8(b).unwrap().contains("SENSITIVE_MARKER"));
    for field in ["owner_id", "actor_id", "privacy_level", "approval_id"] {
        let mut i = input();
        i[field] = json!("forged");
        assert_eq!(call(&r, ROOT, Some(i), &t).await.0, StatusCode::BAD_REQUEST);
    }
    let mut i = input();
    i["source_platform"] = json!("other");
    assert_eq!(call(&r, ROOT, Some(i), &t).await.0, StatusCode::BAD_REQUEST);
    let mut i = input();
    i["content"] = json!("x".repeat(100_000));
    assert_eq!(
        raw(&r, ROOT, Some(i.to_string()), &t, &format!("http://{HOST}"))
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(
        call(&r, &format!("{ROOT}/jobs"), None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn configuration_is_cas_bound_and_does_not_start_compilation_or_cloud_calls() {
    let (_d, r, t) = setup(true);
    call(&r, ROOT, Some(input()), &t).await;
    let cfg = format!("{ROOT}/configuration");
    let body = json!({"expected_revision":1,"selection":{"location":"remote","model_id":"synthetic","endpoint":"https://api.example.test/v1","allow_selected_source":false}});
    assert_eq!(
        call(&r, &cfg, Some(body), &t).await.0,
        StatusCode::BAD_REQUEST
    );
    let body = json!({"expected_revision":1,"selection":{"location":"local","model_id":"synthetic","endpoint":"http://127.0.0.1:11434/v1"}});
    assert_eq!(
        call(&r, &cfg, Some(body.clone()), &t).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&r, &cfg, Some(body), &t).await.0, StatusCode::CONFLICT);
    let cfgdata = call(&r, &cfg, None, &t).await.1;
    assert_eq!(cfgdata["network_calls"], false);
    assert_eq!(cfgdata["temporary_demo"], false);
    assert_eq!(cfgdata["compiler_available"], false);
    let rows = call(&r, &format!("{ROOT}/jobs"), None, &t).await.1;
    assert_eq!(rows["processing_blocker"], "compiler_unavailable");
    assert_eq!(rows["items"][0]["job"]["state"], "received");
    let duplicate =
        r#"{"expected_revision":2,"expected_revision":2,"selection":{"location":"unconfigured"}}"#;
    assert_eq!(
        raw(
            &r,
            &cfg,
            Some(duplicate.into()),
            &t,
            &format!("http://{HOST}")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn disabled_runtime_never_initializes_an_intake_store() {
    let (d, r, t) = setup(false);
    assert_eq!(
        call(&r, ROOT, Some(input()), &t).await.0,
        StatusCode::CONFLICT
    );
    assert!(!d.path().join("intake.sqlite").exists());
    let info = call(&r, "/console/session", None, &t).await.1;
    assert_eq!(info["capabilities"]["automatic_intake"], false);
}
#[tokio::test]
async fn n2b_owner_only_wiki_withdraw_delete_and_replay_fences() {
    let (_d, r, t) = setup(true);
    let (_, a) = call(&r, ROOT, Some(input()), &t).await;
    let sid = a["receipt"]["source"]["object_id"].as_str().unwrap();
    for path in [format!("{ROOT}/wiki"), format!("{ROOT}/withdrawals")] {
        assert_eq!(call(&r, &path, None, &t).await.0, StatusCode::OK);
        assert_ne!(call(&r, &path, None, "wrong-token").await.0, StatusCode::OK);
    }
    let path = format!("{ROOT}/sources/{sid}/withdraw");
    assert_ne!(
        raw(&r, &path, Some("{}".into()), &t, "https://evil.example")
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, &path, Some(json!({"extra":true})), &t).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&r, &path, Some(json!({})), &t).await.0, StatusCode::OK);
    assert_ne!(
        call(&r, &format!("{ROOT}/sources/{sid}"), None, &t).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&r, &format!("{ROOT}/withdrawals"), None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let path = format!("{ROOT}/sources/{sid}/delete");
    assert_ne!(
        call(&r, &path, Some(json!({})), "wrong-token").await.0,
        StatusCode::OK
    );
    assert_eq!(call(&r, &path, Some(json!({})), &t).await.0, StatusCode::OK);
    assert_ne!(call(&r, ROOT, Some(input()), &t).await.0, StatusCode::OK);
    assert!(
        call(&r, &format!("{ROOT}/jobs"), None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
