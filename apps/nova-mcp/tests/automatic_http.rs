use nova_core::{compilation::*, intake::*, *};
use nova_mcp::automatic::{IntakeApplicationService, router};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "test".into(),
    }
}
fn setup() -> anyhow::Result<(
    tempfile::TempDir,
    IntakeApplicationService,
    IntakeReceipt,
    String,
    IssuedConnection,
)> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("intake.sqlite");
    let key = d.path().join("vault.key");
    let k = VaultKey::generate()?;
    k.write_new_file(&key)?;
    let mut s = IntakeStore::create(&db, k, scope())?;
    s.enable_compilation(&scope())?;
    s.enable_connections(&scope())?;
    let r=s.receive(&IntakeAuthority{scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"synthetic","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC source","occurred_at":null}))?)?;
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-not-an-llm".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )?;
    let job = s.claim_next(&scope(), 60)?.unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)?;
    s.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 100)?;
    let out = json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"user","predicate":"test:decision","value":"synthetic local","polarity":"positive","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC source","occurrence":0}]}]});
    let commit = s.commit_compilation(
        &scope(),
        &job.job.job_id,
        &lease.lease_id,
        &serde_json::to_vec(&out)?,
    )?;
    let c = s.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "issue".into(),
            label: "Synthetic Codex".into(),
            host: ConnectionHost::Codex,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: false,
        },
    )?;
    Ok((
        d,
        IntakeApplicationService::new(db, key, scope()),
        r,
        commit.output_versions[0].object_id.clone(),
        c,
    ))
}
fn call(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned())
        .with_arguments(serde_json::from_value(args).unwrap())
}
#[tokio::test]
async fn stateless_sdk_discovery_bound_read_receipt_replay_revoke_and_owner_boundary()
-> anyhow::Result<()> {
    let (_d, service, _source, mid, issued) = setup()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/mcp/v03");
    let app = router(service.clone(), addr.to_string());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await });
    let http = reqwest::Client::new();
    assert_eq!(http.post(&url).send().await?.status(), 401);
    for (name, value, status) in [
        ("Origin", "http://evil.invalid", 403),
        ("Host", "evil.invalid", 401),
        ("mcp-session-id", "another-connection", 400),
    ] {
        assert_eq!(
            http.post(&url)
                .bearer_auth(&issued.token)
                .header(name, value)
                .send()
                .await?
                .status(),
            status
        );
    }
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(issued.token.clone()),
    );
    let client = ClientInfo::default().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 3);
    let search = tools
        .iter()
        .find(|tool| tool.name == "search_memory")
        .unwrap();
    let limit = &search.input_schema["properties"]["max_items"];
    assert_eq!(limit["minimum"], 1);
    assert_eq!(limit["maximum"], 8);
    assert_eq!(limit["default"], 5);
    let invalid_limit = client
        .call_tool(call(
            "search_memory",
            json!({"request_id":"invalid-limit","query":"synthetic","max_items":10}),
        ))
        .await?;
    assert_eq!(invalid_limit.is_error, Some(true));
    assert!(
        invalid_limit.content[0]
            .as_text()
            .unwrap()
            .text
            .contains("max_items must be between 1 and 8")
    );
    let read = call("get_memory", json!({"request_id":"read","memory_id":mid}));
    let result = client.call_tool(read.clone()).await?;
    assert_ne!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&result.content[0].as_text().unwrap().text)?,
        result.structured_content.as_ref().unwrap().clone()
    );
    assert!(
        result.content[1]
            .as_text()
            .unwrap()
            .text
            .contains("items[0]: modality=asserted")
    );
    let data = result.structured_content.unwrap();
    assert_eq!(data["receipt"]["view"], "compiled_unverified");
    assert_eq!(data["receipt"]["connection_id"], issued.connection.id);
    assert_eq!(
        data["items"][0]["semantic_support"],
        "model_output_unverified"
    );
    assert!(!data.to_string().contains("SYNTHETIC source"));
    let search_result = client
        .call_tool(call(
            "search_memory",
            json!({
                "request_id":"search-guidance", "query":"synthetic"
            }),
        ))
        .await?;
    assert_ne!(search_result.is_error, Some(true));
    assert_eq!(search_result.content.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(&search_result.content[0].as_text().unwrap().text)?,
        search_result.structured_content.as_ref().unwrap().clone()
    );
    assert_eq!(
        search_result.structured_content.as_ref().unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        search_result.content[1]
            .as_text()
            .unwrap()
            .text
            .contains("字面子串匹配")
    );
    assert!(
        search_result.content[1]
            .as_text()
            .unwrap()
            .text
            .contains("最多再用一个更短词查询一次")
    );
    assert_eq!(
        client
            .call_tool(read.clone())
            .await?
            .structured_content
            .unwrap()["receipt"]["receipt_id"],
        data["receipt"]["receipt_id"]
    );
    assert_eq!(
        client
            .call_tool(call(
                "save_memory",
                json!({"request_id":"save","content":"synthetic","user_requested_save":true})
            ))
            .await?
            .is_error,
        Some(true)
    );
    assert_eq!(
        client
            .call_tool(call(
                "get_memory",
                json!({"request_id":"spoof","memory_id":mid,"owner_id":"owner:other"})
            ))
            .await?
            .is_error,
        Some(true)
    );
    service
        .open()?
        .revoke_connection(&scope(), &issued.connection.id)?;
    assert!(client.call_tool(read).await.is_err());
    let _ = client.cancel().await;
    handle.abort();
    Ok(())
}
#[tokio::test]
async fn sdk_capture_queues_f1_without_confirmation_and_source_withdrawal_fences_read_cache()
-> anyhow::Result<()> {
    let (_d, service, source, _mid, issued) = setup()?;
    let capture = service.open()?.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "capture-client".into(),
            label: "Synthetic collector".into(),
            host: ConnectionHost::Codex,
            preset: ConnectionPreset::CaptureRead,
            topic_id: None,
            include_future: true,
        },
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(service.clone(), addr.to_string());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await });
    let url = format!("http://{addr}/mcp/v03");
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(capture.token),
        ))
        .await?;
    let saved=client.call_tool(call("save_memory",json!({"request_id":"save","content":"SYNTHETIC selected digest","user_requested_save":true}))).await?.structured_content.unwrap();
    assert_eq!(saved["saved"], true);
    let sid = saved["receipt"]["source"]["object_id"].as_str().unwrap();
    let raw = service.open()?.source(&scope(), sid)?;
    assert_eq!(raw.role, SourceRole::Mixed);
    assert_eq!(raw.capture_fidelity, CaptureFidelity::F1AiDigest);
    let reader = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url).auth_header(issued.token),
        ))
        .await?;
    let search = call(
        "search_memory",
        json!({"request_id":"search","query":"synthetic"}),
    );
    assert_eq!(
        reader
            .call_tool(search.clone())
            .await?
            .structured_content
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    service
        .open()?
        .withdraw_source(&scope(), &source.source.object_id)?;
    assert_eq!(reader.call_tool(search).await?.is_error, Some(true));
    assert!(
        reader
            .call_tool(call(
                "search_memory",
                json!({"request_id":"fresh","query":"synthetic"})
            ))
            .await?
            .structured_content
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let _ = reader.cancel().await;
    let _ = client.cancel().await;
    handle.abort();
    Ok(())
}

#[tokio::test]
async fn save_discovery_and_replay_report_failed_processing_without_cross_protocol_fallback()
-> anyhow::Result<()> {
    let (_d, service, _source, _mid, _) = setup()?;
    let issued = service.open()?.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "grok-capture".into(),
            label: "Synthetic Grok".into(),
            host: ConnectionHost::Grok,
            preset: ConnectionPreset::CaptureRead,
            topic_id: None,
            include_future: true,
        },
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(service.clone(), addr.to_string());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp/v03"))
                .auth_header(issued.token),
        ))
        .await?;
    let info = client.peer_info().unwrap();
    assert_eq!(
        info.server_info.as_ref().unwrap().name,
        "nova-memory-automatic"
    );
    assert!(
        info.instructions
            .as_deref()
            .unwrap()
            .contains("without per-memory approval")
    );
    let tools = client.list_all_tools().await?;
    let tool = tools.iter().find(|t| t.name == "save_memory").unwrap();
    // Reproduce strict clients' discovery check: an advertised output schema
    // must explicitly describe an object, even for dynamic JSON envelopes.
    assert_eq!(tools.len(), 3);
    for advertised in &tools {
        assert_eq!(
            advertised.output_schema.as_ref().unwrap()["type"],
            "object",
            "invalid output schema for {}",
            advertised.name
        );
    }
    assert_eq!(
        tool.annotations.as_ref().and_then(|a| a.title.as_deref()),
        Some("Save to Nova Memory")
    );
    assert_eq!(tool.input_schema["additionalProperties"], false);
    assert!(tool.input_schema["properties"].get("content").is_some());
    assert!(
        tool.input_schema["properties"]
            .get("schema_version")
            .is_none()
    );
    let args = json!({"request_id":"save","content":"SYNTHETIC digest","user_requested_save":true});
    for bad in [
        json!({"request_id":"no-consent","content":"SYNTHETIC digest","user_requested_save":false}),
        json!({"request_id":"p2","content":"SYNTHETIC digest","user_requested_save":true,"schema_version":"nova.capture.v0.3"}),
        json!({"request_id":"spoof","content":"SYNTHETIC digest","user_requested_save":true,"host":"chatgpt"}),
    ] {
        assert_eq!(
            client.call_tool(call("save_memory", bad)).await?.is_error,
            Some(true)
        );
    }
    let saved = client
        .call_tool(call("save_memory", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(saved["processing"], "queued");
    assert_eq!(saved["replayed"], false);
    let mut store = service.open()?;
    assert_eq!(
        store
            .source(
                &scope(),
                saved["receipt"]["source"]["object_id"].as_str().unwrap()
            )?
            .source_platform,
        "mcp-grok"
    );
    let job = store.claim_next(&scope(), 60)?.unwrap();
    let lease = job.lease.unwrap();
    store.fail_attempt(
        &scope(),
        &job.job.job_id,
        &lease.lease_id,
        AttemptFailure::ModelUnavailable,
    )?;
    let replay = client
        .call_tool(call("save_memory", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(replay["receipt"], saved["receipt"]);
    assert_eq!(replay["saved"], true);
    assert_eq!(replay["processing"], "failed");
    assert_eq!(replay["replayed"], true);
    assert!(
        replay["user_message"]
            .as_str()
            .unwrap()
            .contains("原始材料已保存")
    );
    assert_eq!(
        store
            .connections(&scope())?
            .iter()
            .find(|c| c.id == issued.connection.id)
            .unwrap()
            .used_calls,
        1
    );
    store.withdraw_source(
        &scope(),
        saved["receipt"]["source"]["object_id"].as_str().unwrap(),
    )?;
    assert_eq!(
        client.call_tool(call("save_memory", args)).await?.is_error,
        Some(true)
    );
    let _ = client.cancel().await;
    task.abort();
    Ok(())
}
