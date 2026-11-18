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
        space: "ranked-http".into(),
    }
}
fn ingest(s: &mut IntakeStore, id: &str, value: &str) -> anyhow::Result<IntakeReceipt> {
    let r=s.receive(&IntakeAuthority {scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"fixture","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC RAW EVIDENCE NEVER DISCLOSED","occurred_at":null}))?)?;
    let j = s.claim_next(&scope(), 60)?.unwrap();
    let l = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &l.lease_id)?;
    s.authorize_dispatch(&scope(), &j.job.job_id, &l.lease_id, 1000)?;
    s.commit_compilation(&scope(),&j.job.job_id,&l.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:fixture","predicate":"work:rule","value":value,"polarity":"negative","modality":"asserted","conditions":{"work:scope":"工作"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC RAW EVIDENCE NEVER DISCLOSED","occurrence":0}]}]}))?)?;
    Ok(r)
}
fn fixture() -> anyhow::Result<(
    tempfile::TempDir,
    IntakeApplicationService,
    IssuedConnection,
    String,
)> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("intake.sqlite");
    let kf = d.path().join("vault.key");
    let k = VaultKey::generate()?;
    k.write_new_file(&kf)?;
    let mut s = IntakeStore::create(&db, k, scope())?;
    s.enable_compilation(&scope())?;
    s.enable_connections(&scope())?;
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "fixture-no-model".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )?;
    let source = ingest(&mut s, "visible", "工作场景不参加超过三十分钟的会议")?;
    let c = s.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "client".into(),
            label: "fixture".into(),
            host: ConnectionHost::Codex,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: false,
        },
    )?;
    ingest(&mut s, "not-granted", "SECRET_UNAPPROVED 工作 会议")?;
    Ok((
        d,
        IntakeApplicationService::new(db, kf, scope()),
        c,
        source.source.object_id,
    ))
}
fn q(
    s: &IntakeApplicationService,
    c: &IssuedConnection,
    id: &str,
) -> anyhow::Result<HistoricalQuery> {
    let u = s
        .open()?
        .compiled_unit(&scope(), &c.connection.member_snapshot[0].object_id)?;
    Ok(HistoricalQuery {
        request_id: id.into(),
        query: None,
        memory_id: Some(u.unit.header.object.object_id),
        max_items: 1,
        known_as_of: u.unit.header.recorded_at,
        valid_at: None,
        unknown_validity: UnknownValidity::Exclude,
    })
}
#[test]
fn history_service_and_owner_policy_are_both_required() -> anyhow::Result<()> {
    let (_d, base, c, _) = fixture()?;
    let a = base.authenticate(&c.token)?;
    assert!(base.query_historical(&a, q(&base, &c, "first")?).is_err());
    let service = base.with_historical_retrieval();
    assert!(
        service
            .query_historical(&a, q(&service, &c, "first")?)
            .is_err()
    );
    service.open()?.set_connection_history_policy(
        &scope(),
        &c.connection.id,
        0,
        Some("2000-01-01T00:00:00Z".into()),
    )?;
    let result = service.query_historical(&a, q(&service, &c, "first")?)?;
    assert_eq!(result.items.len(), 1);
    assert!(!serde_json::to_string(&result)?.contains("SECRET_UNAPPROVED"));
    assert!(!serde_json::to_string(&result)?.contains("SYNTHETIC RAW EVIDENCE"));
    Ok(())
}
fn call(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(serde_json::from_value(args).unwrap())
}
#[tokio::test]
async fn history_http_tool_has_no_permission_mutation_and_live_revocation_fences_cache()
-> anyhow::Result<()> {
    let (_d, base, c, source) = fixture()?;
    let service = base.with_historical_retrieval();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(service.clone(), addr.to_string());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp/v03"))
                .auth_header(c.token.clone()),
        ))
        .await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 4);
    assert!(tools.iter().any(|t| t.name == "read_memory_history"));
    assert!(!tools.iter().any(|t| t.name.contains("policy")));
    let query = q(&service, &c, "same")?;
    let args =
        json!({"request_id":"same","memory_id":query.memory_id,"known_as_of":query.known_as_of});
    assert_eq!(
        client
            .call_tool(call("read_memory_history", args.clone()))
            .await?
            .is_error,
        Some(true)
    );
    service.open()?.set_connection_history_policy(
        &scope(),
        &c.connection.id,
        0,
        Some("2000-01-01T00:00:00Z".into()),
    )?;
    let first = client
        .call_tool(call("read_memory_history", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let replay = client
        .call_tool(call("read_memory_history", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(first, replay);
    let mut spoof = args.clone();
    spoof["owner_id"] = json!("other");
    assert_eq!(
        client
            .call_tool(call("read_memory_history", spoof))
            .await?
            .is_error,
        Some(true)
    );
    service
        .open()?
        .set_connection_history_policy(&scope(), &c.connection.id, 1, None)?;
    assert_eq!(
        client
            .call_tool(call("read_memory_history", args.clone()))
            .await?
            .is_error,
        Some(true)
    );
    service.open()?.set_connection_history_policy(
        &scope(),
        &c.connection.id,
        2,
        Some("2000-01-01T00:00:00Z".into()),
    )?;
    assert_eq!(
        client
            .call_tool(call("read_memory_history", args.clone()))
            .await?
            .is_error,
        Some(true)
    );
    let mut fresh = args.clone();
    fresh["request_id"] = json!("fresh");
    assert_ne!(
        client
            .call_tool(call("read_memory_history", fresh.clone()))
            .await?
            .is_error,
        Some(true)
    );
    service.open()?.withdraw_source(&scope(), &source)?;
    assert_eq!(
        client
            .call_tool(call("read_memory_history", fresh))
            .await?
            .is_error,
        Some(true)
    );
    service
        .open()?
        .revoke_connection(&scope(), &c.connection.id)?;
    assert!(
        client
            .call_tool(call("read_memory_history", args))
            .await
            .is_err()
    );
    let _ = client.cancel().await;
    task.abort();
    Ok(())
}
