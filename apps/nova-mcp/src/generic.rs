//! Three independently configured tools for the Codex-first local contract.
use super::*;
use nova_core::{QueryOutcome, QuerySelection, QuerySpec};
use sha2::{Digest, Sha256};
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub enum CaptureVersion {
    #[default]
    #[serde(rename = "nova.capture.v0.3")]
    V03,
}
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureOrigin {
    #[default]
    AiExtractedProposal,
    UserSelfReport,
    ModelInferred,
}
impl From<CaptureOrigin> for AssertionOrigin {
    fn from(v: CaptureOrigin) -> Self {
        match v {
            CaptureOrigin::AiExtractedProposal => Self::AiExtractedProposal,
            CaptureOrigin::UserSelfReport => Self::UserSelfReport,
            CaptureOrigin::ModelInferred => Self::ModelInferred,
        }
    }
}
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveMemoryInput {
    pub schema_version: CaptureVersion,
    pub assertion_origin: CaptureOrigin,
    /// Current interaction surface; grants no hardware or action authority.
    pub surface: ToolContextSurface,
    /// Only an AI digest is accepted; this does not claim a full transcript or raw audio.
    pub capture_fidelity: ToolCaptureFidelity,
    /// Optional opaque reference, never a URL, credential, or raw source. Nova does not fetch it.
    #[serde(default)]
    pub source_ref: Option<String>,
    /// Must be true only after the user explicitly asks to save this conversation to Nova.
    pub user_requested_save: bool,
    /// Stable idempotency key for this save attempt. Reuse it only when retrying identical input.
    #[schemars(length(min = 1, max = 96))]
    pub request_id: String,
    /// Short human-readable title for later review.
    #[schemars(length(min = 1, max = 240))]
    pub title: String,
    /// Primary semantic type of the proposed memory.
    pub memory_type: ToolMemoryType,
    /// Concise, faithful summary. Do not add facts that were not present in the conversation.
    #[schemars(length(min = 1, max = 4000))]
    pub summary: String,
    /// Structured digest of the main conversation content, not a raw audio recording.
    #[schemars(length(min = 1, max = 24000))]
    pub session_digest: String,
    /// Important facts, experiences, constraints, or ideas stated in the conversation.
    #[serde(default)]
    pub key_points: Vec<String>,
    /// Decisions actually made. Do not turn suggestions into decisions.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// Reasons explicitly stated for the decisions.
    #[serde(default)]
    pub rationales: Vec<String>,
    /// Questions or uncertainties that remain unresolved.
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// Follow-up actions explicitly agreed or requested.
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// Sensitivity assigned to the pending proposal. Default to private when uncertain.
    pub privacy_level: ToolPrivacyLevel,
    /// ISO-8601 timestamp for when the conversation occurred, when known.
    #[serde(default)]
    pub occurred_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub enum QueryVersion {
    #[default]
    #[serde(rename = "nova.query.v0.1")]
    V01,
}
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryView {
    #[default]
    Confirmed,
    CaptureStatus,
}
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMemoryInput {
    pub schema_version: QueryVersion,
    #[schemars(length(min = 1, max = 96))]
    pub request_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub approval_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub purpose: String,
    #[schemars(length(min = 1, max = 128))]
    pub memory_id: String,
    #[serde(default)]
    pub view: QueryView,
}
fn default_max_items() -> usize {
    5
}
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchMemoryInput {
    pub schema_version: QueryVersion,
    #[schemars(length(min = 1, max = 96))]
    pub request_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub approval_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub purpose: String,
    #[schemars(length(min = 1, max = 1000))]
    pub query: String,
    #[serde(default = "default_max_items")]
    #[schemars(range(min = 1, max = 10))]
    pub max_items: usize,
}
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SaveMemoryOutput {
    /// Receipt-backed persistence; retries return original receipt and saved_at.
    pub status: String,
    pub schema_version: String,
    pub request_id: String,
    pub memory_id: String,
    pub proposal_id: String,
    pub source_id: String,
    pub canonical_memory_id: Option<String>,
    pub proposal_status: String,
    pub receipt_id: String,
    pub capture_fidelity: String,
    pub assertion_origin: String,
    pub needs_confirmation: bool,
    pub saved_at: String,
    pub occurred_at: Option<String>,
    pub content_digest: String,
}
/// Operation and client namespaces are constructed by the service, never tool input.
pub fn namespaced_request(operation: &str, request_id: &str) -> String {
    namespaced_request_for("codex", operation, request_id)
}
pub fn namespaced_request_for(client: &str, operation: &str, request_id: &str) -> String {
    format!(
        "nova3:{client}:{operation}:{:x}",
        Sha256::digest(request_id.as_bytes())
    )
}
pub fn query_spec(request_id: &str, purpose: &str, selection: QuerySelection) -> QuerySpec {
    query_spec_for("codex", request_id, purpose, selection)
}
pub fn query_spec_for(
    client: &str,
    request_id: &str,
    purpose: &str,
    selection: QuerySelection,
) -> QuerySpec {
    QuerySpec {
        subject: "owner:founder".into(),
        client: client.into(),
        space: "personal".into(),
        request_id: namespaced_request_for(client, "query", request_id),
        purpose: purpose.into(),
        selection,
    }
}
pub fn query_spec_bound(
    binding: &LocalClientBinding,
    request_id: &str,
    purpose: &str,
    selection: QuerySelection,
) -> QuerySpec {
    let client = binding.generic_client().unwrap_or("unavailable");
    let mut spec = query_spec_for(client, request_id, purpose, selection);
    spec.subject = binding.owner().into();
    spec.space = binding.space().into();
    spec.request_id = request_bound(binding, "query", request_id);
    spec
}
pub fn request_bound(binding: &LocalClientBinding, operation: &str, id: &str) -> String {
    let client = binding.generic_client().unwrap_or("unavailable");
    if binding.owner() == "owner:founder" && binding.space() == "personal" {
        return namespaced_request_for(client, operation, id);
    }
    let identity = serde_json::to_vec(&(binding.owner(), binding.space(), client, id))
        .expect("tuple serialization");
    format!("nova4:{operation}:{:x}", Sha256::digest(identity))
}
#[derive(Debug, Clone)]
pub struct GenericMcpServer {
    service: ApplicationService,
    tool_router: ToolRouter<Self>,
}
impl GenericMcpServer {
    pub fn with_service(service: ApplicationService) -> Self {
        Self {
            service,
            tool_router: Self::tool_router(),
        }
    }
    pub fn new(vault_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            service: ApplicationService::new(
                LocalClientBinding::codex_local(),
                Arc::new(LocalVaultPort::new(vault_path, key_path)),
            ),
            tool_router: Self::tool_router(),
        }
    }
    pub fn new_chatgpt(vault: &std::path::Path, key: &std::path::Path) -> Self {
        let mut server = Self::new(vault, key);
        server.service = ApplicationService::new(
            LocalClientBinding::chatgpt_generic_local(),
            std::sync::Arc::new(crate::service::LocalVaultPort::new(vault, key)),
        );
        server
    }
    pub fn with_binding(
        vault: impl Into<PathBuf>,
        key: impl Into<PathBuf>,
        binding: LocalClientBinding,
    ) -> Self {
        Self {
            service: ApplicationService::new(binding, Arc::new(LocalVaultPort::new(vault, key))),
            tool_router: Self::tool_router(),
        }
    }
    pub fn verify_configuration(&self) -> Result<(), String> {
        self.service.verify_configuration().map_err(safe_nova_error)
    }
}
fn query_error(error: NovaError) -> String {
    match error {
        NovaError::PolicyDenied(_)
        | NovaError::NotFound { .. }
        | NovaError::IdempotencyReplayUnavailable { .. } => {
            "NOVA_RESOURCE_UNAVAILABLE: requested resource is unavailable".into()
        }
        _ => safe_nova_error(error),
    }
}
fn check_query(request_id: &str, approval_id: &str, purpose: &str) -> Result<(), String> {
    validate_text("request_id", request_id, 96)?;
    validate_text("approval_id", approval_id, 128)?;
    validate_text("purpose", purpose, 128)
}
#[tool_router]
impl GenericMcpServer {
    /// Explicitly requested F1 digest capture. Creates only a pending proposal, never confirms memory.
    #[tool(
        name = "save_memory",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn save_memory(
        &self,
        Parameters(input): Parameters<SaveMemoryInput>,
    ) -> Result<Json<SaveMemoryOutput>, String> {
        validate_text("request_id", &input.request_id, 96)?;
        if let Some(reference) = &input.source_ref
            && (reference.is_empty()
                || reference.len() > 128
                || !reference
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b)))
        {
            return Err("NOVA_VALIDATION_FAILED: invalid opaque source_ref".into());
        }
        let legacy = SaveVoiceSessionDigestInput {
            user_requested_save: input.user_requested_save,
            request_id: input.request_id.clone(),
            title: input.title.clone(),
            memory_type: input.memory_type,
            summary: input.summary.clone(),
            session_digest: input.session_digest.clone(),
            key_points: input.key_points.clone(),
            decisions: input.decisions.clone(),
            rationales: input.rationales.clone(),
            open_questions: input.open_questions.clone(),
            next_steps: input.next_steps.clone(),
            privacy_level: input.privacy_level,
            observed_at: input.occurred_at.clone(),
        };
        validate_save_input(&legacy)?;
        let client = self
            .service
            .binding()
            .generic_client()
            .ok_or("NOVA_RESOURCE_UNAVAILABLE")?;
        let mut source = serde_json::to_value(&input).map_err(|_| "NOVA_SERIALIZATION_FAILED")?;
        source
            .as_object_mut()
            .unwrap()
            .insert("client".into(), serde_json::json!(client));
        let privacy: PrivacyLevel = input.privacy_level.into();
        let origin: AssertionOrigin = input.assertion_origin.into();
        let (outcome, canonical_id) = self
            .service
            .capture_generic(
                CaptureRequest {
                    structure: None,
                    request_id: Some(request_bound(
                        self.service.binding(),
                        "save",
                        &input.request_id,
                    )),
                    owner_id: self.service.binding().owner().into(),
                    source_platform: client.into(),
                    capture_channel: CaptureChannel::RemoteMcp,
                    capture_fidelity: CaptureFidelity::F1AiDigest,
                    source_content: source.to_string(),
                    title: input.title,
                    memory_type: input.memory_type.into(),
                    summary: input.summary,
                    decisions: input.decisions,
                    privacy_level: privacy,
                    proposed_origin: origin,
                    review_tier: match privacy {
                        PrivacyLevel::Shareable => ReviewTier::Routine,
                        PrivacyLevel::Private => ReviewTier::Attention,
                        _ => ReviewTier::Restricted,
                    },
                    permission_scope: vec![
                        "personal_review".into(),
                        format!("client:{client}"),
                        format!("space:{}", self.service.binding().space()),
                    ],
                    purpose_limits: vec!["personal_memory_continuity".into()],
                    observed_at: input.occurred_at.clone(),
                },
                input.user_requested_save,
            )
            .map_err(safe_nova_error)?;
        Ok(Json(SaveMemoryOutput {
            status: "persisted_capture_receipt".into(),
            schema_version: "nova.capture.result.v0.3".into(),
            request_id: input.request_id,
            memory_id: outcome.proposal.proposal_id.clone(),
            proposal_id: outcome.proposal.proposal_id,
            source_id: outcome.source.source_id,
            canonical_memory_id: canonical_id,
            proposal_status: outcome.proposal.status.as_str().into(),
            receipt_id: outcome.receipt.envelope.receipt_id,
            capture_fidelity: "f1_ai_digest".into(),
            assertion_origin: outcome.proposal.proposed_origin.as_str().into(),
            needs_confirmation: outcome.proposal.status == ProposalStatus::Pending,
            saved_at: outcome.source.created_at,
            occurred_at: input.occurred_at,
            content_digest: outcome.source.content_hash,
        }))
    }
    /// Get an owner-approved confirmed summary using an exact request or collection grant, or explicitly approved own capture status. Writes a disclosure receipt.
    #[tool(
        name = "get_memory",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn get_memory(
        &self,
        Parameters(input): Parameters<GetMemoryInput>,
    ) -> Result<Json<serde_json::Value>, String> {
        check_query(&input.request_id, &input.approval_id, &input.purpose)?;
        validate_text("memory_id", &input.memory_id, 128)?;
        let outcome: QueryOutcome = self
            .service
            .query_generic(
                query_spec_bound(
                    self.service.binding(),
                    &input.request_id,
                    &input.purpose,
                    QuerySelection::Get {
                        memory_id: input.memory_id,
                        capture_status: matches!(input.view, QueryView::CaptureStatus),
                    },
                ),
                &input.approval_id,
            )
            .map_err(query_error)?;
        Ok(Json(
            serde_json::to_value(outcome).map_err(|_| "NOVA_SERIALIZATION_FAILED")?,
        ))
    }
    /// Lexical search within an owner-approved fixed list. Only active confirmed minimum summaries; writes a receipt.
    #[tool(
        name = "search_memory",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn search_memory(
        &self,
        Parameters(input): Parameters<SearchMemoryInput>,
    ) -> Result<Json<serde_json::Value>, String> {
        check_query(&input.request_id, &input.approval_id, &input.purpose)?;
        validate_text("query", &input.query, 1000)?;
        if !(1..=10).contains(&input.max_items) {
            return Err("NOVA_VALIDATION_FAILED: max_items must be 1..10".into());
        }
        let outcome = self
            .service
            .query_generic(
                query_spec_bound(
                    self.service.binding(),
                    &input.request_id,
                    &input.purpose,
                    QuerySelection::Search {
                        query: input.query,
                        max_items: input.max_items,
                    },
                ),
                &input.approval_id,
            )
            .map_err(query_error)?;
        Ok(Json(
            serde_json::to_value(outcome).map_err(|_| "NOVA_SERIALIZATION_FAILED")?,
        ))
    }
}
#[tool_handler(router = self.tool_router)]
impl ServerHandler for GenericMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new("nova-memory-local-v01","0.1.0")).with_instructions("Configured personal Vault. Save only when asked. Capture stays pending. Reads require owner-issued exact-request approval IDs from the local CLI. No confirmation or approval tool is exposed.")
    }
}
