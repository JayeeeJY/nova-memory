//! Experimental, pure candidate analysis over explicitly structured assertions.
//! This module neither extracts claims nor reads/writes a Vault. Entity and
//! predicate keys must already be reviewed by the caller; textual similarity is
//! intentionally absent. Results are suggestions, never canonical transitions.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    AssertionOrigin, CaptureFidelity, MemoryStatus, MemoryType, NovaError, PrivacyLevel, Result,
};

pub const ENGINE_SCHEMA_VERSION: &str = "nova.engine.experimental.v0.3";
const MAX_ASSERTIONS: usize = 256;
const MAX_CANDIDATES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssertionTime {
    /// Missing time is uncertainty, not an unbounded interval.
    Unknown,
    /// Half-open [start, end); None is an explicitly open end.
    Interval { start: String, end: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionEvidence {
    pub source_artifact_id: String,
    /// Opaque source-local pointer. The engine never fetches or resolves it.
    pub locator: String,
    pub fidelity: CaptureFidelity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtomicAssertion {
    pub assertion_id: String,
    pub owner_id: String,
    pub entity_id: String,
    pub memory_type: MemoryType,
    /// An explicitly assigned semantic slot, not a free-text similarity key.
    pub predicate: String,
    /// Distinguishes conditions, modality, negation and scope of the claim.
    pub context_key: String,
    pub value: String,
    /// Conflict candidates require both sides to declare the slot single-valued.
    pub single_valued: bool,
    pub valid_time: AssertionTime,
    pub observed_at: String,
    pub confirmed_at: Option<String>,
    pub origin: AssertionOrigin,
    pub privacy_level: PrivacyLevel,
    pub status: MemoryStatus,
    pub evidence: Vec<AssertionEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateKind {
    Duplicate,
    MergeProposal,
    ConflictSet,
    TemporalEvolution,
    NeedsReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReason {
    SameClaimAndInterval,
    SameValueOverlappingIntervals,
    DifferentValuesInExclusiveSlot,
    NonOverlappingIntervals,
    UnknownValidTime,
    EpistemicOrPrivacyDifference,
    MultiValuedSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionCandidate {
    pub incoming_id: String,
    pub existing_id: String,
    pub kind: CandidateKind,
    pub reason: CandidateReason,
    /// Preserve both evidence chains; do not synthesize a new authoritative source.
    pub incoming_evidence: Vec<AssertionEvidence>,
    pub existing_evidence: Vec<AssertionEvidence>,
    pub requires_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineAnalysis {
    pub schema_version: String,
    pub candidates: Vec<AssertionCandidate>,
    /// No matching structured slot found; this does not prove semantic novelty.
    pub unmatched_incoming_ids: Vec<String>,
    pub excluded_existing_ids: Vec<String>,
}

type ParsedInterval = (OffsetDateTime, Option<OffsetDateTime>);

fn timestamp(value: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| NovaError::Validation("engine timestamp must be RFC3339".into()))
}

fn interval(value: &AssertionTime) -> Result<Option<ParsedInterval>> {
    match value {
        AssertionTime::Unknown => Ok(None),
        AssertionTime::Interval { start, end } => {
            let start = timestamp(start)?;
            let end = end.as_deref().map(timestamp).transpose()?;
            if end.is_some_and(|end| end <= start) {
                return Err(NovaError::Validation(
                    "engine interval must have end after start".into(),
                ));
            }
            Ok(Some((start, end)))
        }
    }
}

fn bounded(value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(NovaError::Validation(
            "engine field is empty, too long or contains controls".into(),
        ));
    }
    Ok(())
}

fn validate(assertion: &AtomicAssertion, owner: &str) -> Result<()> {
    if assertion.owner_id != owner {
        return Err(NovaError::PolicyDenied(
            "engine assertions must belong to one owner".into(),
        ));
    }
    for value in [
        &assertion.assertion_id,
        &assertion.entity_id,
        &assertion.predicate,
        &assertion.context_key,
    ] {
        bounded(value, 256)?;
    }
    bounded(&assertion.value, 4000)?;
    timestamp(&assertion.observed_at)?;
    if let Some(confirmed) = &assertion.confirmed_at {
        timestamp(confirmed)?;
    }
    interval(&assertion.valid_time)?;
    if assertion.evidence.is_empty() || assertion.evidence.len() > 32 {
        return Err(NovaError::Validation(
            "engine assertion requires 1..32 evidence pointers".into(),
        ));
    }
    for evidence in &assertion.evidence {
        bounded(&evidence.source_artifact_id, 256)?;
        bounded(&evidence.locator, 512)?;
    }
    Ok(())
}

fn classify(a: &AtomicAssertion, b: &AtomicAssertion) -> Result<(CandidateKind, CandidateReason)> {
    let (Some(a_time), Some(b_time)) = (interval(&a.valid_time)?, interval(&b.valid_time)?) else {
        return Ok((
            CandidateKind::NeedsReview,
            CandidateReason::UnknownValidTime,
        ));
    };
    let disjoint =
        a_time.1.is_some_and(|end| end <= b_time.0) || b_time.1.is_some_and(|end| end <= a_time.0);
    if disjoint {
        return Ok((
            CandidateKind::TemporalEvolution,
            CandidateReason::NonOverlappingIntervals,
        ));
    }
    if a.origin != b.origin || a.privacy_level != b.privacy_level {
        return Ok((
            CandidateKind::NeedsReview,
            CandidateReason::EpistemicOrPrivacyDifference,
        ));
    }
    if a.value == b.value && a.single_valued == b.single_valued {
        return Ok(if a_time == b_time {
            (
                CandidateKind::Duplicate,
                CandidateReason::SameClaimAndInterval,
            )
        } else {
            (
                CandidateKind::MergeProposal,
                CandidateReason::SameValueOverlappingIntervals,
            )
        });
    }
    if a.value != b.value && a.single_valued && b.single_valued {
        return Ok((
            CandidateKind::ConflictSet,
            CandidateReason::DifferentValuesInExclusiveSlot,
        ));
    }
    Ok((CandidateKind::NeedsReview, CandidateReason::MultiValuedSlot))
}

/// Analyze at most 256 incoming and 256 existing assertions, preserving input
/// order. The owner argument scopes analysis; it is NOT an authentication token.
/// Hosts must not be allowed to supply authoritative entity/slot mappings or use
/// this pure helper as a bypass around Vault permissions and user review.
pub fn analyze_assertions(
    owner: &str,
    incoming: &[AtomicAssertion],
    existing: &[AtomicAssertion],
) -> Result<EngineAnalysis> {
    bounded(owner, 256)?;
    if incoming.len() > MAX_ASSERTIONS || existing.len() > MAX_ASSERTIONS {
        return Err(NovaError::Validation(
            "engine assertion batch exceeds 256 per side".into(),
        ));
    }
    let mut ids = HashSet::new();
    for assertion in incoming.iter().chain(existing) {
        validate(assertion, owner)?;
        if !ids.insert(&assertion.assertion_id) {
            return Err(NovaError::Validation(
                "engine assertion IDs must be unique across the batch".into(),
            ));
        }
    }
    if incoming.iter().any(|a| {
        a.status != MemoryStatus::Active
            || matches!(
                a.origin,
                AssertionOrigin::Disputed | AssertionOrigin::Superseded
            )
    }) {
        return Err(NovaError::Validation(
            "incoming engine assertions must be active and undisputed".into(),
        ));
    }
    let eligible = |a: &&AtomicAssertion| {
        a.status == MemoryStatus::Active
            && !matches!(
                a.origin,
                AssertionOrigin::Disputed | AssertionOrigin::Superseded
            )
    };
    let mut result = EngineAnalysis {
        schema_version: ENGINE_SCHEMA_VERSION.into(),
        candidates: Vec::new(),
        unmatched_incoming_ids: Vec::new(),
        excluded_existing_ids: existing
            .iter()
            .filter(|a| !eligible(a))
            .map(|a| a.assertion_id.clone())
            .collect(),
    };
    for a in incoming {
        let before = result.candidates.len();
        for b in existing.iter().filter(eligible) {
            if a.entity_id != b.entity_id
                || a.memory_type != b.memory_type
                || a.predicate != b.predicate
                || a.context_key != b.context_key
            {
                continue;
            }
            if result.candidates.len() == MAX_CANDIDATES {
                return Err(NovaError::Validation(
                    "engine candidate limit of 1024 exceeded; narrow the batch".into(),
                ));
            }
            let (kind, reason) = classify(a, b)?;
            result.candidates.push(AssertionCandidate {
                incoming_id: a.assertion_id.clone(),
                existing_id: b.assertion_id.clone(),
                kind,
                reason,
                incoming_evidence: a.evidence.clone(),
                existing_evidence: b.evidence.clone(),
                requires_review: true,
            });
        }
        if before == result.candidates.len() {
            result.unmatched_incoming_ids.push(a.assertion_id.clone());
        }
    }
    Ok(result)
}
