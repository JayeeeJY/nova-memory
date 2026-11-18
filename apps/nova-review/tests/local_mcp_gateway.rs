#![cfg(unix)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    middleware,
    response::Response,
};
use nova_core::{VaultKey, compilation::AssetScope, intake::*};
use nova_mcp::automatic::{IntakeApplicationService, router as upstream_router};
use nova_review::{
    local_mcp_bridge::{LocalConnection, write_local_connection},
    local_mcp_gateway::router,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:39911";
struct Fixture {
    _dir: tempfile::TempDir,
    service: IntakeApplicationService,
    token: String,
    id: String,
    path: PathBuf,
    calls: Arc<AtomicUsize>,
    upstream: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.upstream.abort();
    }
}
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "gateway".into(),
    }
}
async fn fixture() -> (Fixture, Router) {
    let dir = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let key_path = dir.path().join("vault.key");
    key.write_new_file(&key_path).unwrap();
    let db = dir.path().join("intake.sqlite");
    let mut store = IntakeStore::create(&db, key, scope()).unwrap();
    store.enable_compilation(&scope()).unwrap();
    store.enable_connections(&scope()).unwrap();
    let issued = store
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "gateway".into(),
                label: "Synthetic remote ChatGPT".into(),
                host: ConnectionHost::Chatgpt,
                preset: ConnectionPreset::CaptureRead,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    let service = IntakeApplicationService::new(db, key_path, scope()).with_ranked_retrieval();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let path = write_local_connection(
        &dir.path().join("private"),
        &LocalConnection::new(
            issued.connection.id.clone(),
            format!("http://{address}/mcp/v03"),
            issued.token.clone(),
        ),
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app = upstream_router(service.clone(), address.to_string()).layer(middleware::from_fn(
        move |request: Request<Body>, next: middleware::Next| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                next.run(request).await
            }
        },
    ));
    let upstream = tokio::spawn(async move { axum::serve(listener, app).await });
    let gateway = router(path.clone(), HOST.into()).unwrap();
    (
        Fixture {
            _dir: dir,
            service,
            token: issued.token,
            id: issued.connection.id,
            path,
            calls,
            upstream,
        },
        gateway,
    )
}
fn packet(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
async fn request(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> Response {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", HOST)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", "2025-11-25");
    for (key, value) in extra {
        req = req.header(*key, *value);
    }
    app.clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}
async fn rpc(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    let response = request(
        app,
        token,
        "POST",
        "/mcp",
        body.to_string().into_bytes(),
        &[],
    )
    .await;
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 300_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn gateway_preserves_application_identity_save_receipt_replay_and_scoped_read() {
    let (f, app) = fixture().await;
    let init=rpc(&app,&f.token,packet(1,"initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-remote","version":"1"}}))).await;
    assert_eq!(init.0, StatusCode::OK);
    assert_eq!(
        init.1["result"]["serverInfo"]["name"],
        "nova-memory-automatic"
    );
    let tools = rpc(&app, &f.token, packet(2, "tools/list", json!({}))).await;
    assert_eq!(tools.1["result"]["tools"].as_array().unwrap().len(), 3);
    let args = json!({"name":"save_memory","arguments":{"request_id":"gateway-save","content":"SYNTHETIC selected digest only","user_requested_save":true}});
    let saved = rpc(&app, &f.token, packet(3, "tools/call", args.clone()))
        .await
        .1;
    let replay = rpc(&app, &f.token, packet(4, "tools/call", args)).await.1;
    assert_eq!(saved["result"]["structuredContent"]["saved"], true);
    assert_eq!(replay["result"]["structuredContent"]["replayed"], true);
    assert_eq!(
        saved["result"]["structuredContent"]["receipt"],
        replay["result"]["structuredContent"]["receipt"]
    );
    let read=rpc(&app,&f.token,packet(5,"tools/call",json!({"name":"search_memory","arguments":{"request_id":"empty-read","query":"synthetic"}}))).await.1;
    assert!(
        read["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let store = f.service.open().unwrap();
    assert_eq!(store.connections(&scope()).unwrap()[0].used_calls, 2);
    let source_id = saved["result"]["structuredContent"]["receipt"]["source"]["object_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        store.source(&scope(), source_id).unwrap().source_platform,
        "mcp-chatgpt"
    );
    assert!(!saved.to_string().contains(&f.token));
}
#[tokio::test]
async fn owner_paths_other_methods_and_arbitrary_tool_names_never_reach_upstream() {
    let (f, app) = fixture().await;
    for path in [
        "/",
        "/app/",
        "/console/api/owner/intake/overview",
        "/mcp/v03",
        "/mcp?target=/console/api/owner",
        "/mcp/",
    ] {
        let response = request(&app, &f.token, "POST", path, b"{}".to_vec(), &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    for name in [
        "delete_memory",
        "search_memory_index",
        "read_memory_detail",
        "read_memory_neighborhood",
    ] {
        let (status, out) = rpc(
            &app,
            &f.token,
            packet(9, "tools/call", json!({"name":name,"arguments":{}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(out["error"]["code"], -32601);
    }
    assert_eq!(
        rpc(&app, &f.token, packet(10, "owner/overview", json!({})))
            .await
            .1["error"]["code"],
        -32601
    );
    let response = request(&app, &f.token, "GET", "/mcp", vec![], &[]).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn http_boundary_rejects_credentials_origin_session_cookies_and_host_confusion() {
    let (f, app) = fixture().await;
    let body = packet(1, "ping", json!({})).to_string().into_bytes();
    assert_eq!(
        request(&app, &"0".repeat(64), "POST", "/mcp", body.clone(), &[])
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for header in [
        ("Origin", "http://evil.invalid"),
        ("Cookie", "synthetic=1"),
        ("X-Nova-Review", "owner-review"),
        ("Sec-Fetch-Site", "cross-site"),
        ("Host", "public.invalid"),
    ] {
        assert_eq!(
            request(&app, &f.token, "POST", "/mcp", body.clone(), &[header])
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &app,
            &f.token,
            "POST",
            "/mcp",
            body.clone(),
            &[("Mcp-Session-Id", "not-supported")]
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            &f.token,
            "POST",
            "/mcp",
            body,
            &[("Authorization", "Bearer duplicate")]
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn malformed_batch_and_oversize_bodies_do_not_forward_or_consume_authority() {
    let (f, app) = fixture().await;
    for body in [
        b"{".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        packet(1, "ping", json!({}))
            .to_string()
            .replacen("\"id\":1", "\"id\":null", 1)
            .into_bytes(),
    ] {
        assert_eq!(
            request(&app, &f.token, "POST", "/mcp", body, &[])
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &app,
            &f.token,
            "POST",
            "/mcp",
            vec![b' '; 96 * 1024 + 1],
            &[]
        )
        .await
        .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
        0
    );
}
#[tokio::test]
async fn replacement_and_live_core_revocation_reject_replay_without_returning_cached_data() {
    let (f, app) = fixture().await;
    let original = std::fs::read(&f.path).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    value["token"] = json!("b".repeat(64));
    std::fs::write(&f.path, value.to_string()).unwrap();
    assert_eq!(
        rpc(&app, &f.token, packet(1, "ping", json!({}))).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    std::fs::write(&f.path, &original).unwrap();
    f.service
        .open()
        .unwrap()
        .revoke_connection(&scope(), &f.id)
        .unwrap();
    assert_ne!(
        rpc(&app, &f.token, packet(2, "ping", json!({}))).await.0,
        StatusCode::OK
    );
    std::fs::remove_file(&f.path).unwrap();
    assert_eq!(
        rpc(&app, &f.token, packet(3, "ping", json!({}))).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
        0
    );
}
#[test]
fn public_authorities_and_missing_private_config_are_rejected_before_router_creation() {
    assert!(
        router(
            PathBuf::from("/missing/private.json"),
            "0.0.0.0:31415".into()
        )
        .is_err()
    );
    assert!(router(PathBuf::from("/missing/private.json"), "127.0.0.1:0".into()).is_err());
    assert!(
        router(
            PathBuf::from("/missing/private.json"),
            "public.example:443".into()
        )
        .is_err()
    );
}

#[tokio::test]
async fn authenticated_slow_requests_are_bounded_and_excess_work_fails_fast() {
    let (f, _app) = fixture().await;
    let ready = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let observed = ready.clone();
    let released = release.clone();
    let mock = Router::new().route(
        "/mcp/v03",
        axum::routing::post(move |axum::Json(input): axum::Json<Value>| {
            let observed = observed.clone();
            let released = released.clone();
            async move {
                let wait = released.notified();
                observed.fetch_add(1, Ordering::SeqCst);
                wait.await;
                axum::Json(json!({"jsonrpc":"2.0","id":input["id"],"result":{}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let path = write_local_connection(
        &f._dir.path().join("slow private"),
        &LocalConnection::new(
            "synthetic-slow".into(),
            format!("http://{address}/mcp/v03"),
            f.token.clone(),
        ),
    )
    .unwrap();
    let app = router(path, HOST.into()).unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await });
    let mut requests = Vec::new();
    for id in 0..8 {
        let app = app.clone();
        let token = f.token.clone();
        requests.push(tokio::spawn(async move {
            rpc(&app, &token, packet(id, "ping", json!({}))).await
        }));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while ready.load(Ordering::SeqCst) != 8 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(ready.load(Ordering::SeqCst), 8);
    assert_eq!(
        rpc(&app, &f.token, packet(99, "ping", json!({}))).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(ready.load(Ordering::SeqCst), 8);
    release.notify_waiters();
    for request in requests {
        assert_eq!(request.await.unwrap().0, StatusCode::OK);
    }
    assert_eq!(
        f.service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
        0
    );
    task.abort();
}
