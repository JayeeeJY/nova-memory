pub mod compiler;
pub mod provider_capabilities;
pub mod compiler_schema;
pub mod quote_text_schema;
pub mod generation_guard;
pub mod quote_text;
pub mod acceptance;
pub mod structured_acceptance;
pub mod structured_suite;
pub mod capture_hook;
pub mod local_mcp_bridge;
pub mod local_mcp_gateway;
mod gateway_runtime;
mod connection_diagnostics;
mod tunnel_runtime;
mod quick_tunnel;
mod context_pack;
mod console;
mod intake;
mod native;
mod capture_control;
mod continuity_control;
mod history_control;
mod maintenance_control;
mod diagnostics_control;
mod workspace;
mod workspace_access;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use nova_core::{NovaError, ReviewRequest, Vault, VaultKey};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct ReviewServer {
    vault_path: PathBuf,
    key_path: PathBuf,
    authority: String,
    demo: bool,
    owner: String,
    space: String,
    owner_token: Option<nova_mcp::transport::ClientCredential>,
    registry: Option<PathBuf>,
    site_dir: Option<PathBuf>,
    intake_store: Option<PathBuf>,
    temporary_demo: bool,
    native_workspace: bool,
    local_access: Option<Arc<workspace_access::LocalAccess>>,
    capture_ingress: bool,
    local_runtime_instance: Option<String>,
    continuity_enabled: bool,
    ranked_retrieval_enabled: bool,
    historical_retrieval_enabled: bool,
    model_runtime: Option<compiler::ModelRuntime>,
    worker_queue_cursor: Arc<tokio::sync::Mutex<Option<nova_core::intake::WorkerQueueCursor>>>,
    lock: Arc<tokio::sync::Mutex<()>>,
    gateways: Arc<gateway_runtime::GatewayManager>,
    tunnels: Arc<tunnel_runtime::TunnelManager>,
    quick_tunnels: Arc<quick_tunnel::QuickManager>,
}

impl ReviewServer {
    pub fn new(vault_path: PathBuf, key_path: PathBuf, authority: String, demo: bool) -> Self {
        Self {
            vault_path,
            key_path,
            authority,
            demo,
            owner: "owner:founder".into(),
            space: "personal".into(),
            owner_token: None,
            registry: None,
            site_dir: None,
            intake_store: None,
            temporary_demo: false,
            native_workspace: false,
            local_access: None,
            capture_ingress: false,
            local_runtime_instance: None,
            continuity_enabled: false,
            ranked_retrieval_enabled: false,
            historical_retrieval_enabled: false,
            model_runtime: None,
            worker_queue_cursor: Arc::new(tokio::sync::Mutex::new(None)),
            lock: Arc::new(tokio::sync::Mutex::new(())),
            gateways: Arc::new(gateway_runtime::GatewayManager::default()),
            tunnels: Arc::new(tunnel_runtime::TunnelManager::default()),
            quick_tunnels: Arc::new(quick_tunnel::QuickManager::default()),
        }
    }
    pub fn with_owner(mut self, owner: String, space: String) -> Self {
        self.local_access = None;
        // A changed store/scope must not share managed listeners with the old builder.
        self.gateways = Arc::new(gateway_runtime::GatewayManager::default());
        self.tunnels = Arc::new(tunnel_runtime::TunnelManager::default());
        self.quick_tunnels = Arc::new(quick_tunnel::QuickManager::default());
        self.worker_queue_cursor = Arc::new(tokio::sync::Mutex::new(None));
        self.owner = owner;
        self.space = space;
        self
    }
    /// Stop only the listeners owned by this workspace. Never discovers or kills other processes.
    pub async fn stop_local_gateways(&self) {
        self.quick_tunnels.stop_all().await;
        self.tunnels.stop_all().await;
        self.gateways.stop_all().await;
    }
    pub fn with_registry(mut self, path: PathBuf) -> Self {
        self.registry = Some(path);
        self
    }
    /// Serve the shared website UI from an explicit public directory only.
    pub fn with_site_dir(mut self, path: &std::path::Path) -> Result<Self, NovaError> {
        let root = path.canonicalize()?;
        if !root.is_dir() || !root.join("console/index.html").is_file() {
            return Err(NovaError::Validation(
                "site directory must contain console/index.html".into(),
            ));
        }
        self.site_dir = Some(root);
        Ok(self)
    }
    pub fn with_owner_token(mut self, path: &std::path::Path) -> Result<Self, NovaError> {
        self.owner_token = Some(nova_mcp::transport::ClientCredential::read_for_vault(
            path,
            &self.key_path,
        )?);
        Ok(self)
    }
    /// Explicit opt-in to processing the configured intake store. Never enables itself at open.
    pub fn with_model_runtime(
        mut self,
        runtime: compiler::ModelRuntime,
    ) -> Result<Self, NovaError> {
        runtime.validate()?;
        if let Some(path) = &runtime.credential_file {
            let credential = path.canonicalize()?;
            if credential == self.key_path.canonicalize()? {
                return Err(NovaError::PolicyDenied(
                    "model credential must differ from Vault key".into(),
                ));
            }
            let candidate = zeroize::Zeroizing::new(std::fs::read_to_string(path)?);
            self.ensure_model_credential_separate(candidate.trim())?;
        }
        let scope = nova_core::compilation::AssetScope {
            owner_id: self.owner.clone(),
            space: self.space.clone(),
        };
        let mut intake = nova_core::intake::IntakeStore::open(
            self.intake_store
                .as_ref()
                .ok_or_else(|| NovaError::Validation("compiler requires intake store".into()))?,
            VaultKey::read_file(&self.key_path)?,
            scope.clone(),
        )?;
        if intake.backup_recovery_manifest(&scope)?.is_some() {
            return Err(NovaError::PolicyDenied("backup requires trusted lifecycle reconciliation before model runtime".into()));
        }
        intake.enable_compilation(&scope)?;
        self.model_runtime = Some(runtime);
        Ok(self)
    }
    fn ensure_model_credential_separate(&self, value: &str) -> Result<(), NovaError> {
        let vault = zeroize::Zeroizing::new(VaultKey::read_file(&self.key_path)?.to_hex());
        let bearer = zeroize::Zeroizing::new(format!("Bearer {value}"));
        if value.eq_ignore_ascii_case(vault.as_str())
            || self
                .owner_token
                .as_ref()
                .is_some_and(|t| t.permits(Some(bearer.as_str())))
        {
            return Err(NovaError::PolicyDenied(
                "model credential must differ from Vault and Owner credentials".into(),
            ));
        }
        if let Some(path) = &self.intake_store {
            let scope = nova_core::compilation::AssetScope {
                owner_id: self.owner.clone(),
                space: self.space.clone(),
            };
            if nova_core::intake::IntakeStore::open(
                path,
                VaultKey::read_file(&self.key_path)?,
                scope.clone(),
            )?
            .contains_connection_credential(&scope, value)?
            {
                return Err(NovaError::PolicyDenied(
                    "model credential must differ from connection credentials".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn start_compiler(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.model_runtime
            .as_ref()
            .map(|_| compiler::start(self.clone()))
    }
    /// Distinguish disposable CLI demos from persistent synthetic workspaces.
    pub fn with_temporary_demo(mut self, temporary: bool) -> Self {
        self.temporary_demo = temporary;
        self
    }
    /// Opt in to an already initialized, separate automatic-intake store.
    pub fn with_intake_store(mut self, path: &std::path::Path) -> Result<Self, NovaError> {
        self.local_access = None;
        self.gateways = Arc::new(gateway_runtime::GatewayManager::default());
        self.tunnels = Arc::new(tunnel_runtime::TunnelManager::default());
        self.quick_tunnels = Arc::new(quick_tunnel::QuickManager::default());
        nova_core::intake::IntakeStore::open(
            path,
            VaultKey::read_file(&self.key_path)?,
            nova_core::compilation::AssetScope {
                owner_id: self.owner.clone(),
                space: self.space.clone(),
            },
        )?;
        self.worker_queue_cursor = Arc::new(tokio::sync::Mutex::new(None));
        self.intake_store = Some(path.to_path_buf());
        Ok(self)
    }
    pub fn with_native_workspace(mut self) -> Result<Self, NovaError> {
        if self.owner_token.is_none() {
            return Err(NovaError::PolicyDenied(
                "native workspace requires independent Owner credential".into(),
            ));
        }
        self.native_workspace = true;
        if !self.authority.parse::<std::net::SocketAddr>().is_ok_and(|a| a.ip().is_loopback()) {
            return Err(NovaError::PolicyDenied("本机工作台只能绑定回环地址。".into()));
        }
        self.local_access = Some(Arc::new(workspace_access::LocalAccess::open(
            self.intake_store.as_ref().ok_or_else(|| NovaError::Validation("intake store required".into()))?, &self.owner, &self.space, self.authority.parse::<std::net::SocketAddr>().unwrap().port(),
        )?));
        let service = native::service(&self)?;
        let mut store=service.open()?;
        if store.backup_recovery_manifest(service.scope())?.is_none() { store.enable_connections(service.scope())?; }
        Ok(self)
    }
    /// Public instance binding for a self-owned local supervisor. This identifier
    /// never grants authority; it lets the supervisor verify the intended child
    /// before sending the separate Owner credential to a loopback health endpoint.
    pub fn with_local_runtime_instance(mut self, instance: String) -> Result<Self, NovaError> {
        let loopback = self.authority.parse::<std::net::SocketAddr>()
            .is_ok_and(|address| address.ip().is_loopback());
        if !self.native_workspace || !loopback || instance.len() != 32
            || !instance.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(NovaError::Validation("invalid local runtime instance".into()));
        }
        self.local_runtime_instance = Some(instance);
        Ok(self)
    }
    /// Enables a separately authenticated capture ingress, never grants authority by itself.
    pub fn with_capture_ingress(mut self)->Result<Self,NovaError> {
        let service=native::service(&self)?;
        let mut store=service.open()?;
        if store.backup_recovery_manifest(service.scope())?.is_none() { store.enable_capture(service.scope())?; self.capture_ingress=true; }
        Ok(self)
    }
    pub fn with_continuity(mut self)->Result<Self,NovaError> {
        let service=native::service(&self)?;
        let mut store=service.open()?;
        if store.backup_recovery_manifest(service.scope())?.is_none() { store.enable_continuity(service.scope())?; self.continuity_enabled=true; }
        Ok(self)
    }
    pub fn with_ranked_retrieval(mut self)->Result<Self,NovaError> {
        native::service(&self)?;
        self.ranked_retrieval_enabled=true;Ok(self)
    }
    pub fn with_historical_retrieval(mut self)->Result<Self,NovaError> {
        native::service(&self)?;
        self.historical_retrieval_enabled=true;Ok(self)
    }
    fn open(&self) -> Result<Vault, NovaError> {
        Vault::open(&self.vault_path, VaultKey::read_file(&self.key_path)?)
    }
    fn credentials_separate(&self) -> Result<(), NovaError> {
        if let (Some(token), Some(_)) = (&self.owner_token, &self.registry) {
            let r = console::read_registry(self)?;
            for e in r["clients"].as_array().unwrap() {
                let p = e["token_file"]
                    .as_str()
                    .ok_or_else(|| NovaError::Validation("invalid registry".into()))?;
                let client_token = std::fs::read_to_string(p)?;
                if token.permits(Some(&format!("Bearer {}", client_token.trim()))) {
                    return Err(NovaError::PolicyDenied(
                        "owner credential must be distinct from every client credential".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn verify(&self) -> Result<(), NovaError> {
        self.open()?;
        self.credentials_separate()
    }
    pub fn router(self) -> Router {
        let capture = if self.capture_ingress {
            Some(nova_mcp::capture::router(native::service(&self).expect("validated capture service"),self.authority.clone()))
        }else{None};
        let mcp = if self.native_workspace {
            Some(nova_mcp::automatic::router(
                native::service(&self).expect("validated native service"),
                self.authority.clone(),
            ))
        } else {
            None
        };
        let owner = Router::new()
            .route("/", get(index))
            .route("/review.css", get(css))
            .route("/review.js", get(js))
            .route("/install", get(install))
            .route("/app/about", get(about))
            .route("/app/license.txt", get(license_text))
            .route("/session", get(session_info))
            .route("/api/capture", post(console::capture))
            .route(
                "/api/collections",
                get(console::collections).post(console::collection),
            )
            .route("/api/collection-grants", post(console::grant))
            .route("/api/assets/{id}", get(console::detail))
            .route("/api/assets/mutate", post(console::mutate))
            .route("/api/clients", get(console::clients).post(console::client))
            .route("/api/connection", post(console::connection))
            .route("/api/backup", post(console::backup))
            .route("/api/restore", post(console::restore))
            .route("/api/inbox", get(inbox))
            .route("/api/assets", get(assets))
            .route("/api/access", get(access))
            .route("/api/recovery", get(recovery))
            .route("/api/access/revoke", post(revoke))
            .route("/api/review", post(review))
            .merge(workspace::routes())
            .merge(workspace_access::routes())
            .merge(intake::routes())
            .merge(native::routes())
            .merge(context_pack::routes())
            .merge(capture_control::routes())
            .merge(continuity_control::routes())
            .merge(history_control::routes())
            .merge(maintenance_control::routes())
            .merge(diagnostics_control::routes())
            .fallback(workspace::static_file)
            .layer(axum::extract::DefaultBodyLimit::max(96 * 1024))
            .layer(middleware::from_fn_with_state(self.clone(), boundary))
            .with_state(self);
        let app=if let Some(mcp)=mcp {owner.merge(mcp)}else{owner};
        if let Some(capture)=capture {app.merge(capture)}else{app}
    }
}

async fn boundary(State(state): State<ReviewServer>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let expected_origin = format!("http://{}", state.authority);
    let host_valid =
        headers.get(header::HOST).and_then(|v| v.to_str().ok()) == Some(&state.authority);
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let api = request.uri().path().starts_with("/api/")
        || request.uri().path().starts_with("/console/api/");
    let mutation = request.method() != axum::http::Method::GET;
    let valid = host_valid
        && !matches!(site, Some("cross-site" | "same-site"))
        && (!headers.contains_key(header::ORIGIN) || origin == Some(expected_origin.as_str()))
        && (!api
            || headers.get("x-nova-review").and_then(|v| v.to_str().ok()) == Some("owner-review"))
        && (!mutation || origin == Some(expected_origin.as_str()))
        && (!api || state.credentials_separate().is_ok())
        && (!api || state.owner_token.as_ref().is_none_or(|token| {
            if headers.contains_key(header::AUTHORIZATION) {
                token.permits(headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()))
            } else {
                request.uri().path().starts_with("/console/api/owner/")
                    && state.local_access.as_ref().is_some_and(|access| access.permits_cookie(headers))
            }
        }));
    let mut response = if valid {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(json!({"error": {"code": "NOVA_REVIEW_BOUNDARY_DENIED",
            "message": "请从本机审核页面操作。"}})),
        )
            .into_response()
    };
    let out = response.headers_mut();
    out.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    out.insert("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    out.insert("x-content-type-options", "nosniff".parse().unwrap());
    out.insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}

async fn index(State(s): State<ReviewServer>) -> Response {
    if s.site_dir.is_some() {
        workspace::serve_static(&s, "/").await
    } else {
        Html(include_str!("index.html")).into_response()
    }
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("review.css"),
    )
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("review.js"),
    )
}

fn failure(error: NovaError) -> Response {
    let status = match error {
        NovaError::InvalidState(_) => StatusCode::CONFLICT,
        NovaError::Validation(_) => StatusCode::BAD_REQUEST,
        NovaError::PolicyDenied(_) => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = match status {
        StatusCode::CONFLICT => "草稿状态已变化，请刷新后重新核对。",
        StatusCode::BAD_REQUEST => "请核对输入长度、必填内容和审核说明。",
        StatusCode::FORBIDDEN => "当前用户无权审核此草稿。",
        _ => "未能确认本次操作结果，请刷新核对；不要直接重复确认。",
    };
    (
        status,
        Json(json!({"error": {"code": error.code(), "message": message}})),
    )
        .into_response()
}

async fn inbox(State(state): State<ReviewServer>) -> Response {
    let _guard = state.lock.lock().await;
    match state
        .open()
        .and_then(|vault| vault.review_inbox(&state.owner))
    {
        Ok(items) => Json(json!({"items": items, "demo": state.demo})).into_response(),
        Err(error) => failure(error),
    }
}

async fn assets(State(state): State<ReviewServer>) -> Response {
    let _guard = state.lock.lock().await;
    match state.open().and_then(|vault| {
        // Fail closed on a broken audit chain. This owner view issues no client read grant.
        let audit = vault.verify_audit_chain()?;
        let items: Vec<_> = vault.memories()?.into_iter()
            .filter(|memory| memory.owner_id == state.owner)
            .map(|memory| json!({
                "memory_id": memory.memory_id, "title": memory.title,
                "summary": memory.summary, "memory_type": memory.memory_type,
                "status": memory.status, "assertion_origin": memory.assertion_origin,
                "privacy_level": memory.privacy_level, "source_platform": memory.source_platform,
                "source_artifact_ids": memory.source_artifact_ids,
                "created_at": memory.created_at, "updated_at": memory.updated_at,
                "supersedes": memory.supersedes, "superseded_by": memory.superseded_by,
                "conflicts_with": memory.conflicts_with,
                "permission_scope": memory.permission_scope,
                "purpose_limits": memory.purpose_limits
                ,"observed_at": memory.observed_at,"valid_from": memory.valid_from,"valid_to": memory.valid_to,
                "structure": memory.structure
            })).collect();
        Ok(json!({"items": items, "memories": items, "audit": audit, "demo": state.demo}))
    }) {
        Ok(data) => Json(data).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request_id: String,
    proposal_id: String,
    expected_snapshot: String,
    action: nova_core::ReviewAction,
    #[serde(default)]
    user_approved: Option<bool>,
}

async fn recovery(State(state): State<ReviewServer>) -> Response {
    let _guard = state.lock.lock().await;
    match state
        .open()
        .and_then(|vault| vault.owner_recovery_overview(&state.owner))
    {
        Ok(mut data) => {
            data["demo"] = json!(state.demo);
            if let Ok(path) = std::env::current_exe() {
                data["tool_directory"] = json!(path.parent().map(|p| p.to_string_lossy()));
            }
            Json(data).into_response()
        }
        Err(error) => failure(error),
    }
}

async fn access(State(state): State<ReviewServer>) -> Response {
    let _guard = state.lock.lock().await;
    match state
        .open()
        .and_then(|vault| {
            let mut result=vault.owner_access_overview(&state.owner)?;
            let collections=vault.owner_collections(&state.owner)?;
            if let Some(approvals)=result["approvals"].as_array_mut() {
                for row in collections["grants"].as_array().unwrap() {let g=&row["grant"];approvals.push(json!({"approval_id":g["approval_id"],"client":g["client"],"purpose":g["purpose"],"allowed_ids":g["member_snapshot"],"maximum_privacy":g["maximum_privacy"],"expires_at":g["expires_at"],"status":row["status"],"used_calls":row["used_calls"],"call_quota":g["call_quota"]}));}
            }
            Ok(result)
        })
    {
        Ok(mut data) => {
            data["demo"] = json!(state.demo);
            Json(data).into_response()
        }
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeInput {
    approval_id: String,
    #[serde(alias = "user_approved")]
    acknowledge: bool,
}
async fn revoke(State(state): State<ReviewServer>, Json(input): Json<RevokeInput>) -> Response {
    if !input.acknowledge || input.approval_id.is_empty() || input.approval_id.len() > 128 {
        return failure(NovaError::Validation(
            "explicit revocation acknowledgement required".into(),
        ));
    }
    let _guard = state.lock.lock().await;
    match state.open().and_then(|mut vault| {
        vault.verify_audit_chain()?;
        vault.revoke_disclosure_grant(&state.owner, &input.approval_id)
    }) {
        Ok(()) => {
            Json(json!({"status": "revoked", "approval_id": input.approval_id})).into_response()
        }
        Err(error) => failure(error),
    }
}
async fn review(State(state): State<ReviewServer>, Json(input): Json<Input>) -> Response {
    if input.user_approved == Some(false) {
        return failure(NovaError::PolicyDenied(
            "owner acknowledgement required".into(),
        ));
    }
    let _guard = state.lock.lock().await;
    match state.open().and_then(|mut vault| {
        vault.review_proposal(ReviewRequest {
            request_id: Some(input.request_id),
            subject: state.owner.clone(),
            proposal_id: input.proposal_id,
            expected_snapshot: input.expected_snapshot,
            action: input.action,
        })
    }) {
        Ok(receipt) => {
            Json(json!({"status": "committed_local", "receipt": receipt})).into_response()
        }
        Err(error) => failure(error),
    }
}

async fn install() -> Html<&'static str> {
    Html(include_str!("install.html"))
}

async fn about() -> Html<&'static str> {
    Html(include_str!("about.html"))
}

async fn license_text() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        include_str!("../../../LICENSE"),
    )
}

async fn session_info(State(s): State<ReviewServer>) -> Json<serde_json::Value> {
    Json(workspace::session_metadata(&s))
}
