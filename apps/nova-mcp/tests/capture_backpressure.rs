use nova_core::{compilation::*, intake::*, *};
use nova_mcp::{
    automatic::IntakeApplicationService,
    capture::{CaptureIngressLimits, router_with_limits},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn slow_ingress_has_bounded_admission_no_false_ack_and_releases_capacity()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("intake.sqlite");
    let keyfile = dir.path().join("vault.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&keyfile)?;
    let scope = AssetScope {
        owner_id: "owner:backpressure-fixture".into(),
        space: "test".into(),
    };
    let mut store = IntakeStore::create(&db, key, scope.clone())?;
    store.enable_capture(&scope)?;
    let grant = store.create_capture_grant(
        &scope,
        CaptureGrantRequest {
            request_id: "fixture".into(),
            source_namespace: "fixture".into(),
            host: "codex".into(),
            surface: "cli_hooks".into(),
            host_version: "synthetic".into(),
            events: vec![CaptureEventKind::UserPrompt],
            lifetime_seconds: 600,
            event_budget: 10,
            byte_budget: 8192,
        },
    )?;
    let service = IntakeApplicationService::new(db, keyfile, scope.clone());
    assert!(
        router_with_limits(
            service.clone(),
            "127.0.0.1:9".into(),
            CaptureIngressLimits {
                maximum_in_flight: 0,
                body_deadline: Duration::from_secs(1)
            }
        )
        .is_err()
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = router_with_limits(
        service,
        address.to_string(),
        CaptureIngressLimits {
            maximum_in_flight: 1,
            body_deadline: Duration::from_millis(500),
        },
    )?;
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    let url = format!("http://{address}/capture/v01/session");
    let client = reqwest::Client::new();
    let mut slow = tokio::net::TcpStream::connect(address).await?;
    slow.write_all(format!("POST /capture/v01/session HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{{",grant.token).as_bytes()).await?;
    let mut busy_seen = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let r = client
            .post(&url)
            .bearer_auth(&grant.token)
            .json(&json!({}))
            .send()
            .await?;
        if r.status() == 503 {
            assert_eq!(r.headers()["retry-after"], "1");
            let b: Value = r.json().await?;
            assert_eq!(b["saved"], false);
            assert_eq!(b["phase"], "before_dispatch");
            busy_seen = true;
            break;
        }
        assert_eq!(r.status(), 400);
    }
    assert!(busy_seen);
    assert!(store.jobs(&scope, None)?.is_empty());
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), slow.read_to_end(&mut bytes)).await??;
    let response = String::from_utf8(bytes)?;
    assert!(response.starts_with("HTTP/1.1 408"));
    assert!(response.contains("NOVA_CAPTURE_BODY_TIMEOUT"));
    assert!(response.contains("\"saved\":false"));
    let large = client
        .post(&url)
        .bearer_auth(&grant.token)
        .body(vec![b'x'; 96 * 1024 + 1])
        .send()
        .await?;
    assert_eq!(large.status(), 413);
    assert_eq!(large.json::<Value>().await?["saved"], false);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", grant.token).parse()?,
    );
    headers.append(reqwest::header::AUTHORIZATION, "Bearer different".parse()?);
    assert_eq!(
        client
            .post(&url)
            .headers(headers)
            .json(&json!({"origin_session_id":"one"}))
            .send()
            .await?
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&grant.token)
            .json(&json!({"origin_session_id":"one"}))
            .send()
            .await?
            .status(),
        200
    );
    assert!(store.jobs(&scope, None)?.is_empty());
    task.abort();
    Ok(())
}
