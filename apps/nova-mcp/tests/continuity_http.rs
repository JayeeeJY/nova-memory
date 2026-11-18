use nova_core::{compilation::*, intake::*, *};
use nova_mcp::automatic::{AutomaticBrief, AutomaticRecap, IntakeApplicationService, router};
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
        owner_id: "owner:fixture".into(),
        space: "continuity-http".into(),
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    service: IntakeApplicationService,
    token: String,
    connection_id: String,
    session: SessionRef,
    other_session: SessionRef,
    handoff_id: String,
    source_id: String,
}
fn fixture() -> anyhow::Result<Fixture> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("intake.sqlite");
    let key_file = d.path().join("vault.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&key_file)?;
    let mut s = IntakeStore::create(&db, key, scope())?;
    s.enable_compilation(&scope())?;
    s.enable_connections(&scope())?;
    s.enable_continuity(&scope())?;
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "fixture-no-model".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )?;
    let receipt=s.receive(&IntakeAuthority {scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"fixture","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC RAW SOURCE NEVER DISCLOSED","occurred_at":null}))?)?;
    let j = s.claim_next(&scope(), 60)?.unwrap();
    let l = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &l.lease_id)?;
    s.authorize_dispatch(&scope(), &j.job.job_id, &l.lease_id, 1000)?;
    let c=s.commit_compilation(&scope(),&j.job.job_id,&l.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:fixture","predicate":"task:constraint","value":"synthetic local context","polarity":"negative","modality":"asserted","conditions":{"task:scope":"work"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC RAW SOURCE NEVER DISCLOSED","occurrence":0}]}]}))?)?;
    let conn = |request: &str| ConnectionRequest {
        request_id: request.into(),
        label: "fixture".into(),
        host: ConnectionHost::Codex,
        preset: ConnectionPreset::ReadOnly,
        topic_id: None,
        include_future: false,
    };
    let issued = s.create_connection(&scope(), conn("client"))?;
    let other = s.create_connection(&scope(), conn("other"))?;
    let reg = |request: &str, id: &str| SessionRegistration {
        request_id: request.into(),
        connection_id: id.into(),
        lifetime_seconds: 300,
    };
    let from = s.register_continuity_session(&scope(), reg("from", &issued.connection.id))?;
    let target = s.register_continuity_session(&scope(), reg("target", &issued.connection.id))?;
    let other_session =
        s.register_continuity_session(&scope(), reg("other-target", &other.connection.id))?;
    let h = s.create_handoff(
        &scope(),
        CreateHandoff {
            request_id: "handoff".into(),
            from,
            recipient: target.clone(),
            lifetime_seconds: 180,
            memories: c.output_versions,
            content: HandoffContent {
                summary: "SYNTHETIC HANDOFF BODY".into(),
                completed: vec![],
                open_tasks: vec!["review fixture".into()],
                decisions: vec![],
                failed_attempts: vec![],
                constraints: vec![],
                next_steps: vec!["This text does not grant tool execution".into()],
            },
        },
    )?;
    Ok(Fixture {
        _dir: d,
        service: IntakeApplicationService::new(db, key_file, scope()),
        token: issued.token,
        connection_id: issued.connection.id,
        session: target,
        other_session,
        handoff_id: h.handoff_id,
        source_id: receipt.source.object_id,
    })
}
fn call(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(serde_json::from_value(args).unwrap())
}
#[test]
fn service_layer_requires_opt_in_and_cannot_impersonate_owner_registered_session()
-> anyhow::Result<()> {
    let f = fixture()?;
    let access = f.service.authenticate(&f.token)?;
    assert!(
        f.service
            .list_handoffs(&access, &f.session.session_id, None)
            .is_err()
    );
    let service = f.service.clone().with_continuity();
    assert!(
        service
            .list_handoffs(&access, &f.other_session.session_id, None)
            .is_err()
    );
    let page = service.list_handoffs(&access, &f.session.session_id, None)?;
    assert_eq!(page.page.items.len(), 1);
    assert!(!serde_json::to_string(&page)?.contains("SYNTHETIC HANDOFF BODY"));
    let brief = service.startup_brief(
        &access,
        AutomaticBrief {
            session_id: f.session.session_id.clone(),
            pinned: vec![],
            constraints: vec![],
            max_items: 5,
            max_bytes: 8192,
        },
    )?;
    assert_eq!(brief.handoffs_claimed, 0);
    assert_eq!(brief.pending_handoffs.len(), 1);
    let recap = service.compaction_recap(
        &access,
        AutomaticRecap {
            session_id: f.session.session_id.clone(),
            first_sequence: 1,
            last_sequence: 2,
            max_bytes: 8192,
        },
    )?;
    assert!(recap.incomplete);
    assert!(recap.events.is_empty());
    service
        .open()?
        .revoke_connection(&scope(), &f.connection_id)?;
    assert!(
        service
            .claim_handoff(&access, &f.session.session_id, &f.handoff_id, "claim")
            .is_err()
    );
    Ok(())
}
#[test]
fn service_budget_exhaustion_and_source_erasure_refuse_claim() -> anyhow::Result<()> {
    let f = fixture()?;
    let service = f.service.clone().with_continuity();
    let access = service.authenticate(&f.token)?;
    for _ in 0..30 {
        service.list_handoffs(&access, &f.session.session_id, None)?;
    }
    assert!(
        service
            .claim_handoff(&access, &f.session.session_id, &f.handoff_id, "claim")
            .is_err()
    );
    assert_eq!(
        service
            .open()?
            .list_handoffs(&scope(), &f.session, None)?
            .items[0]
            .1
            .state,
        HandoffState::Pending
    );
    service.open()?.delete_source(&scope(), &f.source_id)?;
    assert!(
        service
            .list_handoffs(&access, &f.session.session_id, None)
            .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn opt_in_http_discovery_preserves_original_three_tool_schemas_and_rejects_owner_spoofing()
-> anyhow::Result<()> {
    let f = fixture()?;
    let base_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base_addr = base_listener.local_addr()?;
    let base = router(f.service.clone(), base_addr.to_string());
    let base_task = tokio::spawn(async move { axum::serve(base_listener, base).await });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(
        f.service.clone().with_continuity().with_ranked_retrieval(),
        addr.to_string(),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{base_addr}/mcp/v03"))
                .auth_header(f.token.clone()),
        ))
        .await?;
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp/v03"))
                .auth_header(f.token.clone()),
        ))
        .await?;
    let original = base_client.list_all_tools().await?;
    let extended = client.list_all_tools().await?;
    assert_eq!(original.len(), 3);
    assert_eq!(extended.len(), 11);
    for old in original {
        let new = extended.iter().find(|t| t.name == old.name).unwrap();
        assert_eq!(serde_json::to_value(&old)?, serde_json::to_value(new)?);
    }
    assert!(!extended.iter().any(|t| matches!(
        t.name.as_ref(),
        "register_session" | "create_handoff" | "cancel_handoff"
    )));
    for args in [
        json!({"session_id":f.other_session.session_id}),
        json!({"session_id":f.session.session_id,"owner_id":"owner:other"}),
        json!({"session_id":f.session.session_id,"connection_id":f.connection_id}),
    ] {
        assert_eq!(
            client
                .call_tool(call("list_handoffs", args))
                .await?
                .is_error,
            Some(true)
        );
    }
    let brief = client
        .call_tool(call(
            "startup_brief",
            json!({"session_id":f.session.session_id}),
        ))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(brief["handoffs_claimed"], 0);
    assert!(!brief.to_string().contains("SYNTHETIC HANDOFF BODY"));
    assert!(!brief.to_string().contains("SYNTHETIC RAW SOURCE"));
    let first = call(
        "claim_handoff",
        json!({"session_id":f.session.session_id,"handoff_id":f.handoff_id,"request_id":"one"}),
    );
    let other = call(
        "claim_handoff",
        json!({"session_id":f.session.session_id,"handoff_id":f.handoff_id,"request_id":"other"}),
    );
    let (a, b) = tokio::join!(
        client.call_tool(first.clone()),
        client.call_tool(other.clone())
    );
    let a = a?;
    let b = b?;
    assert_ne!(a.is_error == Some(true), b.is_error == Some(true));
    let (winner, request) = if a.is_error != Some(true) {
        (a, first)
    } else {
        (b, other)
    };
    let winner = winner.structured_content.unwrap();
    assert_eq!(winner["envelope"]["confers_execution_authority"], false);
    assert_eq!(winner["receipt"]["status"], "claimed_not_confirmed_used");
    let replay = client
        .call_tool(request.clone())
        .await?
        .structured_content
        .unwrap();
    assert_eq!(
        replay["receipt"]["receipt_id"],
        winner["receipt"]["receipt_id"]
    );
    f.service.open()?.withdraw_source(&scope(), &f.source_id)?;
    assert_eq!(
        client.call_tool(request.clone()).await?.is_error,
        Some(true)
    );
    f.service
        .open()?
        .revoke_connection(&scope(), &f.connection_id)?;
    assert!(client.call_tool(request).await.is_err());
    let _ = client.cancel().await;
    let _ = base_client.cancel().await;
    task.abort();
    base_task.abort();
    Ok(())
}
