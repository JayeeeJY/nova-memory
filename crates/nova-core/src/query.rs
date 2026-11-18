//! Owner-issued, exact-request disclosure contracts. These are not MCP approval tools.
use crate::{AssertionOrigin, CaptureFidelity, ContextDeliveryReceipt, MemoryType, PrivacyLevel};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuerySelection {
    Get {
        memory_id: String,
        #[serde(default)]
        capture_status: bool,
    },
    Search {
        query: String,
        max_items: usize,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerySpec {
    pub subject: String,
    pub client: String,
    pub space: String,
    pub request_id: String,
    pub purpose: String,
    pub selection: QuerySelection,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureGrant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    pub approval_id: String,
    pub spec: QuerySpec,
    pub allowed_ids: Vec<String>,
    pub maximum_privacy: PrivacyLevel,
    pub expires_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryView {
    pub memory_id: String,
    pub canonical_memory_id: Option<String>,
    pub active_successor_ids: Vec<String>,
    pub status: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub memory_type: MemoryType,
    pub assertion_origin: AssertionOrigin,
    pub capture_fidelity: CaptureFidelity,
    pub source_artifact_ids: Vec<String>,
    pub occurred_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryOutcome {
    pub schema_version: String,
    pub items: Vec<MemoryView>,
    pub receipt: ContextDeliveryReceipt,
}
