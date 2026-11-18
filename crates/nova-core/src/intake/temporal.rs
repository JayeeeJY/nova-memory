//! Historical reads require a separate Owner permission, intersected with live N3 scope.
//! Transaction time (known_as_of) never substitutes for stated validity (valid_at).
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionHistoryPolicy {
    pub connection_id: String,
    pub revision: u64,
    /// None disables historical reads. No existing connection opts in implicitly.
    pub earliest_recorded_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownValidity {
    Exclude,
    IncludeAsUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalQuery {
    pub request_id: String,
    pub query: Option<String>,
    pub memory_id: Option<String>,
    pub max_items: usize,
    pub known_as_of: String,
    pub valid_at: Option<String>,
    pub unknown_validity: UnknownValidity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalMemory {
    pub memory: RevisionRef,
    pub authorized_current_version: RevisionRef,
    pub recorded_at: String,
    pub content: SemanticContent,
    pub origin: SemanticOrigin,
    pub valid_time: ValidTime,
    pub occurred_at: Option<String>,
    pub sources: Vec<SourceMetadata>,
    /// matched_stated_interval, unknown_included or not_filtered; not an assertion of truth.
    pub temporal_match: String,
    pub semantic_support: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalDelivery {
    pub known_as_of: String,
    pub valid_at: Option<String>,
    pub history_policy_revision: u64,
    pub items: Vec<HistoricalMemory>,
    pub more_available: bool,
    pub receipt: ConnectionReceipt,
}

fn history_policy(c: &Connection, key: &VaultKey, id: &str) -> Result<ConnectionHistoryPolicy> {
    Ok(
        optional(c, key, "connection_history_policy", id)?.unwrap_or(ConnectionHistoryPolicy {
            connection_id: id.into(),
            revision: 0,
            earliest_recorded_at: None,
        }),
    )
}

fn time_match(unit: &MemoryUnit, input: &HistoricalQuery) -> Result<Option<&'static str>> {
    let Some(at) = input.valid_at.as_deref().map(parse_time).transpose()? else {
        return Ok(Some("not_filtered"));
    };
    Ok(match &unit.valid_time {
        ValidTime::Unknown if input.unknown_validity == UnknownValidity::IncludeAsUnknown => {
            Some("unknown_included")
        }
        ValidTime::Unknown => None,
        ValidTime::Interval { start, end } => {
            if parse_time(start)? <= at
                && end
                    .as_deref()
                    .map(parse_time)
                    .transpose()?
                    .is_none_or(|end| at < end)
            {
                Some("matched_stated_interval")
            } else {
                None
            }
        }
    })
}

impl IntakeStore {
    /// Owner control plane only. A read grant alone is insufficient to disclose older revisions.
    pub fn set_connection_history_policy(
        &mut self,
        scope: &AssetScope,
        connection_id: &str,
        expected_revision: u64,
        earliest_recorded_at: Option<String>,
    ) -> Result<ConnectionHistoryPolicy> {
        self.check(scope)?;
        if !identifier(connection_id)
            || earliest_recorded_at
                .as_deref()
                .map(parse_time)
                .transpose()?
                .is_some_and(|at| at > OffsetDateTime::now_utc())
        {
            return Err(invalid("historical_policy"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Also rejects revoked/expired connections; a policy cannot recreate client authority.
        continuity_recipient(&tx, &self.key, scope, connection_id)?;
        let old = history_policy(&tx, &self.key, connection_id)?;
        if old.revision != expected_revision {
            return Err(conflict());
        }
        let revision = old.revision.checked_add(1).ok_or_else(conflict)?;
        let policy = ConnectionHistoryPolicy {
            connection_id: connection_id.into(),
            revision,
            earliest_recorded_at,
        };
        if old.revision == 0 {
            insert(
                &tx,
                &self.key,
                "connection_history_policy",
                connection_id,
                &policy,
            )?;
        } else {
            update(
                &tx,
                &self.key,
                "connection_history_policy",
                connection_id,
                &policy,
            )?;
        }
        tx.commit()?;
        Ok(policy)
    }

    pub fn connection_history_policy(
        &self,
        scope: &AssetScope,
        connection_id: &str,
    ) -> Result<ConnectionHistoryPolicy> {
        self.check(scope)?;
        continuity_recipient(&self.connection, &self.key, scope, connection_id)?;
        history_policy(&self.connection, &self.key, connection_id)
    }

    /// Read the last recorded revision at a timestamp, then independently filter stated validity.
    /// No caller-provided scope, resurrection of withdrawn data, or reuse of old authorization.
    pub fn query_connected_historical(
        &mut self,
        scope: &AssetScope,
        access: &ConnectionAccess,
        input: HistoricalQuery,
    ) -> Result<HistoricalDelivery> {
        self.check(scope)?;
        let known = parse_time(&input.known_as_of)?;
        input.valid_at.as_deref().map(parse_time).transpose()?;
        if !identifier(&input.request_id)
            || !(1..=8).contains(&input.max_items)
            || input.query.is_some() == input.memory_id.is_some()
            || input.memory_id.as_ref().is_some_and(|id| !identifier(id))
            || known > OffsetDateTime::now_utc()
        {
            return Err(invalid("historical_query"));
        }
        let lexical = input
            .query
            .as_deref()
            .map(super::super::retrieval::LexicalQuery::new)
            .transpose()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = current(&tx, &self.key, scope, access)?;
        let policy = history_policy(&tx, &self.key, &record.policy.id)?;
        let from = parse_time(policy.earliest_recorded_at.as_deref().ok_or_else(denied)?)?;
        if known < from {
            return Err(denied());
        }
        // Resolve current authority first; historical matches and counts never inspect hidden IDs.
        let permitted = allowed(&tx, &self.key, scope, &record.policy)?;
        let fingerprint = self
            .key
            .authentication_tag(&[b"n3:historical:v0.1", &serde_json::to_vec(&input)?]);
        let cache = call_key(
            &self.key,
            &record.policy.id,
            "query_historical_v01",
            &input.request_id,
        );
        if let Some(old) = saved_call(&tx, &self.key, &cache, &fingerprint)? {
            let old: HistoricalDelivery = serde_json::from_value(old)?;
            if old.history_policy_revision != policy.revision {
                return Err(denied());
            }
            for item in &old.items {
                if !permitted
                    .iter()
                    .any(|u| u.unit.header.object == item.authorized_current_version)
                {
                    return Err(denied());
                }
                // Cache dependency revocation is checked by saved_call; recheck each source too.
                for source in &item.sources {
                    load_source(&tx, &self.key, scope, &source.source)?;
                }
            }
            return Ok(old);
        }
        charge(&mut record)?;
        let mut candidates = Vec::new();
        for current_unit in permitted {
            let head = current_unit.unit.header.object;
            if input
                .memory_id
                .as_ref()
                .is_some_and(|id| *id != head.object_id)
            {
                continue;
            }
            if head.revision > 100 {
                return Err(invalid("history_revision_limit"));
            }
            let mut selected = None;
            // Revision order is commit order; stop at the first visible version. If that version
            // does not match valid_at, do not fall back to an obsolete earlier assertion.
            for revision in (1..=head.revision).rev() {
                let reference = RevisionRef {
                    object_id: head.object_id.clone(),
                    revision,
                };
                let unit: MemoryUnit =
                    load(&tx, &self.key, "unit_revision", &revision_id(&reference))?;
                if unit.header.scope != *scope || unit.header.object != reference {
                    return Err(denied());
                }
                let recorded = parse_time(&unit.header.recorded_at)?;
                if recorded <= known {
                    if recorded >= from {
                        selected = Some(unit);
                    }
                    break;
                }
            }
            let Some(unit) = selected else {
                continue;
            };
            if unit.header.state != UnitState::Active
                || !matches!(
                    unit.header.privacy_level,
                    PrivacyLevel::Private | PrivacyLevel::Shareable
                )
            {
                continue;
            }
            let Some(temporal_match) = time_match(&unit, &input)? else {
                continue;
            };
            let score = lexical
                .as_ref()
                .map(|q| q.score(&unit.content).map(|s| s.0))
                .transpose()?
                .unwrap_or(1);
            if score == 0 {
                continue;
            }
            let mut sources = Vec::new();
            for e in &unit.evidence {
                let source = load_source(&tx, &self.key, scope, &e.source)?;
                if !sources
                    .iter()
                    .any(|s: &SourceMetadata| s.source == source.source)
                {
                    sources.push(SourceMetadata {
                        source: source.source,
                        platform: source.source_platform,
                        role: source.role,
                        capture_fidelity: source.capture_fidelity,
                    });
                }
            }
            candidates.push((
                score,
                HistoricalMemory {
                    memory: unit.header.object,
                    authorized_current_version: head,
                    recorded_at: unit.header.recorded_at,
                    content: unit.content,
                    origin: unit.origin,
                    valid_time: unit.valid_time,
                    occurred_at: unit.occurred_at,
                    sources,
                    temporal_match: temporal_match.into(),
                    semantic_support: "model_output_unverified".into(),
                },
            ));
        }
        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.memory.object_id.cmp(&b.1.memory.object_id))
        });
        let more_available = candidates.len() > input.max_items;
        let items: Vec<_> = candidates
            .into_iter()
            .take(input.max_items)
            .map(|(_, item)| item)
            .collect();
        let receipt = ConnectionReceipt {
            receipt_id: id("disclosure"),
            connection_id: record.policy.id.clone(),
            created_at: now()?,
            purpose: "personal_memory_continuity".into(),
            view: "compiled_unverified_historical".into(),
            memory_versions: items.iter().map(|i| i.memory.clone()).collect(),
            disclosed_fields: vec![
                "content".into(),
                "origin".into(),
                "recorded_and_valid_time".into(),
                "source_metadata".into(),
                "authorizing_version".into(),
            ],
            status: if items.is_empty() {
                "empty"
            } else {
                "delivered"
            }
            .into(),
            remaining_calls: record.policy.call_budget - record.policy.used_calls,
        };
        let result = HistoricalDelivery {
            known_as_of: input.known_as_of,
            valid_at: input.valid_at,
            history_policy_revision: policy.revision,
            items,
            more_available,
            receipt,
        };
        let value = serde_json::to_value(&result)?;
        if serde_json::to_vec(&value)?.len() > 32 * 1024 {
            return Err(invalid("context_byte_budget"));
        }
        store_call(
            &tx,
            &self.key,
            &record,
            &cache,
            fingerprint,
            value,
            CallUsage {
                operation: "read_historical",
                count: result.items.len(),
                receipt_id: result.receipt.receipt_id.clone(),
            },
        )?;
        tx.commit()?;
        Ok(result)
    }
}
