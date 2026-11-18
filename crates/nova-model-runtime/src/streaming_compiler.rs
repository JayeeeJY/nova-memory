//! Explicit local-only schema/stream adapter. Partial results never reach a commit path.
use super::{StructuredProbeReport, Transport, error, quote};
use crate::generation_guard::{CompletionEvidence, GenerationGuard, GuardedGeneration};
use nova_core::NovaError;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Default, Deserialize)]
struct Frame {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
    error: Option<Value>,
}
#[derive(Deserialize)]
struct Choice {
    index: u64,
    delta: Delta,
    finish_reason: Option<String>,
}
#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    role: Option<String>,
    tool_calls: Option<Value>,
    function_call: Option<Value>,
}
#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Default)]
struct Stream {
    guard: GenerationGuard,
    quote_text: bool,
    line: Vec<u8>,
    data: Vec<u8>,
    finish_reason: Option<String>,
    done: bool,
    usage: Option<Value>,
    diagnostic: Option<String>,
    diagnostic_truncated: bool,
    decoded_bytes: usize,
    started: Option<Instant>,
    first_content_ms: Option<u64>,
}
impl Stream {
    fn feed(&mut self, bytes: &[u8]) -> Result<(), NovaError> {
        self.guard
            .record_wire(bytes.len())
            .map_err(|e| error(e.code()))?;
        for byte in bytes {
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.line(&line)?;
            } else {
                self.line.push(*byte);
            }
        }
        Ok(())
    }
    fn line(&mut self, line: &[u8]) -> Result<(), NovaError> {
        if line.is_empty() {
            if !self.data.is_empty() {
                self.data.pop(); // The final SSE data separator is not part of the JSON.
                let data = std::mem::take(&mut self.data);
                self.event(&data)?;
            }
        } else if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            self.data.extend_from_slice(value);
            self.data.push(b'\n');
        } else if line == b"data" {
            self.data.push(b'\n');
        }
        // SSE comments, id/event/retry and unknown fields are never copied to the result.
        Ok(())
    }
    fn event(&mut self, data: &[u8]) -> Result<(), NovaError> {
        if self.done {
            return Err(error("generation_data_after_done"));
        }
        if data == b"[DONE]" {
            if self.finish_reason.is_none() {
                return Err(error("generation_done_before_finish"));
            }
            self.done = true;
            return Ok(());
        }
        if data.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            self.usage = None;
            return Err(error("generation_provider_frame_invalid"));
        }
        // Typed deserialization rejects duplicate fields affecting protocol, text and usage.
        // Unused provider metadata is ignored and never retained or executed.
        let frame: Frame = serde_json::from_slice(data).map_err(|_| {
            // A malformed or duplicate-key usage frame cannot safely confirm an earlier
            // usage observation. Keep accounting unknown instead of retaining stale counts.
            self.usage = None;
            error("generation_provider_frame_invalid")
        })?;
        let has_usage = frame.usage.is_some();
        if frame.error.is_some() {
            return Err(error("generation_provider_error"));
        }
        if frame.choices.len() > 1 {
            return Err(error("generation_multiple_choices"));
        }
        if let Some(choice) = frame.choices.first() {
            if choice.index != 0 || self.finish_reason.is_some() {
                return Err(error("generation_choice_after_finish_or_invalid_index"));
            }
            if choice.delta.tool_calls.is_some() || choice.delta.function_call.is_some() {
                return Err(error("generation_tool_output"));
            }
            if choice
                .delta
                .role
                .as_deref()
                .is_some_and(|role| role != "assistant")
            {
                return Err(error("generation_invalid_role"));
            }
            // Capture terminal evidence before validating generated content. A length frame
            // can contain real usage; rejecting its output must not turn that usage into zero.
            if let Some(reason) = &choice.finish_reason {
                self.finish_reason = Some(
                    match reason.as_str() {
                        "stop" => "stop",
                        "length" => "length",
                        _ => "other",
                    }
                    .into(),
                );
            }
        }
        if let Some(usage) = frame.usage {
            if self.usage.is_some()
                || self.finish_reason.is_none()
                || usage.prompt_tokens.checked_add(usage.completion_tokens)
                    != Some(usage.total_tokens)
            {
                self.usage = None;
                return Err(error("generation_invalid_usage"));
            }
            self.usage = Some(json!({"prompt_tokens":usage.prompt_tokens,
                "completion_tokens":usage.completion_tokens,"total_tokens":usage.total_tokens}));
        }
        if let Some(choice) = frame.choices.first() {
            if let Some(content) = &choice.delta.content {
                self.decoded_bytes = self.decoded_bytes.saturating_add(content.len());
                if !content.is_empty() && self.first_content_ms.is_none() {
                    self.first_content_ms = self.started.map(|started| millis(started.elapsed()));
                }
                if let Some(diagnostic) = &mut self.diagnostic
                    && !self.diagnostic_truncated
                {
                    let mut available = (64 * 1024 - diagnostic.len()).min(content.len());
                    while !content.is_char_boundary(available) {
                        available -= 1;
                    }
                    diagnostic.push_str(&content[..available]);
                    self.diagnostic_truncated |= available < content.len();
                }
                self.guard
                    .feed_content(content.as_bytes())
                    .map_err(|e| error(e.code()))?;
            }
            if let Some(reason) = &choice.finish_reason
                && reason != "stop"
            {
                return Err(error(if reason == "length" {
                    "generation_output_length"
                } else {
                    "generation_non_stop_finish"
                }));
            }
        }
        if frame.choices.is_empty() && !has_usage {
            return Err(error("generation_empty_provider_frame"));
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<GuardedGeneration, NovaError> {
        if !self.line.is_empty() || !self.data.is_empty() {
            return Err(error("generation_incomplete_sse_event"));
        }
        let evidence = CompletionEvidence {
            finish_reason: self.finish_reason.as_deref(),
            sse_done: self.done,
            usage: self.usage.as_ref(),
            ..CompletionEvidence::default()
        };
        let guard = std::mem::take(&mut self.guard);
        if self.quote_text {
            guard.finish_quote_text_envelope(evidence)
        } else {
            guard.finish(evidence)
        }
        .map_err(|e| error(e.code()))
    }
}

/// Only the dedicated synthetic driver enables diagnostic capture. Ordinary worker calls
/// retain the original guarded-result-only interface and never receive partial content.
pub(super) fn observe(
    transport: Transport,
) -> (Result<GuardedGeneration, NovaError>, StructuredProbeReport) {
    execute(transport, false, false)
}

pub(super) fn probe(transport: Transport) -> StructuredProbeReport {
    probe_protocol(transport, false)
}

pub(super) fn probe_quote_text(transport: Transport) -> StructuredProbeReport {
    probe_protocol(transport, true)
}

fn probe_protocol(transport: Transport, quote_text: bool) -> StructuredProbeReport {
    let (result, mut report) = execute(transport, true, quote_text);
    match result {
        Ok(output) => {
            report.content =
                Some(String::from_utf8(output.content).expect("guard validated UTF-8"));
            report.diagnostic_content = None;
            report.diagnostic_truncated = false;
        }
        Err(failure) => report.error_code = Some(code(&failure)),
    }
    report
}

pub(super) fn code(failure: &NovaError) -> String {
    match failure {
        NovaError::Validation(message) => message
            .strip_prefix("compiler: ")
            .filter(|code| {
                code.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            })
            .unwrap_or("structured_probe_internal_error")
            .into(),
        _ => "structured_probe_internal_error".into(),
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// curl is the only spawned child. Cancellation reaps that child, never a model service.
fn execute(
    transport: Transport,
    diagnostics: bool,
    quote_text: bool,
) -> (Result<GuardedGeneration, NovaError>, StructuredProbeReport) {
    let started = Instant::now();
    let mut report = StructuredProbeReport::default();
    let mut stream = Stream {
        quote_text,
        diagnostic: diagnostics.then(String::new),
        started: Some(started),
        ..Stream::default()
    };
    let result = execute_inner(transport, started, &mut stream, &mut report);
    report.elapsed_ms = millis(started.elapsed());
    report.wire_bytes = report.wire_bytes.max(stream.guard.wire_bytes());
    report.decoded_bytes = stream.decoded_bytes;
    report.completed_units = report.completed_units.max(stream.guard.completed_units());
    report.finish_reason = stream.finish_reason;
    report.sse_done = stream.done;
    report.usage = stream
        .usage
        .and_then(|value| serde_json::from_value(value).ok());
    report.first_content_ms = stream.first_content_ms;
    report.diagnostic_content = stream.diagnostic.filter(|content| !content.is_empty());
    report.diagnostic_truncated = stream.diagnostic_truncated;
    (result, report)
}

fn execute_inner(
    transport: Transport,
    started: Instant,
    stream: &mut Stream,
    report: &mut StructuredProbeReport,
) -> Result<GuardedGeneration, NovaError> {
    if transport.credential.is_some() || transport.resolve.is_some() {
        return Err(error("structured_output_local_only"));
    }
    let url = url::Url::parse(&transport.url).map_err(|_| error("endpoint_invalid"))?;
    if url.scheme() != "http"
        || !url
            .host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
    {
        return Err(error("structured_output_local_only"));
    }
    let deadline = started + Duration::from_secs(transport.timeout_seconds);
    let mut config = Zeroizing::new(format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\ndata-binary = {}\n",
        quote(&transport.url),
        quote(&transport.body)
    ));
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--silent",
            "--fail",
            "--no-buffer",
            "--max-time",
            &transport.timeout_seconds.to_string(),
            "--connect-timeout",
            "3",
            "--max-filesize",
            "1048576",
            "--write-out",
            "%{stderr}%{http_code}",
            "--max-redirs",
            "0",
            "--retry",
            "0",
            "--proto",
            "=http",
            "--proxy",
            "",
            "--noproxy",
            "*",
            "--config",
            "-",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| error("transport_start_failed"))?;
    report.dispatch_started = true;
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(mut stdin), Some(mut stdout), Some(mut metadata)) = pipes else {
        report.owned_child_reaped = reap_owned(&mut child);
        report.curl_exit_code = child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code());
        return Err(error(if report.owned_child_reaped {
            "generation_transport_pipe_failed"
        } else {
            "generation_owned_child_cleanup_failed"
        }));
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("nova-model-stream".into())
        .spawn(move || {
            if stdin.write_all(config.as_bytes()).is_err() {
                let _ = sender.send(Err(()));
                return;
            }
            config.clear();
            drop(stdin);
            loop {
                let mut bytes = vec![0; 8192];
                match stdout.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.truncate(count);
                        if sender.send(Ok(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(Err(()));
                        break;
                    }
                }
            }
        });
    let reader = match reader {
        Ok(reader) => reader,
        Err(_) => {
            report.owned_child_reaped = reap_owned(&mut child);
            report.curl_exit_code = child
                .try_wait()
                .ok()
                .flatten()
                .and_then(|status| status.code());
            return Err(error(if report.owned_child_reaped {
                "generation_transport_reader_start_failed"
            } else {
                "generation_owned_child_cleanup_failed"
            }));
        }
    };
    let mut result = (|| {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(error("generation_client_deadline"));
            }
            match receiver.recv_timeout(remaining) {
                Ok(Ok(bytes)) => {
                    stream.feed(&bytes)?;
                }
                Ok(Err(())) => return Err(error("generation_transport_read_failed")),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(error("generation_client_deadline"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| error("transport_wait_failed"))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(error("generation_client_deadline"));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        report.curl_exit_code = status.code();
        // Separate metadata keeps terminal SSE events immediately actionable on open streams.
        report.http_status = read_http_status(&mut metadata);
        if !status.success() {
            return Err(error(match status.code() {
                Some(28) => "generation_client_deadline",
                Some(63) => "generation_wire_limit",
                Some(22) => "generation_http_status_rejected",
                _ => "generation_transport_failed",
            }));
        }
        if !report
            .http_status
            .is_some_and(|code| (200..300).contains(&code))
        {
            return Err(error("generation_http_status_rejected"));
        }
        report.wire_bytes = stream.guard.wire_bytes();
        report.completed_units = stream.guard.completed_units();
        stream.finish()
    })();
    drop(receiver);
    report.owned_child_reaped = reap_owned(&mut child);
    report.curl_exit_code = child
        .try_wait()
        .ok()
        .flatten()
        .and_then(|status| status.code());
    // Never wait on metadata or pipe readers if the owned child could not be terminated.
    let joined = report.owned_child_reaped && reader.join().is_ok();
    if report.owned_child_reaped && report.http_status.is_none() {
        report.http_status = read_http_status(&mut metadata);
    }
    if !report.owned_child_reaped || !joined {
        result = Err(error("generation_owned_child_cleanup_failed"));
    }
    result
}

fn read_http_status(metadata: &mut impl Read) -> Option<u16> {
    let mut trailer = Vec::new();
    metadata.take(4).read_to_end(&mut trailer).ok()?;
    (trailer.len() == 3 && trailer.iter().all(u8::is_ascii_digit))
        .then_some(())
        .and_then(|_| std::str::from_utf8(&trailer).ok())
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|code| *code >= 100)
}

fn reap_owned(child: &mut std::process::Child) -> bool {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return true;
    }
    // A kill error can race with natural exit. Always poll/reap after the signal attempt;
    // a still-running or unreadable child is a cleanup failure, never silently ignored.
    let _signal_result = child.kill();
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) if Instant::now() >= deadline => return false,
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(value: Value) -> Vec<u8> {
        format!("data: {value}\n\n").into_bytes()
    }
    fn content(text: &str) -> Vec<u8> {
        event(json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]}))
    }
    fn stop() -> Vec<u8> {
        [event(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
            event(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})),
            b"data: [DONE]\n\n".to_vec()].concat()
    }
    #[test]
    fn bytewise_sse_role_and_usage_complete_without_extra_provider_data() {
        let wire = [
            event(json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]})),
            content(r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#),
            stop(),
        ]
        .concat();
        let mut stream = Stream::default();
        for byte in &wire {
            stream.feed(&[*byte]).unwrap();
        }
        let output = stream.finish().unwrap();
        assert_eq!(output.usage.unwrap().completion_tokens, 5);
        assert_eq!(output.completed_units, 0);
        assert_eq!(output.wire_bytes, wire.len());
    }
    #[test]
    fn invalid_protocol_and_length_never_return_content() {
        for extra in [
            event(json!({"choices":[{"index":1,"delta":{"content":"x"}}]})),
            event(json!({"choices":[{"index":0,"delta":{"tool_calls":[]}}]})),
            event(json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]})),
            event(json!({"error":{"message":"never-log-this"}})),
            b"data: [DONE]\n\n".to_vec(),
            b"data: {\"choices\":[],\"choices\":[]}\n\n".to_vec(),
        ] {
            let error = Stream::default().feed(&extra).unwrap_err().to_string();
            assert!(error.starts_with("invalid request: compiler: generation_"));
            assert!(!error.contains("never-log-this"));
        }
        let mut stream = Stream::default();
        stream
            .feed(&content(
                r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#,
            ))
            .unwrap();
        assert!(stream.finish().is_err());
    }
    #[test]
    fn conflicting_usage_and_data_after_terminal_fail_closed() {
        for tail in [
            content("extra"),
            stop(),
            event(
                json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
            ),
        ] {
            let mut stream = Stream::default();
            stream
                .feed(&content(
                    r#"{"schema_version":"nova.extraction.v0.1","units":[]}"#,
                ))
                .unwrap();
            stream.feed(&stop()).unwrap();
            assert!(stream.feed(&tail).is_err());
        }
    }

    #[test]
    fn own_curl_is_reaped_at_deadline_when_server_keeps_body_open() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 8192);
            }
            let length: usize = String::from_utf8(headers)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 128 * 1024);
            socket.read_exact(&mut vec![0; length]).unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
            let mut byte = [0];
            matches!(socket.read(&mut byte), Ok(0)) || socket.take_error().unwrap().is_some()
        });
        let started = Instant::now();
        let failed = observe(Transport {
            url: format!("http://{address}/v1/chat/completions"),
            resolve: None,
            body: Zeroizing::new("{\"stream\":true}".into()),
            credential: None,
            timeout_seconds: 1,
            output_mode: super::super::OutputMode::StructuredLocal,
        })
        .0
        .unwrap_err();
        assert!(failed.to_string().contains("generation_client_deadline"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(server.join().unwrap());
    }
}
