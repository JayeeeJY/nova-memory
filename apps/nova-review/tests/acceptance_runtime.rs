//! Fixed local responders exercise the gate and transactions, never semantic/Gold acceptance.
use axum::{Json, Router, extract::State, routing::post};
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
    mode: &'static str,
}
async fn responder(State(f): State<Fixture>, Json(body): Json<Value>) -> Json<Value> {
    f.calls.fetch_add(1, Ordering::SeqCst);
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let source = input["source_text"].as_str().unwrap();
    let output = json!({"schema_version":"nova.extraction.v0.1","units":[{"origin":"extracted","content":{"subject":"user","predicate":"acceptance:preference","value":if input["correction_target"].is_null(){"before"}else{"after"},"polarity":"positive","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":source,"occurrence":0}]}]});
    let mut response =
        json!({"choices":[{"finish_reason":"stop","message":{"content":output.to_string()}}]});
    if f.mode == "timeout" {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    if f.mode != "missing" {
        response["usage"] = json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,"prompt_tokens_details":{"cached_tokens":10}});
    }
    if f.mode == "partial" {
        response["usage"]
            .as_object_mut()
            .unwrap()
            .remove("prompt_tokens_details");
    }
    if f.mode == "output" {
        response["usage"]["completion_tokens"] = json!(3000);
        response["usage"]["total_tokens"] = json!(3100);
    }
    Json(response)
}
fn begin(endpoint: &str) -> Value {
    let m = nova_review::acceptance::metadata();
    json!({"command":"begin","schema_version":"nova.acceptance.execute.v0.1","plan_id":"a".repeat(64),"cases_sha256":m["cases_sha256"],"prompt_sha256":m["prompt_sha256"],"provider_alias":"fixed-responder","model_alias":"fixed-fixture","selection":{"location":"local","model_id":"fixed-fixture-not-a-real-model","endpoint":endpoint},"budget":{"maximum_dispatches":10,"maximum_seconds":60,"per_call_seconds":5,"maximum_input_bytes":131072,"maximum_output_tokens":2048,"currency":"USD","input_rate_micros_per_million":1000000,"output_rate_micros_per_million":2000000,"maximum_cost_micros":2000000,"maximum_charged_input_tokens":131200,"maximum_charged_output_tokens":2048,"pricing_attestation":"configured_envelope_includes_all_billed_tokens"}})
}
async fn exercise(mode: &'static str) -> (Vec<Value>, usize, bool) {
    let fixture = Fixture {
        calls: Arc::new(AtomicUsize::new(0)),
        mode,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(responder))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let result=tokio::task::spawn_blocking(move||{
        let d=tempfile::tempdir().unwrap();let profile=d.path().canonicalize().unwrap().join("fresh");
        let mut child=Command::new(env!("CARGO_BIN_EXE_nova-review-acceptance")).arg("--profile").arg(&profile).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin=child.stdin.take().unwrap();let mut lines=BufReader::new(child.stdout.take().unwrap()).lines();
        let mut config=begin(&endpoint);
        if mode=="money"{config["budget"]["maximum_cost_micros"]=json!(1);}
        if mode=="input"{config["budget"]["maximum_input_bytes"]=json!(1024);config["budget"]["maximum_charged_input_tokens"]=json!(1152);}
        if mode=="timeout"{config["budget"]["maximum_seconds"]=json!(2);config["budget"]["per_call_seconds"]=json!(1);}
        if mode=="remote"{config["selection"]=json!({"location":"remote","model_id":"fixed","endpoint":"https://example.com/v1","allow_selected_source":true});}
        writeln!(stdin,"{config}").unwrap();stdin.flush().unwrap();
        let mut events=vec![];
        if let Some(Ok(line))=lines.next(){events.push(serde_json::from_str::<Value>(&line).unwrap());}
        if mode!="remote" {
            let nonce=events[0]["run_nonce"].as_str().unwrap().to_owned();
            let catalog:Value=serde_json::from_str(include_str!("../../../eval/model-acceptance/cases.json")).unwrap();
            for (i,case) in catalog["cases"].as_array().unwrap().iter().enumerate(){
                let wrong=if mode=="nonce"{"0".repeat(64)}else{nonce.clone()};
                writeln!(stdin,"{}",json!({"command":"step","sequence":if mode=="replay" && i==1{1}else{i+1},"case_id":case["id"],"run_nonce":wrong})).unwrap();stdin.flush().unwrap();
                let Some(Ok(line))=lines.next()else{break};
                let first:Value=serde_json::from_str(&line).unwrap();let mut event=first.clone();events.push(first);
                if event["event"]=="dispatch_started"{event=serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();events.push(event.clone());}
                if event["status"]!="pass"{if let Some(Ok(line))=lines.next(){events.push(serde_json::from_str(&line).unwrap());}break;}
            }
        }
        if matches!(mode,"good"|"partial"|"finish_nonce") {writeln!(stdin,"{}",json!({"command":"finish","run_nonce":if mode=="finish_nonce"{json!("0".repeat(64))}else{events[0]["run_nonce"].clone()}})).unwrap();stdin.flush().unwrap();}
        drop(stdin);for line in lines{events.push(serde_json::from_str(&line.unwrap()).unwrap());}
        let success=child.wait().unwrap().success();(events,success)
    }).await.unwrap();
    server.abort();
    (result.0, fixture.calls.load(Ordering::SeqCst), result.1)
}
#[tokio::test]
async fn fixed_responder_completes_twelve_transactions_with_ten_explicit_dispatches() {
    let (events, calls, success) = exercise("good").await;
    assert!(success, "{events:?}");
    assert_eq!(calls, 10);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "dispatch_started")
            .count(),
        10
    );
    assert_eq!(events.last().unwrap()["event"], "finished");
    assert_eq!(
        events.last().unwrap()["real_model_semantic_acceptance"],
        false
    );
    assert_eq!(events.last().unwrap()["dispatches"], 10);
}
#[tokio::test]
async fn unknown_usage_stops_after_one_dispatch_and_keeps_estimated_reservation() {
    let (events, calls, success) = exercise("missing").await;
    assert!(!success);
    assert_eq!(calls, 1);
    let last = events.last().unwrap();
    assert_eq!(last["event"], "aborted");
    assert!(last["envelope_reserved_micros"].as_u64().unwrap() > 0);
    assert_eq!(last["estimated_settled_cost_micros"], 0);
    let step = events
        .iter()
        .find(|e| e["event"] == "step_completed")
        .unwrap();
    assert_eq!(step["attempt"]["usage"]["status"], "missing");
}
#[tokio::test]
async fn tiny_currency_envelope_stops_before_any_model_request() {
    let (events, calls, success) = exercise("money").await;
    assert!(!success);
    assert_eq!(calls, 0);
    assert_eq!(events.last().unwrap()["dispatches"], 0);
}
#[tokio::test]
async fn remote_model_and_wrong_nonce_fail_closed_without_model_request() {
    for mode in ["remote", "nonce"] {
        let (_, calls, success) = exercise(mode).await;
        assert!(!success);
        assert_eq!(calls, 0);
    }
}

#[tokio::test]
async fn cache_unknown_is_partial_observation_without_invented_zero() {
    let (events, calls, success) = exercise("partial").await;
    assert!(success);
    assert_eq!(calls, 10);
    let step = events
        .iter()
        .find(|e| e["event"] == "step_completed")
        .unwrap();
    assert_eq!(step["attempt"]["usage"]["status"], "partial");
    assert_eq!(step["attempt"]["usage"]["input_total_tokens"], "100");
    assert!(step["attempt"]["usage"]["input_cached_tokens"].is_null());
}
#[tokio::test]
async fn input_byte_limit_stops_before_dispatch_and_output_usage_overrun_holds_reservation() {
    let (events, calls, success) = exercise("input").await;
    assert!(!success);
    assert_eq!(calls, 0);
    assert_eq!(events.last().unwrap()["dispatches"], 0);
    let (events, calls, success) = exercise("output").await;
    assert!(!success);
    assert_eq!(calls, 1);
    assert!(
        events.last().unwrap()["envelope_reserved_micros"]
            .as_u64()
            .unwrap()
            > 0
    );
}
#[tokio::test]
async fn total_and_per_request_deadline_stop_without_automatic_retry() {
    let t = std::time::Instant::now();
    let (events, calls, success) = exercise("timeout").await;
    assert!(!success);
    assert_eq!(calls, 1);
    assert!(t.elapsed() < std::time::Duration::from_secs(4));
    assert!(
        events.last().unwrap()["envelope_reserved_micros"]
            .as_u64()
            .unwrap()
            > 0
    );
}
#[tokio::test]
async fn replayed_step_and_invalid_finish_cannot_complete_session() {
    let (events, calls, success) = exercise("replay").await;
    assert!(!success);
    assert_eq!(calls, 1);
    assert!(!events.iter().any(|e| e["event"] == "finished"));
    let (events, calls, success) = exercise("finish_nonce").await;
    assert!(!success);
    assert_eq!(calls, 10);
    assert!(!events.iter().any(|e| e["event"] == "finished"));
}
