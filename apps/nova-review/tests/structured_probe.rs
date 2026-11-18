//! Dedicated diagnostic API against owned synthetic loopback SSE fixtures only.
use nova_review::compiler::{
    StructuredProbeReport, structured_probe, structured_probe_request_sha256,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};
const INPUT: &str = r#"{"schema_version":"nova.extraction.v0.1","source_text":"合成诊断材料","source_role":"user","occurred_at":null,"correction_target":null}"#;
fn event(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn content(value: &str) -> String {
    event(json!({"choices":[{"index":0,"delta":{"content":value}}]}))
}
fn end(usage: bool) -> String {
    let mut value = event(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
    if usage {
        value += &event(
            json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}}),
        );
    }
    value + "data: [DONE]\n\n"
}
fn run(wire: String, held_open: bool, timeout: u64) -> StructuredProbeReport {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < until);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 8192);
        }
        let headers = String::from_utf8(headers).unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(!headers.to_ascii_lowercase().contains("authorization"));
        let length = headers
            .lines()
            .find_map(|line| {
                let (key, val) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| val.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        assert!(length < 128 * 1024);
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "llama3.2:latest");
        assert_eq!(value["stream"], true);
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert_eq!(value["max_tokens"], 2048);
        assert_eq!(
            value["response_format"],
            nova_review::compiler_schema::extraction_response_format()
        );
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        for bytes in wire.as_bytes().chunks(97) {
            socket.write_all(bytes).unwrap();
        }
        socket.flush().unwrap();
        if held_open {
            let mut byte = [0];
            assert!(
                matches!(socket.read(&mut byte), Ok(0)) || socket.take_error().unwrap().is_some(),
                "owned curl failed to disconnect"
            );
        }
        format!("{:x}", Sha256::digest(&body))
    });
    let prepared_hash =
        structured_probe_request_sha256("llama3.2:latest", &endpoint, INPUT, timeout).unwrap();
    let report = structured_probe("llama3.2:latest", &endpoint, INPUT, timeout);
    assert_eq!(report.request_sha256, prepared_hash);
    assert_eq!(report.request_sha256, server.join().unwrap());
    assert!(report.dispatch_started);
    assert!(report.owned_child_reaped);
    report
}
#[test]
fn success_and_missing_usage_remain_distinct() {
    let text = r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#;
    for usage in [true, false] {
        let report = run(content(text) + &end(usage), false, 3);
        assert!(report.error_code.is_none());
        assert_eq!(report.content.as_deref(), Some(text));
        assert!(report.diagnostic_content.is_none());
        assert_eq!(report.usage.is_some(), usage);
        if let Some(usage) = report.usage {
            assert_eq!(usage.total_tokens, 18);
        }
        assert_eq!(report.http_status, Some(200));
        assert_eq!(report.curl_exit_code, Some(0));
        assert_eq!(report.decoded_bytes, text.len());
        assert!(report.first_content_ms.is_some());
        assert!(report.sse_done);
    }
}
#[test]
fn length_retains_same_frame_usage_and_only_diagnostic_content() {
    let frame = event(
        json!({"choices":[{"index":0,"delta":{"content":"{\"schema_version\":"},"finish_reason":"length"}],
        "usage":{"prompt_tokens":611,"completion_tokens":2048,"total_tokens":2659},
        "provider_private":"do-not-copy-provider-metadata"}),
    );
    let report = run(frame, true, 3);
    assert_eq!(
        report.error_code.as_deref(),
        Some("generation_output_length")
    );
    assert_eq!(report.finish_reason.as_deref(), Some("length"));
    assert!(report.content.is_none());
    assert_eq!(
        report.diagnostic_content.as_deref(),
        Some("{\"schema_version\":")
    );
    assert_eq!(report.usage.as_ref().unwrap().total_tokens, 2659);
    assert!(!report.sse_done);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("do-not-copy")
    );
}
#[test]
fn guard_abort_disconnects_open_stream_with_unknown_usage() {
    let unit = json!({"origin":"extracted","content":{"subject":"user","predicate":"scope:preference","value":"合成值","polarity":"negative","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"合成诊断材料","occurrence":0}]});
    let text =
        json!({"schema_version":"nova.extraction.v0.1","units":[unit.clone(),unit.clone(),unit]})
            .to_string();
    let report = run(content(&text), true, 3);
    assert_eq!(
        report.error_code.as_deref(),
        Some("generation_repeated_units")
    );
    assert!(report.elapsed_ms < 2500);
    assert!(report.content.is_none());
    assert!(report.usage.is_none());
    assert_eq!(report.completed_units, 3);
    assert_eq!(report.diagnostic_content.as_deref(), Some(text.as_str()));
}
#[test]
fn failed_schema_has_bounded_utf8_diagnostic_and_actual_usage() {
    let text =
        json!({"schema_version":"nova.extraction.v0.1","units":[],"unknown":"合成".repeat(15000)})
            .to_string();
    let report = run(content(&text) + &end(true), false, 3);
    assert_eq!(
        report.error_code.as_deref(),
        Some("generation_invalid_extraction")
    );
    assert!(report.content.is_none());
    assert!(report.diagnostic_truncated);
    let diagnostic = report.diagnostic_content.unwrap();
    assert!(diagnostic.len() <= 64 * 1024);
    assert!(text.starts_with(&diagnostic));
    assert_eq!(report.decoded_bytes, text.len());
    assert_eq!(report.usage.unwrap().total_tokens, 18);
}
#[test]
fn deadline_reports_reap_without_inventing_usage_or_response() {
    let report = run(String::new(), true, 1);
    assert_eq!(
        report.error_code.as_deref(),
        Some("generation_client_deadline")
    );
    assert!(report.usage.is_none());
    assert!(report.content.is_none());
    assert!(report.diagnostic_content.is_none());
    assert!(report.elapsed_ms < 3000);
}
#[test]
fn invalid_scope_is_rejected_before_dispatch() {
    for (model, endpoint, input, timeout) in [
        ("different-model", "http://127.0.0.1:1/v1", INPUT, 1),
        ("llama3.2:latest", "http://localhost:1/v1", INPUT, 1),
        (
            "llama3.2:latest",
            "http://user:secret@127.0.0.1:1/v1",
            INPUT,
            1,
        ),
        (
            "llama3.2:latest",
            "http://127.0.0.1:1/v1?secret=x",
            INPUT,
            1,
        ),
        ("llama3.2:latest", "http://127.0.0.1:1/v1", "[]", 1),
        ("llama3.2:latest", "http://127.0.0.1:1/v1", INPUT, 121),
    ] {
        let report = structured_probe(model, endpoint, input, timeout);
        assert!(!report.dispatch_started);
        assert!(report.error_code.is_some());
        assert!(report.content.is_none());
        assert!(report.diagnostic_content.is_none());
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
    }
}

#[test]
fn truncated_utf8_diagnostic_remains_an_exact_prefix_across_frames() {
    let prefix = format!(
        "{{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[],\"unknown\":\"{}",
        "中".repeat(24000)
    );
    let report = run(content(&prefix) + &content("\"}") + &end(false), false, 3);
    assert!(report.error_code.is_some());
    assert!(report.content.is_none());
    assert!(report.diagnostic_truncated);
    let diagnostic = report.diagnostic_content.unwrap();
    assert!(diagnostic.len() <= 64 * 1024);
    assert!(prefix.starts_with(&diagnostic));
}

#[test]
fn conflicting_usage_is_unknown_in_failed_diagnostics() {
    let text = r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#;
    let mut wire = content(text);
    wire += &event(
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}], "usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}}),
    );
    wire += &event(
        json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":8,"total_tokens":19}}),
    );
    let report = run(wire, true, 3);
    assert_eq!(
        report.error_code.as_deref(),
        Some("generation_invalid_usage")
    );
    assert!(report.usage.is_none());
    assert!(report.content.is_none());
}

#[test]
fn malformed_or_duplicate_usage_clears_previous_counts() {
    for bad_frame in [
        "[]",
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":-1,"total_tokens":10}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":11,"prompt_tokens":12,"completion_tokens":7,"total_tokens":18}}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18},"usage":{"prompt_tokens":11,"completion_tokens":8,"total_tokens":19}}"#,
    ] {
        let mut wire = content(r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#);
        wire += &event(
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}], "usage":{"prompt_tokens":11,"completion_tokens":7,"total_tokens":18}}),
        );
        wire += &format!("data: {bad_frame}\n\n");
        let report = run(wire, true, 3);
        assert_eq!(
            report.error_code.as_deref(),
            Some("generation_provider_frame_invalid")
        );
        assert!(report.usage.is_none());
        assert!(report.content.is_none());
    }
}
