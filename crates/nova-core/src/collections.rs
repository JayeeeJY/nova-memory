//! Owner-authored collections and bounded reusable disclosure authority.
use crate::PrivacyLevel;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCollection {
    pub collection_id: String,
    pub owner_id: String,
    pub space: String,
    pub title: String,
    pub member_ids: Vec<String>,
    pub revision: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionGrantRequest {
    pub collection_id: String,
    pub client: String,
    pub purpose: String,
    pub ttl_seconds: i64,
    pub per_call_limit: usize,
    pub call_quota: usize,
    pub maximum_privacy: PrivacyLevel,
    #[serde(default)]
    pub include_future_members: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionGrant {
    pub approval_id: String,
    pub owner_id: String,
    pub space: String,
    pub collection_id: String,
    pub client: String,
    pub purpose: String,
    pub member_snapshot: Vec<String>,
    pub expires_at: String,
    pub per_call_limit: usize,
    pub call_quota: usize,
    pub maximum_privacy: PrivacyLevel,
    pub include_future_members: bool,
}
