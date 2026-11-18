//! Owner-only N1 adapter. Existing authentication/Origin/Host middleware protects all routes.
use super::*;
use axum::{
    body::Bytes,
    extract::{Path, Query},
};
use nova_core::{
    compilation::{AssetScope, SourceRole},
    intake::{IntakeAuthority, IntakeStore, ModelSelection},
};
use serde_json::Value;

pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/app/intake/", get(page))
        .route("/intake.js", get(js))
        .route("/intake-logo.png", get(logo))
        .route("/intake.css", get(css))
        .route("/console/api/owner/intake", post(receive))
        .route("/console/api/owner/intake/jobs", get(jobs))
        .route("/console/api/owner/intake/model-usage", get(model_usage))
        .route("/console/api/owner/intake/model-capabilities", get(model_capabilities))
        .route("/console/api/owner/intake/units", get(units))
        .route("/console/api/owner/intake/wiki", get(wiki))
        .route("/console/api/owner/intake/withdrawals", get(withdrawals))
        .route(
            "/console/api/owner/intake/wiki/{id}/rebuild",
            post(rebuild_wiki),
        )
        .route(
            "/console/api/owner/intake/sources/{id}/withdraw",
            post(withdraw),
        )
        .route(
            "/console/api/owner/intake/sources/{id}/delete",
            post(delete_source),
        )
        .route("/console/api/owner/intake/units/{id}/history", get(history))
        .route(
            "/console/api/owner/intake/units/{id}/correct",
            post(correct),
        )
        .route("/console/api/owner/intake/units/{id}/undo", post(undo))
        .route("/console/api/owner/intake/jobs/{id}/retry", post(retry))
        .route("/console/api/owner/intake/sources/{id}", get(source))
        .route(
            "/console/api/owner/intake/configuration",
            get(configuration).post(configure),
        )
        .route("/console/api/owner/intake/backup", post(backup))
        .route("/console/api/owner/intake/backup/status", get(backup_status))
        .route("/console/api/owner/intake/backup/verify", post(backup_verify))
}
fn scope(s: &ReviewServer) -> AssetScope {
    AssetScope {
        owner_id: s.owner.clone(),
        space: s.space.clone(),
    }
}
fn store(s: &ReviewServer) -> Result<IntakeStore, NovaError> {
    IntakeStore::open(
        s.intake_store
            .as_ref()
            .ok_or_else(|| NovaError::InvalidState("intake not enabled".into()))?,
        VaultKey::read_file(&s.key_path)?,
        scope(s),
    )
}
pub(super) fn response(result: Result<Value, NovaError>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => {
            let status = match e {
                NovaError::Validation(_) => StatusCode::BAD_REQUEST,
                NovaError::PolicyDenied(_) => StatusCode::FORBIDDEN,
                NovaError::NotFound { .. } => StatusCode::NOT_FOUND,
                NovaError::IdempotencyConflict { .. } | NovaError::InvalidState(_) => {
                    StatusCode::CONFLICT
                }
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (
                status,
                Json(json!({"error":{"code":e.code(),"message":match status {
                    StatusCode::BAD_REQUEST => "内容或配置不符合接收要求，请检查长度与设置。",
                    StatusCode::CONFLICT => "请求或配置已变化，请刷新；原文仍保留。",
                    StatusCode::FORBIDDEN => "当前凭据没有此操作权限。",
                    StatusCode::NOT_FOUND => "未找到这条接收记录。",
                    _ => "暂时无法确认结果，请保留输入并重试同一次接收。"
                }}})),
            )
                .into_response()
        }
    }
}
async fn page() -> Html<&'static str> {
    Html(include_str!("intake.html"))
}
async fn js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("intake.js"),
    )
}
async fn logo() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("../../../docs/assets/nova-memory-logo-primary.png").as_slice(),
    )
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("intake.css"),
    )
}
async fn receive(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    let result = (|| {
        // Role, privacy and actor are bound to this authenticated owner route, not JSON input.
        let parsed = nova_core::compilation::IntakeRequest::parse(&bytes)?;
        if parsed.source_platform != "web-console"
            || parsed.capture_fidelity != nova_core::CaptureFidelity::F2VisibleTranscript
        {
            return Err(NovaError::Validation(
                "owner capture metadata required".into(),
            ));
        }
        let receipt = store(&s)?.receive(
            &IntakeAuthority {
                scope: scope(&s),
                actor_id: "owner-web".into(),
                capture_allowed: true,
                role: SourceRole::User,
                privacy_level: nova_core::PrivacyLevel::Private,
            },
            &bytes,
        )?;
        Ok(json!({"receipt":receipt,"compilation":"waiting","saved":true}))
    })();
    response(result)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<i64>,
}
async fn jobs(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let mut store = store(&s)?;
        store.recover_expired(&scope(&s))?;
        let rows = store.jobs(&scope(&s), page.before)?;
        let next = if rows.len() == 50 {
            rows.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        let items=rows.into_iter().map(|(_,job)|{
            let mut item=json!(job);
            if let Some(change)=&job.job.committed_change_id {
                if let Ok(commit)=store.compilation_commit(&scope(&s),change) {
                    item["compilation_summary"]=json!({"unit_count":commit.output_versions.len(),"no_op":commit.change.operations.is_empty()});
                } else {item["compilation_summary"]=json!({"invalidated":true});}
            }
            Ok(item)
        }).collect::<Result<Vec<_>,NovaError>>()?;
        Ok(
            json!({"items":items,"next_before":next,"compiler_available":s.model_runtime.is_some(),"processing_blocker":if store.backup_recovery_manifest(&scope(&s))?.is_some() {Some("backup_quarantined")} else if store.configuration()?.selection == ModelSelection::Unconfigured {Some("model_unconfigured")} else if s.model_runtime.is_none() {Some("compiler_unavailable")} else {None}}),
        )
    })())
}
async fn source(State(s): State<ReviewServer>, Path(id): Path<String>) -> Response {
    let _guard = s.lock.lock().await;
    response(store(&s).and_then(|store| Ok(json!(store.source(&scope(&s), &id)?))))
}
async fn model_usage(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    response(store(&s).and_then(|store| Ok(json!(store.compilation_usage(&scope(&s), page.before)?))))
}
async fn retry(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty retry request required".into()));
        }
        let mut store = store(&s)?;
        Ok(json!(store.retry(&scope(&s), &id)?))
    })())
}
async fn model_capabilities(State(s):State<ReviewServer>)->Response {
    let _guard=s.lock.lock().await;
    response((|| {let store=store(&s)?;let config=store.configuration()?;
        Ok(crate::provider_capabilities::describe(&config,s.model_runtime.as_ref(),store.backup_recovery_manifest(&scope(&s))?.is_some()))})())
}
async fn configuration(State(s): State<ReviewServer>) -> Response {
    let _guard = s.lock.lock().await;
    response(store(&s).and_then(|store| {let quarantined=store.backup_recovery_manifest(&scope(&s))?.is_some(); Ok(json!({"configuration":store.configuration()?,"compiler_available":s.model_runtime.is_some()&&!quarantined,"backup_quarantined":quarantined,"maximum_calls":s.model_runtime.as_ref().map(|r|r.maximum_calls),"network_calls":s.model_runtime.is_some()&&!quarantined,"context_scope":if s.model_runtime.is_some(){"selected_source_and_explicit_correction_target"}else{"selected_source_only"},"store_version":nova_core::intake::STORE_VERSION,"enabled":true,"demo":s.demo,"temporary_demo":s.temporary_demo}))}))
}
async fn backup_status(State(s):State<ReviewServer>)->Response {
    let _guard=s.lock.lock().await;
    response((|| {let store=store(&s)?;Ok(json!({"last_generation":store.last_backup_generation(&scope(&s))?,"recovery":store.backup_recovery_manifest(&scope(&s))?,"external_copy_verified":false,"off_device_restore_verified":false}))})())
}
async fn backup_verify(State(s):State<ReviewServer>,bytes:Bytes)->Response {
    let _guard=s.lock.lock().await;
    response((|| {if bytes.as_ref()!=b"{}" {return Err(NovaError::Validation("empty verification request required".into()));}Ok(json!(store(&s)?.verify_backup_copy(&scope(&s))?))})())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationInput {
    expected_revision: u64,
    selection: ModelSelection,
}
async fn configure(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        // Derived structs reject duplicate and unknown fields. No opaque JSON maps here.
        let i: ConfigurationInput = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid model configuration".into()))?;
        Ok(json!(
            store(&s)?.configure_model(i.expected_revision, i.selection)?
        ))
    })())
}
async fn backup(State(s): State<ReviewServer>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    let result = (|| -> Result<Vec<u8>, NovaError> {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation(
                "empty backup request required".into(),
            ));
        }
        let d = tempfile::tempdir()?;
        let path = d.path().join("intake.sqlite");
        store(&s)?.backup_to(&scope(&s), &path)?;
        Ok(std::fs::read(path)?)
    })();
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=nova-intake-encrypted.sqlite",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => response(Err(e)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnitPage {
    before: Option<i64>,
    q: Option<String>,
}
async fn units(State(s): State<ReviewServer>, Query(page): Query<UnitPage>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let store = store(&s)?;
        let rows = if let Some(q) = page.q {
            store.search_owner_units(&scope(&s), &q, page.before)?
        } else {
            store.compiled_units(&scope(&s), page.before)?
        };
        let next = if rows.len() == 50 {
            rows.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        let items=rows.into_iter().map(|(_,u)| {
            let commit=store.compilation_commit(&scope(&s),&u.change_id).ok();
            Ok(json!({"memory":u,"can_undo":commit.as_ref().is_some_and(|c|c.correction.is_some()&&c.undoes_change_id.is_none()),"semantic_support":"model_output_unverified"}))
        }).collect::<Result<Vec<_>,NovaError>>()?;
        Ok(json!({"items":items,"next_before":next,"continuity_available":s.continuity_enabled}))
    })())
}
async fn history(State(s): State<ReviewServer>, Path(id): Path<String>) -> Response {
    let _guard = s.lock.lock().await;
    response(
        store(&s).and_then(|store| Ok(json!({"versions":store.unit_history(&scope(&s),&id)?}))),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectionInput {
    request_id: String,
    expected_revision: u64,
    instruction: String,
}
async fn correct(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if s.model_runtime.is_none() {
            return Err(NovaError::InvalidState(
                "compiler runtime not enabled".into(),
            ));
        }
        let input: CorrectionInput = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid correction".into()))?;
        let receipt = store(&s)?.request_correction(
            &nova_core::intake::CorrectionAuthority {
                scope: scope(&s),
                actor_id: "owner-web".into(),
                may_correct: true,
            },
            nova_core::compilation::RevisionRef {
                object_id: id,
                revision: input.expected_revision,
            },
            input.request_id,
            input.instruction,
        )?;
        Ok(json!({"receipt":receipt,"saved":true,"compilation":"waiting"}))
    })())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UndoInput {
    expected_revision: u64,
}
async fn undo(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let input: UndoInput = serde_json::from_slice(&bytes)
            .map_err(|_| NovaError::Validation("invalid undo".into()))?;
        Ok(json!(store(&s)?.undo_correction(
            &nova_core::intake::CorrectionAuthority {
                scope: scope(&s),
                actor_id: "owner-web".into(),
                may_correct: true
            },
            &id,
            input.expected_revision
        )?))
    })())
}

async fn wiki(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let store = store(&s)?;
        let rows = store.wiki_pages(&scope(&s), page.before)?;
        let next = if rows.len() == 50 {
            rows.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        Ok(
            json!({"items":rows.into_iter().map(|(_,p)|p).collect::<Vec<_>>(),"next_before":next,"queue":store.consolidation_queue(&scope(&s))?}),
        )
    })())
}
async fn withdraw(State(s): State<ReviewServer>, Path(id): Path<String>, bytes: Bytes) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty withdrawal required".into()));
        }
        store(&s)?.withdraw_source(&scope(&s), &id)?;
        Ok(json!({"withdrawn":true}))
    })())
}
async fn delete_source(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty deletion required".into()));
        }
        let count = store(&s)?.delete_source(&scope(&s), &id)?;
        Ok(
            json!({"deleted_envelopes":count,"scope":"live_intake_store_only","external_backups_erased":false}),
        )
    })())
}

async fn rebuild_wiki(
    State(s): State<ReviewServer>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        if bytes.as_ref() != b"{}" {
            return Err(NovaError::Validation("empty rebuild required".into()));
        }
        store(&s)?.rebuild_wiki_page(&scope(&s), &id)?;
        Ok(json!({"queued":true}))
    })())
}

async fn withdrawals(State(s): State<ReviewServer>, Query(page): Query<Page>) -> Response {
    let _guard = s.lock.lock().await;
    response((|| {
        let rows = store(&s)?.withdrawn_sources(&scope(&s), page.before)?;
        let next = if rows.len() == 50 {
            rows.last().map(|(seq, _, _)| *seq)
        } else {
            None
        };
        Ok(
            json!({"items":rows.into_iter().map(|(_,id,at)|json!({"source_id":id,"withdrawn_at":at})).collect::<Vec<_>>(),"next_before":next}),
        )
    })())
}
