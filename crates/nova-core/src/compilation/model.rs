use crate::{CaptureFidelity, PrivacyLevel};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetScope {
    pub owner_id: String,
    pub space: String,
}

/// Client input contains no owner, privacy override, confirmation or grants.
/// The service supplies authority and capture consent from its authenticated context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeRequest {
    pub schema_version: String,
    pub request_id: String,
    pub source_platform: String,
    pub capture_fidelity: CaptureFidelity,
    pub content: String,
    pub occurred_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionRef {
    pub object_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    User,
    Assistant,
    External,
    Agent,
    Mixed,
}

/// A trusted source snapshot supplied to validation, never a model write operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshot {
    pub source: RevisionRef,
    pub scope: AssetScope,
    pub recorded_at: String,
    pub source_platform: String,
    pub occurred_at: Option<String>,
    pub role: SourceRole,
    pub capture_fidelity: CaptureFidelity,
    pub privacy_level: PrivacyLevel,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntakeAcceptance {
    #[serde(rename = "received")]
    Received,
}

/// Durable acceptance is reported separately from a job's processing status.
/// N1 must write the source/job/receipt atomically before returning this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeReceipt {
    pub schema_version: String,
    pub request_id: String,
    pub source: RevisionRef,
    pub job_id: String,
    pub saved_at: String,
    pub content_sha256: String,
    pub status: IntakeAcceptance,
}

/// Unicode code-point [start_char, end_char), not UTF-8 bytes or UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpan {
    pub source: RevisionRef,
    pub start_char: usize,
    pub end_char: usize,
    pub quote: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticOrigin {
    UserStatement,
    Extracted,
    Inferred,
    ExternalClaim,
    AgentExperience,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    Active,
    Disputed,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetHeader {
    pub object: RevisionRef,
    pub scope: AssetScope,
    pub recorded_at: String,
    pub privacy_level: PrivacyLevel,
    pub state: UnitState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValidTime {
    Unknown,
    /// An absent end means no stated endpoint, not a permanent truth.
    Interval {
        start: String,
        end: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Asserted,
    Planned,
    Hypothetical,
    Unknown,
}

/// An open predicate/value representation. Conditions are data, not executable filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticContent {
    pub subject: String,
    pub predicate: String,
    pub value: Value,
    pub polarity: Polarity,
    pub modality: Modality,
    pub conditions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleRef {
    pub module_id: String,
    pub revision: u64,
}

/// Declaration only: neither a candidate nor a bundled definition enables a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureModule {
    pub module: ModuleRef,
    pub description: String,
    pub predicates: Vec<String>,
    pub compatible_with: Option<ModuleRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryUnit {
    pub header: AssetHeader,
    pub origin: SemanticOrigin,
    pub content: SemanticContent,
    pub occurred_at: Option<String>,
    pub valid_time: ValidTime,
    pub evidence: Vec<EvidenceSpan>,
    pub derived_from: Vec<RevisionRef>,
    pub modules: Vec<ModuleRef>,
    /// Bounded namespaced JSON, preserved as inert content, including unknown modules.
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRelation {
    pub header: AssetHeader,
    pub predicate: String,
    pub from: RevisionRef,
    pub to: RevisionRef,
    pub valid_time: ValidTime,
    pub evidence: Vec<EvidenceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    pub object: RevisionRef,
    pub deleted_at: String,
}

/// A bounded in-scope planning snapshot, not a Portable archive or disclosure response.
/// Sources and protections must come from the trusted runtime, not model output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationSnapshot {
    pub schema_version: String,
    pub scope: AssetScope,
    pub sources: Vec<SourceSnapshot>,
    pub units: Vec<MemoryUnit>,
    pub relations: Vec<MemoryRelation>,
    pub modules: Vec<StructureModule>,
    pub tombstones: Vec<Tombstone>,
    pub protected_unit_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Received,
    Processing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationJob {
    pub job_id: String,
    pub scope: AssetScope,
    pub source: RevisionRef,
    pub state: JobState,
    pub attempt: u32,
    pub error_code: Option<String>,
    pub committed_change_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerVersion {
    pub model_id: String,
    pub extractor_version: String,
    pub rules_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionIntent {
    Supplement,
    Correct,
    StateChange,
    Qualify,
}

/// Control-plane, source mutation, deletion and module activation are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangeOperation {
    AddUnit {
        unit: MemoryUnit,
    },
    ReviseUnit {
        base_revision: u64,
        intent: RevisionIntent,
        unit: MemoryUnit,
    },
    AddRelation {
        relation: MemoryRelation,
    },
    ReviseRelation {
        base_revision: u64,
        relation: MemoryRelation,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSet {
    pub schema_version: String,
    pub scope: AssetScope,
    pub change_id: String,
    pub compiler: CompilerVersion,
    pub operations: Vec<ChangeOperation>,
}
