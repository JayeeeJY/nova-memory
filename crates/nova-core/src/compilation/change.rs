use super::*;
use crate::Result;
use std::collections::BTreeSet;
use validate::{identifier, rank, scope, timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeImpact {
    /// The caller-supplied context is the conservative compiler read set. The
    /// executor must persist it with output versions, separately from citations.
    pub read_set: Vec<RevisionRef>,
    pub privacy_floor: crate::PrivacyLevel,
    pub created_ids: BTreeSet<String>,
    pub revised_ids: BTreeSet<String>,
    pub affected_ids: BTreeSet<String>,
}

fn revision(old: &AssetHeader, new: &AssetHeader, base: u64) -> Result<()> {
    if old.object.object_id != new.object.object_id
        || old.scope != new.scope
        || old.object.revision != base
        || old.object.revision.checked_add(1) != Some(new.object.revision)
        || timestamp(&new.recorded_at)? < timestamp(&old.recorded_at)?
    {
        return Err(invalid("stale_or_invalid_revision"));
    }
    if rank(new.privacy_level) < rank(old.privacy_level) {
        return Err(invalid("privacy_downgrade"));
    }
    if old.state != UnitState::Active && new.state == UnitState::Active {
        return Err(invalid("lifecycle_resurrection"));
    }
    Ok(())
}

impl ChangeSet {
    /// Parse model data only; callers must also validate against a trusted snapshot.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("document_limit"));
        }
        wire::decode(bytes, "invalid_change_shape")
    }

    /// Dry-run only. No Source, database, permission or job state is written.
    /// A runtime executor must recheck versions, authority and its actual read set
    /// within the committing transaction; this result is not a reusable authorization.
    pub fn validate_against(
        &self,
        current: &CompilationSnapshot,
        expected: &AssetScope,
    ) -> Result<ChangeImpact> {
        self.validate_bound(current, expected, None)
    }

    /// Trusted Owner executor only: exact target/revision from a persisted instruction.
    /// This is not a model operation or reusable grant. The standard compiler path stays protected.
    pub(crate) fn validate_owner_correction(
        &self,
        current: &CompilationSnapshot,
        expected: &AssetScope,
        target: &RevisionRef,
    ) -> Result<ChangeImpact> {
        if self.operations.len() != 1
            || !matches!(&self.operations[0], ChangeOperation::ReviseUnit { base_revision, intent: RevisionIntent::Correct, unit } if *base_revision == target.revision && unit.header.object.object_id == target.object_id)
        {
            return Err(invalid("owner_correction_target_mismatch"));
        }
        self.validate_bound(current, expected, Some(target))
    }

    fn validate_bound(
        &self,
        current: &CompilationSnapshot,
        expected: &AssetScope,
        owner_target: Option<&RevisionRef>,
    ) -> Result<ChangeImpact> {
        current.validate(expected)?;
        scope(&self.scope, expected)?;
        if self.schema_version != COMPILATION_VERSION {
            return Err(invalid("unsupported_compilation_version"));
        }
        if !identifier(&self.change_id)
            || self.operations.len() > 64
            || [
                &self.compiler.model_id,
                &self.compiler.extractor_version,
                &self.compiler.rules_version,
            ]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 128 || s.chars().any(char::is_control))
        {
            return Err(invalid("invalid_change_bounds"));
        }
        let read_set: Vec<_> = current
            .sources
            .iter()
            .map(|s| s.source.clone())
            .chain(current.units.iter().map(|u| u.header.object.clone()))
            .chain(current.relations.iter().map(|r| r.header.object.clone()))
            .collect();
        let privacy_floor = current
            .sources
            .iter()
            .map(|s| s.privacy_level)
            .chain(current.units.iter().map(|u| u.header.privacy_level))
            .chain(current.relations.iter().map(|r| r.header.privacy_level))
            .max_by_key(|p| rank(*p))
            .unwrap_or(crate::PrivacyLevel::Private);
        let mut preview = current.clone();
        let mut existing: BTreeSet<_> = current
            .sources
            .iter()
            .map(|s| s.source.object_id.clone())
            .chain(
                current
                    .units
                    .iter()
                    .map(|u| u.header.object.object_id.clone()),
            )
            .chain(
                current
                    .relations
                    .iter()
                    .map(|r| r.header.object.object_id.clone()),
            )
            .chain(
                current
                    .tombstones
                    .iter()
                    .map(|t| t.object.object_id.clone()),
            )
            .collect();
        let mut touched = BTreeSet::new();
        let mut created = BTreeSet::new();
        let mut revised = BTreeSet::new();
        for operation in &self.operations {
            let header = match operation {
                ChangeOperation::AddUnit { unit } | ChangeOperation::ReviseUnit { unit, .. } => {
                    &unit.header
                }
                ChangeOperation::AddRelation { relation }
                | ChangeOperation::ReviseRelation { relation, .. } => &relation.header,
            };
            if rank(header.privacy_level) < rank(privacy_floor) {
                return Err(invalid("compiler_input_privacy_downgrade"));
            }
            let id = &header.object.object_id;
            if !touched.insert(id.clone()) {
                return Err(invalid("duplicate_change_target"));
            }
            match operation {
                ChangeOperation::AddUnit { unit } => {
                    if !existing.insert(id.clone()) || unit.header.object.revision != 1 {
                        return Err(invalid("duplicate_or_deleted_object"));
                    }
                    preview.units.push(unit.clone());
                    created.insert(id.clone());
                }
                ChangeOperation::ReviseUnit {
                    base_revision,
                    unit,
                    ..
                } => {
                    if current.protected_unit_ids.contains(id)
                        && owner_target.is_none_or(|t| &t.object_id != id)
                    {
                        return Err(invalid("protected_user_correction"));
                    }
                    let old = current
                        .units
                        .iter()
                        .find(|u| u.header.object.object_id == *id)
                        .ok_or_else(|| invalid("revision_target_unavailable"))?;
                    revision(&old.header, &unit.header, *base_revision)?;
                    if old.origin != unit.origin
                        && owner_target.is_none_or(|t| t != &old.header.object)
                    {
                        return Err(invalid("origin_reclassification"));
                    }
                    let target = preview
                        .units
                        .iter_mut()
                        .find(|u| u.header.object.object_id == *id)
                        .ok_or_else(|| invalid("revision_target_unavailable"))?;
                    *target = unit.clone();
                    revised.insert(id.clone());
                }
                ChangeOperation::AddRelation { relation } => {
                    if !existing.insert(id.clone()) || relation.header.object.revision != 1 {
                        return Err(invalid("duplicate_or_deleted_object"));
                    }
                    preview.relations.push(relation.clone());
                    created.insert(id.clone());
                }
                ChangeOperation::ReviseRelation {
                    base_revision,
                    relation,
                } => {
                    let old = current
                        .relations
                        .iter()
                        .find(|r| r.header.object.object_id == *id)
                        .ok_or_else(|| invalid("revision_target_unavailable"))?;
                    revision(&old.header, &relation.header, *base_revision)?;
                    let target = preview
                        .relations
                        .iter_mut()
                        .find(|r| r.header.object.object_id == *id)
                        .ok_or_else(|| invalid("revision_target_unavailable"))?;
                    *target = relation.clone();
                    revised.insert(id.clone());
                }
            }
        }
        // Check the complete preview, not a prefix of the operations. Stale links
        // and dependent views must be revised/inactivated in the same transaction.
        preview.validate(expected)?;
        if serde_json::to_vec(self)?.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("document_limit"));
        }
        let affected = current
            .affected_by(&revised)
            .union(&preview.affected_by(&touched))
            .cloned()
            .collect();
        Ok(ChangeImpact {
            read_set,
            privacy_floor,
            created_ids: created,
            revised_ids: revised,
            affected_ids: affected,
        })
    }
}
