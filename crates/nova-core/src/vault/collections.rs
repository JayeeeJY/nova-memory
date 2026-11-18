use super::*;
use crate::{
    CollectionGrant, CollectionGrantRequest, DisclosureGrant, MemoryCollection, QuerySpec,
};
fn deny() -> NovaError {
    NovaError::PolicyDenied("NOVA_RESOURCE_UNAVAILABLE".into())
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
}
fn eligible(
    connection: &Connection,
    key: &VaultKey,
    owner: &str,
    space: &str,
    ids: &[String],
) -> Result<()> {
    if ids.len() > 100 || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len() {
        return Err(NovaError::Validation(
            "collection permits at most 100 distinct canonical members".into(),
        ));
    }
    for id in ids {
        let m: CanonicalMemory =
            load_encrypted_record(connection, key, "canonical_memories", "memory_id", id)?;
        if m.owner_id != owner
            || m.status != MemoryStatus::Active
            || !m.permission_scope.contains(&format!("space:{space}"))
            || matches!(
                m.privacy_level,
                PrivacyLevel::Sensitive | PrivacyLevel::Sealed
            )
        {
            return Err(deny());
        }
    }
    Ok(())
}
impl Vault {
    pub fn put_collection(
        &mut self,
        owner: &str,
        space: &str,
        id: Option<&str>,
        expected_revision: Option<u64>,
        title: &str,
        member_ids: Vec<String>,
    ) -> Result<MemoryCollection> {
        if !identifier(owner) || !identifier(space) || title.trim().is_empty() || title.len() > 240
        {
            return Err(NovaError::Validation("invalid collection".into()));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        eligible(&tx, &self.key, owner, space, &member_ids)?;
        let (id, revision, key_id, key) = if let Some(id) = id {
            let old: MemoryCollection =
                load_event_payload(&tx, &self.key, "CollectionUpdated", id)?;
            if old.owner_id != owner || old.space != space {
                return Err(deny());
            }
            if expected_revision != Some(old.revision) {
                return Err(NovaError::InvalidState(
                    "collection revision changed".into(),
                ));
            }
            let (kid, k) = load_key_by_aggregate(&tx, &self.key, id)?;
            (id.to_owned(), old.revision + 1, kid, k)
        } else {
            let id = new_id("collection");
            let (kid, k) = create_data_key(&tx, &self.key, &id, "collection")?;
            (id, 1, kid, k)
        };
        let collection = MemoryCollection {
            collection_id: id.clone(),
            owner_id: owner.into(),
            space: space.into(),
            title: title.into(),
            member_ids,
            revision,
        };
        append_event(
            &tx,
            &self.audit_key,
            &key_id,
            &key,
            "CollectionUpdated",
            &id,
            &collection,
        )?;
        tx.commit()?;
        Ok(collection)
    }
    pub fn issue_collection_grant(
        &mut self,
        owner: &str,
        request: CollectionGrantRequest,
    ) -> Result<CollectionGrant> {
        if !identifier(&request.client)
            || request.purpose != "personal_memory_continuity"
            || !(1..=2592000).contains(&request.ttl_seconds)
            || !(1..=10).contains(&request.per_call_limit)
            || !(1..=10000).contains(&request.call_quota)
            || !matches!(
                request.maximum_privacy,
                PrivacyLevel::Private | PrivacyLevel::Shareable
            )
        {
            return Err(NovaError::Validation(
                "invalid collection grant limits".into(),
            ));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let c: MemoryCollection =
            load_event_payload(&tx, &self.key, "CollectionUpdated", &request.collection_id)?;
        if c.owner_id != owner || c.member_ids.is_empty() {
            return Err(deny());
        }
        eligible(&tx, &self.key, owner, &c.space, &c.member_ids)?;
        for id in &c.member_ids {
            let m: CanonicalMemory =
                load_encrypted_record(&tx, &self.key, "canonical_memories", "memory_id", id)?;
            if !privacy_at_or_below(m.privacy_level, request.maximum_privacy)
                || !m.purpose_limits.contains(&request.purpose)
            {
                return Err(deny());
            }
        }
        let g = CollectionGrant {
            approval_id: new_id("grant"),
            owner_id: owner.into(),
            space: c.space,
            collection_id: c.collection_id,
            client: request.client,
            purpose: request.purpose,
            member_snapshot: c.member_ids,
            expires_at: add_seconds(request.ttl_seconds)?,
            per_call_limit: request.per_call_limit,
            call_quota: request.call_quota,
            maximum_privacy: request.maximum_privacy,
            include_future_members: request.include_future_members,
        };
        let (kid, k) = create_data_key(&tx, &self.key, &g.approval_id, "collection_grant")?;
        append_event(
            &tx,
            &self.audit_key,
            &kid,
            &k,
            "CollectionGrantIssued",
            &g.approval_id,
            &g,
        )?;
        tx.commit()?;
        Ok(g)
    }
    pub fn owner_collections(&self, owner: &str) -> Result<serde_json::Value> {
        let snapshot = self.connection.unchecked_transaction()?;
        let mut stmt = self.connection.prepare(
            "SELECT DISTINCT aggregate_id FROM event_log WHERE event_type='CollectionUpdated'",
        )?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut collections = Vec::new();
        for id in ids {
            let c: MemoryCollection =
                load_event_payload(&self.connection, &self.key, "CollectionUpdated", &id)?;
            if c.owner_id == owner {
                collections.push(c);
            }
        }
        let mut stmt=self.connection.prepare("SELECT aggregate_id FROM event_log WHERE event_type='CollectionGrantIssued' ORDER BY sequence DESC")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut grants = Vec::new();
        for id in ids {
            let g: CollectionGrant =
                load_event_payload(&self.connection, &self.key, "CollectionGrantIssued", &id)?;
            if g.owner_id != owner {
                continue;
            }
            let revoked:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM event_log WHERE event_type='DisclosureRevoked' AND aggregate_id=?1)",params![id],|r|r.get(0))?;
            let used:usize=self.connection.query_row("SELECT COUNT(*) FROM event_log WHERE event_type='CollectionQuotaUsed' AND aggregate_id=?1",params![id],|r|r.get(0))?;
            let expired = OffsetDateTime::parse(&g.expires_at, &Rfc3339).map_err(|_| deny())?
                <= OffsetDateTime::now_utc();
            grants.push(json!({"grant":g,"used_calls":used,"remaining_calls":g.call_quota.saturating_sub(used),"status":if revoked {"revoked"}else if expired {"expired"}else if used>=g.call_quota {"exhausted"}else {"active"}}));
        }
        drop(stmt);
        snapshot.commit()?;
        Ok(json!({"collections":collections,"grants":grants}))
    }
    pub fn owner_asset_detail(&self, owner: &str, id: &str) -> Result<serde_json::Value> {
        let m: CanonicalMemory = load_encrypted_record(
            &self.connection,
            &self.key,
            "canonical_memories",
            "memory_id",
            id,
        )?;
        if m.owner_id != owner {
            return Err(deny());
        }
        let mut sources = Vec::new();
        for sid in &m.source_artifact_ids {
            let s: SourceArtifact = load_encrypted_record(
                &self.connection,
                &self.key,
                "source_artifacts",
                "source_id",
                sid,
            )?;
            if s.owner_id != owner {
                return Err(deny());
            }
            sources.push(s);
        }
        Ok(json!({"memory":m,"sources":sources}))
    }
}
pub(super) fn resolve(
    connection: &Connection,
    key: &VaultKey,
    spec: &QuerySpec,
    id: &str,
) -> Result<(DisclosureGrant, CollectionGrant)> {
    let g: CollectionGrant =
        load_event_payload(connection, key, "CollectionGrantIssued", id).map_err(|_| deny())?;
    if g.owner_id != spec.subject
        || g.client != spec.client
        || g.space != spec.space
        || g.purpose != spec.purpose
        || OffsetDateTime::parse(&g.expires_at, &Rfc3339).map_err(|_| deny())?
            <= OffsetDateTime::now_utc()
    {
        return Err(deny());
    }
    let c: MemoryCollection =
        load_event_payload(connection, key, "CollectionUpdated", &g.collection_id)?;
    if c.owner_id != g.owner_id || c.space != g.space {
        return Err(deny());
    }
    let ids: Vec<_> = c
        .member_ids
        .into_iter()
        .filter(|id| g.include_future_members || g.member_snapshot.contains(id))
        .collect();
    match &spec.selection {
        crate::QuerySelection::Get {
            memory_id,
            capture_status,
        } if *capture_status || !ids.contains(memory_id) => return Err(deny()),
        crate::QuerySelection::Search { max_items, .. } if *max_items > g.per_call_limit => {
            return Err(deny());
        }
        _ => {}
    }
    Ok((
        DisclosureGrant {
            approval_id: id.into(),
            spec: spec.clone(),
            allowed_ids: ids,
            maximum_privacy: g.maximum_privacy,
            expires_at: g.expires_at.clone(),
            collection_id: Some(g.collection_id.clone()),
        },
        g,
    ))
}
pub(super) fn charge(
    tx: &Transaction<'_>,
    vault_key: &VaultKey,
    audit: &DataKey,
    g: &CollectionGrant,
    request_id: &str,
) -> Result<()> {
    let used: usize = tx.query_row(
        "SELECT COUNT(*) FROM event_log WHERE event_type='CollectionQuotaUsed' AND aggregate_id=?1",
        params![g.approval_id],
        |r| r.get(0),
    )?;
    if used >= g.call_quota {
        return Err(deny());
    }
    let (kid, k) = load_key_by_aggregate(tx, vault_key, &g.approval_id)?;
    append_event(
        tx,
        audit,
        &kid,
        &k,
        "CollectionQuotaUsed",
        &g.approval_id,
        &json!({"request_id":request_id,"used_calls":used+1}),
    )?;
    Ok(())
}
