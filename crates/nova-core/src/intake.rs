//! Explicitly created, independent automatic-intake store. Never migrates a legacy Vault.
//! N1 persists originals/jobs; N2a commits bounded semantic outputs with versioned provenance.
mod compiler;
mod capacity;
mod capture;
mod capture_provenance;
mod connections;
mod context_pack;
mod continuity;
mod consolidation;
mod diagnostics;
mod recovery;
mod portable;
mod evaluation;
mod graph;
mod hook_install;
mod maintenance;
mod metering;
mod report;
mod retrieval;
mod relay;
mod storage;
mod worker_queue;
use crate::compilation::*;
use crate::{PrivacyLevel, Result};
pub use compiler::*;
pub use capacity::*;
pub use capture::*;
pub use capture_provenance::*;
pub use connections::*;
pub use context_pack::*;
pub use continuity::*;
pub use consolidation::*;
pub use diagnostics::*;
pub use recovery::*;
pub use portable::*;
pub use evaluation::*;
pub use graph::*;
pub use hook_install::*;
pub use maintenance::*;
pub use metering::*;
pub use report::*;
pub use retrieval::*;
pub use relay::*;
use serde::{Deserialize, Serialize};

pub use storage::IntakeStore;
pub use worker_queue::{MAX_WORKER_QUEUE_ROWS, WorkerQueueBatch, WorkerQueueCursor};
pub const STORE_VERSION: &str = "nova.intake-store.v0.1";
pub const MAX_ATTEMPTS: u32 = 5;

/// Obtained from authenticated runtime configuration, never deserialized from capture input.
#[derive(Debug, Clone)]
pub struct IntakeAuthority {
    pub scope: AssetScope,
    pub actor_id: String,
    pub capture_allowed: bool,
    pub role: SourceRole,
    pub privacy_level: PrivacyLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "location", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelSelection {
    #[default]
    Unconfigured,
    Local {
        model_id: String,
        endpoint: String,
    },
    Remote {
        model_id: String,
        endpoint: String,
        allow_selected_source: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfiguration {
    pub revision: u64,
    pub selection: ModelSelection,
}
impl ModelConfiguration {
    pub fn validate(&self) -> Result<()> {
        if self.revision == 0 {
            return Err(invalid("invalid_configuration"));
        }
        let (model, endpoint, local) = match &self.selection {
            ModelSelection::Unconfigured => return Ok(()),
            ModelSelection::Local { model_id, endpoint } => (model_id, endpoint, true),
            ModelSelection::Remote {
                model_id,
                endpoint,
                allow_selected_source: true,
            } => (model_id, endpoint, false),
            ModelSelection::Remote { .. } => {
                return Err(invalid("selected_source_consent_required"));
            }
        };
        if model.trim().is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
            return Err(invalid("invalid_model"));
        }
        // A constrained endpoint description, not an arbitrary URL / credential container.
        // N2 must revalidate DNS/redirects and permissions before any network execution.
        let (scheme, rest) = endpoint
            .split_once("://")
            .ok_or_else(|| invalid("invalid_endpoint"))?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        if endpoint.len() > 512
            || endpoint
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || authority.is_empty()
            || endpoint.contains(['@', '?', '#', '\\'])
            || path.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err(invalid("invalid_endpoint"));
        }
        let loopback = authority
            .strip_prefix("127.0.0.1:")
            .or_else(|| authority.strip_prefix("[::1]:"));
        if local {
            if scheme != "http"
                || loopback
                    .and_then(|p| p.parse::<u16>().ok())
                    .is_none_or(|p| p == 0)
            {
                return Err(invalid("local_loopback_endpoint_required"));
            }
        } else if scheme != "https" || authority.starts_with(['[', '.']) || !authority.contains('.')
        {
            return Err(invalid("remote_https_endpoint_required"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobEvent {
    pub sequence: u32,
    pub at: String,
    pub state: JobState,
    pub attempt: u32,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLease {
    pub lease_id: String,
    pub expires_at: String,
    pub configuration_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeJob {
    pub job: CompilationJob,
    pub receipt: IntakeReceipt,
    pub actor_id: String,
    pub updated_at: String,
    pub lease: Option<WorkerLease>,
    pub retry_requested: bool,
    pub history: Vec<JobEvent>,
}

#[derive(Debug, Clone, Copy)]
pub enum AttemptFailure {
    ModelUnavailable,
    InvalidOutput,
    WorkerInterrupted,
    ConfigurationChanged,
    BudgetExceeded,
}
impl AttemptFailure {
    fn code(self) -> &'static str {
        match self {
            Self::ModelUnavailable => "model_unavailable",
            Self::InvalidOutput => "invalid_output",
            Self::WorkerInterrupted => "worker_interrupted",
            Self::ConfigurationChanged => "configuration_changed",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}

fn invalid(reason: &'static str) -> crate::NovaError {
    crate::NovaError::Validation(format!("intake: {reason}"))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
}
