//! Read-only Owner diagnostics. No model/network probe, credential disclosure or repair action.
use super::*;
use std::path::Path;
pub(super) fn routes() -> Router<ReviewServer> {
    Router::new()
        .route("/diagnostics-console.js", get(script))
        .route("/recovery-console.js", get(recovery_script))
        .route("/console/api/owner/intake/diagnostics", get(report))
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("diagnostics-console.js"),
    )
}
async fn recovery_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("recovery-console.js"),
    )
}
fn pressure(bytes: u64) -> &'static str {
    if bytes < 64 * 1024 * 1024 {
        "critical"
    } else if bytes < 512 * 1024 * 1024 {
        "low"
    } else {
        "available_observed"
    }
}
fn disk(path: &Path) -> serde_json::Value {
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        if let Ok(path) = CString::new(path.as_os_str().as_bytes()) {
            let mut value = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            // SAFETY: NUL-terminated path and correctly sized writable struct; read only on success.
            if unsafe { libc::statvfs(path.as_ptr(), value.as_mut_ptr()) } == 0 {
                let value = unsafe { value.assume_init() };
                let unit = if value.f_frsize > 0 {
                    value.f_frsize
                } else {
                    value.f_bsize
                };
                if let (Some(free), Some(total)) = (
                    (value.f_bavail as u64).checked_mul(unit),
                    (value.f_blocks as u64).checked_mul(unit),
                ) {
                    return json!({"state":pressure(free),"available_bytes":free,"filesystem_bytes":total,"quota_enforced":false,"basis":"filesystem_available_snapshot_not_write_reservation"});
                }
            }
        }
    }
    json!({"state":"unknown","available_bytes":null,"filesystem_bytes":null,"quota_enforced":false})
}
async fn report(State(s): State<ReviewServer>) -> Response {
    let _guard = s.lock.lock().await;
    intake::response((|| {
        let service = native::service(&s)?;
        let store = service.open()?;
        let d = store.operational_diagnostics(service.scope())?;
        let backup = store.last_backup_generation(service.scope())?;
        let copy = store.backup_recovery_manifest(service.scope())?;
        let model_usage = store.compilation_usage(service.scope(), None)?;
        let backup = json!({"last_generated_at":backup.as_ref().map(|b|&b.generated_at),"external_copy_verified":false,"off_device_restore_verified":false,"quarantined_copy":copy.is_some(),"copy_created_at":copy.as_ref().map(|m|&m.created_at),"origin_backup_created_at":copy.as_ref().map(|m|&m.origin_backup_created_at),"current_integrity_reverified":false});
        Ok(
            json!({"schema_version":"nova.local-doctor.v0.1","observed_at":d.observed_at,"store":d,"backup":backup,"model_usage":model_usage,"runtime":{"owner_api":"responding","compiler_configured":s.model_runtime.is_some(),"worker_liveness":"not_probed","capture_ingress_enabled":s.capture_ingress,"model_network_probe":false},"disk":disk(s.intake_store.as_ref().ok_or_else(||NovaError::Validation("intake required".into()))?),"unobserved":{"hook_installation":"use_explicit_installation_manifest","endpoint_relay":"use_selected_binding_status","backup_freshness":"generation_only_external_copy_not_verified","release_signature":"not_checked","real_host_acceptance":"not_verified"},"repairs_performed":false}),
        )
    })())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_observation_is_advisory_and_failure_is_unknown() {
        assert_eq!(pressure(0), "critical");
        assert_eq!(pressure(100 * 1024 * 1024), "low");
        assert_eq!(pressure(1024 * 1024 * 1024), "available_observed");
        let d = tempfile::tempdir().unwrap();
        let report = disk(d.path());
        assert!(report["available_bytes"].is_u64());
        assert_eq!(report["quota_enforced"], false);
        assert_eq!(disk(&d.path().join("missing"))["state"], "unknown");
    }
}
