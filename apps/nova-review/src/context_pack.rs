//! Owner-only stateless context preview/export. No target client call or new connection authority.
use super::*;
use axum::{body::Bytes, extract::DefaultBodyLimit};
use nova_core::{
    compilation::decode_untrusted_json,
    intake::{ContextPackExportRequest, ContextPackSelection},
};

pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/context-pack.js", get(script))
        .route(
            "/console/api/owner/intake/context-packs/preview",
            post(preview),
        )
        .route(
            "/console/api/owner/intake/context-packs/export",
            post(export),
        )
        .layer(DefaultBodyLimit::max(12 * 1024))
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("context-pack.js"),
    )
}
async fn preview(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    super::intake::response((|| {
        let input: ContextPackSelection = decode_untrusted_json(&bytes)?;
        let service = super::native::service(&s)?;
        let pack = service
            .open()?
            .preview_context_pack(service.scope(), &input)?;
        Ok(
            json!({"pack":pack,"mode":"owner_preview","model_called":false,"target_contacted":false}),
        )
    })())
}
async fn export(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    super::intake::response((|| {
        let input: ContextPackExportRequest = decode_untrusted_json(&bytes)?;
        let service = super::native::service(&s)?;
        let release = service
            .open()?
            .export_context_pack(service.scope(), &input)?;
        Ok(json!({"release":release,"model_called":false,"target_contacted":false}))
    })())
}
