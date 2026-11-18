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
fn ingest(
    s: &mut IntakeStore,
    id: &str,
    value: &str,
    occurred_at: Option<&str>,
) -> anyhow::Result<IntakeReceipt> {
    let r=s.receive(&IntakeAuthority {scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"fixture","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC RAW EVIDENCE NEVER DISCLOSED","occurred_at":null}))?)?;
    let j = s.claim_next(&scope(), 60)?.unwrap();
    let l = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &l.lease_id)?;
    s.authorize_dispatch(&scope(), &j.job.job_id, &l.lease_id, 1000)?;
    s.commit_compilation(&scope(),&j.job.job_id,&l.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:fixture","predicate":format!("work:{id}"),"value":value,"polarity":"negative","modality":"asserted","conditions":{"work:scope":"工作"}},"occurred_at":occurred_at,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC RAW EVIDENCE NEVER DISCLOSED","occurrence":0}]}]}))?)?;
    Ok(r)
}
fn fixture(
    multiple_hits: bool,
) -> anyhow::Result<(
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
    let anchor_time = multiple_hits.then_some("2026-10-10T12:00:00Z");
    let source = ingest(
        &mut s,
        "visible",
        "工作场景不参加超过三十分钟的会议",
        anchor_time,
    )?;
    if multiple_hits {
        for hour in 13..20 {
            ingest(
                &mut s,
                &format!("neighbor-{hour}"),
                &format!("工作会议第{hour}项记录，客户现场另行安排"),
                Some(&format!("2026-10-10T{hour}:00:00Z")),
            )?;
        }
    }
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
    ingest(
        &mut s,
        "not-granted",
        "SECRET_UNAPPROVED 工作 会议",
        anchor_time,
    )?;
    Ok((
        d,
        IntakeApplicationService::new(db, kf, scope()),
        c,
        source.source.object_id,
    ))
}
fn query(id: &str) -> ConnectedQuery {
    ConnectedQuery {
        request_id: id.into(),
        query: Some("工作 会议".into()),
        memory_id: None,
        max_items: 5,
    }
}
fn call(name: &str, args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(serde_json::from_value(args).unwrap())
}
#[test]
fn ranked_service_is_explicit_and_uses_existing_scope_budget_and_replay() -> anyhow::Result<()> {
    let (_d, base, c, _) = fixture(false)?;
    let a = base.authenticate(&c.token)?;
    assert!(base.query_ranked(&a, query("rank")).is_err());
    let service = base.with_ranked_retrieval();
    assert!(service.query(&a, query("same"))?.items.is_empty());
    let first = service.query_ranked(&a, query("same"))?;
    assert_eq!(first.delivery.items.len(), 1);
    assert_eq!(first.delivery.items[0].content.polarity, Polarity::Negative);
    assert!(!serde_json::to_string(&first)?.contains("SECRET_UNAPPROVED"));
    let repeat = service.query_ranked(&a, query("same"))?;
    assert_eq!(
        repeat.delivery.receipt.receipt_id,
        first.delivery.receipt.receipt_id
    );
    for i in 0..28 {
        service.query_ranked(&a, query(&format!("q-{i}")))?;
    }
    assert!(service.query_ranked(&a, query("exhausted")).is_err());
    assert_eq!(service.open()?.connections(&scope())?[0].used_calls, 30);
    Ok(())
}
#[tokio::test]
async fn ranked_http_is_separate_from_legacy_filters_first_and_blocks_withdrawn_replay()
-> anyhow::Result<()> {
    let (_d, service, c, source) = fixture(false)?;
    let service = service.with_ranked_retrieval();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(service.clone(), addr.to_string());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp/v03"))
                .auth_header(c.token),
        ))
        .await?;
    let tools = client.list_all_tools().await?;
    assert_eq!(tools.len(), 7);
    assert!(tools.iter().any(|t| t.name == "search_memory"));
    assert!(tools.iter().any(|t| t.name == "search_memory_ranked"));
    let args = json!({"request_id":"same","query":"工作 会议","max_items":5});
    let legacy = client
        .call_tool(call("search_memory", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(legacy["items"].as_array().unwrap().len(), 0);
    let ranked = client
        .call_tool(call("search_memory_ranked", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(ranked["delivery"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(ranked["algorithm"], RANKING_VERSION);
    assert!(!ranked.to_string().contains("SECRET_UNAPPROVED"));
    assert!(!ranked.to_string().contains("SYNTHETIC RAW EVIDENCE"));
    assert_eq!(ranked["trace"].as_array().unwrap().len(), 1);
    let index_args = json!({"request_id":"index","query":"工作 会议"});
    let index_result = client
        .call_tool(call("search_memory_index", index_args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(index_result["items"].as_array().unwrap().len(), 1);
    assert_eq!(index_result["receipt"]["view"], "compiled_unverified_index");
    let index_json = serde_json::to_string(&index_result)?;
    for excluded in [
        "三十分钟",
        "SECRET_UNAPPROVED",
        "SYNTHETIC RAW EVIDENCE",
        "source_metadata",
    ] {
        assert!(!index_json.contains(excluded));
    }
    let memory = &index_result["items"][0]["memory"];
    let unknown_time = client
        .call_tool(call(
            "read_memory_neighborhood",
            json!({
                "request_id":"unknown-time", "window_days":1,
                "anchor":{"memory_id":memory["object_id"],"revision":memory["revision"]}
            }),
        ))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(unknown_time["receipt"]["status"], "anchor_time_unknown");
    assert_eq!(unknown_time["items"], json!([]));
    let detail = client
        .call_tool(call(
            "read_memory_detail",
            json!({
                "request_id":"detail",
                "memory":{"memory_id":memory["object_id"],"revision":memory["revision"]}
            }),
        ))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(detail["items"], ranked["delivery"]["items"]);
    let basic = client
        .call_tool(call(
            "search_memory",
            json!({
                "request_id":"single-call","query":"工作"
            }),
        ))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(basic["items"], detail["items"]);
    let basic_bytes = serde_json::to_vec(&basic)?.len();
    let detail_bytes = serde_json::to_vec(&detail)?.len();
    assert!(index_json.len() < basic_bytes);
    println!(
        "one-hit structured JSON: basic={basic_bytes}, index={}, detail={detail_bytes}",
        index_json.len()
    );
    let replay = client
        .call_tool(call("search_memory_ranked", args.clone()))
        .await?
        .structured_content
        .unwrap();
    assert_eq!(
        replay["delivery"]["receipt"]["receipt_id"],
        ranked["delivery"]["receipt"]["receipt_id"]
    );
    assert_eq!(
        client
            .call_tool(call(
                "search_memory_ranked",
                json!({"request_id":"spoof","query":"工作","owner_id":"owner:other"})
            ))
            .await?
            .is_error,
        Some(true)
    );
    service.open()?.withdraw_source(&scope(), &source)?;
    assert_eq!(
        client
            .call_tool(call("search_memory_index", index_args))
            .await?
            .is_error,
        Some(true)
    );
    assert_eq!(
        client
            .call_tool(call("search_memory_ranked", args))
            .await?
            .is_error,
        Some(true)
    );
    service
        .open()?
        .revoke_connection(&scope(), &c.connection.id)?;
    assert!(
        client
            .call_tool(call(
                "search_memory_ranked",
                json!({"request_id":"new","query":"工作"})
            ))
            .await
            .is_err()
    );
    let _ = client.cancel().await;
    task.abort();
    Ok(())
}

#[tokio::test]
async fn multi_hit_progressive_retrieval_preserves_selected_evidence_and_reduces_payload()
-> anyhow::Result<()> {
    let (_d, service, c, _) = fixture(true)?;
    let service = service.with_ranked_retrieval();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = router(service.clone(), addr.to_string());
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp/v03"))
                .auth_header(c.token),
        ))
        .await?;

    let fetch = |name, params| {
        let client = &client;
        async move {
            let started = std::time::Instant::now();
            let reply = client.call_tool(call(name, params)).await?;
            let result_bytes = serde_json::to_vec(&reply)?.len();
            let content = reply
                .structured_content
                .ok_or_else(|| anyhow::anyhow!("missing structured content"))?;
            Ok::<_, anyhow::Error>((
                content,
                result_bytes,
                started.elapsed().as_secs_f64() * 1_000.0,
            ))
        }
    };
    let (basic, basic_bytes, basic_ms) = fetch(
        "search_memory",
        json!({
            "request_id":"multi-basic","query":"工作","max_items":8
        }),
    )
    .await?;
    let (index, index_bytes, index_ms) = fetch(
        "search_memory_index",
        json!({
            "request_id":"multi-index","query":"工作","max_items":8
        }),
    )
    .await?;
    let hits = index["items"].as_array().unwrap();
    assert_eq!(hits.len(), 8);
    assert_eq!(basic["items"].as_array().unwrap().len(), 8);
    assert_eq!(basic["more_available"], false);
    assert_eq!(index["more_available"], false);
    let selected = &hits
        .iter()
        .find(|hit| hit["predicate"] == "work:visible")
        .unwrap()["memory"];
    let reference = json!({"memory_id":selected["object_id"],"revision":selected["revision"]});
    let (detail, detail_bytes, detail_ms) = fetch(
        "read_memory_detail",
        json!({
            "request_id":"multi-detail","memory":reference
        }),
    )
    .await?;
    let baseline = basic["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| &item["memory"] == selected)
        .unwrap();
    assert_eq!(&detail["items"][0], baseline);
    assert_eq!(detail["items"][0]["content"]["polarity"], "negative");
    assert_eq!(
        detail["items"][0]["content"]["conditions"]["work:scope"],
        "工作"
    );

    let (nearby, nearby_bytes, nearby_ms) = fetch(
        "read_memory_neighborhood",
        json!({
            "request_id":"multi-nearby","anchor":reference,"window_days":1,"max_items":2
        }),
    )
    .await?;
    assert_eq!(nearby["items"][0]["predicate"], "work:neighbor-13");
    assert_eq!(nearby["items"][1]["predicate"], "work:neighbor-14");
    assert_eq!(nearby["more_available"], true);
    assert_eq!(nearby["receipt"]["memory_versions"][0], *selected);
    for response in [&basic, &index, &detail, &nearby] {
        let text = response.to_string();
        assert!(!text.contains("SECRET_UNAPPROVED"));
        assert!(!text.contains("SYNTHETIC RAW EVIDENCE"));
    }
    let index_plus_detail = index_bytes + detail_bytes;
    let all_stages = index_plus_detail + nearby_bytes;
    assert!(index_plus_detail < basic_bytes);
    assert!(all_stages < basic_bytes);
    let usage = service.open()?.connection_usage(&scope(), None)?;
    assert_eq!(usage.len(), 4);
    assert_eq!(usage[0].1.operation, "read_index");
    println!(
        "8-hit MCP result JSON bytes: basic={basic_bytes}, index={index_bytes}, detail={detail_bytes}, neighborhood={nearby_bytes}; tool wall ms: {basic_ms:.2}, {index_ms:.2}, {detail_ms:.2}, {nearby_ms:.2}"
    );
    let _ = client.cancel().await;
    task.abort();
    Ok(())
}
