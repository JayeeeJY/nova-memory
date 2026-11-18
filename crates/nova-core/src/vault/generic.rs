use super::*;
use crate::{DisclosureGrant, MemoryView, QueryOutcome, QuerySelection, QuerySpec};

pub(super) fn client_scope_allows(scopes: &[String], client: &str, space: &str) -> bool {
    let scoped = scopes
        .iter()
        .any(|s| s.starts_with("client:") || s.starts_with("space:"));
    if !scoped {
        return client == "chatgpt" && space == "personal";
    }
    scopes.contains(&format!("client:{client}")) && scopes.contains(&format!("space:{space}"))
}
fn hide_missing(error: NovaError) -> NovaError {
    match error {
        NovaError::NotFound { .. } => unavailable(),
        other => other,
    }
}
fn unavailable() -> NovaError {
    NovaError::PolicyDenied("NOVA_RESOURCE_UNAVAILABLE".into())
}
fn validate(spec: &QuerySpec) -> Result<()> {
    validate_request_id(Some(&spec.request_id))?;
    if [&spec.subject, &spec.client, &spec.space]
        .iter()
        .any(|value| {
            value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
        })
        || spec.purpose != "personal_memory_continuity"
    {
        return Err(unavailable());
    }
    match &spec.selection {
        QuerySelection::Get { memory_id, .. } if memory_id.is_empty() || memory_id.len() > 128 => {
            return Err(NovaError::Validation("invalid memory ID".into()));
        }
        QuerySelection::Search { query, max_items }
            if query.trim().is_empty() || query.len() > 1000 || !(1..=10).contains(max_items) =>
        {
            return Err(NovaError::Validation("invalid search bounds".into()));
        }
        _ => (),
    }
    Ok(())
}
fn permitted(
    owner: &str,
    privacy: PrivacyLevel,
    scopes: &[String],
    purposes: &[String],
    grant: &DisclosureGrant,
) -> bool {
    owner == grant.spec.subject
        && privacy_at_or_below(privacy, grant.maximum_privacy)
        && !matches!(privacy, PrivacyLevel::Sensitive | PrivacyLevel::Sealed)
        && if grant.collection_id.is_some() {
            scopes.contains(&format!("space:{}", grant.spec.space))
        } else {
            client_scope_allows(scopes, &grant.spec.client, &grant.spec.space)
        }
        && purposes.iter().any(|p| p == &grant.spec.purpose)
}
fn source_metadata(
    connection: &Connection,
    key: &VaultKey,
    source: &str,
) -> Result<(Option<String>, CaptureFidelity)> {
    let source: SourceArtifact =
        load_encrypted_record(connection, key, "source_artifacts", "source_id", source)?;
    let value: serde_json::Value = serde_json::from_str(&source.content).unwrap_or_default();
    Ok((
        value
            .get("occurred_at")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        source.capture_fidelity,
    ))
}
fn view(
    connection: &Connection,
    key: &VaultKey,
    id: &str,
    capture_status: bool,
    grant: &DisclosureGrant,
) -> Result<MemoryView> {
    if !grant.allowed_ids.iter().any(|allowed| allowed == id) {
        return Err(unavailable());
    }
    let proposal: Option<MemoryProposal> = if id.starts_with("proposal_") {
        Some(
            load_encrypted_record(connection, key, "memory_proposals", "proposal_id", id)
                .map_err(hide_missing)?,
        )
    } else {
        None
    };
    if let Some(p) = &proposal {
        if !permitted(
            &p.owner_id,
            p.privacy_level,
            &p.permission_scope,
            &p.purpose_limits,
            grant,
        ) {
            return Err(unavailable());
        }
        if capture_status {
            let source: SourceArtifact = load_encrypted_record(
                connection,
                key,
                "source_artifacts",
                "source_id",
                &p.source_artifact_id,
            )?;
            if source.source_platform != grant.spec.client {
                return Err(unavailable());
            }
            let canonical_id: Option<String> = connection
                .query_row(
                    "SELECT memory_id FROM canonical_memories WHERE proposal_id=?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            let expose = p.status == ProposalStatus::Pending;
            let (occurred_at, capture_fidelity) =
                source_metadata(connection, key, &p.source_artifact_id)?;
            return Ok(MemoryView {
                memory_id: id.into(),
                canonical_memory_id: canonical_id,
                active_successor_ids: vec![],
                status: p.status.as_str().into(),
                title: expose.then(|| p.title.clone()),
                summary: expose.then(|| p.summary.clone()),
                memory_type: p.memory_type,
                assertion_origin: p.proposed_origin,
                occurred_at,
                capture_fidelity,
                source_artifact_ids: vec![p.source_artifact_id.clone()],
            });
        }
    } else if capture_status {
        return Err(unavailable());
    }
    let canonical_id = if proposal.is_some() {
        connection
            .query_row(
                "SELECT memory_id FROM canonical_memories WHERE proposal_id=?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(unavailable)?
    } else {
        id.into()
    };
    let m: CanonicalMemory = load_encrypted_record(
        connection,
        key,
        "canonical_memories",
        "memory_id",
        &canonical_id,
    )
    .map_err(hide_missing)?;
    if !permitted(
        &m.owner_id,
        m.privacy_level,
        &m.permission_scope,
        &m.purpose_limits,
        grant,
    ) || matches!(m.status, MemoryStatus::Disputed | MemoryStatus::Deleted)
    {
        return Err(unavailable());
    }
    let active = m.status == MemoryStatus::Active;
    if grant.collection_id.is_some() && !active {
        return Err(unavailable());
    }
    let mut successors = Vec::new();
    // Traverse bounded version links, never include unapproved content.
    let mut queue = m.superseded_by.clone();
    let mut seen = std::collections::HashSet::new();
    while let Some(next) = queue.pop() {
        if !seen.insert(next.clone()) || seen.len() > 32 {
            return Err(unavailable());
        }
        let successor: CanonicalMemory =
            load_encrypted_record(connection, key, "canonical_memories", "memory_id", &next)
                .map_err(hide_missing)?;
        if !permitted(
            &successor.owner_id,
            successor.privacy_level,
            &successor.permission_scope,
            &successor.purpose_limits,
            grant,
        ) {
            return Err(unavailable());
        }
        if successor.status == MemoryStatus::Active {
            successors.push(next);
        } else if successor.status == MemoryStatus::Superseded {
            queue.extend(successor.superseded_by);
        }
    }
    successors.sort();
    let (occurred_at, capture_fidelity) = source_metadata(
        connection,
        key,
        m.source_artifact_ids.first().ok_or_else(unavailable)?,
    )?;
    Ok(MemoryView {
        memory_id: id.into(),
        canonical_memory_id: Some(canonical_id),
        active_successor_ids: successors,
        status: m.status.as_str().into(),
        title: active.then_some(m.title),
        summary: active.then_some(m.summary),
        memory_type: m.memory_type,
        assertion_origin: m.assertion_origin,
        occurred_at,
        capture_fidelity,
        source_artifact_ids: m.source_artifact_ids,
    })
}
fn collect(
    connection: &Connection,
    key: &VaultKey,
    grant: &DisclosureGrant,
) -> Result<Vec<MemoryView>> {
    match &grant.spec.selection {
        QuerySelection::Get {
            memory_id,
            capture_status,
        } => Ok(vec![view(
            connection,
            key,
            memory_id,
            *capture_status,
            grant,
        )?]),
        QuerySelection::Search { query, max_items } => {
            let mut matches = Vec::new();
            for id in &grant.allowed_ids {
                let item = match view(connection, key, id, false, grant) {
                    Ok(item) => item,
                    Err(NovaError::PolicyDenied(_)) => continue,
                    Err(error) => return Err(error),
                };
                if item.status != "active" {
                    continue;
                }
                let haystack = format!(
                    "{} {}",
                    item.title.as_deref().unwrap_or_default(),
                    item.summary.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                let score = haystack.matches(&query.to_lowercase()).count();
                if score > 0 {
                    matches.push((score, item));
                }
            }
            matches.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.memory_id.cmp(&b.1.memory_id))
            });
            Ok(matches
                .into_iter()
                .take(*max_items)
                .map(|(_, v)| v)
                .collect())
        }
    }
}
impl Vault {
    pub fn capture_canonical_id(&self, proposal_id: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT memory_id FROM canonical_memories WHERE proposal_id=?1",
                params![proposal_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    /// Owner control plane only. Never expose as an MCP tool.
    pub fn issue_disclosure_grant(
        &mut self,
        spec: QuerySpec,
        allowed_ids: Vec<String>,
        maximum_privacy: PrivacyLevel,
        ttl_seconds: i64,
    ) -> Result<DisclosureGrant> {
        validate(&spec)?;
        if allowed_ids.is_empty()
            || allowed_ids.len() > 100
            || allowed_ids.iter().any(|s| s.is_empty() || s.len() > 128)
            || !(1..=3600).contains(&ttl_seconds)
            || !matches!(
                maximum_privacy,
                PrivacyLevel::Private | PrivacyLevel::Shareable
            )
        {
            return Err(NovaError::Validation("invalid grant bounds".into()));
        }
        let unique = allowed_ids.iter().collect::<std::collections::HashSet<_>>();
        if unique.len() != allowed_ids.len()
            || (matches!(spec.selection, QuerySelection::Search { .. })
                && allowed_ids.iter().any(|id| !id.starts_with("memory_")))
        {
            return Err(NovaError::Validation(
                "search scope must contain distinct canonical IDs".into(),
            ));
        }
        if let QuerySelection::Get { memory_id, .. } = &spec.selection
            && (allowed_ids.len() != 1 || &allowed_ids[0] != memory_id)
        {
            return Err(unavailable());
        }
        let grant = DisclosureGrant {
            collection_id: None,
            approval_id: new_id("approval"),
            spec,
            allowed_ids,
            maximum_privacy,
            expires_at: add_seconds(ttl_seconds)?,
        };
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // Verify the explicit scope against current records before persisting approval.
        for id in &grant.allowed_ids {
            let capture = matches!(
                &grant.spec.selection,
                QuerySelection::Get {
                    capture_status: true,
                    ..
                }
            );
            view(&tx, &self.key, id, capture, &grant)?;
        }
        let (key_id, key) =
            create_data_key(&tx, &self.key, &grant.approval_id, "disclosure_grant")?;
        append_event(
            &tx,
            &self.audit_key,
            &key_id,
            &key,
            "DisclosureApproved",
            &grant.approval_id,
            &grant,
        )?;
        tx.commit()?;
        Ok(grant)
    }
    pub fn revoke_disclosure_grant(&mut self, subject: &str, approval_id: &str) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let grant_owner = if approval_id.starts_with("grant_") {
            let g: crate::CollectionGrant =
                load_event_payload(&tx, &self.key, "CollectionGrantIssued", approval_id)
                    .map_err(hide_missing)?;
            g.owner_id
        } else {
            let g: DisclosureGrant =
                load_event_payload(&tx, &self.key, "DisclosureApproved", approval_id)
                    .map_err(hide_missing)?;
            g.spec.subject
        };
        if grant_owner != subject {
            return Err(unavailable());
        }
        let revoked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_log WHERE event_type='DisclosureRevoked' AND aggregate_id=?1)",
            params![approval_id], |row| row.get(0),
        )?;
        if revoked {
            tx.commit()?;
            return Ok(());
        }
        let (key_id, key) = load_key_by_aggregate(&tx, &self.key, approval_id)?;
        append_event(
            &tx,
            &self.audit_key,
            &key_id,
            &key,
            "DisclosureRevoked",
            approval_id,
            &json!({"approval_id":approval_id}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn query_memory(&mut self, spec: QuerySpec, approval_id: &str) -> Result<QueryOutcome> {
        validate(&spec)?;
        if approval_id.len() > 128 {
            return Err(unavailable());
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (grant, collection_grant) = if approval_id.starts_with("grant_") {
            let (g, c) = super::collections::resolve(&tx, &self.key, &spec, approval_id)?;
            (g, Some(c))
        } else {
            let g: DisclosureGrant =
                load_event_payload(&tx, &self.key, "DisclosureApproved", approval_id)
                    .map_err(hide_missing)?;
            (g, None)
        };
        let revoked: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM event_log WHERE event_type='DisclosureRevoked' AND aggregate_id=?1)",params![approval_id],|row|row.get(0))?;
        let expires =
            OffsetDateTime::parse(&grant.expires_at, &Rfc3339).map_err(|_| unavailable())?;
        if revoked || expires <= OffsetDateTime::now_utc() || grant.spec != spec {
            return Err(unavailable());
        }
        // State and scope are checked again on every replay, before decryption of old disclosure.
        let items = collect(&tx, &self.key, &grant)?;
        let hash = request_fingerprint(
            &self.audit_key,
            "memory.query",
            &(spec.clone(), approval_id),
        )?;
        let record: Option<(String,String,String,String)>=tx.query_row("SELECT operation,request_hash,primary_id,receipt_id FROM idempotency_records WHERE request_id=?1",params![spec.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((op, stored, primary, receipt_id)) = record {
            if op != "memory.query" || stored != hash {
                return Err(NovaError::IdempotencyConflict {
                    request_id: spec.request_id,
                });
            }
            let prior: Vec<MemoryView> =
                load_event_payload(&tx, &self.key, "MemoryQueryDelivered", &primary)
                    .map_err(hide_missing)?;
            if prior != items {
                return Err(unavailable());
            }
            let receipt =
                load_encrypted_record(&tx, &self.key, "receipts", "receipt_id", &receipt_id)
                    .map_err(hide_missing)?;
            return Ok(QueryOutcome {
                schema_version: "nova.query.result.v0.1".into(),
                items: prior,
                receipt,
            });
        }
        if let Some(g) = &collection_grant {
            super::collections::charge(&tx, &self.key, &self.audit_key, g, &spec.request_id)?;
        }
        let pack_id = new_id("context");
        let (key_id, key) = create_data_key(&tx, &self.key, &pack_id, "memory_query")?;
        let event_hash = append_event(
            &tx,
            &self.audit_key,
            &key_id,
            &key,
            "MemoryQueryDelivered",
            &pack_id,
            &items,
        )?;
        let ids = items
            .iter()
            .filter_map(|i| i.canonical_memory_id.clone())
            .collect::<Vec<_>>();
        let member_ids = items
            .iter()
            .map(|i| i.memory_id.clone())
            .chain(ids.iter().cloned())
            .collect::<Vec<_>>();
        for id in &member_ids {
            tx.execute("INSERT OR IGNORE INTO context_pack_members(context_pack_id,memory_id) VALUES(?1,?2)",params![pack_id,id])?;
        }
        let receipt_id = new_id("receipt");
        let receipt = ContextDeliveryReceipt {
            envelope: ReceiptEnvelope {
                receipt_id: receipt_id.clone(),
                receipt_type: ReceiptType::ContextDeliveryReceipt,
                subject: spec.subject.clone(),
                request_id: spec.request_id.clone(),
                policy_version: POLICY_VERSION.into(),
                purpose: spec.purpose.clone(),
                source_refs: items.iter().map(|i| i.memory_id.clone()).collect(),
                requested_at: now()?,
                completed_at: now()?,
                status: ReceiptStatus::Delivered,
                error_code: None,
                integrity_hash: receipt_hash(
                    &self.audit_key,
                    &receipt_id,
                    &spec.request_id,
                    ReceiptStatus::Delivered,
                    &ids,
                ),
                previous_audit_event_hash: Some(event_hash),
            },
            context_pack_id: pack_id.clone(),
            recipient: spec.client,
            surface: ContextSurface::ChatText,
            disclosed_memory_ids: ids,
            disclosed_fields: [
                "memory_id",
                "canonical_memory_id",
                "active_successor_ids",
                "status",
                "title",
                "summary",
                "memory_type",
                "assertion_origin",
                "capture_fidelity",
                "source_artifact_ids",
                "occurred_at",
            ]
            .map(str::to_owned)
            .to_vec(),
            expires_at: grant.expires_at,
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
        tx.execute("INSERT INTO idempotency_records(request_id,operation,request_hash,primary_id,secondary_id,receipt_id,created_at) VALUES(?1,'memory.query',?2,?3,?4,?5,?6)",params![spec.request_id,hash,pack_id,approval_id,receipt_id,now()?])?;
        tx.commit()?;
        Ok(QueryOutcome {
            schema_version: "nova.query.result.v0.1".into(),
            items,
            receipt,
        })
    }
}
