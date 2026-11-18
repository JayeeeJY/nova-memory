//! Local-only native capture consumer. No Host discovery, transcript reading, model calls or installation.
use anyhow::{Result, bail, ensure};
use nova_core::{
    VaultKey,
    compilation::{IntakeReceipt, decode_untrusted_json},
    intake::*,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub const CAPTURE_SETUP_VERSION: &str = "nova.capture.setup.v0.1";
const BINDING_VERSION: &str = "nova.capture.hook-binding.v0.1";
const MAX_STDIN: usize = 1024 * 1024;
#[path = "capture_history.rs"]
mod capture_history;
pub use capture_history::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupBundle {
    schema_version: String,
    endpoint: String,
    credential: String,
    grant: CaptureGrant,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema_version: String,
    installation_id: String,
    host: String,
    endpoint: String,
    grant: CaptureGrant,
    key_file: PathBuf,
    credential_file: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionBinding {
    origin_session_id: String,
    session: CaptureSession,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureHookSetupReport {
    pub binding_file: PathBuf,
    pub installation_id: String,
    pub host: String,
    pub installed: bool,
    pub pre_registered: bool,
    pub first_event_requires_session_registration: bool,
    pub offline_enqueue_ready: bool,
    pub bundle_permissions_restricted: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureHookRunReport {
    pub receipt: Option<IntakeReceipt>,
    pub provenance: Option<CaptureProvenanceRecord>,
    pub local_queued: bool,
    pub source_acknowledged: bool,
    #[serde(default)]
    pub delivery_stopped: bool,
    pub state: CaptureRelayState,
    pub sequence: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureHookLocalStatus {
    pub installed: bool,
    pub host_verified: bool,
    pub sessions: usize,
    pub pending_events: usize,
    pub acknowledged_events: usize,
    pub stopped_sessions: usize,
    pub bootstrap_pending_events: usize,
    pub bootstrap_stopped: bool,
}

fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
}
fn now() -> i64 {
    nova_time::OffsetDateTime::now_utc().unix_timestamp()
}
fn timestamp(s: &str) -> Result<i64> {
    Ok(
        nova_time::OffsetDateTime::parse(s, &nova_time::format_description::well_known::Rfc3339)?
            .unix_timestamp(),
    )
}
fn absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "capture_path_required"
    );
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "capture_private_directory_required"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
            "capture_private_directory_required"
        );
    }
    Ok(())
}
fn make_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    private_dir(path)
}
fn private_open(path: &Path) -> Result<File> {
    absolute(path)?;
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(path)?;
    let m = f.metadata()?;
    ensure!(m.is_file(), "capture_private_file_required");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0 && m.nlink() == 1,
            "capture_private_file_required"
        );
    }
    Ok(f)
}
fn private_read(path: &Path, max: usize) -> Result<Zeroizing<Vec<u8>>> {
    let f = private_open(path)?;
    ensure!(f.metadata()?.len() <= max as u64, "capture_file_bounds");
    let mut b = Zeroizing::new(Vec::new());
    f.take(max as u64 + 1).read_to_end(&mut b)?;
    ensure!(b.len() <= max, "capture_file_bounds");
    Ok(b)
}
fn bundle_read(path: &Path) -> Result<(Zeroizing<Vec<u8>>, bool)> {
    absolute(path)?;
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let f = o.open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.len() <= 64 * 1024,
        "capture_bundle_file_required"
    );
    #[cfg(unix)]
    let restricted = {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        bundle_metadata_allowed(m.uid(), m.nlink(), m.mode(), unsafe { libc::geteuid() })?;
        let changed = m.mode() & 0o777 != 0o600;
        if changed {
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            f.sync_all()?;
        }
        changed
    };
    #[cfg(not(unix))]
    let restricted = false;
    let mut bytes = Zeroizing::new(Vec::new());
    f.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 64 * 1024, "capture_bundle_bounds");
    Ok((bytes, restricted))
}
#[cfg(unix)]
fn bundle_metadata_allowed(uid: u32, nlink: u64, mode: u32, current_uid: u32) -> Result<()> {
    ensure!(
        uid == current_uid && nlink == 1 && mode & 0o022 == 0,
        "capture_bundle_owner_or_write_permissions"
    );
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_new(path, &serde_json::to_vec_pretty(value)?)
}
fn endpoint(value: &str) -> Result<(SocketAddr, String)> {
    let rest = value
        .strip_prefix("http://127.0.0.1:")
        .ok_or_else(|| anyhow::anyhow!("capture_fixed_loopback_endpoint_required"))?;
    let port = rest
        .strip_suffix("/capture/v01")
        .ok_or_else(|| anyhow::anyhow!("capture_fixed_loopback_endpoint_required"))?;
    ensure!(
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
        "capture_fixed_loopback_endpoint_required"
    );
    let port: u16 = port.parse()?;
    ensure!(port > 0, "capture_fixed_loopback_endpoint_required");
    Ok((
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        format!("127.0.0.1:{port}"),
    ))
}
fn token(path: &Path) -> Result<Zeroizing<String>> {
    let b = private_read(path, 128)?;
    let t = Zeroizing::new(std::str::from_utf8(&b)?.trim().to_owned());
    ensure!(
        t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()),
        "capture_token_invalid"
    );
    Ok(t)
}
fn key(path: &Path) -> Result<VaultKey> {
    let b = private_read(path, 128)?;
    Ok(VaultKey::from_hex(std::str::from_utf8(&b)?)?)
}
fn host(name: &str) -> Result<CaptureHookHost> {
    match name {
        "codex" => Ok(CaptureHookHost::CodexCli),
        "claude" => Ok(CaptureHookHost::ClaudeCodeCli),
        _ => bail!("capture_host_unsupported"),
    }
}
fn load(path: &Path) -> Result<(Binding, PathBuf, File)> {
    absolute(path)?;
    let root = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("capture_binding_path"))?
        .canonicalize()?;
    private_dir(&root)?;
    ensure!(
        path.file_name().and_then(|s| s.to_str()) == Some("binding.json"),
        "capture_binding_path"
    );
    let binding: Binding = decode_untrusted_json(&private_read(path, 64 * 1024)?)?;
    ensure!(
        binding.schema_version == BINDING_VERSION
            && ident(&binding.installation_id)
            && binding.key_file == root.join("relay.key")
            && binding.credential_file == root.join("capture.token"),
        "capture_binding_mismatch"
    );
    endpoint(&binding.endpoint)?;
    host(&binding.host)?;
    ensure!(
        binding.host == binding.grant.host
            && binding.grant.surface == "cli_hooks"
            && !binding.grant.revoked,
        "capture_binding_mismatch"
    );
    private_dir(&root.join("sessions"))?;
    let lock = private_open(&root.join("consumer.lock"))?;
    lock.try_lock()?;
    Ok((binding, root, lock))
}

struct Response {
    status: u16,
    body: Vec<u8>,
    retry_after: Option<u32>,
}
#[derive(Debug)]
struct RegistrationFailure {
    stop: Option<CaptureRelayStop>,
    retry_after: Option<u32>,
}
impl std::fmt::Display for RegistrationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capture_session_registration_incomplete")
    }
}
impl std::error::Error for RegistrationFailure {}
#[derive(Debug)]
struct ProvenanceFailure(CaptureRelayStop);
impl std::fmt::Display for ProvenanceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capture_provenance_stopped")
    }
}
impl std::error::Error for ProvenanceFailure {}
fn post(binding: &Binding, token: &str, suffix: &str, body: &[u8]) -> Result<Response> {
    ensure!(
        matches!(suffix, "session" | "events" | "provenance") && body.len() <= 96 * 1024,
        "capture_request_bounds"
    );
    let (address, authority) = endpoint(&binding.endpoint)?;
    let started = Instant::now();
    let timeout = Duration::from_millis(200);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = Zeroizing::new(format!(
        "POST /capture/v01/{suffix} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    ));
    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        ensure!(
            header.len() < 8192 && started.elapsed() < Duration::from_millis(1200),
            "capture_response_bounds"
        );
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let h = std::str::from_utf8(&header)?;
    let mut lines = h.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    ensure!(
        matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")),
        "capture_http_version"
    );
    let status: u16 = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("capture_http_status"))?
        .parse()?;
    let mut length = None;
    let mut retry_after = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("capture_http_header"))?;
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(length.is_none(), "capture_ambiguous_http_length");
            length = Some(value.trim().parse::<usize>()?);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("capture_unsupported_http_framing");
        }
        if name.eq_ignore_ascii_case("retry-after") {
            ensure!(retry_after.is_none(), "capture_ambiguous_retry_after");
            retry_after = Some(value.trim().parse().unwrap_or(u32::MAX));
        }
    }
    // Error bodies are unnecessary; fixed status/Retry-After drive bounded handling.
    if status != 200 {
        return Ok(Response {
            status,
            body: vec![],
            retry_after,
        });
    }
    let len = length.ok_or_else(|| anyhow::anyhow!("capture_http_length_required"))?;
    ensure!(len <= 8192, "capture_response_bounds");
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    ensure!(
        started.elapsed() < Duration::from_millis(1200),
        "capture_response_timeout"
    );
    Ok(Response {
        status,
        body,
        retry_after,
    })
}
fn bootstrap_configuration(binding: &Binding) -> CaptureBootstrapConfiguration {
    CaptureBootstrapConfiguration {
        instance_id: binding.grant.instance_id.clone(),
        target_id: format!("loopback-{}", hex_digest(binding.endpoint.as_bytes())),
        grant_id: binding.grant.grant_id.clone(),
        source_namespace: binding.grant.source_namespace.clone(),
        expires_at: binding.grant.expires_at.clone(),
        policy: CaptureRelayPolicy {
            version: "native-visible-v1".into(),
            allowed_events: binding.grant.events.clone(),
            redact_literals: vec![],
            max_content_bytes: 32 * 1024,
        },
        max_pending_events: 256,
        max_pending_bytes: 8 * 1024 * 1024,
        max_total_events: 10_000,
        ttl_seconds: 86400,
        lease_seconds: 10,
        max_attempts: 5,
        retry_base_seconds: 2,
        retry_max_seconds: 60,
    }
}
fn open_bootstrap(binding: &Binding, root: &Path) -> Result<CaptureBootstrap> {
    let path = root.join("bootstrap.sqlite");
    let cfg = bootstrap_configuration(binding);
    let k = key(&binding.key_file)?;
    if path.try_exists()? {
        private_open(&path)?;
        Ok(CaptureBootstrap::open(&path, k, cfg)?)
    } else {
        Ok(CaptureBootstrap::create(&path, k, cfg, now())?)
    }
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn session_dir(root: &Path, origin: &str) -> PathBuf {
    root.join("sessions").join(hex_digest(origin.as_bytes()))
}
fn ensure_session(
    binding: &Binding,
    root: &Path,
    origin: &str,
) -> Result<(SessionBinding, PathBuf)> {
    ensure!(ident(origin), "capture_session_identity_required");
    let dir = session_dir(root, origin);
    let session_path = dir.join("session.json");
    let record = if session_path.try_exists()? {
        private_dir(&dir)?;
        let record: SessionBinding = decode_untrusted_json(&private_read(&session_path, 8192)?)?;
        record
    } else {
        ensure!(
            std::fs::read_dir(root.join("sessions"))?.count() < 64,
            "capture_local_session_capacity"
        );
        let t = token(&binding.credential_file)?;
        let response = post(
            binding,
            &t,
            "session",
            &serde_json::to_vec(&serde_json::json!({"origin_session_id":origin}))?,
        )?;
        if response.status != 200 {
            let stop = if matches!(response.status, 401 | 403 | 410) {
                Some(CaptureRelayStop::Revoked)
            } else if (300..400).contains(&response.status) {
                Some(CaptureRelayStop::TargetMismatch)
            } else if (400..500).contains(&response.status) && !matches!(response.status, 408 | 429)
            {
                Some(CaptureRelayStop::InvalidEvent)
            } else {
                None
            };
            return Err(RegistrationFailure {
                stop,
                retry_after: matches!(response.status, 408 | 429 | 503).then_some(
                    response
                        .retry_after
                        .unwrap_or(if response.status == 429 { 60 } else { 1 }),
                ),
            }
            .into());
        }
        let session: CaptureSession = decode_untrusted_json(&response.body)?;
        let record = SessionBinding {
            origin_session_id: origin.into(),
            session,
        };
        validate_session(binding, &record, origin)?;
        if !dir.try_exists()? {
            make_dir(&dir)?;
        }
        private_dir(&dir)?;
        write_json(&session_path, &record)?;
        record
    };
    validate_session(binding, &record, origin)?;
    Ok((record, dir))
}
fn validate_session(binding: &Binding, r: &SessionBinding, origin: &str) -> Result<()> {
    ensure!(
        r.origin_session_id == origin
            && r.session.origin_session_id == origin
            && r.session.source_namespace == binding.grant.source_namespace
            && ident(&r.session.session_id),
        "capture_session_binding_mismatch"
    );
    ensure!(
        timestamp(&r.session.expires_at)? <= timestamp(&binding.grant.expires_at)?
            && timestamp(&r.session.expires_at)? > now(),
        "capture_session_expired"
    );
    Ok(())
}
fn open_relay(binding: &Binding, record: &SessionBinding, dir: &Path) -> Result<CaptureRelay> {
    let path = dir.join("relay.sqlite");
    let cfg = bootstrap_configuration(binding).relay_configuration(&record.session)?;
    let k = key(&binding.key_file)?;
    if path.try_exists()? {
        private_open(&path)?;
        Ok(CaptureRelay::open(&path, k, cfg)?)
    } else {
        Ok(CaptureRelay::create(&path, k, cfg, now())?)
    }
}
fn send(binding: &Binding, bearer: &str, envelope: &CaptureEnvelope) -> CaptureRelayDelivery {
    let result = serde_json::to_vec(envelope)
        .map_err(anyhow::Error::from)
        .and_then(|b| post(binding, bearer, "events", &b));
    match result {
        Ok(r) if r.status == 200 => match decode_untrusted_json::<CaptureAck>(&r.body) {
            Ok(ack) if ack.instance_id == binding.grant.instance_id => {
                CaptureRelayDelivery::SourceAck(Box::new(ack))
            }
            _ => CaptureRelayDelivery::Reject(CaptureRelayStop::TargetMismatch),
        },
        Ok(r) if matches!(r.status, 401 | 403 | 410) => {
            CaptureRelayDelivery::Reject(CaptureRelayStop::Revoked)
        }
        Ok(r) if (300..400).contains(&r.status) => {
            CaptureRelayDelivery::Reject(CaptureRelayStop::TargetMismatch)
        }
        Ok(r) if r.status == 409 => CaptureRelayDelivery::Reject(CaptureRelayStop::Conflict),
        Ok(r) if matches!(r.status, 408 | 429 | 503) => CaptureRelayDelivery::Retry {
            retry_after_seconds: Some(r.retry_after.unwrap_or(if r.status == 429 {
                60
            } else {
                1
            })),
        },
        Ok(r) if (400..500).contains(&r.status) => {
            CaptureRelayDelivery::Reject(CaptureRelayStop::InvalidEvent)
        }
        _ => CaptureRelayDelivery::Retry {
            retry_after_seconds: None,
        },
    }
}

pub fn setup_capture_hook(
    bundle_file: &Path,
    directory: &Path,
    origin_session_id: Option<&str>,
) -> Result<CaptureHookSetupReport> {
    absolute(directory)?;
    let (bytes, bundle_permissions_restricted) = bundle_read(bundle_file)?;
    let mut bundle: SetupBundle = decode_untrusted_json(&bytes)?;
    let credential = Zeroizing::new(std::mem::take(&mut bundle.credential));
    ensure!(
        bundle.schema_version == CAPTURE_SETUP_VERSION,
        "capture_setup_version"
    );
    endpoint(&bundle.endpoint)?;
    host(&bundle.grant.host)?;
    ensure!(
        bundle.grant.surface == "cli_hooks"
            && !bundle.grant.revoked
            && timestamp(&bundle.grant.expires_at)? > now(),
        "capture_setup_grant"
    );
    ensure!(
        [
            &bundle.grant.instance_id,
            &bundle.grant.grant_id,
            &bundle.grant.source_namespace
        ]
        .iter()
        .all(|s| ident(s)),
        "capture_setup_grant"
    );
    ensure!(
        credential.len() == 64 && credential.bytes().all(|b| b.is_ascii_hexdigit()),
        "capture_setup_credential"
    );
    ensure!(
        !bundle.grant.events.is_empty() && bundle.grant.events.len() <= 2,
        "capture_setup_events"
    );
    let parent = directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("capture_setup_directory"))?
        .canonicalize()?;
    let root = parent.join(
        directory
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("capture_setup_directory"))?,
    );
    make_dir(&root)?;
    make_dir(&root.join("sessions"))?;
    let installation_id = format!("capture-{}", uuid::Uuid::now_v7());
    let key_file = root.join("relay.key");
    let credential_file = root.join("capture.token");
    VaultKey::generate()?.write_new_file(&key_file)?;
    write_new(&credential_file, credential.as_bytes())?;
    write_new(&root.join("consumer.lock"), b"")?;
    let binding = Binding {
        schema_version: BINDING_VERSION.into(),
        installation_id: installation_id.clone(),
        host: bundle.grant.host.clone(),
        endpoint: bundle.endpoint,
        grant: bundle.grant,
        key_file,
        credential_file,
    };
    let binding_file = root.join("binding.json");
    write_json(&binding_file, &binding)?;
    let _ = open_bootstrap(&binding, &root)?;
    let pre_registered = if let Some(origin) = origin_session_id {
        ensure_session(&binding, &root, origin).is_ok()
    } else {
        false
    };
    Ok(CaptureHookSetupReport {
        binding_file,
        installation_id,
        host: binding.host,
        installed: false,
        pre_registered,
        first_event_requires_session_registration: !pre_registered,
        offline_enqueue_ready: true,
        bundle_permissions_restricted,
    })
}

pub fn consume_capture_hook(
    binding_file: &Path,
    expected_host: &str,
    installation_id: &str,
    event_name: &str,
    stdin: &[u8],
) -> Result<CaptureHookRunReport> {
    ensure!(stdin.len() <= MAX_STDIN, "capture_hook_stdin_bounds");
    let (binding, root, _lock) = load(binding_file)?;
    ensure!(
        binding.host == expected_host && binding.installation_id == installation_id,
        "capture_hook_invocation_binding"
    );
    let input: serde_json::Value = decode_untrusted_json(stdin)?;
    ensure!(
        input.get("hook_event_name").and_then(|v| v.as_str()) == Some(event_name),
        "capture_hook_event_binding"
    );
    let origin = input
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("capture_hook_session"))?;
    let event = decode_visible_capture_hook(host(expected_host)?, origin, 1, stdin)?;
    process_visible_event(
        &binding,
        &root,
        origin,
        event,
        CaptureDeliveryOrigin::HookUnverified,
    )
}
fn process_visible_event(
    binding: &Binding,
    root: &Path,
    origin: &str,
    event: CaptureRelayEvent,
    delivery_origin: CaptureDeliveryOrigin,
) -> Result<CaptureHookRunReport> {
    let event_id = event.event_id.clone();
    let kind = event.kind;
    let mut spool = open_bootstrap(binding, root)?;
    // An idempotent enqueue may return an already transferred receipt after stop.
    // That receipt is historical evidence, never permission for another HTTP call.
    ensure!(
        spool.snapshot(now())?.stopped.is_none(),
        "capture_binding_stopped"
    );
    let queued = spool.enqueue(origin, event, now())?;
    if queued.state != CaptureBootstrapState::Transferred {
        transfer_pending(binding, root, &mut spool, Some(origin))?;
    }
    let boot = spool.snapshot(now())?;
    let mut delivery_stopped = boot.stopped.is_some();
    let status = boot
        .events
        .iter()
        .find(|s| s.event_key == queued.event_key)
        .ok_or_else(|| anyhow::anyhow!("capture_bootstrap_status"))?;
    let dir = session_dir(root, origin);
    // The one bootstrap transfer can belong to an older event. Give its relay
    // a bounded send opportunity even while the current event is still queued.
    let relay_status = if status.transfer.is_some()
        || (!delivery_stopped && dir.join("relay.sqlite").try_exists()?)
    {
        private_dir(&dir)?;
        let record: SessionBinding =
            decode_untrusted_json(&private_read(&dir.join("session.json"), 8192)?)?;
        ensure!(
            record.origin_session_id == origin
                && record.session.origin_session_id == origin
                && record.session.source_namespace == binding.grant.source_namespace
                && status
                    .transfer
                    .as_ref()
                    .is_none_or(|t| record.session.session_id == t.session_id),
            "capture_transfer_session_mismatch"
        );
        let mut relay = open_relay(binding, &record, &dir)?;
        if !delivery_stopped && let Ok(t) = token(&binding.credential_file) {
            let _ = relay.dispatch_one(&t, |_, bearer, envelope| send(binding, bearer, envelope));
        }
        let snapshot = relay.snapshot()?;
        if let Some(reason) = snapshot.stopped {
            spool.stop(reason, now())?;
            delivery_stopped = true;
        }
        Some((record, snapshot))
    } else {
        None
    };
    if let Some(transferred) = &status.transfer {
        let (record, snapshot) =
            relay_status.ok_or_else(|| anyhow::anyhow!("capture_relay_status"))?;
        let state = snapshot
            .events
            .into_iter()
            .find(|e| e.event_key == transferred.relay_event_key)
            .ok_or_else(|| anyhow::anyhow!("capture_relay_status"))?;
        let provenance = if delivery_stopped {
            None
        } else if let Some(receipt) = &state.receipt {
            match confirm_provenance(
                binding,
                &record.session,
                &event_id,
                kind,
                state.sequence,
                &status.loss,
                receipt,
                delivery_origin,
            ) {
                Ok(p) => Some(p),
                Err(error) => {
                    if let Some(failure) = error.downcast_ref::<ProvenanceFailure>() {
                        spool.stop(failure.0, now())?;
                        delivery_stopped = true;
                    }
                    None
                }
            }
        } else {
            None
        };
        return Ok(CaptureHookRunReport {
            receipt: state.receipt,
            provenance,
            local_queued: state.state != CaptureRelayState::Expired,
            source_acknowledged: state.state == CaptureRelayState::Acknowledged,
            delivery_stopped,
            state: state.state,
            sequence: state.sequence,
        });
    }
    let state = match status.state {
        CaptureBootstrapState::Queued => CaptureRelayState::Queued,
        CaptureBootstrapState::InFlight => CaptureRelayState::InFlight,
        CaptureBootstrapState::RetryWait => CaptureRelayState::RetryWait,
        CaptureBootstrapState::Rejected => CaptureRelayState::Rejected,
        CaptureBootstrapState::Exhausted => CaptureRelayState::Exhausted,
        CaptureBootstrapState::Expired => CaptureRelayState::Expired,
        CaptureBootstrapState::Transferred => bail!("capture_missing_transfer"),
    };
    Ok(CaptureHookRunReport {
        receipt: None,
        provenance: None,
        local_queued: state != CaptureRelayState::Expired,
        source_acknowledged: false,
        delivery_stopped,
        state,
        sequence: status.sequence,
    })
}
#[allow(clippy::too_many_arguments)]
fn confirm_provenance(
    binding: &Binding,
    session: &CaptureSession,
    event_id: &str,
    kind: CaptureEventKind,
    sequence: u64,
    loss: &CaptureLoss,
    receipt: &IntakeReceipt,
    origin: CaptureDeliveryOrigin,
) -> Result<CaptureProvenanceRecord> {
    let t = token(&binding.credential_file)?;
    let request = CaptureProvenanceRequest {
        session_id: session.session_id.clone(),
        event_id: event_id.into(),
        kind,
        origin,
    };
    let r = post(binding, &t, "provenance", &serde_json::to_vec(&request)?)?;
    if matches!(r.status, 401 | 403 | 410) {
        return Err(ProvenanceFailure(CaptureRelayStop::Revoked).into());
    }
    if (300..400).contains(&r.status) {
        return Err(ProvenanceFailure(CaptureRelayStop::TargetMismatch).into());
    }
    ensure!(r.status == 200, "capture_provenance_pending");
    let p: CaptureProvenanceRecord = decode_untrusted_json(&r.body)?;
    let matches = p.schema_version == CAPTURE_PROVENANCE_VERSION
        && p.scope == binding.grant.scope
        && p.instance_id == binding.grant.instance_id
        && p.source == receipt.source
        && p.event_key == receipt.request_id
        && p.source_session.session_id == session.session_id
        && p.source_session.origin_session_id == session.origin_session_id
        && p.source_session.source_namespace == binding.grant.source_namespace
        && p.event_id == event_id
        && p.kind == kind
        && p.sequence == sequence
        && p.loss == *loss
        && p.origins.len() <= 2
        && p.origins.contains(&origin);
    if !matches {
        return Err(ProvenanceFailure(CaptureRelayStop::TargetMismatch).into());
    }
    Ok(p)
}

fn transfer_pending(
    binding: &Binding,
    root: &Path,
    spool: &mut CaptureBootstrap,
    origin: Option<&str>,
) -> Result<bool> {
    let Some(claim) = spool.claim(origin, now())? else {
        return Ok(false);
    };
    let transfer = (|| -> Result<()> {
        let (session, dir) = ensure_session(binding, root, claim.origin_session_id())?;
        let mut relay = open_relay(binding, &session, &dir)?;
        spool.transfer(&claim, &session.session, &mut relay, now())?;
        Ok(())
    })();
    if let Err(e) = transfer {
        let (stop, delay) = if let Some(failure) = e.downcast_ref::<RegistrationFailure>() {
            (failure.stop, failure.retry_after)
        } else if e.to_string() == "capture_session_binding_mismatch" {
            (Some(CaptureRelayStop::TargetMismatch), None)
        } else {
            (None, None)
        };
        spool.retry(&claim, stop, delay, now())?;
    }
    Ok(true)
}

pub fn capture_hook_status(binding_file: &Path, flush: bool) -> Result<CaptureHookLocalStatus> {
    let (binding, root, _lock) = load(binding_file)?;
    let mut report = CaptureHookLocalStatus {
        installed: false,
        host_verified: false,
        sessions: 0,
        pending_events: 0,
        acknowledged_events: 0,
        stopped_sessions: 0,
        bootstrap_pending_events: 0,
        bootstrap_stopped: false,
    };
    let mut bootstrap = open_bootstrap(&binding, &root)?;
    if flush {
        for _ in 0..16 {
            if !transfer_pending(&binding, &root, &mut bootstrap, None)? {
                break;
            }
        }
    }
    let boot = bootstrap.snapshot(now())?;
    report.bootstrap_pending_events = boot.pending_events;
    report.bootstrap_stopped = boot.stopped.is_some();
    report.pending_events = boot.pending_events;
    let mut sent = 0;
    for entry in std::fs::read_dir(root.join("sessions"))?.take(65) {
        let dir = entry?.path();
        private_dir(&dir)?;
        ensure!(report.sessions < 64, "capture_local_session_capacity");
        let record: SessionBinding =
            decode_untrusted_json(&private_read(&dir.join("session.json"), 8192)?)?;
        ensure!(
            dir == session_dir(&root, &record.origin_session_id),
            "capture_session_path_mismatch"
        );
        // Opening an expired queue for sweep must not attempt session registration.
        ensure!(
            record.session.source_namespace == binding.grant.source_namespace
                && record.session.origin_session_id == record.origin_session_id,
            "capture_session_binding_mismatch"
        );
        report.sessions += 1;
        if !dir.join("relay.sqlite").try_exists()? {
            continue;
        }
        let mut relay = open_relay(&binding, &record, &dir)?;
        relay.sweep(now())?;
        if flush && !report.bootstrap_stopped && sent < 16 {
            let t = token(&binding.credential_file)?;
            while sent < 16
                && relay
                    .dispatch_one(&t, |_, bearer, envelope| send(&binding, bearer, envelope))?
                    .is_some()
            {
                sent += 1;
            }
        }
        let s = relay.snapshot()?;
        if let Some(reason) = s.stopped {
            bootstrap.stop(reason, now())?;
            report.bootstrap_stopped = true;
        }
        report.pending_events += s.pending_events;
        report.acknowledged_events += s
            .events
            .iter()
            .filter(|e| e.state == CaptureRelayState::Acknowledged)
            .count();
        report.stopped_sessions += usize::from(s.stopped.is_some());
    }
    Ok(report)
}

/// Returns a pure plan; no Host configuration path is read, discovered or changed.
pub fn render_capture_hook_plan(
    binding_file: &Path,
    executable: &Path,
) -> Result<serde_json::Value> {
    let (binding, _root, _lock) = load(binding_file)?;
    let spec = CaptureHookInstallSpec {
        installation_id: binding.installation_id,
        host: host(&binding.host)?,
        executable: executable.into(),
        binding_file: binding_file.into(),
        target: CaptureRelayTarget {
            instance_id: binding.grant.instance_id,
            target_id: format!("loopback-{}", hex_digest(binding.endpoint.as_bytes())),
            grant_id: binding.grant.grant_id,
            source_namespace: binding.grant.source_namespace,
            session_id: "per-host-session-registration-required".into(),
            expires_at: binding.grant.expires_at,
        },
        observed_host_version: Some(binding.grant.host_version),
    };
    let p = prepare_capture_hook_install(spec, None, None)?;
    Ok(
        serde_json::json!({"schema_version":HOOK_INSTALL_VERSION,"stage":"rendered_uninstalled","real_host_verified":false,"configuration":decode_untrusted_json::<serde_json::Value>(p.rendered())?,"required_live_checks":p.required_live_checks()}),
    )
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn downloaded_bundle_metadata_rejects_other_owners_writers_and_links() {
        assert!(super::bundle_metadata_allowed(501, 1, 0o100644, 501).is_ok());
        assert!(super::bundle_metadata_allowed(502, 1, 0o100600, 501).is_err());
        assert!(super::bundle_metadata_allowed(501, 2, 0o100600, 501).is_err());
        assert!(super::bundle_metadata_allowed(501, 1, 0o100664, 501).is_err());
        assert!(super::bundle_metadata_allowed(501, 1, 0o100602, 501).is_err());
    }
}
