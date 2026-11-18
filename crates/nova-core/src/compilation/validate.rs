use super::*;
use crate::{CaptureFidelity, PrivacyLevel, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b))
}

fn qualified(value: &str) -> bool {
    value.split_once(':').is_some_and(|(namespace, name)| {
        [namespace, name].iter().all(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.as_bytes()[0].is_ascii_lowercase()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b))
        })
    })
}

fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.contains('\0')
}

pub(super) fn timestamp(value: &str) -> Result<OffsetDateTime> {
    if value.len() > 64 {
        return Err(invalid("invalid_time"));
    }
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| invalid("invalid_time"))
}

pub(super) fn reference(value: &RevisionRef) -> Result<()> {
    if !identifier(&value.object_id) || value.revision == 0 {
        return Err(invalid("invalid_reference"));
    }
    Ok(())
}

pub(super) fn scope(actual: &AssetScope, expected: &AssetScope) -> Result<()> {
    if actual != expected || !identifier(&actual.owner_id) || !identifier(&actual.space) {
        return Err(invalid("scope_mismatch"));
    }
    Ok(())
}

pub(super) fn rank(privacy: PrivacyLevel) -> u8 {
    match privacy {
        PrivacyLevel::Shareable => 0,
        PrivacyLevel::Private => 1,
        PrivacyLevel::Sensitive => 2,
        PrivacyLevel::Sealed => 3,
    }
}

fn header(value: &AssetHeader, expected: &AssetScope) -> Result<OffsetDateTime> {
    scope(&value.scope, expected)?;
    reference(&value.object)?;
    timestamp(&value.recorded_at)
}

fn valid_time(value: &ValidTime) -> Result<()> {
    if let ValidTime::Interval { start, end } = value {
        let start = timestamp(start)?;
        if let Some(end) = end
            && timestamp(end)? <= start
        {
            return Err(invalid("invalid_interval"));
        }
    }
    Ok(())
}

fn module_ref(value: &ModuleRef) -> Result<()> {
    if !qualified(&value.module_id) || value.revision == 0 {
        return Err(invalid("invalid_module"));
    }
    Ok(())
}

fn json(value: &Value, depth: usize, budget: &mut usize) -> Result<()> {
    if depth > 8 || *budget == 0 {
        return Err(invalid("semantic_payload_limit"));
    }
    *budget -= 1;
    match value {
        Value::String(s) if s.len() > 8192 || s.contains('\0') => {
            return Err(invalid("semantic_payload_limit"));
        }
        Value::Array(items) => {
            if items.len() > 32 {
                return Err(invalid("semantic_payload_limit"));
            }
            for item in items {
                json(item, depth + 1, budget)?;
            }
        }
        Value::Object(items) => {
            if items.len() > 32 || items.keys().any(|s| s.len() > 128 || s.contains('\0')) {
                return Err(invalid("semantic_payload_limit"));
            }
            for item in items.values() {
                json(item, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn evidence<'a>(
    spans: &[EvidenceSpan],
    at: OffsetDateTime,
    privacy: PrivacyLevel,
    sources: &BTreeMap<&str, &'a SourceSnapshot>,
) -> Result<Vec<&'a SourceSnapshot>> {
    if spans.is_empty() || spans.len() > 16 {
        return Err(invalid("evidence_required_or_limit"));
    }
    let mut seen = BTreeSet::new();
    let mut used = Vec::new();
    for span in spans {
        reference(&span.source)?;
        let source = sources
            .get(span.source.object_id.as_str())
            .ok_or_else(|| invalid("source_unavailable"))?;
        if span.source != source.source
            || span.start_char >= span.end_char
            || span.end_char > source.content.chars().count()
            || !text(&span.quote, 4096)
            || !seen.insert((&span.source.object_id, span.start_char, span.end_char))
            || timestamp(&source.recorded_at)? > at
        {
            return Err(invalid("invalid_evidence"));
        }
        let actual: String = source
            .content
            .chars()
            .skip(span.start_char)
            .take(span.end_char - span.start_char)
            .collect();
        if actual != span.quote {
            return Err(invalid("invalid_evidence"));
        }
        if rank(privacy) < rank(source.privacy_level) {
            return Err(invalid("privacy_downgrade"));
        }
        used.push(*source);
    }
    Ok(used)
}

impl IntakeRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 128 * 1024 {
            return Err(invalid("document_limit"));
        }
        let request: Self = wire::decode(bytes, "invalid_intake_shape")?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != INTAKE_VERSION {
            return Err(invalid("unsupported_intake_version"));
        }
        if !identifier(&self.request_id)
            || !text(&self.source_platform, 128)
            || !text(&self.content, 64 * 1024)
        {
            return Err(invalid("invalid_intake_bounds"));
        }
        // N1 starts with text/digests. Raw-audio metadata cannot masquerade as a text upload.
        if self.capture_fidelity == CaptureFidelity::F3RawAudio {
            return Err(invalid("unsupported_source_format"));
        }
        if let Some(at) = &self.occurred_at {
            timestamp(at)?;
        }
        Ok(())
    }
}

impl CompilationSnapshot {
    pub fn parse(bytes: &[u8], expected: &AssetScope) -> Result<Self> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("document_limit"));
        }
        let snapshot: Self = wire::decode(bytes, "invalid_snapshot_shape")?;
        snapshot.validate(expected)?;
        Ok(snapshot)
    }

    pub fn validate(&self, expected: &AssetScope) -> Result<()> {
        if self.schema_version != COMPILATION_VERSION {
            return Err(invalid("unsupported_compilation_version"));
        }
        scope(&self.scope, expected)?;
        if self.sources.len() > 16
            || self.units.len() > 256
            || self.relations.len() > 512
            || self.modules.len() > 16
            || self.tombstones.len() > 512
            || self.protected_unit_ids.len() > 256
        {
            return Err(invalid("object_limit"));
        }
        let mut ids = BTreeSet::new();
        let mut register = |r: &RevisionRef| -> Result<()> {
            reference(r)?;
            if !ids.insert(r.object_id.clone()) {
                return Err(invalid("duplicate_or_deleted_object"));
            }
            Ok(())
        };
        for t in &self.tombstones {
            register(&t.object)?;
            timestamp(&t.deleted_at)?;
        }
        for source in &self.sources {
            register(&source.source)?;
            scope(&source.scope, expected)?;
            timestamp(&source.recorded_at)?;
            if let Some(at) = &source.occurred_at {
                timestamp(at)?;
            }
            if !text(&source.source_platform, 128)
                || !text(&source.content, 64 * 1024)
                || source.capture_fidelity == CaptureFidelity::F3RawAudio
            {
                return Err(invalid("invalid_source"));
            }
        }
        for u in &self.units {
            register(&u.header.object)?;
        }
        for r in &self.relations {
            register(&r.header.object)?;
        }
        let sources: BTreeMap<_, _> = self
            .sources
            .iter()
            .map(|s| (s.source.object_id.as_str(), s))
            .collect();
        let units: BTreeMap<_, _> = self
            .units
            .iter()
            .map(|u| (u.header.object.object_id.as_str(), u))
            .collect();
        if self
            .protected_unit_ids
            .iter()
            .any(|id| !units.contains_key(id.as_str()))
        {
            return Err(invalid("invalid_protection"));
        }
        let mut modules = BTreeSet::new();
        for m in &self.modules {
            module_ref(&m.module)?;
            if !modules.insert(&m.module)
                || !text(&m.description, 2000)
                || m.predicates.is_empty()
                || m.predicates.len() > 64
                || m.predicates.iter().any(|p| !qualified(p))
                || m.predicates.iter().collect::<BTreeSet<_>>().len() != m.predicates.len()
            {
                return Err(invalid("invalid_module"));
            }
            if let Some(parent) = &m.compatible_with {
                module_ref(parent)?;
                if parent.module_id != m.module.module_id || parent.revision >= m.module.revision {
                    return Err(invalid("invalid_module_parent"));
                }
            }
        }
        for unit in &self.units {
            let at = header(&unit.header, expected)?;
            valid_time(&unit.valid_time)?;
            if let Some(at) = &unit.occurred_at {
                timestamp(at)?;
            }
            if !text(&unit.content.subject, 240)
                || !qualified(&unit.content.predicate)
                || unit.content.conditions.len() > 16
                || unit.extensions.len() > 16
                || unit.derived_from.len() > 16
                || unit.modules.len() > 16
                || unit
                    .content
                    .conditions
                    .keys()
                    .chain(unit.extensions.keys())
                    .any(|key| !qualified(key))
            {
                return Err(invalid("invalid_semantics"));
            }
            let mut budget = 2048;
            json(&unit.content.value, 0, &mut budget)?;
            for value in unit
                .content
                .conditions
                .values()
                .chain(unit.extensions.values())
            {
                json(value, 0, &mut budget)?;
            }
            let used = evidence(&unit.evidence, at, unit.header.privacy_level, &sources)?;
            let provenance_ok = match unit.origin {
                SemanticOrigin::UserStatement => used.iter().all(|s| {
                    s.role == SourceRole::User
                        && s.capture_fidelity == CaptureFidelity::F2VisibleTranscript
                }),
                SemanticOrigin::ExternalClaim => {
                    used.iter().all(|s| s.role == SourceRole::External)
                }
                SemanticOrigin::AgentExperience => used.iter().all(|s| s.role == SourceRole::Agent),
                SemanticOrigin::Extracted | SemanticOrigin::Inferred => true,
            };
            if !provenance_ok {
                return Err(invalid("source_origin_mismatch"));
            }
            let mut seen = BTreeSet::new();
            for dependency in &unit.derived_from {
                let parent = units
                    .get(dependency.object_id.as_str())
                    .ok_or_else(|| invalid("dependency_unavailable"))?;
                if dependency != &parent.header.object
                    || dependency.object_id == unit.header.object.object_id
                    || !seen.insert(&dependency.object_id)
                    || timestamp(&parent.header.recorded_at)? > at
                {
                    return Err(invalid("invalid_dependency"));
                }
                if rank(unit.header.privacy_level) < rank(parent.header.privacy_level) {
                    return Err(invalid("privacy_downgrade"));
                }
                if unit.header.state == UnitState::Active
                    && parent.header.state != UnitState::Active
                {
                    return Err(invalid("inactive_dependency"));
                }
            }
            let mut seen = BTreeSet::new();
            for module in &unit.modules {
                module_ref(module)?;
                if !seen.insert(module) {
                    return Err(invalid("duplicate_module_ref"));
                }
            }
        }
        for relation in &self.relations {
            let at = header(&relation.header, expected)?;
            valid_time(&relation.valid_time)?;
            if !qualified(&relation.predicate) || relation.from.object_id == relation.to.object_id {
                return Err(invalid("invalid_relation"));
            }
            evidence(
                &relation.evidence,
                at,
                relation.header.privacy_level,
                &sources,
            )?;
            for endpoint in [&relation.from, &relation.to] {
                let unit = units
                    .get(endpoint.object_id.as_str())
                    .ok_or_else(|| invalid("endpoint_unavailable"))?;
                if endpoint != &unit.header.object || timestamp(&unit.header.recorded_at)? > at {
                    return Err(invalid("stale_relation_endpoint"));
                }
                if rank(relation.header.privacy_level) < rank(unit.header.privacy_level) {
                    return Err(invalid("privacy_downgrade"));
                }
                if relation.header.state == UnitState::Active
                    && unit.header.state != UnitState::Active
                {
                    return Err(invalid("inactive_relation_endpoint"));
                }
            }
        }
        fn visit<'a>(
            id: &'a str,
            units: &BTreeMap<&'a str, &'a MemoryUnit>,
            visiting: &mut BTreeSet<&'a str>,
            done: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if done.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(invalid("cyclic_derivation"));
            }
            for dependency in &units[id].derived_from {
                visit(&dependency.object_id, units, visiting, done)?;
            }
            visiting.remove(id);
            done.insert(id);
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut done = BTreeSet::new();
        for id in units.keys() {
            visit(id, &units, &mut visiting, &mut done)?;
        }
        if serde_json::to_vec(self)?.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("document_limit"));
        }
        Ok(())
    }

    /// IDs to invalidate/rebuild after a source/unit changes. This performs no deletion.
    /// Validate the trusted planning snapshot before calling; the root may already be removed.
    pub fn affected_by(&self, roots: &BTreeSet<String>) -> BTreeSet<String> {
        let mut affected = roots.clone();
        loop {
            let previous = affected.len();
            for unit in &self.units {
                if unit
                    .evidence
                    .iter()
                    .any(|e| affected.contains(&e.source.object_id))
                    || unit
                        .derived_from
                        .iter()
                        .any(|r| affected.contains(&r.object_id))
                {
                    affected.insert(unit.header.object.object_id.clone());
                }
            }
            for relation in &self.relations {
                if affected.contains(&relation.from.object_id)
                    || affected.contains(&relation.to.object_id)
                    || relation
                        .evidence
                        .iter()
                        .any(|e| affected.contains(&e.source.object_id))
                {
                    affected.insert(relation.header.object.object_id.clone());
                }
            }
            if previous == affected.len() {
                break;
            }
        }
        affected
    }
}

impl IntakeReceipt {
    /// Checks internal binding only, not that a remote service actually persisted data.
    pub fn validate(
        &self,
        request: &IntakeRequest,
        job: &CompilationJob,
        snapshot: &CompilationSnapshot,
    ) -> Result<()> {
        use sha2::{Digest, Sha256};
        request.validate()?;
        snapshot.validate(&job.scope)?;
        job.validate(snapshot)?;
        let source = snapshot
            .sources
            .iter()
            .find(|s| s.source == self.source)
            .ok_or_else(|| invalid("receipt_source_unavailable"))?;
        if self.schema_version != INTAKE_VERSION
            || self.request_id != request.request_id
            || self.source != job.source
            || self.job_id != job.job_id
            || timestamp(&self.saved_at)? != timestamp(&source.recorded_at)?
            || request.source_platform != source.source_platform
            || request.occurred_at != source.occurred_at
            || request.content != source.content
            || request.capture_fidelity != source.capture_fidelity
            || self.content_sha256 != hex::encode(Sha256::digest(source.content.as_bytes()))
        {
            return Err(invalid("receipt_binding_mismatch"));
        }
        Ok(())
    }
}

impl CompilationJob {
    pub fn validate(&self, snapshot: &CompilationSnapshot) -> Result<()> {
        scope(&self.scope, &snapshot.scope)?;
        if !identifier(&self.job_id)
            || !snapshot.sources.iter().any(|s| s.source == self.source)
            || self.attempt > 100
            || self.error_code.as_ref().is_some_and(|s| !identifier(s))
            || self
                .committed_change_id
                .as_ref()
                .is_some_and(|s| !identifier(s))
        {
            return Err(invalid("invalid_job"));
        }
        let valid = match self.state {
            JobState::Received => {
                self.attempt == 0 && self.error_code.is_none() && self.committed_change_id.is_none()
            }
            JobState::Processing => {
                self.attempt > 0 && self.error_code.is_none() && self.committed_change_id.is_none()
            }
            JobState::Ready => {
                self.attempt > 0 && self.error_code.is_none() && self.committed_change_id.is_some()
            }
            JobState::Failed => {
                self.attempt > 0 && self.error_code.is_some() && self.committed_change_id.is_none()
            }
        };
        if !valid {
            return Err(invalid("invalid_job_state"));
        }
        Ok(())
    }

    /// Checks a proposed journal transition, not persistence or commit authenticity.
    pub fn validate_successor(&self, next: &Self, snapshot: &CompilationSnapshot) -> Result<()> {
        self.validate(snapshot)?;
        next.validate(snapshot)?;
        if self.job_id != next.job_id || self.scope != next.scope || self.source != next.source {
            return Err(invalid("job_identity_changed"));
        }
        let valid = match (self.state, next.state) {
            (JobState::Received | JobState::Failed, JobState::Processing) => {
                self.attempt.checked_add(1) == Some(next.attempt)
            }
            (JobState::Processing, JobState::Ready | JobState::Failed) => {
                self.attempt == next.attempt
            }
            _ => false,
        };
        if !valid {
            return Err(invalid("invalid_job_transition"));
        }
        Ok(())
    }
}
