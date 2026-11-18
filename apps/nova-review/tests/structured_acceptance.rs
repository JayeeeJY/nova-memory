//! Dedicated driver tests use only independent synthetic stores and loopback HTTP fixtures.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
const SOURCE: &str = "合成验收材料：我在工作场景不参加超过30分钟的会议；私人聚会没有这个限制。";
struct Driver {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Value>,
}
impl Driver {
    fn new() -> Self {
        Self::with_args(&[])
    }
    fn with_args(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova-structured-acceptance"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output: rx,
        }
    }
    fn next(&self) -> Value {
        self.output
            .recv_timeout(Duration::from_secs(15))
            .expect("driver response deadline")
    }
    fn send(&mut self, value: &Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn finish(&mut self, success: bool) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.success(), success);
                break;
            }
            assert!(Instant::now() < end, "driver exit deadline");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn begin(hello: &Value, port: u16) -> Value {
    json!({"command":"begin","schema_version":hello["schema_version"],"instance_id":hello["instance_id"],"binary_sha256":hello["binary_sha256"],"plan_id":"a".repeat(64),"request_sha256":hello["request_sha256"],"endpoint":format!("http://127.0.0.1:{port}"),"timeout_seconds":5})
}
fn dispatch(ready: &Value) -> Value {
    let mut command = json!({"command":"dispatch"});
    for key in [
        "instance_id",
        "binary_sha256",
        "plan_id",
        "request_sha256",
        "profile_id",
        "run_nonce",
    ] {
        command[key] = ready[key].clone();
    }
    if ready.get("source_snapshot_sha256").is_some() {
        command["source_snapshot_sha256"] = ready["source_snapshot_sha256"].clone();
    }
    command
}
fn output(quote: &str) -> String {
    json!({"schema_version":"nova.extraction.v0.1","units":[{"origin":"extracted","content":{"subject":"user","predicate":"work:meeting_limit","value":30,"polarity":"negative","modality":"asserted","conditions":{"scope:context":"work"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":quote,"occurrence":0}]}]}).to_string()
}
fn server(content: String, finish: &'static str) -> (u16, thread::JoinHandle<Value>) {
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":finish}]}),
        json!({"choices":[],"usage":{"prompt_tokens":17,"completion_tokens":23,"total_tokens":40}})
    );
    http_server(200, &wire)
}
fn http_server(status: u16, body: &str) -> (u16, thread::JoinHandle<Value>) {
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let task = thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < end);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut block = [0; 8192];
        let (offset, length) = loop {
            let n = stream.read(&mut block).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&block[..n]);
            assert!(request.len() < 128 * 1024);
            if let Some(n) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..n]).unwrap();
                assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (n + 4, length);
            }
        };
        while request.len() < offset + length {
            let n = stream.read(&mut block).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&block[..n]);
        }
        let request: Value = serde_json::from_slice(&request[offset..offset + length]).unwrap();
        let _ = stream.write_all(response.as_bytes());
        request
    });
    (port, task)
}
#[test]
fn metadata_is_offline_and_complete() {
    let metadata = nova_review::structured_acceptance::metadata().unwrap();
    assert_eq!(
        metadata["schema_version"],
        "nova.structured.acceptance.v0.1"
    );
    assert_eq!(metadata["maximum_dispatches"], 1);
    assert_eq!(metadata["output_mode"], "structured_local_prompt_v02");
    assert_eq!(metadata["profile"], "llama-v02");
    assert_eq!(metadata["model"], "llama3.2:latest");
    assert_eq!(metadata["reasoning_effort"], Value::Null);
    assert!(
        metadata["compiled_source_hashes"]
            .as_object()
            .unwrap()
            .len()
            >= 20
    );
    use sha2::{Digest, Sha256};
    assert_eq!(
        metadata["prompt_version"],
        "nova.extraction.structured-prompt.v0.2"
    );
    assert_eq!(
        metadata["prompt_sha256"],
        format!(
            "{:x}",
            Sha256::digest(nova_review::compiler::STRUCTURED_SYSTEM_PROMPT.as_bytes())
        )
    );
    assert_eq!(
        metadata["compiled_source_hashes"]["apps/nova-review/src/compiler-structured-prompt-v02.txt"],
        metadata["prompt_sha256"]
    );
    assert_ne!(
        metadata["compiled_source_hashes"]["apps/nova-review/src/compiler-prompt.txt"],
        metadata["prompt_sha256"]
    );
    assert_eq!(
        metadata["schema_sha256"],
        format!(
            "{:x}",
            Sha256::digest(nova_review::compiler_schema::SCHEMA_JSON.as_bytes())
        )
    );
    assert_eq!(
        metadata["provider_schema_sha256"],
        format!(
            "{:x}",
            Sha256::digest(nova_review::compiler_schema::provider_schema_json().as_bytes())
        )
    );
    assert_ne!(
        metadata["schema_sha256"],
        metadata["provider_schema_sha256"]
    );
    assert_eq!(
        metadata["provider_schema_version"],
        "nova.extraction.ollama-schema.v0.1"
    );
    // Independently reconstructed by the Python preparation/controller tests as well.
    assert_eq!(
        metadata["request_sha256"],
        "a63b25845794a6e38618a95d4ee55b01954fe1696a685842ebca2ce32b7a30a3"
    );
}
#[test]
fn wrong_begin_and_dispatch_binding_never_connect() {
    for mode in ["request", "instance", "endpoint", "dispatch", "extra"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut driver = Driver::new();
        let hello = driver.next();
        let mut command = begin(&hello, port);
        match mode {
            "request" => command["request_sha256"] = json!("b".repeat(64)),
            "instance" => command["instance_id"] = json!("wrong"),
            "endpoint" => command["endpoint"] = json!("https://example.com"),
            "extra" => command["input"] = json!("arbitrary source"),
            _ => {}
        }
        driver.send(&command);
        if mode == "dispatch" {
            let ready = driver.next();
            let mut command = dispatch(&ready);
            command["run_nonce"] = json!("b".repeat(64));
            driver.send(&command);
        }
        driver.finish(false);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
#[test]
fn successful_case_uses_actual_prepared_source_and_core_commit() {
    let (port, task) = server(output(SOURCE), "stop");
    let mut driver = Driver::new();
    let hello = driver.next();
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    assert_eq!(ready["fresh_inventory"], true);
    assert_eq!(ready["dispatches"], 0);
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(result["event"], "result");
    assert_eq!(result["dispatches"], 1);
    assert_eq!(result["core_committed"], true);
    assert_eq!(result["quote_grounding_passed"], true);
    assert_eq!(result["source_preserved"], true);
    assert_eq!(result["compiled_unit_count"], 1);
    assert_eq!(result["profile_retained"], false);
    assert_eq!(result["transport"]["usage"]["total_tokens"], 40);
    for key in [
        "instance_id",
        "binary_sha256",
        "plan_id",
        "request_sha256",
        "profile_id",
        "run_nonce",
    ] {
        assert_eq!(ready[key], result[key]);
    }
    let request = task.join().unwrap();
    assert_eq!(request["model"], "llama3.2:latest");
    assert_eq!(request["max_tokens"], 2048);
    assert_eq!(request["stream"], true);
    assert_eq!(request["response_format"]["type"], "json_schema");
    assert_eq!(
        request["messages"][0]["content"],
        nova_review::compiler::STRUCTURED_SYSTEM_PROMPT
    );
    let input: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["source_text"], SOURCE);
}
#[test]
fn failure_preserves_source_and_never_commits_partial_or_unsupported_output() {
    for (content, finish) in [
        (output("not present in source"), "stop"),
        (output(SOURCE), "length"),
    ] {
        let (port, task) = server(content, finish);
        let mut driver = Driver::new();
        let hello = driver.next();
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        task.join().unwrap();
        assert_eq!(result["dispatches"], 1);
        assert_eq!(result["core_committed"], false);
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["compiled_unit_count"], 0);
        assert_eq!(result["transport"]["owned_child_reaped"], true);
        assert!(result["core_error_code"].is_string());
        assert_eq!(result["job"]["error_code"], "invalid_output");
    }
}

#[test]
fn provider_rejection_is_model_unavailable_without_invalid_output_claim() {
    for (status, body, expected_transport) in [
        (
            500,
            "provider grammar rejected",
            "generation_http_status_rejected",
        ),
        (
            200,
            "data: {\"error\":{\"message\":\"fixture provider failure\"}}\n\n",
            "generation_provider_error",
        ),
    ] {
        let (port, task) = http_server(status, body);
        let mut driver = Driver::new();
        let hello = driver.next();
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        task.join().unwrap();
        assert_eq!(result["dispatches"], 1);
        assert_eq!(result["transport"]["error_code"], expected_transport);
        assert_eq!(result["job"]["error_code"], "model_unavailable");
        assert_eq!(result["core_committed"], false);
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["compiled_unit_count"], 0);
        assert_eq!(result["transport"]["owned_child_reaped"], true);
        assert_eq!(result["transport"]["usage"], Value::Null);
    }
}

#[test]
fn unavailable_transport_is_not_invalid_generated_output() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut driver = Driver::new();
    let hello = driver.next();
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(
        result["transport"]["error_code"],
        "generation_transport_failed"
    );
    assert_eq!(result["job"]["error_code"], "model_unavailable");
    assert_eq!(result["core_committed"], false);
    assert_eq!(result["source_preserved"], true);
    assert_eq!(result["compiled_unit_count"], 0);
}

#[test]
fn profile_selection_is_fixed_and_describe_does_not_dispatch() {
    for args in [
        vec!["--profile", "qwen-v02", "--describe"],
        vec!["--describe", "--profile", "qwen-v02"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova-structured-acceptance"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(metadata["profile"], "qwen-v02");
        assert_eq!(metadata["model"], "qwen3:8b-q4_K_M");
        assert_eq!(metadata["reasoning_effort"], "none");
        assert_eq!(
            metadata["request_sha256"],
            "fac64fda25ca1bd8e0a124d235264ca186db53c3cef36795d958c2f68f295221"
        );
        assert_ne!(
            metadata["request_sha256"],
            nova_review::structured_acceptance::metadata().unwrap()["request_sha256"]
        );
    }
    for args in [
        vec!["--profile", "unconfigured"],
        vec!["--profile", "qwen3:8b-q4_K_M", "--describe"],
        vec!["--profile"],
        vec!["--profile", "qwen-v02", "--model", "other"],
        vec!["--profile", "qwen-v02", "--profile", "llama-v02"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova-structured-acceptance"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert!(nova_review::structured_acceptance::metadata_for_profile("arbitrary").is_err());
}

#[test]
fn cross_profile_request_binding_never_connects() {
    for (profile, other) in [("llama-v02", "qwen-v02"), ("qwen-v02", "llama-v02")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut driver = Driver::with_args(&["--profile", profile]);
        let hello = driver.next();
        let mut command = begin(&hello, listener.local_addr().unwrap().port());
        command["request_sha256"] = nova_review::structured_acceptance::metadata_for_profile(other)
            .unwrap()["request_sha256"]
            .clone();
        driver.send(&command);
        driver.finish(false);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn qwen_profile_sends_only_fixed_model_reasoning_and_prompt() {
    let (port, task) = server(output(SOURCE), "stop");
    let mut driver = Driver::with_args(&["--profile", "qwen-v02"]);
    let hello = driver.next();
    assert_eq!(hello["profile"], "qwen-v02");
    assert_eq!(hello["reasoning_effort"], "none");
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(result["dispatches"], 1);
    assert_eq!(result["core_committed"], true);
    assert_eq!(result["source_preserved"], true);
    assert_eq!(result["compiled_unit_count"], 1);
    assert_eq!(
        result["transport"]["request_sha256"],
        hello["request_sha256"]
    );
    let request = task.join().unwrap();
    assert_eq!(request["model"], "qwen3:8b-q4_K_M");
    assert_eq!(request["reasoning_effort"], "none");
    assert_eq!(request["temperature"], 0.7);
    assert_eq!(request["top_p"], 0.8);
    assert_eq!(request["max_tokens"], 2048);
    assert_eq!(request["stream"], true);
    assert_eq!(
        request["messages"][0]["content"],
        nova_review::compiler::STRUCTURED_SYSTEM_PROMPT
    );
    assert_eq!(request["response_format"]["type"], "json_schema");
    let input: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["source_text"], SOURCE);
}

fn quote_output(quote: &str) -> String {
    let mut value: Value = serde_json::from_str(&output(quote)).unwrap();
    value["schema_version"] = json!("nova.extraction.quote-text.v0.1");
    value["units"][0]["quotes"][0]
        .as_object_mut()
        .unwrap()
        .remove("occurrence");
    value.to_string()
}

#[test]
fn quote_profile_binds_source_resolved_bytes_proofs_and_actual_commit() {
    use sha2::{Digest, Sha256};
    let quote = "我在工作场景不参加超过30分钟的会议";
    let raw = quote_output(quote);
    let (port, task) = server(raw.clone(), "stop");
    let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v01"]);
    let hello = driver.next();
    assert_eq!(hello["output_mode"], "structured_quote_text_v01");
    assert_eq!(hello["resolver_version"], "nova.quote-text.resolver.v0.1");
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    assert!(ready["source_snapshot_sha256"].is_string());
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(result["core_committed"], true);
    assert_eq!(result["compiled_unit_count"], 1);
    assert_eq!(result["semantic_acceptance"], "pending_independent_review");
    assert_eq!(
        result["committed_compilation"]["semantic_support"],
        "model_output_unverified"
    );
    let resolution = &result["quote_resolution"];
    let resolved = resolution["resolved_output"].as_str().unwrap();
    assert_eq!(
        resolution["raw_output_sha256"],
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    );
    assert_eq!(
        resolution["resolved_output_sha256"],
        format!("{:x}", Sha256::digest(resolved.as_bytes()))
    );
    assert_eq!(
        resolution["source_snapshot_sha256"],
        ready["source_snapshot_sha256"]
    );
    assert_eq!(
        resolution["source_snapshot_sha256"],
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&resolution["source_snapshot"]).unwrap())
        )
    );
    let parsed: Value = serde_json::from_str(resolved).unwrap();
    assert_eq!(parsed["units"][0]["quotes"][0]["occurrence"], 0);
    let proof = &resolution["proofs"][0];
    let start = proof["start_byte"].as_u64().unwrap() as usize;
    let end = proof["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(&SOURCE[start..end], quote);
    assert_eq!(
        result["units"][0]["evidence"][0]["start_char"],
        proof["start_char"]
    );
    let request = task.join().unwrap();
    assert_eq!(request["model"], "qwen3:8b-q4_K_M");
    assert_eq!(request["reasoning_effort"], "none");
    assert_eq!(
        request["messages"][0]["content"],
        nova_review::quote_text_schema::PROMPT
    );
    assert_eq!(
        request["response_format"],
        nova_review::quote_text_schema::response_format()
    );
}

#[test]
fn quote_v02_sends_the_new_prompt_and_requires_independent_semantic_review() {
    let (port, task) = server(quote_output(SOURCE), "stop");
    let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v02"]);
    let hello = driver.next();
    assert_eq!(
        hello["prompt_version"],
        nova_review::quote_text_schema::PROMPT_V02_VERSION
    );
    assert_eq!(hello["output_mode"], "structured_quote_text_v01");
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(result["dispatches"], 1);
    assert_eq!(result["core_committed"], true);
    assert_eq!(
        result["quote_resolution"]["prompt_sha256"],
        hello["prompt_sha256"]
    );
    assert_eq!(
        result["quote_resolution"]["prompt_version"],
        hello["prompt_version"]
    );
    assert_eq!(result["semantic_acceptance"], "pending_independent_review");
    let request = task.join().unwrap();
    assert_eq!(request["model"], "qwen3:8b-q4_K_M");
    assert_eq!(
        request["messages"][0]["content"],
        nova_review::quote_text_schema::PROMPT_V02
    );
    assert_eq!(
        request["response_format"],
        nova_review::quote_text_schema::response_format()
    );
}

#[test]
fn observed_quote_v02_condition_keys_reject_commit_even_with_exact_grounding() {
    let rule = "我在工作场景不参加超过30分钟的会议";
    let mut raw: Value = serde_json::from_str(&quote_output(rule)).unwrap();
    raw["units"][0]["content"] = json!({
        "subject":"user", "predicate":"participate_in_meetings_with_duration_limitation_of_less_than_30:minutes",
        "value":{"operator":"less_than","amount":30,"unit":"minutes"},
        "polarity":"positive","modality":"asserted",
        "conditions":{"exemption":"private gatherings","scope":"work scenario"}
    });
    for qualified_keys in [false, true] {
        if qualified_keys {
            raw["units"][0]["content"]["conditions"] =
                json!({"scope:exception":"private gatherings","scope:context":"work scenario"});
        }
        let (port, task) = server(raw.to_string(), "stop");
        let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v02"]);
        let hello = driver.next();
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        task.join().unwrap();
        assert_eq!(result["quote_resolution"]["proofs"].as_array().unwrap().len(), 1);
        assert_eq!(result["core_committed"], qualified_keys);
        assert_eq!(result["source_preserved"], true);
        if !qualified_keys {
            assert_eq!(result["core_error_code"], "NOVA_VALIDATION_FAILED");
            assert_eq!(result["compiled_unit_count"], 0);
        }
    }
}

#[test]
fn quote_v03_binds_the_revised_prompt_and_preserves_both_rule_clauses() {
    let rule = "我在工作场景不参加超过30分钟的会议";
    let exception = "私人聚会没有这个限制";
    let mut raw: Value = serde_json::from_str(&quote_output(rule)).unwrap();
    raw["units"][0]["content"] = json!({
        "subject":"user","predicate":"work:attend_meeting",
        "value":{"object":"meeting","operator":"greater_than","amount":30,"unit":"minutes"},
        "polarity":"negative","modality":"asserted",
        "conditions":{"scope:context":"工作场景","scope:exception":"私人聚会不适用这条限制"}
    });
    raw["units"][0]["quotes"] = json!([{"quote":rule},{"quote":exception}]);
    let (port, task) = server(raw.to_string(), "stop");
    let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v03"]);
    let hello = driver.next();
    assert_eq!(hello["prompt_version"], nova_review::quote_text_schema::PROMPT_V03_VERSION);
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);
    assert_eq!(result["core_committed"], true);
    assert_eq!(result["semantic_acceptance"], "pending_independent_review");
    assert_eq!(result["units"][0]["content"], raw["units"][0]["content"]);
    assert_eq!(result["units"][0]["evidence"][0]["quote"], rule);
    assert_eq!(result["units"][0]["evidence"][1]["quote"], exception);
    let request = task.join().unwrap();
    assert_eq!(request["messages"][0]["content"], nova_review::quote_text_schema::PROMPT_V03);
    assert_eq!(request["response_format"], nova_review::quote_text_schema::response_format());
}

#[test]
fn general_profile_binds_its_catalog_source_and_allows_empty_extraction() {
    let case = "injection_abstain";
    let catalog: Value =
        serde_json::from_str(include_str!("../../../eval/model-acceptance/cases.json")).unwrap();
    let source = catalog["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == case)
        .unwrap()["text"]
        .as_str()
        .unwrap();
    let empty = json!({"schema_version":"nova.extraction.quote-text.v0.1","units":[]});
    let (port, task) = server(empty.to_string(), "stop");
    let mut driver = Driver::with_args(&["--profile", case]);
    let hello = driver.next();
    assert_eq!(hello["case_id"], case);
    assert_eq!(hello["reasoning_effort"], "low");
    driver.send(&begin(&hello, port));
    let ready = driver.next();
    driver.send(&dispatch(&ready));
    let result = driver.next();
    driver.finish(true);

    let request = task.join().unwrap();
    let input: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["source_text"], source);
    assert_eq!(
        request["messages"][0]["content"],
        nova_review::quote_text_schema::GENERAL_PROMPT_V10
    );
    assert_eq!(
        request["response_format"],
        nova_review::quote_text_schema::condition_list_response_format()
    );
    assert_eq!(request["reasoning_effort"], "low");
    assert_eq!(result["quote_resolution"]["source_snapshot"]["content"], source);
    assert_eq!(result["quote_resolution"]["proofs"], json!([]));
    assert_eq!(result["core_committed"], true);
    assert_eq!(result["compiled_unit_count"], 0);
    assert_eq!(result["source_preserved"], true);
}

#[test]
fn general_temporal_output_requires_qualified_conditions_to_commit() {
    let case = "temporal_before";
    let quote = "从2026-01-01T00:00:00+08:00起至2026-06-01T00:00:00+08:00之前，我在工作日到甲办公室办公。";
    let mut output: Value = serde_json::from_str(&quote_output(quote)).unwrap();
    let unit = &mut output["units"][0];
    unit["content"] = json!({
        "subject":"user", "predicate":"personal:office_work",
        "value":{"object":"甲办公室"}, "polarity":"positive", "modality":"asserted",
        "conditions":[{"key":"day_of_week","value":"工作日"}]
    });
    unit["valid_time"] = json!({
        "kind":"interval", "start":"2026-01-01T00:00:00+08:00", "end":"2026-06-01T00:00:00+08:00"
    });
    let mut qualified = output.clone();
    qualified["units"][0]["content"]["conditions"][0]["key"] = json!("scope:day_of_week");
    let mut duplicate = qualified.clone();
    duplicate["units"][0]["content"]["conditions"] = json!([
        {"key":"scope:day_of_week","value":"工作日"},
        {"key":"scope:day_of_week","value":"周日"}
    ]);
    let mut expected = qualified["units"][0]["content"].clone();
    expected["conditions"] = json!({"scope:day_of_week":"工作日"});

    for (output, committed) in [(output, false), (duplicate, false), (qualified, true)] {
        let (port, task) = server(output.to_string(), "stop");
        let mut driver = Driver::with_args(&["--profile", case]);
        let hello = driver.next();
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        let request = task.join().unwrap();

        assert_eq!(request["messages"][0]["content"], nova_review::quote_text_schema::GENERAL_PROMPT_V10);
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["core_committed"], committed);
        assert_eq!(result["compiled_unit_count"], usize::from(committed));
        if committed {
            assert_eq!(result["quote_resolution"]["proofs"].as_array().unwrap().len(), 1);
            assert_eq!(result["units"][0]["content"], expected);
            assert_eq!(result["units"][0]["valid_time"], output["units"][0]["valid_time"]);
            assert_eq!(result["units"][0]["evidence"][0]["quote"], quote);
        } else {
            assert_eq!(result["core_error_code"], "NOVA_VALIDATION_FAILED");
            assert_eq!(result["quote_resolution_error"], "quote_text_invalid_shape");
            assert_eq!(result["committed_compilation"], Value::Null);
        }
    }
}

#[test]
fn quote_v04_requires_a_linked_exception_before_commit() {
    let rule = "我在工作场景不参加超过30分钟的会议";
    let exception = "私人聚会没有这个限制";
    let mut observed: Value = serde_json::from_str(&quote_output(rule)).unwrap();
    observed["units"][0]["content"] = json!({
        "subject":"user","predicate":"safety:participate_meeting",
        "value":{"object":"meeting","operator":"greater_than","amount":30,"unit":"minutes"},
        "polarity":"negative","modality":"asserted","conditions":{"scope:context":"work scenario"}
    });
    let mut other = observed["units"][0].clone();
    other["content"]["conditions"]["scope:context"] = json!("private gathering");
    other["content"]["polarity"] = json!("positive");
    other["content"]["value"]["operator"] = json!("no_limit");
    other["content"]["value"]["amount"] = json!(0);
    other["quotes"] = json!([{"quote":exception}]);
    observed["units"].as_array_mut().unwrap().push(other);

    let mut linked = observed.clone();
    linked["units"].as_array_mut().unwrap().truncate(1);
    linked["units"][0]["content"]["conditions"]["scope:exception"] =
        json!({"context":"私人聚会","effect":"rule_not_applicable"});
    linked["units"][0]["quotes"] = json!([{"quote":rule},{"quote":exception}]);
    let mut unsupported_effect = linked.clone();
    unsupported_effect["units"][0]["content"]["conditions"]["scope:exception"]["effect"] =
        json!("no_limit");

    for (raw, should_commit) in [(observed, false), (unsupported_effect, false), (linked, true)] {
        let (port, task) = server(raw.to_string(), "stop");
        let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v04"]);
        let hello = driver.next();
        assert_eq!(hello["prompt_version"], nova_review::quote_text_schema::PROMPT_V04_VERSION);
        assert_eq!(hello["provider_schema_version"], nova_review::quote_text_schema::LINKED_PROVIDER_VERSION);
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        let request = task.join().unwrap();
        assert_eq!(request["messages"][0]["content"], nova_review::quote_text_schema::PROMPT_V04);
        assert_eq!(request["response_format"], nova_review::quote_text_schema::linked_response_format());
        assert_eq!(result["quote_resolution"]["provider_schema_sha256"], hello["provider_schema_sha256"]);
        assert_eq!(result["semantic_acceptance"], "pending_independent_review");
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["core_committed"], should_commit);
        if should_commit {
            assert_eq!(result["compiled_unit_count"], 1);
            assert_eq!(result["units"][0]["content"], raw["units"][0]["content"]);
            assert_eq!(result["units"][0]["evidence"][0]["quote"], rule);
            assert_eq!(result["units"][0]["evidence"][1]["quote"], exception);
        } else {
            assert_eq!(result["linked_rule_shape_error"], "invalid_linked_rule_shape");
            assert_eq!(result["compiled_unit_count"], 0);
        }
    }
}

#[test]
fn quote_v05_rejects_the_observed_reversed_comparison_before_commit() {
    let observed = json!({
        "schema_version":"nova.extraction.quote-text.v0.1",
        "units":[{
            "content":{
                "conditions":{
                    "scope:context":"工作场景",
                    "scope:exception":{"context":"私人聚会","effect":"rule_not_applicable"}
                },
                "modality":"asserted",
                "polarity":"negative",
                "predicate":"participate_in_meetings_duration_limitation_minutes:less_than_or_equal_to",
                "subject":"user",
                "value":{"object":"会议","operator":"less_than_or_equal_to","amount":30,"unit":"minutes"}
            },
            "origin":"extracted",
            "quotes":[{"quote":"我在工作场景不参加超过30分钟的会议"},{"quote":"私人聚会没有这个限制"}],
            "valid_time":{"kind":"unknown"}
        }]
    });
    let mut correct = observed.clone();
    correct["units"][0]["content"]["predicate"] = json!("work:attend_meeting");
    correct["units"][0]["content"]["value"]["operator"] = json!("greater_than");
    let mut inclusive = correct.clone();
    inclusive["units"][0]["content"]["value"]["operator"] = json!("greater_than_or_equal_to");
    let mut positive = correct.clone();
    positive["units"][0]["content"]["polarity"] = json!("positive");

    for (raw, should_commit) in [(observed, false), (inclusive, false), (positive, false), (correct, true)] {
        let (port, task) = server(raw.to_string(), "stop");
        let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v05"]);
        let hello = driver.next();
        assert_eq!(hello["prompt_version"], nova_review::quote_text_schema::PROMPT_V05_VERSION);
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        let request = task.join().unwrap();
        assert_eq!(request["messages"][0]["content"], nova_review::quote_text_schema::PROMPT_V05);
        assert_eq!(request["response_format"], nova_review::quote_text_schema::linked_response_format());
        assert_eq!(result["quote_resolution"]["proofs"].as_array().unwrap().len(), 2);
        assert_eq!(result["semantic_acceptance"], "pending_independent_review");
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["core_committed"], should_commit);
        if should_commit {
            assert_eq!(result["compiled_unit_count"], 1);
            assert_eq!(result["units"][0]["content"], raw["units"][0]["content"]);
        } else {
            assert_eq!(result["fixed_comparison_error"], "fixed_case_comparison_mismatch");
            assert_eq!(result["compiled_unit_count"], 0);
        }
    }
}

#[test]
fn quote_profile_rejects_positions_ambiguity_old_schema_and_partial_before_commit() {
    let mut positions: Value = serde_json::from_str(&quote_output(SOURCE)).unwrap();
    positions["units"][0]["quotes"][0]["occurrence"] = json!(12);
    for (raw, finish) in [
        (positions.to_string(), "stop"),
        (quote_output("会"), "stop"),
        (quote_output("不在材料中的句子"), "stop"),
        (output(SOURCE), "stop"),
        (quote_output(SOURCE), "length"),
        (
            "{\"schema_version\":\"nova.extraction.quote-text.v0.1\",\"units\":[".into(),
            "stop",
        ),
    ] {
        let (port, task) = server(raw, finish);
        let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v01"]);
        let hello = driver.next();
        driver.send(&begin(&hello, port));
        let ready = driver.next();
        driver.send(&dispatch(&ready));
        let result = driver.next();
        driver.finish(true);
        task.join().unwrap();
        assert_eq!(result["core_committed"], false);
        assert_eq!(result["compiled_unit_count"], 0);
        assert_eq!(result["quote_resolution"], Value::Null);
        assert_eq!(result["source_preserved"], true);
        assert_eq!(result["transport"]["owned_child_reaped"], true);
    }
}

#[test]
fn quote_profile_requires_source_snapshot_permit_without_connecting() {
    for supplied in [None, Some("0".repeat(64))] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut driver = Driver::with_args(&["--profile", "qwen-quote-text-v01"]);
        let hello = driver.next();
        driver.send(&begin(&hello, listener.local_addr().unwrap().port()));
        let ready = driver.next();
        let mut permit = dispatch(&ready);
        if let Some(hash) = supplied {
            permit["source_snapshot_sha256"] = json!(hash);
        } else {
            permit
                .as_object_mut()
                .unwrap()
                .remove("source_snapshot_sha256");
        }
        driver.send(&permit);
        driver.finish(false);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn quote_resolution_cannot_bypass_source_withdrawal_or_configuration_cas() {
    use nova_core::{CaptureFidelity, PrivacyLevel, VaultKey, compilation::*, intake::*};
    for changed in ["source", "configuration"] {
        let temp = tempfile::tempdir().unwrap();
        let scope = AssetScope {
            owner_id: "owner:synthetic".into(),
            space: "fixture".into(),
        };
        let mut store = IntakeStore::create(
            &temp.path().join("intake.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        store.enable_compilation(&scope).unwrap();
        let selection = ModelSelection::Local {
            model_id: "synthetic-fixture".into(),
            endpoint: "http://127.0.0.1:1/v1".into(),
        };
        store.configure_model(1, selection.clone()).unwrap();
        let receipt = store
            .receive(
                &IntakeAuthority {
                    scope: scope.clone(),
                    actor_id: "synthetic".into(),
                    capture_allowed: true,
                    role: SourceRole::User,
                    privacy_level: PrivacyLevel::Private,
                },
                &serde_json::to_vec(&IntakeRequest {
                    schema_version: INTAKE_VERSION.into(),
                    request_id: "fixture".into(),
                    source_platform: "synthetic".into(),
                    capture_fidelity: CaptureFidelity::F2VisibleTranscript,
                    content: SOURCE.into(),
                    occurred_at: None,
                })
                .unwrap(),
            )
            .unwrap();
        let lease = store
            .claim_next(&scope, 60)
            .unwrap()
            .unwrap()
            .lease
            .unwrap()
            .lease_id;
        let prepared = store
            .prepare_compilation(&scope, &receipt.job_id, &lease)
            .unwrap();
        store
            .authorize_dispatch(&scope, &receipt.job_id, &lease, 1)
            .unwrap();
        let resolved = nova_review::quote_text::resolve(
            &prepared.context.sources[0].content,
            quote_output(SOURCE).as_bytes(),
        )
        .unwrap();
        if changed == "source" {
            store
                .withdraw_source(&scope, &receipt.source.object_id)
                .unwrap();
        } else {
            store.configure_model(2, selection).unwrap();
        }
        assert!(
            store
                .commit_compilation(&scope, &receipt.job_id, &lease, resolved.resolved_bytes())
                .is_err()
        );
        assert!(store.compiled_units(&scope, None).unwrap().is_empty());
    }
}
