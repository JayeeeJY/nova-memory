//! Explicit capture-only HTTP ingress. Never exposes owner configuration or memory reads.
use crate::automatic::IntakeApplicationService;
use axum::{
    Extension, Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use nova_core::{
    NovaError,
    compilation::decode_untrusted_json,
    intake::{
        CaptureAccess, CaptureAck, CaptureProvenanceRecord, CaptureProvenanceRequest,
        CaptureSession,
    },
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug)]
pub struct CaptureIngressLimits {
    pub maximum_in_flight: usize,
    pub body_deadline: Duration,
}
impl Default for CaptureIngressLimits {
    fn default() -> Self {
        Self {
            maximum_in_flight: 8,
            body_deadline: Duration::from_secs(2),
        }
    }
}

#[derive(Clone)]
struct Boundary {
    service: IntakeApplicationService,
    authority: String,
    in_flight: Arc<Semaphore>,
    limits: CaptureIngressLimits,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterSession {
    origin_session_id: String,
}

fn failure(error: NovaError) -> Response {
    let (status, code, rate, uncertain) = match &error {
        NovaError::Validation(s) if s == "intake: capture_rate_limited" => (
            StatusCode::TOO_MANY_REQUESTS,
            "NOVA_CAPTURE_RATE_LIMIT",
            true,
            false,
        ),
        NovaError::Validation(s) if s == "intake: capture_budget_exhausted" => (
            StatusCode::TOO_MANY_REQUESTS,
            "NOVA_CAPTURE_BUDGET_EXHAUSTED",
            false,
            false,
        ),
        NovaError::Validation(s) if s == "intake: capture_session_capacity" => (
            StatusCode::INSUFFICIENT_STORAGE,
            "NOVA_CAPTURE_CAPACITY",
            false,
            false,
        ),
        NovaError::PolicyDenied(_) => {
            (StatusCode::FORBIDDEN, "NOVA_CAPTURE_REJECTED", false, false)
        }
        NovaError::InvalidState(_) | NovaError::IdempotencyConflict { .. } => {
            (StatusCode::CONFLICT, "NOVA_CAPTURE_CONFLICT", false, false)
        }
        NovaError::NotFound { .. } => (StatusCode::GONE, "NOVA_CAPTURE_UNAVAILABLE", false, false),
        NovaError::Database(_) | NovaError::Io(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "NOVA_CAPTURE_OUTCOME_UNKNOWN",
            false,
            true,
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            "NOVA_CAPTURE_REJECTED",
            false,
            false,
        ),
    };
    let mut r = (
        status,
        Json(json!({"saved":if uncertain {None} else {Some(false)},"code":code})),
    )
        .into_response();
    if rate {
        r.headers_mut()
            .insert(header::RETRY_AFTER, "60".parse().unwrap());
    }
    r
}
async fn guard(State(s): State<Boundary>, mut request: Request, next: Next) -> Response {
    let Ok(_permit) = s.in_flight.clone().try_acquire_owned() else {
        return before_dispatch(StatusCode::SERVICE_UNAVAILABLE, "NOVA_CAPTURE_BUSY", true);
    };
    let h = request.headers();
    let forbidden = h.contains_key(header::ORIGIN)
        || matches!(
            h.get("sec-fetch-site").and_then(|x| x.to_str().ok()),
            Some("cross-site" | "same-site")
        );
    let token = (h.get_all(header::AUTHORIZATION).iter().count() == 1)
        .then(|| h.get(header::AUTHORIZATION))
        .flatten()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let access = if h.get_all(header::HOST).iter().count() == 1
        && h.get(header::HOST).and_then(|x| x.to_str().ok()) == Some(s.authority.as_str())
    {
        token.and_then(|t| {
            s.service
                .open()
                .ok()?
                .authenticate_capture(s.service.scope(), t)
                .ok()
        })
    } else {
        None
    };
    let mut r = if forbidden {
        (StatusCode::FORBIDDEN, "NOVA_ORIGIN_FORBIDDEN").into_response()
    } else if let Some(access) = access {
        request.extensions_mut().insert(access);
        // The deadline covers ingestion only. Never cancel a storage transaction and then
        // claim saved=false: once dispatch begins, durable ACK / outcome-unknown rules apply.
        let (parts, body) = request.into_parts();
        match tokio::time::timeout(s.limits.body_deadline, to_bytes(body, 96 * 1024)).await {
            Err(_) => before_dispatch(
                StatusCode::REQUEST_TIMEOUT,
                "NOVA_CAPTURE_BODY_TIMEOUT",
                true,
            ),
            Ok(Err(_)) => before_dispatch(
                StatusCode::PAYLOAD_TOO_LARGE,
                "NOVA_CAPTURE_BODY_REJECTED",
                false,
            ),
            Ok(Ok(bytes)) => {
                next.run(Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
        }
    } else {
        (StatusCode::UNAUTHORIZED, "NOVA_CAPTURE_UNAUTHORIZED").into_response()
    };
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}
fn before_dispatch(status: StatusCode, code: &str, retry: bool) -> Response {
    let mut r = (
        status,
        Json(json!({"saved":false,"code":code,"phase":"before_dispatch"})),
    )
        .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if retry {
        r.headers_mut()
            .insert(header::RETRY_AFTER, "1".parse().unwrap());
    }
    r
}
async fn session(
    State(s): State<Boundary>,
    Extension(a): Extension<CaptureAccess>,
    bytes: Bytes,
) -> Result<Json<CaptureSession>, Response> {
    let i: RegisterSession = decode_untrusted_json(&bytes).map_err(failure)?;
    let result = s
        .service
        .open()
        .and_then(|mut store| {
            store.register_capture_session(s.service.scope(), &a, &i.origin_session_id)
        })
        .map_err(failure)?;
    Ok(Json(result))
}
async fn event(
    State(s): State<Boundary>,
    Extension(a): Extension<CaptureAccess>,
    bytes: Bytes,
) -> Result<Json<CaptureAck>, Response> {
    let result = s
        .service
        .open()
        .and_then(|mut store| store.capture_event(s.service.scope(), &a, &bytes))
        .map_err(failure)?;
    Ok(Json(result))
}
async fn provenance(
    State(s): State<Boundary>,
    Extension(a): Extension<CaptureAccess>,
    bytes: Bytes,
) -> Result<Json<CaptureProvenanceRecord>, Response> {
    let input: CaptureProvenanceRequest = decode_untrusted_json(&bytes).map_err(failure)?;
    let result = s
        .service
        .open()
        .and_then(|mut store| store.record_capture_provenance(s.service.scope(), &a, input))
        .map_err(failure)?;
    Ok(Json(result))
}
/// Caller explicitly mounts this router on a locally bound, fixed-instance listener.
/// Issuing/revoking grants remains an authenticated owner control-plane operation.
pub fn router(service: IntakeApplicationService, authority: String) -> Router {
    router_with_limits(service, authority, CaptureIngressLimits::default())
        .expect("fixed capture limits are valid")
}
/// Process-local admission and pre-dispatch body bounds. No production DDoS or disk-quota claim.
pub fn router_with_limits(
    service: IntakeApplicationService,
    authority: String,
    limits: CaptureIngressLimits,
) -> nova_core::Result<Router> {
    if !(1..=32).contains(&limits.maximum_in_flight)
        || limits.body_deadline < Duration::from_millis(20)
        || limits.body_deadline > Duration::from_secs(10)
    {
        return Err(NovaError::Validation(
            "invalid capture ingress limits".into(),
        ));
    }
    let state = Boundary {
        service,
        authority,
        in_flight: Arc::new(Semaphore::new(limits.maximum_in_flight)),
        limits,
    };
    Ok(Router::new()
        .route("/capture/v01/session", post(session))
        .route("/capture/v01/events", post(event))
        .route("/capture/v01/provenance", post(provenance))
        .layer(DefaultBodyLimit::max(96 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state))
}
