//! Self-contained Unix consumer runtime. No model calls, migration or PID-file signals.
use clap::{Parser, ValueEnum};
use nova_core::compilation::decode_untrusted_json;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;
#[path = "../local_archive.rs"]
mod archive;
#[path = "../local_backup_inspection.rs"]
mod backup_inspection;
#[path = "../local_upgrade.rs"]
mod upgrade;

const BUNDLE: &str = "nova.local-bundle.v0.3";
const LEGACY_BUNDLE: &str = "nova.local-bundle.v0.2";
const LEGACY_PAYLOAD: [&str; 9] = [
    "bin/nova-review",
    "bin/nova-local-init",
    "bin/nova-local",
    "bin/nova-capture-hook",
    "README.md",
    "启动 Nova Memory.command",
    "停止 Nova Memory.command",
    "检查 Nova Memory.command",
    "复制 Owner 凭据.command",
];
const PAYLOAD: [&str; 10] = [
    "bin/nova-review",
    "bin/nova-local-init",
    "bin/nova-local",
    "bin/nova-capture-hook",
    "bin/nova-memory-connect",
    "README.md",
    "启动 Nova Memory.command",
    "停止 Nova Memory.command",
    "检查 Nova Memory.command",
    "复制 Owner 凭据.command",
];
fn bundle_payload(schema: &str) -> Result<&'static [&'static str]> {
    match schema {
        BUNDLE => Ok(&PAYLOAD),
        LEGACY_BUNDLE => Ok(&LEGACY_PAYLOAD),
        _ => reject("invalid_bundle"),
    }
}
const LIMIT: usize = 65536;
const LOG_LIMIT: u64 = 2 * 1024 * 1024;
type Result<T> = std::result::Result<T, Failure>;
#[derive(Debug)]
struct Failure(&'static str);
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self("local_operation_failed")
    }
}
fn reject<T>(code: &'static str) -> Result<T> {
    Err(Failure(code))
}
fn strict<T: DeserializeOwned>(b: &[u8]) -> Result<T> {
    decode_untrusted_json(b).map_err(|_| Failure("invalid_metadata"))
}
fn uid() -> u32 {
    unsafe { libc::getuid() }
}
fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}
fn absolute(p: &Path) -> Result<()> {
    if !p.is_absolute()
        || p.as_os_str()
            .as_encoded_bytes()
            .split(|b| *b == b'/')
            .any(|x| x == b"." || x == b"..")
    {
        return reject("invalid_path");
    }
    let mut q = PathBuf::new();
    for part in p.components() {
        q.push(part);
        match fs::symlink_metadata(&q) {
            Ok(m) if m.file_type().is_symlink() => return reject("symlink_path"),
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn private(m: &fs::Metadata, directory: bool) -> Result<()> {
    if (if directory { !m.is_dir() } else { !m.is_file() })
        || m.uid() != uid()
        || m.mode() & 0o077 != 0
        || (!directory && m.nlink() != 1)
    {
        return reject("private_path_required");
    }
    Ok(())
}
fn owned(p: &Path, directory: bool) -> Result<()> {
    absolute(p)?;
    private(&fs::symlink_metadata(p)?, directory)
}
fn open_private(p: &Path, write: bool, create: bool) -> Result<File> {
    absolute(p)?;
    let f = OpenOptions::new()
        .read(true)
        .write(write)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(p)?;
    private(&f.metadata()?, false)?;
    Ok(f)
}
fn read_bytes(p: &Path, max: usize) -> Result<Vec<u8>> {
    let f = open_private(p, false, false)?;
    let mut b = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut b)?;
    if b.len() > max {
        return reject("metadata_too_large");
    }
    Ok(b)
}
fn read_json<T: DeserializeOwned>(p: &Path) -> Result<T> {
    strict(&read_bytes(p, LIMIT)?)
}
fn write_json<T: Serialize>(p: &Path, v: &T, new: bool) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|_| Failure("invalid_metadata"))?;
    if bytes.len() > LIMIT {
        return reject("metadata_too_large");
    }
    if exists(p) {
        owned(p, false)?;
        if new {
            return reject("metadata_exists");
        }
    }
    let target = if new {
        p.to_path_buf()
    } else {
        p.parent()
            .ok_or(Failure("invalid_path"))?
            .join(format!(".write-{}", token()))
    };
    absolute(&target)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)?;
    let result = (|| {
        f.write_all(&bytes)?;
        f.sync_all()?;
        if !new {
            fs::rename(&target, p)?;
        }
        Ok(())
    })();
    if result.is_err() && !new {
        let _ = fs::remove_file(target);
    }
    result
}
fn lock(dir: &Path, name: &str, blocking: bool) -> Result<File> {
    let f = open_private(&dir.join(name), true, true)?;
    if blocking {
        f.lock()?;
    } else {
        f.try_lock().map_err(|_| Failure("runtime_busy"))?;
    }
    Ok(f)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    platform: String,
    machine: String,
    build_profile: String,
    cargo_version: String,
    bundle_id: String,
    files: BTreeMap<String, String>,
}
fn bundle_info(path: &Path) -> Result<Manifest> {
    owned(path, true)?;
    let m: Manifest = read_json(&path.join("manifest.json"))?;
    let payload = bundle_payload(&m.schema_version)?;
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        std::env::consts::ARCH
    };
    if m.platform != platform
        || m.machine != arch
        || !["debug", "release"].contains(&m.build_profile.as_str())
        || m.cargo_version != env!("CARGO_PKG_VERSION")
    {
        return reject("wrong_platform");
    }
    let expected: BTreeSet<String> = payload.iter().map(|x| x.to_string()).collect();
    if m.files.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return reject("invalid_bundle");
    }
    let identity = serde_json::to_vec(&m.files).map_err(|_| Failure("invalid_bundle"))?;
    if hash(&identity) != m.bundle_id {
        return reject("bundle_changed");
    }
    let mut all = expected;
    all.extend(["manifest.json".into(), "bin".into()]);
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(path)? {
        let e = entry?;
        let name = e
            .file_name()
            .into_string()
            .map_err(|_| Failure("invalid_bundle"))?;
        actual.insert(name.clone());
        if name == "bin" {
            owned(&e.path(), true)?;
            for item in fs::read_dir(e.path())? {
                let f = item?;
                actual.insert(format!(
                    "bin/{}",
                    f.file_name().to_str().ok_or(Failure("invalid_bundle"))?
                ));
            }
        }
    }
    if actual != all {
        return reject("invalid_bundle");
    }
    let mut total_size = 0u64;
    for (name, digest) in &m.files {
        let p = path.join(name);
        let mut file = open_private(&p, false, false)?;
        let expected_size = file.metadata()?.len();
        total_size = total_size
            .checked_add(expected_size)
            .ok_or(Failure("bundle_size_limit"))?;
        if total_size > 512 * 1024 * 1024 {
            return reject("bundle_size_limit");
        }
        if (name.starts_with("bin/") || name.ends_with(".command"))
            && file.metadata()?.mode() & 0o100 == 0
        {
            return reject("invalid_bundle");
        }
        let mut h = Sha256::new();
        let mut buf = [0; 32768];
        let mut actual_size = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            actual_size += n as u64;
            if actual_size > expected_size {
                return reject("bundle_changed");
            }
            h.update(&buf[..n]);
        }
        if actual_size != expected_size || format!("{:x}", h.finalize()) != *digest {
            return reject("bundle_changed");
        }
    }
    Ok(m)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    schema_version: String,
    profile_id: String,
    owner_id: String,
    space_id: String,
    created_by: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    schema_version: String,
    profile_id: String,
    port: u16,
    compiler_enabled: bool,
    compiler_max_calls: u32,
    #[serde(default, skip_serializing_if = "runtime_feature_disabled")]
    continuity_enabled: bool,
    #[serde(default, skip_serializing_if = "runtime_feature_disabled")]
    ranked_retrieval_enabled: bool,
}
fn runtime_feature_disabled(value: &bool) -> bool {
    !*value
}
fn profile_info(dir: &Path) -> Result<(Profile, Runtime)> {
    owned(dir, true)?;
    let p: Profile = read_json(&dir.join("profile.json"))?;
    let c: Runtime = read_json(&dir.join("runtime.json"))?;
    if p.schema_version != "nova.local-profile.v0.1"
        || p.created_by != "nova-local-init"
        || uuid::Uuid::parse_str(&p.profile_id).is_err()
        || p.profile_id.len() != 36
        || !p
            .profile_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
        || [&p.owner_id, &p.space_id].iter().any(|s| {
            s.is_empty()
                || s.len() > 128
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        })
    {
        return reject("invalid_profile");
    }
    if c.schema_version != "nova.local-runtime.v0.1"
        || c.profile_id != p.profile_id
        || c.port < 1024
        || !(1..=10000).contains(&c.compiler_max_calls)
    {
        return reject("invalid_configuration");
    }
    for n in ["legacy.sqlite", "intake.sqlite", "vault.key", "owner.key"] {
        owned(&dir.join(n), false)?;
    }
    if exists(&dir.join("SYNTHETIC_M1_FIXTURE")) {
        return reject("synthetic_profile");
    }
    Ok((p, c))
}
#[derive(Parser)]
#[command(about = "Nova Memory self-contained private local runtime (no Python required)")]
struct Cli {
    #[arg(value_enum)]
    command: Action,
    #[arg(long)]
    bundle: Option<PathBuf>,
    #[arg(long)]
    directory: PathBuf,
    #[arg(long, default_value_t = 3053)]
    port: u16,
    #[arg(long, default_value = "owner:local")]
    owner_id: String,
    #[arg(long, default_value = "personal")]
    space_id: String,
    #[arg(long,value_parser=["on","off"])]
    compiler: Option<String>,
    /// Explicit opt-in to existing session/brief tools; configure only while stopped.
    #[arg(long, value_parser = ["on", "off"])]
    continuity: Option<String>,
    /// Opt in to local ranked search, index, detail and time-neighborhood tools.
    #[arg(long, value_parser = ["on", "off"])]
    ranked_retrieval: Option<String>,
    #[arg(long)]
    max_calls: Option<u32>,
    #[arg(long)]
    no_open: bool,
    /// Selected unpacked private bundle. No automatic download or execution during preview.
    #[arg(long)]
    candidate: Option<PathBuf>,
    /// Exact preview digest required when applying a bundle selection.
    #[arg(long)]
    plan_id: Option<String>,
    /// Explicitly selected uncompressed USTAR package; never downloaded automatically.
    #[arg(long)]
    archive: Option<PathBuf>,
    /// Encrypted snapshot from the workbench, for inspect-backup.
    #[arg(long)]
    backup_file: Option<PathBuf>,
    /// Matching private Vault key file, for inspect-backup.
    #[arg(long)]
    backup_key_file: Option<PathBuf>,
    /// Require an authenticated quarantine marker before starting an inspection profile.
    #[arg(long)]
    inspection_only: bool,
}
#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Action {
    Init,
    Launch,
    Start,
    Stop,
    Status,
    Doctor,
    Open,
    CopyOwner,
    Configure,
    Recover,
    Serve,
    UpgradePreview,
    UpgradeApply,
    UpgradeRollback,
    Unpack,
    InspectBackup,
    CheckInspection,
}
fn clean_command(path: &Path) -> Command {
    let mut c = Command::new(path);
    c.env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null());
    c
}
fn initialize(a: &Cli, bundle: &Path) -> Result<Value> {
    if exists(&a.directory) {
        return reject("profile_exists");
    }
    if a.port < 1024 {
        return reject("invalid_port");
    }
    bundle_info(bundle)?;
    let result = clean_command(&bundle.join("bin/nova-local-init"))
        .arg("--directory")
        .arg(&a.directory)
        .arg("--owner-id")
        .arg(&a.owner_id)
        .arg("--space-id")
        .arg(&a.space_id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !result.success() {
        return reject("initialization_failed");
    }
    let p: Profile = read_json(&a.directory.join("profile.json"))?;
    write_initial_runtime(&a.directory, &p, a.port)?;
    profile_info(&a.directory)?;
    Ok(
        json!({"status":"initialized","profile_id":p.profile_id,"directory":a.directory,"model_selection":"unconfigured","credentials":"private_files_not_printed"}),
    )
}
fn write_initial_runtime(directory: &Path, profile: &Profile, port: u16) -> Result<()> {
    write_json(
        &directory.join("runtime.json"),
        &Runtime {
            schema_version: "nova.local-runtime.v0.1".into(),
            profile_id: profile.profile_id.clone(),
            port,
            compiler_enabled: false,
            compiler_max_calls: 100,
            continuity_enabled: false,
            ranked_retrieval_enabled: false,
        },
        true,
    )
}
fn socket_path(p: &Profile) -> Result<PathBuf> {
    let root = if cfg!(target_os = "macos") {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let base = Path::new(root).join(format!("nova-memory-local-{}", uid()));
    match fs::DirBuilder::new().mode(0o700).create(&base) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    };
    owned(&base, true)?;
    Ok(base.join(format!("{}.sock", p.profile_id)))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: String,
    directory: PathBuf,
    profile_id: String,
    instance_id: String,
    nonce: String,
    socket: PathBuf,
    bundle_id: String,
    supervisor_pid: u32,
}
fn state_info(dir: &Path, p: &Profile) -> Result<Option<State>> {
    let path = dir.join("runtime-state.json");
    if !exists(&path) {
        return Ok(None);
    }
    let s: State = read_json(&path)?;
    if s.schema_version != "nova.local-state.v0.1"
        || s.directory != dir
        || s.profile_id != p.profile_id
        || s.socket != socket_path(p)?
        || [&s.instance_id, &s.nonce, &s.bundle_id].iter().any(|x| {
            !(32..=64).contains(&x.len())
                || !x
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return reject("invalid_runtime_state");
    }
    Ok(Some(s))
}
fn socket_owned(path: &Path) -> Result<fs::Metadata> {
    absolute(path)?;
    let m = fs::symlink_metadata(path)?;
    if !m.file_type().is_socket() || m.uid() != uid() || m.mode() & 0o077 != 0 {
        return reject("unsafe_control_socket");
    }
    Ok(m)
}
fn receive<T: DeserializeOwned>(s: &mut UnixStream, max: usize, timeout: Duration) -> Result<T> {
    s.set_nonblocking(true)?;
    let b = read_deadline(s, max, timeout)?;
    strict(&b)
}
// A socket's per-read timeout alone permits an indefinitely trickling peer. Keep
// a wall-clock deadline across the whole bounded message. Callers set nonblocking
// once: macOS may reject SO_RCVTIMEO changes after a peer has already closed, even
// though its complete response remains buffered and can be read successfully.
fn read_deadline<R: Read>(stream: &mut R, max: usize, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut data = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Failure("response_timeout"))?;
        let mut chunk = [0u8; 4096];
        let capacity = chunk.len().min(max + 1 - data.len());
        let count = match stream.read(&mut chunk[..capacity]) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(remaining.min(Duration::from_millis(5)));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return reject("response_read_failed"),
        };
        if count == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&chunk[..count]);
        if data.len() > max {
            return reject("message_too_large");
        }
    }
}
fn stopped(p: &Profile) -> Value {
    json!({"status":"stopped","profile_id":p.profile_id,"ready":false})
}
fn unresponsive(p: &Profile) -> Value {
    json!({"status":"unresponsive","profile_id":p.profile_id,"ready":false,"message":"Private supervisor could not be verified. No process was signaled."})
}
fn control(dir: &Path, p: &Profile, command: &str) -> Result<Option<Value>> {
    let Some(s) = state_info(dir, p)? else {
        return Ok(None);
    };
    if exists(&s.socket) {
        socket_owned(&s.socket)?;
    }
    let call = (|| -> Result<Value> {
        let mut client = UnixStream::connect(&s.socket)?;
        client.set_read_timeout(Some(Duration::from_secs(4)))?;
        client.set_write_timeout(Some(Duration::from_secs(4)))?;
        let req = json!({"directory":dir,"profile_id":p.profile_id,"instance_id":s.instance_id,"nonce":s.nonce,"command":command});
        client.write_all(serde_json::to_string(&req).unwrap().as_bytes())?;
        client.shutdown(Shutdown::Write)?;
        receive(&mut client, LIMIT, Duration::from_secs(4))
    })();
    match call {
        Ok(v) => {
            if !v.is_object()
                || v["instance_id"] != s.instance_id
                || v["profile_id"] != p.profile_id
                || v["bundle_id"] != s.bundle_id
            {
                return reject("wrong_runtime_instance");
            }
            Ok(Some(v))
        }
        Err(_) => Ok(Some(unresponsive(p))),
    }
}
fn status(dir: &Path, p: &Profile) -> Result<Value> {
    if let Some(v) = control(dir, p, "status")? {
        return Ok(v);
    }
    let path = dir.join("service.lock");
    if exists(&path) {
        let f = open_private(&path, true, false)?;
        if f.try_lock().is_err() {
            return Ok(unresponsive(p));
        }
    }
    Ok(stopped(p))
}
fn free_port(c: &Runtime) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", c.port)).map_err(|_| Failure("port_in_use"))
}
fn fetch(c: &Runtime, path: &str, token: Option<&str>) -> Result<Value> {
    let addr: SocketAddr = ([127, 0, 0, 1], c.port).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let base = format!("http://127.0.0.1:{}", c.port);
    let auth = Zeroizing::new(
        token
            .map(|t| {
                format!(
                    "Authorization: Bearer {t}\r\nOrigin: {base}\r\nX-Nova-Review: owner-review\r\n"
                )
            })
            .unwrap_or_default(),
    );
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{}\r\n",
        c.port,
        auth.as_str()
    )?;
    stream.set_nonblocking(true)?;
    let data = read_deadline(&mut stream, LIMIT + 16384, Duration::from_secs(1))?;
    let index = data
        .windows(4)
        .position(|x| x == b"\r\n\r\n")
        .ok_or(Failure("invalid_health_response"))?;
    if index > 16384 || data.len() - index - 4 > LIMIT {
        return reject("invalid_health_response");
    }
    let header =
        std::str::from_utf8(&data[..index]).map_err(|_| Failure("invalid_health_response"))?;
    if !header.starts_with("HTTP/1.1 200 ") && !header.starts_with("HTTP/1.0 200 ") {
        return reject("readiness_mismatch");
    }
    // Axum's bounded JSON endpoints send Content-Length. Redirects/chunked data are not followed.
    let length = header
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        })
        .ok_or(Failure("invalid_health_response"))?;
    if length != data.len() - index - 4 {
        return reject("invalid_health_response");
    }
    strict(&data[index + 4..])
}
fn owner_token(dir: &Path) -> Result<Zeroizing<String>> {
    let b = Zeroizing::new(read_bytes(&dir.join("owner.key"), 4096)?);
    let s = std::str::from_utf8(&b)
        .map_err(|_| Failure("invalid_profile"))?
        .trim();
    if s.len() != 64
        || !s
            .bytes()
            .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
    {
        return reject("invalid_profile");
    }
    Ok(Zeroizing::new(s.into()))
}
fn probe(dir: &Path, p: &Profile, c: &Runtime, instance: &str) -> Result<Value> {
    let session = fetch(c, "/console/session", None)?;
    let capability_matches = |name: &str, expected: bool| match session["capabilities"].get(name) {
        Some(Value::Bool(enabled)) => *enabled == expected,
        None => !expected, // Older runtimes may omit an off capability.
        _ => false,
    };
    if session["mode"] != "local"
        || session["demo"] != false
        || session["requires_owner_token"] != true
        || session["owner"] != p.owner_id
        || session["space"] != p.space_id
        || session["capabilities"]["automatic_intake"] != true
        || session["capabilities"]["native_workspace"] != true
        || session["local_runtime_instance"] != instance
        || !capability_matches("continuity", c.continuity_enabled)
        || !capability_matches("ranked_retrieval", c.ranked_retrieval_enabled)
    {
        return reject("readiness_mismatch");
    }
    let token = owner_token(dir)?;
    let config = fetch(c, "/console/api/owner/intake/configuration", Some(&token))?;
    if config["enabled"] != true
        || config["demo"] != false
        || config["compiler_available"] != c.compiler_enabled
    {
        return reject("readiness_mismatch");
    }
    Ok(
        json!({"status":"running","profile_id":p.profile_id,"port":c.port,"url":format!("http://127.0.0.1:{}/app/",c.port),"ready":true,"model_selection":config["configuration"]["selection"]["location"].as_str().unwrap_or("unknown"),"compiler_available":c.compiler_enabled,"continuity_available":c.continuity_enabled,"ranked_retrieval_available":c.ranked_retrieval_enabled,"backup_quarantined":config["backup_quarantined"] == true,"real_host_verified":false}),
    )
}
fn start(a: &Cli, bundle: &Path, m: &Manifest) -> Result<Value> {
    let _lock = lock(&a.directory, "operation.lock", true)?;
    if upgrade::selected(&a.directory, bundle)? != bundle {
        return reject("upgrade_selection_changed");
    }
    let (p, c) = profile_info(&a.directory)?;
    if a.inspection_only {
        backup_inspection::require_profile(&a.directory, &p, &c)?;
    }
    let current = status(&a.directory, &p)?;
    if current["status"] == "running" {
        if current["bundle_id"] != m.bundle_id {
            return reject("different_bundle_running");
        }
        return Ok(current);
    }
    if current["status"] != "stopped" {
        return reject("runtime_unverified");
    }
    drop(free_port(&c)?);
    let log_path = a.directory.join("runtime.log");
    absolute(&log_path)?;
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&log_path)?;
    private(&log.metadata()?, false)?;
    if log.metadata()?.len() > LOG_LIMIT {
        log.set_len(0)?;
    }
    let mut cmd = clean_command(&bundle.join("bin/nova-local"));
    cmd.arg("serve")
        .arg("--bundle")
        .arg(bundle)
        .arg("--directory")
        .arg(&a.directory)
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = StartupGuard {
        child: cmd.spawn()?,
        armed: true,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if child.child.try_wait()?.is_some() {
            return reject("startup_failed");
        }
        let r = status(&a.directory, &p)?;
        if r["status"] == "running" {
            child.armed = false;
            return Ok(r);
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = control(&a.directory, &p, "stop");
    reject("startup_timeout")
}
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
// This guard is disarmed only after an authenticated ready handshake. Failure paths
// retain the exact Child handle; metadata PIDs never participate in cleanup.
struct StartupGuard {
    child: Child,
    armed: bool,
}
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if self.armed {
            // The supervisor may first finish a bounded probe (at most 2s) and
            // then its child's 8s graceful shutdown. Give that cleanup time.
            terminate_owned(&mut self.child, Duration::from_secs(12));
        }
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        terminate_owned(&mut self.0, Duration::from_secs(8));
    }
}
fn terminate_owned(child: &mut Child, grace: Duration) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    // An unreaped child cannot have its PID reassigned, even if it exits here.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let end = Instant::now() + grace;
    while Instant::now() < end {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRequest {
    directory: PathBuf,
    profile_id: String,
    instance_id: String,
    nonce: String,
    command: String,
}
fn serve(a: &Cli, bundle: &Path, p: &Profile, c: &Runtime, m: &Manifest) -> Result<()> {
    let _lock = lock(&a.directory, "service.lock", false)?;
    if state_info(&a.directory, p)?.is_some() {
        return reject("runtime_state_exists");
    }
    let socket = socket_path(p)?;
    if exists(&socket) {
        return reject("control_endpoint_exists");
    }
    STOP.store(false, Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t);
        libc::signal(
            libc::SIGTERM,
            stop_signal as *const () as libc::sighandler_t,
        );
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let socket_identity = socket_owned(&socket)?;
    listener.set_nonblocking(true)?;
    let state = State {
        schema_version: "nova.local-state.v0.1".into(),
        directory: a.directory.clone(),
        profile_id: p.profile_id.clone(),
        instance_id: token(),
        nonce: format!("{}{}", token(), token()),
        socket: socket.clone(),
        bundle_id: m.bundle_id.clone(),
        supervisor_pid: std::process::id(),
    };
    let state_path = a.directory.join("runtime-state.json");
    let result = (|| -> Result<()> {
        write_json(&state_path, &state, true)?;
        let mut cmd = clean_command(&bundle.join("bin/nova-review"));
        cmd.arg("--vault")
            .arg(a.directory.join("legacy.sqlite"))
            .arg("--key-file")
            .arg(a.directory.join("vault.key"))
            .arg("--owner-token-file")
            .arg(a.directory.join("owner.key"))
            .arg("--intake-store")
            .arg(a.directory.join("intake.sqlite"))
            .arg("--owner-id")
            .arg(&p.owner_id)
            .arg("--space-id")
            .arg(&p.space_id)
            .arg("--bind")
            .arg(format!("127.0.0.1:{}", c.port))
            .arg("--workspace-v03")
            .arg("--enable-capture-ingress")
            .arg("--local-runtime-instance")
            .arg(&state.instance_id);
        if c.continuity_enabled {
            cmd.arg("--enable-continuity");
        }
        if c.ranked_retrieval_enabled {
            cmd.arg("--enable-ranked-retrieval");
        }
        if c.compiler_enabled {
            cmd.arg("--enable-compiler")
                .arg("--compiler-max-calls")
                .arg(c.compiler_max_calls.to_string());
        }
        let mut child = OwnedChild(cmd.spawn()?);
        while !STOP.load(Ordering::Relaxed) && child.0.try_wait()?.is_none() {
            match listener.accept() {
                Ok((mut connection, _)) => {
                    let response = (|| -> Result<Value> {
                        let r: ControlRequest =
                            receive(&mut connection, 4096, Duration::from_secs(2))?;
                        if r.directory != a.directory
                            || r.profile_id != p.profile_id
                            || r.instance_id != state.instance_id
                            || r.nonce != state.nonce
                        {
                            return reject("wrong_runtime_instance");
                        }
                        let mut answer = match r.command.as_str() {
                            "stop" => {
                                STOP.store(true, Ordering::Relaxed);
                                json!({"status":"stopping"})
                            }
                            "status" => probe(&a.directory, p, c, &state.instance_id)
                                .unwrap_or_else(|_| json!({"status":"starting","ready":false})),
                            "doctor" => {
                                // Verify the exact child instance before using its Owner endpoint.
                                let mut observed = probe(&a.directory, p, c, &state.instance_id)?;
                                let credential = owner_token(&a.directory)?;
                                observed["diagnostics"] = match fetch(
                                    c,
                                    "/console/api/owner/intake/diagnostics",
                                    Some(&credential),
                                ) {
                                    Ok(v) if v["schema_version"] == "nova.local-doctor.v0.1" => v,
                                    _ => json!({"state":"unavailable","repairs_performed":false}),
                                };
                                observed
                            }
                            _ => return reject("invalid_control_message"),
                        };
                        answer["profile_id"] = json!(p.profile_id);
                        answer["instance_id"] = json!(state.instance_id);
                        answer["bundle_id"] = json!(m.bundle_id);
                        Ok(answer)
                    })();
                    if let Ok(v) = response {
                        let _ = connection.write_all(serde_json::to_string(&v).unwrap().as_bytes());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100))
                }
                Err(e) => return Err(e.into()),
            }
            let f = open_private(&a.directory.join("runtime.log"), true, false)?;
            if f.metadata()?.len() > LOG_LIMIT {
                f.set_len(0)?;
            }
        }
        Ok(())
    })();
    drop(listener);
    if let Ok(now) = socket_owned(&socket)
        && now.ino() == socket_identity.ino()
        && now.dev() == socket_identity.dev()
    {
        let _ = fs::remove_file(&socket);
    }
    if let Ok(Some(current)) = state_info(&a.directory, p)
        && current.instance_id == state.instance_id
    {
        let _ = fs::remove_file(&state_path);
    }
    result
}
fn stop(dir: &Path, p: &Profile) -> Result<Value> {
    let _lock = lock(dir, "operation.lock", true)?;
    let Some(r) = control(dir, p, "stop")? else {
        return status(dir, p);
    };
    if r["status"] != "stopping" {
        return reject("runtime_unverified");
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if !exists(&dir.join("runtime-state.json")) {
            let mut v = stopped(p);
            v["assets"] = json!("preserved");
            return Ok(v);
        }
        thread::sleep(Duration::from_millis(100));
    }
    reject("stop_timeout")
}
fn recover(dir: &Path, p: &Profile, c: &Runtime) -> Result<Value> {
    let _op = lock(dir, "operation.lock", true)?;
    let _svc = lock(dir, "service.lock", false)?;
    let Some(s) = state_info(dir, p)? else {
        return Ok(stopped(p));
    };
    let _port = free_port(c)?;
    if exists(&s.socket) {
        socket_owned(&s.socket)?;
        if UnixStream::connect(&s.socket).is_ok() {
            return reject("runtime_busy");
        }
        fs::remove_file(&s.socket)?;
    }
    fs::remove_file(dir.join("runtime-state.json"))?;
    let mut v = stopped(p);
    v["recovered"] = json!("abandoned_metadata_only");
    Ok(v)
}
fn main_result(mut a: Cli) -> Result<Option<Value>> {
    unsafe {
        libc::umask(0o077);
    }
    if a.continuity.is_some() && a.command != Action::Configure {
        return reject("continuity_requires_configure_action");
    }
    if a.ranked_retrieval.is_some() && a.command != Action::Configure {
        return reject("ranked_retrieval_requires_configure_action");
    }
    if a.inspection_only && a.command != Action::Start {
        return reject("inspection_requires_start_action");
    }
    let mut bundle = match a.bundle.take() {
        Some(p) => p,
        None => std::env::current_exe()?
            .parent()
            .and_then(|p| p.parent())
            .ok_or(Failure("invalid_path"))?
            .to_path_buf(),
    };
    absolute(&a.directory)?;
    absolute(&bundle)?;
    if a.command != Action::InspectBackup && (a.backup_file.is_some() || a.backup_key_file.is_some()) {
        return reject("backup_arguments_require_inspect_backup");
    }
    if a.command == Action::Unpack {
        if a.candidate.is_some() || a.plan_id.is_some() {
            return reject("unpack_arguments_invalid");
        }
        return archive::unpack(
            a.archive.as_deref().ok_or(Failure("archive_required"))?,
            &a.directory,
        )
        .map(Some);
    }
    if a.archive.is_some() {
        return reject("archive_requires_unpack_action");
    }
    if matches!(
        a.command,
        Action::UpgradePreview | Action::UpgradeApply | Action::UpgradeRollback
    ) {
        return upgrade::execute(&a, &bundle).map(Some);
    }
    if a.candidate.is_some() || a.plan_id.is_some() {
        return reject("upgrade_arguments_require_upgrade_action");
    }
    if a.command == Action::InspectBackup {
        bundle_info(&bundle)?;
        return backup_inspection::prepare(&a).map(Some);
    }
    if a.command == Action::Init {
        return initialize(&a, &bundle).map(Some);
    }
    if a.command == Action::Launch && !exists(&a.directory) {
        let parent = a.directory.parent().ok_or(Failure("invalid_path"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        initialize(&a, &bundle)?;
    }
    let (p, mut c) = profile_info(&a.directory)?;
    match a.command {
        Action::CheckInspection => {
            bundle_info(&bundle)?;
            backup_inspection::require_profile(&a.directory, &p, &c)?;
            return Ok(Some(json!({
                "status":"backup_inspection_checked","port":c.port,"profile_id":p.profile_id,
                "backup_quarantined":true,"runtime_started":false
            })));
        }
        Action::Stop => return stop(&a.directory, &p).map(Some),
        Action::Recover => return recover(&a.directory, &p, &c).map(Some),
        Action::Status => return status(&a.directory, &p).map(Some),
        Action::Configure => {
            if a.compiler.is_none()
                && a.max_calls.is_none()
                && a.continuity.is_none()
                && a.ranked_retrieval.is_none()
            {
                return reject("configuration_required");
            }
            if a.max_calls.is_some_and(|n| !(1..=10000).contains(&n)) {
                return reject("invalid_budget");
            }
            let _lock = lock(&a.directory, "operation.lock", true)?;
            let (_, fresh) = profile_info(&a.directory)?;
            c = fresh;
            if status(&a.directory, &p)?["status"] != "stopped" {
                return reject("stop_before_configure");
            }
            if let Some(s) = &a.compiler {
                c.compiler_enabled = s == "on";
            }
            if let Some(n) = a.max_calls {
                c.compiler_max_calls = n;
            }
            if let Some(value) = &a.continuity {
                c.continuity_enabled = value == "on";
            }
            if let Some(value) = &a.ranked_retrieval {
                c.ranked_retrieval_enabled = value == "on";
            }
            write_json(&a.directory.join("runtime.json"), &c, false)?;
            return Ok(Some(
                json!({"status":"configured","compiler_enabled":c.compiler_enabled,"compiler_max_calls":c.compiler_max_calls,"continuity_enabled":c.continuity_enabled,"ranked_retrieval_enabled":c.ranked_retrieval_enabled}),
            ));
        }
        _ => (),
    }
    bundle = upgrade::selected(&a.directory, &bundle)?;
    let m = bundle_info(&bundle)?;
    match a.command {
        Action::Serve => {
            serve(&a, &bundle, &p, &c, &m)?;
            return Ok(None);
        }
        Action::Doctor => {
            let mut runtime = match control(&a.directory, &p, "doctor")? {
                Some(value) => value,
                None => status(&a.directory, &p)?,
            };
            let diagnostics = runtime
                .as_object_mut()
                .and_then(|v| v.remove("diagnostics"))
                .unwrap_or_else(
                    || json!({"state":"not_running_or_unverified","repairs_performed":false}),
                );
            return Ok(Some(
                json!({"status":"checked","bundle_id":m.bundle_id,"build_profile":m.build_profile,"runtime_implementation":"rust_self_contained","data_directory":a.directory,"credentials":"private_files_not_printed","runtime":runtime,"diagnostics":diagnostics,"signature_verified":false,"real_host_verified":false}),
            ));
        }
        Action::CopyOwner => {
            if !cfg!(target_os = "macos") {
                return reject("clipboard_unavailable");
            }
            let secret = owner_token(&a.directory)?;
            let mut child = clean_command(Path::new("/usr/bin/pbcopy"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            child
                .stdin
                .take()
                .ok_or(Failure("clipboard_unavailable"))?
                .write_all(secret.as_bytes())?;
            if !child.wait()?.success() {
                return reject("clipboard_unavailable");
            }
            return Ok(Some(
                json!({"status":"copied","destination":"local_clipboard","credential":"not_printed"}),
            ));
        }
        _ => (),
    }
    let r = if matches!(a.command, Action::Start | Action::Launch) {
        start(&a, &bundle, &m)?
    } else {
        status(&a.directory, &p)?
    };
    if matches!(a.command, Action::Open | Action::Launch) && !a.no_open {
        if r["status"] != "running" {
            return reject("runtime_not_ready");
        }
        if !cfg!(target_os = "macos") {
            return reject("browser_unavailable");
        }
        if !clean_command(Path::new("/usr/bin/open"))
            .arg(format!("http://127.0.0.1:{}/app/", c.port))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
        {
            return reject("browser_unavailable");
        }
    }
    Ok(Some(r))
}
fn main() {
    let a = Cli::parse();
    match main_result(a) {
        Ok(Some(v)) => println!("{v}"),
        Ok(None) => (),
        Err(e) => {
            eprintln!(
                "{}",
                json!({"status":"error","code":e.0,"message":"Check the selected private profile and runtime log. No credentials are printed; no PID from metadata is signaled."})
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuity_is_explicit_and_legacy_runtime_configuration_remains_off() {
        let mut value = json!({"schema_version":"nova.local-runtime.v0.1","profile_id":"synthetic","port":3053,"compiler_enabled":false,"compiler_max_calls":100});
        let old: Runtime = strict(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(!old.continuity_enabled);
        assert!(!old.ranked_retrieval_enabled);
        assert_eq!(serde_json::to_value(old).unwrap(), value);
        value["continuity_enabled"] = json!(true);
        let enabled: Runtime = strict(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(enabled.continuity_enabled);
        assert_eq!(serde_json::to_value(enabled).unwrap(), value);
        for invalid in [Value::Null, json!("on"), json!(1), json!({})] {
            value["continuity_enabled"] = invalid;
            assert!(strict::<Runtime>(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        value["continuity_enabled"] = json!(true);
        value["ranked_retrieval_enabled"] = json!(true);
        let enabled: Runtime = strict(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(enabled.ranked_retrieval_enabled);
        assert_eq!(serde_json::to_value(enabled).unwrap(), value);
        for invalid in [Value::Null, json!("on"), json!(1), json!({})] {
            value["ranked_retrieval_enabled"] = invalid;
            assert!(strict::<Runtime>(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        (dir, path)
    }
    #[test]
    fn hardlinked_private_files_are_rejected_before_truncation_or_read() {
        let (_d, path) = fixture();
        let original = path.join("original");
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&original)
            .unwrap();
        f.write_all(b"unchanged-other-file").unwrap();
        drop(f);
        let link = path.join("runtime.log");
        fs::hard_link(&original, &link).unwrap();
        assert_eq!(
            open_private(&link, true, false).unwrap_err().0,
            "private_path_required"
        );
        assert_eq!(
            read_bytes(&link, 1024).unwrap_err().0,
            "private_path_required"
        );
        assert_eq!(fs::read(&original).unwrap(), b"unchanged-other-file");
    }
    #[test]
    fn bounded_control_reader_accepts_complete_json_followed_by_eof() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            writer
                .write_all(br#"{"status":"running","ready":true}"#)
                .unwrap();
        });
        peer.join().unwrap(); // Force the closed-peer/buffered-response boundary.
        let value: Value = receive(&mut reader, LIMIT, Duration::from_secs(4)).unwrap();
        assert_eq!(value["status"], "running");
    }
    #[test]
    fn trickling_control_peer_cannot_extend_message_deadline() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let peer = thread::spawn(move || {
            for _ in 0..30 {
                if writer.write_all(b"x").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        assert!(read_deadline(&mut reader, 4096, Duration::from_millis(80)).is_err());
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(reader);
        peer.join().unwrap();
    }
    #[test]
    fn startup_guard_stops_only_its_exact_owned_child_on_failure() {
        let mut unrelated = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        {
            let _guard = StartupGuard { child, armed: true };
        }
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }
    #[test]
    fn invalid_public_session_never_receives_owner_authorization() {
        for bad in ["owner", "mode", "instance", "continuity", "ranked_retrieval"] {
            let (_d, path) = fixture();
            let mut key = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path.join("owner.key"))
                .unwrap();
            key.write_all("a".repeat(64).as_bytes()).unwrap();
            drop(key);
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let finished = std::sync::Arc::new(AtomicBool::new(false));
            let stop = finished.clone();
            let mut session = json!({"mode":"local","demo":false,"requires_owner_token":true,"owner":"owner:fixture","space":"synthetic","local_runtime_instance":"b".repeat(32),"capabilities":{"automatic_intake":true,"native_workspace":true}});
            match bad {
                "owner" => session["owner"] = json!("owner:foreign"),
                "mode" => session["mode"] = json!("hosted"),
                "instance" => session["local_runtime_instance"] = json!("c".repeat(32)),
                _ => session["capabilities"][bad] = json!(true),
            };
            let handler = thread::spawn(move || {
                let mut count = 0;
                let mut credential_seen = false;
                let deadline = Instant::now() + Duration::from_secs(4);
                while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut client, _)) => {
                            client
                                .set_read_timeout(Some(Duration::from_millis(500)))
                                .unwrap();
                            let mut raw = Vec::new();
                            let mut byte = [0];
                            while raw.len() < 16384 && !raw.ends_with(b"\r\n\r\n") {
                                if client.read(&mut byte).unwrap_or(0) == 0 {
                                    break;
                                }
                                raw.push(byte[0]);
                            }
                            count += 1;
                            credential_seen |=
                                String::from_utf8_lossy(&raw).contains("Authorization:");
                            let body = serde_json::to_vec(&session).unwrap();
                            write!(client,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                            client.write_all(&body).unwrap();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("fixture listener failed: {e}"),
                    }
                }
                (count, credential_seen)
            });
            let p = Profile {
                schema_version: "nova.local-profile.v0.1".into(),
                profile_id: uuid::Uuid::new_v4().to_string(),
                owner_id: "owner:fixture".into(),
                space_id: "synthetic".into(),
                created_by: "nova-local-init".into(),
            };
            let c = Runtime {
                schema_version: "nova.local-runtime.v0.1".into(),
                profile_id: p.profile_id.clone(),
                port,
                compiler_enabled: false,
                compiler_max_calls: 1,
                continuity_enabled: false,
                ranked_retrieval_enabled: false,
            };
            assert_eq!(
                probe(&path, &p, &c, &"b".repeat(32)).unwrap_err().0,
                "readiness_mismatch"
            );
            finished.store(true, Ordering::SeqCst);
            let (count, credential_seen) = handler.join().unwrap();
            assert_eq!(count, 1);
            assert!(!credential_seen);
        }
    }
}
