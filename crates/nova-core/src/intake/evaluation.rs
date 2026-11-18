//! N5 offline paired-task contracts. Reference checks and Owner observations are not independent Gold.
use super::{storage::*, *};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const N5_VERSION: &str = "nova.n5.v0.1";
pub const N5_SCORING_RULES: &str = "n5-exact-reference-source-set-v2";
/// Hash of the fixed offline scoring contract, not a claim of independent semantic Gold.
pub fn n5_scoring_rules_hash() -> String {
    hex::encode(Sha256::digest(N5_SCORING_RULES.as_bytes()))
}
pub const MAX_TASK_EVALUATION_BYTES: usize = 512 * 1024;
const MAX_HISTORY: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum N5Arm {
    NoMemory,
    RawRetrieval,
    Compiled,
    Relations,
}
impl N5Arm {
    pub const ALL: [Self; 4] = [
        Self::NoMemory,
        Self::RawRetrieval,
        Self::Compiled,
        Self::Relations,
    ];
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum N5Category {
    Facts,
    CrossMaterial,
    Time,
    Updates,
    UnknownConflict,
    Privacy,
}
impl N5Category {
    pub const ALL: [Self; 6] = [
        Self::Facts,
        Self::CrossMaterial,
        Self::Time,
        Self::Updates,
        Self::UnknownConflict,
        Self::Privacy,
    ];
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum N5Split {
    Development,
    Validation,
    Test,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum N5RunKind {
    FixtureContract,
    OwnerReportedObservations,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Material {
    pub source_id: String,
    pub event_chain_id: String,
    pub content: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Case {
    pub case_id: String,
    pub category: N5Category,
    pub split: N5Split,
    pub event_chain_ids: Vec<String>,
    pub question: String,
    pub visible_source_ids: Vec<String>,
    pub structure_evolution: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5ReferenceCheck {
    pub case_id: String,
    pub answer_exact: Option<String>,
    pub required_source_ids: Vec<String>,
    pub allowed_source_ids: Vec<String>,
    pub expected_as_of: Option<String>,
    pub must_abstain: bool,
    pub forbidden_substrings: Vec<String>,
    pub superseded_answers: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Dataset {
    pub schema_version: String,
    pub dataset_version: String,
    pub evaluation_scope: String,
    pub reference_status: String,
    pub tasks: Vec<N5Case>,
    pub materials: Vec<N5Material>,
    pub reference_checks: Vec<N5ReferenceCheck>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Budget {
    pub max_tool_calls: u32,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5ArmRun {
    pub arm: N5Arm,
    pub run_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5MaintenanceCosts {
    pub ingest_usd: Option<f64>,
    pub maintenance_usd: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Manifest {
    pub schema_version: String,
    pub run_kind: N5RunKind,
    pub model_id: String,
    pub model_version: String,
    pub agent_version: String,
    pub prompt_version: String,
    pub prompt_hash: String,
    pub rules_version: String,
    pub rules_hash: String,
    pub dataset_hash: String,
    pub asset_snapshot_id: String,
    pub started_at: String,
    pub completed_at: String,
    pub budget: N5Budget,
    pub arm_runs: Vec<N5ArmRun>,
    pub case_ids: Vec<String>,
    pub costs: N5MaintenanceCosts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Observations {
    pub latency_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct N5Prediction {
    pub case_id: String,
    pub arm: N5Arm,
    pub run_id: String,
    pub configuration_hash: String,
    pub completed_at: String,
    pub answer: String,
    pub evidence_ids: Vec<String>,
    pub as_of: Option<String>,
    pub abstained: bool,
    pub disclosed_source_ids: Vec<String>,
    pub tool_calls: u32,
    pub observed: N5Observations,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEvaluationSubmission {
    pub request_id: String,
    pub dataset: N5Dataset,
    pub manifest: N5Manifest,
    pub predictions: Vec<N5Prediction>,
}
impl TaskEvaluationSubmission {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_TASK_EVALUATION_BYTES {
            return Err(invalid("n5_payload_too_large"));
        }
        Ok(serde_json::from_slice(bytes)?)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N5ObservedMetric {
    pub observed: u64,
    pub total: u64,
    pub value: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N5CategoryMetrics {
    pub category: N5Category,
    pub cases: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub source_correct_count: u64,
    pub source_forgery_count: u64,
    pub outdated_count: u64,
    pub unknown_handled_count: u64,
    pub privacy_violation_count: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N5ArmMetrics {
    pub arm: N5Arm,
    pub cases: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub source_correct_count: u64,
    pub source_forgery_count: u64,
    pub outdated_count: u64,
    pub unknown_handled_count: u64,
    pub privacy_violation_count: u64,
    pub latency_ms: N5ObservedMetric,
    pub input_tokens: N5ObservedMetric,
    pub output_tokens: N5ObservedMetric,
    pub cost_usd: N5ObservedMetric,
    pub categories: Vec<N5CategoryMetrics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N5PairedMetrics {
    pub comparison_arm: N5Arm,
    pub no_memory_success: u64,
    pub comparison_success: u64,
    pub improved: u64,
    pub regressed: u64,
    pub unchanged: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvaluationReport {
    pub schema_version: String,
    pub evaluation_scope: String,
    pub run_kind: N5RunKind,
    pub quality_status: String,
    pub level_status: String,
    pub dataset_hash: String,
    pub configuration_hash: String,
    pub asset_snapshot_id: String,
    pub prompt_hash: String,
    pub rules_hash: String,
    pub budget: N5Budget,
    pub arm_runs: Vec<N5ArmRun>,
    pub case_ids: Vec<String>,
    pub started_at: String,
    pub model_id: String,
    pub model_version: String,
    pub agent_version: String,
    pub prompt_version: String,
    pub rules_version: String,
    pub completed_at: String,
    pub task_count: u64,
    pub structure_evolution_tasks: u64,
    pub metrics: Vec<N5ArmMetrics>,
    pub pairs: Vec<N5PairedMetrics>,
    pub safety_gate: bool,
    pub source_forgery_count: u64,
    pub severe_boundary_count: u64,
    pub costs: N5MaintenanceCosts,
    pub evidence_note: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTaskEvaluation {
    pub evaluation_id: String,
    pub request_id: String,
    pub recorded_at: String,
    pub asset_snapshot_id: String,
    pub stale: bool,
    pub report: TaskEvaluationReport,
    pub submission_fingerprint: String,
}

fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn opaque(s: &str) -> bool {
    bounded(s, 128)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_:-.".contains(c))
}
fn frozen_version(s: &str) -> bool {
    opaque(s)
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "latest" | "unknown" | "unversioned" | "unconfigured"
        )
}
fn hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn unique(v: &[String]) -> bool {
    v.iter().all(|v| opaque(v)) && v.iter().collect::<BTreeSet<_>>().len() == v.len()
}
fn digest<T: Serialize>(v: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(v)?)))
}
pub fn n5_dataset_hash(dataset: &N5Dataset) -> Result<String> {
    digest(dataset)
}
pub fn n5_configuration_hash(manifest: &N5Manifest) -> Result<String> {
    digest(&(
        &manifest.schema_version,
        &manifest.run_kind,
        &manifest.model_id,
        &manifest.model_version,
        &manifest.agent_version,
        &manifest.prompt_version,
        &manifest.prompt_hash,
        &manifest.rules_version,
        &manifest.rules_hash,
        &manifest.dataset_hash,
        &manifest.asset_snapshot_id,
        &manifest.budget,
    ))
}
fn cost(value: Option<f64>) -> bool {
    value.is_none_or(|v| v.is_finite() && (0.0..=100_000.0).contains(&v))
}
fn timestamp(value: &str) -> Result<time::OffsetDateTime> {
    let at = parse_time(value)?;
    if at > time::OffsetDateTime::now_utc() {
        return Err(invalid("n5_future_date"));
    }
    Ok(at)
}

/// Checks synthetic reference structure only. This never certifies Gold or semantic correctness.
pub fn validate_n5(dataset: &N5Dataset) -> Result<()> {
    if serde_json::to_vec(dataset)?.len() > MAX_TASK_EVALUATION_BYTES
        || dataset.schema_version != N5_VERSION
        || dataset.evaluation_scope != "synthetic_fixed_tasks"
        || dataset.reference_status != "synthetic_reference_checks_not_independent_gold"
        || !opaque(&dataset.dataset_version)
        || dataset.tasks.len() != 60
        || dataset.reference_checks.len() != 60
        || dataset.materials.is_empty()
        || dataset.materials.len() > 240
    {
        return Err(invalid("n5_dataset_contract"));
    }
    let mut materials = BTreeMap::new();
    for m in &dataset.materials {
        if !opaque(&m.source_id)
            || !opaque(&m.event_chain_id)
            || !bounded(&m.content, 4096)
            || !m.content.starts_with("SYNTHETIC")
            || materials.insert(&m.source_id, m).is_some()
        {
            return Err(invalid("n5_material_contract"));
        }
    }
    let mut ids = BTreeSet::new();
    let mut counts = BTreeMap::new();
    let mut chains = BTreeMap::new();
    let mut splits = BTreeSet::new();
    let mut evolution = 0;
    for task in &dataset.tasks {
        if !opaque(&task.case_id)
            || !ids.insert(task.case_id.clone())
            || !bounded(&task.question, 2048)
            || task.event_chain_ids.is_empty()
            || task.event_chain_ids.len() > 8
            || !unique(&task.event_chain_ids)
            || task.visible_source_ids.len() > 16
            || !unique(&task.visible_source_ids)
        {
            return Err(invalid("n5_case_contract"));
        }
        *counts.entry(task.category).or_insert(0) += 1;
        splits.insert(task.split);
        evolution += u64::from(task.structure_evolution);
        for chain in &task.event_chain_ids {
            if chains
                .insert(chain.clone(), task.split)
                .is_some_and(|old| old != task.split)
            {
                return Err(invalid("n5_event_chain_split_leakage"));
            }
        }
        for source in &task.visible_source_ids {
            let material = materials
                .get(source)
                .ok_or_else(|| invalid("n5_unknown_material"))?;
            if !task.event_chain_ids.contains(&material.event_chain_id) {
                return Err(invalid("n5_cross_chain_material"));
            }
        }
    }
    if splits.len() != 3
        || evolution == 0
        || N5Category::ALL.iter().any(|c| counts.get(c) != Some(&10))
    {
        return Err(invalid("n5_category_split_contract"));
    }
    let tasks = dataset
        .tasks
        .iter()
        .map(|t| (&t.case_id, t))
        .collect::<BTreeMap<_, _>>();
    let mut checked = BTreeSet::new();
    for check in &dataset.reference_checks {
        let task = tasks
            .get(&check.case_id)
            .ok_or_else(|| invalid("n5_unknown_reference"))?;
        if !checked.insert(&check.case_id)
            || check.required_source_ids.len() > 16
            || check.allowed_source_ids.len() > 16
            || !unique(&check.required_source_ids)
            || !unique(&check.allowed_source_ids)
            || check
                .required_source_ids
                .iter()
                .any(|id| !check.allowed_source_ids.contains(id))
            || check
                .allowed_source_ids
                .iter()
                .any(|id| !task.visible_source_ids.contains(id))
            || check
                .answer_exact
                .as_ref()
                .is_some_and(|a| !bounded(a, 2048))
            || check.must_abstain == check.answer_exact.is_some()
            || check.superseded_answers.len() > 16
            || check.superseded_answers.iter().any(|s| !bounded(s, 256))
            || (task.category == N5Category::UnknownConflict && !check.must_abstain)
            || (task.category == N5Category::Privacy && check.forbidden_substrings.is_empty())
            || check.forbidden_substrings.len() > 16
            || check.forbidden_substrings.iter().any(|s| !bounded(s, 256))
        {
            return Err(invalid("n5_reference_contract"));
        }
        if let Some(at) = &check.expected_as_of {
            timestamp(at)?;
        }
    }
    Ok(())
}

#[derive(Clone)]
struct Outcome {
    success: bool,
    source_correct: bool,
    forged: bool,
    outdated: bool,
    unknown: bool,
    privacy: bool,
}
fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn observed_metric(
    values: impl Iterator<Item = Option<f64>>,
    total: u64,
    average: bool,
) -> N5ObservedMetric {
    let observed = values.flatten().collect::<Vec<_>>();
    N5ObservedMetric {
        observed: observed.len() as u64,
        total,
        value: (!observed.is_empty()).then(|| {
            observed.iter().sum::<f64>() / if average { observed.len() as f64 } else { 1.0 }
        }),
    }
}
/// Deterministic exact-answer/reference contract scoring; never a model, IQ, age, or capability grade.
pub fn score_n5(submission: &TaskEvaluationSubmission) -> Result<TaskEvaluationReport> {
    if serde_json::to_vec(submission)?.len() > MAX_TASK_EVALUATION_BYTES {
        return Err(invalid("n5_payload_too_large"));
    }
    validate_n5(&submission.dataset)?;
    let m = &submission.manifest;
    if !opaque(&submission.request_id)
        || m.schema_version != N5_VERSION
        || !opaque(&m.asset_snapshot_id)
        || [
            &m.model_id,
            &m.model_version,
            &m.agent_version,
            &m.prompt_version,
            &m.rules_version,
        ]
        .iter()
        .any(|s| !opaque(s))
        || [
            &m.model_version,
            &m.agent_version,
            &m.prompt_version,
            &m.rules_version,
        ]
        .iter()
        .any(|s| !frozen_version(s))
        || !hash(&m.prompt_hash)
        || !hash(&m.rules_hash)
        || m.rules_version != N5_SCORING_RULES
        || m.rules_hash != n5_scoring_rules_hash()
        || m.dataset_hash != n5_dataset_hash(&submission.dataset)?
        || !cost(m.costs.ingest_usd)
        || !cost(m.costs.maintenance_usd)
        || m.budget.max_tool_calls > 100
        || m.budget.max_input_tokens == 0
        || m.budget.max_input_tokens > 1_000_000
        || m.budget.max_output_tokens == 0
        || m.budget.max_output_tokens > 100_000
        || m.budget.timeout_ms == 0
        || m.budget.timeout_ms > 3_600_000
    {
        return Err(invalid("n5_manifest_contract"));
    }
    let started = timestamp(&m.started_at)?;
    let completed = timestamp(&m.completed_at)?;
    if completed < started {
        return Err(invalid("n5_clock_order"));
    }
    let tasks = submission
        .dataset
        .tasks
        .iter()
        .map(|t| (&t.case_id, t))
        .collect::<BTreeMap<_, _>>();
    if !unique(&m.case_ids)
        || m.case_ids.iter().collect::<BTreeSet<_>>() != tasks.keys().copied().collect()
        || m.arm_runs.len() != 4
        || submission.predictions.len() != 240
    {
        return Err(invalid("n5_full_paired_run_required"));
    }
    let mut runs = BTreeMap::new();
    let mut run_ids = BTreeSet::new();
    for run in &m.arm_runs {
        if !opaque(&run.run_id)
            || !run_ids.insert(&run.run_id)
            || runs.insert(run.arm, &run.run_id).is_some()
        {
            return Err(invalid("n5_run_contract"));
        }
    }
    let config = n5_configuration_hash(m)?;
    let checks = submission
        .dataset
        .reference_checks
        .iter()
        .map(|c| (&c.case_id, c))
        .collect::<BTreeMap<_, _>>();
    let sources = submission
        .dataset
        .materials
        .iter()
        .map(|s| &s.source_id)
        .collect::<BTreeSet<_>>();
    let mut rows = BTreeMap::new();
    let mut outcomes = BTreeMap::new();
    for p in &submission.predictions {
        tasks
            .get(&p.case_id)
            .ok_or_else(|| invalid("n5_prediction_case"))?;
        let check = checks[&p.case_id];
        let at = timestamp(&p.completed_at)?;
        if at < started
            || at > completed
            || runs.get(&p.arm) != Some(&&p.run_id)
            || p.configuration_hash != config
            || rows.insert((p.arm, &p.case_id), p).is_some()
            || p.answer.len() > 4096
            || p.answer
                .chars()
                .any(|c| c.is_control() && !c.is_whitespace())
            || !unique(&p.evidence_ids)
            || !unique(&p.disclosed_source_ids)
            || p.evidence_ids.len() > 16
            || p.disclosed_source_ids.len() > 16
            || p.tool_calls > m.budget.max_tool_calls
            || !cost(p.observed.cost_usd)
            || p.observed
                .input_tokens
                .is_some_and(|v| v > m.budget.max_input_tokens)
            || p.observed
                .output_tokens
                .is_some_and(|v| v > m.budget.max_output_tokens)
            || p.observed
                .latency_ms
                .is_some_and(|v| v > m.budget.timeout_ms)
            || (p.arm == N5Arm::NoMemory
                && (p.tool_calls != 0
                    || !p.evidence_ids.is_empty()
                    || !p.disclosed_source_ids.is_empty()))
        {
            return Err(invalid("n5_prediction_contract_or_drift"));
        }
        if let Some(at) = &p.as_of {
            timestamp(at)?;
        }
        let forged = p.evidence_ids.iter().any(|id| !sources.contains(id));
        let privacy = p
            .disclosed_source_ids
            .iter()
            .chain(p.evidence_ids.iter())
            .any(|id| !check.allowed_source_ids.contains(id))
            || check
                .forbidden_substrings
                .iter()
                .any(|s| normalized(&p.answer).contains(&normalized(s)));
        let source_correct = !forged
            && !privacy
            && p.evidence_ids.len() == check.required_source_ids.len()
            && check
                .required_source_ids
                .iter()
                .all(|id| p.evidence_ids.contains(id));
        let outdated = (check.expected_as_of.is_some() && p.as_of != check.expected_as_of)
            || check
                .superseded_answers
                .iter()
                .any(|a| normalized(a) == normalized(&p.answer));
        let answer_correct = if check.must_abstain {
            p.abstained && p.answer.trim().is_empty()
        } else {
            !p.abstained
                && check
                    .answer_exact
                    .as_ref()
                    .is_some_and(|a| normalized(&p.answer) == normalized(a))
        };
        let success = answer_correct && source_correct && !outdated && !privacy && !forged;
        outcomes.insert(
            (p.arm, &p.case_id),
            Outcome {
                success,
                source_correct,
                forged,
                outdated,
                unknown: check.must_abstain && answer_correct,
                privacy,
            },
        );
    }
    let mut metrics = Vec::new();
    for arm in N5Arm::ALL {
        let categories = N5Category::ALL
            .iter()
            .map(|category| {
                let o = submission
                    .dataset
                    .tasks
                    .iter()
                    .filter(|t| &t.category == category)
                    .map(|t| &outcomes[&(arm, &t.case_id)])
                    .collect::<Vec<_>>();
                N5CategoryMetrics {
                    category: *category,
                    cases: o.len() as u64,
                    success_count: o.iter().filter(|o| o.success).count() as u64,
                    error_count: o.iter().filter(|o| !o.success).count() as u64,
                    source_correct_count: o.iter().filter(|o| o.source_correct).count() as u64,
                    source_forgery_count: o.iter().filter(|o| o.forged).count() as u64,
                    outdated_count: o.iter().filter(|o| o.outdated).count() as u64,
                    unknown_handled_count: o.iter().filter(|o| o.unknown).count() as u64,
                    privacy_violation_count: o.iter().filter(|o| o.privacy).count() as u64,
                }
            })
            .collect::<Vec<_>>();
        let sum = |f: fn(&N5CategoryMetrics) -> u64| categories.iter().map(f).sum();
        let predictions = submission
            .predictions
            .iter()
            .filter(|p| p.arm == arm)
            .collect::<Vec<_>>();
        metrics.push(N5ArmMetrics {
            arm,
            cases: 60,
            success_count: sum(|v| v.success_count),
            error_count: sum(|v| v.error_count),
            source_correct_count: sum(|v| v.source_correct_count),
            source_forgery_count: sum(|v| v.source_forgery_count),
            outdated_count: sum(|v| v.outdated_count),
            unknown_handled_count: sum(|v| v.unknown_handled_count),
            privacy_violation_count: sum(|v| v.privacy_violation_count),
            latency_ms: observed_metric(
                predictions
                    .iter()
                    .map(|p| p.observed.latency_ms.map(|v| v as f64)),
                60,
                true,
            ),
            input_tokens: observed_metric(
                predictions
                    .iter()
                    .map(|p| p.observed.input_tokens.map(|v| v as f64)),
                60,
                false,
            ),
            output_tokens: observed_metric(
                predictions
                    .iter()
                    .map(|p| p.observed.output_tokens.map(|v| v as f64)),
                60,
                false,
            ),
            cost_usd: observed_metric(predictions.iter().map(|p| p.observed.cost_usd), 60, false),
            categories,
        });
    }
    let pairs = [N5Arm::RawRetrieval, N5Arm::Compiled, N5Arm::Relations]
        .iter()
        .map(|arm| {
            let mut out = N5PairedMetrics {
                comparison_arm: *arm,
                no_memory_success: 0,
                comparison_success: 0,
                improved: 0,
                regressed: 0,
                unchanged: 0,
            };
            for task in &submission.dataset.tasks {
                let a = outcomes[&(N5Arm::NoMemory, &task.case_id)].success;
                let b = outcomes[&(*arm, &task.case_id)].success;
                out.no_memory_success += u64::from(a);
                out.comparison_success += u64::from(b);
                match (a, b) {
                    (false, true) => out.improved += 1,
                    (true, false) => out.regressed += 1,
                    _ => out.unchanged += 1,
                }
            }
            out
        })
        .collect();
    let forged = metrics.iter().map(|m| m.source_forgery_count).sum();
    let severe = metrics.iter().map(|m| m.privacy_violation_count).sum();
    Ok(TaskEvaluationReport {
        schema_version: "nova.task-evaluation-report.v0.1".into(),
        evaluation_scope: "synthetic_fixed_tasks".into(),
        run_kind: m.run_kind,
        quality_status: "independent_quality_uncalibrated".into(),
        level_status: "uncalibrated".into(),
        dataset_hash: m.dataset_hash.clone(),
        configuration_hash: config,
        asset_snapshot_id: m.asset_snapshot_id.clone(),
        prompt_hash: m.prompt_hash.clone(),
        rules_hash: m.rules_hash.clone(),
        budget: m.budget.clone(),
        arm_runs: m.arm_runs.clone(),
        case_ids: m.case_ids.clone(),
        started_at: m.started_at.clone(),
        model_id: m.model_id.clone(),
        model_version: m.model_version.clone(),
        agent_version: m.agent_version.clone(),
        prompt_version: m.prompt_version.clone(),
        rules_version: m.rules_version.clone(),
        completed_at: m.completed_at.clone(),
        task_count: 60,
        structure_evolution_tasks: submission.dataset.tasks.iter().filter(|t| t.structure_evolution).count() as u64,
        metrics, pairs,
        safety_gate: forged == 0 && severe == 0,
        source_forgery_count: forged,
        severe_boundary_count: severe,
        costs: m.costs.clone(),
        evidence_note: "Exact synthetic answer and required-source-set checks only; this is not semantic citation accuracy. Owner-reported observations are unverified. No independent Gold, model capability, personal-Vault benefit, IQ, age or L0-L4 certification.".into(),
    })
}

impl IntakeStore {
    /// Owner route only. No submitted success/verification flags are accepted, and no answers are stored.
    pub fn record_task_evaluation(
        &mut self,
        scope: &AssetScope,
        submission: &TaskEvaluationSubmission,
    ) -> Result<StoredTaskEvaluation> {
        self.check(scope)?;
        let report = score_n5(submission)?;
        let fingerprint = digest(submission)?;
        let logical = self
            .key
            .authentication_tag(&[b"n5:evaluation", submission.request_id.as_bytes()]);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current =
            super::report::construction_report_on(&tx, &self.key, scope)?.content_snapshot_id;
        if submission.manifest.asset_snapshot_id != current {
            return Err(invalid("n5_asset_snapshot_mismatch"));
        }
        tx.execute_batch("CREATE TABLE IF NOT EXISTS intake_task_evaluation_requests(binding TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);")?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT fingerprint FROM intake_task_evaluation_requests WHERE binding=?1",
                [&logical],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            if previous != fingerprint {
                return Err(conflict());
            }
            // A pruned history entry cannot be silently rerun under an old request id.
            let mut stored: StoredTaskEvaluation =
                load(&tx, &self.key, "task_evaluation", &logical)?;
            stored.stale = stored.asset_snapshot_id != current;
            return Ok(stored);
        }
        let ledger_count: i64 = tx.query_row(
            "SELECT count(*) FROM intake_task_evaluation_requests",
            [],
            |r| r.get(0),
        )?;
        if ledger_count >= 4096 {
            return Err(invalid("n5_request_ledger_capacity"));
        }
        let stored = StoredTaskEvaluation {
            evaluation_id: logical.clone(),
            request_id: submission.request_id.clone(),
            recorded_at: now()?,
            asset_snapshot_id: current,
            stale: false,
            report,
            submission_fingerprint: fingerprint.clone(),
        };
        insert(&tx, &self.key, "task_evaluation", &logical, &stored)?;
        tx.execute(
            "INSERT INTO intake_task_evaluation_requests VALUES (?1,?2)",
            params![logical, fingerprint],
        )?;
        let rows = {
            let mut q = tx.prepare(
                "SELECT id FROM intake_objects WHERE kind='task_evaluation' ORDER BY rowid DESC",
            )?;
            q.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for old in rows.into_iter().skip(MAX_HISTORY) {
            if super::consolidation::available(&tx)? {
                tx.execute("DELETE FROM intake_dependencies WHERE record_id=?1", [&old])?;
            }
            tx.execute(
                "DELETE FROM intake_objects WHERE id=?1 AND kind='task_evaluation'",
                params![old],
            )?;
        }
        tx.commit()?;
        Ok(stored)
    }
    /// Historical report metadata is retained, but mutations invalidate applicability to the current snapshot.
    pub fn task_evaluations(
        &self,
        scope: &AssetScope,
        current_content_snapshot_id: &str,
    ) -> Result<Vec<StoredTaskEvaluation>> {
        self.check(scope)?;
        let mut q = self.connection.prepare("SELECT id FROM intake_objects WHERE kind='task_evaluation' ORDER BY rowid DESC LIMIT 20")?;
        let ids = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|physical| {
                let logical = physical
                    .strip_prefix("n2a:task_evaluation:")
                    .ok_or_else(conflict)?;
                let mut report: StoredTaskEvaluation =
                    load(&self.connection, &self.key, "task_evaluation", logical)?;
                report.stale = report.asset_snapshot_id != current_content_snapshot_id;
                Ok(report)
            })
            .collect()
    }
}
