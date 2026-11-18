use nova_core::*;
use nova_mcp::{
    generic::{GenericMcpServer, query_spec},
    transport::{ClientCredential, generic_router},
};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
#[tokio::test]
async fn guarded_http_discovery_save_status_confirm_restart_and_search() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.sqlite");
    let key_path = dir.path().join("test.key");
    let token_path = dir.path().join("client.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&key_path)?;
    drop(Vault::create(&path, key)?);
    VaultKey::generate()?.write_new_file(&token_path)?;
    let token = std::fs::read_to_string(&token_path)?;
    let credential = ClientCredential::read(&token_path)?;
    assert!(!format!("{credential:?}").contains(token.trim()));
    let cancel = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/mcp");
    let router = generic_router(
        GenericMcpServer::new(&path, &key_path),
        credential,
        addr.to_string(),
        cancel.child_token(),
    );
    let handle = tokio::spawn(async move { axum::serve(listener, router).await });
    let http = reqwest::Client::new();
    for method in [
        reqwest::Method::POST,
        reqwest::Method::GET,
        reqwest::Method::DELETE,
    ] {
        let r = http.request(method, &url).send().await?;
        assert_eq!(r.status(), 401);
    }
    for (name, value) in [
        ("Origin", "http://evil.invalid"),
        ("Host", "evil.invalid"),
        ("Sec-Fetch-Site", "cross-site"),
    ] {
        let r = http
            .post(&url)
            .bearer_auth(token.trim())
            .header(name, value)
            .send()
            .await?;
        assert_eq!(r.status(), if name == "Host" { 401 } else { 403 });
    }
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(token.trim()),
    );
    let client = ClientInfo::default().serve(transport).await?;
    let tools = client.list_tools(Default::default()).await?;
    assert_eq!(tools.tools.len(), 3);
    for name in ["save_memory", "get_memory", "search_memory"] {
        let tool = tools.tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(
            tool.annotations.as_ref().unwrap().read_only_hint,
            Some(false)
        );
    }
    assert!(
        !tools
            .tools
            .iter()
            .any(|t| t.name.contains("confirm") || t.name.contains("approve"))
    );
    let args = json!({"schema_version":"nova.capture.v0.3","assertion_origin":"ai_extracted_proposal","surface":"chat_text","capture_fidelity":"f1_ai_digest","user_requested_save":true,"request_id":"http-save","title":"Synthetic HTTP P1","memory_type":"decision","summary":"Synthetic HTTP complete loop","session_digest":"Synthetic explicit save","privacy_level":"private"});
    let call = CallToolRequestParams::new("save_memory")
        .with_arguments(serde_json::from_value(args.clone())?);
    let result = client.call_tool(call.clone()).await?;
    assert_ne!(result.is_error, Some(true));
    let saved: Value = result.structured_content.unwrap();
    let id = saved["memory_id"].as_str().unwrap();
    let retry = client.call_tool(call).await?;
    assert_eq!(
        retry.structured_content.unwrap()["receipt_id"],
        saved["receipt_id"]
    );
    let mut vault = Vault::open(&path, VaultKey::read_file(&key_path)?)?;
    let grant = vault.issue_disclosure_grant(
        query_spec(
            "http-status",
            "personal_memory_continuity",
            QuerySelection::Get {
                memory_id: id.into(),
                capture_status: true,
            },
        ),
        vec![id.into()],
        PrivacyLevel::Private,
        600,
    )?;
    let args = json!({"schema_version":"nova.query.v0.1","request_id":"http-status","purpose":"personal_memory_continuity","approval_id":grant.approval_id,"memory_id":id,"view":"capture_status"});
    let result = client
        .call_tool(
            CallToolRequestParams::new("get_memory").with_arguments(serde_json::from_value(args)?),
        )
        .await?;
    assert_eq!(
        result.structured_content.unwrap()["items"][0]["status"],
        "pending"
    );
    let row = vault.review_inbox("owner:founder")?.remove(0);
    let confirmed = vault.review_proposal(ReviewRequest {
        request_id: Some("http-owner-confirm".into()),
        subject: "owner:founder".into(),
        proposal_id: id.into(),
        expected_snapshot: row.snapshot,
        action: ReviewAction::Confirm,
    })?;
    let mid = confirmed.memory_id.unwrap();
    let grant = vault.issue_disclosure_grant(
        query_spec(
            "http-search",
            "personal_memory_continuity",
            QuerySelection::Search {
                query: "Synthetic".into(),
                max_items: 5,
            },
        ),
        vec![mid.clone()],
        PrivacyLevel::Private,
        600,
    )?;
    drop(vault);
    client.cancel().await?;
    cancel.cancel();
    handle.abort();
    let _ = handle.await;
    let cancel = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = generic_router(
        GenericMcpServer::new(&path, &key_path),
        ClientCredential::read(&token_path)?,
        addr.to_string(),
        cancel.child_token(),
    );
    let handle = tokio::spawn(async move { axum::serve(listener, router).await });
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .auth_header(token.trim()),
    );
    let client = ClientInfo::default().serve(transport).await?;
    let args = json!({"schema_version":"nova.query.v0.1","request_id":"http-search","purpose":"personal_memory_continuity","approval_id":grant.approval_id,"query":"Synthetic","max_items":5});
    let result = client
        .call_tool(
            CallToolRequestParams::new("search_memory")
                .with_arguments(serde_json::from_value(args)?),
        )
        .await?;
    let result = result.structured_content.unwrap();
    assert_eq!(result["items"][0]["canonical_memory_id"], mid);
    assert_eq!(result["receipt"]["recipient"], "codex");
    assert!(
        Vault::open(&path, VaultKey::read_file(&key_path)?)?
            .verify_audit_chain()?
            .valid
    );
    client.cancel().await?;
    cancel.cancel();
    handle.abort();
    let _ = handle.await;
    Ok(())
}
#[test]
fn credential_permissions_and_missing_file_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credential");
    assert!(ClientCredential::read(&path).is_err());
    std::fs::write(&path, "short").unwrap();
    assert!(ClientCredential::read(&path).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, "a".repeat(64)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ClientCredential::read(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ClientCredential::read(&path).is_ok());
        let symlink = dir.path().join("alias");
        std::os::unix::fs::symlink(&path, &symlink).unwrap();
        assert!(ClientCredential::read(&symlink).is_err());
    }
}
