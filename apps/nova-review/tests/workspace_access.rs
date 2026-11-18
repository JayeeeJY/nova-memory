use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::IntakeStore};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31901";
struct Fixture {
    dir: tempfile::TempDir,
    token: String,
    vault_key: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let vault_key = key.to_hex();
        key.write_new_file(&dir.path().join("vault.key")).unwrap();
        drop(Vault::create(&dir.path().join("legacy.sqlite"), key.clone()).unwrap());
        drop(
            IntakeStore::create(
                &dir.path().join("intake.sqlite"),
                key,
                AssetScope {
                    owner_id: "owner:synthetic".into(),
                    space: "test".into(),
                },
            )
            .unwrap(),
        );
        let owner = VaultKey::generate().unwrap();
        owner.write_new_file(&dir.path().join("owner.key")).unwrap();
        Self {
            dir,
            token: owner.to_hex(),
            vault_key,
        }
    }
    fn server(&self) -> Result<ReviewServer, nova_core::NovaError> {
        ReviewServer::new(
            self.dir.path().join("legacy.sqlite"),
            self.dir.path().join("vault.key"),
            HOST.into(),
            false,
        )
        .with_owner("owner:synthetic".into(), "test".into())
        .with_owner_token(&self.dir.path().join("owner.key"))?
        .with_intake_store(&self.dir.path().join("intake.sqlite"))?
        .with_native_workspace()
    }
    fn router(&self) -> Router {
        self.server().unwrap().router()
    }
}
fn request(
    path: &str,
    body: Option<Value>,
    auth: Option<&str>,
    cookie: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Host", HOST)
        .header("X-Nova-Review", "owner-review");
    if body.is_some() {
        builder = builder
            .header("Origin", format!("http://{HOST}"))
            .header("Content-Type", "application/json");
    }
    if let Some(auth) = auth {
        builder = builder.header("Authorization", format!("Bearer {auth}"));
    }
    if let Some(cookie) = cookie {
        builder = builder.header("Cookie", cookie);
    }
    builder
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap()
}
async fn send(router: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn bootstrap(router: &Router) -> String {
    let (status, headers, body) = send(
        router,
        request("/console/local-session", Some(json!({})), None, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"opened":true}));
    let cookie = headers["set-cookie"].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Path=/console/")
    );
    cookie.split(';').next().unwrap().into()
}
const SETTINGS: &str = "/console/api/owner/intake/workspace-access";
#[tokio::test]
async fn default_browser_entry_never_returns_owner_or_vault_secret_or_authorizes_mcp() {
    let f = Fixture::new();
    let r = f.router();
    let session = send(&r, request("/console/session", None, None, None))
        .await
        .2;
    assert_eq!(session["credential_unlock_enabled"], false);
    assert_eq!(session["requires_owner_token"], true);
    let cookie = bootstrap(&r).await;
    assert!(!cookie.contains(&f.token) && !cookie.contains(&f.vault_key));
    let (status, _, body) = send(&r, request(SETTINGS, None, None, Some(&cookie))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["credential_unlock_enabled"], false);
    let serialized = body.to_string();
    assert!(!serialized.contains(&f.token) && !serialized.contains(&f.vault_key));
    assert_eq!(
        send(&r, request(SETTINGS, None, None, None)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&r, request(SETTINGS, None, Some("wrong"), Some(&cookie)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&r, request("/mcp/v03", None, None, Some(&cookie)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let restarted = f.router();
    assert_eq!(
        send(&restarted, request(SETTINGS, None, None, Some(&cookie)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn browser_entry_retains_origin_host_custom_header_and_cookie_boundaries() {
    let f = Fixture::new();
    let r = f.router();
    let cookie = bootstrap(&r).await;
    for path in ["/console/local-session", SETTINGS] {
        for change in ["origin", "host", "site", "guard"] {
            let mut q = request(path, Some(json!({})), None, Some(&cookie));
            match change {
                "origin" => {
                    q.headers_mut()
                        .insert("Origin", "http://evil.invalid".parse().unwrap());
                }
                "host" => {
                    q.headers_mut()
                        .insert("Host", "evil.invalid".parse().unwrap());
                }
                "site" => {
                    q.headers_mut()
                        .insert("Sec-Fetch-Site", "cross-site".parse().unwrap());
                }
                _ => {
                    q.headers_mut().remove("X-Nova-Review");
                }
            }
            assert_eq!(send(&r, q).await.0, StatusCode::FORBIDDEN);
        }
    }
    let duplicate = format!("{cookie}; {cookie}");
    assert_eq!(
        send(&r, request(SETTINGS, None, None, Some(&duplicate)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&r, request("/api/assets", None, None, Some(&cookie)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn opting_in_requires_owner_then_persists_and_revokes_all_old_browser_sessions() {
    let f = Fixture::new();
    let r = f.router();
    let cookie = bootstrap(&r).await;
    for credential in [None, Some("wrong"), Some(f.vault_key.as_str())] {
        let mut body = json!({"expected_revision":1,"credential_unlock_enabled":true});
        if let Some(value) = credential {
            body["owner_credential"] = json!(value);
        }
        assert_eq!(
            send(&r, request(SETTINGS, Some(body), None, Some(&cookie)))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let body =
        json!({"expected_revision":1,"credential_unlock_enabled":true,"owner_credential":f.token});
    let out = send(&r, request(SETTINGS, Some(body), None, Some(&cookie))).await;
    assert_eq!(out.0, StatusCode::OK);
    assert_eq!(out.2["settings"]["revision"], 2);
    assert_eq!(
        send(&r, request(SETTINGS, None, None, Some(&cookie)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &r,
            request("/console/local-session", Some(json!({})), None, None)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let restarted = f.router();
    assert_eq!(
        send(&restarted, request("/console/session", None, None, None))
            .await
            .2["credential_unlock_enabled"],
        true
    );
    assert_eq!(
        send(&restarted, request(SETTINGS, None, Some(&f.token), None))
            .await
            .0,
        StatusCode::OK
    );
    let saved = f.dir.path().join("intake.sqlite.workspace-access.json");
    let saved_text = std::fs::read_to_string(&saved).unwrap();
    assert!(!saved_text.contains(&f.token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(saved).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Disable with an authenticated Owner; cookies from before the lock do not revive.
    assert_eq!(
        send(
            &r,
            request(
                SETTINGS,
                Some(json!({"expected_revision":2,"credential_unlock_enabled":false})),
                Some(&f.token),
                None
            )
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&r, request(SETTINGS, None, None, Some(&cookie)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let fresh = bootstrap(&r).await;
    assert_ne!(fresh, cookie);
    assert_eq!(
        send(&r, request(SETTINGS, None, None, Some(&fresh)))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(
            &r,
            request(
                SETTINGS,
                Some(json!({"expected_revision":1,"credential_unlock_enabled":true})),
                Some(&f.token),
                None
            )
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.server()
            .unwrap()
            .router()
            .oneshot(request("/console/session", None, None, None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn malformed_saved_settings_do_not_silently_remove_the_lock() {
    let f = Fixture::new();
    let r = f.router();
    let body = json!({"expected_revision":1,"credential_unlock_enabled":true});
    assert_eq!(
        send(&r, request(SETTINGS, Some(body), Some(&f.token), None))
            .await
            .0,
        StatusCode::OK
    );
    let path = f.dir.path().join("intake.sqlite.workspace-access.json");
    std::fs::write(&path, "broken").unwrap();
    assert!(f.server().is_err());
    assert_eq!(
        send(&r, request("/console/session", None, None, None))
            .await
            .2["credential_unlock_enabled"],
        true
    );
    assert_ne!(
        send(
            &r,
            request("/console/local-session", Some(json!({})), None, None)
        )
        .await
        .0,
        StatusCode::OK
    );
}
