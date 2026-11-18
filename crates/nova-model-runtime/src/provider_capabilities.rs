//! Static adapter/configuration contract, never a service probe or provider/model quality claim.
use crate::compiler::{ModelRuntime, OutputMode};
use nova_core::{
    NovaError,
    intake::{ModelConfiguration, ModelSelection},
};
use serde_json::{Value, json};
use std::net::IpAddr;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 45;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 2048;
pub(crate) fn workspace_timeout_seconds(selection: &ModelSelection) -> u64 {
    if matches!(selection, ModelSelection::Local { .. }) {
        // Allow model loading and inference while leaving 30 seconds of the
        // 120-second workspace lease for dispatch bookkeeping and commit.
        90
    } else {
        DEFAULT_TIMEOUT_SECONDS
    }
}
fn error(code: &'static str) -> NovaError {
    NovaError::Validation(format!("compiler: {code}"))
}
pub(crate) struct TransportTarget {
    pub model: String,
    pub url: url::Url,
    pub host: String,
    pub port: u16,
    pub remote: bool,
}
/// Shared by the Owner description and real dispatch before credentials, DNS or network I/O.
/// Remote DNS resolution and credential contents are still rechecked at actual dispatch.
pub(crate) fn transport_target(
    config: &ModelConfiguration,
    runtime: &ModelRuntime,
) -> Result<TransportTarget, NovaError> {
    runtime.validate()?;
    config.validate()?;
    let (model, endpoint, remote) = match &config.selection {
        ModelSelection::Local { model_id, endpoint } => (model_id, endpoint, false),
        ModelSelection::Remote {
            model_id,
            endpoint,
            allow_selected_source: true,
        } => (model_id, endpoint, true),
        _ => return Err(error("model_not_configured")),
    };
    if remote && runtime.output_mode == OutputMode::StructuredLocal {
        return Err(error("structured_output_local_only"));
    }
    let url = url::Url::parse(endpoint).map_err(|_| error("endpoint_invalid"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error("endpoint_credentials_denied"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| error("endpoint_host"))?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| error("endpoint_port"))?;
    if remote {
        if runtime.credential_origin.as_deref() != Some(url.origin().ascii_serialization().as_str())
        {
            return Err(error("credential_origin_mismatch"));
        }
        if runtime.credential_file.is_none() {
            return Err(error("credential_missing"));
        }
        if url.scheme() != "https" || host.parse::<IpAddr>().is_ok() {
            return Err(error("remote_public_dns_required"));
        }
    } else if url.scheme() != "http" || !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
        return Err(error("local_loopback_required"));
    }
    Ok(TransportTarget {
        model: model.clone(),
        url,
        host,
        port,
        remote,
    })
}
/// Metadata only: no endpoint URL, credential path/contents, material, probe or usage estimate.
pub fn describe(
    config: &ModelConfiguration,
    runtime: Option<&ModelRuntime>,
    quarantined: bool,
) -> Value {
    let (location, model) = match &config.selection {
        ModelSelection::Unconfigured => ("unconfigured", None),
        ModelSelection::Local { model_id, .. } => {
            ("loopback_service_configured", Some(model_id.as_str()))
        }
        ModelSelection::Remote { model_id, .. } => {
            ("remote_service_configured", Some(model_id.as_str()))
        }
    };
    let blocker = if quarantined {
        Some("backup_quarantined")
    } else if model.is_none() {
        Some("model_not_configured")
    } else if let Some(runtime) = runtime {
        transport_target(config, runtime)
            .err()
            .map(|_| "configuration_not_admissible")
    } else {
        Some("runtime_not_enabled")
    };
    let adapter=runtime.map(|r|{
        let structured=r.output_mode==OutputMode::StructuredLocal;
        json!({"id":if structured{"local_json_schema_sse_v01"}else{"chat_completions_json_object_v01"},
            "protocol":"openai_compatible_chat_completions","request_constraint":if structured{"json_schema"}else{"json_object"},
            "streaming":structured,"requires_finish_stop":true,"requires_sse_done":structured,
            "source_quote_validation":true,"independent_semantic_validation":false,
            "diagnostic_quote_text_profile_selected":false,
            "usage":{"token_usage_requested":structured,"token_usage_parsed":true,"normal_worker_persists_usage":true,
                "cache_detail_parsed":!structured,"cache_token_split":null,"supplier_price":null,"total_cost":null,"missing_usage_is_zero":false,
                "persistence_basis":"encrypted_per_attempt_observations_not_billing"},
            "limits":{"request_max_output_tokens":DEFAULT_MAX_OUTPUT_TOKENS,"transport_timeout_seconds":workspace_timeout_seconds(&config.selection),
                "connection_timeout_seconds":3,"maximum_calls":r.maximum_calls,"request_bytes":131072,
                "model_content_bytes":nova_core::intake::MAX_MODEL_BYTES},
            "cancellation":{"user_cancel_endpoint":false,"transport_deadline":true,"provider_compute_stopped":null},
            "remote_compatible":!structured})
    });
    json!({"schema_version":"nova.provider-capabilities.v0.1","configuration_revision":config.revision,
        "selected_model":model,"configuration_admissible":blocker.is_none(),"blocker":blocker,
        "runtime_configured":runtime.is_some(),"adapter":adapter,
        "processing":{"configured_location":location,"actual_processing_location":"unverified","region":null},
        "observations":{"service_probed":false,"service_reachable":null,"provider_version":null,"provider_capabilities_verified":false,
            "model_quality":"unverified_for_this_configuration","live_dispatch_performed":false},
        "fallback":{"automatic":false,"automatic_retry":false,"changes_model":false,"changes_location":false,
            "policy":"fail_closed_preserve_source_explicit_owner_configuration_required"},
        "authority":{"read_grant_created":false,"model_call_started":false,"pricing_approved":false},
        "basis":"compiled_adapter_and_saved_configuration_only"})
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn config(remote: bool) -> ModelConfiguration {
        ModelConfiguration {
            revision: 1,
            selection: if remote {
                ModelSelection::Remote {
                    model_id: "synthetic".into(),
                    endpoint: "https://provider.invalid/v1".into(),
                    allow_selected_source: true,
                }
            } else {
                ModelSelection::Local {
                    model_id: "synthetic".into(),
                    endpoint: "http://127.0.0.1:9/v1".into(),
                }
            },
        }
    }
    #[test]
    fn unknowns_do_not_become_free_or_ready() {
        let c = config(false);
        let r = describe(&c, None, false);
        assert_eq!(r["blocker"], "runtime_not_enabled");
        assert!(r["adapter"].is_null());
        assert!(r["observations"]["service_reachable"].is_null());
        assert_eq!(r["fallback"]["automatic"], false);
        assert_eq!(r["authority"]["model_call_started"], false);
        assert_eq!(
            describe(&c, Some(&ModelRuntime::local(2)), true)["blocker"],
            "backup_quarantined"
        );
    }
    #[test]
    fn modes_are_distinct_from_provider_acceptance() {
        for (r, structured) in [
            (ModelRuntime::local(2), false),
            (ModelRuntime::structured_local(2), true),
        ] {
            let report = describe(&config(false), Some(&r), false);
            assert_eq!(report["configuration_admissible"], true);
            assert_eq!(report["adapter"]["streaming"], structured);
            assert_eq!(report["adapter"]["limits"]["transport_timeout_seconds"], 90);
            assert_eq!(report["adapter"]["usage"]["token_usage_parsed"], true);
            assert_eq!(
                report["adapter"]["usage"]["cache_detail_parsed"],
                !structured
            );
            assert_eq!(
                report["adapter"]["usage"]["normal_worker_persists_usage"],
                true
            );
            assert!(report["adapter"]["usage"]["total_cost"].is_null());
            assert_eq!(
                report["observations"]["provider_capabilities_verified"],
                false
            );
            assert_eq!(
                report["adapter"]["diagnostic_quote_text_profile_selected"],
                false
            );
        }
        let remote = describe(&config(true), Some(&ModelRuntime::local(2)), false);
        assert_eq!(remote["adapter"]["limits"]["transport_timeout_seconds"], 45);
    }
    #[test]
    fn remote_binding_and_protocol_are_checked_without_reading_credentials() {
        let mut r = ModelRuntime::local(4);
        r.credential_file = Some(PathBuf::from("/nonexistent/SECRET_CREDENTIAL_FILE"));
        r.credential_origin = Some("https://provider.invalid".into());
        let c = config(true);
        assert!(transport_target(&c, &r).is_ok());
        let report = describe(&c, Some(&r), false);
        assert_eq!(report["configuration_admissible"], true);
        assert!(!report.to_string().contains("SECRET_CREDENTIAL"));
        assert!(!report.to_string().contains("provider.invalid"));
        r.credential_origin = Some("https://other.invalid".into());
        assert!(transport_target(&c, &r).is_err());
        assert_eq!(
            describe(&c, Some(&r), false)["configuration_admissible"],
            false
        );
        assert!(transport_target(&c, &ModelRuntime::structured_local(4)).is_err());
    }
    #[test]
    fn no_consent_invalid_endpoint_or_runtime_never_becomes_admissible() {
        let mut c = config(true);
        if let ModelSelection::Remote {
            allow_selected_source,
            ..
        } = &mut c.selection
        {
            *allow_selected_source = false;
        }
        assert!(transport_target(&c, &ModelRuntime::local(4)).is_err());
        let mut c = config(false);
        if let ModelSelection::Local { endpoint, .. } = &mut c.selection {
            *endpoint = "http://127.0.0.1:9/v1?credential=SECRET".into();
        }
        let r = describe(&c, Some(&ModelRuntime::local(4)), false);
        assert_eq!(r["configuration_admissible"], false);
        assert!(!r.to_string().contains("SECRET"));
        assert!(transport_target(&config(false), &ModelRuntime::local(0)).is_err());
    }
}
