//! Read-only owner projection. Client delivery still goes through the existing metered claim.
use super::*;
use crate::NovaError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerTaskNote {
    /// User-supplied task context, never a model-confirmed fact or execution permission.
    pub kind: String,
    pub text: String,
    pub recorded_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenOwnerTask {
    pub request_id: String,
    pub memory: RevisionRef,
    pub connection_id: String,
    pub instruction: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmendOwnerTask {
    pub request_id: String,
    pub expected_revision: u64,
    pub recipient: SessionRef,
    pub instruction: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskOperation {
    scope: AssetScope,
    fingerprint: String,
    handoff_id: String,
}
fn note_input(request_id: &str, instruction: &str) -> Result<()> {
    if !identifier(request_id) || instruction.trim().is_empty() || instruction.len() > 1024 {
        return Err(invalid("continuity_task_note_limit"));
    }
    Ok(())
}
fn operation_replay(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    id: &str,
    fingerprint: &str,
) -> Result<Option<HandoffMetadata>> {
    if !exists(c, "continuity_task_operation", id)? {
        return Ok(None);
    }
    let op: TaskOperation = load(c, key, "continuity_task_operation", id)?;
    if op.scope != *scope || op.fingerprint != fingerprint {
        return Err(conflict());
    }
    Ok(Some(
        owner_task(c, key, scope, &op.handoff_id)?
            .0
            .envelope
            .metadata,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerTaskCard {
    pub envelope: HandoffEnvelope,
    pub recipient_label: String,
    pub recipient_host: ConnectionHost,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerTaskPage {
    pub items: Vec<OwnerTaskCard>,
    pub next_before: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareTaskResume {
    pub expected_revision: u64,
    pub recipient: SessionRef,
}
/// Only opaque routing metadata leaves this preview; no task text, credentials or new authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResumeReference {
    pub handoff_id: String,
    pub revision: u64,
    pub recipient: SessionRef,
    pub recipient_host: ConnectionHost,
    pub recipient_label: String,
    pub claim_request_id: String,
    pub brief_memory: RevisionRef,
    pub evidence_count: usize,
    pub expires_at: String,
}

fn owner_task(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    id: &str,
) -> Result<(HandoffRecord, MemoryConnection)> {
    require(c)?;
    if !identifier(id)
        || !exists(c, "continuity_handoff", id)?
        || !consolidation::readable(c, &physical_id("continuity_handoff", id))?
    {
        return Err(denied());
    }
    let record: HandoffRecord = load(c, key, "continuity_handoff", id)?;
    let (policy, allowed) = continuity_recipient(
        c,
        key,
        scope,
        &record.envelope.metadata.recipient.connection_id,
    )?;
    live_handoff(c, key, scope, &record, &allowed)?;
    Ok((record, policy))
}

impl IntakeStore {
    /// Current editability only; never delivers task text or changes client authority.
    pub fn owner_task_edit_state(&self, scope: &AssetScope, id: &str) -> Result<HandoffMetadata> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        let metadata = owner_task(&tx, &self.key, scope, id)?.0.envelope.metadata;
        tx.commit()?;
        Ok(metadata)
    }
    /// Eligible existing grants only. Does not include an unapproved memory by changing a grant.
    pub fn owner_task_connections(
        &self,
        scope: &AssetScope,
        memory: &RevisionRef,
    ) -> Result<Vec<MemoryConnection>> {
        self.check(scope)?;
        require(&self.connection)?;
        let mut result = Vec::new();
        for policy in self.connections(scope)? {
            match continuity_recipient(&self.connection, &self.key, scope, &policy.id) {
                Ok((live, allowed)) if allowed.iter().any(|u| u.unit.header.object == *memory) => {
                    result.push(live)
                }
                Ok(_) | Err(NovaError::PolicyDenied(_)) => (),
                Err(error) => return Err(error),
            }
        }
        Ok(result)
    }

    /// Explicit owner goal anchored to one current approved memory. Session IDs stay internal.
    pub fn open_owner_task(
        &mut self,
        scope: &AssetScope,
        input: OpenOwnerTask,
    ) -> Result<HandoffMetadata> {
        self.check(scope)?;
        note_input(&input.request_id, &input.instruction)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let operation = tagged(&self.key, b"task-operation", input.request_id.as_bytes());
        let fingerprint = tagged(&self.key, b"task-open", &serde_json::to_vec(&input)?);
        if let Some(metadata) = operation_replay(&tx, &self.key, scope, &operation, &fingerprint)? {
            return Ok(metadata);
        }
        let (_, allowed) = continuity_recipient(&tx, &self.key, scope, &input.connection_id)?;
        refs_allowed(std::slice::from_ref(&input.memory), &allowed)?;
        let mut sessions = Vec::new();
        for role in ["owner-origin", "recipient"] {
            sessions.push(register_session_in(
                &tx,
                &self.key,
                scope,
                SessionRegistration {
                    request_id: tagged(&self.key, role.as_bytes(), operation.as_bytes()),
                    connection_id: input.connection_id.clone(),
                    lifetime_seconds: 3600,
                },
            )?);
        }
        let metadata = create_handoff_in(
            &tx,
            &self.key,
            scope,
            CreateHandoff {
                request_id: operation.clone(),
                from: sessions[0].clone(),
                recipient: sessions[1].clone(),
                lifetime_seconds: 3600,
                memories: vec![input.memory],
                content: HandoffContent {
                    summary: input.instruction.clone(),
                    completed: vec![],
                    open_tasks: vec![],
                    decisions: vec![],
                    failed_attempts: vec![],
                    constraints: vec![],
                    next_steps: vec![],
                },
            },
        )?;
        let mut record: HandoffRecord =
            load(&tx, &self.key, "continuity_handoff", &metadata.handoff_id)?;
        record.envelope.owner_notes.push(OwnerTaskNote {
            kind: "user_task_goal".into(),
            text: input.instruction,
            recorded_at: now()?,
        });
        if serde_json::to_vec(&(&record.envelope.content, &record.envelope.owner_notes))?.len()
            > MAX_CONTEXT_BYTES
        {
            return Err(invalid("continuity_context_limit"));
        }
        update(
            &tx,
            &self.key,
            "continuity_handoff",
            &metadata.handoff_id,
            &record,
        )?;
        insert(
            &tx,
            &self.key,
            "continuity_task_operation",
            &operation,
            &TaskOperation {
                scope: scope.clone(),
                fingerprint,
                handoff_id: metadata.handoff_id.clone(),
            },
        )?;
        tx.commit()?;
        Ok(metadata)
    }

    /// Append the owner's words, not an invented interpretation. Supersede atomically and do not
    /// extend expiry, evidence scope, connection authority, or the previous successful claim.
    pub fn amend_owner_task(
        &mut self,
        scope: &AssetScope,
        id: &str,
        input: AmendOwnerTask,
    ) -> Result<HandoffMetadata> {
        self.check(scope)?;
        note_input(&input.request_id, &input.instruction)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require(&tx)?;
        let operation = tagged(&self.key, b"task-operation", input.request_id.as_bytes());
        let fingerprint = tagged(
            &self.key,
            b"task-amend",
            &serde_json::to_vec(&(id, &input))?,
        );
        if let Some(metadata) = operation_replay(&tx, &self.key, scope, &operation, &fingerprint)? {
            return Ok(metadata);
        }
        let (mut old, _) = owner_task(&tx, &self.key, scope, id)?;
        let m = &old.envelope.metadata;
        if m.revision != input.expected_revision || m.recipient != input.recipient {
            return Err(conflict());
        }
        if old.envelope.owner_notes.len() >= 16 {
            return Err(invalid("continuity_task_note_limit"));
        }
        let mut notes = old.envelope.owner_notes.clone();
        notes.push(OwnerTaskNote {
            kind: "user_task_clarification".into(),
            text: input.instruction,
            recorded_at: now()?,
        });
        if serde_json::to_vec(&(&old.envelope.content, &notes))?.len() > MAX_CONTEXT_BYTES {
            return Err(invalid("continuity_context_limit"));
        }
        let metadata = create_handoff_in(
            &tx,
            &self.key,
            scope,
            CreateHandoff {
                request_id: operation.clone(),
                from: m.from.clone(),
                recipient: m.recipient.clone(),
                lifetime_seconds: 3600,
                memories: old.envelope.memories.clone(),
                content: old.envelope.content.clone(),
            },
        )?;
        let mut replacement: HandoffRecord =
            load(&tx, &self.key, "continuity_handoff", &metadata.handoff_id)?;
        replacement.envelope.metadata.expires_at = m.expires_at.clone();
        replacement.envelope.owner_notes = notes;
        replacement.envelope.supersedes = Some(id.into());
        old.envelope.metadata.state = HandoffState::Cancelled;
        update(&tx, &self.key, "continuity_handoff", id, &old)?;
        update(
            &tx,
            &self.key,
            "continuity_handoff",
            &metadata.handoff_id,
            &replacement,
        )?;
        insert(
            &tx,
            &self.key,
            "continuity_task_operation",
            &operation,
            &TaskOperation {
                scope: scope.clone(),
                fingerprint,
                handoff_id: metadata.handoff_id,
            },
        )?;
        tx.commit()?;
        Ok(replacement.envelope.metadata)
    }

    /// Bounded inventory of live, evidence-backed task context, never an automatic claim.
    /// A filtered page may be empty while next_before still points to older records.
    pub fn owner_task_cards(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<OwnerTaskPage> {
        self.check(scope)?;
        if before.is_some_and(|n| n <= 0) {
            return Err(invalid("continuity_task_cursor"));
        }
        let tx = self.connection.unchecked_transaction()?;
        require(&tx)?;
        let rows = {
            let mut stmt = tx.prepare("SELECT rowid,handoff_id FROM intake_continuity_handoffs WHERE rowid<?1 ORDER BY rowid DESC LIMIT 2")?;
            stmt.query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let next_before = (rows.len() == 2).then(|| rows.last().unwrap().0);
        let mut items = Vec::new();
        for (_, id) in rows {
            match owner_task(&tx, &self.key, scope, &id) {
                Ok((record, policy)) => items.push(OwnerTaskCard {
                    envelope: record.envelope,
                    recipient_label: policy.label,
                    recipient_host: policy.host,
                }),
                // Expired/revoked/corrected records must not leave stale context on a card.
                Err(NovaError::PolicyDenied(_)) => continue,
                Err(error) => return Err(error),
            }
        }
        let page = OwnerTaskPage { items, next_before };
        if serde_json::to_vec(&page)?.len() > 64 * 1024 {
            return Err(invalid("continuity_task_page_limit"));
        }
        tx.commit()?;
        Ok(page)
    }

    /// Recheck the exact card and live authority. No usage, session, claim or grant is written.
    pub fn prepare_task_resume(
        &self,
        scope: &AssetScope,
        id: &str,
        input: PrepareTaskResume,
    ) -> Result<TaskResumeReference> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        let (record, policy) = owner_task(&tx, &self.key, scope, id)?;
        let m = record.envelope.metadata;
        if m.revision != input.expected_revision || m.recipient != input.recipient {
            return Err(denied());
        }
        let claim_request_id = record
            .claim
            .map(|claim| claim.request_id)
            .unwrap_or_else(|| tagged(&self.key, b"owner_resume", id.as_bytes()));
        let reference = TaskResumeReference {
            handoff_id: m.handoff_id,
            revision: m.revision,
            recipient: m.recipient,
            recipient_host: policy.host,
            recipient_label: policy.label,
            claim_request_id,
            brief_memory: record.envelope.memories[0].clone(),
            evidence_count: record.envelope.memories.len(),
            expires_at: m.expires_at,
        };
        tx.commit()?;
        Ok(reference)
    }
}
