use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{MigrationPackage, Vault, VaultKey};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    dir: tempfile::TempDir,
    router: axum::Router,
    token: String,
    key_path: std::path::PathBuf,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    Vault::create(&dir.path().join("vault.sqlite"), key).unwrap();
    let owner_path = dir.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner_path)
        .unwrap();
    let token = std::fs::read_to_string(&owner_path)
        .unwrap()
        .trim()
        .to_owned();
    let public = dir.path().join("public");
    std::fs::create_dir_all(public.join("console")).unwrap();
    std::fs::create_dir_all(public.join("app")).unwrap();
    std::fs::create_dir_all(public.join("demo/workspace")).unwrap();
    std::fs::write(public.join("index.html"), "Public site").unwrap();
    std::fs::write(public.join("console/index.html"), "Shared workspace").unwrap();
    std::fs::write(public.join("app/index.html"), "Shared workspace").unwrap();
    std::fs::write(
        public.join("demo/workspace/index.html"),
        "Unified synthetic demo",
    )
    .unwrap();
    std::fs::write(
        public.join("console/app.js"),
        "export const synthetic = true;",
    )
    .unwrap();
    std::fs::write(public.join("secret.key"), "not a public asset").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&owner_path, public.join("outside.js")).unwrap();
    let router = ReviewServer::new(
        dir.path().join("vault.sqlite"),
        key_path.clone(),
        "127.0.0.1:31410".into(),
        true,
    )
    .with_owner("owner:synthetic".into(), "personal".into())
    .with_owner_token(&owner_path)
    .unwrap()
    .with_site_dir(&public)
    .unwrap()
    .router();
    Fixture {
        dir,
        router,
        token,
        key_path,
    }
}
async fn request(
    f: &Fixture,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
    origin: Option<&str>,
    guard: bool,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("host", "127.0.0.1:31410");
    if guard {
        b = b.header("x-nova-review", "owner-review");
    }
    if let Some(token) = token {
        b = b.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        b = b.header("origin", origin);
    }
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let response = f
        .router
        .clone()
        .oneshot(
            b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    (
        status,
        to_bytes(response.into_body(), 10_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
}
async fn api(f: &Fixture, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (status, bytes) = request(
        f,
        &format!("/console/api/owner/{path}"),
        body,
        Some(&f.token),
        Some("http://127.0.0.1:31410"),
        true,
    )
    .await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn shared_ui_keeps_owner_and_static_file_boundaries() {
    let f = fixture();
    let (status, bytes) = request(&f, "/app/", None, None, None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"Shared workspace");
    for path in ["/demo/workspace", "/demo/workspace/"] {
        let (status, bytes) = request(&f, path, None, None, None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, b"Unified synthetic demo");
    }
    for path in ["/vault", "/vault/", "/demo/vault", "/demo/vault/"] {
        let response = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:31410")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(response.headers()["location"], "/demo/workspace/");
    }
    for path in [
        "/console/api/owner/unknown",
        "/api/unknown",
        "/demo/not-implemented/",
    ] {
        assert_eq!(
            request(&f, path, None, Some(&f.token), None, true).await.0,
            StatusCode::NOT_FOUND
        );
    }
    let (status, bytes) = request(&f, "/session", None, None, None, false).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(session["mode"], "local");
    assert_eq!(session["requires_owner_token"], true);
    assert_eq!(session["capabilities"]["cloud_account"], false);
    for path in [
        "/secret.key",
        "/outside.js",
        "/../owner.key",
        "/console/%2e%2e/%2e%2e/owner.key",
    ] {
        assert_eq!(
            request(&f, path, None, None, None, false).await.0,
            StatusCode::NOT_FOUND
        );
    }
    for (token, origin, guard) in [
        (None, None, true),
        (Some("client-key"), None, true),
        (Some(f.token.as_str()), Some("https://evil.example"), true),
        (Some(f.token.as_str()), None, false),
    ] {
        assert_eq!(
            request(&f, "/console/api/owner/inbox", None, token, origin, guard)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(api(&f, "clients", None).await.1["configured"], false);
}

#[tokio::test]
async fn structure_capture_review_versions_and_export_use_shared_local_contract() {
    let f = fixture();
    let field = json!({"id":"constraint","label":"Constraint","kind":"text","required":true,"source_required":true,"max_length":120});
    let template = json!({"template_id":null,"expected_revision":null,"title":"Decision record","memory_type":"decision","privacy_level":"private","guidance":"Keep the source quote","fields":[field],"user_approved":true});
    let (status, t) = api(&f, "templates", Some(template)).await;
    assert_eq!(status, StatusCode::OK);
    let values = json!({"constraint":"Local first"});
    let evidence = json!({"constraint":"Local first"});
    let (status, result) = api(&f,"templates/validate",Some(json!({"template":{"title":"Sample","memory_type":"decision","privacy_level":"private","guidance":"","fields":[field]},"source_content":"Local first","values":values,"evidence":evidence}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["valid"], true);
    let capture = json!({"request_id":"synthetic-capture","title":"Keep data local","content":"Local first","summary":"Keep the asset local","memory_type":"decision","privacy_level":"private","user_approved":true,"structure":{"template_id":t["template_id"],"template_revision":1,"values":values,"evidence":evidence}});
    assert_eq!(api(&f, "capture", Some(capture)).await.0, StatusCode::OK);
    assert!(
        api(&f, "assets", None).await.1["memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let inbox = api(&f, "inbox", None).await.1;
    let item = &inbox["items"][0];
    let mut review = json!({"request_id":"synthetic-review","proposal_id":item["proposal"]["proposal_id"],"expected_snapshot":item["snapshot"],"action":{"kind":"confirm"},"user_approved":false});
    assert_eq!(
        api(&f, "review", Some(review.clone())).await.0,
        StatusCode::FORBIDDEN
    );
    review["user_approved"] = json!(true);
    assert_eq!(api(&f, "review", Some(review)).await.0, StatusCode::OK);
    let memories = api(&f, "assets", None).await.1;
    assert_eq!(
        memories["memories"][0]["structure"]["template"]["revision"],
        1
    );
    assert_eq!(
        memories["memories"][0]["structure"]["values"]["constraint"],
        "Local first"
    );
    let (status, versions) = api(
        &f,
        &format!("templates/{}/versions", t["template_id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(versions["versions"].as_array().unwrap().len(), 1);
    let root_key = VaultKey::read_file(&f.key_path).unwrap();
    assert_ne!(
        api(
            &f,
            "export",
            Some(json!({"package_key":root_key.to_hex(),"user_approved":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let package_key = VaultKey::generate().unwrap();
    let (status, bytes) = request(
        &f,
        "/console/api/owner/export",
        Some(json!({"package_key":package_key.to_hex(),"user_approved":true})),
        Some(&f.token),
        Some("http://127.0.0.1:31410"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    MigrationPackage::inspect_encrypted(&bytes, &package_key, "owner:synthetic").unwrap();
    let backup_dir = f.dir.path().join("backup");
    let (status, backup) = api(
        &f,
        "backup",
        Some(json!({"directory":backup_dir,"user_approved":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        VaultKey::read_file(std::path::Path::new(
            backup["package_key_file"].as_str().unwrap()
        ))
        .unwrap()
        .to_hex(),
        root_key.to_hex()
    );
    let target = f.dir.path().join("restored");
    let (status,restore) = api(&f,"restore",Some(json!({"package":backup["package"],"package_key_file":backup["package_key_file"],"target_directory":target,"user_approved":true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restore["live_vault_switched"], false);
}
