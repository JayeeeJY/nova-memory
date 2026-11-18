use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureChannel {
    RemoteMcp,
    ShareSheet,
    WebAdapter,
    QuickCapture,
    OfficialExport,
}

impl CaptureChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoteMcp => "remote_mcp",
            Self::ShareSheet => "share_sheet",
            Self::WebAdapter => "web_adapter",
            Self::QuickCapture => "quick_capture",
            Self::OfficialExport => "official_export",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFidelity {
    F1AiDigest,
    F2VisibleTranscript,
    F3RawAudio,
}

impl CaptureFidelity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::F1AiDigest => "f1_ai_digest",
            Self::F2VisibleTranscript => "f2_visible_transcript",
            Self::F3RawAudio => "f3_raw_audio",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Fact,
    Preference,
    Decision,
    Goal,
    Commitment,
    Event,
    Lesson,
    Reflection,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::Goal => "goal",
            Self::Commitment => "commitment",
            Self::Event => "event",
            Self::Lesson => "lesson",
            Self::Reflection => "reflection",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    Shareable,
    Private,
    Sensitive,
    Sealed,
}

impl PrivacyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shareable => "shareable",
            Self::Private => "private",
            Self::Sensitive => "sensitive",
            Self::Sealed => "sealed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTier {
    Routine,
    Attention,
    Restricted,
}

impl ReviewTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Attention => "attention",
            Self::Restricted => "restricted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionOrigin {
    SourceEvidence,
    UserSelfReport,
    AiExtractedProposal,
    ModelInferred,
    UserConfirmedAiExtraction,
    UserConfirmedInference,
    Disputed,
    Superseded,
}

impl AssertionOrigin {
    pub fn confirmed(self) -> Self {
        match self {
            Self::AiExtractedProposal => Self::UserConfirmedAiExtraction,
            Self::ModelInferred => Self::UserConfirmedInference,
            other => other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceEvidence => "source_evidence",
            Self::UserSelfReport => "user_self_report",
            Self::AiExtractedProposal => "ai_extracted_proposal",
            Self::ModelInferred => "model_inferred",
            Self::UserConfirmedAiExtraction => "user_confirmed_ai_extraction",
            Self::UserConfirmedInference => "user_confirmed_inference",
            Self::Disputed => "disputed",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Confirmed,
    Rejected,
    Deleted,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Active,
    Disputed,
    Superseded,
    Deleted,
}

impl MemoryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disputed => "disputed",
            Self::Superseded => "superseded",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub entity_id: String,
    pub role: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmotionEvidence {
    pub label: String,
    pub source: AssertionOrigin,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEligibility {
    pub allow_personal_model: bool,
    pub allowed_facets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub source_id: String,
    pub schema_version: String,
    pub owner_id: String,
    pub source_platform: String,
    pub capture_channel: CaptureChannel,
    pub capture_fidelity: CaptureFidelity,
    pub content: String,
    pub observed_at: String,
    pub created_at: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryProposal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<crate::StructuredMemory>,
    #[serde(default)]
    pub review_revision: u64,
    pub proposal_id: String,
    pub schema_version: String,
    pub owner_id: String,
    pub source_artifact_id: String,
    pub title: String,
    pub memory_type: MemoryType,
    pub entities: Vec<EntityRef>,
    pub tags: Vec<String>,
    pub summary: String,
    pub decisions: Vec<String>,
    pub emotion: Option<EmotionEvidence>,
    pub privacy_level: PrivacyLevel,
    pub permission_scope: Vec<String>,
    pub purpose_limits: Vec<String>,
    pub model_eligibility: ModelEligibility,
    pub observed_at: String,
    pub created_at: String,
    pub proposed_origin: AssertionOrigin,
    pub review_tier: ReviewTier,
    pub status: ProposalStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalMemory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<crate::StructuredMemory>,
    pub memory_id: String,
    pub schema_version: String,
    pub owner_id: String,
    pub proposal_id: String,
    pub title: String,
    pub memory_type: MemoryType,
    pub entities: Vec<EntityRef>,
    pub tags: Vec<String>,
    pub source_platform: String,
    pub source_artifact_ids: Vec<String>,
    pub summary: String,
    pub decisions: Vec<String>,
    pub emotion: Option<EmotionEvidence>,
    pub privacy_level: PrivacyLevel,
    pub permission_scope: Vec<String>,
    pub purpose_limits: Vec<String>,
    pub model_eligibility: ModelEligibility,
    pub observed_at: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
    pub status: MemoryStatus,
    #[serde(default)]
    pub status_reason: Option<String>,
    pub confidence: f32,
    pub assertion_origin: AssertionOrigin,
    pub supersedes: Vec<String>,
    #[serde(default)]
    pub superseded_by: Vec<String>,
    pub conflicts_with: Vec<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<crate::StructureInput>,
    pub request_id: Option<String>,
    pub owner_id: String,
    pub source_platform: String,
    pub capture_channel: CaptureChannel,
    pub capture_fidelity: CaptureFidelity,
    pub source_content: String,
    pub title: String,
    pub memory_type: MemoryType,
    pub summary: String,
    #[serde(default)]
    pub decisions: Vec<String>,
    pub privacy_level: PrivacyLevel,
    pub proposed_origin: AssertionOrigin,
    pub review_tier: ReviewTier,
    pub permission_scope: Vec<String>,
    pub purpose_limits: Vec<String>,
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmRequest {
    pub request_id: Option<String>,
    pub subject: String,
    pub proposal_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionRequest {
    pub request_id: Option<String>,
    pub subject: String,
    pub memory_id: String,
    pub title: Option<String>,
    pub summary: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputeRequest {
    pub request_id: Option<String>,
    pub subject: String,
    pub memory_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSurface {
    #[default]
    ChatText,
    VoiceAudio,
    DesktopReview,
}

impl ContextSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatText => "chat_text",
            Self::VoiceAudio => "voice_audio",
            Self::DesktopReview => "desktop_review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRequest {
    pub request_id: Option<String>,
    pub subject: String,
    pub recipient: String,
    pub purpose: String,
    pub query: String,
    #[serde(default)]
    pub surface: ContextSurface,
    #[serde(default)]
    pub allowed_memory_types: Vec<MemoryType>,
    pub maximum_privacy_level: PrivacyLevel,
    pub max_items: usize,
    pub expires_in_seconds: i64,
    pub user_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptType {
    CaptureReceipt,
    MutationReceipt,
    ContextDeliveryReceipt,
}

impl ReceiptType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CaptureReceipt => "capture_receipt",
            Self::MutationReceipt => "mutation_receipt",
            Self::ContextDeliveryReceipt => "context_delivery_receipt",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    QueuedEncrypted,
    ProposalCreated,
    CommittedLocal,
    Delivered,
    PartiallyCompleted,
    Failed,
    Revoked,
}

impl ReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::QueuedEncrypted => "queued_encrypted",
            Self::ProposalCreated => "proposal_created",
            Self::CommittedLocal => "committed_local",
            Self::Delivered => "delivered",
            Self::PartiallyCompleted => "partially_completed",
            Self::Failed => "failed",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEnvelope {
    pub receipt_id: String,
    pub receipt_type: ReceiptType,
    pub subject: String,
    pub request_id: String,
    pub policy_version: String,
    pub purpose: String,
    pub source_refs: Vec<String>,
    pub requested_at: String,
    pub completed_at: String,
    pub status: ReceiptStatus,
    pub error_code: Option<String>,
    pub integrity_hash: String,
    pub previous_audit_event_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureReceipt {
    pub envelope: ReceiptEnvelope,
    pub source_id: String,
    pub proposal_id: String,
    pub source_platform: String,
    pub capture_channel: CaptureChannel,
    pub capture_fidelity: CaptureFidelity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationReceipt {
    pub envelope: ReceiptEnvelope,
    pub proposal_id: Option<String>,
    pub memory_id: Option<String>,
    pub mutation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureOutcome {
    pub source: SourceArtifact,
    pub proposal: MemoryProposal,
    pub receipt: CaptureReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmationOutcome {
    pub memory: CanonicalMemory,
    pub receipt: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionOutcome {
    pub previous_memory: CanonicalMemory,
    pub memory: CanonicalMemory,
    pub source: SourceArtifact,
    pub receipt: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisputeOutcome {
    pub memory: CanonicalMemory,
    pub source: SourceArtifact,
    pub receipt: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPackItem {
    pub memory_id: String,
    pub title: String,
    pub memory_type: MemoryType,
    pub summary: String,
    pub observed_at: String,
    pub assertion_origin: AssertionOrigin,
    pub source_artifact_ids: Vec<String>,
    pub recall_reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPack {
    pub context_pack_id: String,
    pub schema_version: String,
    pub subject: String,
    pub recipient: String,
    pub purpose: String,
    pub query: String,
    #[serde(default)]
    pub surface: ContextSurface,
    pub created_at: String,
    pub expires_at: String,
    pub disclosure_class: String,
    pub items: Vec<ContextPackItem>,
    pub excluded_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDeliveryReceipt {
    pub envelope: ReceiptEnvelope,
    pub context_pack_id: String,
    pub recipient: String,
    #[serde(default)]
    pub surface: ContextSurface,
    pub disclosed_memory_ids: Vec<String>,
    pub disclosed_fields: Vec<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextOutcome {
    pub pack: ContextPack,
    pub receipt: ContextDeliveryReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteOutcome {
    pub deleted_memory_id: String,
    pub preserved_source_artifact_ids: Vec<String>,
    pub receipt: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub valid: bool,
    pub event_count: usize,
    pub head_hash: Option<String>,
}

/// Local owner-only review projection. Never exposed through the ChatGPT MCP adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub proposal: MemoryProposal,
    pub source: SourceArtifact,
    pub capture_receipt: Option<CaptureReceipt>,
    pub snapshot: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewAction {
    Confirm,
    EditStructure {
        structure: crate::StructureInput,
        reason: String,
    },
    Edit {
        title: String,
        summary: String,
        decisions: Vec<String>,
        reason: String,
    },
    Reject {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub request_id: Option<String>,
    pub subject: String,
    pub proposal_id: String,
    pub expected_snapshot: String,
    pub action: ReviewAction,
}
