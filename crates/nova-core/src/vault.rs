mod collections;
mod generic;
mod migration;
mod owner_access;
mod platform;
pub(crate) mod migration_types {
    pub use super::migration::{MigrationPackage, MigrationReport};
}
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::crypto::{DataKey, EncryptedBlob, VaultKey, hmac_sha256_hex, sha256_hex};
use crate::error::{NovaError, Result};
use crate::model::*;
use crate::structure::*;
use crate::{NMDS_VERSION, POLICY_VERSION};

const VAULT_SCHEMA_VERSION: &str = "2";
const KEY_CHECK_PLAINTEXT: &[u8] = b"nova-memory-vault-key-check:v1";
const AUDIT_AGGREGATE_ID: &str = "nova:audit:v1";

pub struct Vault {
    connection: Connection,
    key: VaultKey,
    audit_key: DataKey,
}

impl Vault {
    /// Inspect a current-schema Vault without changing permissions or upgrading old schemas.
    pub fn open_readonly(path: &Path, key: VaultKey) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        if metadata(&connection, "schema_version")?.as_deref()
            != Some(VAULT_SCHEMA_VERSION.as_bytes())
        {
            return Err(NovaError::Validation(
                "read-only inspection requires current Vault schema; upgrade separately".into(),
            ));
        }
        let nonce = metadata(&connection, "key_check_nonce")?.ok_or(NovaError::InvalidVaultKey)?;
        let ciphertext =
            metadata(&connection, "key_check_ciphertext")?.ok_or(NovaError::InvalidVaultKey)?;
        let check = key
            .decrypt(&EncryptedBlob { nonce, ciphertext }, b"vault:key-check:v1")
            .map_err(|_| NovaError::InvalidVaultKey)?;
        if check != KEY_CHECK_PLAINTEXT {
            return Err(NovaError::InvalidVaultKey);
        }
        let (_, audit_key) = load_key_by_aggregate(&connection, &key, AUDIT_AGGREGATE_ID)?;
        Ok(Self {
            connection,
            key,
            audit_key,
        })
    }

    pub fn create(path: &Path, key: VaultKey) -> Result<Self> {
        if path.exists() {
            return Err(NovaError::VaultAlreadyExists(path.display().to_string()));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(path)?;
        restrict_vault_file_permissions(path)?;
        configure_connection(&connection)?;
        create_schema(&connection)?;

        let key_check = key.encrypt(KEY_CHECK_PLAINTEXT, b"vault:key-check:v1")?;
        connection.execute(
            "INSERT INTO vault_metadata (key, value) VALUES (?1, ?2)",
            params!["schema_version", VAULT_SCHEMA_VERSION.as_bytes()],
        )?;
        connection.execute(
            "INSERT INTO vault_metadata (key, value) VALUES (?1, ?2)",
            params!["key_check_nonce", key_check.nonce],
        )?;
        connection.execute(
            "INSERT INTO vault_metadata (key, value) VALUES (?1, ?2)",
            params!["key_check_ciphertext", key_check.ciphertext],
        )?;

        let tx = connection.transaction()?;
        let audit_key =
            create_data_key_with_id(&tx, &key, "dek_audit_v1", AUDIT_AGGREGATE_ID, "audit")?;
        tx.commit()?;
        Ok(Self {
            connection,
            key,
            audit_key,
        })
    }

    pub fn open(path: &Path, key: VaultKey) -> Result<Self> {
        if !path.exists() {
            return Err(NovaError::VaultNotFound(path.display().to_string()));
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        restrict_vault_file_permissions(path)?;
        configure_connection(&connection)?;

        let schema_version = metadata(&connection, "schema_version")?
            .ok_or_else(|| NovaError::Validation("vault schema version is missing".into()))?;
        if schema_version != b"1" && schema_version != VAULT_SCHEMA_VERSION.as_bytes() {
            return Err(NovaError::Validation(format!(
                "unsupported vault schema version: {}",
                String::from_utf8_lossy(&schema_version)
            )));
        }

        let nonce =
            metadata(&connection, "key_check_nonce")?.ok_or_else(|| NovaError::InvalidVaultKey)?;
        let ciphertext = metadata(&connection, "key_check_ciphertext")?
            .ok_or_else(|| NovaError::InvalidVaultKey)?;
        let check = key
            .decrypt(&EncryptedBlob { nonce, ciphertext }, b"vault:key-check:v1")
            .map_err(|_| NovaError::InvalidVaultKey)?;
        if check != KEY_CHECK_PLAINTEXT {
            return Err(NovaError::InvalidVaultKey);
        }

        if schema_version == b"1" {
            migrate_v1_to_v2(&mut connection)?;
        }

        let (_, audit_key) = load_key_by_aggregate(&connection, &key, AUDIT_AGGREGATE_ID)?;
        Ok(Self {
            connection,
            key,
            audit_key,
        })
    }

    pub fn capture(&mut self, request: CaptureRequest) -> Result<CaptureOutcome> {
        validate_capture_request(&request)?;
        let mut structure = if let Some(input) = &request.structure {
            let template = self.owner_template_version(
                &request.owner_id,
                &input.template_id,
                input.template_revision,
            )?;
            if template.memory_type != request.memory_type
                || !template
                    .validate_sample(&request.source_content, &input.values, &input.evidence)?
                    .valid
            {
                return Err(NovaError::Validation(
                    "invalid structured capture; validate sample first".into(),
                ));
            }
            Some(StructuredMemory {
                template,
                values: input.values.clone(),
                evidence: input.evidence.clone(),
                source_artifact_id: String::new(),
            })
        } else {
            None
        };
        let requested_at = now()?;
        let connector_supplied_request_id = request.request_id.is_some();
        let request_id = request.request_id.clone().unwrap_or_else(|| new_id("req"));
        let request_hash = connector_supplied_request_id
            .then(|| request_fingerprint(&self.audit_key, "capture", &request))
            .transpose()?;
        if let Some(request_hash) = &request_hash
            && let Some(outcome) =
                replay_capture(&self.connection, &self.key, &request_id, request_hash)?
        {
            return Ok(outcome);
        }
        let observed_at = request
            .observed_at
            .clone()
            .unwrap_or_else(|| requested_at.clone());
        let source_id = new_id("src");
        if let Some(s) = &mut structure {
            s.source_artifact_id = source_id.clone();
        }
        let proposal_id = new_id("proposal");

        let source = SourceArtifact {
            source_id: source_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.owner_id.clone(),
            source_platform: request.source_platform.clone(),
            capture_channel: request.capture_channel,
            capture_fidelity: request.capture_fidelity,
            content: request.source_content.clone(),
            observed_at: observed_at.clone(),
            created_at: requested_at.clone(),
            content_hash: sha256_hex(&[request.source_content.as_bytes()]),
        };

        let review_tier = match request.privacy_level {
            PrivacyLevel::Sensitive | PrivacyLevel::Sealed => ReviewTier::Restricted,
            _ => request.review_tier,
        };
        let proposal = MemoryProposal {
            structure,
            review_revision: 0,
            proposal_id: proposal_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.owner_id.clone(),
            source_artifact_id: source_id.clone(),
            title: request.title,
            memory_type: request.memory_type,
            entities: Vec::new(),
            tags: Vec::new(),
            summary: request.summary,
            decisions: request.decisions,
            emotion: None,
            privacy_level: request.privacy_level,
            permission_scope: request.permission_scope,
            purpose_limits: request.purpose_limits,
            model_eligibility: ModelEligibility {
                allow_personal_model: request.privacy_level != PrivacyLevel::Sealed,
                allowed_facets: Vec::new(),
            },
            observed_at,
            created_at: requested_at.clone(),
            proposed_origin: request.proposed_origin,
            review_tier,
            status: ProposalStatus::Pending,
        };

        let tx = self.connection.transaction()?;
        let (source_key_id, source_key) =
            create_data_key(&tx, &self.key, &source.source_id, "source_artifact")?;
        insert_source(&tx, &source_key_id, &source_key, &source)?;
        append_event(
            &tx,
            &self.audit_key,
            &source_key_id,
            &source_key,
            "SourceCaptured",
            &source.source_id,
            &source,
        )?;

        let (proposal_key_id, proposal_key) =
            create_data_key(&tx, &self.key, &proposal.proposal_id, "memory_proposal")?;
        insert_proposal(&tx, &proposal_key_id, &proposal_key, &proposal)?;
        let proposal_event_hash = append_event(
            &tx,
            &self.audit_key,
            &proposal_key_id,
            &proposal_key,
            "MemoryProposed",
            &proposal.proposal_id,
            &proposal,
        )?;

        let receipt_id = new_id("receipt");
        let completed_at = now()?;
        let integrity_hash = receipt_hash(
            &self.audit_key,
            &receipt_id,
            &request_id,
            ReceiptStatus::ProposalCreated,
            &[source_id.clone(), proposal_id.clone()],
        );
        let receipt = CaptureReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::CaptureReceipt,
                subject: request.owner_id,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: "memory.capture.propose".into(),
                source_refs: vec![source_id.clone()],
                requested_at,
                completed_at,
                status: ReceiptStatus::ProposalCreated,
                error_code: None,
                integrity_hash,
                previous_audit_event_hash: Some(proposal_event_hash),
            },
            source_id,
            proposal_id,
            source_platform: request.source_platform,
            capture_channel: request.capture_channel,
            capture_fidelity: request.capture_fidelity,
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::CaptureReceipt,
            ReceiptStatus::ProposalCreated,
            &receipt,
        )?;
        if let Some(request_hash) = request_hash {
            tx.execute(
                "INSERT INTO idempotency_records
                 (request_id, operation, request_hash, primary_id, secondary_id, receipt_id,
                  created_at)
                 VALUES (?1, 'capture', ?2, ?3, ?4, ?5, ?6)",
                params![
                    request_id,
                    request_hash,
                    source.source_id,
                    proposal.proposal_id,
                    receipt.envelope.receipt_id,
                    now()?
                ],
            )?;
        }
        tx.commit()?;

        Ok(CaptureOutcome {
            source,
            proposal,
            receipt,
        })
    }

    pub fn confirm_proposal(&mut self, request: ConfirmRequest) -> Result<ConfirmationOutcome> {
        self.confirm_proposal_checked(request, None)
    }

    fn confirm_proposal_checked(
        &mut self,
        request: ConfirmRequest,
        expected_snapshot: Option<&str>,
    ) -> Result<ConfirmationOutcome> {
        validate_request_id(request.request_id.as_deref())?;
        if request.subject.trim().is_empty() {
            return Err(NovaError::Validation("subject is required".into()));
        }
        let requested_at = now()?;
        let request_id = request.request_id.unwrap_or_else(|| new_id("req"));
        let tx = self.connection.transaction()?;
        let mut proposal: MemoryProposal = load_encrypted_record(
            &tx,
            &self.key,
            "memory_proposals",
            "proposal_id",
            &request.proposal_id,
        )?;
        if proposal.owner_id != request.subject {
            return Err(NovaError::PolicyDenied(
                "only the vault owner can confirm a proposal".into(),
            ));
        }
        if proposal.status != ProposalStatus::Pending {
            return Err(NovaError::InvalidState(format!(
                "proposal {} is {}",
                proposal.proposal_id,
                proposal.status.as_str()
            )));
        }
        if let Some(snapshot) = expected_snapshot {
            check_review_snapshot(&self.audit_key, &proposal, snapshot)?;
        }
        let source: SourceArtifact = load_encrypted_record(
            &tx,
            &self.key,
            "source_artifacts",
            "source_id",
            &proposal.source_artifact_id,
        )?;

        let memory_id = new_id("memory");
        let confirmed_at = now()?;
        let assertion_origin = proposal.proposed_origin.confirmed();
        let confidence = match assertion_origin {
            AssertionOrigin::UserSelfReport | AssertionOrigin::SourceEvidence => 1.0,
            AssertionOrigin::UserConfirmedAiExtraction => 0.95,
            AssertionOrigin::UserConfirmedInference => 0.70,
            _ => 0.50,
        };
        let content_hash = sha256_hex(&[
            proposal.title.as_bytes(),
            proposal.summary.as_bytes(),
            proposal.source_artifact_id.as_bytes(),
        ]);
        let memory = CanonicalMemory {
            structure: proposal.structure.clone(),
            memory_id: memory_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: proposal.owner_id.clone(),
            proposal_id: proposal.proposal_id.clone(),
            title: proposal.title.clone(),
            memory_type: proposal.memory_type,
            entities: proposal.entities.clone(),
            tags: proposal.tags.clone(),
            source_platform: source.source_platform,
            source_artifact_ids: vec![proposal.source_artifact_id.clone()],
            summary: proposal.summary.clone(),
            decisions: proposal.decisions.clone(),
            emotion: proposal.emotion.clone(),
            privacy_level: proposal.privacy_level,
            permission_scope: proposal.permission_scope.clone(),
            purpose_limits: proposal.purpose_limits.clone(),
            model_eligibility: proposal.model_eligibility.clone(),
            observed_at: proposal.observed_at.clone(),
            valid_from: proposal.observed_at.clone(),
            valid_to: None,
            created_at: confirmed_at.clone(),
            updated_at: confirmed_at.clone(),
            deleted_at: None,
            status: MemoryStatus::Active,
            status_reason: None,
            confidence,
            assertion_origin,
            supersedes: Vec::new(),
            superseded_by: Vec::new(),
            conflicts_with: Vec::new(),
            content_hash,
        };

        proposal.status = ProposalStatus::Confirmed;
        update_encrypted_record(
            &tx,
            &self.key,
            "memory_proposals",
            "proposal_id",
            &proposal.proposal_id,
            &proposal,
        )?;

        let (memory_key_id, memory_key) =
            create_data_key(&tx, &self.key, &memory.memory_id, "canonical_memory")?;
        insert_memory(&tx, &memory_key_id, &memory_key, &memory)?;
        let memory_event_hash = append_event(
            &tx,
            &self.audit_key,
            &memory_key_id,
            &memory_key,
            "MemoryConfirmed",
            &memory.memory_id,
            &memory,
        )?;

        let receipt_id = new_id("receipt");
        let receipt = MutationReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::MutationReceipt,
                subject: request.subject,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: "memory.confirm".into(),
                source_refs: memory.source_artifact_ids.clone(),
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::CommittedLocal,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::CommittedLocal,
                    std::slice::from_ref(&memory.memory_id),
                ),
                previous_audit_event_hash: Some(memory_event_hash),
            },
            proposal_id: Some(memory.proposal_id.clone()),
            memory_id: Some(memory.memory_id.clone()),
            mutation: "confirm".into(),
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::MutationReceipt,
            ReceiptStatus::CommittedLocal,
            &receipt,
        )?;
        tx.commit()?;

        Ok(ConfirmationOutcome { memory, receipt })
    }

    /// Read source and proposed structure together, with a stale-review guard.
    pub fn review_inbox(&self, subject: &str) -> Result<Vec<ReviewItem>> {
        if subject.trim().is_empty() {
            return Err(NovaError::Validation("subject is required".into()));
        }
        self.pending_proposals()?.into_iter().filter(|p| p.owner_id == subject).map(|proposal| {
            let source: SourceArtifact = load_encrypted_record(&self.connection, &self.key,
                "source_artifacts", "source_id", &proposal.source_artifact_id)?;
            if source.owner_id != subject {
                return Err(NovaError::PolicyDenied("source owner mismatch".into()));
            }
            let receipt_id: Option<String> = self.connection.query_row(
                "SELECT receipt_id FROM idempotency_records WHERE operation = 'capture' AND secondary_id = ?1",
                params![proposal.proposal_id], |row| row.get(0)).optional()?;
            let capture_receipt = receipt_id.map(|id| load_encrypted_record(&self.connection,
                &self.key, "receipts", "receipt_id", &id)).transpose()?;
            let snapshot = request_fingerprint(&self.audit_key, "owner_review_snapshot", &proposal)?;
            Ok(ReviewItem { proposal, source, capture_receipt, snapshot })
        }).collect()
    }

    /// Review mutations are guarded atomically by the complete proposal snapshot.
    /// A retry after mutation fails stale rather than applying a second mutation.
    pub fn review_proposal(&mut self, request: ReviewRequest) -> Result<MutationReceipt> {
        validate_request_id(request.request_id.as_deref())?;
        if request.subject.trim().is_empty() || request.expected_snapshot.trim().is_empty() {
            return Err(NovaError::Validation(
                "subject and expected_snapshot are required".into(),
            ));
        }
        if matches!(request.action, ReviewAction::Confirm) {
            return self
                .confirm_proposal_checked(
                    ConfirmRequest {
                        request_id: request.request_id,
                        subject: request.subject,
                        proposal_id: request.proposal_id,
                    },
                    Some(&request.expected_snapshot),
                )
                .map(|result| result.receipt);
        }
        let reason = match &request.action {
            ReviewAction::Edit {
                title,
                summary,
                decisions,
                reason,
            } => {
                if title.trim().is_empty()
                    || title.len() > 240
                    || summary.trim().is_empty()
                    || summary.len() > 4000
                    || decisions.len() > 20
                    || decisions
                        .iter()
                        .any(|d| d.trim().is_empty() || d.len() > 1500)
                {
                    return Err(NovaError::Validation(
                        "review fields exceed limits or are empty".into(),
                    ));
                }
                reason
            }
            ReviewAction::EditStructure { reason, .. } | ReviewAction::Reject { reason } => reason,
            ReviewAction::Confirm => unreachable!(),
        };
        if reason.trim().is_empty() || reason.len() > 2000 {
            return Err(NovaError::Validation(
                "review reason must contain 1 to 2000 bytes".into(),
            ));
        }
        let requested_at = now()?;
        let request_id = request.request_id.unwrap_or_else(|| new_id("req"));
        let tx = self.connection.transaction()?;
        let mut proposal: MemoryProposal = load_encrypted_record(
            &tx,
            &self.key,
            "memory_proposals",
            "proposal_id",
            &request.proposal_id,
        )?;
        if proposal.owner_id != request.subject {
            return Err(NovaError::PolicyDenied(
                "only the owner can review a proposal".into(),
            ));
        }
        if proposal.status != ProposalStatus::Pending {
            return Err(NovaError::InvalidState(
                "proposal is no longer pending".into(),
            ));
        }
        check_review_snapshot(&self.audit_key, &proposal, &request.expected_snapshot)?;
        let mutation = match &request.action {
            ReviewAction::Edit {
                title,
                summary,
                decisions,
                ..
            } => {
                if proposal.title == *title
                    && proposal.summary == *summary
                    && proposal.decisions == *decisions
                {
                    return Err(NovaError::Validation("no proposal changes to save".into()));
                }
                proposal.title = title.clone();
                proposal.summary = summary.clone();
                proposal.decisions = decisions.clone();
                "edit_proposal"
            }
            ReviewAction::EditStructure {
                structure: input, ..
            } => {
                let template =
                    platform::template_history(&tx, &self.key, Some(&input.template_id))?
                        .into_iter()
                        .find(|t| {
                            t.owner_id == request.subject && t.revision == input.template_revision
                        })
                        .ok_or_else(|| {
                            NovaError::PolicyDenied("template version unavailable".into())
                        })?;
                let source: SourceArtifact = load_encrypted_record(
                    &tx,
                    &self.key,
                    "source_artifacts",
                    "source_id",
                    &proposal.source_artifact_id,
                )?;
                if template.memory_type != proposal.memory_type
                    || !template
                        .validate_sample(&source.content, &input.values, &input.evidence)?
                        .valid
                {
                    return Err(NovaError::Validation("invalid structured review".into()));
                }
                let next = StructuredMemory {
                    template,
                    values: input.values.clone(),
                    evidence: input.evidence.clone(),
                    source_artifact_id: source.source_id,
                };
                if proposal.structure.as_ref() == Some(&next) {
                    return Err(NovaError::Validation("no structure changes to save".into()));
                }
                proposal.structure = Some(next);
                "edit_structure"
            }
            ReviewAction::Reject { .. } => {
                proposal.status = ProposalStatus::Rejected;
                "reject_proposal"
            }
            ReviewAction::Confirm => unreachable!(),
        };
        proposal.review_revision = proposal
            .review_revision
            .checked_add(1)
            .ok_or_else(|| NovaError::InvalidState("proposal review revision exhausted".into()))?;
        update_encrypted_record(
            &tx,
            &self.key,
            "memory_proposals",
            "proposal_id",
            &proposal.proposal_id,
            &proposal,
        )?;
        let (key_id, key) = {
            let id: String = tx.query_row(
                "SELECT key_id FROM memory_proposals WHERE proposal_id = ?1",
                params![proposal.proposal_id],
                |row| row.get(0),
            )?;
            let key = load_key_by_id(&tx, &self.key, &id)?;
            (id, key)
        };
        let previous_hash = append_event(
            &tx,
            &self.audit_key,
            &key_id,
            &key,
            "ProposalReviewed",
            &proposal.proposal_id,
            &json!({ "proposal": proposal, "reason": reason, "mutation": mutation }),
        )?;
        let receipt_id = new_id("receipt");
        let receipt = MutationReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::MutationReceipt,
                subject: request.subject,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: format!("memory.{mutation}"),
                source_refs: vec![proposal.source_artifact_id.clone()],
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::CommittedLocal,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::CommittedLocal,
                    std::slice::from_ref(&proposal.proposal_id),
                ),
                previous_audit_event_hash: Some(previous_hash),
            },
            proposal_id: Some(proposal.proposal_id),
            memory_id: None,
            mutation: mutation.into(),
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::MutationReceipt,
            ReceiptStatus::CommittedLocal,
            &receipt,
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn correct_memory(&mut self, request: CorrectionRequest) -> Result<CorrectionOutcome> {
        validate_request_id(request.request_id.as_deref())?;
        if request.subject.trim().is_empty()
            || request.memory_id.trim().is_empty()
            || request.summary.trim().is_empty()
            || request.reason.trim().is_empty()
        {
            return Err(NovaError::Validation(
                "subject, memory_id, summary, and correction reason are required".into(),
            ));
        }
        let requested_at = now()?;
        let request_id = request.request_id.unwrap_or_else(|| new_id("req"));
        let tx = self.connection.transaction()?;
        let mut previous_memory: CanonicalMemory = load_encrypted_record(
            &tx,
            &self.key,
            "canonical_memories",
            "memory_id",
            &request.memory_id,
        )?;
        assert_owner_and_active(&previous_memory, &request.subject, "correct")?;

        let changed_at = now()?;
        let new_memory_id = new_id("memory");
        let correction_source_id = new_id("src");
        let correction_content = serde_json::to_string(&json!({
            "operation": "user_correction",
            "corrects_memory_id": &previous_memory.memory_id,
            "reason": &request.reason,
            "title": &request.title,
            "summary": &request.summary,
        }))?;
        let correction_source = SourceArtifact {
            source_id: correction_source_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.subject.clone(),
            source_platform: "nova_local".into(),
            capture_channel: CaptureChannel::QuickCapture,
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content_hash: sha256_hex(&[correction_content.as_bytes()]),
            content: correction_content,
            observed_at: changed_at.clone(),
            created_at: changed_at.clone(),
        };
        let (source_key_id, source_key) = create_data_key(
            &tx,
            &self.key,
            &correction_source.source_id,
            "source_artifact",
        )?;
        insert_source(&tx, &source_key_id, &source_key, &correction_source)?;
        append_event(
            &tx,
            &self.audit_key,
            &source_key_id,
            &source_key,
            "SourceCaptured",
            &correction_source.source_id,
            &correction_source,
        )?;

        let proposal_id = new_id("proposal");
        let corrected_title = request
            .title
            .clone()
            .unwrap_or_else(|| previous_memory.title.clone());
        let correction_proposal = MemoryProposal {
            structure: previous_memory.structure.clone(),
            review_revision: 0,
            proposal_id: proposal_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.subject.clone(),
            source_artifact_id: correction_source.source_id.clone(),
            title: corrected_title.clone(),
            memory_type: previous_memory.memory_type,
            entities: previous_memory.entities.clone(),
            tags: previous_memory.tags.clone(),
            summary: request.summary.clone(),
            decisions: previous_memory.decisions.clone(),
            emotion: previous_memory.emotion.clone(),
            privacy_level: previous_memory.privacy_level,
            permission_scope: previous_memory.permission_scope.clone(),
            purpose_limits: previous_memory.purpose_limits.clone(),
            model_eligibility: previous_memory.model_eligibility.clone(),
            observed_at: changed_at.clone(),
            created_at: changed_at.clone(),
            proposed_origin: AssertionOrigin::UserSelfReport,
            review_tier: mutation_review_tier(previous_memory.privacy_level),
            status: ProposalStatus::Confirmed,
        };
        let (proposal_key_id, proposal_key) = create_data_key(
            &tx,
            &self.key,
            &correction_proposal.proposal_id,
            "memory_proposal",
        )?;
        insert_proposal(&tx, &proposal_key_id, &proposal_key, &correction_proposal)?;
        append_event(
            &tx,
            &self.audit_key,
            &proposal_key_id,
            &proposal_key,
            "MemoryProposed",
            &correction_proposal.proposal_id,
            &correction_proposal,
        )?;

        let mut source_artifact_ids = previous_memory.source_artifact_ids.clone();
        source_artifact_ids.push(correction_source.source_id.clone());
        let corrected_content_hash = sha256_hex(&[
            corrected_title.as_bytes(),
            request.summary.as_bytes(),
            correction_source.source_id.as_bytes(),
        ]);
        let corrected_memory = CanonicalMemory {
            structure: previous_memory.structure.clone(),
            memory_id: new_memory_id.clone(),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.subject.clone(),
            proposal_id,
            title: corrected_title,
            memory_type: previous_memory.memory_type,
            entities: previous_memory.entities.clone(),
            tags: previous_memory.tags.clone(),
            source_platform: "nova_local".into(),
            source_artifact_ids,
            summary: request.summary,
            decisions: previous_memory.decisions.clone(),
            emotion: previous_memory.emotion.clone(),
            privacy_level: previous_memory.privacy_level,
            permission_scope: previous_memory.permission_scope.clone(),
            purpose_limits: previous_memory.purpose_limits.clone(),
            model_eligibility: previous_memory.model_eligibility.clone(),
            observed_at: changed_at.clone(),
            valid_from: changed_at.clone(),
            valid_to: None,
            created_at: changed_at.clone(),
            updated_at: changed_at.clone(),
            deleted_at: None,
            status: MemoryStatus::Active,
            status_reason: None,
            confidence: 1.0,
            assertion_origin: AssertionOrigin::UserSelfReport,
            supersedes: vec![previous_memory.memory_id.clone()],
            superseded_by: Vec::new(),
            conflicts_with: previous_memory.conflicts_with.clone(),
            content_hash: corrected_content_hash,
        };

        previous_memory.status = MemoryStatus::Superseded;
        previous_memory.status_reason = Some(request.reason);
        previous_memory.valid_to = Some(changed_at.clone());
        previous_memory.updated_at = changed_at;
        previous_memory
            .superseded_by
            .push(corrected_memory.memory_id.clone());
        update_encrypted_record(
            &tx,
            &self.key,
            "canonical_memories",
            "memory_id",
            &previous_memory.memory_id,
            &previous_memory,
        )?;
        let (previous_key_id, previous_key) =
            load_key_by_aggregate(&tx, &self.key, &previous_memory.memory_id)?;
        append_event(
            &tx,
            &self.audit_key,
            &previous_key_id,
            &previous_key,
            "MemorySuperseded",
            &previous_memory.memory_id,
            &previous_memory,
        )?;

        let (memory_key_id, memory_key) = create_data_key(
            &tx,
            &self.key,
            &corrected_memory.memory_id,
            "canonical_memory",
        )?;
        insert_memory(&tx, &memory_key_id, &memory_key, &corrected_memory)?;
        let correction_event_hash = append_event(
            &tx,
            &self.audit_key,
            &memory_key_id,
            &memory_key,
            "MemoryCorrected",
            &corrected_memory.memory_id,
            &corrected_memory,
        )?;

        let receipt_id = new_id("receipt");
        let receipt = MutationReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::MutationReceipt,
                subject: request.subject,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: "memory.correct".into(),
                source_refs: corrected_memory.source_artifact_ids.clone(),
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::CommittedLocal,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::CommittedLocal,
                    &[
                        previous_memory.memory_id.clone(),
                        corrected_memory.memory_id.clone(),
                    ],
                ),
                previous_audit_event_hash: Some(correction_event_hash),
            },
            proposal_id: Some(corrected_memory.proposal_id.clone()),
            memory_id: Some(corrected_memory.memory_id.clone()),
            mutation: "correct_and_supersede".into(),
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::MutationReceipt,
            ReceiptStatus::CommittedLocal,
            &receipt,
        )?;
        tx.commit()?;

        Ok(CorrectionOutcome {
            previous_memory,
            memory: corrected_memory,
            source: correction_source,
            receipt,
        })
    }

    pub fn dispute_memory(&mut self, request: DisputeRequest) -> Result<DisputeOutcome> {
        validate_request_id(request.request_id.as_deref())?;
        if request.subject.trim().is_empty()
            || request.memory_id.trim().is_empty()
            || request.reason.trim().is_empty()
        {
            return Err(NovaError::Validation(
                "subject, memory_id, and dispute reason are required".into(),
            ));
        }
        let requested_at = now()?;
        let request_id = request.request_id.unwrap_or_else(|| new_id("req"));
        let tx = self.connection.transaction()?;
        let mut memory: CanonicalMemory = load_encrypted_record(
            &tx,
            &self.key,
            "canonical_memories",
            "memory_id",
            &request.memory_id,
        )?;
        assert_owner_and_active(&memory, &request.subject, "dispute")?;

        let disputed_at = now()?;
        let dispute_content = serde_json::to_string(&json!({
            "operation": "user_dispute",
            "disputes_memory_id": &memory.memory_id,
            "reason": &request.reason,
        }))?;
        let dispute_source = SourceArtifact {
            source_id: new_id("src"),
            schema_version: NMDS_VERSION.into(),
            owner_id: request.subject.clone(),
            source_platform: "nova_local".into(),
            capture_channel: CaptureChannel::QuickCapture,
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content_hash: sha256_hex(&[dispute_content.as_bytes()]),
            content: dispute_content,
            observed_at: disputed_at.clone(),
            created_at: disputed_at.clone(),
        };
        let (source_key_id, source_key) =
            create_data_key(&tx, &self.key, &dispute_source.source_id, "source_artifact")?;
        insert_source(&tx, &source_key_id, &source_key, &dispute_source)?;
        append_event(
            &tx,
            &self.audit_key,
            &source_key_id,
            &source_key,
            "SourceCaptured",
            &dispute_source.source_id,
            &dispute_source,
        )?;

        memory.status = MemoryStatus::Disputed;
        memory.status_reason = Some(request.reason);
        memory.updated_at = disputed_at;
        memory
            .source_artifact_ids
            .push(dispute_source.source_id.clone());
        update_encrypted_record(
            &tx,
            &self.key,
            "canonical_memories",
            "memory_id",
            &memory.memory_id,
            &memory,
        )?;
        let (memory_key_id, memory_key) = load_key_by_aggregate(&tx, &self.key, &memory.memory_id)?;
        let dispute_event_hash = append_event(
            &tx,
            &self.audit_key,
            &memory_key_id,
            &memory_key,
            "MemoryDisputed",
            &memory.memory_id,
            &memory,
        )?;

        let receipt_id = new_id("receipt");
        let receipt = MutationReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::MutationReceipt,
                subject: request.subject,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: "memory.dispute".into(),
                source_refs: memory.source_artifact_ids.clone(),
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::CommittedLocal,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::CommittedLocal,
                    std::slice::from_ref(&memory.memory_id),
                ),
                previous_audit_event_hash: Some(dispute_event_hash),
            },
            proposal_id: Some(memory.proposal_id.clone()),
            memory_id: Some(memory.memory_id.clone()),
            mutation: "dispute".into(),
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::MutationReceipt,
            ReceiptStatus::CommittedLocal,
            &receipt,
        )?;
        tx.commit()?;

        Ok(DisputeOutcome {
            memory,
            source: dispute_source,
            receipt,
        })
    }

    pub fn pending_proposals(&self) -> Result<Vec<MemoryProposal>> {
        let mut proposals: Vec<MemoryProposal> = load_all_encrypted_records(
            &self.connection,
            &self.key,
            "memory_proposals",
            "created_at ASC",
        )?;
        proposals.retain(|proposal| proposal.status == ProposalStatus::Pending);
        Ok(proposals)
    }

    pub fn memories(&self) -> Result<Vec<CanonicalMemory>> {
        load_all_encrypted_records(
            &self.connection,
            &self.key,
            "canonical_memories",
            "created_at ASC",
        )
    }

    pub fn prepare_context(&mut self, request: ContextRequest) -> Result<ContextOutcome> {
        validate_context_request(&request)?;
        let requested_at = now()?;
        let connector_supplied_request_id = request.request_id.is_some();
        let request_id = request.request_id.clone().unwrap_or_else(|| new_id("req"));
        let request_hash = connector_supplied_request_id
            .then(|| request_fingerprint(&self.audit_key, "context_pack.deliver", &request))
            .transpose()?;
        if let Some(request_hash) = &request_hash
            && let Some(outcome) =
                replay_context(&self.connection, &self.key, &request_id, request_hash)?
        {
            return Ok(outcome);
        }
        let mut candidates = Vec::new();
        let mut excluded_count = 0;

        for memory in self.memories()? {
            if memory.owner_id != request.subject
                || memory.status != MemoryStatus::Active
                || !generic::client_scope_allows(
                    &memory.permission_scope,
                    &request.recipient,
                    "personal",
                )
            {
                excluded_count += 1;
                continue;
            }
            if memory.privacy_level == PrivacyLevel::Sealed {
                excluded_count += 1;
                continue;
            }
            if !privacy_at_or_below(memory.privacy_level, request.maximum_privacy_level) {
                excluded_count += 1;
                continue;
            }
            if !request.allowed_memory_types.is_empty()
                && !request.allowed_memory_types.contains(&memory.memory_type)
            {
                excluded_count += 1;
                continue;
            }
            if !memory.purpose_limits.is_empty()
                && !memory
                    .purpose_limits
                    .iter()
                    .any(|purpose| purpose == &request.purpose)
            {
                excluded_count += 1;
                continue;
            }
            let score = relevance_score(&memory, &request.query);
            if !request.query.trim().is_empty() && score == 0 {
                excluded_count += 1;
                continue;
            }
            candidates.push((score, memory));
        }
        candidates.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.observed_at.cmp(&left.observed_at))
        });
        if candidates.len() > request.max_items {
            excluded_count += candidates.len() - request.max_items;
            candidates.truncate(request.max_items);
        }

        let created_at = now()?;
        let expires_at = add_seconds(request.expires_in_seconds)?;
        let pack_id = new_id("context");
        let items = candidates
            .into_iter()
            .map(|(score, memory)| ContextPackItem {
                memory_id: memory.memory_id,
                title: memory.title,
                memory_type: memory.memory_type,
                summary: memory.summary,
                observed_at: memory.observed_at,
                assertion_origin: memory.assertion_origin,
                source_artifact_ids: memory.source_artifact_ids,
                recall_reason: if request.query.trim().is_empty() {
                    "recent_confirmed_memory".into()
                } else {
                    format!("lexical_match_score:{score}")
                },
            })
            .collect::<Vec<_>>();
        let pack = ContextPack {
            context_pack_id: pack_id.clone(),
            schema_version: "0.1.0".into(),
            subject: request.subject.clone(),
            recipient: request.recipient.clone(),
            purpose: request.purpose.clone(),
            query: request.query,
            surface: request.surface,
            created_at,
            expires_at: expires_at.clone(),
            disclosure_class: "minimum_summary_only".into(),
            items,
            excluded_count,
        };

        let tx = self.connection.transaction()?;
        let (pack_key_id, pack_key) =
            create_data_key(&tx, &self.key, &pack.context_pack_id, "context_pack")?;
        for item in &pack.items {
            tx.execute(
                "INSERT INTO context_pack_members (context_pack_id, memory_id) VALUES (?1, ?2)",
                params![pack.context_pack_id, item.memory_id],
            )?;
        }
        let pack_event_hash = append_event(
            &tx,
            &self.audit_key,
            &pack_key_id,
            &pack_key,
            "ContextPackDelivered",
            &pack.context_pack_id,
            &pack,
        )?;

        let receipt_id = new_id("receipt");
        let disclosed_memory_ids = pack
            .items
            .iter()
            .map(|item| item.memory_id.clone())
            .collect::<Vec<_>>();
        let receipt = ContextDeliveryReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::ContextDeliveryReceipt,
                subject: request.subject,
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: request.purpose,
                source_refs: disclosed_memory_ids.clone(),
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::Delivered,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::Delivered,
                    &disclosed_memory_ids,
                ),
                previous_audit_event_hash: Some(pack_event_hash),
            },
            context_pack_id: pack.context_pack_id.clone(),
            recipient: request.recipient,
            surface: request.surface,
            disclosed_memory_ids,
            disclosed_fields: vec![
                "memory_id".into(),
                "title".into(),
                "memory_type".into(),
                "summary".into(),
                "observed_at".into(),
                "assertion_origin".into(),
                "source_artifact_ids".into(),
            ],
            expires_at,
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::ContextDeliveryReceipt,
            ReceiptStatus::Delivered,
            &receipt,
        )?;
        if let Some(request_hash) = request_hash {
            tx.execute(
                "INSERT INTO idempotency_records
                 (request_id, operation, request_hash, primary_id, secondary_id, receipt_id,
                  created_at)
                 VALUES (?1, 'context_pack.deliver', ?2, ?3, NULL, ?4, ?5)",
                params![
                    request_id,
                    request_hash,
                    pack.context_pack_id,
                    receipt.envelope.receipt_id,
                    now()?
                ],
            )?;
        }
        tx.commit()?;

        Ok(ContextOutcome { pack, receipt })
    }

    pub fn delete_memory(&mut self, subject: &str, memory_id: &str) -> Result<DeleteOutcome> {
        if subject.trim().is_empty() {
            return Err(NovaError::Validation("subject is required".into()));
        }
        let requested_at = now()?;
        let request_id = new_id("req");
        let tx = self.connection.transaction()?;
        let memory: CanonicalMemory =
            load_encrypted_record(&tx, &self.key, "canonical_memories", "memory_id", memory_id)?;
        if memory.owner_id != subject {
            return Err(NovaError::PolicyDenied(
                "only the vault owner can delete a memory".into(),
            ));
        }

        let memory_key_id: String = tx.query_row(
            "SELECT key_id FROM canonical_memories WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        let proposal_key_id: Option<String> = tx
            .query_row(
                "SELECT key_id FROM memory_proposals WHERE proposal_id = ?1",
                params![memory.proposal_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut pack_ids = Vec::new();
        {
            let mut statement = tx
                .prepare("SELECT DISTINCT context_pack_id FROM context_pack_members WHERE memory_id = ?1 OR memory_id = ?2")?;
            let rows = statement.query_map(params![memory_id, memory.proposal_id], |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                pack_ids.push(row?);
            }
        }

        tx.execute(
            "DELETE FROM context_pack_members WHERE memory_id = ?1 OR memory_id = ?2",
            params![memory_id, memory.proposal_id],
        )?;
        tx.execute(
            "DELETE FROM canonical_memories WHERE memory_id = ?1",
            params![memory_id],
        )?;
        tx.execute(
            "DELETE FROM memory_proposals WHERE proposal_id = ?1",
            params![memory.proposal_id],
        )?;
        tx.execute(
            "DELETE FROM data_keys WHERE key_id = ?1",
            params![memory_key_id],
        )?;
        if let Some(proposal_key_id) = proposal_key_id {
            tx.execute(
                "DELETE FROM data_keys WHERE key_id = ?1",
                params![proposal_key_id],
            )?;
        }
        for pack_id in &pack_ids {
            tx.execute(
                "DELETE FROM data_keys WHERE aggregate_id = ?1",
                params![pack_id],
            )?;
        }

        let audit_key_id: String = tx.query_row(
            "SELECT key_id FROM data_keys WHERE aggregate_id = ?1",
            params![AUDIT_AGGREGATE_ID],
            |row| row.get(0),
        )?;
        let deletion_event_hash = append_event(
            &tx,
            &self.audit_key,
            &audit_key_id,
            &self.audit_key,
            "MemoryDeleted",
            memory_id,
            &json!({
                "memory_id": memory_id,
                "proposal_id": memory.proposal_id,
                "deleted_at": now()?,
                "content_keys_destroyed": true,
                "invalidated_context_pack_count": pack_ids.len(),
                "source_artifacts_preserved": memory.source_artifact_ids,
            }),
        )?;

        let receipt_id = new_id("receipt");
        let receipt = MutationReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::MutationReceipt,
                subject: subject.into(),
                request_id: request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: "memory.delete".into(),
                source_refs: vec![memory_id.into()],
                requested_at,
                completed_at: now()?,
                status: ReceiptStatus::CommittedLocal,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &request_id,
                    ReceiptStatus::CommittedLocal,
                    &[memory_id.into()],
                ),
                previous_audit_event_hash: Some(deletion_event_hash),
            },
            proposal_id: Some(memory.proposal_id),
            memory_id: Some(memory_id.into()),
            mutation: "delete_with_crypto_erasure".into(),
        };
        insert_receipt_and_event(
            &tx,
            &self.key,
            &self.audit_key,
            &receipt_id,
            ReceiptType::MutationReceipt,
            ReceiptStatus::CommittedLocal,
            &receipt,
        )?;
        let preserved_source_artifact_ids = memory.source_artifact_ids;
        tx.commit()?;
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

        Ok(DeleteOutcome {
            deleted_memory_id: memory_id.into(),
            preserved_source_artifact_ids,
            receipt,
        })
    }

    pub fn verify_audit_chain(&self) -> Result<AuditReport> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, event_id, event_type, aggregate_id, occurred_at, previous_hash, \
                    event_hash, nonce, ciphertext \
             FROM event_log ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Vec<u8>>(7)?,
                row.get::<_, Vec<u8>>(8)?,
            ))
        })?;

        let mut expected_previous: Option<String> = None;
        let mut count = 0_usize;
        for row in rows {
            let (
                sequence,
                event_id,
                event_type,
                aggregate_id,
                occurred_at,
                stored_previous,
                stored_hash,
                nonce,
                ciphertext,
            ) = row?;
            if stored_previous != expected_previous {
                return Err(NovaError::AuditIntegrity {
                    sequence,
                    reason: "previous hash pointer does not match".into(),
                });
            }
            let computed = event_hash(
                &self.audit_key,
                EventHashInput {
                    previous_hash: expected_previous.as_deref(),
                    event_id: &event_id,
                    event_type: &event_type,
                    aggregate_id: &aggregate_id,
                    occurred_at: &occurred_at,
                    nonce: &nonce,
                    ciphertext: &ciphertext,
                },
            );
            if computed != stored_hash {
                return Err(NovaError::AuditIntegrity {
                    sequence,
                    reason: "event hash does not match encrypted payload".into(),
                });
            }
            expected_previous = Some(stored_hash);
            count += 1;
        }
        Ok(AuditReport {
            valid: true,
            event_count: count,
            head_hash: expected_previous,
        })
    }
}

fn check_review_snapshot(
    audit_key: &DataKey,
    proposal: &MemoryProposal,
    expected: &str,
) -> Result<()> {
    if request_fingerprint(audit_key, "owner_review_snapshot", proposal)? != expected {
        return Err(NovaError::InvalidState(
            "proposal changed since review; reload before acting".into(),
        ));
    }
    Ok(())
}

fn validate_capture_request(request: &CaptureRequest) -> Result<()> {
    validate_request_id(request.request_id.as_deref())?;
    if request.owner_id.trim().is_empty() {
        return Err(NovaError::Validation("owner_id is required".into()));
    }
    if request.source_platform.trim().is_empty() {
        return Err(NovaError::Validation("source_platform is required".into()));
    }
    if request.source_content.trim().is_empty() {
        return Err(NovaError::Validation("source content is required".into()));
    }
    if request.title.trim().is_empty() || request.summary.trim().is_empty() {
        return Err(NovaError::Validation(
            "proposal title and summary are required".into(),
        ));
    }
    if request.capture_fidelity == CaptureFidelity::F3RawAudio {
        return Err(NovaError::PolicyDenied(
            "F3 raw audio capture is outside the P0 product scope".into(),
        ));
    }
    match request.proposed_origin {
        AssertionOrigin::AiExtractedProposal
        | AssertionOrigin::ModelInferred
        | AssertionOrigin::UserSelfReport => {}
        _ => {
            return Err(NovaError::Validation(
                "capture may only propose ai extraction, model inference, or user self-report"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn validate_request_id(request_id: Option<&str>) -> Result<()> {
    if let Some(request_id) = request_id
        && (request_id.trim().is_empty() || request_id.len() > 128)
    {
        return Err(NovaError::Validation(
            "request_id must contain 1 to 128 characters".into(),
        ));
    }
    Ok(())
}

fn assert_owner_and_active(memory: &CanonicalMemory, subject: &str, operation: &str) -> Result<()> {
    if memory.owner_id != subject {
        return Err(NovaError::PolicyDenied(format!(
            "only the vault owner can {operation} a memory"
        )));
    }
    if memory.status != MemoryStatus::Active {
        return Err(NovaError::InvalidState(format!(
            "memory {} is {} and cannot undergo {operation}",
            memory.memory_id,
            memory.status.as_str()
        )));
    }
    Ok(())
}

fn mutation_review_tier(privacy_level: PrivacyLevel) -> ReviewTier {
    match privacy_level {
        PrivacyLevel::Sensitive | PrivacyLevel::Sealed => ReviewTier::Restricted,
        PrivacyLevel::Shareable | PrivacyLevel::Private => ReviewTier::Attention,
    }
}

fn privacy_at_or_below(value: PrivacyLevel, ceiling: PrivacyLevel) -> bool {
    fn rank(level: PrivacyLevel) -> u8 {
        match level {
            PrivacyLevel::Shareable => 0,
            PrivacyLevel::Private => 1,
            PrivacyLevel::Sensitive => 2,
            PrivacyLevel::Sealed => 3,
        }
    }

    rank(value) <= rank(ceiling)
}

fn validate_context_request(request: &ContextRequest) -> Result<()> {
    validate_request_id(request.request_id.as_deref())?;
    if request.subject.trim().is_empty()
        || request.recipient.trim().is_empty()
        || request.purpose.trim().is_empty()
    {
        return Err(NovaError::Validation(
            "subject, recipient, and purpose are required".into(),
        ));
    }
    if !request.user_approved {
        return Err(NovaError::PolicyDenied(
            "P0 requires explicit approval before any Context Pack delivery".into(),
        ));
    }
    if !(1..=25).contains(&request.max_items) {
        return Err(NovaError::Validation(
            "max_items must be between 1 and 25".into(),
        ));
    }
    if !(1..=86_400).contains(&request.expires_in_seconds) {
        return Err(NovaError::Validation(
            "expires_in_seconds must be between 1 and 86400".into(),
        ));
    }
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA secure_delete = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

#[cfg(unix)]
fn restrict_vault_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_vault_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE vault_metadata (
            key TEXT PRIMARY KEY,
            value BLOB NOT NULL
        );

        CREATE TABLE data_keys (
            key_id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL UNIQUE,
            kind TEXT NOT NULL,
            wrapped_nonce BLOB NOT NULL,
            wrapped_key BLOB NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE source_artifacts (
            source_id TEXT PRIMARY KEY,
            key_id TEXT NOT NULL REFERENCES data_keys(key_id),
            source_platform TEXT NOT NULL,
            capture_channel TEXT NOT NULL,
            capture_fidelity TEXT NOT NULL,
            created_at TEXT NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );

        CREATE TABLE memory_proposals (
            proposal_id TEXT PRIMARY KEY,
            key_id TEXT NOT NULL REFERENCES data_keys(key_id),
            source_id TEXT NOT NULL REFERENCES source_artifacts(source_id),
            status TEXT NOT NULL,
            review_tier TEXT NOT NULL,
            privacy_level TEXT NOT NULL,
            created_at TEXT NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );

        CREATE TABLE canonical_memories (
            memory_id TEXT PRIMARY KEY,
            key_id TEXT NOT NULL REFERENCES data_keys(key_id),
            proposal_id TEXT NOT NULL UNIQUE,
            memory_type TEXT NOT NULL,
            assertion_origin TEXT NOT NULL,
            privacy_level TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );

        CREATE TABLE receipts (
            receipt_id TEXT PRIMARY KEY,
            key_id TEXT NOT NULL REFERENCES data_keys(key_id),
            receipt_type TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );

        CREATE TABLE event_log (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL UNIQUE,
            event_type TEXT NOT NULL,
            aggregate_id TEXT NOT NULL,
            key_id TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            previous_hash TEXT,
            event_hash TEXT NOT NULL,
            nonce BLOB NOT NULL,
            ciphertext BLOB NOT NULL
        );

        CREATE TABLE context_pack_members (
            context_pack_id TEXT NOT NULL,
            memory_id TEXT NOT NULL,
            PRIMARY KEY (context_pack_id, memory_id)
        );

        CREATE TABLE idempotency_records (
            request_id TEXT PRIMARY KEY,
            operation TEXT NOT NULL,
            request_hash TEXT NOT NULL,
            primary_id TEXT NOT NULL,
            secondary_id TEXT,
            receipt_id TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE INDEX idx_proposals_status ON memory_proposals(status, created_at);
        CREATE INDEX idx_memories_status ON canonical_memories(status, created_at);
        CREATE INDEX idx_events_aggregate ON event_log(aggregate_id, sequence);
        CREATE INDEX idx_context_memory ON context_pack_members(memory_id);",
    )?;
    Ok(())
}

fn migrate_v1_to_v2(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction()?;
    tx.execute_batch(
        "CREATE TABLE idempotency_records (
            request_id TEXT PRIMARY KEY,
            operation TEXT NOT NULL,
            request_hash TEXT NOT NULL,
            primary_id TEXT NOT NULL,
            secondary_id TEXT,
            receipt_id TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    )?;
    tx.execute(
        "UPDATE vault_metadata SET value = ?1 WHERE key = 'schema_version'",
        params![VAULT_SCHEMA_VERSION.as_bytes()],
    )?;
    tx.commit()?;
    Ok(())
}

fn metadata(connection: &Connection, key: &str) -> Result<Option<Vec<u8>>> {
    Ok(connection
        .query_row(
            "SELECT value FROM vault_metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn request_fingerprint<T: Serialize>(
    audit_key: &DataKey,
    operation: &str,
    request: &T,
) -> Result<String> {
    let serialized = serde_json::to_vec(request)?;
    Ok(hmac_sha256_hex(
        audit_key.as_bytes(),
        &[operation.as_bytes(), &serialized],
    ))
}

fn replay_capture(
    connection: &Connection,
    vault_key: &VaultKey,
    request_id: &str,
    request_hash: &str,
) -> Result<Option<CaptureOutcome>> {
    let record: Option<(String, String, String, Option<String>, String)> = connection
        .query_row(
            "SELECT operation, request_hash, primary_id, secondary_id, receipt_id
             FROM idempotency_records WHERE request_id = ?1",
            params![request_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((operation, stored_hash, source_id, proposal_id, receipt_id)) = record else {
        return Ok(None);
    };
    if operation != "capture" || stored_hash != request_hash {
        return Err(NovaError::IdempotencyConflict {
            request_id: request_id.into(),
        });
    }
    let proposal_id = proposal_id.ok_or_else(|| NovaError::IdempotencyReplayUnavailable {
        request_id: request_id.into(),
    })?;
    let replay_unavailable = || NovaError::IdempotencyReplayUnavailable {
        request_id: request_id.into(),
    };
    let source = load_encrypted_record(
        connection,
        vault_key,
        "source_artifacts",
        "source_id",
        &source_id,
    )
    .map_err(|error| match error {
        NovaError::NotFound { .. } => replay_unavailable(),
        other => other,
    })?;
    let proposal = load_encrypted_record(
        connection,
        vault_key,
        "memory_proposals",
        "proposal_id",
        &proposal_id,
    )
    .map_err(|error| match error {
        NovaError::NotFound { .. } => replay_unavailable(),
        other => other,
    })?;
    let receipt =
        load_encrypted_record(connection, vault_key, "receipts", "receipt_id", &receipt_id)
            .map_err(|error| match error {
                NovaError::NotFound { .. } => replay_unavailable(),
                other => other,
            })?;
    Ok(Some(CaptureOutcome {
        source,
        proposal,
        receipt,
    }))
}

fn replay_context(
    connection: &Connection,
    vault_key: &VaultKey,
    request_id: &str,
    request_hash: &str,
) -> Result<Option<ContextOutcome>> {
    let record: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT operation, request_hash, primary_id, receipt_id
             FROM idempotency_records WHERE request_id = ?1",
            params![request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((operation, stored_hash, context_pack_id, receipt_id)) = record else {
        return Ok(None);
    };
    if operation != "context_pack.deliver" || stored_hash != request_hash {
        return Err(NovaError::IdempotencyConflict {
            request_id: request_id.into(),
        });
    }
    let replay_unavailable = || NovaError::IdempotencyReplayUnavailable {
        request_id: request_id.into(),
    };
    let pack: ContextPack = load_event_payload(
        connection,
        vault_key,
        "ContextPackDelivered",
        &context_pack_id,
    )
    .map_err(|error| match error {
        NovaError::NotFound { .. } => replay_unavailable(),
        other => other,
    })?;
    if context_pack_expired(&pack, OffsetDateTime::now_utc())? {
        return Err(replay_unavailable());
    }
    let receipt =
        load_encrypted_record(connection, vault_key, "receipts", "receipt_id", &receipt_id)
            .map_err(|error| match error {
                NovaError::NotFound { .. } => replay_unavailable(),
                other => other,
            })?;
    Ok(Some(ContextOutcome { pack, receipt }))
}

fn context_pack_expired(pack: &ContextPack, at: OffsetDateTime) -> Result<bool> {
    let expires_at = OffsetDateTime::parse(&pack.expires_at, &Rfc3339)
        .map_err(|error| NovaError::Time(error.to_string()))?;
    Ok(expires_at <= at)
}

fn load_event_payload<T: DeserializeOwned>(
    connection: &Connection,
    vault_key: &VaultKey,
    event_type: &'static str,
    aggregate_id: &str,
) -> Result<T> {
    let row: Option<(String, String, Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT event_id, key_id, nonce, ciphertext
             FROM event_log
             WHERE event_type = ?1 AND aggregate_id = ?2
             ORDER BY sequence DESC LIMIT 1",
            params![event_type, aggregate_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (event_id, key_id, nonce, ciphertext) = row.ok_or_else(|| NovaError::NotFound {
        kind: "event payload",
        id: aggregate_id.into(),
    })?;
    let key = load_key_by_id(connection, vault_key, &key_id)?;
    let aad = format!("event:{event_id}:{event_type}:{aggregate_id}:v1");
    let payload = key.decrypt(&EncryptedBlob { nonce, ciphertext }, aad.as_bytes())?;
    Ok(serde_json::from_slice(&payload)?)
}

fn create_data_key(
    tx: &Transaction<'_>,
    vault_key: &VaultKey,
    aggregate_id: &str,
    kind: &str,
) -> Result<(String, DataKey)> {
    let key_id = new_id("dek");
    let data_key = create_data_key_with_id(tx, vault_key, &key_id, aggregate_id, kind)?;
    Ok((key_id, data_key))
}

fn create_data_key_with_id(
    tx: &Transaction<'_>,
    vault_key: &VaultKey,
    key_id: &str,
    aggregate_id: &str,
    kind: &str,
) -> Result<DataKey> {
    let data_key = DataKey::generate()?;
    let aad = format!("wrapped-key:{key_id}:{aggregate_id}:{kind}:v1");
    let wrapped = vault_key.encrypt(data_key.as_bytes(), aad.as_bytes())?;
    tx.execute(
        "INSERT INTO data_keys
         (key_id, aggregate_id, kind, wrapped_nonce, wrapped_key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            key_id,
            aggregate_id,
            kind,
            wrapped.nonce,
            wrapped.ciphertext,
            now()?
        ],
    )?;
    Ok(data_key)
}

fn load_key_by_id(connection: &Connection, vault_key: &VaultKey, key_id: &str) -> Result<DataKey> {
    let (aggregate_id, kind, nonce, ciphertext): (String, String, Vec<u8>, Vec<u8>) = connection
        .query_row(
            "SELECT aggregate_id, kind, wrapped_nonce, wrapped_key FROM data_keys WHERE key_id = ?1",
            params![key_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| NovaError::NotFound {
            kind: "data key",
            id: key_id.into(),
        })?;
    let aad = format!("wrapped-key:{key_id}:{aggregate_id}:{kind}:v1");
    let bytes = vault_key.decrypt(&EncryptedBlob { nonce, ciphertext }, aad.as_bytes())?;
    DataKey::from_bytes(bytes)
}

fn load_key_by_aggregate(
    connection: &Connection,
    vault_key: &VaultKey,
    aggregate_id: &str,
) -> Result<(String, DataKey)> {
    let key_id: String = connection
        .query_row(
            "SELECT key_id FROM data_keys WHERE aggregate_id = ?1",
            params![aggregate_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| NovaError::NotFound {
            kind: "aggregate key",
            id: aggregate_id.into(),
        })?;
    let key = load_key_by_id(connection, vault_key, &key_id)?;
    Ok((key_id, key))
}

fn encrypt_json<T: Serialize>(
    key: &DataKey,
    table: &str,
    record_id: &str,
    value: &T,
) -> Result<EncryptedBlob> {
    let payload = serde_json::to_vec(value)?;
    let aad = format!("row:{table}:{record_id}:v1");
    key.encrypt(&payload, aad.as_bytes())
}

fn decrypt_json<T: DeserializeOwned>(
    key: &DataKey,
    table: &str,
    record_id: &str,
    blob: EncryptedBlob,
) -> Result<T> {
    let aad = format!("row:{table}:{record_id}:v1");
    let payload = key.decrypt(&blob, aad.as_bytes())?;
    Ok(serde_json::from_slice(&payload)?)
}

fn insert_source(
    tx: &Transaction<'_>,
    key_id: &str,
    key: &DataKey,
    source: &SourceArtifact,
) -> Result<()> {
    let encrypted = encrypt_json(key, "source_artifacts", &source.source_id, source)?;
    tx.execute(
        "INSERT INTO source_artifacts
         (source_id, key_id, source_platform, capture_channel, capture_fidelity, created_at,
          nonce, ciphertext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            source.source_id,
            key_id,
            source.source_platform,
            source.capture_channel.as_str(),
            source.capture_fidelity.as_str(),
            source.created_at,
            encrypted.nonce,
            encrypted.ciphertext
        ],
    )?;
    Ok(())
}

fn insert_proposal(
    tx: &Transaction<'_>,
    key_id: &str,
    key: &DataKey,
    proposal: &MemoryProposal,
) -> Result<()> {
    let encrypted = encrypt_json(key, "memory_proposals", &proposal.proposal_id, proposal)?;
    tx.execute(
        "INSERT INTO memory_proposals
         (proposal_id, key_id, source_id, status, review_tier, privacy_level, created_at,
          nonce, ciphertext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            proposal.proposal_id,
            key_id,
            proposal.source_artifact_id,
            proposal.status.as_str(),
            proposal.review_tier.as_str(),
            proposal.privacy_level.as_str(),
            proposal.created_at,
            encrypted.nonce,
            encrypted.ciphertext
        ],
    )?;
    Ok(())
}

fn insert_memory(
    tx: &Transaction<'_>,
    key_id: &str,
    key: &DataKey,
    memory: &CanonicalMemory,
) -> Result<()> {
    let encrypted = encrypt_json(key, "canonical_memories", &memory.memory_id, memory)?;
    tx.execute(
        "INSERT INTO canonical_memories
         (memory_id, key_id, proposal_id, memory_type, assertion_origin, privacy_level, status,
          created_at, nonce, ciphertext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            memory.memory_id,
            key_id,
            memory.proposal_id,
            memory.memory_type.as_str(),
            memory.assertion_origin.as_str(),
            memory.privacy_level.as_str(),
            memory.status.as_str(),
            memory.created_at,
            encrypted.nonce,
            encrypted.ciphertext
        ],
    )?;
    Ok(())
}

fn update_encrypted_record<T: Serialize>(
    tx: &Transaction<'_>,
    vault_key: &VaultKey,
    table: &str,
    id_column: &str,
    record_id: &str,
    value: &T,
) -> Result<()> {
    let sql = format!("SELECT key_id FROM {table} WHERE {id_column} = ?1");
    let key_id: String = tx.query_row(&sql, params![record_id], |row| row.get(0))?;
    let key = load_key_by_id(tx, vault_key, &key_id)?;
    let encrypted = encrypt_json(&key, table, record_id, value)?;
    let sql = format!(
        "UPDATE {table} SET nonce = ?1, ciphertext = ?2, status = ?3 WHERE {id_column} = ?4"
    );
    let status = serde_json::to_value(value)?["status"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    tx.execute(
        &sql,
        params![encrypted.nonce, encrypted.ciphertext, status, record_id],
    )?;
    Ok(())
}

fn load_encrypted_record<T: DeserializeOwned>(
    connection: &Connection,
    vault_key: &VaultKey,
    table: &'static str,
    id_column: &str,
    record_id: &str,
) -> Result<T> {
    let sql = format!("SELECT key_id, nonce, ciphertext FROM {table} WHERE {id_column} = ?1");
    let row: Option<(String, Vec<u8>, Vec<u8>)> = connection
        .query_row(&sql, params![record_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let (key_id, nonce, ciphertext) = row.ok_or_else(|| NovaError::NotFound {
        kind: table,
        id: record_id.into(),
    })?;
    let key = load_key_by_id(connection, vault_key, &key_id)?;
    decrypt_json(&key, table, record_id, EncryptedBlob { nonce, ciphertext })
}

fn load_all_encrypted_records<T: DeserializeOwned>(
    connection: &Connection,
    vault_key: &VaultKey,
    table: &'static str,
    order_by: &str,
) -> Result<Vec<T>> {
    let id_column = match table {
        "memory_proposals" => "proposal_id",
        "canonical_memories" => "memory_id",
        "source_artifacts" => "source_id",
        "receipts" => "receipt_id",
        _ => return Err(NovaError::Validation(format!("unknown table: {table}"))),
    };
    let sql =
        format!("SELECT {id_column}, key_id, nonce, ciphertext FROM {table} ORDER BY {order_by}");
    let encrypted_rows = {
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        collected
    };
    encrypted_rows
        .into_iter()
        .map(|(record_id, key_id, nonce, ciphertext)| {
            let key = load_key_by_id(connection, vault_key, &key_id)?;
            decrypt_json(&key, table, &record_id, EncryptedBlob { nonce, ciphertext })
        })
        .collect()
}

fn insert_receipt_and_event<T: Serialize>(
    tx: &Transaction<'_>,
    vault_key: &VaultKey,
    audit_key: &DataKey,
    receipt_id: &str,
    receipt_type: ReceiptType,
    status: ReceiptStatus,
    receipt: &T,
) -> Result<()> {
    let (key_id, key) = create_data_key(tx, vault_key, receipt_id, "receipt")?;
    let encrypted = encrypt_json(&key, "receipts", receipt_id, receipt)?;
    tx.execute(
        "INSERT INTO receipts
         (receipt_id, key_id, receipt_type, status, created_at, nonce, ciphertext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            receipt_id,
            key_id,
            receipt_type.as_str(),
            status.as_str(),
            now()?,
            encrypted.nonce,
            encrypted.ciphertext
        ],
    )?;
    let event_type = match receipt_type {
        ReceiptType::CaptureReceipt => "CaptureReceiptRecorded",
        ReceiptType::MutationReceipt => "MutationReceiptRecorded",
        ReceiptType::ContextDeliveryReceipt => "ContextDeliveryReceiptRecorded",
    };
    append_event(
        tx, audit_key, &key_id, &key, event_type, receipt_id, receipt,
    )?;
    Ok(())
}

fn append_event<T: Serialize>(
    tx: &Transaction<'_>,
    audit_key: &DataKey,
    key_id: &str,
    key: &DataKey,
    event_type: &str,
    aggregate_id: &str,
    payload: &T,
) -> Result<String> {
    let event_id = new_id("event");
    let occurred_at = now()?;
    let aad = format!("event:{event_id}:{event_type}:{aggregate_id}:v1");
    let encrypted = key.encrypt(&serde_json::to_vec(payload)?, aad.as_bytes())?;
    let previous_hash: Option<String> = tx
        .query_row(
            "SELECT event_hash FROM event_log ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let hash = event_hash(
        audit_key,
        EventHashInput {
            previous_hash: previous_hash.as_deref(),
            event_id: &event_id,
            event_type,
            aggregate_id,
            occurred_at: &occurred_at,
            nonce: &encrypted.nonce,
            ciphertext: &encrypted.ciphertext,
        },
    );
    tx.execute(
        "INSERT INTO event_log
         (event_id, event_type, aggregate_id, key_id, occurred_at, previous_hash, event_hash,
          nonce, ciphertext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event_id,
            event_type,
            aggregate_id,
            key_id,
            occurred_at,
            previous_hash,
            hash,
            encrypted.nonce,
            encrypted.ciphertext
        ],
    )?;
    Ok(hash)
}

struct EventHashInput<'a> {
    previous_hash: Option<&'a str>,
    event_id: &'a str,
    event_type: &'a str,
    aggregate_id: &'a str,
    occurred_at: &'a str,
    nonce: &'a [u8],
    ciphertext: &'a [u8],
}

fn event_hash(audit_key: &DataKey, input: EventHashInput<'_>) -> String {
    hmac_sha256_hex(
        audit_key.as_bytes(),
        &[
            input.previous_hash.unwrap_or("GENESIS").as_bytes(),
            input.event_id.as_bytes(),
            input.event_type.as_bytes(),
            input.aggregate_id.as_bytes(),
            input.occurred_at.as_bytes(),
            input.nonce,
            input.ciphertext,
        ],
    )
}

fn receipt_hash(
    audit_key: &DataKey,
    receipt_id: &str,
    request_id: &str,
    status: ReceiptStatus,
    refs: &[String],
) -> String {
    let joined = refs.join("\u{1f}");
    hmac_sha256_hex(
        audit_key.as_bytes(),
        &[
            receipt_id.as_bytes(),
            request_id.as_bytes(),
            status.as_str().as_bytes(),
            joined.as_bytes(),
        ],
    )
}

fn relevance_score(memory: &CanonicalMemory, query: &str) -> usize {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 1;
    }
    let haystack = format!(
        "{} {} {}",
        memory.title,
        memory.summary,
        memory.tags.join(" ")
    )
    .to_lowercase();
    let mut score = usize::from(haystack.contains(&query)) * 5;
    for token in query
        .split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
        .filter(|token| !token.is_empty())
    {
        score += haystack.matches(token).count();
    }
    score
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7())
}

fn now() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| NovaError::Time(error.to_string()))
}

fn add_seconds(seconds: i64) -> Result<String> {
    (OffsetDateTime::now_utc() + Duration::seconds(seconds))
        .format(&Rfc3339)
        .map_err(|error| NovaError::Time(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct TestVault {
        _directory: TempDir,
        path: std::path::PathBuf,
        vault: Vault,
    }

    fn test_vault() -> TestVault {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("vault.sqlite");
        let key = VaultKey::generate().expect("test key");
        let vault = Vault::create(&path, key).expect("vault");
        TestVault {
            _directory: directory,
            path,
            vault,
        }
    }

    fn capture_request(origin: AssertionOrigin, privacy: PrivacyLevel) -> CaptureRequest {
        CaptureRequest {
            structure: None,
            request_id: Some("req_test_capture".into()),
            owner_id: "owner:founder".into(),
            source_platform: "chatgpt".into(),
            capture_channel: CaptureChannel::RemoteMcp,
            capture_fidelity: CaptureFidelity::F1AiDigest,
            source_content: "今天决定先验证 ChatGPT 单平台可信记忆闭环。".into(),
            title: "Nova P0 平台范围".into(),
            memory_type: MemoryType::Decision,
            summary: "P0 只做 ChatGPT，不扩展第二个平台。".into(),
            decisions: vec!["P0 只做 ChatGPT".into()],
            privacy_level: privacy,
            proposed_origin: origin,
            review_tier: ReviewTier::Routine,
            permission_scope: vec!["personal_review".into()],
            purpose_limits: vec!["nova_project_continuity".into()],
            observed_at: Some("2026-08-14T08:00:00Z".into()),
        }
    }

    fn review_request(vault: &Vault, action: ReviewAction) -> ReviewRequest {
        let item = vault.review_inbox("owner:founder").unwrap().remove(0);
        ReviewRequest {
            request_id: Some("review_test".into()),
            subject: "owner:founder".into(),
            proposal_id: item.proposal.proposal_id,
            expected_snapshot: item.snapshot,
            action,
        }
    }

    #[test]
    fn review_edit_preserves_source_and_inference_then_requires_fresh_confirmation() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::ModelInferred,
                PrivacyLevel::Private,
            ))
            .unwrap();
        let initial = test.vault.review_inbox("owner:founder").unwrap().remove(0);
        assert_eq!(
            initial.capture_receipt.unwrap().envelope.receipt_id,
            captured.receipt.envelope.receipt_id
        );
        let stale_confirm = review_request(&test.vault, ReviewAction::Confirm);
        let edit = review_request(
            &test.vault,
            ReviewAction::Edit {
                title: "用户编辑标题".into(),
                summary: "更精确的模型推断，仍需核实。".into(),
                decisions: vec![],
                reason: "原提炼不准确".into(),
            },
        );
        let receipt = test.vault.review_proposal(edit.clone()).unwrap();
        assert_eq!(receipt.mutation, "edit_proposal");
        assert!(receipt.memory_id.is_none());
        assert!(test.vault.memories().unwrap().is_empty());
        let revised = test.vault.review_inbox("owner:founder").unwrap().remove(0);
        assert_eq!(revised.source, initial.source);
        assert_eq!(
            revised.proposal.proposed_origin,
            AssertionOrigin::ModelInferred
        );
        assert_ne!(initial.snapshot, revised.snapshot);
        let events = test.vault.verify_audit_chain().unwrap().event_count;
        assert!(test.vault.review_proposal(stale_confirm).is_err());
        assert!(test.vault.review_proposal(edit).is_err());
        assert_eq!(test.vault.verify_audit_chain().unwrap().event_count, events);
        let confirm = review_request(&test.vault, ReviewAction::Confirm);
        let receipt = test.vault.review_proposal(confirm.clone()).unwrap();
        assert_eq!(receipt.mutation, "confirm");
        assert!(test.vault.review_proposal(confirm).is_err());
        let memories = test.vault.memories().unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].summary, revised.proposal.summary);
        assert_eq!(
            memories[0].assertion_origin,
            AssertionOrigin::UserConfirmedInference
        );
    }

    #[test]
    fn restored_proposal_text_does_not_restore_a_stale_review_snapshot() {
        let mut test = test_vault();
        test.vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .unwrap();
        let original = test.vault.review_inbox("owner:founder").unwrap().remove(0);
        let stale = review_request(&test.vault, ReviewAction::Confirm);
        let edit = review_request(
            &test.vault,
            ReviewAction::Edit {
                title: "temporary edit".into(),
                summary: "edited".into(),
                decisions: vec![],
                reason: "test".into(),
            },
        );
        test.vault.review_proposal(edit).unwrap();
        let restore = review_request(
            &test.vault,
            ReviewAction::Edit {
                title: original.proposal.title,
                summary: original.proposal.summary,
                decisions: original.proposal.decisions,
                reason: "restore text".into(),
            },
        );
        test.vault.review_proposal(restore).unwrap();
        assert!(test.vault.review_proposal(stale).is_err());
        assert!(test.vault.memories().unwrap().is_empty());
    }

    #[test]
    fn review_reject_removes_only_pending_candidate_and_never_confirms() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .unwrap();
        let confirm = review_request(&test.vault, ReviewAction::Confirm);
        let reject = review_request(
            &test.vault,
            ReviewAction::Reject {
                reason: "未作出这个决定".into(),
            },
        );
        let receipt = test.vault.review_proposal(reject.clone()).unwrap();
        assert_eq!(receipt.mutation, "reject_proposal");
        assert!(test.vault.review_inbox("owner:founder").unwrap().is_empty());
        assert!(test.vault.memories().unwrap().is_empty());
        assert!(test.vault.review_proposal(confirm).is_err());
        assert!(test.vault.review_proposal(reject).is_err());
        let replay = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .unwrap();
        assert_eq!(replay.source, captured.source);
        assert_eq!(replay.proposal.status, ProposalStatus::Rejected);
        assert!(test.vault.verify_audit_chain().unwrap().valid);
    }

    #[test]
    fn review_owner_and_input_guards_leave_audit_unchanged() {
        let mut test = test_vault();
        test.vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .unwrap();
        let before = test.vault.verify_audit_chain().unwrap();
        assert!(test.vault.review_inbox("other-owner").unwrap().is_empty());
        for action in [
            ReviewAction::Confirm,
            ReviewAction::Reject {
                reason: "拒绝".into(),
            },
            ReviewAction::Edit {
                title: "修改".into(),
                summary: "修改摘要".into(),
                decisions: vec![],
                reason: "修正".into(),
            },
        ] {
            let mut request = review_request(&test.vault, action);
            request.subject = "other-owner".into();
            assert!(matches!(
                test.vault.review_proposal(request),
                Err(NovaError::PolicyDenied(_))
            ));
        }
        let empty = review_request(&test.vault, ReviewAction::Reject { reason: " ".into() });
        assert!(test.vault.review_proposal(empty).is_err());
        assert_eq!(before, test.vault.verify_audit_chain().unwrap());
    }

    #[test]
    fn capture_creates_a_proposal_but_not_canonical_memory() {
        let mut test = test_vault();
        let outcome = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");

        assert_eq!(outcome.proposal.status, ProposalStatus::Pending);
        assert_eq!(
            outcome.receipt.envelope.status,
            ReceiptStatus::ProposalCreated
        );
        assert!(test.vault.memories().expect("memories").is_empty());
        assert_eq!(test.vault.pending_proposals().expect("proposals").len(), 1);
    }

    #[test]
    fn capture_idempotency_replays_the_original_objects_and_receipt() {
        let mut test = test_vault();
        let request = capture_request(AssertionOrigin::AiExtractedProposal, PrivacyLevel::Private);
        let first = test.vault.capture(request.clone()).expect("first capture");
        let replay = test.vault.capture(request).expect("idempotent replay");

        assert_eq!(first.source.source_id, replay.source.source_id);
        assert_eq!(first.proposal.proposal_id, replay.proposal.proposal_id);
        assert_eq!(
            first.receipt.envelope.receipt_id,
            replay.receipt.envelope.receipt_id
        );
        assert_eq!(test.vault.pending_proposals().expect("proposals").len(), 1);
        assert_eq!(
            test.vault.verify_audit_chain().expect("audit").event_count,
            3
        );
    }

    #[test]
    fn capture_without_connector_request_id_is_a_new_logical_request() {
        let mut test = test_vault();
        let mut request =
            capture_request(AssertionOrigin::AiExtractedProposal, PrivacyLevel::Private);
        request.request_id = None;
        let first = test.vault.capture(request.clone()).expect("first capture");
        let second = test.vault.capture(request).expect("second capture");
        assert_ne!(first.source.source_id, second.source.source_id);
        assert_ne!(
            first.receipt.envelope.request_id,
            second.receipt.envelope.request_id
        );
    }

    #[test]
    fn idempotency_key_reuse_with_different_input_is_rejected() {
        let mut test = test_vault();
        let request = capture_request(AssertionOrigin::AiExtractedProposal, PrivacyLevel::Private);
        test.vault.capture(request.clone()).expect("first capture");
        let mut changed = request;
        changed.summary = "同一个请求标识下的不同内容".into();

        let error = test.vault.capture(changed).expect_err("conflict");
        assert!(matches!(error, NovaError::IdempotencyConflict { .. }));
        assert_eq!(error.code(), "NOVA_IDEMPOTENCY_CONFLICT");
    }

    #[test]
    fn explicit_confirmation_is_the_only_path_to_canonical_memory() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: Some("req_test_confirm".into()),
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");

        assert_eq!(
            confirmed.memory.assertion_origin,
            AssertionOrigin::UserConfirmedAiExtraction
        );
        assert_eq!(confirmed.memory.source_artifact_ids.len(), 1);
        assert!(
            test.vault
                .pending_proposals()
                .expect("proposals")
                .is_empty()
        );
        assert_eq!(test.vault.memories().expect("memories").len(), 1);
    }

    #[test]
    fn confirmed_inference_never_becomes_source_truth() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::ModelInferred,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");

        assert_eq!(
            confirmed.memory.assertion_origin,
            AssertionOrigin::UserConfirmedInference
        );
        assert!(confirmed.memory.confidence < 1.0);
    }

    #[test]
    fn correction_creates_a_new_version_and_supersedes_without_overwrite() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");
        let corrected = test
            .vault
            .correct_memory(CorrectionRequest {
                request_id: Some("req_test_correction".into()),
                subject: "owner:founder".into(),
                memory_id: confirmed.memory.memory_id.clone(),
                title: None,
                summary: "修正后：P0 先做 ChatGPT，但必须保留多路径降级。".into(),
                reason: "原摘要遗漏了降级策略".into(),
            })
            .expect("correct");

        assert_eq!(corrected.previous_memory.status, MemoryStatus::Superseded);
        assert_eq!(corrected.memory.status, MemoryStatus::Active);
        assert_eq!(
            corrected.memory.assertion_origin,
            AssertionOrigin::UserSelfReport
        );
        assert_eq!(
            corrected.memory.supersedes,
            vec![confirmed.memory.memory_id]
        );
        assert_eq!(corrected.memory.source_artifact_ids.len(), 2);
        assert_eq!(test.vault.memories().expect("history").len(), 2);

        let context = test
            .vault
            .prepare_context(ContextRequest {
                request_id: None,
                subject: "owner:founder".into(),
                recipient: "chatgpt".into(),
                purpose: "nova_project_continuity".into(),
                query: "多路径降级".into(),
                surface: ContextSurface::DesktopReview,
                allowed_memory_types: vec![MemoryType::Decision],
                maximum_privacy_level: PrivacyLevel::Private,
                max_items: 5,
                expires_in_seconds: 600,
                user_approved: true,
            })
            .expect("context");
        assert_eq!(context.pack.items.len(), 1);
        assert_eq!(context.pack.items[0].memory_id, corrected.memory.memory_id);
    }

    #[test]
    fn dispute_excludes_memory_but_preserves_original_epistemic_origin() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::ModelInferred,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");
        let disputed = test
            .vault
            .dispute_memory(DisputeRequest {
                request_id: Some("req_test_dispute".into()),
                subject: "owner:founder".into(),
                memory_id: confirmed.memory.memory_id,
                reason: "这一推断不准确，需要停止复用".into(),
            })
            .expect("dispute");

        assert_eq!(disputed.memory.status, MemoryStatus::Disputed);
        assert_eq!(
            disputed.memory.assertion_origin,
            AssertionOrigin::UserConfirmedInference
        );
        let context = test
            .vault
            .prepare_context(ContextRequest {
                request_id: None,
                subject: "owner:founder".into(),
                recipient: "chatgpt".into(),
                purpose: "nova_project_continuity".into(),
                query: "".into(),
                surface: ContextSurface::ChatText,
                allowed_memory_types: Vec::new(),
                maximum_privacy_level: PrivacyLevel::Private,
                max_items: 5,
                expires_in_seconds: 600,
                user_approved: true,
            })
            .expect("context");
        assert!(context.pack.items.is_empty());
    }

    #[test]
    fn sensitive_capture_is_forced_into_restricted_review() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Sensitive,
            ))
            .expect("capture");
        assert_eq!(captured.proposal.review_tier, ReviewTier::Restricted);
    }

    #[test]
    fn context_delivery_requires_approval_and_honors_purpose() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        test.vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");

        let denied = test.vault.prepare_context(ContextRequest {
            request_id: None,
            subject: "owner:founder".into(),
            recipient: "chatgpt".into(),
            purpose: "nova_project_continuity".into(),
            query: "ChatGPT".into(),
            surface: ContextSurface::ChatText,
            allowed_memory_types: Vec::new(),
            maximum_privacy_level: PrivacyLevel::Private,
            max_items: 5,
            expires_in_seconds: 600,
            user_approved: false,
        });
        assert!(matches!(denied, Err(NovaError::PolicyDenied(_))));

        let delivered = test
            .vault
            .prepare_context(ContextRequest {
                request_id: None,
                subject: "owner:founder".into(),
                recipient: "chatgpt".into(),
                purpose: "nova_project_continuity".into(),
                query: "ChatGPT".into(),
                surface: ContextSurface::ChatText,
                allowed_memory_types: Vec::new(),
                maximum_privacy_level: PrivacyLevel::Private,
                max_items: 5,
                expires_in_seconds: 600,
                user_approved: true,
            })
            .expect("context");
        assert_eq!(delivered.pack.items.len(), 1);
        assert_eq!(delivered.receipt.envelope.status, ReceiptStatus::Delivered);

        let wrong_purpose = test
            .vault
            .prepare_context(ContextRequest {
                request_id: None,
                subject: "owner:founder".into(),
                recipient: "chatgpt".into(),
                purpose: "unrelated_purpose".into(),
                query: "".into(),
                surface: ContextSurface::ChatText,
                allowed_memory_types: Vec::new(),
                maximum_privacy_level: PrivacyLevel::Private,
                max_items: 5,
                expires_in_seconds: 600,
                user_approved: true,
            })
            .expect("empty context");
        assert!(wrong_purpose.pack.items.is_empty());
    }

    #[test]
    fn context_delivery_retries_replay_the_same_pack_and_receipt() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        test.vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");
        let request = ContextRequest {
            request_id: Some("req_context_replay".into()),
            subject: "owner:founder".into(),
            recipient: "chatgpt".into(),
            purpose: "nova_project_continuity".into(),
            query: "ChatGPT".into(),
            surface: ContextSurface::VoiceAudio,
            allowed_memory_types: vec![MemoryType::Decision],
            maximum_privacy_level: PrivacyLevel::Private,
            max_items: 5,
            expires_in_seconds: 600,
            user_approved: true,
        };
        let first = test
            .vault
            .prepare_context(request.clone())
            .expect("context");
        let event_count = test.vault.verify_audit_chain().expect("audit").event_count;
        let replay = test.vault.prepare_context(request).expect("replay");

        assert_eq!(first.pack.context_pack_id, replay.pack.context_pack_id);
        assert_eq!(
            first.receipt.envelope.receipt_id,
            replay.receipt.envelope.receipt_id
        );
        assert_eq!(
            test.vault.verify_audit_chain().expect("audit").event_count,
            event_count
        );
    }

    #[test]
    fn context_request_id_reuse_with_a_different_query_is_rejected() {
        let mut test = test_vault();
        let request = ContextRequest {
            request_id: Some("req_context_conflict".into()),
            subject: "owner:founder".into(),
            recipient: "chatgpt".into(),
            purpose: "nova_project_continuity".into(),
            query: "first".into(),
            surface: ContextSurface::ChatText,
            allowed_memory_types: Vec::new(),
            maximum_privacy_level: PrivacyLevel::Private,
            max_items: 5,
            expires_in_seconds: 600,
            user_approved: true,
        };
        test.vault
            .prepare_context(request.clone())
            .expect("first empty context");
        let mut changed = request;
        changed.query = "changed".into();
        assert!(matches!(
            test.vault.prepare_context(changed),
            Err(NovaError::IdempotencyConflict { .. })
        ));
    }

    #[test]
    fn expired_context_pack_is_not_replay_eligible() {
        let pack = ContextPack {
            context_pack_id: "context_expired".into(),
            schema_version: "0.1.0".into(),
            subject: "owner:founder".into(),
            recipient: "chatgpt".into(),
            purpose: "test".into(),
            query: "".into(),
            surface: ContextSurface::ChatText,
            created_at: "2026-08-14T00:00:00Z".into(),
            expires_at: "2026-08-14T00:10:00Z".into(),
            disclosure_class: "minimum_summary_only".into(),
            items: Vec::new(),
            excluded_count: 0,
        };
        let later =
            OffsetDateTime::parse("2026-08-14T00:11:00Z", &Rfc3339).expect("test timestamp");
        assert!(context_pack_expired(&pack, later).expect("expiry"));
    }

    #[test]
    fn deletion_destroys_memory_and_invalidates_prior_context() {
        let mut test = test_vault();
        let captured = test
            .vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");
        let context_request = ContextRequest {
            request_id: Some("req_context_before_delete".into()),
            subject: "owner:founder".into(),
            recipient: "chatgpt".into(),
            purpose: "nova_project_continuity".into(),
            query: "ChatGPT".into(),
            surface: ContextSurface::ChatText,
            allowed_memory_types: Vec::new(),
            maximum_privacy_level: PrivacyLevel::Private,
            max_items: 5,
            expires_in_seconds: 600,
            user_approved: true,
        };
        test.vault
            .prepare_context(context_request.clone())
            .expect("context");

        let deleted = test
            .vault
            .delete_memory("owner:founder", &confirmed.memory.memory_id)
            .expect("delete");
        assert!(test.vault.memories().expect("memories").is_empty());
        assert_eq!(deleted.preserved_source_artifact_ids.len(), 1);
        assert_eq!(deleted.receipt.mutation, "delete_with_crypto_erasure");
        assert!(matches!(
            test.vault.prepare_context(context_request),
            Err(NovaError::IdempotencyReplayUnavailable { .. })
        ));
        assert!(test.vault.verify_audit_chain().expect("audit").valid);
    }

    #[test]
    fn audit_hash_chain_detects_consistent_history() {
        let mut test = test_vault();
        test.vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        let report = test.vault.verify_audit_chain().expect("audit");
        assert!(report.valid);
        assert_eq!(report.event_count, 3);
        assert!(report.head_hash.is_some());
    }

    #[test]
    fn raw_audio_is_rejected_in_p0() {
        let mut test = test_vault();
        let mut request =
            capture_request(AssertionOrigin::AiExtractedProposal, PrivacyLevel::Private);
        request.capture_fidelity = CaptureFidelity::F3RawAudio;
        assert!(matches!(
            test.vault.capture(request),
            Err(NovaError::PolicyDenied(_))
        ));
    }

    #[test]
    fn wrong_master_key_cannot_open_the_vault() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("vault.sqlite");
        let key = VaultKey::generate().expect("key");
        let vault = Vault::create(&path, key).expect("vault");
        drop(vault);

        let wrong_key = VaultKey::generate().expect("wrong key");
        assert!(matches!(
            Vault::open(&path, wrong_key),
            Err(NovaError::InvalidVaultKey)
        ));
    }

    #[test]
    fn schema_v1_is_migrated_to_idempotency_aware_v2_after_key_verification() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("vault.sqlite");
        let key = VaultKey::generate().expect("key");
        let vault = Vault::create(&path, key.clone()).expect("vault");
        drop(vault);

        let connection = Connection::open(&path).expect("raw database");
        connection
            .execute_batch(
                "DROP TABLE idempotency_records;
                 UPDATE vault_metadata SET value = X'31' WHERE key = 'schema_version';",
            )
            .expect("simulate v1");
        drop(connection);

        let mut migrated = Vault::open(&path, key).expect("migrated vault");
        let first = migrated
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture after migration");
        let replay = migrated
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("replay after migration");
        assert_eq!(first.source.source_id, replay.source.source_id);
    }

    #[test]
    fn deleted_capture_cannot_be_silently_recreated_by_idempotent_replay() {
        let mut test = test_vault();
        let request = capture_request(AssertionOrigin::AiExtractedProposal, PrivacyLevel::Private);
        let captured = test.vault.capture(request.clone()).expect("capture");
        let confirmed = test
            .vault
            .confirm_proposal(ConfirmRequest {
                request_id: None,
                subject: "owner:founder".into(),
                proposal_id: captured.proposal.proposal_id,
            })
            .expect("confirm");
        test.vault
            .delete_memory("owner:founder", &confirmed.memory.memory_id)
            .expect("delete");

        let error = test.vault.capture(request).expect_err("replay unavailable");
        assert!(matches!(
            error,
            NovaError::IdempotencyReplayUnavailable { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn vault_database_is_owner_read_write_only() {
        use std::os::unix::fs::PermissionsExt;

        let test = test_vault();
        let mode = std::fs::metadata(&test.path)
            .expect("vault metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn sensitive_payload_does_not_appear_as_plaintext_in_sqlite_files() {
        let mut test = test_vault();
        let marker = "NOVA_UNIQUE_PLAINTEXT_MARKER_7B3F1D";
        let mut request = capture_request(
            AssertionOrigin::AiExtractedProposal,
            PrivacyLevel::Sensitive,
        );
        request.source_content = format!("source {marker}");
        request.summary = format!("summary {marker}");
        test.vault.capture(request).expect("capture");

        for path in [
            test.path.clone(),
            std::path::PathBuf::from(format!("{}-wal", test.path.display())),
            std::path::PathBuf::from(format!("{}-shm", test.path.display())),
        ] {
            if path.exists() {
                let bytes = std::fs::read(path).expect("database bytes");
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "sensitive payload marker was written in plaintext"
                );
            }
        }
    }

    #[test]
    fn audit_hash_chain_detects_ciphertext_tampering() {
        let mut test = test_vault();
        test.vault
            .capture(capture_request(
                AssertionOrigin::AiExtractedProposal,
                PrivacyLevel::Private,
            ))
            .expect("capture");
        test.vault
            .connection
            .execute(
                "UPDATE event_log SET ciphertext = ?1 WHERE sequence = 2",
                params![vec![0_u8, 1_u8, 2_u8]],
            )
            .expect("tamper event");

        assert!(matches!(
            test.vault.verify_audit_chain(),
            Err(NovaError::AuditIntegrity { sequence: 2, .. })
        ));
    }
}
