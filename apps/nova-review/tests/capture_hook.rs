use nova_core::{VaultKey, compilation::AssetScope, intake::*};
use nova_review::capture_hook::*;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    thread,
    time::Duration,
};

fn private_write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
#[derive(Clone)]
enum Reply {
    Normal,
    Retry,
    Busy,
    BodyTimeout,
    WrongInstance,
    Revoked,
    Redirect,
    Oversized,
    LostAck,
}
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    report: CaptureHookSetupReport,
    stop: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
    session_response: Arc<AtomicU16>,
    provenance_response: Arc<AtomicU16>,
    provenance_calls: Arc<AtomicU16>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<Value>>>,
    join: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let scope = AssetScope {
            owner_id: "owner:hook-fixture".into(),
            space: "synthetic".into(),
        };
        let mut store = IntakeStore::create(
            &root.join("source.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        store.enable_capture(&scope).unwrap();
        let issued = store
            .create_capture_grant(
                &scope,
                CaptureGrantRequest {
                    request_id: "hook-grant".into(),
                    source_namespace: "nova-capture-codex".into(),
                    host: "codex".into(),
                    surface: "cli_hooks".into(),
                    host_version: "unverified".into(),
                    events: vec![
                        CaptureEventKind::UserPrompt,
                        CaptureEventKind::AssistantResponse,
                    ],
                    lifetime_seconds: 3600,
                    event_budget: 100,
                    byte_budget: 1024 * 1024,
                },
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let bundle = root.join("bundle.json");
        private_write(&bundle,&serde_json::to_vec(&json!({"schema_version":CAPTURE_SETUP_VERSION,"endpoint":format!("http://{address}/capture/v01"),"credential":issued.token,"grant":issued.grant})).unwrap());
        let report = setup_capture_hook(&bundle, &root.join("consumer"), None).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let replies = Arc::new(Mutex::new(VecDeque::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let online = Arc::new(AtomicBool::new(true));
        let online2 = online.clone();
        let session_response = Arc::new(AtomicU16::new(200));
        let session_response2 = session_response.clone();
        let provenance_response = Arc::new(AtomicU16::new(200));
        let provenance_response2 = provenance_response.clone();
        let provenance_calls = Arc::new(AtomicU16::new(0));
        let provenance_calls2 = provenance_calls.clone();
        let (stop2, replies2, seen2) = (stop.clone(), replies.clone(), seen.clone());
        let join = thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                // Listener polling is nonblocking; each accepted request uses bounded blocking I/O.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let (path, token, body) = request(&mut stream);
                let access = store.authenticate_capture(&scope, &token).unwrap();
                if path.ends_with("/session") {
                    if session_response2.load(Ordering::Relaxed) != 200 {
                        respond(
                            &mut stream,
                            session_response2.load(Ordering::Relaxed),
                            b"{}",
                            "Retry-After: 120\r\n",
                        );
                        continue;
                    }
                    if !online2.load(Ordering::Relaxed) {
                        respond(&mut stream, 503, b"{}", "");
                        continue;
                    }
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let session = store
                        .register_capture_session(
                            &scope,
                            &access,
                            v["origin_session_id"].as_str().unwrap(),
                        )
                        .unwrap();
                    respond(&mut stream, 200, &serde_json::to_vec(&session).unwrap(), "");
                    continue;
                }
                if path.ends_with("/provenance") {
                    provenance_calls2.fetch_add(1, Ordering::Relaxed);
                    let code = provenance_response2.load(Ordering::Relaxed);
                    if code != 200 {
                        respond(&mut stream, code, b"{}", "");
                        continue;
                    }
                    let input: CaptureProvenanceRequest = serde_json::from_slice(&body).unwrap();
                    let proof = store
                        .record_capture_provenance(&scope, &access, input)
                        .unwrap();
                    respond(&mut stream, 200, &serde_json::to_vec(&proof).unwrap(), "");
                    continue;
                }
                let v: Value = serde_json::from_slice(&body).unwrap();
                seen2.lock().unwrap().push(v);
                match replies2
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Reply::Normal)
                {
                    Reply::Normal => {
                        let ack = store.capture_event(&scope, &access, &body).unwrap();
                        respond(&mut stream, 200, &serde_json::to_vec(&ack).unwrap(), "")
                    }
                    Reply::LostAck => {
                        store.capture_event(&scope, &access, &body).unwrap();
                    }
                    Reply::WrongInstance => {
                        let mut ack = store.capture_event(&scope, &access, &body).unwrap();
                        ack.instance_id = "instance-wrong".into();
                        respond(&mut stream, 200, &serde_json::to_vec(&ack).unwrap(), "")
                    }
                    Reply::Retry => respond(&mut stream, 429, b"{}", "Retry-After: 0\r\n"),
                    Reply::Busy => respond(&mut stream, 503, b"{}", "Retry-After: 0\r\n"),
                    Reply::BodyTimeout => respond(&mut stream, 408, b"{}", "Retry-After: 0\r\n"),
                    Reply::Revoked => respond(&mut stream, 401, b"{}", ""),
                    Reply::Redirect => respond(
                        &mut stream,
                        307,
                        b"{}",
                        "Location: http://example.invalid/\r\n",
                    ),
                    Reply::Oversized => respond(&mut stream, 200, &vec![b'x'; 8193], ""),
                }
            }
        });
        Self {
            _dir: dir,
            root,
            report,
            stop,
            online,
            session_response,
            provenance_response,
            provenance_calls,
            replies,
            seen,
            join: Some(join),
        }
    }
    fn consume(&self, id: &str) -> anyhow::Result<CaptureHookRunReport> {
        consume_capture_hook(&self.report.binding_file,"codex",&self.report.installation_id,"UserPromptSubmit",&serde_json::to_vec(&json!({"hook_event_name":"UserPromptSubmit","session_id":"session-fixture","turn_id":id,"prompt":"VISIBLE SENTINEL TEXT","thinking":"HIDDEN SECRET SENTINEL","transcript_path":"/must-not-open/history","cwd":"/must-not-open"})).unwrap())
    }
    fn reply(&self, r: Reply) {
        self.replies.lock().unwrap().push_back(r);
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take()
            && let Err(payload) = j.join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown()
    }
}
fn request(s: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut header = Vec::new();
    let mut byte = [0];
    while !header.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() < 8192)
    }
    let h = String::from_utf8(header).unwrap();
    let path = h
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let token = h
        .lines()
        .find_map(|s| s.strip_prefix("Authorization: Bearer "))
        .unwrap()
        .to_owned();
    let len: usize = h
        .lines()
        .find_map(|s| s.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(len <= 96 * 1024);
    let mut body = vec![0; len];
    s.read_exact(&mut body).unwrap();
    (path, token, body)
}
fn respond(s: &mut TcpStream, status: u16, body: &[u8], extra: &str) {
    let _ = write!(
        s,
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(body);
}

#[test]
fn setup_is_private_uninstalled_and_render_requires_trust() {
    let f = Fixture::new();
    assert!(!f.report.installed);
    assert!(!f.report.pre_registered);
    assert!(f.report.first_event_requires_session_registration);
    let status = capture_hook_status(&f.report.binding_file, false).unwrap();
    assert_eq!(status.sessions, 0);
    assert!(!status.host_verified);
    let plan = render_capture_hook_plan(
        &f.report.binding_file,
        Path::new(env!("CARGO_BIN_EXE_nova-capture-hook")),
    )
    .unwrap();
    assert_eq!(plan["stage"], "rendered_uninstalled");
    assert_eq!(plan["real_host_verified"], false);
    let text = serde_json::to_string(&plan).unwrap();
    assert!(text.contains("enqueue-visible"));
    assert!(!text.contains("capture.token"));
    let binding = std::fs::read_to_string(&f.report.binding_file).unwrap();
    let token = std::fs::read_to_string(f.root.join("consumer/capture.token")).unwrap();
    assert!(!binding.contains(&token));
    assert!(
        setup_capture_hook(&f.root.join("bundle.json"), &f.root.join("consumer"), None).is_err()
    );
}
#[test]
fn native_roundtrip_projects_visible_only_and_deduplicates_after_restart() {
    let f = Fixture::new();
    let first = f.consume("turn-1").unwrap();
    assert!(first.local_queued && first.source_acknowledged);
    assert_eq!(first.sequence, 1);
    let replay = f.consume("turn-1").unwrap();
    assert!(replay.source_acknowledged);
    assert_eq!(replay.sequence, 1);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    let second = f.consume("turn-2").unwrap();
    assert_eq!(second.sequence, 2);
    assert!(second.source_acknowledged);
    let v = f.seen.lock().unwrap()[0].clone();
    assert_eq!(v["content"], "VISIBLE SENTINEL TEXT");
    assert_eq!(v["loss"]["partial_history"], true);
    assert!(!v.to_string().contains("HIDDEN"));
    assert!(!v.to_string().contains("transcript"));
    let s = capture_hook_status(&f.report.binding_file, false).unwrap();
    assert_eq!(s.acknowledged_events, 2);
    assert_eq!(s.pending_events, 0);
}
#[test]
fn retry_and_lost_ack_replay_keep_envelope_and_sequence() {
    for mode in [
        Reply::Retry,
        Reply::Busy,
        Reply::BodyTimeout,
        Reply::LostAck,
    ] {
        let f = Fixture::new();
        f.reply(mode);
        let first = f.consume("turn-1").unwrap();
        assert!(first.local_queued);
        assert!(!first.source_acknowledged);
        assert_eq!(first.state, CaptureRelayState::RetryWait);
        thread::sleep(Duration::from_millis(2100));
        let s = capture_hook_status(&f.report.binding_file, true).unwrap();
        assert_eq!(s.acknowledged_events, 1);
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
    }
}
#[test]
fn wrong_instance_redirect_revoked_are_sticky_and_never_ack() {
    for mode in [Reply::WrongInstance, Reply::Redirect, Reply::Revoked] {
        let f = Fixture::new();
        f.reply(mode);
        let r = f.consume("turn-1").unwrap();
        assert!(r.local_queued);
        assert!(!r.source_acknowledged);
        assert_eq!(r.state, CaptureRelayState::Rejected);
        let s = capture_hook_status(&f.report.binding_file, true).unwrap();
        assert_eq!(s.stopped_sessions, 1);
        assert_eq!(s.acknowledged_events, 0);
        assert_eq!(f.seen.lock().unwrap().len(), 1);
    }
}
#[test]
fn oversized_response_retries_and_offline_registered_session_stays_encrypted() {
    let mut f = Fixture::new();
    f.reply(Reply::Oversized);
    let r = f.consume("turn-1").unwrap();
    assert!(r.local_queued && !r.source_acknowledged);
    f.shutdown();
    let r = f.consume("turn-2").unwrap();
    assert!(r.local_queued && !r.source_acknowledged);
    assert_eq!(r.sequence, 2);
    let dir = std::fs::read_dir(f.root.join("consumer/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes = std::fs::read(dir.join("relay.sqlite")).unwrap();
    assert!(
        !bytes
            .windows(b"VISIBLE SENTINEL".len())
            .any(|b| b == b"VISIBLE SENTINEL")
    );
}
#[test]
fn invalid_invocation_and_private_files_reject_before_network() {
    let f = Fixture::new();
    assert!(
        consume_capture_hook(
            &f.report.binding_file,
            "claude",
            &f.report.installation_id,
            "UserPromptSubmit",
            b"{}"
        )
        .is_err()
    );
    assert!(
        consume_capture_hook(
            &f.report.binding_file,
            "codex",
            &f.report.installation_id,
            "Stop",
            &vec![b' '; 1024 * 1024 + 1]
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let token = f.root.join("consumer/capture.token");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        let r = f.consume("turn-1").unwrap();
        assert!(r.local_queued && !r.source_acknowledged);
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&token, token.with_extension("saved")).unwrap();
        symlink(token.with_extension("saved"), &token).unwrap();
        let r = f.consume("turn-1").unwrap();
        assert!(r.local_queued && !r.source_acknowledged);
    }
    assert!(f.seen.lock().unwrap().is_empty());
}
#[test]
fn cli_stdout_never_contains_visible_or_hidden_content() {
    let f = Fixture::new();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_nova-capture-hook"))
        .args([
            "enqueue-visible",
            "--binding-file",
            f.report.binding_file.to_str().unwrap(),
            "--host",
            "codex",
            "--installation",
            &f.report.installation_id,
            "--event",
            "UserPromptSubmit",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(&json!({"hook_event_name":"UserPromptSubmit","session_id":"cli-session","turn_id":"turn-1","prompt":"CLI VISIBLE SENTINEL","thinking":"HIDDEN SENTINEL"})).unwrap()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}\n");
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("source_acknowledged=true"));
    assert!(!err.contains("SENTINEL"));
}

#[test]
fn first_session_offline_is_encrypted_and_flush_registers_same_event() {
    let f = Fixture::new();
    f.online.store(false, Ordering::Relaxed);
    let a = f.consume("first-offline").unwrap();
    assert!(a.local_queued && !a.source_acknowledged);
    assert_eq!(a.sequence, 1);
    let s = capture_hook_status(&f.report.binding_file, false).unwrap();
    assert_eq!(s.bootstrap_pending_events, 1);
    assert_eq!(s.sessions, 0);
    let bytes = std::fs::read(f.root.join("consumer/bootstrap.sqlite")).unwrap();
    assert!(
        !bytes
            .windows(b"VISIBLE SENTINEL".len())
            .any(|b| b == b"VISIBLE SENTINEL")
    );
    f.online.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(2100));
    let s = capture_hook_status(&f.report.binding_file, true).unwrap();
    assert_eq!(s.bootstrap_pending_events, 0);
    assert_eq!(s.acknowledged_events, 1);
    let replay = f.consume("first-offline").unwrap();
    assert!(replay.source_acknowledged);
    assert_eq!(replay.sequence, 1);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}

#[test]
fn new_hook_sends_recovered_backlog_without_claiming_its_own_ack() {
    let f = Fixture::new();
    f.online.store(false, Ordering::Relaxed);
    assert!(!f.consume("offline-first").unwrap().source_acknowledged);
    f.online.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(2100));
    let second = f.consume("after-reconnect").unwrap();
    assert!(second.local_queued);
    assert!(!second.source_acknowledged);
    assert!(!second.delivery_stopped);
    assert!(second.receipt.is_none());
    assert_eq!(second.sequence, 2);
    {
        let received = f.seen.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["sequence"], 1);
    }
    let status = capture_hook_status(&f.report.binding_file, false).unwrap();
    assert_eq!(status.acknowledged_events, 1);
    assert_eq!(status.bootstrap_pending_events, 1);
    let third = f.consume("another-live-turn").unwrap();
    assert!(!third.source_acknowledged);
    assert_eq!(f.seen.lock().unwrap()[1]["sequence"], 2);

    let status = capture_hook_status(&f.report.binding_file, true).unwrap();
    assert_eq!(status.acknowledged_events, 3);
    assert_eq!(status.pending_events, 0);
    let replay = f.consume("after-reconnect").unwrap();
    assert!(replay.source_acknowledged);
    assert_eq!(replay.sequence, 2);
    assert_eq!(f.seen.lock().unwrap().len(), 3);
}

#[test]
fn revoked_backlog_stops_delivery_but_preserves_newly_queued_event() {
    let f = Fixture::new();
    f.online.store(false, Ordering::Relaxed);
    assert!(!f.consume("offline-first").unwrap().source_acknowledged);
    f.online.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(2100));
    f.reply(Reply::Revoked);
    let second = f.consume("after-reconnect").unwrap();
    assert!(second.local_queued && second.delivery_stopped);
    assert!(!second.source_acknowledged);
    assert!(second.receipt.is_none());
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    let status = capture_hook_status(&f.report.binding_file, true).unwrap();
    assert!(status.bootstrap_stopped);
    assert_eq!(status.bootstrap_pending_events, 1);
    assert_eq!(status.acknowledged_events, 0);
    assert!(f.consume("after-revocation").is_err());
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}

#[test]
fn explicit_flush_drains_same_session_backlog_up_to_its_dispatch_limit() {
    let f = Fixture::new();
    let flush = || {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_nova-capture-hook"))
            .args(["flush", "--binding-file"])
            .arg(&f.report.binding_file)
            .output()
            .unwrap();
        assert!(output.status.success(), "local flush command failed");
        serde_json::from_slice::<CaptureHookLocalStatus>(&output.stdout).unwrap()
    };
    f.online.store(false, Ordering::Relaxed);
    for sequence in 1..=17 {
        let queued = f.consume(&format!("offline-{sequence}")).unwrap();
        assert!(!queued.source_acknowledged);
    }
    assert!(f.seen.lock().unwrap().is_empty());
    f.online.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(4100));
    let first = flush();
    assert_eq!(first.acknowledged_events, 16);
    assert_eq!(first.pending_events, 1);
    assert_eq!(f.seen.lock().unwrap().len(), 16);
    let second = flush();
    assert_eq!(second.acknowledged_events, 17);
    assert_eq!(second.pending_events, 0);
    let mut sequences = f
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=17).collect::<Vec<_>>());
}

#[cfg(unix)]
#[test]
fn selected_download_is_secured_on_same_fd_but_ambiguous_files_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    let bundle = f.root.join("bundle.json");
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o644)).unwrap();
    let r = setup_capture_hook(&bundle, &f.root.join("consumer-secured"), None).unwrap();
    assert!(r.bundle_permissions_restricted);
    assert_eq!(
        std::fs::metadata(&bundle).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(setup_capture_hook(&bundle, &f.root.join("bad-writers"), None).is_err());
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.root.join("bundle-link.json");
    symlink(&bundle, &link).unwrap();
    assert!(setup_capture_hook(&link, &f.root.join("bad-link"), None).is_err());
    let hard = f.root.join("bundle-hard.json");
    std::fs::hard_link(&bundle, &hard).unwrap();
    assert!(setup_capture_hook(&bundle, &f.root.join("bad-hardlink"), None).is_err());
}

#[test]
fn first_registration_revocation_redirect_and_retry_after_are_bounded() {
    for (status, state, stopped) in [
        (401, CaptureRelayState::Rejected, true),
        (307, CaptureRelayState::Rejected, true),
        (429, CaptureRelayState::Exhausted, false),
        (408, CaptureRelayState::Exhausted, false),
        (503, CaptureRelayState::Exhausted, false),
    ] {
        let f = Fixture::new();
        f.session_response.store(status, Ordering::Relaxed);
        let r = f.consume("first-session").unwrap();
        assert!(r.local_queued && !r.source_acknowledged);
        assert_eq!(r.state, state);
        let s = capture_hook_status(&f.report.binding_file, true).unwrap();
        assert_eq!(s.bootstrap_stopped, stopped);
        assert_eq!(s.sessions, 0);
        assert_eq!(s.acknowledged_events, 0);
    }
}

fn history_line(id: &str) -> Value {
    json!({"schema_version":CAPTURE_HISTORY_VERSION,"host":"codex","origin_session_id":"session-fixture","host_turn_id":id,"kind":"user_prompt","content":"VISIBLE SENTINEL TEXT","occurred_at":null,"loss":{"redacted":false,"truncated":false,"partial_history":true},"origin":"user_provided_export"})
}
fn history_file(f: &Fixture, lines: &[Value]) -> PathBuf {
    let file = f.root.join("visible-export.jsonl");
    let bytes = lines
        .iter()
        .map(|v| serde_json::to_string(v).unwrap() + "\n")
        .collect::<String>();
    private_write(&file, bytes.as_bytes());
    file
}
#[test]
fn selected_history_and_live_use_one_identity_and_double_receipts() {
    let f = Fixture::new();
    let live = f.consume("turn-1").unwrap();
    assert!(live.provenance.is_some());
    let file = history_file(&f, &[history_line("turn-1"), history_line("turn-2")]);
    let first = import_visible_capture_history(&f.report.binding_file, &file, 1).unwrap();
    assert!(!first.completed);
    assert_eq!(first.progress.next_line, 1);
    assert_eq!(first.received_sources[0].provenance.origins.len(), 2);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    let completed = import_visible_capture_history(&f.report.binding_file, &file, 16).unwrap();
    assert!(completed.completed);
    assert_eq!(completed.received_sources.len(), 2);
    assert_eq!(f.seen.lock().unwrap().len(), 2);
    assert_eq!(completed.received_sources[1].provenance.sequence, 2);
    let again = import_visible_capture_history(&f.report.binding_file, &file, 16).unwrap();
    assert_eq!(again.processed_this_run, 0);
    assert_eq!(f.seen.lock().unwrap().len(), 2);
    let live2 = f.consume("turn-2").unwrap();
    assert!(live2.source_acknowledged);
    assert_eq!(f.seen.lock().unwrap().len(), 2);
}
#[test]
fn history_pending_provenance_is_not_complete_and_resumes_without_new_source() {
    let f = Fixture::new();
    f.provenance_response.store(503, Ordering::Relaxed);
    let file = history_file(&f, &[history_line("history-1")]);
    let p = import_visible_capture_history(&f.report.binding_file, &file, 16).unwrap();
    assert!(!p.completed);
    assert_eq!(p.progress.phase, CaptureHistoryPhase::PendingProvenance);
    assert_eq!(p.progress.next_line, 0);
    assert!(p.received_sources.is_empty());
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    f.provenance_response.store(200, Ordering::Relaxed);
    let p = import_visible_capture_history(&f.report.binding_file, &file, 16).unwrap();
    assert!(p.completed);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}
#[test]
fn history_provenance_revocation_stops_without_rolling_back_saved_source() {
    let f = Fixture::new();
    f.provenance_response.store(401, Ordering::Relaxed);
    let file = history_file(&f, &[history_line("history-revoked")]);
    let p = import_visible_capture_history(&f.report.binding_file, &file, 16).unwrap();
    assert!(!p.completed);
    assert_eq!(p.progress.phase, CaptureHistoryPhase::PendingProvenance);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    assert!(
        capture_hook_status(&f.report.binding_file, false)
            .unwrap()
            .bootstrap_stopped
    );
    f.provenance_response.store(200, Ordering::Relaxed);
    assert!(import_visible_capture_history(&f.report.binding_file, &file, 16).is_err());
    assert!(f.consume("history-revoked").is_err());
    assert!(f.consume("new-after-revocation").is_err());
    capture_hook_status(&f.report.binding_file, true).unwrap();
    assert_eq!(f.provenance_calls.load(Ordering::Relaxed), 1);
    assert_eq!(f.seen.lock().unwrap().len(), 1);
}
#[test]
fn history_file_mutation_and_conflicting_live_payload_fail_without_second_source() {
    let f = Fixture::new();
    let file = history_file(&f, &[history_line("history-1"), history_line("history-2")]);
    import_visible_capture_history(&f.report.binding_file, &file, 1).unwrap();
    let mut changed = history_line("history-2");
    changed["content"] = json!("CHANGED");
    history_file(&f, &[history_line("history-1"), changed]);
    assert!(import_visible_capture_history(&f.report.binding_file, &file, 16).is_err());
    assert_eq!(f.seen.lock().unwrap().len(), 1);
    let g = Fixture::new();
    g.consume("conflict").unwrap();
    let mut changed = history_line("conflict");
    changed["content"] = json!("DIFFERENT");
    let file = history_file(&g, &[changed]);
    assert!(import_visible_capture_history(&g.report.binding_file, &file, 16).is_err());
    assert_eq!(g.seen.lock().unwrap().len(), 1);
}
#[test]
fn history_preflight_rejects_hidden_fields_bad_later_line_and_nonpartial_export() {
    let f = Fixture::new();
    let good = history_line("good");
    let mut bad = history_line("bad");
    bad["thinking"] = json!("HIDDEN");
    let file = history_file(&f, &[good.clone(), bad]);
    assert!(import_visible_capture_history(&f.report.binding_file, &file, 16).is_err());
    assert!(f.seen.lock().unwrap().is_empty());
    let mut bad = good;
    bad["loss"]["partial_history"] = json!(false);
    history_file(&f, &[bad]);
    assert!(import_visible_capture_history(&f.report.binding_file, &file, 16).is_err());
    assert!(f.seen.lock().unwrap().is_empty());
    assert!(import_visible_capture_history(&f.report.binding_file, &file, 17).is_err());
}
