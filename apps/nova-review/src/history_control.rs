//! Historical permission is a separate Owner operation, never a tool argument.
use super::*;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path},
};
use nova_core::compilation::decode_untrusted_json;

pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route(
            "/console/api/owner/intake/connections/{id}/history-policy",
            get(read).post(set),
        )
        .layer(DefaultBodyLimit::max(4096))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    expected_revision: u64,
    earliest_recorded_at: Option<String>,
}
fn service(s: &ReviewServer) -> Result<nova_mcp::automatic::IntakeApplicationService, NovaError> {
    if !s.historical_retrieval_enabled {
        return Err(NovaError::PolicyDenied("history retrieval disabled".into()));
    }
    native::service(s)
}
async fn read(State(s): State<ReviewServer>, Path(id): Path<String>) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let svc = service(&s)?;
        Ok(json!(
            svc.open()?.connection_history_policy(svc.scope(), &id)?
        ))
    })())
}
async fn set(State(s): State<ReviewServer>, Path(id): Path<String>, body: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let i: Input = decode_untrusted_json(&body)?;
        let svc = service(&s)?;
        Ok(json!(svc.open()?.set_connection_history_policy(
            svc.scope(),
            &id,
            i.expected_revision,
            i.earliest_recorded_at
        )?))
    })())
}
