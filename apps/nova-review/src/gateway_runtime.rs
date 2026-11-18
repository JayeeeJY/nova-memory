//! Workspace-owned loopback listeners. No subprocess, shell, tunnel, persisted PID or autostart.
use crate::local_mcp_bridge::{LocalConnection, forward, http_client, read_local_connection};
use nova_core::NovaError;
use nova_mcp::automatic::IntakeApplicationService;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};

const MAX_LIVE: usize = 8;
const MAX_HISTORY: usize = 64;
struct FenceOnDrop(Arc<AtomicBool>);
impl Drop for FenceOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct GatewayStatus {
    pub connection_id: String,
    pub phase: &'static str,
    pub endpoint: Option<String>,
    pub upstream_verified: bool,
    pub checked_at: Option<String>,
    pub last_error: Option<&'static str>,
    pub external_tunnel: &'static str,
    pub public_endpoint: Option<String>,
    pub real_host_verified: bool,
}
impl GatewayStatus {
    fn stopped(id: &str) -> Self {
        Self {
            connection_id: id.into(),
            phase: "stopped",
            endpoint: None,
            upstream_verified: false,
            checked_at: None,
            last_error: None,
            external_tunnel: "not_managed",
            public_endpoint: None,
            real_host_verified: false,
        }
    }
}
struct Runtime {
    endpoint: String,
    path: PathBuf,
    active: Arc<AtomicBool>,
    status: Arc<Mutex<GatewayStatus>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    delegated: crate::local_mcp_gateway::DelegatedCredential,
}
impl Runtime {
    fn status(&self) -> GatewayStatus {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !self.active.load(Ordering::Acquire) || self.task.is_finished() {
            if status.phase == "running" {
                status.phase = if self.task.is_finished() {
                    "failed"
                } else {
                    "stopping"
                };
            }
            status.endpoint = None;
            status.upstream_verified = false;
        }
        status
    }
    fn stop(&self) {
        self.active.store(false, Ordering::Release);
        self.delegated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if !self.task.is_finished() {
            status.phase = "stopping";
        }
        status.endpoint = None;
        status.upstream_verified = false;
        let _ = self.stop.send(true);
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Default)]
pub(crate) struct GatewayManager {
    runtimes: AsyncMutex<BTreeMap<String, Runtime>>,
}
pub(crate) struct GatewayDelegation {
    shared: crate::local_mcp_gateway::DelegatedCredential,
    secret: zeroize::Zeroizing<String>,
}
impl GatewayDelegation {
    pub fn credential(&self) -> Option<zeroize::Zeroizing<String>> {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|s| s.as_str() == self.secret.as_str())
            .map(|s| zeroize::Zeroizing::new(s.to_string()))
    }
    pub fn close(&self) {
        let mut value = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if value
            .as_ref()
            .is_some_and(|s| s.as_str() == self.secret.as_str())
        {
            value.take();
        }
    }
}
impl Drop for GatewayDelegation {
    fn drop(&mut self) {
        self.close();
    }
}

fn unavailable() -> NovaError {
    NovaError::PolicyDenied("gateway authority unavailable".into())
}
fn current(
    service: &IntakeApplicationService,
    path: &Path,
    id: &str,
    upstream: &str,
) -> Result<LocalConnection, NovaError> {
    let config = read_local_connection(path).map_err(|_| unavailable())?;
    if config.connection_id != id || config.endpoint != upstream {
        return Err(unavailable());
    }
    let access = service.authenticate(&config.token)?;
    if access.connection_id() != id {
        return Err(unavailable());
    }
    Ok(config)
}
fn now() -> Option<String> {
    nova_time::OffsetDateTime::now_utc()
        .format(&nova_time::format_description::well_known::Rfc3339)
        .ok()
}

impl GatewayManager {
    pub async fn delegate(&self, id: &str, endpoint: &str) -> Result<GatewayDelegation, NovaError> {
        let rows = self.runtimes.lock().await;
        let r = rows
            .get(id)
            .filter(|r| {
                let s = r.status();
                s.phase == "running"
                    && s.upstream_verified
                    && s.endpoint.as_deref() == Some(endpoint)
            })
            .ok_or_else(unavailable)?;
        let secret = zeroize::Zeroizing::new(nova_core::VaultKey::generate()?.to_hex());
        *r.delegated.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(zeroize::Zeroizing::new(secret.to_string()));
        Ok(GatewayDelegation {
            shared: r.delegated.clone(),
            secret,
        })
    }
    pub async fn is_running(&self, id: &str, endpoint: &str) -> bool {
        self.runtimes.lock().await.get(id).is_some_and(|r| {
            let status = r.status();
            status.phase == "running"
                && status.upstream_verified
                && status.endpoint.as_deref() == Some(endpoint)
        })
    }
    pub async fn statuses(&self) -> Vec<GatewayStatus> {
        self.runtimes
            .lock()
            .await
            .values()
            .map(Runtime::status)
            .collect()
    }
    pub async fn start(
        &self,
        id: &str,
        path: PathBuf,
        upstream: &str,
        service: IntakeApplicationService,
    ) -> Result<GatewayStatus, NovaError> {
        current(&service, &path, id, upstream)?;
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get(id)
            && !runtime.task.is_finished()
        {
            return if runtime.active.load(Ordering::Acquire) {
                Ok(runtime.status())
            } else {
                Err(NovaError::InvalidState(
                    "gateway is stopping; retry after shutdown".into(),
                ))
            };
        }
        if runtimes.values().filter(|r| !r.task.is_finished()).count() >= MAX_LIVE {
            return Err(NovaError::InvalidState("gateway runtime limit".into()));
        }
        if runtimes.len() >= MAX_HISTORY {
            runtimes.retain(|_, r| !r.task.is_finished());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| NovaError::InvalidState("gateway listener unavailable".into()))?;
        let address = listener
            .local_addr()
            .map_err(|_| NovaError::InvalidState("gateway listener unavailable".into()))?;
        let endpoint = format!("http://{address}/mcp");
        let active = Arc::new(AtomicBool::new(true));
        let delegated = Arc::new(Mutex::new(None));
        let router = crate::local_mcp_gateway::delegated_router(
            path.clone(),
            address.to_string(),
            active.clone(),
            delegated.clone(),
        )
        .map_err(|_| unavailable())?;
        let status = Arc::new(Mutex::new(GatewayStatus {
            phase: "running",
            endpoint: Some(endpoint.clone()),
            ..GatewayStatus::stopped(id)
        }));
        let (stop, mut requested) = watch::channel(false);
        let fence = active.clone();
        let observed = status.clone();
        let config_path = path.clone();
        let id_owned = id.to_owned();
        let upstream = upstream.to_owned();
        let task = tokio::spawn(async move {
            let _fence_on_drop = FenceOnDrop(fence.clone());
            let monitor_fence = fence.clone();
            let monitor_status = observed.clone();
            let shutdown = async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tokio::select! {
                        changed=requested.changed()=>{if changed.is_err() || *requested.borrow(){break;}},
                        _=interval.tick()=>{
                            if current(&service,&config_path,&id_owned,&upstream).is_err(){
                                let mut status=monitor_status.lock().unwrap_or_else(|e|e.into_inner());
                                status.last_error=Some("authorization_unavailable");break;
                            }
                        }
                    }
                }
                monitor_fence.store(false, Ordering::Release);
                let mut status = monitor_status.lock().unwrap_or_else(|e| e.into_inner());
                status.phase = "stopping";
                status.endpoint = None;
                status.upstream_verified = false;
            };
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown)
                .await;
            fence.store(false, Ordering::Release);
            let mut status = observed.lock().unwrap_or_else(|e| e.into_inner());
            status.phase = if result.is_ok() { "stopped" } else { "failed" };
            status.endpoint = None;
            status.upstream_verified = false;
            if result.is_err() {
                status.last_error = Some("listener_failed");
            }
        });
        let runtime = Runtime {
            delegated,
            endpoint,
            path,
            active,
            status,
            stop,
            task,
        };
        let result = runtime.status();
        runtimes.insert(id.into(), runtime);
        Ok(result)
    }
    pub async fn check(
        &self,
        id: &str,
        upstream: &str,
        service: &IntakeApplicationService,
    ) -> Result<GatewayStatus, NovaError> {
        let (endpoint, path, active, status) = {
            let runtimes = self.runtimes.lock().await;
            let Some(r) = runtimes.get(id) else {
                return Ok(GatewayStatus::stopped(id));
            };
            if !r.active.load(Ordering::Acquire) || r.task.is_finished() {
                return Ok(r.status());
            }
            (
                r.endpoint.clone(),
                r.path.clone(),
                r.active.clone(),
                r.status.clone(),
            )
        };
        let config = current(service, &path, id, upstream)?;
        let selected = LocalConnection::new(id.into(), endpoint, config.token.clone());
        let client = http_client()
            .map_err(|_| NovaError::InvalidState("gateway check unavailable".into()))?;
        let init = json!({"jsonrpc":"2.0","id":"nova-gateway-health","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"nova-workspace-gateway-check","version":"1"}}});
        let verified = tokio::time::timeout(
            Duration::from_secs(3),
            forward(&client, &selected, &init, None),
        )
        .await
        .is_ok_and(|r| {
            r.is_ok_and(|r| {
                r.is_some_and(|r| {
                    r["result"]["serverInfo"]["name"] == "nova-memory-automatic"
                        && r["result"]["serverInfo"]["version"] == "0.3.0"
                })
            })
        });
        let mut result = status.lock().unwrap_or_else(|e| e.into_inner());
        if active.load(Ordering::Acquire) && result.phase == "running" {
            result.upstream_verified = verified;
            result.checked_at = now();
            result.last_error = if verified {
                None
            } else {
                Some("upstream_unavailable")
            };
        }
        Ok(result.clone())
    }
    pub async fn stop(&self, id: &str) -> GatewayStatus {
        let runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get(id) {
            runtime.stop();
            runtime.status()
        } else {
            GatewayStatus::stopped(id)
        }
    }
    pub async fn stop_all(&self) {
        for runtime in self.runtimes.lock().await.values() {
            runtime.stop();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nova_core::{
        VaultKey,
        compilation::AssetScope,
        intake::{ConnectionHost, ConnectionPreset, ConnectionRequest, IntakeStore},
    };
    pub(crate) struct Fixture {
        _dir: tempfile::TempDir,
        pub(crate) service: IntakeApplicationService,
        pub(crate) path: PathBuf,
        pub(crate) id: String,
        pub(crate) upstream: String,
        task: JoinHandle<std::io::Result<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    pub(crate) async fn fixture() -> Fixture {
        fixture_with_host(ConnectionHost::Chatgpt).await
    }
    pub(crate) async fn fixture_with_host(host: ConnectionHost) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let key_path = dir.path().join("key");
        key.write_new_file(&key_path).unwrap();
        let db = dir.path().join("intake.sqlite");
        let scope = AssetScope {
            owner_id: "owner:synthetic".into(),
            space: "runtime".into(),
        };
        let mut store = IntakeStore::create(&db, key, scope.clone()).unwrap();
        store.enable_compilation(&scope).unwrap();
        store.enable_connections(&scope).unwrap();
        let issued = store
            .create_connection(
                &scope,
                ConnectionRequest {
                    request_id: "runtime".into(),
                    label: "Synthetic".into(),
                    host,
                    preset: ConnectionPreset::CaptureRead,
                    topic_id: None,
                    include_future: false,
                },
            )
            .unwrap();
        let service = IntakeApplicationService::new(db, key_path, scope);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = format!("http://{address}/mcp/v03");
        let path = crate::local_mcp_bridge::write_local_connection(
            &dir.path().join("private"),
            &LocalConnection::new(issued.connection.id.clone(), upstream.clone(), issued.token),
        )
        .unwrap();
        let app = nova_mcp::automatic::router(service.clone(), address.to_string());
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Fixture {
            _dir: dir,
            service,
            path,
            id: issued.connection.id,
            upstream,
            task,
        }
    }
    async fn closed(endpoint: &str) -> bool {
        let url = url::Url::parse(endpoint).unwrap();
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", url.port().unwrap()))
                .await
                .is_err()
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }
    #[tokio::test]
    async fn live_core_revocation_stops_monitor_even_if_private_file_still_exists() {
        let f = fixture().await;
        let manager = GatewayManager::default();
        let running = manager
            .start(&f.id, f.path.clone(), &f.upstream, f.service.clone())
            .await
            .unwrap();
        assert!(
            manager
                .check(&f.id, &f.upstream, &f.service)
                .await
                .unwrap()
                .upstream_verified
        );
        f.service
            .open()
            .unwrap()
            .revoke_connection(f.service.scope(), &f.id)
            .unwrap();
        assert!(f.path.exists());
        assert!(closed(running.endpoint.as_ref().unwrap()).await);
        let status = manager.statuses().await.pop().unwrap();
        assert_eq!(status.phase, "stopped");
        assert_eq!(status.last_error, Some("authorization_unavailable"));
        assert_eq!(
            f.service
                .open()
                .unwrap()
                .connections(f.service.scope())
                .unwrap()[0]
                .used_calls,
            0
        );
    }
    #[tokio::test]
    async fn dropping_workspace_manager_closes_listener_without_revoking_grant() {
        let f = fixture().await;
        let manager = GatewayManager::default();
        let running = manager
            .start(&f.id, f.path.clone(), &f.upstream, f.service.clone())
            .await
            .unwrap();
        drop(manager);
        assert!(closed(running.endpoint.as_ref().unwrap()).await);
        let rows = f
            .service
            .open()
            .unwrap()
            .connections(f.service.scope())
            .unwrap();
        assert!(!rows[0].revoked);
        assert_eq!(rows[0].used_calls, 0);
    }
}
