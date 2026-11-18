pub mod automatic;
pub mod capture;
pub mod generic;
pub mod registry;
pub mod service;
pub mod transport;

use service::{ApplicationService, LocalClientBinding, LocalVaultPort};
use std::{path::PathBuf, sync::Arc};

use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ContextRequest,
    ContextSurface, MemoryType, NovaError, PrivacyLevel, ProposalStatus, ReviewTier,
};
use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[cfg(test)]
const OWNER_ID: &str = "owner:founder";
#[cfg(test)]
use nova_core::{Vault, VaultKey};
const SOURCE_PLATFORM: &str = "chatgpt";
const MAX_SOURCE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct NovaMcpServer {
    service: ApplicationService,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolMemoryType {
    Fact,
    Preference,
    #[default]
    Decision,
    Goal,
    Commitment,
    Event,
    Lesson,
    Reflection,
}

impl From<ToolMemoryType> for MemoryType {
    fn from(value: ToolMemoryType) -> Self {
        match value {
            ToolMemoryType::Fact => Self::Fact,
            ToolMemoryType::Preference => Self::Preference,
            ToolMemoryType::Decision => Self::Decision,
            ToolMemoryType::Goal => Self::Goal,
            ToolMemoryType::Commitment => Self::Commitment,
            ToolMemoryType::Event => Self::Event,
            ToolMemoryType::Lesson => Self::Lesson,
            ToolMemoryType::Reflection => Self::Reflection,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolPrivacyLevel {
    Shareable,
    #[default]
    Private,
    Sensitive,
    Sealed,
}

impl From<ToolPrivacyLevel> for PrivacyLevel {
    fn from(value: ToolPrivacyLevel) -> Self {
        match value {
            ToolPrivacyLevel::Shareable => Self::Shareable,
            ToolPrivacyLevel::Private => Self::Private,
            ToolPrivacyLevel::Sensitive => Self::Sensitive,
            ToolPrivacyLevel::Sealed => Self::Sealed,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolContextSurface {
    #[default]
    ChatText,
    VoiceAudio,
    DesktopReview,
}

impl From<ToolContextSurface> for ContextSurface {
    fn from(value: ToolContextSurface) -> Self {
        match value {
            ToolContextSurface::ChatText => Self::ChatText,
            ToolContextSurface::VoiceAudio => Self::VoiceAudio,
            ToolContextSurface::DesktopReview => Self::DesktopReview,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolContextPrivacyCeiling {
    Shareable,
    #[default]
    Private,
}

impl From<ToolContextPrivacyCeiling> for PrivacyLevel {
    fn from(value: ToolContextPrivacyCeiling) -> Self {
        match value {
            ToolContextPrivacyCeiling::Shareable => Self::Shareable,
            ToolContextPrivacyCeiling::Private => Self::Private,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveVoiceSessionDigestInput {
    /// Must be true only after the user explicitly asks to save this conversation to Nova.
    pub user_requested_save: bool,
    /// Stable idempotency key for this save attempt. Reuse it only when retrying identical input.
    pub request_id: String,
    /// Short human-readable title for later review.
    pub title: String,
    /// Primary semantic type of the proposed memory.
    pub memory_type: ToolMemoryType,
    /// Concise, faithful summary. Do not add facts that were not present in the conversation.
    pub summary: String,
    /// Structured digest of the main conversation content, not a raw audio recording.
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
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveToNovaInput {
    /// Current Alpha accepts only ChatGPT. This declaration is not authenticated Host identity.
    pub host: ToolCaptureHost,
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
    pub request_id: String,
    /// Short human-readable title for later review.
    pub title: String,
    /// Primary semantic type of the proposed memory.
    pub memory_type: ToolMemoryType,
    /// Concise, faithful summary. Do not add facts that were not present in the conversation.
    pub summary: String,
    /// Structured digest of the main conversation content, not a raw audio recording.
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
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCaptureHost {
    #[default]
    Chatgpt,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCaptureFidelity {
    #[default]
    F1AiDigest,
}

impl From<SaveToNovaInput> for SaveVoiceSessionDigestInput {
    fn from(input: SaveToNovaInput) -> Self {
        Self {
            user_requested_save: input.user_requested_save,
            request_id: input.request_id,
            title: input.title,
            memory_type: input.memory_type,
            summary: input.summary,
            session_digest: input.session_digest,
            key_points: input.key_points,
            decisions: input.decisions,
            rationales: input.rationales,
            open_questions: input.open_questions,
            next_steps: input.next_steps,
            privacy_level: input.privacy_level,
            observed_at: input.observed_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SaveToNovaOutput {
    #[serde(flatten)]
    pub proposal: SaveVoiceSessionDigestOutput,
    pub request_id: String,
    pub host: ToolCaptureHost,
    pub surface: ToolContextSurface,
    pub source_ref: Option<String>,
    /// SHA-256 of the exact structured source bytes stored by Nova, not of the original dialogue.
    pub content_digest: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SaveVoiceSessionDigestOutput {
    pub proposal_status: String,
    pub status: String,
    pub source_id: String,
    pub proposal_id: String,
    pub receipt_id: String,
    pub capture_fidelity: String,
    pub needs_confirmation: bool,
    pub user_message: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareContextPackInput {
    /// Must be true only after the user explicitly approves this one Context Pack disclosure.
    pub user_approved: bool,
    /// Stable idempotency key for this delivery attempt. Reuse only for an identical retry.
    pub request_id: String,
    /// Purpose that must match each memory's purpose limits.
    pub purpose: String,
    /// User question or task used to select relevant confirmed memories.
    #[serde(default)]
    pub query: String,
    /// Current approved ChatGPT interaction surface. This grants no device capability.
    pub surface: ToolContextSurface,
    /// Empty means any memory type within the other restrictions.
    #[serde(default)]
    pub allowed_memory_types: Vec<ToolMemoryType>,
    /// M2 exposes only shareable or private memory. Sensitive and sealed cannot be requested.
    pub maximum_privacy_level: ToolContextPrivacyCeiling,
    /// Maximum number of summary-only memory items to disclose. Allowed range: 1 to 10.
    pub max_items: usize,
    /// Short-lived pack duration in seconds. Allowed range: 1 to 3600.
    pub expires_in_seconds: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ToolContextPackItem {
    pub memory_id: String,
    pub title: String,
    pub memory_type: String,
    pub summary: String,
    pub observed_at: String,
    pub assertion_origin: String,
    pub source_artifact_ids: Vec<String>,
    pub recall_reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PrepareContextPackOutput {
    pub status: String,
    pub context_pack_id: String,
    pub receipt_id: String,
    pub recipient: String,
    pub purpose: String,
    pub surface: String,
    pub expires_at: String,
    pub disclosure_class: String,
    pub items: Vec<ToolContextPackItem>,
    pub excluded_count: usize,
    pub user_message: String,
}

#[derive(Debug, Serialize)]
struct VoiceSessionSource<'a> {
    schema_version: &'static str,
    session_digest: &'a str,
    key_points: &'a [String],
    decisions: &'a [String],
    rationales: &'a [String],
    open_questions: &'a [String],
    next_steps: &'a [String],
}

#[derive(Debug, Serialize)]
struct NativeSessionSource<'a> {
    host: ToolCaptureHost,
    surface: ToolContextSurface,
    capture_fidelity: ToolCaptureFidelity,
    source_ref: &'a Option<String>,
    #[serde(flatten)]
    digest: VoiceSessionSource<'a>,
}

impl NovaMcpServer {
    pub fn new(vault_path: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            service: ApplicationService::new(
                LocalClientBinding::legacy_chatgpt(),
                Arc::new(LocalVaultPort::new(vault_path, key_path)),
            ),
            tool_router: Self::tool_router(),
        }
    }

    pub fn verify_configuration(&self) -> Result<(), String> {
        self.service.verify_configuration().map_err(safe_nova_error)
    }

    fn capture_digest(
        &self,
        input: SaveVoiceSessionDigestInput,
        source_content: String,
    ) -> Result<(SaveVoiceSessionDigestOutput, String), String> {
        if source_content.len() > MAX_SOURCE_BYTES {
            return Err(format!(
                "NOVA_VALIDATION_FAILED: structured digest must be at most {MAX_SOURCE_BYTES} bytes"
            ));
        }

        let privacy_level: PrivacyLevel = input.privacy_level.into();
        let review_tier = match privacy_level {
            PrivacyLevel::Shareable => ReviewTier::Routine,
            PrivacyLevel::Private => ReviewTier::Attention,
            PrivacyLevel::Sensitive | PrivacyLevel::Sealed => ReviewTier::Restricted,
        };
        let outcome = self
            .service
            .capture_legacy(
                CaptureRequest {
                    structure: None,
                    request_id: Some(input.request_id),
                    owner_id: self.service.binding().owner().into(),
                    source_platform: SOURCE_PLATFORM.into(),
                    capture_channel: CaptureChannel::RemoteMcp,
                    capture_fidelity: CaptureFidelity::F1AiDigest,
                    source_content,
                    title: input.title,
                    memory_type: input.memory_type.into(),
                    summary: input.summary,
                    decisions: input.decisions,
                    privacy_level,
                    proposed_origin: AssertionOrigin::AiExtractedProposal,
                    review_tier,
                    permission_scope: vec!["personal_review".into()],
                    purpose_limits: vec!["personal_memory_continuity".into()],
                    observed_at: input.observed_at,
                },
                input.user_requested_save,
            )
            .map_err(safe_nova_error)?;

        let content_digest = outcome.source.content_hash;
        let (status, message) = match outcome.proposal.status {
            ProposalStatus::Pending => (
                "proposal_created",
                "已保存为 Nova 待确认记忆草稿，尚未写入永久记忆。",
            ),
            ProposalStatus::Confirmed => (
                "proposal_already_confirmed",
                "此保存请求对应的草稿已被用户确认；本次重试没有创建新记忆。",
            ),
            ProposalStatus::Rejected => (
                "proposal_rejected",
                "此保存请求对应的草稿已被用户拒绝；本次重试没有重新保存。",
            ),
            ProposalStatus::Deleted => (
                "proposal_deleted",
                "此保存请求对应的草稿已删除；本次重试没有重新保存。",
            ),
        };

        Ok((
            SaveVoiceSessionDigestOutput {
                status: status.into(),
                source_id: outcome.source.source_id,
                proposal_id: outcome.proposal.proposal_id,
                receipt_id: outcome.receipt.envelope.receipt_id,
                capture_fidelity: outcome.source.capture_fidelity.as_str().into(),
                needs_confirmation: outcome.proposal.status == ProposalStatus::Pending,
                proposal_status: outcome.proposal.status.as_str().into(),
                user_message: message.into(),
            },
            content_digest,
        ))
    }
}

#[tool_router]
impl NovaMcpServer {
    /// Save a ChatGPT conversation digest to Nova as a pending proposal. Call this only after the
    /// user explicitly asks to save or remember the conversation. This never creates canonical
    /// memory and never confirms a proposal on the user's behalf.
    #[tool(
        name = "save_voice_session_digest",
        annotations(
            title = "Save conversation to Nova",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn save_voice_session_digest(
        &self,
        Parameters(input): Parameters<SaveVoiceSessionDigestInput>,
    ) -> Result<Json<SaveVoiceSessionDigestOutput>, String> {
        validate_save_input(&input)?;

        let source_content = serde_json::to_string(&VoiceSessionSource {
            schema_version: "nova.chatgpt_session_digest.v0.1",
            session_digest: &input.session_digest,
            key_points: &input.key_points,
            decisions: &input.decisions,
            rationales: &input.rationales,
            open_questions: &input.open_questions,
            next_steps: &input.next_steps,
        })
        .map_err(|_| {
            "NOVA_SERIALIZATION_FAILED: unable to encode the session digest".to_string()
        })?;

        self.capture_digest(input, source_content)
            .map(|(output, _)| Json(output))
    }

    /// Save a text or Voice conversation as a pending proposal after an explicit user request.
    /// Capture metadata describes the submitted F1 digest; it grants no new authority.
    #[tool(
        name = "save_to_nova",
        annotations(
            title = "Save to Nova Memory",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn save_to_nova(
        &self,
        Parameters(input): Parameters<SaveToNovaInput>,
    ) -> Result<Json<SaveToNovaOutput>, String> {
        let host = input.host;
        let surface = input.surface;
        let request_id = input.request_id.clone();
        let source_ref = input.source_ref.clone();
        if let Some(reference) = &source_ref
            && (reference.is_empty()
                || reference.len() > 128
                || !reference
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b)))
        {
            return Err("NOVA_VALIDATION_FAILED: source_ref must be an opaque identifier of 1 to 128 ASCII letters, digits, _, -, . or :".into());
        }
        let legacy: SaveVoiceSessionDigestInput = input.into();
        validate_save_input(&legacy)?;
        let source_content = serde_json::to_string(&NativeSessionSource {
            host,
            surface,
            capture_fidelity: ToolCaptureFidelity::F1AiDigest,
            source_ref: &source_ref,
            digest: VoiceSessionSource {
                schema_version: "nova.session_digest.v0.2",
                session_digest: &legacy.session_digest,
                key_points: &legacy.key_points,
                decisions: &legacy.decisions,
                rationales: &legacy.rationales,
                open_questions: &legacy.open_questions,
                next_steps: &legacy.next_steps,
            },
        })
        .map_err(|_| {
            "NOVA_SERIALIZATION_FAILED: unable to encode the session digest".to_string()
        })?;
        let (proposal, content_digest) = self.capture_digest(legacy, source_content)?;
        Ok(Json(SaveToNovaOutput {
            proposal,
            request_id,
            host,
            surface,
            source_ref,
            content_digest,
        }))
    }

    /// Build a short-lived, summary-only Context Pack from confirmed Nova memories. Call this only
    /// after the user explicitly approves this one disclosure for the stated purpose. The tool does
    /// not return sensitive or sealed memory and does not grant proactive or device authority.
    #[tool(
        name = "prepare_context_pack",
        annotations(
            title = "Use approved Nova context",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn prepare_context_pack(
        &self,
        Parameters(input): Parameters<PrepareContextPackInput>,
    ) -> Result<Json<PrepareContextPackOutput>, String> {
        validate_context_input(&input)?;

        let surface: ContextSurface = input.surface.into();
        let outcome = self
            .service
            .context_legacy(ContextRequest {
                request_id: Some(input.request_id),
                subject: self.service.binding().owner().into(),
                recipient: SOURCE_PLATFORM.into(),
                purpose: input.purpose,
                query: input.query,
                surface,
                allowed_memory_types: input
                    .allowed_memory_types
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                maximum_privacy_level: input.maximum_privacy_level.into(),
                max_items: input.max_items,
                expires_in_seconds: input.expires_in_seconds,
                user_approved: input.user_approved,
            })
            .map_err(safe_nova_error)?;

        let items = outcome
            .pack
            .items
            .into_iter()
            .map(|item| ToolContextPackItem {
                memory_id: item.memory_id,
                title: item.title,
                memory_type: item.memory_type.as_str().into(),
                summary: item.summary,
                observed_at: item.observed_at,
                assertion_origin: item.assertion_origin.as_str().into(),
                source_artifact_ids: item.source_artifact_ids,
                recall_reason: item.recall_reason,
            })
            .collect();

        Ok(Json(PrepareContextPackOutput {
            status: "context_pack_delivered".into(),
            context_pack_id: outcome.pack.context_pack_id,
            receipt_id: outcome.receipt.envelope.receipt_id,
            recipient: outcome.pack.recipient,
            purpose: outcome.pack.purpose,
            surface: outcome.pack.surface.as_str().into(),
            expires_at: outcome.pack.expires_at,
            disclosure_class: outcome.pack.disclosure_class,
            items,
            excluded_count: outcome.pack.excluded_count,
            user_message: "Nova 已按本次授权提供最小化上下文；授权仅限当前目的和有效期。".into(),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NovaMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("nova-memory", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Nova Memory is user-owned memory infrastructure. Invoke save_to_nova (or legacy save_voice_session_digest) only after an explicit user request to save; success creates a pending proposal, never canonical memory. Invoke prepare_context_pack only after explicit approval for one named purpose; it returns a short-lived minimum-summary disclosure and excludes sensitive and sealed memory. Neither operation grants proactive, device, or action authority.",
            )
    }
}

fn validate_save_input(input: &SaveVoiceSessionDigestInput) -> Result<(), String> {
    if !input.user_requested_save {
        return Err(
            "NOVA_EXPLICIT_SAVE_REQUIRED: ask the user whether to save before invoking Nova".into(),
        );
    }
    validate_text("request_id", &input.request_id, 128)?;
    validate_text("title", &input.title, 240)?;
    validate_text("summary", &input.summary, 4_000)?;
    validate_text("session_digest", &input.session_digest, 24_000)?;
    validate_list("key_points", &input.key_points, 30, 1_500)?;
    validate_list("decisions", &input.decisions, 20, 1_500)?;
    validate_list("rationales", &input.rationales, 20, 1_500)?;
    validate_list("open_questions", &input.open_questions, 20, 1_500)?;
    validate_list("next_steps", &input.next_steps, 20, 1_500)?;
    if let Some(observed_at) = &input.observed_at {
        OffsetDateTime::parse(observed_at, &Rfc3339).map_err(|_| {
            "NOVA_VALIDATION_FAILED: observed_at must be an RFC 3339 timestamp".to_string()
        })?;
    }
    Ok(())
}

fn validate_context_input(input: &PrepareContextPackInput) -> Result<(), String> {
    if !input.user_approved {
        return Err(
            "NOVA_CONTEXT_APPROVAL_REQUIRED: obtain explicit approval for this one disclosure"
                .into(),
        );
    }
    validate_text("request_id", &input.request_id, 128)?;
    validate_text("purpose", &input.purpose, 240)?;
    if input.query.len() > 4_000 {
        return Err("NOVA_VALIDATION_FAILED: query must be at most 4000 bytes".into());
    }
    if input.allowed_memory_types.len() > 8 {
        return Err(
            "NOVA_VALIDATION_FAILED: allowed_memory_types may contain at most 8 items".into(),
        );
    }
    if !(1..=10).contains(&input.max_items) {
        return Err("NOVA_VALIDATION_FAILED: max_items must be between 1 and 10".into());
    }
    if !(1..=3_600).contains(&input.expires_in_seconds) {
        return Err("NOVA_VALIDATION_FAILED: expires_in_seconds must be between 1 and 3600".into());
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max_bytes {
        return Err(format!(
            "NOVA_VALIDATION_FAILED: {field} must contain 1 to {max_bytes} bytes"
        ));
    }
    Ok(())
}

fn validate_list(
    field: &str,
    values: &[String],
    max_items: usize,
    max_item_bytes: usize,
) -> Result<(), String> {
    if values.len() > max_items {
        return Err(format!(
            "NOVA_VALIDATION_FAILED: {field} may contain at most {max_items} items"
        ));
    }
    for value in values {
        validate_text(field, value, max_item_bytes)?;
    }
    Ok(())
}

fn safe_nova_error(error: NovaError) -> String {
    match error {
        NovaError::Validation(_) | NovaError::PolicyDenied(_) | NovaError::InvalidState(_) => {
            format!("{}: {}", error.code(), error)
        }
        NovaError::IdempotencyConflict { .. }
        | NovaError::IdempotencyReplayUnavailable { .. }
        | NovaError::InvalidVaultKey => format!("{}: {}", error.code(), error),
        _ => format!("{}: Nova Vault operation failed", error.code()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nova_core::ConfirmRequest;
    use tempfile::TempDir;

    struct TestServer {
        _directory: TempDir,
        vault_path: PathBuf,
        key_path: PathBuf,
        server: NovaMcpServer,
    }

    impl TestServer {
        fn open_vault(&self) -> Result<Vault, NovaError> {
            Vault::open(&self.vault_path, VaultKey::read_file(&self.key_path)?)
        }
    }

    fn test_server() -> TestServer {
        let directory = tempfile::tempdir().expect("tempdir");
        let vault_path = directory.path().join("nova.sqlite");
        let key_path = directory.path().join("vault.key");
        let key = VaultKey::generate().expect("key");
        key.write_new_file(&key_path).expect("key file");
        Vault::create(&vault_path, key).expect("vault");
        let server = NovaMcpServer::new(&vault_path, &key_path);
        TestServer {
            _directory: directory,
            vault_path,
            key_path,
            server,
        }
    }

    fn sample_input() -> SaveVoiceSessionDigestInput {
        SaveVoiceSessionDigestInput {
            user_requested_save: true,
            request_id: "chatgpt_session_001".into(),
            title: "Nova ChatGPT 单平台验证".into(),
            memory_type: ToolMemoryType::Decision,
            summary: "用 ChatGPT 作为 P0 唯一主平台。".into(),
            session_digest: "用户希望在 ChatGPT 对话中明确说出保存后，直接生成 Nova 待确认草稿。"
                .into(),
            key_points: vec!["ChatGPT 是最重要的首发场景".into()],
            decisions: vec!["P0 只做 ChatGPT".into()],
            rationales: vec!["先验证最高频真实场景".into()],
            open_questions: vec!["移动端 Voice 是否稳定触发工具".into()],
            next_steps: vec!["连接 ChatGPT developer mode 做真机验证".into()],
            privacy_level: ToolPrivacyLevel::Private,
            observed_at: Some("2026-08-14T08:00:00Z".into()),
        }
    }

    fn unified_input() -> SaveToNovaInput {
        serde_json::from_str(include_str!(
            "../tests/fixtures/save_to_nova_chat_text.json"
        ))
        .expect("synthetic fixture")
    }

    #[tokio::test]
    async fn unified_save_replays_without_new_events_and_never_confirms() {
        let test = test_server();
        let first = test
            .server
            .save_to_nova(Parameters(unified_input()))
            .await
            .expect("save")
            .0;
        let before = test
            .open_vault()
            .unwrap()
            .verify_audit_chain()
            .unwrap()
            .event_count;
        let replay = test
            .server
            .save_to_nova(Parameters(unified_input()))
            .await
            .expect("retry")
            .0;
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&replay).unwrap()
        );
        assert_eq!(first.content_digest.len(), 71);
        assert_eq!(first.proposal.capture_fidelity, "f1_ai_digest");
        assert!(first.proposal.needs_confirmation);
        let vault = test.open_vault().unwrap();
        assert_eq!(vault.verify_audit_chain().unwrap().event_count, before);
        assert_eq!(vault.pending_proposals().unwrap().len(), 1);
        assert!(vault.memories().unwrap().is_empty());
        let bytes = std::fs::read(&test.vault_path).unwrap();
        assert!(
            !bytes
                .windows(b"session:synthetic-001".len())
                .any(|w| w == b"session:synthetic-001")
        );
    }

    #[tokio::test]
    async fn unified_metadata_changes_conflict_and_do_not_mutate_vault() {
        let test = test_server();
        let first = test
            .server
            .save_to_nova(Parameters(unified_input()))
            .await
            .unwrap()
            .0;
        let before = test
            .open_vault()
            .unwrap()
            .verify_audit_chain()
            .unwrap()
            .event_count;
        for change in ["surface", "source_ref"] {
            let mut input = unified_input();
            if change == "surface" {
                input.surface = ToolContextSurface::VoiceAudio;
            } else {
                input.source_ref = Some("session:changed".into());
            }
            let error = test
                .server
                .save_to_nova(Parameters(input))
                .await
                .err()
                .expect("request must be rejected");
            assert!(error.starts_with("NOVA_IDEMPOTENCY_CONFLICT"));
        }
        let mut fresh = unified_input();
        fresh.request_id = "fresh_voice".into();
        fresh.surface = ToolContextSurface::VoiceAudio;
        let voice = test.server.save_to_nova(Parameters(fresh)).await.unwrap().0;
        assert_ne!(first.content_digest, voice.content_digest);
        assert_eq!(
            test.open_vault()
                .unwrap()
                .verify_audit_chain()
                .unwrap()
                .event_count,
            before * 2
        );
    }

    #[tokio::test]
    async fn unified_save_checks_intent_and_reference_before_vault_access() {
        let server = NovaMcpServer::new("absent.sqlite", "absent.key");
        let mut input = unified_input();
        input.user_requested_save = false;
        assert!(
            server
                .save_to_nova(Parameters(input))
                .await
                .err()
                .expect("request must be rejected")
                .starts_with("NOVA_EXPLICIT_SAVE_REQUIRED")
        );
        for reference in ["", "https://example.com/source", "raw dialogue with spaces"] {
            let mut input = unified_input();
            input.source_ref = Some(reference.into());
            assert!(
                server
                    .save_to_nova(Parameters(input))
                    .await
                    .err()
                    .expect("request must be rejected")
                    .starts_with("NOVA_VALIDATION_FAILED")
            );
        }
    }

    #[test]
    fn unified_contract_rejects_unsupported_host_fidelity_surface_and_unknown_fields() {
        for (field, value) in [
            ("host", "other_host"),
            ("capture_fidelity", "f3_raw_audio"),
            ("capture_fidelity", "f2_visible_transcript"),
            ("surface", "car"),
            ("auto_confirm", "true"),
        ] {
            let mut input = serde_json::to_value(unified_input()).unwrap();
            input[field] = serde_json::json!(value);
            assert!(serde_json::from_value::<SaveToNovaInput>(input).is_err());
        }
        for field in ["host", "surface", "capture_fidelity"] {
            let mut input = serde_json::to_value(unified_input()).unwrap();
            input.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<SaveToNovaInput>(input).is_err());
        }
    }

    #[tokio::test]
    async fn migration_cannot_reinterpret_a_legacy_request_id() {
        let test = test_server();
        let legacy: SaveVoiceSessionDigestInput = unified_input().into();
        let first = test
            .server
            .save_voice_session_digest(Parameters(legacy.clone()))
            .await
            .unwrap()
            .0;
        let error = test
            .server
            .save_to_nova(Parameters(unified_input()))
            .await
            .err()
            .expect("request must be rejected");
        assert!(error.starts_with("NOVA_IDEMPOTENCY_CONFLICT"));
        let replay = test
            .server
            .save_voice_session_digest(Parameters(legacy))
            .await
            .unwrap()
            .0;
        assert_eq!(first.receipt_id, replay.receipt_id);
        assert_eq!(
            test.open_vault()
                .unwrap()
                .pending_proposals()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn capture_retry_reports_reviewed_lifecycle_without_resaving() {
        for confirm in [true, false] {
            let test = test_server();
            test.server
                .save_to_nova(Parameters(unified_input()))
                .await
                .unwrap();
            let mut vault = test.open_vault().unwrap();
            let review = vault.review_inbox(OWNER_ID).unwrap().remove(0);
            vault
                .review_proposal(nova_core::ReviewRequest {
                    request_id: None,
                    subject: OWNER_ID.into(),
                    proposal_id: review.proposal.proposal_id,
                    expected_snapshot: review.snapshot,
                    action: if confirm {
                        nova_core::ReviewAction::Confirm
                    } else {
                        nova_core::ReviewAction::Reject {
                            reason: "not a user decision".into(),
                        }
                    },
                })
                .unwrap();
            let before = vault.verify_audit_chain().unwrap().event_count;
            let replay = test
                .server
                .save_to_nova(Parameters(unified_input()))
                .await
                .unwrap()
                .0;
            assert!(!replay.proposal.needs_confirmation);
            assert_eq!(
                replay.proposal.status,
                if confirm {
                    "proposal_already_confirmed"
                } else {
                    "proposal_rejected"
                }
            );
            assert_eq!(vault.verify_audit_chain().unwrap().event_count, before);
            assert_eq!(vault.memories().unwrap().len(), usize::from(confirm));
        }
    }

    fn sample_context_input() -> PrepareContextPackInput {
        PrepareContextPackInput {
            user_approved: true,
            request_id: "chatgpt_context_001".into(),
            purpose: "personal_memory_continuity".into(),
            query: "ChatGPT 单平台".into(),
            surface: ToolContextSurface::VoiceAudio,
            allowed_memory_types: vec![ToolMemoryType::Decision],
            maximum_privacy_level: ToolContextPrivacyCeiling::Private,
            max_items: 5,
            expires_in_seconds: 600,
        }
    }

    fn seed_confirmed_memory(test: &TestServer, privacy_level: PrivacyLevel) -> String {
        let key = VaultKey::read_file(&test.key_path).expect("key");
        let mut vault = Vault::open(&test.vault_path, key).expect("vault");
        let captured = vault
            .capture(CaptureRequest {
                structure: None,
                request_id: Some(format!("seed_{privacy_level:?}")),
                owner_id: OWNER_ID.into(),
                source_platform: SOURCE_PLATFORM.into(),
                capture_channel: CaptureChannel::RemoteMcp,
                capture_fidelity: CaptureFidelity::F1AiDigest,
                source_content: "用户决定先验证 ChatGPT 单平台的可信记忆闭环。".into(),
                title: "Nova P0 平台范围".into(),
                memory_type: MemoryType::Decision,
                summary: "P0 只做 ChatGPT，不扩展第二个平台。".into(),
                decisions: vec!["P0 只做 ChatGPT".into()],
                privacy_level,
                proposed_origin: AssertionOrigin::AiExtractedProposal,
                review_tier: ReviewTier::Attention,
                permission_scope: vec!["personal_review".into()],
                purpose_limits: vec!["personal_memory_continuity".into()],
                observed_at: Some("2026-08-14T08:00:00Z".into()),
            })
            .expect("capture");
        vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: OWNER_ID.into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm")
            .memory
            .memory_id
    }

    #[tokio::test]
    async fn save_creates_only_a_pending_proposal() {
        let test = test_server();
        let Json(output) = test
            .server
            .save_voice_session_digest(Parameters(sample_input()))
            .await
            .expect("save");

        assert_eq!(output.status, "proposal_created");
        assert!(output.needs_confirmation);
        let key = VaultKey::read_file(&test.key_path).expect("key");
        let vault = Vault::open(&test.vault_path, key).expect("vault");
        assert_eq!(vault.pending_proposals().expect("pending").len(), 1);
        assert!(vault.memories().expect("memories").is_empty());
        assert_eq!(
            vault.pending_proposals().expect("pending")[0].decisions,
            vec!["P0 只做 ChatGPT"]
        );
    }

    #[tokio::test]
    async fn save_requires_explicit_user_request_before_opening_vault() {
        let test = test_server();
        let mut input = sample_input();
        input.user_requested_save = false;
        let result = test
            .server
            .save_voice_session_digest(Parameters(input))
            .await;
        assert!(result.is_err(), "implicit save must be denied");
        let error = result.err().expect("error");
        assert!(error.starts_with("NOVA_EXPLICIT_SAVE_REQUIRED"));
    }

    #[tokio::test]
    async fn retry_with_same_request_id_is_idempotent() {
        let test = test_server();
        let first = test
            .server
            .save_voice_session_digest(Parameters(sample_input()))
            .await
            .expect("first")
            .0;
        let replay = test
            .server
            .save_voice_session_digest(Parameters(sample_input()))
            .await
            .expect("replay")
            .0;
        assert_eq!(first.proposal_id, replay.proposal_id);
        assert_eq!(first.receipt_id, replay.receipt_id);
    }

    #[tokio::test]
    async fn changed_digest_under_same_request_id_is_rejected() {
        let test = test_server();
        test.server
            .save_voice_session_digest(Parameters(sample_input()))
            .await
            .expect("first");
        let mut changed = sample_input();
        changed.summary = "尝试更改同一请求的摘要。".into();
        let result = test
            .server
            .save_voice_session_digest(Parameters(changed))
            .await;
        assert!(result.is_err(), "changed retry must be rejected");
        assert!(
            result
                .err()
                .expect("error")
                .starts_with("NOVA_IDEMPOTENCY_CONFLICT")
        );
    }

    #[tokio::test]
    async fn save_rejects_a_malformed_observation_time() {
        let test = test_server();
        let mut input = sample_input();
        input.observed_at = Some("this-morning".into());
        let result = test
            .server
            .save_voice_session_digest(Parameters(input))
            .await;
        assert!(result.is_err(), "malformed timestamp must be denied");
        assert!(
            result
                .err()
                .expect("error")
                .starts_with("NOVA_VALIDATION_FAILED")
        );
    }

    #[tokio::test]
    async fn context_pack_requires_explicit_approval() {
        let test = test_server();
        let mut input = sample_context_input();
        input.user_approved = false;
        let result = test.server.prepare_context_pack(Parameters(input)).await;
        assert!(result.is_err(), "unapproved disclosure must be denied");
        assert!(
            result
                .err()
                .expect("error")
                .starts_with("NOVA_CONTEXT_APPROVAL_REQUIRED")
        );
    }

    #[tokio::test]
    async fn context_pack_returns_minimum_confirmed_context_for_the_surface() {
        let test = test_server();
        let memory_id = seed_confirmed_memory(&test, PrivacyLevel::Private);
        let Json(output) = test
            .server
            .prepare_context_pack(Parameters(sample_context_input()))
            .await
            .expect("context pack");

        assert_eq!(output.status, "context_pack_delivered");
        assert_eq!(output.surface, "voice_audio");
        assert_eq!(output.disclosure_class, "minimum_summary_only");
        assert_eq!(output.items.len(), 1);
        assert_eq!(output.items[0].memory_id, memory_id);
        assert_eq!(output.items[0].memory_type, "decision");
    }

    #[tokio::test]
    async fn context_pack_respects_the_privacy_ceiling() {
        let test = test_server();
        seed_confirmed_memory(&test, PrivacyLevel::Private);
        let mut input = sample_context_input();
        input.maximum_privacy_level = ToolContextPrivacyCeiling::Shareable;
        let Json(output) = test
            .server
            .prepare_context_pack(Parameters(input))
            .await
            .expect("context pack");

        assert!(output.items.is_empty());
        assert_eq!(output.excluded_count, 1);
    }
}
