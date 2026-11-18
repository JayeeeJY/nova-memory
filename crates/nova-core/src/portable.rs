//! Experimental portable inventory validation. No Vault export, import or authority transfer.
//! Payloads are opaque bytes: integrity validation does not establish record authenticity.
use crate::{NovaError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const PORTABLE_VERSION: &str = "nova.portable.inventory.v0.1";
const MAX_OBJECTS: usize = 10_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableKind {
    Source,
    Proposal,
    Canonical,
    Audit,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortableState {
    Pending,
    Confirmed,
    Rejected,
    Active,
    Disputed,
    Superseded,
    Deleted,
    Retained,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableObject {
    pub id: String,
    pub kind: PortableKind,
    pub state: PortableState,
    pub revision: u64,
    pub sha256: Option<String>,
    pub references: Vec<String>,
    /// Missing evidence is explicit; it is never treated as a supplied Source.
    pub missing_source_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableInventory {
    pub schema_version: String,
    pub owner: String,
    pub space: String,
    pub objects: Vec<PortableObject>,
}

fn invalid() -> NovaError {
    // Avoid exposing imported IDs, contents or paths in diagnostics.
    NovaError::Validation("invalid portable inventory or payload set".into())
}

impl PortableInventory {
    /// Validate a bounded inventory and exact payload set without reading paths or mutating storage.
    /// expected_owner is a local mapping check, not proof of identity or permission to import.
    pub fn validate(&self, expected_owner: &str, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        if self.schema_version != PORTABLE_VERSION
            || self.owner != expected_owner
            || self.owner.is_empty()
            || self.owner.len() > 128
            || self.space != "personal"
            || self.objects.len() > MAX_OBJECTS
        {
            return Err(invalid());
        }
        let mut ids = BTreeSet::new();
        let mut payload_ids = BTreeSet::new();
        let mut size = 0usize;
        for object in &self.objects {
            if !valid_id(&object.id) || !ids.insert(object.id.as_str()) || object.revision == 0 {
                return Err(invalid());
            }
            let valid_state = match object.kind {
                PortableKind::Source | PortableKind::Audit => {
                    object.state == PortableState::Retained
                }
                PortableKind::Proposal => matches!(
                    object.state,
                    PortableState::Pending | PortableState::Confirmed | PortableState::Rejected
                ),
                PortableKind::Canonical => matches!(
                    object.state,
                    PortableState::Active
                        | PortableState::Disputed
                        | PortableState::Superseded
                        | PortableState::Deleted
                ),
            };
            if !valid_state || object.references.len() + object.missing_source_ids.len() > 100 {
                return Err(invalid());
            }
            if object.state == PortableState::Deleted {
                if object.sha256.is_some() || blobs.contains_key(&object.id) {
                    return Err(invalid());
                }
            } else {
                let bytes = blobs.get(&object.id).ok_or_else(invalid)?;
                size = size.checked_add(bytes.len()).ok_or_else(invalid)?;
                if size > MAX_BYTES
                    || object.sha256.as_deref() != Some(hex::encode(Sha256::digest(bytes)).as_str())
                {
                    return Err(invalid());
                }
                payload_ids.insert(object.id.as_str());
            }
        }
        if blobs.len() != payload_ids.len() {
            return Err(invalid());
        }
        for object in &self.objects {
            let mut seen = BTreeSet::new();
            for id in &object.references {
                if id == &object.id || !ids.contains(id.as_str()) || !seen.insert(id.as_str()) {
                    return Err(invalid());
                }
            }
            for id in &object.missing_source_ids {
                if !valid_id(id) || ids.contains(id.as_str()) || !seen.insert(id.as_str()) {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PortableInventory, BTreeMap<String, Vec<u8>>) {
        let blob = b"synthetic retained record".to_vec();
        let object = PortableObject {
            id: "memory_test".into(),
            kind: PortableKind::Canonical,
            state: PortableState::Active,
            revision: 1,
            sha256: Some(hex::encode(Sha256::digest(&blob))),
            references: vec![],
            missing_source_ids: vec!["src_missing".into()],
        };
        (
            PortableInventory {
                schema_version: PORTABLE_VERSION.into(),
                owner: "owner:founder".into(),
                space: "personal".into(),
                objects: vec![object],
            },
            BTreeMap::from([("memory_test".into(), blob)]),
        )
    }
    #[test]
    fn inventory_roundtrip_preserves_missing_evidence() {
        let (inventory, blobs) = fixture();
        let decoded: PortableInventory =
            serde_json::from_slice(&serde_json::to_vec(&inventory).unwrap()).unwrap();
        decoded.validate("owner:founder", &blobs).unwrap();
        assert_eq!(decoded.objects[0].missing_source_ids, ["src_missing"]);
    }
    #[test]
    fn tampered_missing_and_extra_payloads_fail() {
        let (inventory, mut blobs) = fixture();
        blobs.get_mut("memory_test").unwrap().push(0);
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        blobs.clear();
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        let (_, mut blobs) = fixture();
        blobs.insert("extra".into(), vec![]);
        assert!(inventory.validate("owner:founder", &blobs).is_err());
    }
    #[test]
    fn duplicate_dangling_and_path_ids_fail() {
        let (mut inventory, blobs) = fixture();
        inventory.objects.push(inventory.objects[0].clone());
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        inventory.objects.pop();
        inventory.objects[0].references.push("unknown".into());
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        inventory.objects[0].references.clear();
        inventory.objects[0].id = "../vault.key".into();
        assert!(inventory.validate("owner:founder", &blobs).is_err());
    }
    #[test]
    fn tombstone_cannot_carry_payload() {
        let (mut inventory, mut blobs) = fixture();
        inventory.objects[0].state = PortableState::Deleted;
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        inventory.objects[0].sha256 = None;
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        blobs.clear();
        inventory.validate("owner:founder", &blobs).unwrap();
    }
    #[test]
    fn owner_version_revision_and_state_mismatch_fail() {
        let (mut inventory, blobs) = fixture();
        assert!(inventory.validate("another-owner", &blobs).is_err());
        inventory.schema_version = "unknown".into();
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        inventory.schema_version = PORTABLE_VERSION.into();
        inventory.objects[0].revision = 0;
        assert!(inventory.validate("owner:founder", &blobs).is_err());
        inventory.objects[0].revision = 1;
        inventory.objects[0].state = PortableState::Pending;
        assert!(inventory.validate("owner:founder", &blobs).is_err());
    }
    #[test]
    fn authority_kinds_and_unknown_fields_are_rejected() {
        let (inventory, _) = fixture();
        let mut value = serde_json::to_value(inventory).unwrap();
        value["objects"][0]["kind"] = "disclosure_grant".into();
        assert!(serde_json::from_value::<PortableInventory>(value.clone()).is_err());
        value["objects"][0]["kind"] = "canonical".into();
        value["client_token"] = "not-a-real-token".into();
        assert!(serde_json::from_value::<PortableInventory>(value).is_err());
    }
}
