//! Explicit, ephemeral lexical ranking after N3 authorization. No plaintext side index.
//! Scores measure lexical overlap only, never semantic truth or task success.
use super::*;
use crate::CaptureFidelity;
use std::collections::{BTreeMap, BTreeSet};
use time::{Duration, OffsetDateTime};

pub const RANKING_VERSION: &str = "nova.retrieval.lexical-cjk.v0.1";
const MAX_TERMS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedHitTrace {
    pub memory: RevisionRef,
    pub matched_terms: Vec<String>,
    pub exact_substring: bool,
    pub lexical_score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedConnectedDelivery {
    pub algorithm: String,
    pub index_mode: String,
    pub delivery: ConnectedDelivery,
    pub trace: Vec<RankedHitTrace>,
}

/// Locator only. Callers need the exact current detail before answering from a hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexHit {
    pub memory: RevisionRef,
    pub subject: String,
    pub predicate: String,
    pub occurred_at: Option<String>,
    pub source_fidelities: Vec<CaptureFidelity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedIndexDelivery {
    pub items: Vec<MemoryIndexHit>,
    pub more_available: bool,
    pub receipt: ConnectionReceipt,
}

pub(super) fn index(mut delivery: ConnectedDelivery) -> ConnectedIndexDelivery {
    delivery.receipt.view = "compiled_unverified_index".into();
    delivery.receipt.disclosed_fields = [
        "memory_versions",
        "subject",
        "predicate",
        "occurred_at",
        "source_fidelities",
    ]
    .map(String::from)
    .to_vec();
    ConnectedIndexDelivery {
        items: delivery
            .items
            .into_iter()
            .map(|item| {
                let mut source_fidelities = Vec::new();
                for source in item.sources {
                    if !source_fidelities.contains(&source.capture_fidelity) {
                        source_fidelities.push(source.capture_fidelity);
                    }
                }
                MemoryIndexHit {
                    memory: item.memory,
                    subject: item.content.subject,
                    predicate: item.content.predicate,
                    occurred_at: item.occurred_at,
                    source_fidelities,
                }
            })
            .collect(),
        more_available: delivery.more_available,
        receipt: delivery.receipt,
    }
}

pub(super) struct Neighborhood {
    pub anchor: RevisionRef,
    subject: String,
    time: Option<OffsetDateTime>,
    window: Duration,
}

impl Neighborhood {
    pub fn new(anchor: &MemoryUnit, window_days: u8) -> Result<Self> {
        Ok(Self {
            anchor: anchor.header.object.clone(),
            subject: anchor.content.subject.clone(),
            time: anchor
                .occurred_at
                .as_deref()
                .map(super::storage::parse_time)
                .transpose()?,
            window: Duration::days(i64::from(window_days)),
        })
    }

    pub fn time_unknown(&self) -> bool {
        self.time.is_none()
    }

    fn distance(&self, time: Option<&str>) -> Result<Option<Duration>> {
        match (self.time, time) {
            (Some(anchor), Some(time)) => {
                Ok(Some((super::storage::parse_time(time)? - anchor).abs()))
            }
            _ => Ok(None),
        }
    }

    pub fn includes(&self, unit: &MemoryUnit) -> Result<bool> {
        Ok(unit.header.object != self.anchor
            && unit.content.subject == self.subject
            && self
                .distance(unit.occurred_at.as_deref())?
                .is_some_and(|distance| distance <= self.window))
    }

    pub fn nearest(
        &self,
        groups: BTreeMap<String, DeliveredMemory>,
        max_items: usize,
    ) -> Result<Vec<DeliveredMemory>> {
        let mut ordered = groups
            .into_iter()
            .map(|(key, item)| {
                let distance = self
                    .distance(item.occurred_at.as_deref())?
                    .ok_or_else(|| invalid("neighborhood_missing_time"))?;
                Ok((distance, key, item))
            })
            .collect::<Result<Vec<_>>>()?;
        ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Ok(ordered
            .into_iter()
            .take(max_items)
            .map(|(_, _, item)| item)
            .collect())
    }
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af)
}

fn append_run(run: &mut Vec<char>, chinese: bool, terms: &mut BTreeSet<String>) {
    if run.is_empty() {
        return;
    }
    if chinese && run.len() > 1 {
        for pair in run.windows(2) {
            terms.insert(pair.iter().collect());
        }
    } else {
        terms.insert(run.iter().collect());
    }
    run.clear();
}

fn terms(text: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    let mut run = vec![];
    let mut chinese = false;
    for c in text.to_lowercase().chars() {
        if !c.is_alphanumeric() {
            append_run(&mut run, chinese, &mut terms);
        } else {
            let next = cjk(c);
            if chinese != next {
                append_run(&mut run, chinese, &mut terms);
            }
            chinese = next;
            run.push(c);
        }
    }
    append_run(&mut run, chinese, &mut terms);
    terms
}

pub(super) struct LexicalQuery {
    normalized: String,
    terms: BTreeSet<String>,
}

impl LexicalQuery {
    pub(super) fn new(input: &str) -> Result<Self> {
        if input.trim().is_empty() || input.len() > 1000 || input.chars().any(char::is_control) {
            return Err(invalid("retrieval_query_bounds"));
        }
        let terms = terms(input);
        if terms.is_empty() || terms.len() > MAX_TERMS {
            return Err(invalid("retrieval_query_terms"));
        }
        Ok(Self {
            normalized: input.trim().to_lowercase(),
            terms,
        })
    }

    pub(super) fn score(&self, content: &SemanticContent) -> Result<(u32, bool, Vec<String>)> {
        // Only already-authorized content reaches this function. The ephemeral token
        // projection and corpus-independent score cannot inspect other candidates.
        let text = format!(
            "{} {} {} {} {} {}",
            content.subject,
            content.predicate,
            serde_json::to_string(&content.value)?,
            serde_json::to_string(&content.conditions)?,
            serde_json::to_string(&content.polarity)?,
            serde_json::to_string(&content.modality)?
        )
        .to_lowercase();
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid("retrieval_content_bounds"));
        }
        let document = terms(&text);
        let matched: Vec<_> = self.terms.intersection(&document).cloned().collect();
        let exact = text.contains(&self.normalized);
        let count = matched.len() as u32;
        let score =
            u32::from(exact) * 10_000 + count * 100 + count * 1000 / self.terms.len() as u32;
        Ok((score, exact, matched))
    }
}

pub(super) fn explain(input: &str, delivery: ConnectedDelivery) -> Result<RankedConnectedDelivery> {
    let query = LexicalQuery::new(input)?;
    let trace = delivery
        .items
        .iter()
        .map(|item| {
            let (lexical_score, exact_substring, matched_terms) = query.score(&item.content)?;
            Ok(RankedHitTrace {
                memory: item.memory.clone(),
                matched_terms,
                exact_substring,
                lexical_score,
            })
        })
        .collect::<Result<_>>()?;
    Ok(RankedConnectedDelivery {
        algorithm: RANKING_VERSION.into(),
        index_mode: "authorized_ephemeral_projection".into(),
        delivery,
        trace,
    })
}
