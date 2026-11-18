//! Local worker uses the shared bounded model transport.
use crate::ReviewServer;
use nova_core::{NovaError, VaultKey, compilation::AssetScope, intake::*};
pub use nova_model_runtime::compiler::*;
use std::time::Duration;
fn error(code: &'static str) -> NovaError {
    NovaError::Validation(format!("compiler: {code}"))
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
            .ok_or_else(|| error("intake_not_enabled"))?,
        VaultKey::read_file(&s.key_path)?,
        scope(s),
    )
}

pub(super) fn start(s: ReviewServer) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if s.model_runtime.is_none() {
                break;
            }
            // One worker per server; transaction leases also fence independent processes.
            let _ = run_once(&s).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}
/// Public for integration tests / explicit single-step tools. Never installs a model or sends all-Vault data.
pub async fn run_once(s: &ReviewServer) -> Result<Option<String>, NovaError> {
    let runtime = s
        .model_runtime
        .as_ref()
        .ok_or_else(|| error("runtime_not_enabled"))?
        .clone();
    let claimed = {
        let _guard = s.lock.lock().await;
        let mut store = store(s)?;
        store.consolidate_pending(&scope(s), 4)?;
        let mut cursor = s.worker_queue_cursor.lock().await;
        let batch =
            store.claim_next_batch(&scope(s), 120, cursor.as_ref(), MAX_WORKER_QUEUE_ROWS)?;
        *cursor = batch.next_cursor;
        batch.claimed
    };
    let Some(job) = claimed else { return Ok(None) };
    let lease = job
        .lease
        .as_ref()
        .ok_or_else(|| error("lease_missing"))?
        .lease_id
        .clone();
    let result = async {
        let prepared = {
            let _guard = s.lock.lock().await;
            store(s)?.prepare_compilation(&scope(s), &job.job.job_id, &lease)?
        };
        // Endpoints and credentials are selected exclusively from saved Owner config and CLI binding.
        let copy = prepared.clone();
        let policy = runtime.clone();
        let transport =
            tokio::task::spawn_blocking(move || prepare_workspace_transport(&copy, &policy))
                .await
                .map_err(|_| error("transport_setup_failed"))??;
        transport
            .validate_credential(|credential| s.ensure_model_credential_separate(credential))?;
        let attempt = {
            let _guard = s.lock.lock().await;
            store(s)?.authorize_metered_dispatch(
                &scope(s),
                &job.job.job_id,
                &lease,
                runtime.maximum_calls,
                match runtime.output_mode {
                    OutputMode::JsonObject => CompilationTransport::OpenaiCompatibleJson,
                    OutputMode::StructuredLocal => CompilationTransport::OpenaiCompatibleLocalSse,
                },
            )?
        };
        if attempt.configuration != prepared.configuration {
            return Err(error("configuration_changed"));
        }
        // No auth/config mutation waits on network. In-flight responses are rechecked at commit.
        let observed = tokio::task::spawn_blocking(move || generate(transport))
            .await
            .unwrap_or_else(|_| ObservedGeneration::interrupted());
        let _guard = s.lock.lock().await;
        let mut store = store(s)?;
        // Persist provider facts BEFORE attempting semantic commit. Invalid output, a changed
        // configuration or a withdrawn source must not erase already incurred provider usage.
        store.record_compilation_observation(
            &scope(s),
            &job.job.job_id,
            attempt.attempt,
            &lease,
            observed.observation,
        )?;
        let bytes = observed.result?;
        store.commit_compilation(&scope(s), &job.job.job_id, &lease, &bytes)?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let failure = match &e {
            NovaError::InvalidState(_) => AttemptFailure::ConfigurationChanged,
            NovaError::Validation(m) if m.contains("budget") => AttemptFailure::BudgetExceeded,
            NovaError::Validation(m)
                if m.starts_with("compiler: generation_")
                    && ![
                        "client_deadline",
                        "transport_",
                        "http_status",
                        "owned_child_",
                        "provider_error",
                    ]
                    .iter()
                    .any(|part| m.contains(part)) =>
            {
                AttemptFailure::InvalidOutput
            }
            NovaError::Validation(m) if m.starts_with("compiler:") => {
                AttemptFailure::ModelUnavailable
            }
            _ => AttemptFailure::InvalidOutput,
        };
        let _guard = s.lock.lock().await;
        // A stale/expired lease cannot mark a later attempt failed. Recovery handles expiry.
        let _ = store(s)?.fail_attempt(&scope(s), &job.job.job_id, &lease, failure);
        return Err(e);
    }
    Ok(Some(job.job.job_id))
}
