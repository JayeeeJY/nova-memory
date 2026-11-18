use anyhow::Result;
use axum::Router;
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ConfirmRequest, MemoryType,
    PrivacyLevel, ReviewTier, Vault, VaultKey,
};
use nova_mcp::NovaMcpServer;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
    transport::{
        StreamableHttpClientTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn streamable_http_exposes_bounded_write_and_context_tools() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let vault_path = directory.path().join("nova.sqlite");
    let key_path = directory.path().join("vault.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&key_path)?;
    let mut vault = Vault::create(&vault_path, key)?;
    let captured = vault.capture(CaptureRequest {
        structure: None,
        request_id: Some("seed_http_context".into()),
        owner_id: "owner:founder".into(),
        source_platform: "chatgpt".into(),
        capture_channel: CaptureChannel::RemoteMcp,
        capture_fidelity: CaptureFidelity::F1AiDigest,
        source_content: "用户决定 Nova P0 只验证 ChatGPT。".into(),
        title: "Nova P0 平台范围".into(),
        memory_type: MemoryType::Decision,
        summary: "Nova P0 只做 ChatGPT。".into(),
        decisions: vec!["P0 只做 ChatGPT".into()],
        privacy_level: PrivacyLevel::Private,
        proposed_origin: AssertionOrigin::AiExtractedProposal,
        review_tier: ReviewTier::Attention,
        permission_scope: vec!["personal_review".into()],
        purpose_limits: vec!["personal_memory_continuity".into()],
        observed_at: Some("2026-08-14T08:00:00Z".into()),
    })?;
    let seeded = vault.confirm_proposal(ConfirmRequest {
        request_id: None,
        subject: "owner:founder".into(),
        proposal_id: captured.proposal.proposal_id,
    })?;
    drop(vault);

    let cancellation = CancellationToken::new();
    let configured_server = NovaMcpServer::new(&vault_path, &key_path);
    let service: StreamableHttpService<NovaMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(configured_server.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );
    let router = Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_handle = tokio::spawn(async move { axum::serve(listener, router).await });

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp")),
    );
    let client = ClientInfo::default().serve(transport).await?;
    let tools = client.list_tools(Default::default()).await?;
    assert_eq!(tools.tools.len(), 3);
    let save_tool = tools
        .tools
        .iter()
        .find(|tool| tool.name == "save_voice_session_digest")
        .expect("save tool");
    let annotations = save_tool.annotations.as_ref().expect("annotations");
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(false));
    assert_eq!(annotations.open_world_hint, Some(false));

    let arguments = serde_json::from_value(serde_json::json!({
        "user_requested_save": true,
        "request_id": "chatgpt_http_001",
        "title": "ChatGPT 保存流程",
        "memory_type": "decision",
        "summary": "ChatGPT 将明确请求的对话保存为待确认草稿。",
        "session_digest": "用户明确要求将对话的主要内容存入 Nova。",
        "key_points": ["Nova 是环境式记忆基础设施"],
        "decisions": ["首个插件只生成待确认草稿"],
        "rationales": ["保留用户对真值的最终控制"],
        "open_questions": ["ChatGPT Voice 真机是否能触发该工具"],
        "next_steps": ["进行 ChatGPT developer mode 验证"],
        "privacy_level": "private",
        "observed_at": "2026-08-14T08:00:00Z"
    }))?;
    let result = client
        .call_tool(
            CallToolRequestParams::new("save_voice_session_digest").with_arguments(arguments),
        )
        .await?;
    assert_ne!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structured output");
    assert_eq!(structured["status"], "proposal_created");
    assert_eq!(structured["needs_confirmation"], true);

    let unified = tools
        .tools
        .iter()
        .find(|tool| tool.name == "save_to_nova")
        .expect("unified save tool");
    let required = unified
        .input_schema
        .get("required")
        .expect("required fields");
    for field in [
        "host",
        "surface",
        "capture_fidelity",
        "user_requested_save",
        "request_id",
    ] {
        assert!(
            required
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == field)
        );
    }
    for fixture in [
        include_str!("fixtures/save_to_nova_chat_text.json"),
        include_str!("fixtures/save_to_nova_voice_audio.json"),
    ] {
        let arguments: serde_json::Value = serde_json::from_str(fixture)?;
        let result = client
            .call_tool(
                CallToolRequestParams::new("save_to_nova")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await?;
        assert_ne!(result.is_error, Some(true));
        let output = result.structured_content.expect("unified output");
        assert_eq!(output["status"], "proposal_created");
        assert_eq!(output["surface"], arguments["surface"]);
        assert_eq!(output["host"], "chatgpt");
        assert_eq!(output["capture_fidelity"], "f1_ai_digest");
        assert_eq!(output["content_digest"].as_str().unwrap().len(), 71);
        assert_eq!(output["needs_confirmation"], true);
        let retry = client
            .call_tool(
                CallToolRequestParams::new("save_to_nova")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await?;
        assert_eq!(retry.structured_content.unwrap(), output);
    }

    let context_arguments = serde_json::from_value(serde_json::json!({
        "user_approved": true,
        "request_id": "chatgpt_http_context_001",
        "purpose": "personal_memory_continuity",
        "query": "Nova P0 ChatGPT",
        "surface": "chat_text",
        "allowed_memory_types": ["decision"],
        "maximum_privacy_level": "private",
        "max_items": 5,
        "expires_in_seconds": 600
    }))?;
    let context_result = client
        .call_tool(
            CallToolRequestParams::new("prepare_context_pack").with_arguments(context_arguments),
        )
        .await?;
    assert_ne!(context_result.is_error, Some(true));
    let context = context_result.structured_content.expect("context output");
    assert_eq!(context["status"], "context_pack_delivered");
    assert_eq!(context["surface"], "chat_text");
    assert_eq!(context["items"][0]["memory_id"], seeded.memory.memory_id);

    let key = VaultKey::read_file(&key_path)?;
    let vault = Vault::open(&vault_path, key)?;
    assert_eq!(vault.pending_proposals()?.len(), 3);
    assert_eq!(vault.memories()?.len(), 1);

    client.cancel().await?;
    server_handle.abort();
    Ok(())
}

/// SDK interoperability only: this is deliberately NOT described as a real Codex client.
#[tokio::test]
async fn discovery_spike_rejects_codex_declaration_without_vault_mutation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let vault_path = directory.path().join("nova.sqlite");
    let key_path = directory.path().join("vault.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&key_path)?;
    drop(Vault::create(&vault_path, key)?);
    let service: StreamableHttpService<NovaMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(NovaMcpServer::new(&vault_path, &key_path)),
            Default::default(),
            StreamableHttpServerConfig::default(),
        );
    let router = Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_handle = tokio::spawn(async move { axum::serve(listener, router).await });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp")),
        ))
        .await?;
    let tools = client.list_tools(Default::default()).await?;
    assert_eq!(tools.tools.len(), 3);
    assert!(!tools.tools.iter().any(|tool| matches!(
        tool.name.as_ref(),
        "save_memory" | "get_memory" | "search_memory"
    )));
    let mut args: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/save_to_nova_chat_text.json"))?;
    args["host"] = serde_json::json!("codex");
    // Deserialization must reject unsupported identity before capture can execute.
    let rejected = client
        .call_tool(
            CallToolRequestParams::new("save_to_nova")
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await;
    assert!(
        rejected.is_err()
            || rejected
                .as_ref()
                .is_ok_and(|result| result.is_error == Some(true))
    );
    let vault = Vault::open(
        &directory.path().join("nova.sqlite"),
        VaultKey::read_file(&directory.path().join("vault.key"))?,
    )?;
    assert!(vault.pending_proposals()?.is_empty());
    assert!(vault.memories()?.is_empty());
    client.cancel().await?;
    server_handle.abort();
    Ok(())
}
