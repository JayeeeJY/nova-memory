//! Explicit experimental HTTPS exposure of one authenticated MCP-only gateway.
//! No account/config discovery, credential injection, public listener, shell or arbitrary target.
use crate::tunnel_runtime::{TunnelBinding, current, installed_binary};
use nova_core::NovaError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuickRequest {
    pub expose_via_cloudflare: bool,
}
impl QuickRequest {
    pub fn validate(&self) -> Result<(), NovaError> {
        if self.expose_via_cloudflare {
            Ok(())
        } else {
            Err(NovaError::Validation(
                "explicit Cloudflare exposure required".into(),
            ))
        }
    }
}
pub(crate) fn installed_client() -> Option<PathBuf> {
    installed_binary("cloudflared")
}

#[derive(Clone, Serialize)]
pub(crate) struct QuickStatus {
    pub connection_id: String,
    pub phase: &'static str,
    pub public_endpoint: Option<String>,
    pub transport_ready: bool,
    pub last_error: Option<&'static str>,
    pub real_host_verified: bool,
    pub authentication: &'static str,
    pub provider: &'static str,
}
struct Runtime {
    status: Arc<Mutex<QuickStatus>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    delegation: Arc<crate::gateway_runtime::GatewayDelegation>,
}
impl Runtime {
    fn status(&self) -> QuickStatus {
        let mut s = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if self.task.is_finished()
            && matches!(s.phase, "starting" | "address_allocated" | "connected")
        {
            s.phase = "failed";
            s.transport_ready = false;
            s.public_endpoint = None;
        }
        s
    }
    fn stop(&self) {
        self.delegation.close();
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.task.is_finished() {
            s.phase = "stopping";
        }
        s.public_endpoint = None;
        s.transport_ready = false;
        let _ = self.stop.send(true);
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Default)]
pub(crate) struct QuickManager {
    runtimes: AsyncMutex<BTreeMap<String, Runtime>>,
}

fn unavailable() -> NovaError {
    NovaError::PolicyDenied("Grok gateway unavailable".into())
}

/// Parse only a single provider hostname. Ignore all other diagnostics, paths and credential-like text.
fn public_endpoint(line: &str) -> Option<String> {
    // The bootstrap URL occupies its own ASCII-box line. Never promote a URL from an error/request log.
    let (_, message) = line.split_once('|')?;
    let candidate = message.trim_end().strip_suffix('|')?.trim();
    if !candidate.starts_with("https://") || candidate.chars().any(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(candidate).ok()?;
    let host = url.host_str()?;
    let label = host.strip_suffix(".trycloudflare.com")?;
    if url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        Some(format!("https://{host}/mcp"))
    } else {
        None
    }
}
fn metrics_endpoint(line: &str) -> Option<String> {
    let (_, address) = line.split_once("Starting metrics server on ")?;
    let address = address
        .split(|c: char| c.is_whitespace() || ['"', '\\'].contains(&c))
        .next()?;
    let socket = address
        .strip_suffix("/metrics")?
        .parse::<std::net::SocketAddr>()
        .ok()?;
    if socket.ip() != std::net::Ipv4Addr::LOCALHOST || socket.port() == 0 {
        return None;
    }
    Some(format!("http://{socket}/ready"))
}
fn command(binding: &TunnelBinding, directory: &std::path::Path) -> Result<Command, NovaError> {
    use std::io::Write;
    let binary = installed_client()
        .ok_or_else(|| NovaError::InvalidState("cloudflared not installed".into()))?;
    let url = url::Url::parse(&binding.gateway).map_err(|_| unavailable())?;
    if url.host_str() != Some("127.0.0.1") || url.scheme() != "http" || url.path() != "/mcp" {
        return Err(unavailable());
    }
    let port = url.port().filter(|p| *p != 0).ok_or_else(unavailable)?;
    let authority = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, port);
    let config = directory.join("config.yaml");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(&config)?.write_all(b"{}\n")?;
    let mut cmd = Command::new(binary);
    cmd.args(["tunnel", "--no-autoupdate", "--config"])
        .arg(config)
        // Override default cert lookup too: no existing ~/.cloudflared material is read.
        .arg("--origincert")
        .arg(directory.join("unused-cert.pem"))
        .args([
            "--url",
            &format!("http://{authority}"),
            "--http-host-header",
            &authority.to_string(),
            "--metrics",
            "127.0.0.1:0",
            "--management-diagnostics=false",
            "--loglevel",
            "info",
            "--protocol",
            "http2",
            "--grace-period",
            "1s",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(directory);
    Ok(cmd)
}

impl QuickManager {
    pub async fn statuses(&self) -> Vec<QuickStatus> {
        self.runtimes
            .lock()
            .await
            .values()
            .map(Runtime::status)
            .collect()
    }
    pub async fn configuration(&self, id: &str) -> Result<serde_json::Value, NovaError> {
        let rows = self.runtimes.lock().await;
        let r = rows.get(id).ok_or_else(unavailable)?;
        let status = r.status();
        if status.phase != "connected" || !status.transport_ready {
            return Err(unavailable());
        }
        let secret = r.delegation.credential().ok_or_else(unavailable)?;
        Ok(
            serde_json::json!({"configuration":{"url":status.public_endpoint,"headers":{"Authorization":format!("Bearer {}",secret.as_str())}},
            "contains_client_credential":true,"temporary_credential":true,"grok_authentication":"bearer_support_unverified_oauth_not_implemented","real_host_verified":false}),
        )
    }
    pub async fn stop(&self, id: &str) {
        if let Some(r) = self.runtimes.lock().await.get(id) {
            r.stop();
        }
    }
    pub async fn stop_all(&self) {
        for r in self.runtimes.lock().await.values() {
            r.stop();
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
    async fn authority(binding: &TunnelBinding) -> Result<(), NovaError> {
        current(
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
        Ok(())
    }
    pub async fn start(
        &self,
        request: QuickRequest,
        binding: TunnelBinding,
    ) -> Result<QuickStatus, NovaError> {
        request.validate()?;
        Self::authority(&binding).await?;
        let directory = tempfile::Builder::new()
            .prefix("nova-quick-tunnel-")
            .tempdir()?;
        let cmd = command(&binding, directory.path())?;
        self.launch(binding, cmd, directory).await
    }
    async fn launch(
        &self,
        binding: TunnelBinding,
        mut cmd: Command,
        directory: tempfile::TempDir,
    ) -> Result<QuickStatus, NovaError> {
        Self::authority(&binding).await?;
        let mut rows = self.runtimes.lock().await;
        if let Some(r) = rows.get(&binding.id)
            && !r.task.is_finished()
        {
            return if matches!(
                r.status().phase,
                "starting" | "address_allocated" | "connected"
            ) {
                Ok(r.status())
            } else {
                Err(NovaError::InvalidState(
                    "quick tunnel still stopping".into(),
                ))
            };
        }
        if rows.values().filter(|r| !r.task.is_finished()).count() >= 4 {
            return Err(NovaError::InvalidState("quick tunnel runtime limit".into()));
        }
        if rows.len() >= 64 {
            rows.retain(|_, r| !r.task.is_finished());
        }
        let delegation = Arc::new(
            binding
                .gateways
                .delegate(&binding.id, &binding.gateway)
                .await?,
        );
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|_| NovaError::InvalidState("cloudflared launch failed".into()))?;
        drop(cmd);
        let mut diagnostics = child.stderr.take().ok_or_else(unavailable)?;
        let status = Arc::new(Mutex::new(QuickStatus {
            connection_id: binding.id.clone(),
            phase: "starting",
            public_endpoint: None,
            transport_ready: false,
            last_error: None,
            real_host_verified: false,
            authentication: "scoped_bearer_required_grok_support_unverified",
            provider: "cloudflare_quick_experimental",
        }));
        let observed = status.clone();
        let id = binding.id.clone();
        let (stop, mut requested) = watch::channel(false);
        let client = crate::local_mcp_bridge::http_client().map_err(|_| unavailable())?;
        let permission = delegation.clone();
        let task = tokio::spawn(async move {
            let _directory = directory;
            let started = Instant::now();
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let mut chunk = [0u8; 4096];
            let mut line = Vec::new();
            let mut discard_line = false;
            let mut total = 0usize;
            let mut metrics = None;
            let mut ever_ready = false;
            let error = loop {
                tokio::select! {
                    change=requested.changed()=>{if change.is_err()||*requested.borrow(){break None;}},
                    read=diagnostics.read(&mut chunk)=>{
                        let length=match read {Ok(0)|Err(_)=>break Some("client_exited"),Ok(n)=>n};
                        total+=length;if total>1024*1024{break Some("diagnostics_limit");}
                        for &byte in &chunk[..length]{
                            if byte==b'\n'{
                                if !discard_line&&let Ok(text)=std::str::from_utf8(&line){
                                    if let Some(url)=public_endpoint(text){let mut s=observed.lock().unwrap_or_else(|e|e.into_inner());if s.phase=="starting"&&s.public_endpoint.is_none(){s.public_endpoint=Some(url);s.phase="address_allocated";s.transport_ready=false;}}
                                    if metrics.is_none()&&let Some(url)=metrics_endpoint(text){metrics=Some(url);}
                                }
                                line.clear();discard_line=false;
                            }else if line.len()<4096&&!discard_line{line.push(byte);}else{line.clear();discard_line=true;}
                        }
                    },
                    _=interval.tick()=>{
                        match child.try_wait(){Ok(None)=>{},_=>break Some("client_exited")}
                        if Self::authority(&binding).await.is_err(){break Some("authorization_or_gateway_unavailable");}
                        let ready=if let Some(url)=&metrics{client.get(url).timeout(Duration::from_millis(700)).send().await.is_ok_and(|r|r.status()==reqwest::StatusCode::OK)}else{false};
                        {let mut s=observed.lock().unwrap_or_else(|e|e.into_inner());
                            if s.phase!="stopping"{s.transport_ready=ready&&s.public_endpoint.is_some();ever_ready|=s.transport_ready;s.phase=if s.transport_ready{"connected"}else if s.public_endpoint.is_some(){"address_allocated"}else{"starting"};}}
                        if !ever_ready&&started.elapsed()>Duration::from_secs(120){break Some("readiness_timeout");}
                    }
                }
            };
            permission.close();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.kill()).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            let mut s = observed.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = if error.is_some() { "failed" } else { "stopped" };
            s.last_error = error;
            s.public_endpoint = None;
            s.transport_ready = false;
        });
        let r = Runtime {
            status,
            stop,
            task,
            delegation,
        };
        let result = r.status();
        rows.insert(id, r);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn binding() -> (crate::gateway_runtime::tests::Fixture, TunnelBinding) {
        let f = crate::gateway_runtime::tests::fixture_with_host(
            nova_core::intake::ConnectionHost::Grok,
        )
        .await;
        let gateways = Arc::new(crate::gateway_runtime::GatewayManager::default());
        let gateway = gateways
            .start(&f.id, f.path.clone(), &f.upstream, f.service.clone())
            .await
            .unwrap();
        gateways
            .check(&f.id, &f.upstream, &f.service)
            .await
            .unwrap();
        let b = TunnelBinding {
            id: f.id.clone(),
            path: f.path.clone(),
            upstream: f.upstream.clone(),
            gateway: gateway.endpoint.unwrap(),
            gateways,
            service: f.service.clone(),
        };
        (f, b)
    }
    fn synthetic_child() -> Command {
        // Test adapter only: a local health listener and two synthetic diagnostic lines. No tunnel/relay.
        let python = [
            "/opt/homebrew/bin/python3.14",
            "/usr/bin/python3",
            "/usr/local/bin/python3",
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .expect("Python 3 required for local child fixture");
        let mut c = Command::new(python);
        c.args(["-u","-c",r#"import http.server,sys
class H(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200 if self.path=='/ready' else 404);self.end_headers()
 def log_message(self,*args): pass
s=http.server.HTTPServer(('127.0.0.1',0),H)
print('INFO | https://synthetic-nova-test-only.trycloudflare.com |',file=sys.stderr,flush=True)
print('INFO Starting metrics server on 127.0.0.1:%s/metrics'%s.server_port,file=sys.stderr,flush=True)
s.serve_forever()
"#]).env_clear().env("PATH","/usr/bin:/bin");
        c
    }
    async fn connected(manager: &QuickManager) {
        for _ in 0..60 {
            let s = manager.statuses().await.pop().unwrap();
            if s.phase == "connected" {
                return;
            }
            assert_ne!(s.phase, "failed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("synthetic child readiness timeout");
    }
    async fn finished(manager: &QuickManager) {
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
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("owned child cleanup timeout");
    }
    async fn rpc(
        endpoint: &str,
        auth: &str,
        method: &str,
        params: serde_json::Value,
    ) -> reqwest::Response {
        crate::local_mcp_bridge::http_client()
            .unwrap()
            .post(endpoint)
            .header("Authorization", auth)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn synthetic_child_real_gateway_credential_rotation_and_core_revocation() {
        let (f, b) = binding().await;
        let endpoint = b.gateway.clone();
        let gateways = b.gateways.clone();
        let manager = QuickManager::default();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        manager
            .launch(b, synthetic_child(), directory)
            .await
            .unwrap();
        connected(&manager).await;
        let config = manager.configuration(&f.id).await.unwrap();
        let first = config["configuration"]["headers"]["Authorization"]
            .as_str()
            .unwrap()
            .to_owned();
        let internal = crate::local_mcp_bridge::read_local_connection(&f.path).unwrap();
        assert_ne!(first, format!("Bearer {}", internal.token));
        assert!(
            !serde_json::to_string(&manager.statuses().await)
                .unwrap()
                .contains(&first)
        );
        let args = serde_json::json!({"name":"save_memory","arguments":{"request_id":"synthetic-https-save","content":"SYNTHETIC HTTPS controller test only.","user_requested_save":true}});
        let a: serde_json::Value = rpc(&endpoint, &first, "tools/call", args.clone())
            .await
            .json()
            .await
            .unwrap();
        let replay: serde_json::Value = rpc(&endpoint, &first, "tools/call", args)
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(a["result"]["structuredContent"]["saved"], true);
        assert_eq!(replay["result"]["structuredContent"]["replayed"], true);
        assert_eq!(
            a["result"]["structuredContent"]["receipt"],
            replay["result"]["structuredContent"]["receipt"]
        );
        let count = f
            .service
            .open()
            .unwrap()
            .connections(f.service.scope())
            .unwrap()[0]
            .used_calls;
        assert_eq!(count, 1);
        manager.stop(&f.id).await;
        assert_eq!(
            rpc(&endpoint, &first, "ping", serde_json::json!({}))
                .await
                .status(),
            401
        ); // No polling/drain wait.
        assert!(manager.configuration(&f.id).await.is_err());
        finished(&manager).await;
        assert!(!path.exists());
        assert_eq!(
            rpc(
                &endpoint,
                &format!("Bearer {}", internal.token),
                "ping",
                serde_json::json!({})
            )
            .await
            .status(),
            200
        );
        let b = TunnelBinding {
            id: f.id.clone(),
            path: f.path.clone(),
            upstream: f.upstream.clone(),
            gateway: endpoint.clone(),
            gateways: gateways.clone(),
            service: f.service.clone(),
        };
        manager
            .launch(b, synthetic_child(), tempfile::tempdir().unwrap())
            .await
            .unwrap();
        connected(&manager).await;
        let config = manager.configuration(&f.id).await.unwrap();
        let second = config["configuration"]["headers"]["Authorization"]
            .as_str()
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(
            rpc(&endpoint, &first, "ping", serde_json::json!({}))
                .await
                .status(),
            401
        );
        assert_eq!(
            rpc(&endpoint, second, "ping", serde_json::json!({}))
                .await
                .status(),
            200
        );
        f.service
            .open()
            .unwrap()
            .revoke_connection(f.service.scope(), &f.id)
            .unwrap();
        finished(&manager).await;
        assert_eq!(
            manager.statuses().await[0].last_error,
            Some("authorization_or_gateway_unavailable")
        );
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            1
        );
        gateways.stop_all().await;
    }
    #[tokio::test]
    async fn stopping_temporary_credential_fences_waiting_body_without_revoking_gateway() {
        let (f, b) = binding().await;
        let gateways = b.gateways.clone();
        let endpoint = b.gateway.clone();
        let manager = QuickManager::default();
        manager
            .launch(b, synthetic_child(), tempfile::tempdir().unwrap())
            .await
            .unwrap();
        connected(&manager).await;
        let config = manager.configuration(&f.id).await.unwrap();
        let auth = config["configuration"]["headers"]["Authorization"]
            .as_str()
            .unwrap();
        let url = url::Url::parse(&endpoint).unwrap();
        let address = format!("127.0.0.1:{}", url.port().unwrap());
        let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
        let body=serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"save_memory","arguments":{"request_id":"waiting-body","content":"SYNTHETIC must not save.","user_requested_save":true}}}).to_string();
        let headers = format!(
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body.as_bytes()[..1]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        manager.stop(&f.id).await;
        stream.write_all(&body.as_bytes()[1..]).await.unwrap();
        let mut response = [0; 512];
        let n = stream.read(&mut response).await.unwrap();
        assert!(
            std::str::from_utf8(&response[..n])
                .unwrap()
                .starts_with("HTTP/1.1 401")
        );
        finished(&manager).await;
        assert!(gateways.is_running(&f.id, &endpoint).await);
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
    #[tokio::test]
    async fn dropping_controller_invalidates_credential_and_reaps_owned_fixture() {
        let (f, b) = binding().await;
        let gateways = b.gateways.clone();
        let endpoint = b.gateway.clone();
        let manager = QuickManager::default();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        manager
            .launch(b, synthetic_child(), directory)
            .await
            .unwrap();
        connected(&manager).await;
        let config = manager.configuration(&f.id).await.unwrap();
        let auth = config["configuration"]["headers"]["Authorization"]
            .as_str()
            .unwrap();
        drop(manager);
        assert_eq!(
            rpc(&endpoint, auth, "ping", serde_json::json!({}))
                .await
                .status(),
            401
        );
        for _ in 0..60 {
            if !path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!path.exists());
        assert!(gateways.is_running(&f.id, &endpoint).await);
        gateways.stop_all().await;
    }
    #[tokio::test]
    #[ignore = "requires installed cloudflared; loopback bootstrap rejection only, never an edge/public tunnel"]
    async fn actual_cloudflared_accepts_generated_flags_and_stops_on_local_bootstrap_rejection() {
        let (f, b) = binding().await;
        let gateways = b.gateways.clone();
        let manager = QuickManager::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = requested.clone();
        let app = axum::Router::new().route(
            "/tunnel",
            axum::routing::post(move || async move {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                axum::Json(serde_json::json!({"result":{"id":"synthetic-invalid-no-edge-issued"}}))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let directory = tempfile::tempdir().unwrap();
        let mut cmd = command(&b, directory.path()).unwrap();
        // Official client's local test bootstrap override; not exposed by the production API.
        cmd.args(["--quick-service", &format!("http://{address}")]);
        manager.launch(b, cmd, directory).await.unwrap();
        finished(&manager).await;
        assert!(requested.load(std::sync::atomic::Ordering::SeqCst));
        let s = manager.statuses().await.pop().unwrap();
        assert_eq!(s.phase, "failed");
        assert!(!s.transport_ready);
        assert!(s.public_endpoint.is_none());
        assert!(!s.real_host_verified);
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            0
        );
        server.abort();
        gateways.stop_all().await;
    }
    #[tokio::test]
    async fn temporary_credentials_are_isolated_and_stale_cleanup_cannot_erase_replacement() {
        let (f, a) = binding().await;
        let (g, b) = binding().await;
        let old = a.gateways.delegate(&f.id, &a.gateway).await.unwrap();
        let old_token = old.credential().unwrap();
        let replacement = a.gateways.delegate(&f.id, &a.gateway).await.unwrap();
        let new_token = replacement.credential().unwrap();
        old.close();
        drop(old);
        let auth = format!("Bearer {}", new_token.as_str());
        assert_eq!(
            rpc(&a.gateway, &auth, "ping", serde_json::json!({}))
                .await
                .status(),
            200
        );
        assert_eq!(
            rpc(
                &a.gateway,
                &format!("Bearer {}", old_token.as_str()),
                "ping",
                serde_json::json!({})
            )
            .await
            .status(),
            401
        );
        assert_eq!(
            rpc(&b.gateway, &auth, "ping", serde_json::json!({}))
                .await
                .status(),
            401
        );
        replacement.close();
        assert_eq!(
            rpc(&a.gateway, &auth, "ping", serde_json::json!({}))
                .await
                .status(),
            401
        );
        assert_eq!(
            g.service
                .open()
                .unwrap()
                .connections(g.service.scope())
                .unwrap()[0]
                .used_calls,
            0
        );
        a.gateways.stop_all().await;
        b.gateways.stop_all().await;
    }
    #[test]
    fn only_expected_public_hostname_and_private_metrics_are_extracted() {
        assert_eq!(
            public_endpoint("INFO | https://synthetic-four-words.trycloudflare.com |"),
            Some("https://synthetic-four-words.trycloudflare.com/mcp".into())
        );
        for invalid in [
            "https://evil.invalid",
            "https://x.trycloudflare.com.evil.invalid",
            "https://a.b.trycloudflare.com",
            "https://token@x.trycloudflare.com",
            "https://x.trycloudflare.com/?key=secret",
            "https://x.trycloudflare.com/path",
            "https://x.trycloudflare.com:444",
        ] {
            assert!(public_endpoint(&format!("INFO | {invalid} |")).is_none());
        }
        assert_eq!(
            metrics_endpoint("INFO Starting metrics server on 127.0.0.1:1234/metrics"),
            Some("http://127.0.0.1:1234/ready".into())
        );
        for address in [
            "0.0.0.0:1234",
            "localhost:1234",
            "192.168.1.1:1",
            "127.0.0.1:0",
        ] {
            assert!(
                metrics_endpoint(&format!("Starting metrics server on {address}/metrics"))
                    .is_none()
            );
        }
        assert!(
            QuickRequest {
                expose_via_cloudflare: false
            }
            .validate()
            .is_err()
        );
    }
}
