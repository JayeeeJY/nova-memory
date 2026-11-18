use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31444";
fn setup(native: bool) -> (tempfile::TempDir, axum::Router, String, String) {
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
    let scope = AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "test".into(),
    };
    let path = d.path().join("intake.sqlite");
    drop(IntakeStore::create(&path, k.clone(), scope).unwrap());
    let mut server = ReviewServer::new(db, key, HOST.into(), true)
        .with_owner("owner:synthetic".into(), "test".into())
        .with_owner_token(&owner)
        .unwrap()
        .with_intake_store(&path)
        .unwrap();
    if native {
        server = server.with_native_workspace().unwrap();
    }
    (d, server.router(), token, k.to_hex())
}
async fn raw(
    r: &axum::Router,
    path: &str,
    body: Option<Value>,
    token: &str,
    origin: &str,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Host", HOST)
        .header("X-Nova-Review", "owner-review")
        .header("Authorization", format!("Bearer {token}"));
    if body.is_some() {
        b = b
            .header("Origin", origin)
            .header("Content-Type", "application/json");
    }
    let out = r
        .clone()
        .oneshot(
            b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
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
    let (status, bytes) = raw(r, path, body, token, &format!("http://{HOST}")).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn input() -> Value {
    json!({"request_id":"synthetic-issue","label":"Synthetic Codex","host":"codex","preset":"read_only","topic_id":null,"include_future":true})
}
const ROOT: &str = "/console/api/owner/intake";
#[tokio::test]
async fn owner_can_shorten_connection_lifetime_but_invalid_or_unknown_fields_create_nothing() {
    let (_d, r, t, _k) = setup(true);
    for ttl in [
        json!(0),
        json!(86401),
        json!(-1),
        json!(1.5),
        json!("1200"),
        Value::Null,
    ] {
        let mut request = input();
        request["ttl_seconds"] = ttl;
        assert_ne!(
            call(&r, &format!("{ROOT}/connections"), Some(request), &t)
                .await
                .0,
            StatusCode::OK
        );
    }
    let mut request = input();
    request["ttl_seconds"] = json!(1200);
    request["unknown_lifetime"] = json!(10);
    assert_ne!(
        call(&r, &format!("{ROOT}/connections"), Some(request), &t)
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        call(&r, &format!("{ROOT}/connections"), None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut request = input();
    request["ttl_seconds"] = json!(1200);
    let (status, out) = call(&r, &format!("{ROOT}/connections"), Some(request), &t).await;
    assert_eq!(status, StatusCode::OK);
    let parse = |field: &str| {
        nova_time::OffsetDateTime::parse(
            out["connection"][field].as_str().unwrap(),
            &nova_time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    };
    assert_eq!(
        (parse("expires_at") - parse("created_at")).whole_seconds(),
        1200
    );
}
#[tokio::test]
async fn opt_in_serves_four_entries_and_preserves_legacy_default() {
    let (_d, r, t, _k) = setup(true);
    let (s, bytes) = raw(&r, "/app/", None, &t, "").await;
    assert_eq!(s, StatusCode::OK);
    let html = String::from_utf8(bytes).unwrap();
    for label in [
        "记忆空间",
        "记忆库",
        "AI 整理",
        "连接",
        "global-capture",
        "/native.js",
    ] {
        assert!(html.contains(label));
    }
    let session = call(&r, "/console/session", None, &t).await.1;
    assert_eq!(session["capabilities"]["native_workspace"], true);
    let (_d, r, t, _) = setup(false);
    let (_, bytes) = raw(&r, "/app/", None, &t, "").await;
    assert!(!String::from_utf8(bytes).unwrap().contains("/native.js"));
    assert_ne!(
        call(&r, &format!("{ROOT}/connections"), None, &t).await.0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn credential_is_shown_only_at_creation_never_master_or_owner_and_client_cannot_manage_policy()
 {
    let (_d, r, t, k) = setup(true);
    let (s, out) = call(&r, &format!("{ROOT}/connections"), Some(input()), &t).await;
    assert_eq!(s, StatusCode::OK);
    let auth = out["configuration"]["http"]["headers"]["Authorization"]
        .as_str()
        .unwrap();
    let client = auth.strip_prefix("Bearer ").unwrap();
    assert_ne!(client, t);
    assert_ne!(client, k);
    assert!(!out.to_string().contains(&t));
    assert!(!out.to_string().contains(&k));
    assert!(
        out["configuration"]["codex_toml"]
            .as_str()
            .unwrap()
            .contains("http_headers")
    );
    assert_eq!(out["connection"]["include_future"], true);
    let (s, list) = call(&r, &format!("{ROOT}/connections"), None, &t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains(client));
    assert_ne!(
        call(&r, &format!("{ROOT}/connections"), None, client)
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, &format!("{ROOT}/connections"), Some(input()), client)
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(
        call(&r, &format!("{ROOT}/connections"), Some(input()), &t)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&r, &format!("{ROOT}/connections"), None, &t).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let id = out["connection"]["id"].as_str().unwrap();
    let path = format!("{ROOT}/connections/{id}/revoke");
    assert_eq!(
        raw(&r, &path, Some(json!({})), &t, "http://evil.invalid")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&r, &path, Some(json!({})), &t).await.0, StatusCode::OK);
    assert_eq!(
        call(&r, &format!("{ROOT}/connections"), None, &t).await.1["items"][0]["revoked"],
        true
    );
}
#[tokio::test]
async fn no_implicit_future_or_spoofed_control_and_inventory_contains_actual_data() {
    let (_d, r, t, _) = setup(true);
    let mut none = input();
    none["include_future"] = json!(false);
    assert_eq!(
        call(&r, &format!("{ROOT}/connections"), Some(none), &t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut spoof = input();
    spoof["owner_id"] = json!("owner:other");
    assert_eq!(
        call(&r, &format!("{ROOT}/connections"), Some(spoof), &t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let capture = json!({"schema_version":"nova.intake.v0.1","request_id":"web","source_platform":"web-console","capture_fidelity":"f2_visible_transcript","content":"synthetic","occurred_at":null});
    assert_eq!(call(&r, ROOT, Some(capture), &t).await.0, StatusCode::OK);
    let (s, inventory) = call(&r, &format!("{ROOT}/overview"), None, &t).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(inventory["sources"], 1);
    assert_eq!(inventory["extraction_records"], 0);
    assert_eq!(inventory["evaluation"], "not_measured");
}

#[tokio::test]
async fn provider_description_is_owner_only_does_not_probe_or_promote_config_to_acceptance() {
    let (dir, r, owner, key) = setup(true);
    let path = format!("{ROOT}/model-capabilities");
    let (_, connection) = call(&r, &format!("{ROOT}/connections"), Some(input()), &owner).await;
    let bearer = connection["configuration"]["http"]["headers"]["Authorization"]
        .as_str()
        .unwrap();
    let client = bearer.strip_prefix("Bearer ").unwrap();
    assert_eq!(call(&r, &path, None, client).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&r, &path, None, &key).await.0, StatusCode::FORBIDDEN);
    let (status, result) = call(&r, &path, None, &owner).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["blocker"], "model_not_configured");
    let configuration = json!({"expected_revision":1,"selection":{"location":"local","model_id":"synthetic-selected","endpoint":"http://127.0.0.1:9/v1"}});
    assert_eq!(
        call(
            &r,
            &format!("{ROOT}/configuration"),
            Some(configuration),
            &owner
        )
        .await
        .0,
        StatusCode::OK
    );
    let result = call(&r, &path, None, &owner).await.1;
    assert_eq!(result["blocker"], "runtime_not_enabled");
    assert_eq!(result["observations"]["service_probed"], false);
    assert_eq!(result["configuration_revision"], 2);
    assert!(!result.to_string().contains("127.0.0.1"));
    assert!(!result.to_string().contains(&owner));
    assert!(!result.to_string().contains(&key));
    let scope = AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "test".into(),
    };
    let store = IntakeStore::open(
        &dir.path().join("intake.sqlite"),
        VaultKey::from_hex(&key).unwrap(),
        scope.clone(),
    )
    .unwrap();
    assert!(store.jobs(&scope, None).unwrap().is_empty());
}

#[tokio::test]
async fn six_platform_setup_preserves_identity_and_generates_only_scoped_configuration() {
    let (_d, r, owner, master) = setup(true);
    for host in ["chatgpt", "codex", "claude", "workbuddy", "grok", "cursor"] {
        let request = json!({"request_id":format!("issue-{host}"),"label":format!("Synthetic {host}"),"host":host,"preset":"capture_read","topic_id":null,"include_future":true});
        let (status, out) = call(&r, &format!("{ROOT}/connections"), Some(request), &owner).await;
        assert_eq!(status, StatusCode::OK, "{host}: {out}");
        assert_eq!(out["connection"]["host"], host);
        assert_eq!(
            out["configuration"]["contract"],
            "nova-memory-automatic/0.3.0"
        );
        assert_eq!(out["configuration"]["endpoint_visibility"], "loopback_only");
        assert_eq!(out["configuration"]["real_host_verification"], "not_run");
        assert_eq!(
            out["configuration"]["mcp_json"]["mcpServers"]["nova-memory"],
            out["configuration"]["http"]
        );
        let auth = out["configuration"]["http"]["headers"]["Authorization"]
            .as_str()
            .unwrap();
        assert!(!out.to_string().contains(&owner));
        assert!(!out.to_string().contains(&master));
        let listed = call(&r, &format!("{ROOT}/connections"), None, &owner)
            .await
            .1;
        assert!(
            !listed
                .to_string()
                .contains(auth.strip_prefix("Bearer ").unwrap())
        );
        assert_eq!(
            call(
                &r,
                &format!(
                    "{ROOT}/connections/{}/revoke",
                    out["connection"]["id"].as_str().unwrap()
                ),
                Some(json!({})),
                &owner
            )
            .await
            .0,
            StatusCode::OK
        );
    }
}
