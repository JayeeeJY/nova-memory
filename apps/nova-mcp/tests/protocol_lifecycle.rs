use nova_core::{Vault, VaultKey};
use nova_mcp::{
    generic::GenericMcpServer,
    transport::{ClientCredential, generic_router},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
#[tokio::test]
async fn legacy_protocol_notification_202_delete_204_and_closed_session_404() -> anyhow::Result<()>
{
    let d = tempfile::tempdir()?;
    let db = d.path().join("test.sqlite");
    let key = d.path().join("key.key");
    let token = d.path().join("token.key");
    let k = VaultKey::generate()?;
    k.write_new_file(&key)?;
    drop(Vault::create(&db, k)?);
    VaultKey::generate()?.write_new_file(&token)?;
    let secret = std::fs::read_to_string(&token)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let cancel = CancellationToken::new();
    let router = generic_router(
        GenericMcpServer::new(&db, &key),
        ClientCredential::read(&token)?,
        addr.to_string(),
        cancel.clone(),
    );
    let task = tokio::spawn(async move { axum::serve(listener, router).await });
    let url = format!("http://{addr}/mcp");
    let http = reqwest::Client::new();
    let init=http.post(&url).bearer_auth(secret.trim()).header("Accept","application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-sdk","version":"1"}}})).send().await?;
    assert_eq!(init.status(), 200);
    let session = init.headers()["mcp-session-id"].to_str()?.to_string();
    let _ = init.bytes().await?;
    let initialized = http
        .post(&url)
        .bearer_auth(secret.trim())
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &session)
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await?;
    assert_eq!(initialized.status(), 202);
    assert!(initialized.bytes().await?.is_empty());
    let deleted = http
        .delete(&url)
        .bearer_auth(secret.trim())
        .header("MCP-Session-Id", &session)
        .header("MCP-Protocol-Version", "2025-11-25")
        .send()
        .await?;
    assert_eq!(deleted.status(), 204);
    let closed = http
        .post(&url)
        .bearer_auth(secret.trim())
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Session-Id", &session)
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await?;
    assert_eq!(closed.status(), 404);
    let origin = http
        .post(&url)
        .bearer_auth(secret.trim())
        .header("Origin", "https://evil.invalid")
        .send()
        .await?;
    assert_eq!(origin.status(), 403);
    let oversized = http
        .post(&url)
        .bearer_auth(secret.trim())
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body("x".repeat(100 * 1024))
        .send()
        .await?;
    assert_eq!(oversized.status(), 413);
    cancel.cancel();
    task.abort();
    let _ = task.await;
    Ok(())
}
