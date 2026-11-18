use super::*;
use crate::DisclosureGrant;

impl Vault {
    fn known_owner(&self, subject: &str) -> Result<bool> {
        if subject == "owner:founder" {
            return Ok(true);
        }
        let sources: Vec<SourceArtifact> = load_all_encrypted_records(
            &self.connection,
            &self.key,
            "source_artifacts",
            "source_id ASC",
        )?;
        Ok(sources.is_empty() || sources.iter().any(|s| s.owner_id == subject))
    }

    /// Durable receipt metadata for hosted reconciliation. No source/query text.
    /// Imported history has no local ReceiptRecorded event and is not new usage.
    pub fn successful_usage_receipts(
        &self,
        subject: &str,
    ) -> Result<Vec<(String, String, String)>> {
        if !self.known_owner(subject)? {
            return Err(NovaError::PolicyDenied("usage owner unavailable".into()));
        }
        let snapshot = self.connection.unchecked_transaction()?;
        self.verify_audit_chain()?;
        let mut q=snapshot.prepare("SELECT r.receipt_id,r.receipt_type FROM receipts r WHERE EXISTS(SELECT 1 FROM event_log e WHERE e.aggregate_id=r.receipt_id AND e.event_type IN ('CaptureReceiptRecorded','MutationReceiptRecorded','ContextDeliveryReceiptRecorded')) ORDER BY r.created_at,r.receipt_id")?;
        let ids = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (id, kind) in ids {
            let envelope = match kind.as_str() {
                "capture_receipt" => {
                    load_encrypted_record::<CaptureReceipt>(
                        &snapshot,
                        &self.key,
                        "receipts",
                        "receipt_id",
                        &id,
                    )?
                    .envelope
                }
                "mutation_receipt" => {
                    load_encrypted_record::<MutationReceipt>(
                        &snapshot,
                        &self.key,
                        "receipts",
                        "receipt_id",
                        &id,
                    )?
                    .envelope
                }
                "context_delivery_receipt" => {
                    load_encrypted_record::<ContextDeliveryReceipt>(
                        &snapshot,
                        &self.key,
                        "receipts",
                        "receipt_id",
                        &id,
                    )?
                    .envelope
                }
                _ => continue,
            };
            if envelope.subject == subject
                && matches!(
                    envelope.status,
                    ReceiptStatus::ProposalCreated
                        | ReceiptStatus::CommittedLocal
                        | ReceiptStatus::Delivered
                )
            {
                out.push((id, kind, envelope.completed_at));
            }
        }
        drop(q);
        snapshot.commit()?;
        Ok(out)
    }

    /// Local owner-only control-plane metadata. Never returns source, summary, query text or keys.
    pub fn owner_access_overview(&self, subject: &str) -> Result<serde_json::Value> {
        if !self.known_owner(subject)? {
            return Err(NovaError::PolicyDenied(
                "owner control plane unavailable".into(),
            ));
        }
        let snapshot = self.connection.unchecked_transaction()?;
        let audit = self.verify_audit_chain()?;
        let mut statement = self.connection.prepare(
            "SELECT aggregate_id FROM event_log WHERE event_type='DisclosureApproved' ORDER BY sequence DESC",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut approvals = Vec::new();
        for id in ids {
            let grant: DisclosureGrant =
                load_event_payload(&self.connection, &self.key, "DisclosureApproved", &id)?;
            if grant.spec.subject != subject {
                continue;
            }
            let revoked: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM event_log WHERE event_type='DisclosureRevoked' AND aggregate_id=?1)",
                params![id], |row| row.get(0),
            )?;
            let expires = OffsetDateTime::parse(&grant.expires_at, &Rfc3339)
                .map_err(|_| NovaError::Validation("invalid approval expiry".into()))?;
            approvals.push(json!({"approval_id": id, "client": grant.spec.client,
                "purpose": grant.spec.purpose, "allowed_ids": grant.allowed_ids,
                "maximum_privacy": grant.maximum_privacy, "expires_at": grant.expires_at,
                "status": if revoked { "revoked" } else if expires <= OffsetDateTime::now_utc() { "expired" } else { "active" }}));
        }
        let mut statement = self.connection.prepare(
            "SELECT receipt_id FROM receipts WHERE receipt_type='context_delivery_receipt' ORDER BY created_at DESC",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut deliveries = Vec::new();
        for id in ids {
            let receipt: ContextDeliveryReceipt =
                load_encrypted_record(&self.connection, &self.key, "receipts", "receipt_id", &id)?;
            if receipt.envelope.subject != subject {
                continue;
            }
            let approval_id:Option<String>=self.connection.query_row("SELECT secondary_id FROM idempotency_records WHERE receipt_id=?1 AND operation='memory.query'",params![id],|r|r.get(0)).optional()?;
            deliveries.push(json!({"receipt_id": receipt.envelope.receipt_id,"approval_id":approval_id,
                "recipient": receipt.recipient, "surface": receipt.surface,
                "purpose": receipt.envelope.purpose, "completed_at": receipt.envelope.completed_at,
                "status": receipt.envelope.status, "disclosed_memory_ids": receipt.disclosed_memory_ids,
                "disclosed_fields": receipt.disclosed_fields, "expires_at": receipt.expires_at}));
        }
        // End prepared statements before committing the read snapshot.
        drop(statement);
        snapshot.commit()?;
        Ok(json!({"approvals": approvals, "deliveries": deliveries, "audit": audit}))
    }
}

impl Vault {
    /// Recovery metadata only, scoped through owner mutation receipts and same-owner imports.
    pub fn owner_recovery_overview(&self, subject: &str) -> Result<serde_json::Value> {
        if !self.known_owner(subject)? {
            return Err(NovaError::PolicyDenied(
                "owner control plane unavailable".into(),
            ));
        }
        let snapshot = self.connection.unchecked_transaction()?;
        let audit = self.verify_audit_chain()?;
        let mut statement = self.connection.prepare("SELECT receipt_id FROM receipts WHERE receipt_type='mutation_receipt' ORDER BY created_at DESC")?;
        let ids = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut deletions = Vec::new();
        for id in ids {
            let receipt: MutationReceipt =
                load_encrypted_record(&self.connection, &self.key, "receipts", "receipt_id", &id)?;
            if receipt.envelope.subject == subject
                && receipt.mutation == "delete_with_crypto_erasure"
            {
                deletions.push(
                    json!({"memory_id":receipt.memory_id,"receipt_id":receipt.envelope.receipt_id,
                    "completed_at":receipt.envelope.completed_at,"status":receipt.envelope.status,
                    "source_policy":"sources_preserved_not_restored_memory"}),
                );
            }
        }
        let mut statement = self.connection.prepare("SELECT aggregate_id,occurred_at FROM event_log WHERE event_type='MigrationImported' ORDER BY sequence DESC")?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut imports = Vec::new();
        for (id, imported_at) in rows {
            let payload: serde_json::Value =
                load_event_payload(&self.connection, &self.key, "MigrationImported", &id)?;
            // Historical packages without an owner field are founder-only evidence.
            if payload["client_access"] == "requires_new_owner_approval"
                && (payload["owner"] == subject
                    || (payload.get("owner").is_none() && subject == "owner:founder"))
            {
                imports.push(json!({"package_digest":id,"imported_at":imported_at,
                    "tombstone_count":payload["tombstones"].as_array().map_or(0,Vec::len),
                    "client_access":"requires_new_owner_approval_at_import"}));
            }
        }
        drop(statement);
        snapshot.commit()?;
        Ok(json!({"audit":audit,"deletions":deletions,"imports":imports}))
    }
}
