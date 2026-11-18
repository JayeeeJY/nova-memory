//! Local adapter for the shared Workspace UI. No hosted account or cloud session is created.
use super::*;
use axum::{
    extract::{OriginalUri, Path},
    http::Method,
};
use nova_core::{StructureField, StructureTemplate};
use serde_json::Value;
use std::{collections::BTreeMap, path::Component};
use zeroize::Zeroize;

pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/console/session", get(session_info))
        .route("/console/api/owner/inbox", get(inbox))
        .route("/console/api/owner/review", post(review_approved))
        .route("/console/api/owner/capture", post(console::capture))
        .route("/console/api/owner/assets", get(assets))
        .route("/console/api/owner/assets/{id}", get(console::detail))
        .route("/console/api/owner/assets/mutate", post(console::mutate))
        .route(
            "/console/api/owner/collections",
            get(console::collections).post(console::collection),
        )
        .route(
            "/console/api/owner/collections/grant",
            post(collection_grant),
        )
        .route(
            "/console/api/owner/clients",
            get(clients).post(client_register),
        )
        .route("/console/api/owner/clients/revoke", post(client_revoke))
        .route("/console/api/owner/client-audit", get(client_audit))
        .route("/console/api/owner/connection", post(console::connection))
        .route("/console/api/owner/access", get(access_overview))
        .route("/console/api/owner/grants/revoke", post(revoke))
        .route("/console/api/owner/profile", get(profile))
        .route("/console/api/owner/recovery", get(recovery))
        .route("/console/api/owner/backup", post(console::backup))
        .route("/console/api/owner/restore", post(console::restore))
        .route("/console/api/owner/export", post(export))
        .route(
            "/console/api/owner/templates",
            get(templates).post(put_template),
        )
        .route(
            "/console/api/owner/templates/{id}/versions",
            get(template_versions),
        )
        .route(
            "/console/api/owner/templates/validate",
            post(validate_template),
        )
}

async fn review_approved(State(s): State<ReviewServer>, Json(i): Json<Input>) -> Response {
    if i.user_approved != Some(true) {
        return failure(NovaError::PolicyDenied(
            "owner acknowledgement required".into(),
        ));
    }
    review(State(s), Json(i)).await
}

async fn access_overview(State(s): State<ReviewServer>) -> Response {
    let _lock = s.lock.lock().await;
    match s.open().and_then(|v| v.owner_access_overview(&s.owner)) {
        Ok(data) => Json(data).into_response(),
        Err(e) => failure(e),
    }
}

pub(super) fn session_metadata(s: &ReviewServer) -> Value {
    json!({"mode":"local", "requires_owner_token":s.owner_token.is_some(), "demo":s.demo,
    "credential_unlock_enabled":s.local_access.as_ref().map(|a|a.preference().map(|p|p.credential_unlock_enabled).unwrap_or(true)),
    "owner":s.owner,"space":s.space,"storage_mode":"local_encrypted_vault","local_runtime_instance":s.local_runtime_instance,
    "model_processing":"none", "capabilities":{
        "automatic_intake":s.intake_store.is_some(),"native_workspace":s.native_workspace,"continuity":s.continuity_enabled,"ranked_retrieval":s.ranked_retrieval_enabled,"capture":true,"review":true,"templates":true,"template_validation":true,
        "collections":true,"client_registry":s.registry.is_some(),"client_rotation":false,
        "encrypted_export":true,"local_backup":true,"restore_to_new_target":true,
        "cloud_account":false,"sync":false,"model_extraction":false
    }})
}

async fn profile(State(s): State<ReviewServer>) -> Json<Value> {
    Json(
        json!({"mode":"local","owner_id":s.owner,"space":s.space,"demo":s.demo,
        "storage":"local_encrypted_vault","processing":"local_declarative_rules",
        "cloud_account":false,"mcp_endpoint":null,"mcp_status":"separate_service_not_verified"}),
    )
}

pub(super) async fn static_file(
    State(s): State<ReviewServer>,
    method: Method,
    uri: OriginalUri,
) -> Response {
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    serve_static(&s, uri.path()).await
}

pub(super) async fn serve_static(s: &ReviewServer, uri: &str) -> Response {
    if s.native_workspace && matches!(uri, "/app" | "/app/") {
        return super::native::page();
    }
    let Some(root) = &s.site_dir else {
        if matches!(uri, "/app" | "/app/") {
            return Html(include_str!("index.html")).into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    if matches!(uri, "/vault" | "/vault/" | "/demo/vault" | "/demo/vault/") {
        return (
            StatusCode::PERMANENT_REDIRECT,
            [(header::LOCATION, "/demo/workspace/")],
        )
            .into_response();
    }
    let relative = match uri {
        "/" => "index.html",
        "/app" | "/app/" => "app/index.html",
        "/console" | "/console/" => "console/index.html",
        "/demo/workspace" | "/demo/workspace/" => "demo/workspace/index.html",
        _ => uri.trim_start_matches('/'),
    };
    // Only public asset types, normal path components and canonical files within the explicit root.
    // In particular, a symlink cannot expose a Vault, key, registry or arbitrary local file.
    let relative = std::path::Path::new(relative);
    if relative
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mime = match relative.extension().and_then(|s| s.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Ok(path) = root.join(relative).canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !path.starts_with(root) || !path.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if std::fs::metadata(&path).map_or(true, |m| m.len() > 16 * 1024 * 1024) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(path) {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn clients(State(s): State<ReviewServer>) -> Response {
    if s.registry.is_none() {
        return Json(json!({"clients":[],"configured":false})).into_response();
    }
    let _lock = s.lock.lock().await;
    match console::read_registry(&s).and_then(|registry| {
        let rows = registry["clients"].as_array().ok_or_else(|| NovaError::Validation("invalid registry".into()))?;
        let clients: Vec<_> = rows.iter().filter(|e| e["principal"]["owner_id"] == s.owner && e["principal"]["space_id"] == s.space)
            .map(|e| {
                let expires = e["expires_at"].as_str().and_then(|v| nova_time::OffsetDateTime::parse(v, &nova_time::format_description::well_known::Rfc3339).ok()).map(|t| t.unix_timestamp());
                json!({"credential_id":e["principal"]["client_id"],"label":e["principal"]["client_id"],
                    "capabilities":e["principal"]["capabilities"],"expires_at":expires,"revoked":e["revoked"],"connection":"not_verified"})
            }).collect();
        Ok(json!({"clients":clients,"configured":true,"mcp_endpoint":null}))
    }) { Ok(v) => Json(v).into_response(), Err(e) => failure(e) }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRegistration {
    label: String,
    capabilities: Vec<String>,
    ttl_seconds: i64,
    user_approved: bool,
}
async fn client_register(
    State(s): State<ReviewServer>,
    Json(i): Json<ClientRegistration>,
) -> Response {
    let input = serde_json::from_value(
        json!({"client_id":i.label,"operation":"register","capabilities":i.capabilities,"ttl_seconds":i.ttl_seconds,"acknowledge":i.user_approved}),
    );
    match input {
        Ok(input) => console::client(State(s), Json(input)).await,
        Err(e) => failure(e.into()),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRevocation {
    credential_id: String,
    user_approved: bool,
}
async fn client_revoke(State(s): State<ReviewServer>, Json(i): Json<ClientRevocation>) -> Response {
    let input = serde_json::from_value(
        json!({"client_id":i.credential_id,"operation":"revoke","capabilities":["query"],"ttl_seconds":1,"acknowledge":i.user_approved}),
    );
    match input {
        Ok(input) => console::client(State(s), Json(input)).await,
        Err(e) => failure(e.into()),
    }
}
async fn client_audit() -> Json<Value> {
    Json(json!({"events":[],"available":false,"reason":"registry_has_no_durable_event_log"}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantInput {
    collection_id: String,
    credential_id: String,
    purpose: String,
    ttl_seconds: i64,
    per_call_limit: usize,
    call_quota: usize,
    maximum_privacy: nova_core::PrivacyLevel,
    #[serde(default)]
    include_future_members: bool,
    user_approved: bool,
}
async fn collection_grant(State(s): State<ReviewServer>, Json(i): Json<GrantInput>) -> Response {
    let input = serde_json::from_value(json!({"acknowledge":i.user_approved,"request":{
        "collection_id":i.collection_id,"client":i.credential_id,"purpose":i.purpose,"ttl_seconds":i.ttl_seconds,
        "per_call_limit":i.per_call_limit,"call_quota":i.call_quota,"maximum_privacy":i.maximum_privacy,
        "include_future_members":i.include_future_members}}));
    match input {
        Ok(input) => console::grant(State(s), Json(input)).await,
        Err(e) => failure(e.into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportInput {
    package_key: String,
    user_approved: bool,
}
async fn export(State(s): State<ReviewServer>, Json(mut i): Json<ExportInput>) -> Response {
    if !i.user_approved {
        i.package_key.zeroize();
        return failure(NovaError::PolicyDenied(
            "owner acknowledgement required".into(),
        ));
    }
    let key = VaultKey::from_hex(&i.package_key);
    i.package_key.zeroize();
    let _lock = s.lock.lock().await;
    match key.and_then(|key| s.open()?.export_encrypted(&s.owner, &key)) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"nova-memory.nmv\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateInput {
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
    title: String,
    memory_type: nova_core::MemoryType,
    privacy_level: nova_core::PrivacyLevel,
    #[serde(default)]
    guidance: String,
    #[serde(default)]
    fields: Vec<StructureField>,
    user_approved: bool,
}
async fn templates(State(s): State<ReviewServer>) -> Response {
    let _lock = s.lock.lock().await;
    match s.open().and_then(|v| v.owner_templates(&s.owner)) {
        Ok(templates) => Json(json!({"templates":templates})).into_response(),
        Err(e) => failure(e),
    }
}
async fn put_template(State(s): State<ReviewServer>, Json(i): Json<TemplateInput>) -> Response {
    if !i.user_approved {
        return failure(NovaError::PolicyDenied(
            "owner acknowledgement required".into(),
        ));
    }
    let _lock = s.lock.lock().await;
    let template = StructureTemplate {
        template_id: i.template_id.unwrap_or_default(),
        owner_id: s.owner.clone(),
        revision: 0,
        title: i.title,
        memory_type: i.memory_type,
        privacy_level: i.privacy_level,
        guidance: i.guidance,
        fields: i.fields,
    };
    match s
        .open()
        .and_then(|mut v| v.put_template(&s.owner, template, i.expected_revision))
    {
        Ok(template) => Json(json!(template)).into_response(),
        Err(e) => failure(e),
    }
}
async fn template_versions(State(s): State<ReviewServer>, Path(id): Path<String>) -> Response {
    let _lock = s.lock.lock().await;
    match s
        .open()
        .and_then(|v| v.owner_template_versions(&s.owner, &id))
    {
        Ok(versions) => Json(json!({"versions":versions})).into_response(),
        Err(e) => failure(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationInput {
    template: Value,
    source_content: String,
    values: BTreeMap<String, Value>,
    evidence: BTreeMap<String, String>,
}
async fn validate_template(
    State(s): State<ReviewServer>,
    Json(mut i): Json<ValidationInput>,
) -> Response {
    if i.source_content.len() > 65536 || !i.template.is_object() {
        return failure(NovaError::Validation("invalid validation bounds".into()));
    }
    i.template["owner_id"] = json!(s.owner);
    i.template["template_id"] = i
        .template
        .get("template_id")
        .filter(|v| v.is_string())
        .cloned()
        .unwrap_or(json!(""));
    i.template["revision"] = i
        .template
        .get("revision")
        .filter(|v| v.is_u64())
        .cloned()
        .unwrap_or(json!(0));
    match serde_json::from_value::<StructureTemplate>(i.template)
        .map_err(NovaError::from)
        .and_then(|template| template.validate_sample(&i.source_content, &i.values, &i.evidence))
    {
        Ok(result) => Json(json!(result)).into_response(),
        Err(e) => failure(e),
    }
}
