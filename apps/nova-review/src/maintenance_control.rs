//! Owner-only local deterministic maintenance, using the existing authenticated workspace.
use super::*;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query},
};
use nova_core::{compilation::decode_untrusted_json, intake::MaintenancePolicy};

pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/maintenance-console.js", get(script))
        .route(
            "/console/api/owner/intake/maintenance/preview",
            post(preview),
        )
        .route(
            "/console/api/owner/intake/maintenance/jobs",
            get(list).post(schedule),
        )
        .route(
            "/console/api/owner/intake/maintenance/jobs/{id}/run",
            post(run),
        )
        .route(
            "/console/api/owner/intake/maintenance/jobs/{id}/cancel",
            post(cancel),
        )
        .layer(DefaultBodyLimit::max(4096))
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("maintenance-console.js"),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    #[serde(default)]
    policy: MaintenancePolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Schedule {
    request_id: String,
    #[serde(default)]
    policy: MaintenancePolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn preview(State(s): State<ReviewServer>, body: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let input: Preview = decode_untrusted_json(&body)?;
        let service = native::service(&s)?;
        Ok(json!(
            service
                .open()?
                .preview_maintenance(service.scope(), input.policy)?
        ))
    })())
}
async fn schedule(State(s): State<ReviewServer>, body: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let input: Schedule = decode_untrusted_json(&body)?;
        let service = native::service(&s)?;
        Ok(json!(service.open()?.schedule_maintenance(
            service.scope(),
            &input.request_id,
            input.policy
        )?))
    })())
}
async fn list(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let service = native::service(&s)?;
        let items = service
            .open()?
            .maintenance_jobs(service.scope(), page.before)?;
        Ok(
            json!({"next_before":items.last().map(|r|r.0),"items":items.into_iter().map(|r|r.1).collect::<Vec<_>>() }),
        )
    })())
}
async fn run(State(s): State<ReviewServer>, Path(id): Path<String>, body: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let _: Empty = decode_untrusted_json(&body)?;
        let service = native::service(&s)?;
        Ok(json!(
            service
                .open()?
                .run_maintenance_when_idle(service.scope(), &id)?
        ))
    })())
}
async fn cancel(State(s): State<ReviewServer>, Path(id): Path<String>, body: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let _: Empty = decode_untrusted_json(&body)?;
        let service = native::service(&s)?;
        Ok(json!(
            service.open()?.cancel_maintenance(service.scope(), &id)?
        ))
    })())
}
