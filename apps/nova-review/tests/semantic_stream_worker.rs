//! Actual system curl and loopback SSE fixtures through the worker and trusted core commit.
//! Fixtures never contact a model service or read an existing user's Vault or credentials.
use nova_core::{
    CaptureFidelity, PrivacyLevel, Vault, VaultKey,
    compilation::{AssetScope, IntakeReceipt, JobState, SourceRole},
    intake::{IntakeAuthority, IntakeStore, ModelSelection},
};
use nova_review::{
    ReviewServer,
    compiler::{
        ModelRuntime, STRUCTURED_PROMPT_VERSION, STRUCTURED_SYSTEM_PROMPT, SYSTEM_PROMPT, run_once,
        structured_probe, structured_probe_qwen_v02, structured_probe_request_sha256,
        structured_probe_request_sha256_qwen_v02, structured_probe_request_sha256_v02,
        structured_probe_v02,
    },
    compiler_schema,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

const SOURCE: &str = "合成测试：在工作时我不参加超过30分钟的会议。";
const MODEL: &str = "synthetic-sse-fixture-not-a-real-model";
const PRIVATE_PROVIDER_TEXT: &str = "never-copy-private-provider-body";

#[derive(Clone, Copy, Debug)]
enum Mode {
    Good,
    NoUsage,
    DefaultJson,
    Length,
    MissingDone,
    Tools,
    InvalidUsage,
    RepeatedUnits,
    TooManyUnits,
    DuplicateKeys,
    SchemaRejected,
    EarlyEof,
    ProviderError,
}
struct ConnectionReport {
    request: Value,
    peer_disconnected_while_fixture_held_open: bool,
}
struct Fixture {
    endpoint: String,
    task: thread::JoinHandle<ConnectionReport>,
}
impl Fixture {
    fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, address)) => {
                        assert!(address.ip().is_loopback());
                        break stream;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "worker did not contact fixture");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            // macOS may inherit the listener's nonblocking flag on accepted sockets. The
            // independent read timeout below must wait for a real peer close, not WouldBlock.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut stream);
            let (content_type, wire) = response(mode);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(headers.as_bytes()).unwrap();
            // Fragment the HTTP body across byte boundaries, independently from SSE event/token
            // boundaries; the transport must tolerate arbitrary reads and split Unicode scalars.
            for bytes in wire.chunks(97) {
                if let Err(error) = stream.write_all(bytes) {
                    assert!(
                        matches!(mode, Mode::RepeatedUnits | Mode::TooManyUnits),
                        "{error}"
                    );
                    return ConnectionReport {
                        request,
                        peer_disconnected_while_fixture_held_open: true,
                    };
                }
            }
            stream.flush().unwrap();
            if matches!(mode, Mode::RepeatedUnits | Mode::TooManyUnits) {
                // There is deliberately no Content-Length, DONE, or shutdown here. Waiting for
                // the peer proves the worker's guard kills curl instead of waiting for EOF/45s.
                let mut byte = [0];
                let disconnected = match stream.read(&mut byte) {
                    Ok(0) => true,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        true
                    }
                    Ok(_) | Err(_) => false,
                };
                ConnectionReport {
                    request,
                    peer_disconnected_while_fixture_held_open: disconnected,
                }
            } else {
                stream.shutdown(Shutdown::Write).unwrap();
                ConnectionReport {
                    request,
                    peer_disconnected_while_fixture_held_open: false,
                }
            }
        });
        Self { endpoint, task }
    }
    fn finish(self) -> ConnectionReport {
        self.task.join().unwrap()
    }
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let (offset, length) = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        assert!(bytes.len() <= 128 * 1024);
        if let Some(offset) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..offset]).unwrap();
            assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(length <= 128 * 1024);
            break (offset + 4, length);
        }
    };
    while bytes.len() < offset + length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        assert!(bytes.len() <= 128 * 1024);
    }
    serde_json::from_slice(&bytes[offset..offset + length]).unwrap()
}

fn unit(occurrence: usize) -> Value {
    json!({"origin":"extracted",
        "content":{"subject":"user","predicate":"work:meeting_preference","value":"30 minutes",
            "polarity":"negative","modality":"asserted","conditions":{"scope:context":"work"}},
        "occurred_at":null,"valid_time":{"kind":"unknown"},
        "quotes":[{"quote":SOURCE,"occurrence":occurrence}]})
}
fn extraction() -> Value {
    json!({"schema_version":"nova.extraction.v0.1","units":[unit(0)]})
}
fn event(value: Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}
fn content(text: &str) -> Vec<u8> {
    event(
        json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}],
        "provider_private":PRIVATE_PROVIDER_TEXT}),
    )
}
fn end(reason: &str, usage: bool, done: bool) -> Vec<u8> {
    let mut wire = event(json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}));
    if usage {
        wire.extend(event(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})));
    }
    if done {
        wire.extend_from_slice(b"data: [DONE]\n\n");
    }
    wire
}
fn response(mode: Mode) -> (&'static str, Vec<u8>) {
    if matches!(mode, Mode::DefaultJson) {
        return (
            "application/json",
            serde_json::to_vec(&json!({"choices":[{
                "finish_reason":"stop","message":{"content":extraction().to_string()}
            }]}))
            .unwrap(),
        );
    }
    let mut wire = b": synthetic loopback fixture\n\n".to_vec();
    wire.extend(event(json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]})));
    match mode {
        Mode::Good | Mode::NoUsage | Mode::Length | Mode::MissingDone => {
            let text = extraction().to_string();
            let characters: Vec<char> = text.chars().collect();
            for part in characters.chunks(13) {
                wire.extend(content(&part.iter().collect::<String>()));
            }
            wire.extend(end(
                if matches!(mode, Mode::Length) {
                    "length"
                } else {
                    "stop"
                },
                !matches!(mode, Mode::NoUsage),
                !matches!(mode, Mode::MissingDone),
            ));
        }
        Mode::Tools => {
            wire.extend(event(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"name":PRIVATE_PROVIDER_TEXT}}]},"finish_reason":null}]})));
        }
        Mode::InvalidUsage => {
            wire.extend(content(&extraction().to_string()));
            wire.extend(end("stop", false, false));
            wire.extend(event(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":999}})));
            wire.extend_from_slice(b"data: [DONE]\n\n");
        }
        Mode::RepeatedUnits | Mode::TooManyUnits => {
            let units = if matches!(mode, Mode::RepeatedUnits) {
                vec![unit(0); 3]
            } else {
                (0..17).map(unit).collect()
            };
            let text = format!(
                "{{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[{}",
                units
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            wire.extend(content(&text));
        }
        Mode::DuplicateKeys => {
            wire.extend(content(
                "{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[],\"units\":[]}",
            ));
            wire.extend(end("stop", true, true));
        }
        Mode::SchemaRejected => {
            let mut value = extraction();
            value["units"][0]["content"]["untrusted_extra"] = json!(PRIVATE_PROVIDER_TEXT);
            wire.extend(content(&value.to_string()));
            wire.extend(end("stop", true, true));
        }
        Mode::EarlyEof => {
            wire.extend(content(
                "{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[",
            ));
        }
        Mode::ProviderError => {
            wire.extend(event(json!({"error":{"message":PRIVATE_PROVIDER_TEXT}})))
        }
        Mode::DefaultJson => unreachable!(),
    }
    ("text/event-stream", wire)
}

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic-stream-test".into(),
        space: "private".into(),
    }
}
fn open_store(directory: &tempfile::TempDir) -> IntakeStore {
    IntakeStore::open(
        &directory.path().join("intake.sqlite"),
        VaultKey::read_file(&directory.path().join("vault.key")).unwrap(),
        scope(),
    )
    .unwrap()
}
fn setup(
    mode: Mode,
    structured: bool,
) -> (tempfile::TempDir, ReviewServer, IntakeReceipt, Fixture) {
    let fixture = Fixture::start(mode);
    let directory = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&directory.path().join("vault.key"))
        .unwrap();
    drop(Vault::create(&directory.path().join("legacy.sqlite"), key.clone()).unwrap());
    let mut store =
        IntakeStore::create(&directory.path().join("intake.sqlite"), key, scope()).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: MODEL.into(),
                endpoint: fixture.endpoint.clone(),
            },
        )
        .unwrap();
    drop(store);
    let runtime = if structured {
        ModelRuntime::structured_local(1)
    } else {
        ModelRuntime::local(1)
    };
    let server = ReviewServer::new(
        directory.path().join("legacy.sqlite"),
        directory.path().join("vault.key"),
        "127.0.0.1:31412".into(),
        true,
    )
    .with_owner(scope().owner_id, scope().space)
    .with_intake_store(&directory.path().join("intake.sqlite"))
    .unwrap()
    .with_model_runtime(runtime)
    .unwrap();
    let request = json!({"schema_version":"nova.intake.v0.1","request_id":"synthetic-stream-input",
        "source_platform":"synthetic-loopback-test","capture_fidelity":"f2_visible_transcript",
        "content":SOURCE,"occurred_at":null});
    let authority = IntakeAuthority {
        scope: scope(),
        actor_id: "owner:synthetic-stream-test".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    };
    let receipt = open_store(&directory)
        .receive(&authority, &serde_json::to_vec(&request).unwrap())
        .unwrap();
    (directory, server, receipt, fixture)
}
fn assert_request(body: &Value, structured: bool) {
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["stream"], structured);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["messages"][0],
        json!({"role":"system","content":SYSTEM_PROMPT})
    );
    assert_eq!(body["messages"][1]["role"], "user");
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        input,
        json!({"schema_version":"nova.extraction.v0.1","source_text":SOURCE,
        "source_role":"user","occurred_at":null,"correction_target":null})
    );
    if structured {
        assert_eq!(body["stream_options"], json!({"include_usage":true}));
        assert_eq!(
            body["response_format"],
            compiler_schema::extraction_response_format()
        );
    } else {
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["response_format"], json!({"type":"json_object"}));
    }
}

#[test]
fn structured_prompt_is_versioned_separately_without_changing_legacy_prompt_bytes() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        STRUCTURED_PROMPT_VERSION,
        "nova.extraction.structured-prompt.v0.2"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(SYSTEM_PROMPT.as_bytes())),
        "81a59689adc2267d6e0fa55b3c125be6f9942a07fa97bb88ffc2e37a84b612ff"
    );
    assert_ne!(STRUCTURED_SYSTEM_PROMPT, SYSTEM_PROMPT);
    assert!(!STRUCTURED_SYSTEM_PROMPT.contains(r"\n"));
    assert!(STRUCTURED_SYSTEM_PROMPT.lines().count() > 1);
}

#[test]
fn experimental_probes_require_explicit_api_and_preserve_fixed_profile_request_binding() {
    use sha2::{Digest, Sha256};
    let input = json!({"schema_version":"nova.extraction.v0.1","source_text":SOURCE,
        "source_role":"user","occurred_at":null,"correction_target":null})
    .to_string();
    let mut original_request = None;
    for profile in ["original", "llama-v02", "qwen-v02"] {
        let fixture = Fixture::start(Mode::Good);
        let model = if profile == "qwen-v02" {
            "qwen3:8b-q4_K_M"
        } else {
            "llama3.2:latest"
        };
        let (expected_hash, report) = match profile {
            "qwen-v02" => (
                structured_probe_request_sha256_qwen_v02(model, &fixture.endpoint, &input, 5)
                    .unwrap(),
                structured_probe_qwen_v02(model, &fixture.endpoint, &input, 5),
            ),
            "llama-v02" => (
                structured_probe_request_sha256_v02(model, &fixture.endpoint, &input, 5).unwrap(),
                structured_probe_v02(model, &fixture.endpoint, &input, 5),
            ),
            "original" => (
                structured_probe_request_sha256(model, &fixture.endpoint, &input, 5).unwrap(),
                structured_probe(model, &fixture.endpoint, &input, 5),
            ),
            _ => unreachable!(),
        };
        assert!(report.error_code.is_none(), "{:?}", report.error_code);
        assert_eq!(report.request_sha256, expected_hash);
        assert!(report.dispatch_started);
        assert!(report.owned_child_reaped);
        let mut request = fixture.finish().request;
        assert_eq!(request["model"], model);
        assert_eq!(request["max_tokens"], 2048);
        assert_eq!(request["stream"], true);
        assert_eq!(request["stream_options"]["include_usage"], true);
        assert_eq!(
            request["response_format"],
            compiler_schema::extraction_response_format()
        );
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&request).unwrap())
            ),
            expected_hash
        );
        assert_eq!(
            request["messages"][0]["content"],
            if profile != "original" {
                STRUCTURED_SYSTEM_PROMPT
            } else {
                SYSTEM_PROMPT
            }
        );
        if profile == "qwen-v02" {
            assert_eq!(request["reasoning_effort"], "none");
            assert_eq!(request["temperature"], 0.7);
            assert_eq!(request["top_p"], 0.8);
            request.as_object_mut().unwrap().remove("reasoning_effort");
            request.as_object_mut().unwrap().remove("top_p");
            request["temperature"] = json!(0);
            request["model"] = json!("llama3.2:latest");
        } else {
            assert!(request.get("reasoning_effort").is_none());
            assert!(request.get("top_p").is_none());
            assert_eq!(request["temperature"], 0);
        }
        if profile != "original" {
            // Explicit experiments differ only in their fixed prompt/model/reasoning/sampling
            // selections. All remaining transport/schema parameters are unchanged.
            request["messages"][0]["content"] = json!(SYSTEM_PROMPT);
            assert_eq!(Some(request), original_request);
        } else {
            original_request = Some(request);
        }
    }
}

#[test]
fn experimental_probe_retains_original_pre_dispatch_scope_guards() {
    let valid_input = r#"{"schema_version":"nova.extraction.v0.1","source_text":"synthetic","source_role":"user","occurred_at":null,"correction_target":null}"#;
    for (model, endpoint, input, seconds) in [
        ("other-model", "http://127.0.0.1:11434/v1", valid_input, 5),
        ("llama3.2:latest", "https://example.com/v1", valid_input, 5),
        (
            "llama3.2:latest",
            "http://token@127.0.0.1:11434/v1",
            valid_input,
            5,
        ),
        (
            "llama3.2:latest",
            "http://127.0.0.1:11434/v1?token=x",
            valid_input,
            5,
        ),
        ("llama3.2:latest", "http://127.0.0.1:11434/v1", "[]", 5),
        (
            "llama3.2:latest",
            "http://127.0.0.1:11434/v1",
            valid_input,
            121,
        ),
    ] {
        let original = structured_probe(model, endpoint, input, seconds);
        let experimental = structured_probe_v02(model, endpoint, input, seconds);
        let qwen_model = if model == "other-model" {
            model
        } else {
            "qwen3:8b-q4_K_M"
        };
        let qwen_experimental = structured_probe_qwen_v02(qwen_model, endpoint, input, seconds);
        assert!(!original.dispatch_started);
        assert!(!experimental.dispatch_started);
        assert!(original.error_code.is_some());
        assert_eq!(experimental.error_code, original.error_code);
        assert!(!qwen_experimental.dispatch_started);
        assert_eq!(qwen_experimental.error_code, original.error_code);
        assert!(structured_probe_request_sha256_v02(model, endpoint, input, seconds).is_err());
        assert!(
            structured_probe_request_sha256_qwen_v02(qwen_model, endpoint, input, seconds).is_err()
        );
    }
    assert!(
        structured_probe_request_sha256(
            "qwen3:8b-q4_K_M",
            "http://127.0.0.1:11434/v1",
            valid_input,
            5
        )
        .is_err()
    );
    assert!(
        structured_probe_request_sha256_v02(
            "qwen3:8b-q4_K_M",
            "http://127.0.0.1:11434/v1",
            valid_input,
            5
        )
        .is_err()
    );
    assert!(
        structured_probe_request_sha256_qwen_v02(
            "llama3.2:latest",
            "http://127.0.0.1:11434/v1",
            valid_input,
            5
        )
        .is_err()
    );
}

fn assert_saved_source(directory: &tempfile::TempDir, receipt: &IntakeReceipt) {
    let source = open_store(directory)
        .source(&scope(), &receipt.source.object_id)
        .unwrap();
    assert_eq!(source.content, SOURCE);
    assert_eq!(
        source.capture_fidelity,
        CaptureFidelity::F2VisibleTranscript
    );
}

#[tokio::test]
async fn structured_local_curl_sse_worker_commits_supported_synthetic_unit_and_preserves_request() {
    let (directory, server, receipt, fixture) = setup(Mode::Good, true);
    assert_eq!(
        run_once(&server).await.unwrap(),
        Some(receipt.job_id.clone())
    );
    assert_request(&fixture.finish().request, true);
    assert_saved_source(&directory, &receipt);
    let store = open_store(&directory);
    let job = store.job(&scope(), &receipt.job_id).unwrap();
    assert_eq!(job.job.state, JobState::Ready);
    assert!(job.job.committed_change_id.is_some());
    let usage = store.compilation_usage(&scope(), None).unwrap();
    let observed = &usage.items[0].observed.as_ref().unwrap().observation;
    assert_eq!(observed.usage.as_ref().unwrap().total_tokens, 15);
    assert_eq!(observed.usage.as_ref().unwrap().cached_input_tokens, None);
    let units = store.compiled_units(&scope(), None).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].1.unit.content.value, "30 minutes");
    assert_eq!(units[0].1.unit.content.conditions["scope:context"], "work");
    assert_eq!(
        units[0].1.unit.origin,
        nova_core::compilation::SemanticOrigin::Extracted
    );
}

#[tokio::test]
async fn missing_usage_remains_unknown_and_does_not_block_ordinary_worker_commit() {
    let (directory, server, receipt, fixture) = setup(Mode::NoUsage, true);
    run_once(&server).await.unwrap();
    assert_request(&fixture.finish().request, true);
    assert_saved_source(&directory, &receipt);
    let usage = open_store(&directory).compilation_usage(&scope(), None).unwrap();
    assert!(usage.items[0].observed.as_ref().unwrap().observation.usage.is_none());
    assert_eq!(
        open_store(&directory)
            .compiled_units(&scope(), None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn original_json_object_worker_path_remains_the_default() {
    let (directory, server, receipt, fixture) = setup(Mode::DefaultJson, false);
    run_once(&server).await.unwrap();
    assert_request(&fixture.finish().request, false);
    assert_saved_source(&directory, &receipt);
    assert_eq!(
        open_store(&directory)
            .compiled_units(&scope(), None)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn incomplete_invalid_or_tool_streams_keep_originals_and_never_commit_partial_results() {
    for (mode, code) in [
        (Mode::Length, "generation_output_length"),
        (Mode::MissingDone, "generation_missing_done"),
        (Mode::Tools, "generation_tool_output"),
        (Mode::InvalidUsage, "generation_invalid_usage"),
        (Mode::DuplicateKeys, "generation_duplicate_json_key"),
        (Mode::SchemaRejected, "generation_invalid_extraction"),
        (Mode::EarlyEof, "generation_missing_finish_reason"),
        (Mode::ProviderError, "generation_provider_error"),
    ] {
        let (directory, server, receipt, fixture) = setup(mode, true);
        let error = run_once(&server).await.unwrap_err().to_string();
        assert!(error.contains(code), "{mode:?}: {error}");
        assert!(!error.contains(SOURCE));
        assert!(!error.contains(PRIVATE_PROVIDER_TEXT));
        assert!(!error.contains(&fixture.endpoint));
        assert_request(&fixture.finish().request, true);
        assert_saved_source(&directory, &receipt);
        let store = open_store(&directory);
        let job = store.job(&scope(), &receipt.job_id).unwrap();
        assert_eq!(job.job.state, JobState::Failed, "{mode:?}");
        assert!(job.job.committed_change_id.is_none(), "{mode:?}");
        let usage=store.compilation_usage(&scope(),None).unwrap();
        let observed=&usage.items[0].observed.as_ref().unwrap().observation;
        if matches!(mode, Mode::MissingDone | Mode::SchemaRejected) {
            assert_eq!(observed.usage.as_ref().unwrap().total_tokens,15);
        } else if matches!(mode, Mode::InvalidUsage | Mode::Tools | Mode::EarlyEof) {
            assert!(observed.usage.is_none());
        }
        assert!(
            store.compiled_units(&scope(), None).unwrap().is_empty(),
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn repetition_and_unit_limit_abort_curl_while_fixture_keeps_connection_open() {
    for (mode, code) in [
        (Mode::RepeatedUnits, "generation_repeated_units"),
        (Mode::TooManyUnits, "generation_units_limit"),
    ] {
        let (directory, server, receipt, fixture) = setup(mode, true);
        let started = Instant::now();
        let error = run_once(&server).await.unwrap_err().to_string();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "{mode:?}: guard took {elapsed:?}"
        );
        assert!(error.contains(code), "{mode:?}: {error}");
        assert!(!error.contains(SOURCE));
        assert!(!error.contains(PRIVATE_PROVIDER_TEXT));
        let report = fixture.finish();
        assert!(
            report.peer_disconnected_while_fixture_held_open,
            "{mode:?}: client did not disconnect before fixture's independent 5s read timeout"
        );
        assert_request(&report.request, true);
        assert_saved_source(&directory, &receipt);
        let store = open_store(&directory);
        assert_eq!(
            store.job(&scope(), &receipt.job_id).unwrap().job.state,
            JobState::Failed
        );
        assert!(store.compiled_units(&scope(), None).unwrap().is_empty());
    }
}
