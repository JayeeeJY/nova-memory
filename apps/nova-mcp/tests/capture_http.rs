use nova_core::{compilation::*, intake::*, *};
use nova_mcp::{automatic::IntakeApplicationService, capture::router};
use serde_json::{Value, json};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:fixture".into(),
        space: "test".into(),
    }
}
#[tokio::test]
async fn http_capture_is_scoped_durable_replayable_and_not_owner_or_read_authority()
-> anyhow::Result<()> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("intake.sqlite");
    let key = d.path().join("test.key");
    let k = VaultKey::generate()?;
    k.write_new_file(&key)?;
    let mut store = IntakeStore::create(&db, k, scope())?;
    store.enable_compilation(&scope())?;
    store.enable_capture(&scope())?;
    store.enable_connections(&scope())?;
    let issued = store.create_capture_grant(
        &scope(),
        CaptureGrantRequest {
            request_id: "capture".into(),
            source_namespace: "fixture".into(),
            host: "codex".into(),
            surface: "cli_hooks".into(),
            host_version: "synthetic".into(),
            events: vec![CaptureEventKind::UserPrompt],
            lifetime_seconds: 600,
            event_budget: 1,
            byte_budget: 4096,
        },
    )?;
    let reader = store.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "reader".into(),
            label: "synthetic reader".into(),
            host: ConnectionHost::Codex,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: true,
        },
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");
    let app = router(
        IntakeApplicationService::new(db, key, scope()),
        addr.to_string(),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let http = reqwest::Client::new();
    for token in ["bogus", reader.token.as_str()] {
        assert_eq!(
            http.post(format!("{url}/capture/v01/session"))
                .bearer_auth(token)
                .json(&json!({"origin_session_id":"s1"}))
                .send()
                .await?
                .status(),
            401
        );
    }
    assert_eq!(
        http.post(format!("{url}/capture/v01/session"))
            .bearer_auth(&issued.token)
            .header("Origin", "https://evil.invalid")
            .json(&json!({"origin_session_id":"s1"}))
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        http.post(format!("{url}/capture/v01/session"))
            .bearer_auth(&issued.token)
            .header("Host", "foreign.invalid")
            .json(&json!({"origin_session_id":"s1"}))
            .send()
            .await?
            .status(),
        401
    );
    let res = http
        .post(format!("{url}/capture/v01/session"))
        .bearer_auth(&issued.token)
        .json(&json!({"origin_session_id":"s1"}))
        .send()
        .await?;
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["cache-control"], "no-store");
    let session: Value = res.json().await?;
    let mut event = json!({"schema_version":CAPTURE_VERSION,"session_id":session["session_id"],"event_id":"e1","sequence":1,"kind":"user_prompt","content":"SYNTHETIC private text","occurred_at":null,"loss":{"truncated":false,"redacted":false,"partial_history":true}});
    let saved = http
        .post(format!("{url}/capture/v01/events"))
        .bearer_auth(&issued.token)
        .json(&event)
        .send()
        .await?;
    assert_eq!(saved.status(), 200);
    let ack: Value = saved.json().await?;
    assert!(!ack.to_string().contains("SYNTHETIC private text"));
    assert_eq!(ack["receipt"]["status"], "received");
    let replay: Value = http
        .post(format!("{url}/capture/v01/events"))
        .bearer_auth(&issued.token)
        .json(&event)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(ack, replay);
    let provenance = json!({"session_id":session["session_id"],"event_id":"e1","kind":"user_prompt","origin":"user_provided_export"});
    let described = http
        .post(format!("{url}/capture/v01/provenance"))
        .bearer_auth(&issued.token)
        .json(&provenance)
        .send()
        .await?;
    assert_eq!(described.status(), 200);
    assert_eq!(described.headers()["cache-control"], "no-store");
    let description: Value = described.json().await?;
    assert_eq!(description["source"], ack["receipt"]["source"]);
    assert_eq!(description["event_key"], ack["event_key"]);
    assert_eq!(description["origins"], json!(["user_provided_export"]));
    assert!(!description.to_string().contains("SYNTHETIC private text"));
    assert_eq!(
        http.post(format!("{url}/capture/v01/provenance"))
            .bearer_auth(&reader.token)
            .json(&provenance)
            .send()
            .await?
            .status(),
        401
    );
    let mut unknown = provenance.clone();
    unknown["source_id"] = json!("forged");
    assert_eq!(
        http.post(format!("{url}/capture/v01/provenance"))
            .bearer_auth(&issued.token)
            .json(&unknown)
            .send()
            .await?
            .status(),
        400
    );
    let mut future = provenance.clone();
    future["event_id"] = json!("unreceived");
    assert!(
        !http
            .post(format!("{url}/capture/v01/provenance"))
            .bearer_auth(&issued.token)
            .json(&future)
            .send()
            .await?
            .status()
            .is_success()
    );
    event["event_id"] = json!("e2");
    event["sequence"] = json!(2);
    let budget = http
        .post(format!("{url}/capture/v01/events"))
        .bearer_auth(&issued.token)
        .json(&event)
        .send()
        .await?;
    assert_eq!(budget.status(), 429);
    assert!(budget.headers().get("retry-after").is_none());
    assert_eq!(
        budget.json::<Value>().await?["code"],
        "NOVA_CAPTURE_BUDGET_EXHAUSTED"
    );
    assert_eq!(store.jobs(&scope(), None)?.len(), 1);
    assert_eq!(
        http.post(format!("{url}/capture/v01/events"))
            .bearer_auth(&issued.token)
            .body("x".repeat(100 * 1024))
            .send()
            .await?
            .status(),
        413
    );
    assert_eq!(
        http.post(format!("{url}/api/clients"))
            .bearer_auth(&issued.token)
            .send()
            .await?
            .status(),
        404
    );
    store.revoke_capture(&scope(), &issued.grant.grant_id)?;
    assert_eq!(
        http.post(format!("{url}/capture/v01/provenance"))
            .bearer_auth(&issued.token)
            .json(&provenance)
            .send()
            .await?
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{url}/capture/v01/events"))
            .bearer_auth(&issued.token)
            .json(&event)
            .send()
            .await?
            .status(),
        401
    );
    task.abort();
    Ok(())
}
