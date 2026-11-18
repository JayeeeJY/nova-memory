//! Rejected synthetic model output must remain reviewable without another dispatch.
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct Fixture {
    calls: Arc<AtomicUsize>,
    schema_reject: bool,
    transport_failure: Option<&'static str>,
}

async fn responder(State(fixture): State<Fixture>) -> Response {
    fixture.calls.fetch_add(1, Ordering::SeqCst);
    if fixture.transport_failure == Some("timeout") {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    if fixture.transport_failure.is_some() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("x-provider-private", "never-copy-http-header")],
            "never-copy-http-error-body",
        )
            .into_response();
    }
    let content = if fixture.schema_reject {
        json!({"schema_version":"unsupported-synthetic-schema","units":[]})
    } else {
        json!({"schema_version":"nova.extraction.v0.1","units":[{
            "origin":"extracted",
            "content":{"subject":"user","predicate":"acceptance:preference","value":"fictional","polarity":"positive","modality":"asserted","conditions":{}},
            "occurred_at":null,"valid_time":{"kind":"unknown"},
            "quotes":[{"quote":"This synthetic quote does not occur in the frozen source.","occurrence":0}]
        }]})
    };
    Json(
        json!({"choices":[{"finish_reason":"stop","message":{"content":content.to_string(),"provider_private":"never-copy-message-extra"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120},
        "provider_private":"never-copy-envelope-extra"}),
    ).into_response()
}

async fn rejected_run(
    schema_reject: bool,
    transport_failure: Option<&'static str>,
) -> (Vec<Value>, usize) {
    let fixture = Fixture {
        calls: Arc::new(AtomicUsize::new(0)),
        schema_reject,
        transport_failure,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(responder))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let events = tokio::task::spawn_blocking(move || {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().canonicalize().unwrap().join("fresh");
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova-review-acceptance"))
            .arg("--profile").arg(&profile).stdin(Stdio::piped()).stdout(Stdio::piped())
            .stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let metadata = nova_review::acceptance::metadata();
        let begin = json!({"command":"begin","schema_version":"nova.acceptance.execute.v0.1",
            "plan_id":"a".repeat(64),"cases_sha256":metadata["cases_sha256"],"prompt_sha256":metadata["prompt_sha256"],
            "provider_alias":"synthetic-responder","model_alias":"synthetic-fixture",
            "selection":{"location":"local","model_id":"synthetic-fixture","endpoint":endpoint},
            "budget":{"maximum_dispatches":10,"maximum_seconds":60,"per_call_seconds":if transport_failure == Some("timeout") { 1 } else { 5 },
                "maximum_input_bytes":131072,"maximum_output_tokens":2048,"currency":"USD",
                "input_rate_micros_per_million":1000000,"output_rate_micros_per_million":2000000,
                "maximum_cost_micros":2000000,"maximum_charged_input_tokens":131200,
                "maximum_charged_output_tokens":2048,"pricing_attestation":"configured_envelope_includes_all_billed_tokens"}});
        writeln!(stdin,"{begin}").unwrap();
        stdin.flush().unwrap();
        let bound: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(bound["event"], "bound");
        let step = json!({"command":"step","sequence":1,"case_id":"negation_context","run_nonce":bound["run_nonce"]});
        writeln!(stdin,"{step}").unwrap();
        stdin.flush().unwrap();
        drop(stdin);
        let mut events = vec![bound];
        for line in lines { events.push(serde_json::from_str(&line.unwrap()).unwrap()); }
        assert!(!child.wait().unwrap().success());
        events
    }).await.unwrap();
    server.abort();
    (events, fixture.calls.load(Ordering::SeqCst))
}

fn assert_reviewable_failure<'a>(events: &'a [Value], phase: &str) -> &'a Value {
    let step = events
        .iter()
        .find(|event| event["event"] == "step_completed")
        .unwrap();
    assert_eq!(step["status"], "fail");
    assert_eq!(step["dispatches"], 1);
    assert_eq!(step["execution_failure"]["mechanical_only"], true);
    assert_eq!(step["execution_failure"]["phase"], phase);
    assert_eq!(
        step["execution_failure"]["error_code"],
        "NOVA_VALIDATION_FAILED"
    );
    assert_eq!(step["model_diagnostic"]["mechanical_only"], true);
    assert_eq!(step["model_diagnostic"]["truncated"], false);
    assert_eq!(
        step["semantic_acceptance"],
        "pending_independent_gold_and_review"
    );
    assert_eq!(step["attempt"]["status"], "failed");
    assert_eq!(step["attempt"]["usage"]["input_total_tokens"], "100");
    assert_eq!(step["envelope_reserved_micros"], 0);
    assert!(step["estimated_settled_cost_micros"].as_u64().unwrap() > 0);
    let encoded = serde_json::to_string(events).unwrap();
    assert!(!encoded.contains("never-copy-"));
    assert!(!events.iter().any(|event| event["event"] == "finished"));
    assert_eq!(events.last().unwrap()["event"], "aborted");
    &step["model_diagnostic"]
}

#[tokio::test]
async fn extraction_schema_rejection_retains_only_bounded_content_after_one_dispatch() {
    let (events, calls) = rejected_run(true, None).await;
    assert_eq!(calls, 1);
    let diagnostic = assert_reviewable_failure(&events, "decode");
    let content = diagnostic["model_output_utf8"].as_str().unwrap();
    assert!(content.contains("unsupported-synthetic-schema"));
    assert_eq!(diagnostic["model_output_bytes"], content.len());
    assert_eq!(
        diagnostic["model_output_sha256"].as_str().unwrap().len(),
        64
    );
}

#[tokio::test]
async fn ungrounded_quote_commit_rejection_retains_output_and_paid_usage_without_retry() {
    let (events, calls) = rejected_run(false, None).await;
    assert_eq!(calls, 1);
    let diagnostic = assert_reviewable_failure(&events, "commit");
    assert!(
        diagnostic["model_output_utf8"]
            .as_str()
            .unwrap()
            .contains("does not occur in the frozen source")
    );
}

#[tokio::test]
async fn http_failure_and_timeout_are_distinct_private_and_stop_without_retry() {
    for (mode, expected_exit, expected_status) in
        [("http", 22, json!(500)), ("timeout", 28, Value::Null)]
    {
        let (events, calls) = rejected_run(false, Some(mode)).await;
        assert_eq!(calls, 1);
        let step = events
            .iter()
            .find(|e| e["event"] == "step_completed")
            .unwrap();
        assert_eq!(step["status"], "fail");
        assert_eq!(step["execution_failure"]["phase"], "transport");
        assert_eq!(
            step["transport_diagnostic"]["curl_exit_code"],
            expected_exit
        );
        assert_eq!(step["transport_diagnostic"]["http_status"], expected_status);
        assert_eq!(step["transport_diagnostic"]["automatic_retries"], 0);
        assert!(step["model_diagnostic"].is_null());
        assert_eq!(step["attempt"]["usage"]["status"], "missing");
        assert_eq!(step["dispatches"], 1);
        assert_eq!(events.last().unwrap()["event"], "aborted");
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("never-copy-")
        );
    }
}
