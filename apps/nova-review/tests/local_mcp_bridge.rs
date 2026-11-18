#![cfg(unix)]
use nova_core::{compilation::AssetScope, intake::*, *};
use nova_mcp::automatic::{IntakeApplicationService, router};
use nova_review::local_mcp_bridge::{
    LocalConnection, read_local_connection, run, write_local_connection,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "bridge-test".into(),
    }
}
fn scope_store() -> (
    tempfile::TempDir,
    IntakeApplicationService,
    IssuedConnection,
) {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("intake.sqlite");
    let key_file = d.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_file).unwrap();
    let mut store = IntakeStore::create(&db, key, scope()).unwrap();
    store.enable_compilation(&scope()).unwrap();
    store.enable_connections(&scope()).unwrap();
    let issued = store
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "bridge".into(),
                label: "Synthetic Claude".into(),
                host: ConnectionHost::Claude,
                preset: ConnectionPreset::CaptureRead,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    (
        d,
        IntakeApplicationService::new(db, key_file, scope()),
        issued,
    )
}
fn packet(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn init() -> Value {
    packet(
        1,
        "initialize",
        json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-stdio-client","version":"1"}}),
    )
}
fn save(id: i64) -> Value {
    packet(
        id,
        "tools/call",
        json!({"name":"save_memory","arguments":{"request_id":"same-save","content":"SYNTHETIC selected digest","user_requested_save":true}}),
    )
}
fn lines(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>()
        .into_bytes()
}
fn replies(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

async fn serve_fixture() -> (
    tempfile::TempDir,
    IntakeApplicationService,
    IssuedConnection,
    PathBuf,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (d, service, issued) = scope_store();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let path = write_local_connection(
        &d.path().join("private connections"),
        &LocalConnection::new(
            issued.connection.id.clone(),
            format!("http://{addr}/mcp/v03"),
            issued.token.clone(),
        ),
    )
    .unwrap();
    let app = router(service.clone(), addr.to_string());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    (d, service, issued, path, task)
}

#[tokio::test]
async fn stdio_roundtrip_preserves_contract_receipts_and_scope_without_automatic_retry() {
    let (_d, service, issued, path, task) = serve_fixture().await;
    let mut out = Vec::new();
    run(&path,std::io::Cursor::new(lines(&[init(),packet(2,"tools/list",json!({})),save(3),save(4),packet(5,"tools/call",json!({"name":"search_memory","arguments":{"request_id":"empty-snapshot","query":"synthetic"}}))])),&mut out).await.unwrap();
    let replies = replies(&out);
    assert_eq!(replies.len(), 5);
    assert_eq!(
        replies[0]["result"]["serverInfo"]["name"],
        "nova-memory-automatic"
    );
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 3);
    let saved = &replies[2]["result"]["structuredContent"];
    let replay = &replies[3]["result"]["structuredContent"];
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["replayed"], false);
    assert_eq!(replay["replayed"], true);
    assert_eq!(saved["receipt"], replay["receipt"]);
    assert!(
        replies[4]["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let store = service.open().unwrap();
    assert_eq!(store.connections(&scope()).unwrap()[0].used_calls, 2);
    let source = store
        .source(
            &scope(),
            saved["receipt"]["source"]["object_id"].as_str().unwrap(),
        )
        .unwrap();
    assert_eq!(source.source_platform, "mcp-claude");
    assert!(!String::from_utf8(out).unwrap().contains(&issued.token));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_child_stdio_is_protocol_only_and_live_revocation_stops_replay() {
    let (_d, service, issued, path, task) = serve_fixture().await;
    let connection_id = issued.connection.id.clone();
    let token = issued.token.clone();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova-memory-connect"))
            .arg("--config")
            .arg(&path)
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        {
            let mut exchange = |input: Value| {
                writeln!(stdin, "{input}").unwrap();
                stdin.flush().unwrap();
                let mut line = String::new();
                stdout.read_line(&mut line).unwrap();
                serde_json::from_str::<Value>(&line).unwrap()
            };
            assert!(exchange(init())["result"].is_object());
            assert_eq!(
                exchange(save(2))["result"]["structuredContent"]["saved"],
                true
            );
            service
                .open()
                .unwrap()
                .revoke_connection(&scope(), &connection_id)
                .unwrap();
            let denied = exchange(save(3));
            assert_eq!(denied["error"]["code"], -32001);
            assert!(
                denied["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("NOVA_CONNECTION_NOT_AUTHORIZED")
            );
            assert!(!denied.to_string().contains(&token));
            // Even another initialize cannot revive a terminally invalidated process.
            assert_eq!(
                exchange(init())["error"]["message"],
                denied["error"]["message"]
            );
            assert_eq!(
                service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
                1
            );
        }
        drop(stdin);
        let status = child.wait().unwrap();
        assert!(status.success());
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
        assert!(stderr.is_empty());
    })
    .await
    .unwrap();
    task.abort();
}

#[test]
fn setup_rejects_nonlocal_routes_unsafe_paths_permissions_and_links() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("connections");
    let token = "a".repeat(64);
    for endpoint in [
        "https://example.invalid/mcp/v03",
        "http://localhost:3030/mcp/v03",
        "http://127.0.0.1:3030/console/session",
        "http://user:pass@127.0.0.1:3030/mcp/v03",
        "http://127.0.0.1:3030/mcp/v03?x=1",
    ] {
        assert!(
            write_local_connection(
                &directory,
                &LocalConnection::new("test".into(), endpoint.into(), token.clone())
            )
            .is_err()
        );
    }
    let good = LocalConnection::new(
        "bound".into(),
        "http://127.0.0.1:3030/mcp/v03".into(),
        token,
    );
    let path = write_local_connection(&directory, &good).unwrap();
    assert!(read_local_connection(&path).is_ok());
    assert!(write_local_connection(&directory, &good).is_err());
    let link = directory.join("link.json");
    symlink(&path, &link).unwrap();
    assert!(read_local_connection(&link).is_err());
    let hard = directory.join("hard.json");
    std::fs::hard_link(&path, &hard).unwrap();
    assert!(read_local_connection(&path).is_err());
    std::fs::remove_file(hard).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_local_connection(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(read_local_connection(&path).is_err());
}

#[tokio::test]
async fn malformed_and_oversize_input_never_gets_sent_and_missing_setup_never_connects() {
    let (_d, service, _issued, path, task) = serve_fixture().await;
    let mut out = Vec::new();
    run(
        &path,
        std::io::Cursor::new(
            b"not-json\n{\"jsonrpc\":\"2.0\",\"method\":\"tools/list\",\"id\":9}\n",
        ),
        &mut out,
    )
    .await
    .unwrap();
    let responses = replies(&out);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["error"]["code"], -32002);
    assert_eq!(
        service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
        0
    );
    assert!(
        run(
            &path,
            std::io::Cursor::new(vec![b'a'; 96 * 1024 + 1]),
            &mut Vec::new()
        )
        .await
        .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        run(&path, tokio::io::empty(), &mut Vec::new())
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn redirect_is_not_followed_and_bad_contract_is_not_exposed_or_promoted() {
    use axum::{Router, response::IntoResponse, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let target_hits = Arc::new(AtomicUsize::new(0));
    let counted = target_hits.clone();
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let target_app = Router::new().route(
        "/mcp/v03",
        post(move || {
            let count = counted.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                "not reached"
            }
        }),
    );
    let target_task = tokio::spawn(async move { axum::serve(target, target_app).await });
    let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_addr = source.local_addr().unwrap();
    let app = Router::new().route(
        "/mcp/v03",
        post(move || async move {
            (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [("location", format!("http://{target_addr}/mcp/v03"))],
            )
                .into_response()
        }),
    );
    let source_task = tokio::spawn(async move { axum::serve(source, app).await });
    let d = tempfile::tempdir().unwrap();
    let config = LocalConnection::new(
        "redirect".into(),
        format!("http://{source_addr}/mcp/v03"),
        "b".repeat(64),
    );
    let path = write_local_connection(&d.path().join("connections"), &config).unwrap();
    let mut out = Vec::new();
    run(&path, std::io::Cursor::new(lines(&[init()])), &mut out)
        .await
        .unwrap();
    assert_eq!(replies(&out)[0]["error"]["code"], -32001);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    source_task.abort();
    target_task.abort();

    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let token = "c".repeat(64);
    let echo = token.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app=Router::new().route("/mcp/v03",post(move ||{let count=count.clone();let echo=echo.clone();async move {count.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"legacy-or-unknown","version":"1"},"protocolVersion":"2025-11-25","unexpected_secret":echo}}))}}));
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let path = write_local_connection(
        &d.path().join("connections"),
        &LocalConnection::new(
            "wrong-contract".into(),
            format!("http://{addr}/mcp/v03"),
            token.clone(),
        ),
    )
    .unwrap();
    let mut out = Vec::new();
    run(
        &path,
        std::io::Cursor::new(lines(&[init(), packet(2, "tools/list", json!({}))])),
        &mut out,
    )
    .await
    .unwrap();
    assert!(replies(&out).iter().all(|r| r["error"]["code"] == -32002));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(!String::from_utf8(out).unwrap().contains(&token));
    task.abort();
}

#[tokio::test]
async fn changed_or_removed_configuration_explains_denial_and_cannot_resume_after_restore() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    for remove in [false, true] {
        let (_d, service, _issued, path, task) = serve_fixture().await;
        let original = std::fs::read(&path).unwrap();
        let child_path = path.clone();
        let (adapter, mut client) = tokio::io::duplex(16 * 1024);
        let (r, w) = tokio::io::split(adapter);
        let bridge = tokio::spawn(async move { run(&child_path, r, w).await });
        client.write_all(&lines(&[init()])).await.unwrap();
        let mut client = BufReader::new(client);
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        assert!(serde_json::from_str::<Value>(&line).unwrap()["result"].is_object());
        if remove {
            std::fs::remove_file(&path).unwrap();
        } else {
            let mut current = read_local_connection(&path).unwrap();
            current.token = "d".repeat(64);
            std::fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
        }
        client
            .get_mut()
            .write_all(&lines(&[save(2)]))
            .await
            .unwrap();
        line.clear();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let denied: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(denied["id"], 2);
        assert_eq!(denied["error"]["code"], -32001);
        assert!(
            denied["error"]["message"]
                .as_str()
                .unwrap()
                .contains("NOVA_CONNECTION_CONFIG_INVALIDATED")
        );
        assert!(!line.contains("SYNTHETIC selected digest"));
        assert!(!line.contains(&path.to_string_lossy().to_string()));
        assert!(!line.contains(&read_token(&original)));
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        client
            .get_mut()
            .write_all(&lines(&[packet(3, "tools/list", json!({}))]))
            .await
            .unwrap();
        line.clear();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let still_denied: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(still_denied["id"], 3);
        assert_eq!(still_denied["error"], denied["error"]);
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), bridge)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            service.open().unwrap().connections(&scope()).unwrap()[0].used_calls,
            0
        );
        task.abort();
    }
}

fn read_token(config: &[u8]) -> String {
    serde_json::from_slice::<Value>(config).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn oversized_upstream_response_is_bounded_and_not_copied_into_stdio() {
    use axum::{Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/mcp/v03",
        post(|| async {
            axum::Json(json!({"jsonrpc":"2.0","id":1,"result":"x".repeat(256*1024)}))
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let d = tempfile::tempdir().unwrap();
    let path = write_local_connection(
        &d.path().join("connections"),
        &LocalConnection::new(
            "oversized".into(),
            format!("http://{addr}/mcp/v03"),
            "e".repeat(64),
        ),
    )
    .unwrap();
    let mut out = Vec::new();
    run(&path, std::io::Cursor::new(lines(&[init()])), &mut out)
        .await
        .unwrap();
    assert!(out.len() < 1024);
    assert_eq!(replies(&out)[0]["error"]["code"], -32001);
    task.abort();
}

#[tokio::test]
async fn bridge_negotiates_its_supported_transport_without_changing_nova_application_contract() {
    for offered in ["2025-06-18", "2025-11-25", "2026-07-28"] {
        let (_d, _service, _issued, path, task) = serve_fixture().await;
        let mut request = init();
        request["params"]["protocolVersion"] = json!(offered);
        let mut out = Vec::new();
        run(
            &path,
            std::io::Cursor::new(lines(&[
                request,
                packet(2, "tools/list", json!({})),
                save(3),
            ])),
            &mut out,
        )
        .await
        .unwrap();
        let responses = replies(&out);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(
            responses[0]["result"]["serverInfo"]["name"],
            "nova-memory-automatic"
        );
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 3);
        assert_eq!(responses[2]["result"]["structuredContent"]["saved"], true);
        task.abort();
    }
}
