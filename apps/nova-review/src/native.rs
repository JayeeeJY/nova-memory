//! Opt-in native workspace, Owner graph and connection controls, separate from legacy Canonical workflows.
use super::*;
use axum::{
    body::Bytes,
    extract::{Path, Query},
};
use nova_core::{compilation::AssetScope, intake::*};
use nova_mcp::automatic::IntakeApplicationService;

pub(super) fn service(s: &ReviewServer) -> Result<IntakeApplicationService, NovaError> {
    if !s.native_workspace {
        return Err(NovaError::PolicyDenied(
            "native workspace not enabled".into(),
        ));
    }
    let mut service = IntakeApplicationService::new(
        s.intake_store
            .clone()
            .ok_or_else(|| NovaError::InvalidState("intake unavailable".into()))?,
        s.key_path.clone(),
        AssetScope {
            owner_id: s.owner.clone(),
            space: s.space.clone(),
        },
    );
    if s.continuity_enabled {
        service = service.with_continuity();
    }
    if s.ranked_retrieval_enabled {
        service = service.with_ranked_retrieval();
    }
    if s.historical_retrieval_enabled {
        service = service.with_historical_retrieval();
    }
    Ok(service)
}
pub(super) fn page() -> Response {
    Html(include_str!("native.html")).into_response()
}
pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/native.js", get(js))
        .route("/capture-console.js", get(capture_js))
        .route("/provider-console.js", get(provider_js))
        .route("/native.css", get(css))
        .route("/graph.js", get(graph_js))
        .route("/graph.css", get(graph_css))
        .route("/report.js", get(report_js))
        .route("/report.css", get(report_css))
        .route(
            "/console/api/owner/intake/connections",
            get(connections).post(create),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/revoke",
            post(revoke),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/diagnose",
            post(diagnose_connection),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/local-setup",
            post(recover_local_setup),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/gateway-setup",
            post(recover_gateway_setup),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/gateway/start",
            post(start_gateway),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/gateway/check",
            post(check_gateway),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/gateway/stop",
            post(stop_gateway),
        )
        .route("/console/api/owner/intake/usage", get(usage))
        .route(
            "/console/api/owner/intake/connections/{id}/tunnel/start",
            post(start_tunnel),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/tunnel/stop",
            post(stop_tunnel),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/https/start",
            post(start_https),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/https/stop",
            post(stop_https),
        )
        .route(
            "/console/api/owner/intake/connections/{id}/https/configuration",
            post(https_configuration),
        )
        .route("/console/api/owner/intake/overview", get(overview))
        .route("/console/api/owner/intake/graph", get(graph))
        .route("/console/api/owner/intake/report", get(report))
        .route(
            "/console/api/owner/intake/task-evaluations",
            post(import_task_evaluation).layer(axum::extract::DefaultBodyLimit::max(512 * 1024)),
        )
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("native.js"),
    )
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("native.css"),
    )
}
async fn provider_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("provider-console.js"),
    )
}
async fn capture_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("capture-console.js"),
    )
}
async fn graph_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("graph.js"),
    )
}
async fn graph_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("graph.css"),
    )
}
async fn report_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("report.js"),
    )
}
async fn report_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("report.css"),
    )
}
fn response(result: Result<serde_json::Value, NovaError>) -> Response {
    super::intake::response(result)
}
#[cfg(unix)]
fn local_bridge_binary() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let binary = std::env::current_exe()
        .ok()?
        .parent()?
        .join("nova-memory-connect");
    let metadata = std::fs::symlink_metadata(&binary).ok()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || (metadata.uid() != unsafe { libc::geteuid() } && metadata.uid() != 0)
    {
        return None;
    }
    Some(binary)
}
#[cfg(not(unix))]
fn local_bridge_binary() -> Option<std::path::PathBuf> {
    None
}
fn local_host(host: ConnectionHost) -> bool {
    matches!(
        host,
        ConnectionHost::Codex
            | ConnectionHost::Claude
            | ConnectionHost::Cursor
            | ConnectionHost::Workbuddy
    )
}
fn remote_host(host: ConnectionHost) -> bool {
    matches!(host, ConnectionHost::Chatgpt | ConnectionHost::Grok)
}
fn gateway_configuration(binary: &std::path::Path, path: &std::path::Path) -> serde_json::Value {
    json!({"command":binary,"args":["--config",path,"--http-bind","127.0.0.1:0"],"transport":"authenticated_loopback_mcp_gateway","credential_storage":"private_local_file","contains_inline_credential":false,"authentication_required":true,"owner_routes":false,"public_endpoint":null,"tunnel_started":false,"real_host_verification":"not_run"})
}
fn local_configuration(
    binary: &std::path::Path,
    path: &std::path::Path,
    host: ConnectionHost,
) -> serde_json::Value {
    let args = json!(["--config", path]);
    let codex = format!(
        "[mcp_servers.nova_memory]\ncommand = {}\nargs = {}\n",
        json!(binary),
        args
    );
    let mut server = json!({"command":binary,"args":args});
    if matches!(host, ConnectionHost::Cursor) {
        server["type"] = json!("stdio");
    }
    json!({"mcp_json":{"mcpServers":{"nova-memory":server}},"codex_toml":codex,"credential_storage":"private_local_file","contains_inline_credential":false,"transport":"stdio_to_loopback_http","bridge_protocol_version":"2025-11-25","real_host_verification":"not_run"})
}
#[cfg(unix)]
fn local_client_setup(
    s: &ReviewServer,
    issued: &IssuedConnection,
    url: &str,
) -> Option<serde_json::Value> {
    use crate::local_mcp_bridge::{LocalConnection, write_local_connection};
    if !local_host(issued.connection.host) {
        return None;
    }
    let binary = local_bridge_binary()?;
    let directory = s
        .key_path
        .parent()?
        .canonicalize()
        .ok()?
        .join("mcp-connections");
    let settings = LocalConnection::new(
        issued.connection.id.clone(),
        url.into(),
        issued.token.clone(),
    );
    let path = write_local_connection(&directory, &settings).ok()?;
    Some(local_configuration(&binary, &path, issued.connection.host))
}
#[cfg(unix)]
fn remote_client_setup(
    s: &ReviewServer,
    issued: &IssuedConnection,
    url: &str,
) -> Option<serde_json::Value> {
    use crate::local_mcp_bridge::{LocalConnection, write_local_connection};
    if !remote_host(issued.connection.host) {
        return None;
    }
    let binary = local_bridge_binary()?;
    let directory = s
        .key_path
        .parent()?
        .canonicalize()
        .ok()?
        .join("mcp-connections");
    let settings = LocalConnection::new(
        issued.connection.id.clone(),
        url.into(),
        issued.token.clone(),
    );
    let path = write_local_connection(&directory, &settings).ok()?;
    Some(gateway_configuration(&binary, &path))
}
#[cfg(not(unix))]
fn remote_client_setup(
    _s: &ReviewServer,
    _issued: &IssuedConnection,
    _url: &str,
) -> Option<serde_json::Value> {
    None
}
#[cfg(not(unix))]
fn local_client_setup(
    _s: &ReviewServer,
    _issued: &IssuedConnection,
    _url: &str,
) -> Option<serde_json::Value> {
    None
}

async fn recover_local_setup(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    recover_setup(s, id, bytes, false).await
}

async fn diagnose_connection(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let selection = {
        let _guard = s.lock.lock().await;
        (|| {
            if bytes.as_ref() != b"{}" {
                return Err(NovaError::Validation(
                    "empty diagnostic request required".into(),
                ));
            }
            let service = service(&s)?;
            let store = service.open()?;
            if !store
                .connections(service.scope())?
                .iter()
                .any(|c| c.id == id && !c.revoked)
            {
                return Err(NovaError::PolicyDenied("connection unavailable".into()));
            }
            let parent = s
                .key_path
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .ok_or_else(|| {
                    NovaError::InvalidState("private configuration unavailable".into())
                })?;
            let path = parent.join("mcp-connections").join(format!("{id}.json"));
            let config = crate::local_mcp_bridge::read_local_connection(&path)
                .map_err(|_| NovaError::InvalidState("private configuration unavailable".into()))?;
            let access = store.authenticate_connection(service.scope(), &config.token)?;
            if access.connection_id() != id
                || config.connection_id != id
                || config.endpoint != format!("http://{}/mcp/v03", s.authority)
            {
                return Err(NovaError::PolicyDenied("connection unavailable".into()));
            }
            Ok((path, config))
        })()
    };
    let (path, config) = match selection {
        Ok(selected) => selected,
        Err(error) => return response(Err(error)),
    };
    // Do not hold the Owner lock over network IO: revocation must remain available.
    let check = crate::connection_diagnostics::check(&config).await;
    let _guard = s.lock.lock().await;
    response((|| {
        let service = service(&s)?;
        let store = service.open()?;
        let current = crate::local_mcp_bridge::read_local_connection(&path)
            .map_err(|_| NovaError::PolicyDenied("connection unavailable".into()))?;
        let access = store.authenticate_connection(service.scope(), &current.token)?;
        if current != config || access.connection_id() != id {
            return Err(NovaError::PolicyDenied("connection unavailable".into()));
        }
        Ok(
            json!({"connection_id":id,"diagnostic":check,"bridge_available":local_bridge_binary().is_some(),"authority_changed":false}),
        )
    })())
}
async fn recover_gateway_setup(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    recover_setup(s, id, bytes, true).await
}
async fn recover_setup(s: ReviewServer, id: String, bytes: Bytes, gateway: bool) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty setup request required".into()));
        }
        let service = service(&s)?;
        let store = service.open()?;
        // Select only the Owner's bounded metadata; never turn an arbitrary ID into a file path.
        let connection = store
            .connections(service.scope())?
            .into_iter()
            .find(|row| {
                row.id == id
                    && !row.revoked
                    && if gateway {
                        remote_host(row.host)
                    } else {
                        local_host(row.host)
                    }
            })
            .ok_or_else(|| NovaError::PolicyDenied("local setup unavailable".into()))?;
        let binary = local_bridge_binary()
            .ok_or_else(|| NovaError::InvalidState("local bridge unavailable".into()))?;
        #[cfg(unix)]
        {
            let parent = s
                .key_path
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .ok_or_else(|| NovaError::InvalidState("local setup unavailable".into()))?;
            let path = parent.join("mcp-connections").join(format!("{id}.json"));
            let config = crate::local_mcp_bridge::read_local_connection(&path)
                .map_err(|_| NovaError::InvalidState("local setup unavailable".into()))?;
            let access = store.authenticate_connection(service.scope(), &config.token)?;
            if access.connection_id() != id
                || config.connection_id != id
                || config.endpoint != format!("http://{}/mcp/v03", s.authority)
            {
                return Err(NovaError::PolicyDenied("local setup unavailable".into()));
            }
            let configuration = if gateway {
                json!({"remote_gateway":gateway_configuration(&binary,&path),"contract":"nova-memory-automatic/0.3.0","endpoint_visibility":"loopback_gateway_requires_separate_tunnel","real_host_verification":"not_run"})
            } else {
                json!({"local_stdio":local_configuration(&binary,&path,connection.host),"contract":"nova-memory-automatic/0.3.0","endpoint_visibility":"loopback_only","real_host_verification":"not_run"})
            };
            Ok(
                json!({"connection":connection,"configuration":configuration,"contains_client_credential":false,"credential_reissued":false,"authority_changed":false}),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (connection, binary);
            Err(NovaError::InvalidState("local setup unavailable".into()))
        }
    })())
}

async fn connections(State(s): State<ReviewServer>) -> Response {
    let _guard = s.lock.lock().await;
    let runtimes = s.gateways.statuses().await;
    let tunnels = s.tunnels.statuses().await;
    let https_tunnels = s.quick_tunnels.statuses().await;
    let https_client_available = crate::quick_tunnel::installed_client().is_some();
    let tunnel_client_available = crate::tunnel_runtime::installed_client().is_some();
    response((|| {
        let service = service(&s)?;
        let store = service.open()?;
        let rows = store.connections(service.scope())?;
        let quarantined = store.backup_recovery_manifest(service.scope())?.is_some();
        Ok(
            json!({"items":rows,"gateway_runtimes":runtimes,"tunnel_runtimes":tunnels,"https_tunnel_runtimes":https_tunnels,"https_tunnel_client_available":https_client_available,"tunnel_client_available":tunnel_client_available,"endpoint":format!("http://{}/mcp/v03",s.authority),"service_ready":!quarantined,"local_bridge_ready":local_bridge_binary().is_some()&&!quarantined,"backup_quarantined":quarantined,"host_status":"not_verified_by_readiness_check","capabilities":{"delete":false,"policy_write":false,"source_text_read":false,"capture_ingress":s.capture_ingress&&!quarantined,"continuity":s.continuity_enabled,"ranked_retrieval":s.ranked_retrieval_enabled,"historical_retrieval":s.historical_retrieval_enabled},"limits":{"ttl_seconds":86400,"call_budget":1000,"calls_per_minute":30,"max_context_items":8,"max_context_bytes":32768,"max_scope_members":128}}),
        )
    })())
}
async fn create(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid connection request".into()))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| NovaError::Validation("invalid connection request".into()))?;
        let lifetime = match object.remove("ttl_seconds") {
            None => 86400,
            Some(value) => value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| NovaError::Validation("invalid connection lifetime".into()))?,
        };
        let input: ConnectionRequest = serde_json::from_value(value)
            .map_err(|_| NovaError::Validation("invalid connection request".into()))?;
        let service = service(&s)?;
        let issued =
            service
                .open()?
                .create_connection_with_lifetime(service.scope(), input, lifetime)?;
        let name = format!("nova_memory_{}", issued.connection.id.replace('-', "_"));
        let url = format!("http://{}/mcp/v03", s.authority);
        let auth = format!("Bearer {}", issued.token);
        let codex = format!(
            "[mcp_servers.{name}]\nurl = {}\nhttp_headers = {{ Authorization = {} }}\n",
            json!(url),
            json!(auth)
        );
        let http = json!({"url":url,"headers":{"Authorization":auth}});
        let mcp_json = json!({"mcpServers":{"nova-memory":http}});
        let local_stdio = local_client_setup(&s, &issued, &url);
        let remote_gateway = remote_client_setup(&s, &issued, &url);
        Ok(
            json!({"connection":issued.connection,"configuration":{"codex_toml":codex,"http":http,"mcp_json":mcp_json,"local_stdio":local_stdio,"remote_gateway":remote_gateway,"contract":"nova-memory-automatic/0.3.0","endpoint_visibility":"loopback_only","real_host_verification":"not_run"},"contains_client_credential":true,"credential_shown_once":true}),
        )
    })())
}
async fn revoke(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    let result = (|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty revoke required".into()));
        }
        let service = service(&s)?;
        service.open()?.revoke_connection(service.scope(), &id)?;
        #[cfg(unix)]
        let local_setup_removed = (|| {
            use crate::local_mcp_bridge::read_local_connection;
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return false;
            }
            let Some(parent) = s.key_path.parent().and_then(|p| p.canonicalize().ok()) else {
                return false;
            };
            let path = parent.join("mcp-connections").join(format!("{id}.json"));
            if std::fs::symlink_metadata(&path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return true;
            }
            if !read_local_connection(&path).is_ok_and(|config| config.connection_id == id) {
                return false;
            }
            std::fs::remove_file(path).is_ok()
        })();
        #[cfg(not(unix))]
        let local_setup_removed = false;
        Ok(json!({"revoked":true,"local_setup_removed":local_setup_removed}))
    })();
    if result.is_ok() {
        s.quick_tunnels.stop(&id).await;
        s.tunnels.stop(&id).await;
        s.gateways.stop(&id).await;
    }
    response(result)
}

async fn start_gateway(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    gateway_operation(s, id, bytes, "start").await
}
async fn check_gateway(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    gateway_operation(s, id, bytes, "check").await
}
async fn stop_gateway(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    gateway_operation(s, id, bytes, "stop").await
}
async fn gateway_operation(s: ReviewServer, id: String, bytes: Bytes, operation: &str) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation(
                "empty gateway operation required".into(),
            ));
        }
        let service = service(&s)?;
        let store = service.open()?;
        let owned = store
            .connections(service.scope())?
            .into_iter()
            .any(|row| row.id == id && remote_host(row.host));
        if !owned {
            return Err(NovaError::PolicyDenied(
                "gateway connection unavailable".into(),
            ));
        }
        let parent = s
            .key_path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .ok_or_else(|| NovaError::InvalidState("gateway configuration unavailable".into()))?;
        let path = parent.join("mcp-connections").join(format!("{id}.json"));
        let upstream = format!("http://{}/mcp/v03", s.authority);
        let gateway = match operation {
            "stop" => {
                s.quick_tunnels.stop(&id).await;
                s.tunnels.stop(&id).await;
                s.gateways.stop(&id).await
            }
            "check" => s.gateways.check(&id, &upstream, &service).await?,
            "start" => {
                s.gateways
                    .start(&id, path, &upstream, service.clone())
                    .await?;
                s.gateways.check(&id, &upstream, &service).await?
            }
            _ => {
                return Err(NovaError::Validation(
                    "unsupported gateway operation".into(),
                ));
            }
        };
        Ok(json!({"gateway":gateway,"authority_changed":false,"model_called":false}))
    }
    .await;
    response(result)
}
async fn grok_binding(
    s: &ReviewServer,
    id: String,
) -> Result<crate::tunnel_runtime::TunnelBinding, NovaError> {
    let service = service(s)?;
    if !service
        .open()?
        .connections(service.scope())?
        .into_iter()
        .any(|r| r.id == id && matches!(r.host, ConnectionHost::Grok) && !r.revoked)
    {
        return Err(NovaError::PolicyDenied(
            "Grok connection unavailable".into(),
        ));
    }
    let gateway = s
        .gateways
        .statuses()
        .await
        .into_iter()
        .find(|r| r.connection_id == id && r.phase == "running" && r.upstream_verified)
        .and_then(|r| r.endpoint)
        .ok_or_else(|| {
            NovaError::InvalidState("start and verify the local gateway first".into())
        })?;
    let parent = s
        .key_path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| NovaError::InvalidState("gateway configuration unavailable".into()))?;
    let binding = crate::tunnel_runtime::TunnelBinding {
        id: id.clone(),
        path: parent.join("mcp-connections").join(format!("{id}.json")),
        upstream: format!("http://{}/mcp/v03", s.authority),
        gateway,
        gateways: s.gateways.clone(),
        service,
    };
    crate::tunnel_runtime::current(
        &binding.service,
        &binding.path,
        &binding.id,
        &binding.upstream,
    )?;
    Ok(binding)
}
async fn start_https(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        let input: crate::quick_tunnel::QuickRequest = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid HTTPS exposure request".into()))?;
        input.validate()?;
        let binding = grok_binding(&s, id).await?;
        let tunnel = s.quick_tunnels.start(input, binding).await?;
        Ok(json!({"tunnel":tunnel,"authority_changed":false,"model_called":false}))
    }
    .await;
    response(result)
}
async fn stop_https(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty HTTPS stop required".into()));
        }
        let service = service(&s)?;
        if !service
            .open()?
            .connections(service.scope())?
            .into_iter()
            .any(|r| r.id == id && matches!(r.host, ConnectionHost::Grok))
        {
            return Err(NovaError::PolicyDenied(
                "Grok connection unavailable".into(),
            ));
        }
        s.quick_tunnels.stop(&id).await;
        Ok(json!({"stop_requested":true,"authority_changed":false}))
    }
    .await;
    response(result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpsConfigurationRequest {
    reveal_scoped_key: bool,
}
async fn https_configuration(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        let input: HttpsConfigurationRequest = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid HTTPS configuration request".into()))?;
        if !input.reveal_scoped_key {
            return Err(NovaError::Validation(
                "explicit credential handoff required".into(),
            ));
        }
        let _ = grok_binding(&s, id.clone()).await?;
        s.quick_tunnels.configuration(&id).await
    }
    .await;
    response(result)
}
async fn start_tunnel(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        let request: crate::tunnel_runtime::TunnelRequest = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid tunnel setup".into()))?;
        request.validate()?;
        s.ensure_model_credential_separate(&request.runtime_key)?;
        let service = service(&s)?;
        let owned = service
            .open()?
            .connections(service.scope())?
            .into_iter()
            .any(|row| row.id == id && matches!(row.host, ConnectionHost::Chatgpt) && !row.revoked);
        if !owned {
            return Err(NovaError::PolicyDenied(
                "ChatGPT tunnel connection unavailable".into(),
            ));
        }
        let gateway = s
            .gateways
            .statuses()
            .await
            .into_iter()
            .find(|r| r.connection_id == id && r.phase == "running" && r.upstream_verified)
            .and_then(|r| r.endpoint)
            .ok_or_else(|| {
                NovaError::InvalidState("start and verify the local gateway first".into())
            })?;
        let parent = s
            .key_path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .ok_or_else(|| NovaError::InvalidState("tunnel configuration unavailable".into()))?;
        let binding = crate::tunnel_runtime::TunnelBinding {
            path: parent.join("mcp-connections").join(format!("{id}.json")),
            id,
            upstream: format!("http://{}/mcp/v03", s.authority),
            gateway,
            service,
            gateways: s.gateways.clone(),
        };
        let tunnel = s.tunnels.start(request, binding).await?;
        Ok(json!({"tunnel":tunnel,"authority_changed":false,"model_called":false}))
    }
    .await;
    response(result)
}
async fn stop_tunnel(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    let result = async {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty tunnel stop required".into()));
        }
        let service = service(&s)?;
        if !service
            .open()?
            .connections(service.scope())?
            .into_iter()
            .any(|r| r.id == id && matches!(r.host, ConnectionHost::Chatgpt))
        {
            return Err(NovaError::PolicyDenied(
                "ChatGPT tunnel connection unavailable".into(),
            ));
        }
        s.tunnels.stop(&id).await;
        Ok(json!({"stop_requested":true,"authority_changed":false}))
    }
    .await;
    response(result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<i64>,
}
async fn usage(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let service = service(&s)?;
        let rows = service
            .open()?
            .connection_usage(service.scope(), page.before)?;
        let next = if rows.len() == 50 {
            rows.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        Ok(
            json!({"items":rows.into_iter().map(|(_,event)|event).collect::<Vec<_>>(),"next_before":next}),
        )
    })())
}
async fn overview(State(s): State<ReviewServer>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let service = service(&s)?;
        service.open()?.owner_inventory(service.scope())
    })())
}

async fn graph(State(s): State<ReviewServer>, Query(page): Query<GraphRequest>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let service = service(&s)?;
        Ok(serde_json::to_value(
            service.open()?.owner_graph(service.scope(), &page)?,
        )?)
    })())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportQuery {}

async fn report(State(s): State<ReviewServer>, Query(_): Query<ReportQuery>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let service = service(&s)?;
        let store = service.open()?;
        let construction = store.construction_report(service.scope())?;
        let rows = store.task_evaluations(service.scope(), &construction.content_snapshot_id)?;
        Ok(json!({"construction":construction,"task_evaluations":{"items":rows}}))
    })())
}

async fn import_task_evaluation(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let submission: TaskEvaluationSubmission = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid task evaluation artifact".into()))?;
        let service = service(&s)?;
        let result = service
            .open()?
            .record_task_evaluation(service.scope(), &submission)?;
        Ok(json!({"evaluation":result}))
    })())
}
