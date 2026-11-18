//! Explicit, workspace-owned Secure MCP Tunnel child. No credential discovery or global config.
use crate::{gateway_runtime::GatewayManager, local_mcp_bridge::read_local_connection};
use nova_core::NovaError;
use nova_mcp::automatic::IntakeApplicationService;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};
use zeroize::{Zeroize, Zeroizing};

const MAX_LIVE: usize = 4;
const MAX_HISTORY: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TunnelRequest {
    pub tunnel_id: String,
    pub runtime_key: String,
    pub connect_to_openai: bool,
}
impl Drop for TunnelRequest {
    fn drop(&mut self) {
        self.runtime_key.zeroize();
    }
}
impl TunnelRequest {
    pub fn validate(&self) -> Result<(), NovaError> {
        let valid_id = self
            .tunnel_id
            .strip_prefix("tunnel_")
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        if !self.connect_to_openai
            || !valid_id
            || !self.runtime_key.starts_with("sk-")
            || !(20..=512).contains(&self.runtime_key.len())
            || !self
                .runtime_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(NovaError::Validation(
                "invalid tunnel setup or explicit connection intent missing".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct TunnelStatus {
    pub connection_id: String,
    pub tunnel_id: String,
    pub phase: &'static str,
    pub last_error: Option<&'static str>,
    pub transport_ready: bool,
    pub real_host_verified: bool,
    pub public_endpoint: Option<String>,
    pub credential_persisted: bool,
}
struct Runtime {
    status: Arc<Mutex<TunnelStatus>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    #[cfg(test)]
    local_proxy: PathBuf,
}
impl Runtime {
    fn status(&self) -> TunnelStatus {
        let mut s = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if self.task.is_finished() && matches!(s.phase, "starting" | "ready") {
            s.phase = "failed";
            s.transport_ready = false;
        }
        s
    }
    fn stop(&self) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.task.is_finished() {
            s.phase = "stopping";
            s.transport_ready = false;
        }
        let _ = self.stop.send(true);
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
pub(crate) struct TunnelManager {
    runtimes: AsyncMutex<BTreeMap<String, Runtime>>,
}

/// Only known install locations; never PATH, request-supplied commands, profiles or credential files.
pub(crate) fn installed_client() -> Option<PathBuf> {
    installed_binary("tunnel-client")
}
pub(crate) fn installed_binary(name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // The packaged supervisor deliberately clears inherited environment variables.
        // Resolve only the effective user's OS home, without restoring their environment
        // or inspecting existing tool profiles/credentials. System locations must still
        // be checked if the account has no home entry.
        let candidates = install_candidates(name, account_home());
        for candidate in candidates {
            let Ok(path) = candidate.canonicalize() else {
                continue;
            };
            let Ok(m) = std::fs::metadata(&path) else {
                continue;
            };
            if m.is_file()
                && (m.uid() == unsafe { libc::geteuid() } || m.uid() == 0)
                && m.mode() & 0o022 == 0
                && m.mode() & 0o111 != 0
            {
                return Some(path);
            }
        }
    }
    None
}
#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0u8; 65_536];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // getpwuid_r writes record and pointers into this live, bounded buffer.
    let code = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return None;
    }
    let directory = unsafe { (*result).pw_dir };
    if directory.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(directory) }.to_bytes();
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
    path.is_absolute().then_some(path)
}
#[cfg(unix)]
fn install_candidates(name: &str, home: Option<PathBuf>) -> Vec<PathBuf> {
    home.into_iter()
        .map(|p| p.join(".local/bin").join(name))
        .chain([
            PathBuf::from("/opt/homebrew/bin").join(name),
            PathBuf::from("/usr/local/bin").join(name),
        ])
        .collect()
}
fn unavailable() -> NovaError {
    NovaError::PolicyDenied("tunnel connection unavailable".into())
}

fn profile(tunnel_id: &str, gateway: &str, health_file: &Path) -> serde_json::Value {
    // JSON is YAML. These are references only: neither credential reaches this file or argv.
    json!({"config_version":1,
        "control_plane":{"base_url":"https://api.openai.com","tunnel_id":tunnel_id,"api_key":"env:NOVA_TUNNEL_RUNTIME_KEY"},
        "mcp":{"server_urls":[{"channel":"main","url":gateway}],
            "extra_headers":{"Authorization":"env:NOVA_TUNNEL_MCP_AUTH"},
            "discovery_extra_headers":{"Authorization":"env:NOVA_TUNNEL_MCP_AUTH"}},
        "health":{"listen_addr":"127.0.0.1:0","url_file":health_file},
        "admin_ui":{"open_browser":false},"log":{"level":"error","format":"json"}})
}
fn private_write(path: &Path, content: &[u8]) -> Result<(), NovaError> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)?.write_all(content)?;
    Ok(())
}
pub(crate) fn current(
    service: &IntakeApplicationService,
    path: &Path,
    id: &str,
    upstream: &str,
) -> Result<crate::local_mcp_bridge::LocalConnection, NovaError> {
    let config = read_local_connection(path).map_err(|_| unavailable())?;
    if config.connection_id != id
        || config.endpoint != upstream
        || service.authenticate(&config.token)?.connection_id() != id
    {
        return Err(unavailable());
    }
    Ok(config)
}
fn health_endpoint(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 256 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(257).read_to_end(&mut bytes).ok()?;
    let raw = std::str::from_utf8(&bytes).ok()?.trim();
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|p| p == 0)
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(format!("{raw}/readyz"))
}

impl TunnelManager {
    pub async fn statuses(&self) -> Vec<TunnelStatus> {
        self.runtimes
            .lock()
            .await
            .values()
            .map(Runtime::status)
            .collect()
    }
    pub async fn stop(&self, id: &str) {
        if let Some(r) = self.runtimes.lock().await.get(id) {
            r.stop();
        }
    }
    pub async fn stop_all(&self) {
        for runtime in self.runtimes.lock().await.values() {
            runtime.stop();
        }
        for _ in 0..60 {
            if self
                .runtimes
                .lock()
                .await
                .values()
                .all(|r| r.task.is_finished())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    pub async fn start(
        &self,
        request: TunnelRequest,
        binding: TunnelBinding,
    ) -> Result<TunnelStatus, NovaError> {
        self.launch(request, binding, false).await
    }
    async fn launch(
        &self,
        request: TunnelRequest,
        binding: TunnelBinding,
        local_test: bool,
    ) -> Result<TunnelStatus, NovaError> {
        request.validate()?;
        let config = current(
            &binding.service,
            &binding.path,
            &binding.id,
            &binding.upstream,
        )?;
        if !binding
            .gateways
            .is_running(&binding.id, &binding.gateway)
            .await
        {
            return Err(unavailable());
        }
        let mut runtimes = self.runtimes.lock().await;
        if let Some(r) = runtimes.get(&binding.id)
            && !r.task.is_finished()
        {
            let status = r.status();
            return if status.tunnel_id == request.tunnel_id
                && matches!(status.phase, "starting" | "ready")
            {
                Ok(status)
            } else {
                Err(NovaError::InvalidState(
                    "stop current tunnel before restarting".into(),
                ))
            };
        }
        if runtimes.values().filter(|r| !r.task.is_finished()).count() >= MAX_LIVE {
            return Err(NovaError::InvalidState("tunnel runtime limit".into()));
        }
        if runtimes.len() >= MAX_HISTORY {
            runtimes.retain(|_, r| !r.task.is_finished());
        }
        let binary = installed_client()
            .ok_or_else(|| NovaError::InvalidState("tunnel client not installed".into()))?;
        let directory = tempfile::Builder::new().prefix("nova-tunnel-").tempdir()?;
        let profile_path = directory.path().join("profile.yaml");
        let health = directory.path().join("health.url");
        private_write(
            &profile_path,
            &serde_json::to_vec(&profile(&request.tunnel_id, &binding.gateway, &health))?,
        )?;
        let mut command = Command::new(binary);
        // local_test is only selected by the cfg(test) call below, never an API/config/env option.
        if local_test {
            command
                .args(["dev", "proxy", "--listen", "127.0.0.1:0", "--url-file"])
                .arg(directory.path().join("local-proxy.json"))
                .arg("--health-url-file")
                .arg(&health);
        } else {
            command.arg("run");
        }
        command
            .arg(if local_test {
                "--profile-file"
            } else {
                "--config"
            })
            .arg(&profile_path)
            .arg("--profile-dir")
            .arg(directory.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("NOVA_TUNNEL_RUNTIME_KEY", &request.runtime_key)
            .env(
                "NOVA_TUNNEL_MCP_AUTH",
                Zeroizing::new(format!("Bearer {}", config.token)).as_str(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| NovaError::InvalidState("tunnel client launch failed".into()))?;
        drop(command);
        drop(config);
        let status = Arc::new(Mutex::new(TunnelStatus {
            connection_id: binding.id.clone(),
            tunnel_id: request.tunnel_id.clone(),
            phase: "starting",
            last_error: None,
            transport_ready: false,
            real_host_verified: false,
            public_endpoint: None,
            credential_persisted: false,
        }));
        let (stop, mut requested) = watch::channel(false);
        let observed = status.clone();
        let id = binding.id.clone();
        let client = crate::local_mcp_bridge::http_client().map_err(|_| unavailable())?;
        #[cfg(test)]
        let local_proxy = directory.path().join("local-proxy.json");
        let task = tokio::spawn(async move {
            let _directory = directory;
            let started = Instant::now();
            let mut ever_ready = false;
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let error = loop {
                tokio::select! {
                    changed=requested.changed()=> { if changed.is_err() || *requested.borrow() { break None; } },
                    _=interval.tick()=> {
                        if child.try_wait().ok().flatten().is_some() { break Some("client_exited"); }
                        if current(&binding.service, &binding.path, &binding.id, &binding.upstream).is_err()
                            || !binding.gateways.is_running(&binding.id, &binding.gateway).await {
                            break Some("authorization_or_gateway_unavailable");
                        }
                        // Readiness is the client's report, never evidence of a real ChatGPT call.
                        let ready = if let Some(url) = health_endpoint(&health) {
                            client.get(url).timeout(Duration::from_millis(800)).send().await
                                .is_ok_and(|r| r.status() == reqwest::StatusCode::OK)
                        } else { false };
                        ever_ready |= ready;
                        { let mut s=observed.lock().unwrap_or_else(|e| e.into_inner());
                          // A stop can arrive while an HTTP readiness probe is awaiting.
                          if s.phase != "stopping" { s.phase=if ready {"ready"} else {"starting"}; s.transport_ready=ready; }
                        }
                        if !ever_ready && started.elapsed() > Duration::from_secs(120) { break Some("readiness_timeout"); }
                    }
                }
            };
            // Owned child handle only. No process-name search, shell or PID-file signalling.
            let _ = tokio::time::timeout(Duration::from_secs(2), child.kill()).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            let mut s = observed.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = if error.is_some() { "failed" } else { "stopped" };
            s.transport_ready = false;
            s.last_error = error;
        });
        let runtime = Runtime {
            status,
            stop,
            task,
            #[cfg(test)]
            local_proxy,
        };
        let result = runtime.status();
        runtimes.insert(id, runtime);
        Ok(result)
    }
}
pub(crate) struct TunnelBinding {
    pub id: String,
    pub path: PathBuf,
    pub upstream: String,
    pub gateway: String,
    pub gateways: Arc<GatewayManager>,
    pub service: IntakeApplicationService,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn system_install_locations_survive_missing_account_home() {
        assert_eq!(
            install_candidates("cloudflared", None),
            vec![
                PathBuf::from("/opt/homebrew/bin/cloudflared"),
                PathBuf::from("/usr/local/bin/cloudflared"),
            ]
        );
    }

    #[test]
    fn user_install_location_is_derived_from_account_home() {
        let home = account_home().expect("effective user has an OS account home");
        assert!(home.is_absolute());
        assert_eq!(
            install_candidates("tunnel-client", Some(home.clone()))[0],
            home.join(".local/bin/tunnel-client")
        );
    }

    async fn ready(manager: &TunnelManager) -> TunnelStatus {
        for _ in 0..160 {
            let status = manager.statuses().await.pop().unwrap();
            if status.phase == "ready" {
                return status;
            }
            assert_ne!(
                status.phase, "failed",
                "local test client exited before readiness"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("local test client readiness timeout");
    }
    async fn finished(manager: &TunnelManager) {
        for _ in 0..60 {
            if manager
                .runtimes
                .lock()
                .await
                .values()
                .all(|r| r.task.is_finished())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("owned child shutdown timeout");
    }
    async fn rpc(endpoint: &str, method: &str, params: Value, id: i64) -> Value {
        let response = crate::local_mcp_bridge::http_client()
            .unwrap()
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "local proxy RPC rejected");
        response.json().await.unwrap()
    }
    // Explicit opt-in: actual installed official client, local in-memory control plane only.
    #[tokio::test]
    #[ignore = "requires installed tunnel-client; runs actual client against local synthetic MCP only"]
    async fn installed_client_local_transport_save_replay_and_revocation() {
        assert!(
            installed_client().is_some(),
            "install tunnel-client for this local transport test"
        );
        let f = crate::gateway_runtime::tests::fixture().await;
        let gateways = Arc::new(GatewayManager::default());
        let gateway = gateways
            .start(&f.id, f.path.clone(), &f.upstream, f.service.clone())
            .await
            .unwrap();
        gateways
            .check(&f.id, &f.upstream, &f.service)
            .await
            .unwrap();
        let manager = TunnelManager::default();
        let test_key = "sk-synthetic-local-transport-only";
        let request = || TunnelRequest {
            tunnel_id: "tunnel_22222222222222222222222222222222".into(),
            runtime_key: test_key.into(),
            connect_to_openai: true,
        };
        let binding = || TunnelBinding {
            id: f.id.clone(),
            path: f.path.clone(),
            upstream: f.upstream.clone(),
            gateway: gateway.endpoint.clone().unwrap(),
            gateways: gateways.clone(),
            service: f.service.clone(),
        };
        manager.launch(request(), binding(), true).await.unwrap();
        let status = ready(&manager).await;
        assert!(!status.real_host_verified);
        assert!(status.public_endpoint.is_none());
        assert!(!status.credential_persisted);
        let proxy_path = manager
            .runtimes
            .lock()
            .await
            .get(&f.id)
            .unwrap()
            .local_proxy
            .clone();
        let profile_path = proxy_path.with_file_name("profile.yaml");
        let written = std::fs::read_to_string(&profile_path).unwrap();
        assert!(!written.contains(test_key));
        assert!(!written.contains(&read_local_connection(&f.path).unwrap().token));
        let proxy: Value = serde_json::from_slice(&std::fs::read(&proxy_path).unwrap()).unwrap();
        assert_eq!(proxy["control_plane_transport"], "unix");
        assert_eq!(proxy["control_plane_base_url"], "http://localhost");
        let endpoint = proxy["mcp_url"].as_str().unwrap().to_owned();
        let before = f
            .service
            .open()
            .unwrap()
            .connections(f.service.scope())
            .unwrap()[0]
            .used_calls;
        let tools = rpc(&endpoint, "tools/list", json!({}), 1).await;
        let mut names: Vec<_> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        assert_eq!(names, vec!["get_memory", "save_memory", "search_memory"]);
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            before
        );
        let params = json!({"name":"save_memory","arguments":{"request_id":"synthetic-tunnel-save","content":"SYNTHETIC tunnel transport only.","user_requested_save":true}});
        let first = rpc(&endpoint, "tools/call", params.clone(), 2).await;
        let replay = rpc(&endpoint, "tools/call", params, 3).await;
        assert_eq!(first["result"]["structuredContent"]["saved"], true);
        assert_eq!(replay["result"]["structuredContent"]["replayed"], true);
        assert_eq!(
            first["result"]["structuredContent"]["receipt"],
            replay["result"]["structuredContent"]["receipt"]
        );
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            before + 1
        );
        // Core revoke while the private file remains: both gateway and tunnel stop independently.
        f.service
            .open()
            .unwrap()
            .revoke_connection(f.service.scope(), &f.id)
            .unwrap();
        assert!(f.path.exists());
        finished(&manager).await;
        assert_eq!(
            manager.statuses().await[0].last_error,
            Some("authorization_or_gateway_unavailable")
        );
        assert!(!profile_path.exists());
        assert!(
            crate::local_mcp_bridge::http_client()
                .unwrap()
                .post(endpoint)
                .send()
                .await
                .is_err()
        );
        gateways.stop_all().await;
    }
    #[tokio::test]
    #[ignore = "requires installed tunnel-client; local lifecycle test only"]
    async fn installed_client_local_stop_restart_and_manager_drop() {
        let f = crate::gateway_runtime::tests::fixture().await;
        let gateways = Arc::new(GatewayManager::default());
        let gateway = gateways
            .start(&f.id, f.path.clone(), &f.upstream, f.service.clone())
            .await
            .unwrap();
        gateways
            .check(&f.id, &f.upstream, &f.service)
            .await
            .unwrap();
        let manager = TunnelManager::default();
        let request = || TunnelRequest {
            tunnel_id: "tunnel_22222222222222222222222222222222".into(),
            runtime_key: "sk-synthetic-local-lifecycle-only".into(),
            connect_to_openai: true,
        };
        let binding = || TunnelBinding {
            id: f.id.clone(),
            path: f.path.clone(),
            upstream: f.upstream.clone(),
            gateway: gateway.endpoint.clone().unwrap(),
            gateways: gateways.clone(),
            service: f.service.clone(),
        };
        manager.launch(request(), binding(), true).await.unwrap();
        ready(&manager).await;
        // Repeated start retains the same owned runtime and does not rotate the Nova grant.
        manager.launch(request(), binding(), true).await.unwrap();
        assert_eq!(manager.runtimes.lock().await.len(), 1);
        manager.stop(&f.id).await;
        finished(&manager).await;
        assert!(
            !f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .revoked
        );
        manager.launch(request(), binding(), true).await.unwrap();
        ready(&manager).await;
        let proxy = manager
            .runtimes
            .lock()
            .await
            .get(&f.id)
            .unwrap()
            .local_proxy
            .clone();
        drop(manager);
        for _ in 0..60 {
            if !proxy.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!proxy.exists());
        assert!(
            gateways
                .is_running(&f.id, gateway.endpoint.as_deref().unwrap())
                .await
        );
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            0
        );
        gateways.stop_all().await;
    }
    #[test]
    fn profile_and_validation_never_serialize_a_credential_or_allow_unsafe_input() {
        let profile = profile(
            "tunnel_22222222222222222222222222222222",
            "http://127.0.0.1:1234/mcp",
            Path::new("/tmp/health"),
        );
        assert_eq!(
            profile["mcp"]["extra_headers"]["Authorization"],
            "env:NOVA_TUNNEL_MCP_AUTH"
        );
        assert_eq!(
            profile["control_plane"]["api_key"],
            "env:NOVA_TUNNEL_RUNTIME_KEY"
        );
        for bad in ["sk-secret\nInjected", "Bearer secret", "short"] {
            assert!(
                TunnelRequest {
                    tunnel_id: "tunnel_22222222222222222222222222222222".into(),
                    runtime_key: bad.into(),
                    connect_to_openai: true
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            TunnelRequest {
                tunnel_id: "../../escape".into(),
                runtime_key: "sk-synthetic-only-credential".into(),
                connect_to_openai: true
            }
            .validate()
            .is_err()
        );
        assert!(
            TunnelRequest {
                tunnel_id: "tunnel_22222222222222222222222222222222".into(),
                runtime_key: "sk-synthetic-only-credential".into(),
                connect_to_openai: false
            }
            .validate()
            .is_err()
        );
    }
}
