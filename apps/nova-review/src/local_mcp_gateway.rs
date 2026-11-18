//! Optional MCP-only loopback target for an independently configured authenticated tunnel.
//! No Owner routes, arbitrary destinations, tunnel management, or new client authority.
use crate::local_mcp_bridge::{
    LocalConnection, MAX_REQUEST, MAX_RESPONSE, forward, http_client, read_local_connection,
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

pub(crate) type DelegatedCredential = Arc<Mutex<Option<Zeroizing<String>>>>;

const MAX_IN_FLIGHT: usize = 8;
const BASIC_TOOLS: [&str; 3] = ["save_memory", "get_memory", "search_memory"];

struct Gateway {
    path: PathBuf,
    pinned: LocalConnection,
    authority: String,
    client: reqwest::Client,
    slots: Semaphore,
    active: Arc<AtomicBool>,
    delegated: DelegatedCredential,
}

fn denied(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

fn credential_matches(s: &Gateway, value: &str) -> bool {
    let Some(token) = value.strip_prefix("Bearer ") else {
        return false;
    };
    let matches = |expected: &str| {
        token.len() == expected.len()
            && token
                .bytes()
                .zip(expected.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0
    };
    matches(&s.pinned.token)
        || s.delegated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|value| matches(value))
}

async fn guard(State(s): State<Arc<Gateway>>, request: Request, next: Next) -> Response {
    let h = request.headers();
    let mut response = if request.uri().path() != "/mcp" || request.uri().query().is_some() {
        denied(StatusCode::NOT_FOUND, "NOVA_GATEWAY_MCP_ONLY")
    } else if h.get_all(header::HOST).iter().count() != 1
        || h.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(s.authority.as_str())
        || h.contains_key(header::ORIGIN)
        || h.contains_key(header::COOKIE)
        || h.contains_key("x-nova-review")
        || matches!(
            h.get("sec-fetch-site").and_then(|v| v.to_str().ok()),
            Some("cross-site" | "same-site")
        )
    {
        denied(StatusCode::FORBIDDEN, "NOVA_GATEWAY_BOUNDARY_DENIED")
    } else if h.contains_key("mcp-session-id") {
        denied(StatusCode::BAD_REQUEST, "NOVA_GATEWAY_STATELESS_ONLY")
    } else if h.get_all(header::AUTHORIZATION).iter().count() != 1
        || !h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| credential_matches(&s, v))
        || !read_local_connection(&s.path).is_ok_and(|config| config == s.pinned)
    {
        denied(
            StatusCode::UNAUTHORIZED,
            "NOVA_GATEWAY_CONNECTION_UNAVAILABLE",
        )
    } else if !s.active.load(Ordering::Acquire) {
        denied(StatusCode::SERVICE_UNAVAILABLE, "NOVA_GATEWAY_STOPPED")
    } else {
        next.run(request).await
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}

async fn rpc(State(s): State<Arc<Gateway>>, request: Request) -> Response {
    let Ok(_slot) = s.slots.try_acquire() else {
        return denied(StatusCode::TOO_MANY_REQUESTS, "NOVA_GATEWAY_BUSY");
    };
    let (parts, body) = request.into_parts();
    if !parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/json"))
    {
        return denied(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "NOVA_GATEWAY_JSON_REQUIRED",
        );
    }
    let version = parts
        .headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok());
    if version.is_some_and(|v| v.len() > 24 || !v.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
    {
        return denied(StatusCode::BAD_REQUEST, "NOVA_GATEWAY_INVALID_PROTOCOL");
    }
    let body =
        match tokio::time::timeout(Duration::from_secs(5), to_bytes(body, MAX_REQUEST as usize))
            .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(_)) => return denied(StatusCode::PAYLOAD_TOO_LARGE, "NOVA_GATEWAY_INPUT_LIMIT"),
            Err(_) => return denied(StatusCode::REQUEST_TIMEOUT, "NOVA_GATEWAY_INPUT_TIMEOUT"),
        };
    let Ok(input) = serde_json::from_slice::<Value>(&body) else {
        return denied(StatusCode::BAD_REQUEST, "NOVA_GATEWAY_INVALID_JSON");
    };
    let id = input.get("id");
    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !input.is_object()
        || input.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || id.is_some_and(|v| !(v.is_string() || v.is_i64() || v.is_u64()))
        || input.as_object().is_some_and(|o| {
            o.keys()
                .any(|k| !["jsonrpc", "id", "method", "params"].contains(&k.as_str()))
        })
        || (method != "notifications/initialized" && id.is_none())
        || (method == "notifications/initialized" && id.is_some())
    {
        return denied(StatusCode::BAD_REQUEST, "NOVA_GATEWAY_INVALID_RPC");
    }
    if ![
        "initialize",
        "notifications/initialized",
        "ping",
        "tools/list",
        "tools/call",
    ]
    .contains(&method)
        || (method == "tools/call"
            && !input["params"]["name"]
                .as_str()
                .is_some_and(|name| BASIC_TOOLS.contains(&name)))
    {
        return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Only the selected Nova connection's basic memory tools are available."}})).into_response();
    }
    if !s.active.load(Ordering::Acquire) {
        return denied(StatusCode::SERVICE_UNAVAILABLE, "NOVA_GATEWAY_STOPPED");
    }
    // A temporary HTTPS credential may be revoked while this request waits for its body.
    if !parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| credential_matches(&s, h))
    {
        return denied(
            StatusCode::UNAUTHORIZED,
            "NOVA_GATEWAY_CONNECTION_UNAVAILABLE",
        );
    }
    match forward(&s.client, &s.pinned, &input, version).await {
        Ok(Some(mut output)) => {
            if method == "initialize"
                && (output["result"]["serverInfo"]["name"] != "nova-memory-automatic"
                    || output["result"]["serverInfo"]["version"] != "0.3.0")
            {
                return denied(
                    StatusCode::BAD_GATEWAY,
                    "NOVA_GATEWAY_APPLICATION_CONTRACT_MISMATCH",
                );
            }
            if method == "tools/list"
                && let Some(tools) = output["result"]["tools"].as_array_mut()
            {
                tools.retain(|tool| {
                    tool["name"]
                        .as_str()
                        .is_some_and(|name| BASIC_TOOLS.contains(&name))
                });
            }
            if serde_json::to_vec(&output).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE) {
                Json(output).into_response()
            } else {
                denied(StatusCode::BAD_GATEWAY, "NOVA_GATEWAY_RESPONSE_LIMIT")
            }
        }
        Ok(None) if id.is_none() => StatusCode::ACCEPTED.into_response(),
        _ => denied(StatusCode::BAD_GATEWAY, "NOVA_GATEWAY_UPSTREAM_UNAVAILABLE"),
    }
}

/// Caller must bind the exact loopback listener first. No tunnel or public listener is created.
pub fn router(path: PathBuf, authority: String) -> Result<Router> {
    managed_router(path, authority, Arc::new(AtomicBool::new(true)))
}

pub(crate) fn managed_router(
    path: PathBuf,
    authority: String,
    active: Arc<AtomicBool>,
) -> Result<Router> {
    delegated_router(path, authority, active, Arc::new(Mutex::new(None)))
}
pub(crate) fn delegated_router(
    path: PathBuf,
    authority: String,
    active: Arc<AtomicBool>,
    delegated: DelegatedCredential,
) -> Result<Router> {
    let address: std::net::SocketAddr = authority.parse()?;
    ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "NOVA_GATEWAY_LOOPBACK_ONLY"
    );
    let state = Arc::new(Gateway {
        pinned: read_local_connection(&path)?,
        path,
        authority,
        client: http_client()?,
        slots: Semaphore::new(MAX_IN_FLIGHT),
        active,
        delegated,
    });
    Ok(Router::new()
        .route("/mcp", post(rpc))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state))
}
