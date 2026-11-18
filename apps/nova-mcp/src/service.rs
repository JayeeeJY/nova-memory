//! Minimum application boundary. Trusted local configuration supplies the binding;
//! a model's Host/owner strings and initialize.clientInfo never authenticate it.
//! Codex operations require the separately guarded transport and owner-issued query approval.
use std::{fmt, path::PathBuf, sync::Arc};

use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureOutcome, CaptureRequest,
    ContextOutcome, ContextRequest, NovaError, PrivacyLevel, Result, Vault, VaultKey,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    LegacyChatgpt,
    Codex,
    GenericChatgpt,
    Registered,
}

/// Constructed by the local control plane, never deserialized from tool input.
/// It names a configured policy binding, not verified process identity or OAuth.
#[derive(Debug, Clone)]
pub struct LocalClientBinding {
    client: ClientKind,
    owner: String,
    space: String,
    generic: Option<String>,
    capabilities: Vec<String>,
}
impl LocalClientBinding {
    pub fn legacy_chatgpt() -> Self {
        Self {
            client: ClientKind::LegacyChatgpt,
            owner: "owner:founder".into(),
            space: "personal".into(),
            generic: None,
            capabilities: vec!["capture".into(), "query".into()],
        }
    }
    pub fn codex_pending() -> Self {
        Self::codex_local()
    }
    pub fn codex_local() -> Self {
        Self {
            client: ClientKind::Codex,
            owner: "owner:founder".into(),
            space: "personal".into(),
            generic: None,
            capabilities: vec!["capture".into(), "query".into()],
        }
    }
    pub fn chatgpt_generic_local() -> Self {
        Self {
            client: ClientKind::GenericChatgpt,
            owner: "owner:founder".into(),
            space: "personal".into(),
            generic: None,
            capabilities: vec!["capture".into(), "query".into()],
        }
    }
    pub fn registered(
        owner: String,
        space: String,
        client: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            client: ClientKind::Registered,
            owner,
            space,
            generic: Some(client),
            capabilities,
        }
    }
    pub fn generic_client(&self) -> Option<&str> {
        match self.client {
            ClientKind::Codex => Some("codex"),
            ClientKind::GenericChatgpt => Some("chatgpt"),
            ClientKind::Registered => self.generic.as_deref(),
            _ => None,
        }
    }
    pub fn allows(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
    pub fn client(&self) -> ClientKind {
        self.client
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn space(&self) -> &str {
        &self.space
    }
    fn require_legacy(&self) -> Result<()> {
        if self.client != ClientKind::LegacyChatgpt {
            return Err(NovaError::PolicyDenied(
                "legacy operations require the legacy binding; Codex is not enabled".into(),
            ));
        }
        Ok(())
    }
}

/// A short-lived local unit of work. Vault owns atomic transactions and audit.
/// No canonical confirmation/edit/delete method is available to the MCP service.
pub trait VaultSession {
    fn canonical_for_capture(&self, _proposal_id: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn capture(&mut self, request: CaptureRequest) -> Result<CaptureOutcome>;
    fn prepare_context(&mut self, request: ContextRequest) -> Result<ContextOutcome>;
    fn query(
        &mut self,
        _spec: nova_core::QuerySpec,
        _approval_id: &str,
    ) -> Result<nova_core::QueryOutcome> {
        Err(NovaError::PolicyDenied("query is unavailable".into()))
    }
}
impl VaultSession for Vault {
    fn canonical_for_capture(&self, proposal_id: &str) -> Result<Option<String>> {
        self.capture_canonical_id(proposal_id)
    }
    fn query(
        &mut self,
        spec: nova_core::QuerySpec,
        approval_id: &str,
    ) -> Result<nova_core::QueryOutcome> {
        self.query_memory(spec, approval_id)
    }
    fn capture(&mut self, request: CaptureRequest) -> Result<CaptureOutcome> {
        Vault::capture(self, request)
    }
    fn prepare_context(&mut self, request: ContextRequest) -> Result<ContextOutcome> {
        Vault::prepare_context(self, request)
    }
}

pub trait VaultPort: Send + Sync {
    /// Open an existing store. Never initialize, reset or export the key.
    fn open(&self) -> Result<Box<dyn VaultSession>>;
}

#[derive(Clone)]
pub struct LocalVaultPort {
    vault_path: PathBuf,
    key_path: PathBuf,
}
impl LocalVaultPort {
    pub fn new(vault_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            vault_path: vault_path.into(),
            key_path: key_path.into(),
        }
    }
}
impl fmt::Debug for LocalVaultPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalVaultPort { paths: [REDACTED] }")
    }
}
impl VaultPort for LocalVaultPort {
    fn open(&self) -> Result<Box<dyn VaultSession>> {
        let key = VaultKey::read_file(&self.key_path)?;
        Ok(Box::new(Vault::open(&self.vault_path, key)?))
    }
}

#[derive(Clone)]
pub struct ApplicationService {
    binding: LocalClientBinding,
    store: Arc<dyn VaultPort>,
}
impl fmt::Debug for ApplicationService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationService")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}
impl ApplicationService {
    pub fn new(binding: LocalClientBinding, store: Arc<dyn VaultPort>) -> Self {
        Self { binding, store }
    }
    pub fn binding(&self) -> &LocalClientBinding {
        &self.binding
    }
    pub fn verify_configuration(&self) -> Result<()> {
        self.store.open().map(|_| ())
    }

    pub fn capture_generic(
        &self,
        request: CaptureRequest,
        explicit_save: bool,
    ) -> Result<(CaptureOutcome, Option<String>)> {
        self.capture_bound(request, explicit_save, false)
    }
    pub fn capture_web(
        &self,
        request: CaptureRequest,
        explicit_save: bool,
    ) -> Result<(CaptureOutcome, Option<String>)> {
        self.capture_bound(request, explicit_save, true)
    }
    fn capture_bound(
        &self,
        request: CaptureRequest,
        explicit_save: bool,
        web: bool,
    ) -> Result<(CaptureOutcome, Option<String>)> {
        if !self.binding.allows("capture")
            || self.binding.generic_client().is_none()
            || !explicit_save
            || request.owner_id != self.binding.owner
            || Some(request.source_platform.as_str()) != self.binding.generic_client()
            || request.permission_scope
                != [
                    "personal_review".to_owned(),
                    format!(
                        "client:{}",
                        self.binding.generic_client().unwrap_or("unavailable")
                    ),
                    format!("space:{}", self.binding.space),
                ]
            || request.purpose_limits != ["personal_memory_continuity"]
            || request.capture_channel
                != if web {
                    CaptureChannel::WebAdapter
                } else {
                    CaptureChannel::RemoteMcp
                }
            || request.capture_fidelity
                != if web {
                    CaptureFidelity::F2VisibleTranscript
                } else {
                    CaptureFidelity::F1AiDigest
                }
            || !matches!(
                request.proposed_origin,
                AssertionOrigin::AiExtractedProposal
                    | AssertionOrigin::UserSelfReport
                    | AssertionOrigin::ModelInferred
            )
        {
            return Err(NovaError::PolicyDenied(
                "generic capture binding mismatch".into(),
            ));
        }
        if request.source_content.len() > 64 * 1024 {
            return Err(NovaError::Validation(
                "structured source exceeds 65536 bytes".into(),
            ));
        }
        let mut session = self.store.open()?;
        let outcome = session.capture(request)?;
        let canonical_id = session.canonical_for_capture(&outcome.proposal.proposal_id)?;
        Ok((outcome, canonical_id))
    }
    pub fn query_generic(
        &self,
        spec: nova_core::QuerySpec,
        approval_id: &str,
    ) -> Result<nova_core::QueryOutcome> {
        if !self.binding.allows("query")
            || self.binding.generic_client().is_none()
            || spec.subject != self.binding.owner
            || Some(spec.client.as_str()) != self.binding.generic_client()
            || spec.space != self.binding.space
        {
            return Err(NovaError::PolicyDenied("NOVA_RESOURCE_UNAVAILABLE".into()));
        }
        self.store.open()?.query(spec, approval_id)
    }

    /// Legacy replay uses the EXACT original request/serialization. No namespace
    /// prefix, ownership rewrite or new metadata is inserted in this path.
    pub fn capture_legacy(
        &self,
        request: CaptureRequest,
        explicit_save: bool,
    ) -> Result<CaptureOutcome> {
        self.binding.require_legacy()?;
        if !explicit_save
            || request.owner_id != self.binding.owner
            || request.source_platform != "chatgpt"
            || request.capture_channel != CaptureChannel::RemoteMcp
            || request.capture_fidelity != CaptureFidelity::F1AiDigest
            || request.proposed_origin != AssertionOrigin::AiExtractedProposal
        {
            return Err(NovaError::PolicyDenied(
                "capture intent or configured subject/client mismatch".into(),
            ));
        }
        if request.source_content.len() > 64 * 1024 {
            return Err(NovaError::Validation(
                "structured source exceeds 65536 bytes".into(),
            ));
        }
        self.store.open()?.capture(request)
    }
    /// Approval bool retains the original Alpha contract. It is not a new
    /// owner-issued grant. The future get/search grant path must be separate.
    pub fn context_legacy(&self, request: ContextRequest) -> Result<ContextOutcome> {
        self.binding.require_legacy()?;
        if !request.user_approved
            || request.subject != self.binding.owner
            || request.recipient != "chatgpt"
            || !matches!(
                request.maximum_privacy_level,
                PrivacyLevel::Private | PrivacyLevel::Shareable
            )
        {
            return Err(NovaError::PolicyDenied(
                "context approval or configured subject/recipient mismatch".into(),
            ));
        }
        self.store.open()?.prepare_context(request)
    }
}
