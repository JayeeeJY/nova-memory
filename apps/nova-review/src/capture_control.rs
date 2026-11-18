//! Owner-only capture policy; all routes inherit the existing independent owner boundary.
use super::*;
use axum::{body::Bytes, extract::Path};
use nova_core::{compilation::decode_untrusted_json, intake::CaptureGrantRequest};
pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route(
            "/console/api/owner/intake/capture-grants",
            get(list).post(create),
        )
        .route(
            "/console/api/owner/intake/capture-grants/{id}/revoke",
            post(revoke),
        )
}
fn service(s: &ReviewServer) -> Result<nova_mcp::automatic::IntakeApplicationService, NovaError> {
    if !s.capture_ingress {
        return Err(NovaError::PolicyDenied("capture ingress disabled".into()));
    }
    native::service(s)
}
async fn list(State(s): State<ReviewServer>) -> Response {
    let _lock = s.lock.lock().await;
    intake::response((|| {
        let svc = service(&s)?;
        Ok(
            json!({"items":svc.open()?.capture_grants(svc.scope())?,"bounded_to_latest":128,"endpoint":format!("http://{}/capture/v01",s.authority),"installed":false,"host_acceptance":"not_verified_by_configuration"}),
        )
    })())
}
async fn create(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _lock = s.lock.lock().await;
    intake::response((|| {
        let svc = service(&s)?;
        let input: CaptureGrantRequest = decode_untrusted_json(&bytes)?;
        let issued = svc.open()?.create_capture_grant(svc.scope(), input)?;
        Ok(
            json!({"grant":issued.grant,"credential":issued.token,"credential_shown_once":true,"capabilities":{"capture":true,"read":false,"owner":false},"endpoint":format!("http://{}/capture/v01",s.authority),"installed":false}),
        )
    })())
}
async fn revoke(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _lock = s.lock.lock().await;
    intake::response((|| {
        let svc = service(&s)?;
        let input: serde_json::Value = decode_untrusted_json(&bytes)?;
        if input != json!({}) {
            return Err(NovaError::Validation("empty request required".into()));
        }
        svc.open()?.revoke_capture(svc.scope(), &id)?;
        Ok(json!({"revoked":true,"already_saved_sources_unchanged":true}))
    })())
}
