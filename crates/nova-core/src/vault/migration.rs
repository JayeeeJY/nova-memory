//! Same-owner, empty-target recovery. No credentials, grants, context packs or original keys.
use super::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;
const VERSION: &str = "nova.portable.v0.4";
const MAX_BYTES: usize = 64 * 1024 * 1024;
const AAD: &[u8] = b"nova.portable.encrypted.v0.1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationPackage {
    version: String,
    owner: String,
    sources: Vec<SourceArtifact>,
    proposals: Vec<MemoryProposal>,
    memories: Vec<CanonicalMemory>,
    tombstones: Vec<String>,
    source_audit_head: Option<String>,
    original_permissions: BTreeMap<String, Vec<String>>,
    audit_evidence: Vec<MigrationAuditEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    collections: Vec<crate::MemoryCollection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    templates: Vec<crate::StructureTemplate>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationAuditEvidence {
    sequence: i64,
    event_type: String,
    aggregate_id: String,
    occurred_at: String,
    event_hash: String,
}
#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub package_digest: String,
    pub source_count: usize,
    pub proposal_count: usize,
    pub memory_count: usize,
    pub tombstone_count: usize,
    pub replayed: bool,
    pub client_access: &'static str,
}
impl MigrationPackage {
    fn validate(&self, owner: &str) -> Result<()> {
        if !matches!(
            self.version.as_str(),
            VERSION | "nova.portable.v0.3" | "nova.portable.v0.2" | "nova.portable.v0.1"
        ) || self.owner != owner
            || owner.is_empty()
            || owner.len() > 128
            || !owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
        {
            return Err(NovaError::PolicyDenied(
                "migration version or owner mismatch".into(),
            ));
        }
        if self.audit_evidence.iter().any(|e| {
            e.sequence <= 0
                || e.event_type.len() > 64
                || e.aggregate_id.len() > 128
                || e.event_hash.len() > 128
                || OffsetDateTime::parse(&e.occurred_at, &Rfc3339).is_err()
        }) {
            return Err(NovaError::Validation(
                "invalid migration audit evidence".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        if self.audit_evidence.len() > 100_000 {
            return Err(NovaError::Validation("migration audit limit".into()));
        }
        let total =
            self.sources.len() + self.proposals.len() + self.memories.len() + self.tombstones.len();
        if total > 10_000 {
            return Err(NovaError::Validation("migration object limit".into()));
        }
        for id in self
            .sources
            .iter()
            .map(|s| &s.source_id)
            .chain(self.proposals.iter().map(|p| &p.proposal_id))
            .chain(self.memories.iter().map(|m| &m.memory_id))
            .chain(self.tombstones.iter())
        {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !ids.insert(id)
            {
                return Err(NovaError::Validation(
                    "invalid or duplicate migration ID".into(),
                ));
            }
        }
        let sources: BTreeSet<_> = self.sources.iter().map(|s| s.source_id.as_str()).collect();
        let proposals: BTreeSet<_> = self
            .proposals
            .iter()
            .map(|p| p.proposal_id.as_str())
            .collect();
        let memories: BTreeSet<_> = self
            .memories
            .iter()
            .map(|m| m.memory_id.as_str())
            .chain(self.tombstones.iter().map(String::as_str))
            .collect();
        if (self.version == "nova.portable.v0.1" && !self.collections.is_empty())
            || self
                .collections
                .iter()
                .map(|c| &c.collection_id)
                .collect::<BTreeSet<_>>()
                .len()
                != self.collections.len()
            || self.collections.len() > 1000
            || self.collections.iter().any(|c| {
                c.owner_id != owner
                    || c.collection_id.len() > 128
                    || !c.collection_id.starts_with("collection_")
                    || c.space.is_empty()
                    || c.space.len() > 128
                    || c.revision == 0
                    || c.member_ids.len() > 100
                    || c.member_ids
                        .iter()
                        .any(|id| !memories.contains(id.as_str()))
            })
        {
            return Err(NovaError::Validation("invalid portable collections".into()));
        }
        if self.original_permissions.len() != self.proposals.len() + self.memories.len()
            || self
                .proposals
                .iter()
                .map(|p| &p.proposal_id)
                .chain(self.memories.iter().map(|m| &m.memory_id))
                .any(|id| !self.original_permissions.contains_key(id))
        {
            return Err(NovaError::Validation(
                "incomplete migration permissions provenance".into(),
            ));
        }
        for s in &self.sources {
            if s.owner_id != owner
                || s.schema_version != NMDS_VERSION
                || s.content_hash != sha256_hex(&[s.content.as_bytes()])
            {
                return Err(NovaError::Validation("invalid migration source".into()));
            }
        }
        for p in &self.proposals {
            if p.owner_id != owner
                || p.schema_version != NMDS_VERSION
                || !sources.contains(p.source_artifact_id.as_str())
                || p.status == ProposalStatus::Deleted
                || p.structure.as_ref().is_some_and(|s| {
                    p.status != ProposalStatus::Confirmed
                        && s.source_artifact_id != p.source_artifact_id
                })
            {
                return Err(NovaError::Validation("invalid migration proposal".into()));
            }
        }
        if self
            .proposals
            .iter()
            .filter(|p| p.status == ProposalStatus::Confirmed)
            .count()
            != self.memories.len()
        {
            return Err(NovaError::Validation(
                "confirmed proposal mapping incomplete".into(),
            ));
        }
        let mut used_proposals = BTreeSet::new();
        for m in &self.memories {
            let p = self
                .proposals
                .iter()
                .find(|p| p.proposal_id == m.proposal_id);
            if m.owner_id != owner
                || m.schema_version != NMDS_VERSION
                || m.status == MemoryStatus::Deleted
                || m.structure
                    .as_ref()
                    .is_some_and(|s| !m.source_artifact_ids.contains(&s.source_artifact_id))
                || !proposals.contains(m.proposal_id.as_str())
                || !used_proposals.insert(&m.proposal_id)
                || p.is_none_or(|p| {
                    p.status != ProposalStatus::Confirmed || p.structure != m.structure
                })
                || m.source_artifact_ids
                    .iter()
                    .any(|id| !sources.contains(id.as_str()))
                || m.supersedes
                    .iter()
                    .chain(&m.superseded_by)
                    .chain(&m.conflicts_with)
                    .any(|id| !memories.contains(id.as_str()))
            {
                return Err(NovaError::Validation(
                    "invalid migration memory relationship".into(),
                ));
            }
        }
        let mut template_versions = BTreeSet::new();
        let mut latest = BTreeMap::<&str, u64>::new();
        let mut field_kinds = BTreeMap::new();
        if self.templates.len() > 10_000 {
            return Err(NovaError::Validation("too many template versions".into()));
        }
        for t in &self.templates {
            t.validate_definition()?;
            for field in &t.fields {
                let key = (&t.template_id, &field.id);
                if field_kinds
                    .insert(key, field.kind)
                    .is_some_and(|kind| kind != field.kind)
                {
                    return Err(NovaError::Validation(
                        "field kind changes within template history".into(),
                    ));
                }
            }
            if (self.version != VERSION && !t.fields.is_empty())
                || t.owner_id != owner
                || !template_versions.insert((&t.template_id, t.revision))
                || !t.template_id.starts_with("template_")
                || t.template_id.len() > 128
                || !t
                    .template_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || t.revision == 0
                || latest
                    .get(t.template_id.as_str())
                    .is_some_and(|previous| *previous >= t.revision)
            {
                return Err(NovaError::Validation("invalid migration template".into()));
            }
            latest.insert(&t.template_id, t.revision);
        }
        if latest.len() > 100 {
            return Err(NovaError::Validation("too many templates".into()));
        }
        for (structure, memory_type) in self
            .proposals
            .iter()
            .map(|p| (&p.structure, p.memory_type))
            .chain(self.memories.iter().map(|m| (&m.structure, m.memory_type)))
        {
            if let Some(s) = structure {
                let source = self
                    .sources
                    .iter()
                    .find(|source| source.source_id == s.source_artifact_id)
                    .ok_or_else(|| NovaError::Validation("structured source missing".into()))?;
                if self.version != VERSION
                    || s.template.owner_id != owner
                    || s.template.memory_type != memory_type
                    || !self.templates.iter().any(|t| t == &s.template)
                    || !s
                        .template
                        .validate_sample(&source.content, &s.values, &s.evidence)?
                        .valid
                {
                    return Err(NovaError::Validation(
                        "invalid structured migration record".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    fn report(&self, digest: String, replayed: bool) -> MigrationReport {
        MigrationReport {
            package_digest: digest,
            source_count: self.sources.len(),
            proposal_count: self.proposals.len(),
            memory_count: self.memories.len(),
            tombstone_count: self.tombstones.len(),
            replayed,
            client_access: "requires_new_owner_approval",
        }
    }
    pub fn inspect_encrypted(bytes: &[u8], key: &VaultKey, owner: &str) -> Result<MigrationReport> {
        let (p, d) = Self::decode(bytes, key, owner)?;
        Ok(p.report(d, false))
    }
    fn decode(bytes: &[u8], key: &VaultKey, owner: &str) -> Result<(Self, String)> {
        if bytes.len() < 24 + 16 || bytes.len() > MAX_BYTES {
            return Err(NovaError::Validation("migration file size".into()));
        }
        let plain = Zeroizing::new(key.decrypt(
            &EncryptedBlob {
                nonce: bytes[..24].to_vec(),
                ciphertext: bytes[24..].to_vec(),
            },
            AAD,
        )?);
        let value: serde_json::Value = serde_json::from_slice(&plain)?;
        let p: Self = serde_json::from_value(value.clone())?;
        // Reject ignored fields even inside existing record types; do not silently strip evidence.
        fn known(input: &serde_json::Value, output: &serde_json::Value) -> bool {
            match (input, output) {
                (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                    a.iter().all(|(k, v)| b.get(k).is_some_and(|o| known(v, o)))
                }
                (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(v, o)| known(v, o))
                }
                _ => true,
            }
        }
        if !known(&value, &serde_json::to_value(&p)?) {
            return Err(NovaError::Validation(
                "unknown migration record fields".into(),
            ));
        }
        p.validate(owner)?;
        let digest = sha256_hex(&[&serde_json::to_vec(&p)?]);
        Ok((p, digest))
    }
}
impl Vault {
    pub fn export_encrypted(&mut self, owner: &str, package_key: &VaultKey) -> Result<Vec<u8>> {
        if self.key.to_hex() == package_key.to_hex() {
            return Err(NovaError::PolicyDenied(
                "package key must differ from Vault key".into(),
            ));
        }
        let audit = self.verify_audit_chain()?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let head: Option<String> = tx
            .query_row(
                "SELECT event_hash FROM event_log ORDER BY sequence DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if head != audit.head_hash {
            return Err(NovaError::InvalidState(
                "export changed during audit verification; retry".into(),
            ));
        }
        let audit_evidence = {
            let mut s=tx.prepare("SELECT sequence,event_type,aggregate_id,occurred_at,event_hash FROM event_log WHERE event_type IN ('SourceCaptured','MemoryProposed','MemoryConfirmed','MemoryCorrected','MemoryDisputed','MemoryDeleted','MigrationImported','ClientAccessAuthorized') ORDER BY sequence")?;
            s.query_map([], |r| {
                Ok(MigrationAuditEvidence {
                    sequence: r.get(0)?,
                    event_type: r.get(1)?,
                    aggregate_id: r.get(2)?,
                    occurred_at: r.get(3)?,
                    event_hash: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut tombstones: Vec<String> = Vec::new();
        let deleted: Vec<String> = {
            let mut s = tx.prepare(
                "SELECT DISTINCT aggregate_id FROM event_log WHERE event_type='MemoryDeleted'",
            )?;
            s.query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        tombstones.extend(deleted);
        let mut inherited_permissions = BTreeMap::<String, Vec<String>>::new();
        let imported: Vec<String> = {
            let mut s = tx.prepare(
                "SELECT aggregate_id FROM event_log WHERE event_type='MigrationImported'",
            )?;
            s.query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        for id in imported {
            let evidence: serde_json::Value =
                load_event_payload(&tx, &self.key, "MigrationImported", &id)?;
            if let Some(scopes) = evidence.get("original_permissions") {
                inherited_permissions.extend(serde_json::from_value::<
                    BTreeMap<String, Vec<String>>,
                >(scopes.clone())?);
            }
            if let Some(ids) = evidence["tombstones"].as_array() {
                for id in ids {
                    if let Some(id) = id.as_str() {
                        tombstones.push(id.to_owned());
                    }
                }
            }
        }
        tombstones.sort();
        tombstones.dedup();
        let mut p = MigrationPackage {
            version: VERSION.into(),
            owner: owner.into(),
            sources: load_all_encrypted_records(
                &tx,
                &self.key,
                "source_artifacts",
                "source_id ASC",
            )?,
            proposals: load_all_encrypted_records(
                &tx,
                &self.key,
                "memory_proposals",
                "proposal_id ASC",
            )?,
            memories: load_all_encrypted_records(
                &tx,
                &self.key,
                "canonical_memories",
                "memory_id ASC",
            )?,
            tombstones,
            source_audit_head: audit.head_hash,
            original_permissions: BTreeMap::new(),
            audit_evidence,
            templates: platform::template_history(&tx, &self.key, None)?,
            collections: {
                let mut stmt=tx.prepare("SELECT DISTINCT aggregate_id FROM event_log WHERE event_type='CollectionUpdated'")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                ids.iter()
                    .map(|id| load_event_payload(&tx, &self.key, "CollectionUpdated", id))
                    .collect::<Result<Vec<_>>>()?
            },
        };
        for (id, scopes) in p
            .proposals
            .iter()
            .map(|p| (&p.proposal_id, &p.permission_scope))
            .chain(
                p.memories
                    .iter()
                    .map(|m| (&m.memory_id, &m.permission_scope)),
            )
        {
            p.original_permissions.insert(
                id.clone(),
                inherited_permissions
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| scopes.clone()),
            );
        }
        p.validate(owner)?;
        let plain = Zeroizing::new(serde_json::to_vec(&p)?);
        if plain.len() > MAX_BYTES - 40 {
            return Err(NovaError::Validation("migration file size".into()));
        }
        let encrypted = package_key.encrypt(&plain, AAD)?;
        tx.commit()?;
        Ok([encrypted.nonce, encrypted.ciphertext].concat())
    }
    /// Check a recovery against the current target without importing any records.
    pub fn preflight_import_encrypted(
        &self,
        bytes: &[u8],
        package_key: &VaultKey,
        owner: &str,
    ) -> Result<MigrationReport> {
        if self.key.to_hex() == package_key.to_hex() {
            return Err(NovaError::PolicyDenied(
                "package key must differ from target key".into(),
            ));
        }
        let (package, digest) = MigrationPackage::decode(bytes, package_key, owner)?;
        let snapshot = self.connection.unchecked_transaction()?;
        self.verify_audit_chain()?;
        let already = target_replayed(&snapshot, &digest)?;
        snapshot.commit()?;
        Ok(package.report(digest, already))
    }

    pub fn import_encrypted(
        &mut self,
        bytes: &[u8],
        package_key: &VaultKey,
        owner: &str,
    ) -> Result<MigrationReport> {
        if self.key.to_hex() == package_key.to_hex() {
            return Err(NovaError::PolicyDenied(
                "package key must differ from target key".into(),
            ));
        }
        let (mut p, digest) = MigrationPackage::decode(bytes, package_key, owner)?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let already = target_replayed(&tx, &digest)?;
        if already {
            return Ok(p.report(digest, true));
        }
        for s in &p.sources {
            let (id, key) = create_data_key(&tx, &self.key, &s.source_id, "source")?;
            insert_source(&tx, &id, &key, s)?;
        }
        for proposal in &mut p.proposals {
            proposal
                .permission_scope
                .retain(|s| s.starts_with("space:"));
            if proposal.permission_scope.is_empty() {
                proposal.permission_scope.push("space:personal".into());
            }
            proposal
                .permission_scope
                .extend(["personal_review".into(), "client:unapproved".into()]);
            let (id, key) = create_data_key(&tx, &self.key, &proposal.proposal_id, "proposal")?;
            insert_proposal(&tx, &id, &key, proposal)?;
        }
        for m in &mut p.memories {
            m.permission_scope.retain(|s| s.starts_with("space:"));
            if m.permission_scope.is_empty() {
                m.permission_scope.push("space:personal".into());
            }
            m.permission_scope
                .extend(["personal_review".into(), "client:unapproved".into()]);
            let (id, key) = create_data_key(&tx, &self.key, &m.memory_id, "memory")?;
            insert_memory(&tx, &id, &key, m)?;
        }
        let mut restored_templates = BTreeSet::new();
        for t in &p.templates {
            let (kid, k) = if restored_templates.insert(&t.template_id) {
                create_data_key(&tx, &self.key, &t.template_id, "structure_template")?
            } else {
                load_key_by_aggregate(&tx, &self.key, &t.template_id)?
            };
            append_event(
                &tx,
                &self.audit_key,
                &kid,
                &k,
                "StructureTemplateUpdated",
                &t.template_id,
                t,
            )?;
        }
        for c in &p.collections {
            let (kid, k) = create_data_key(&tx, &self.key, &c.collection_id, "collection")?;
            append_event(
                &tx,
                &self.audit_key,
                &kid,
                &k,
                "CollectionUpdated",
                &c.collection_id,
                c,
            )?;
        }
        append_event(
            &tx,
            &self.audit_key,
            "dek_audit_v1",
            &self.audit_key,
            "MigrationImported",
            &digest,
            &json!({"owner":owner,"package_digest":digest,"source_audit_head":p.source_audit_head,"audit_evidence":p.audit_evidence,"tombstones":p.tombstones,"original_permissions":p.original_permissions,"client_access":"requires_new_owner_approval"}),
        )?;
        tx.commit()?;
        Ok(p.report(digest, false))
    }
}

fn target_replayed(connection: &Connection, digest: &str) -> Result<bool> {
    let already: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM event_log WHERE event_type='MigrationImported' AND aggregate_id=?1)",
        params![digest], |r| r.get(0),
    )?;
    if already {
        return Ok(true);
    }
    let used: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_artifacts) OR EXISTS(SELECT 1 FROM memory_proposals) OR EXISTS(SELECT 1 FROM canonical_memories) OR EXISTS(SELECT 1 FROM event_log)",
        [], |r| r.get(0),
    )?;
    if used {
        return Err(NovaError::InvalidState(
            "migration target must be empty; conflicting snapshots are not merged".into(),
        ));
    }
    Ok(false)
}

impl Vault {
    /// Owner control-plane snapshot; this is not exposed as an MCP tool.
    pub fn memory_access_snapshot(&self, owner: &str, id: &str) -> Result<String> {
        let m: CanonicalMemory = load_encrypted_record(
            &self.connection,
            &self.key,
            "canonical_memories",
            "memory_id",
            id,
        )?;
        assert_owner_and_active(&m, owner, "review client access")?;
        Ok(sha256_hex(&[&serde_json::to_vec(&m)?]))
    }
    pub fn authorize_memory_client(
        &mut self,
        owner: &str,
        id: &str,
        client: &str,
        snapshot: &str,
    ) -> Result<()> {
        if !matches!(client, "codex" | "chatgpt") {
            return Err(NovaError::Validation("unsupported client".into()));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut m: CanonicalMemory =
            load_encrypted_record(&tx, &self.key, "canonical_memories", "memory_id", id)?;
        assert_owner_and_active(&m, owner, "authorize client")?;
        if sha256_hex(&[&serde_json::to_vec(&m)?]) != snapshot {
            return Err(NovaError::InvalidState(
                "client access snapshot is stale".into(),
            ));
        }
        // Convert the historical unscoped ChatGPT policy to explicit scope without broadening it.
        if !m.permission_scope.iter().any(|s| s.starts_with("client:")) {
            m.permission_scope.push("client:chatgpt".into());
        }
        m.permission_scope.retain(|s| s != "client:unapproved");
        for scope in [format!("client:{client}"), "space:personal".into()] {
            if !m.permission_scope.contains(&scope) {
                m.permission_scope.push(scope);
            }
        }
        m.updated_at = now()?;
        update_encrypted_record(&tx, &self.key, "canonical_memories", "memory_id", id, &m)?;
        append_event(
            &tx,
            &self.audit_key,
            "dek_audit_v1",
            &self.audit_key,
            "ClientAccessAuthorized",
            id,
            &json!({"memory_id":id,"client":client,"owner":owner,"base_snapshot":snapshot}),
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod migration_validation_tests {
    use super::*;
    fn seal(value: &serde_json::Value, key: &VaultKey) -> Vec<u8> {
        let e = key
            .encrypt(&serde_json::to_vec(value).unwrap(), AAD)
            .unwrap();
        [e.nonce, e.ciphertext].concat()
    }
    #[test]
    fn authenticated_unknown_fields_versions_and_missing_relationships_are_rejected() {
        let key = VaultKey::generate().unwrap();
        let base = json!({"version":VERSION,"owner":"owner:founder","sources":[],"proposals":[],"memories":[],"tombstones":[],"source_audit_head":null,"original_permissions":{},"audit_evidence":[]});
        let mut bad = base.clone();
        bad["client_token"] = json!("synthetic credential");
        assert!(
            MigrationPackage::inspect_encrypted(&seal(&bad, &key), &key, "owner:founder").is_err()
        );
        let mut bad = base.clone();
        bad["version"] = json!("nova.portable.future");
        assert!(
            MigrationPackage::inspect_encrypted(&seal(&bad, &key), &key, "owner:founder").is_err()
        );
        let mut bad = base.clone();
        bad["original_permissions"] = json!({"missing_record":["client:codex"]});
        assert!(
            MigrationPackage::inspect_encrypted(&seal(&bad, &key), &key, "owner:founder").is_err()
        );
        assert!(
            MigrationPackage::inspect_encrypted(&seal(&base, &key), &key, "owner:founder").is_ok()
        );
    }
}
