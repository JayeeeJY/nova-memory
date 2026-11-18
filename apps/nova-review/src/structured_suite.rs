//! Offline fixed-response substrate for the unchanged 12-case catalog.
//! It has no transport/provider execution path. A simulated dispatch exercises core
//! authorization only and must never be presented as a real model call or Gold pass.
use crate::{compiler, quote_text, quote_text_schema};
use nova_core::{CaptureFidelity, NovaError, PrivacyLevel, VaultKey, compilation::*, intake::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const PROTOCOL: &str = "nova.structured-suite.fixture.v0.1";
pub const PROFILE: &str = "qwen-quote-text-suite-fixture-v01";
pub const CONDITION_LIST_PROFILE: &str = "qwen-quote-text-suite-fixture-v10";
pub const CATALOG: &str = include_str!("../../../eval/model-acceptance/cases.json");
const ORIGIN: &str = "http://127.0.0.1:11434";
const MODEL: &str = "qwen3:8b-q4_K_M";
const UPPER_INPUT: u64 = 131200;
const UPPER_OUTPUT: u64 = 2048;
const RESERVATION: u64 = UPPER_INPUT + UPPER_OUTPUT;
fn error(code: &str) -> NovaError {
    NovaError::Validation(format!("structured_suite_fixture: {code}"))
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn canonical(value: &impl Serialize) -> Result<Vec<u8>, NovaError> {
    Ok(serde_json::to_vec(&serde_json::to_value(value)?)?)
}
fn hash_value(value: &impl Serialize) -> Result<String, NovaError> {
    Ok(hash(&canonical(value)?))
}
fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Case {
    pub id: String,
    pub operation: String,
    pub text: Option<String>,
    pub target_case: Option<String>,
    pub model_calls: u64,
}
#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}
pub fn cases() -> Result<Vec<Case>, NovaError> {
    let c: Cases = serde_json::from_str(CATALOG)?;
    if c.cases.len() != 12 || c.cases.iter().map(|c| c.model_calls).sum::<u64>() != 10 {
        return Err(error("catalog_changed"));
    }
    Ok(c.cases)
}
pub fn metadata() -> Value {
    metadata_for(FixtureProfile::Legacy)
}

#[derive(Clone, Copy)]
pub enum FixtureProfile {
    Legacy,
    ConditionList,
}
impl FixtureProfile {
    fn name(self) -> &'static str {
        match self {
            Self::Legacy => PROFILE,
            Self::ConditionList => CONDITION_LIST_PROFILE,
        }
    }
    fn prompt(self) -> (&'static str, &'static str) {
        match self {
            Self::Legacy => (quote_text_schema::PROMPT_VERSION, quote_text_schema::PROMPT),
            Self::ConditionList => (
                quote_text_schema::GENERAL_PROMPT_V10_VERSION,
                quote_text_schema::GENERAL_PROMPT_V10,
            ),
        }
    }
    fn response_format(self) -> Value {
        match self {
            Self::Legacy => quote_text_schema::response_format(),
            Self::ConditionList => quote_text_schema::condition_list_response_format(),
        }
    }
}
pub fn metadata_for(profile: FixtureProfile) -> Value {
    let (prompt_version, prompt) = profile.prompt();
    let response_format = profile.response_format();
    json!({"schema_version":PROTOCOL,"profile":profile.name(),"fixture_only":true,"real_model_path_available":false,"real_model_dispatches":0,"maximum_simulated_dispatches":10,"automatic_retries":0,"maximum_seconds":1200,"per_step_seconds":120,"maximum_output_tokens":2048,"cases_sha256":hash(CATALOG.as_bytes()),"prompt_sha256":hash(prompt.as_bytes()),"prompt_version":prompt_version,"generation_schema_sha256":hash(quote_text_schema::SCHEMA_JSON.as_bytes()),"provider_schema_sha256":hash(&serde_json::to_vec(&response_format["json_schema"]["schema"]).expect("fixed schema")),"resolver_source_sha256":hash(include_bytes!("quote_text.rs")),"semantic_acceptance":"pending_independent_gold_and_review","currency_cost":null,"budget_mode":"synthetic_token_envelope"})
}

/// An explicit independent selection record, not a rule that guesses semantics
/// from a predicate, first array item, quote substring, or sorted identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TargetMapping {
    pub schema_version: String,
    pub target_case: String,
    pub candidates: Vec<RevisionRef>,
    pub selected: Vec<RevisionRef>,
    pub selected_unit_sha256: String,
    pub selection_evidence_sha256: String,
    pub criterion: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StepPermit {
    pub schema_version: String,
    pub profile: String,
    pub instance_id: String,
    pub profile_id: String,
    pub plan_id: String,
    pub run_nonce: String,
    pub case_id: String,
    pub sequence: usize,
    pub request_sha256: String,
    pub context_sha256: String,
    pub target_mapping_sha256: String,
    pub prepared_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedStep {
    pub schema_version: String,
    pub profile: String,
    pub instance_id: String,
    pub profile_id: String,
    pub plan_id: String,
    pub run_nonce: String,
    pub case_id: String,
    pub sequence: usize,
    pub operation: String,
    pub fixture_only: bool,
    pub expected_simulated_dispatches: u64,
    pub model_input: Option<Value>,
    pub model_request: Option<Value>,
    pub context: Value,
    pub source: Option<RevisionRef>,
    pub target_mapping: Option<TargetMapping>,
    pub request_sha256: String,
    pub context_sha256: String,
    pub target_mapping_sha256: String,
    pub configuration_revision: Option<u64>,
}
impl PreparedStep {
    pub fn permit(&self) -> Result<StepPermit, NovaError> {
        Ok(StepPermit {
            schema_version: PROTOCOL.into(),
            profile: self.profile.clone(),
            instance_id: self.instance_id.clone(),
            profile_id: self.profile_id.clone(),
            plan_id: self.plan_id.clone(),
            run_nonce: self.run_nonce.clone(),
            case_id: self.case_id.clone(),
            sequence: self.sequence,
            request_sha256: self.request_sha256.clone(),
            context_sha256: self.context_sha256.clone(),
            target_mapping_sha256: self.target_mapping_sha256.clone(),
            prepared_sha256: hash_value(self)?,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureResponse {
    pub fixture_only: bool,
    pub content: String,
    pub usage: Option<FixtureUsage>,
    pub finish_reason: String,
    pub sse_done: bool,
    pub error_code: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub schema_version: String,
    pub case_id: String,
    pub sequence: usize,
    pub fixture_only: bool,
    pub simulated_dispatches: u64,
    pub real_model_dispatches: u64,
    pub semantic_acceptance: String,
    pub request_sha256: String,
    pub context_sha256: String,
    pub source: Option<RevisionRef>,
    pub output_versions: Vec<RevisionRef>,
    pub units: Vec<MemoryUnit>,
    pub compilation: Option<CommittedCompilation>,
    pub raw_output_sha256: Option<String>,
    pub resolved_output_sha256: Option<String>,
    pub quote_proofs: Value,
    pub usage: Option<FixtureUsage>,
    pub reserved_token_bound: u64,
    pub settled_fixture_tokens: u64,
    pub target_mapping: Option<TargetMapping>,
    pub deterministic_checks: Value,
}
#[derive(Clone)]
struct Target {
    receipt: IntakeReceipt,
    versions: Vec<RevisionRef>,
}
struct Pending {
    prepared: PreparedStep,
    attempt: Option<CompilationAttempt>,
    receipt: Option<IntakeReceipt>,
    mapping: Option<TargetMapping>,
    siblings: Vec<MemoryUnit>,
    started: Instant,
}
pub struct FixtureSuite {
    _directory: tempfile::TempDir,
    store: IntakeStore,
    scope: AssetScope,
    authority: IntakeAuthority,
    correction: CorrectionAuthority,
    plan_id: String,
    profile: FixtureProfile,
    instance_id: String,
    profile_id: String,
    nonce: String,
    next: usize,
    targets: BTreeMap<String, Target>,
    pending: Option<Pending>,
    aborted: bool,
    simulated_dispatches: u64,
    reserved: u64,
    settled: u64,
    started: Instant,
}
impl FixtureSuite {
    /// Creates an independent ephemeral synthetic core store only. No source paths,
    /// credentials, endpoints or user-supplied source text can be selected.
    pub fn new(plan_id: &str) -> Result<Self, NovaError> {
        Self::new_with_profile(plan_id, FixtureProfile::Legacy)
    }
    pub fn new_with_profile(plan_id: &str, profile: FixtureProfile) -> Result<Self, NovaError> {
        if !is_hash(plan_id) {
            return Err(error("plan_invalid"));
        }
        cases()?;
        let directory = tempfile::Builder::new()
            .prefix("nova-structured-suite-fixture-")
            .tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let profile_id = Uuid::now_v7().to_string();
        let scope = AssetScope {
            owner_id: "owner:synthetic-suite-fixture".into(),
            space: format!("suite-{profile_id}"),
        };
        let mut store = IntakeStore::create(
            &directory.path().join("intake.sqlite"),
            VaultKey::generate()?,
            scope.clone(),
        )?;
        store.configure_model(
            1,
            ModelSelection::Local {
                model_id: MODEL.into(),
                endpoint: ORIGIN.into(),
            },
        )?;
        store.enable_compilation(&scope)?;
        let authority = IntakeAuthority {
            scope: scope.clone(),
            actor_id: "actor:synthetic-suite-fixture".into(),
            capture_allowed: true,
            role: SourceRole::User,
            privacy_level: PrivacyLevel::Private,
        };
        let correction = CorrectionAuthority {
            scope: scope.clone(),
            actor_id: authority.actor_id.clone(),
            may_correct: true,
        };
        Ok(Self {
            _directory: directory,
            store,
            scope,
            authority,
            correction,
            plan_id: plan_id.into(),
            profile,
            instance_id: Uuid::now_v7().to_string(),
            profile_id,
            nonce: hash(Uuid::now_v7().as_bytes()),
            next: 0,
            targets: BTreeMap::new(),
            pending: None,
            aborted: false,
            simulated_dispatches: 0,
            reserved: 0,
            settled: 0,
            started: Instant::now(),
        })
    }
    pub fn ready(&self) -> Value {
        json!({"schema_version":PROTOCOL,"event":"ready","profile":self.profile.name(),"instance_id":self.instance_id,"profile_id":self.profile_id,"plan_id":self.plan_id,"run_nonce":self.nonce,"fixture_only":true,"real_model_dispatches":0})
    }
    fn live(&self) -> Result<(), NovaError> {
        if self.aborted || self.started.elapsed() > Duration::from_secs(1200) {
            Err(error("aborted_or_deadline"))
        } else {
            Ok(())
        }
    }
    pub fn prepare_next(
        &mut self,
        mapping: Option<TargetMapping>,
    ) -> Result<PreparedStep, NovaError> {
        let outcome = self.prepare_inner(mapping);
        if outcome.is_err() {
            self.aborted = true;
        }
        outcome
    }
    fn prepare_inner(&mut self, mapping: Option<TargetMapping>) -> Result<PreparedStep, NovaError> {
        self.live()?;
        if self.pending.is_some() || self.next >= 12 {
            return Err(error("step_order"));
        }
        let case = cases()?.remove(self.next);
        self.store.consolidate_pending(&self.scope, 4)?;
        let mut siblings = vec![];
        let target = if case.operation == "correct" {
            let m = mapping.as_ref().ok_or_else(|| error("target_ambiguous"))?;
            let entry = self
                .targets
                .get(
                    case.target_case
                        .as_ref()
                        .ok_or_else(|| error("catalog_target"))?,
                )
                .ok_or_else(|| error("target_missing"))?;
            if m.schema_version != "nova.acceptance.explicit-target-map.v0.1"
                || m.target_case != *case.target_case.as_ref().unwrap()
                || m.selected.len() != 1
                || m.candidates != entry.versions
                || !m.candidates.contains(&m.selected[0])
                || !is_hash(&m.selection_evidence_sha256)
                || m.criterion != "work_meeting_duration_constraint"
            {
                return Err(error("target_ambiguous"));
            }
            let selected = self
                .store
                .compiled_unit(&self.scope, &m.selected[0].object_id)?
                .unit;
            if selected.header.object != m.selected[0]
                || hash_value(&selected)? != m.selected_unit_sha256
            {
                return Err(error("target_snapshot_changed"));
            }
            for version in &entry.versions {
                if version != &m.selected[0] {
                    siblings.push(
                        self.store
                            .compiled_unit(&self.scope, &version.object_id)?
                            .unit,
                    );
                }
            }
            Some(m.selected[0].clone())
        } else {
            if mapping.is_some() {
                return Err(error("unexpected_target_mapping"));
            }
            None
        };
        let receipt = match case.operation.as_str() {
            "intake" => Some(self.store.receive(
                &self.authority,
                &serde_json::to_vec(&IntakeRequest {
                    schema_version: INTAKE_VERSION.into(),
                    request_id: format!("suite-fixture-{}", case.id),
                    source_platform: "synthetic-suite-fixture".into(),
                    capture_fidelity: CaptureFidelity::F2VisibleTranscript,
                    content: case.text.clone().ok_or_else(|| error("catalog_text"))?,
                    occurred_at: None,
                })?,
            )?),
            "correct" => Some(self.store.request_correction(
                &self.correction,
                target.unwrap(),
                format!("suite-fixture-{}", case.id),
                case.text.clone().unwrap(),
            )?),
            "undo" | "withdraw" => None,
            _ => return Err(error("catalog_operation")),
        };
        let attempt = if let Some(receipt) = &receipt {
            let job = self
                .store
                .claim_next(&self.scope, 150)?
                .ok_or_else(|| error("job_missing"))?;
            if job.job.job_id != receipt.job_id {
                return Err(error("job_mismatch"));
            }
            let lease = job.lease.ok_or_else(|| error("lease_missing"))?;
            Some(
                self.store
                    .prepare_compilation(&self.scope, &receipt.job_id, &lease.lease_id)?,
            )
        } else {
            None
        };
        let model_input = attempt.as_ref().map(CompilationAttempt::model_input);
        let request = model_input
            .as_ref()
            .map(|input| request_body_for(self.profile, input))
            .transpose()?;
        let context = if let Some(a) = &attempt {
            serde_json::to_value(&a.context)?
        } else {
            let t = self
                .targets
                .get(
                    case.target_case
                        .as_ref()
                        .ok_or_else(|| error("target_missing"))?,
                )
                .ok_or_else(|| error("target_missing"))?;
            let units = t
                .versions
                .iter()
                .map(|v| {
                    self.store
                        .compiled_unit(&self.scope, &v.object_id)
                        .map(|u| u.unit)
                })
                .collect::<Result<Vec<_>, _>>()?;
            json!({"source":t.receipt.source,"units":units})
        };
        let prepared = PreparedStep {
            schema_version: PROTOCOL.into(),
            profile: self.profile.name().into(),
            instance_id: self.instance_id.clone(),
            profile_id: self.profile_id.clone(),
            plan_id: self.plan_id.clone(),
            run_nonce: self.nonce.clone(),
            case_id: case.id.clone(),
            sequence: self.next + 1,
            operation: case.operation.clone(),
            fixture_only: true,
            expected_simulated_dispatches: case.model_calls,
            request_sha256: hash_value(&request)?,
            context_sha256: hash_value(&context)?,
            target_mapping_sha256: hash_value(&mapping)?,
            configuration_revision: attempt.as_ref().map(|a| a.configuration.revision),
            source: receipt.as_ref().map(|r| r.source.clone()),
            model_input,
            model_request: request,
            context,
            target_mapping: mapping.clone(),
        };
        self.pending = Some(Pending {
            prepared: prepared.clone(),
            attempt,
            receipt,
            mapping,
            siblings,
            started: Instant::now(),
        });
        Ok(prepared)
    }
    pub fn complete_fixture(
        &mut self,
        permit: &StepPermit,
        response: Option<FixtureResponse>,
    ) -> Result<StepResult, NovaError> {
        let outcome = self.complete_inner(permit, response);
        if outcome.is_err() {
            self.aborted = true;
        }
        outcome
    }
    fn complete_inner(
        &mut self,
        permit: &StepPermit,
        response: Option<FixtureResponse>,
    ) -> Result<StepResult, NovaError> {
        self.live()?;
        let pending = self
            .pending
            .take()
            .ok_or_else(|| error("step_not_prepared"))?;
        if permit != &pending.prepared.permit()?
            || pending.started.elapsed() > Duration::from_secs(120)
        {
            return Err(error("permit_binding_or_deadline"));
        }
        let case = cases()?.remove(self.next);
        let mut result = StepResult {
            schema_version: PROTOCOL.into(),
            case_id: case.id.clone(),
            sequence: self.next + 1,
            fixture_only: true,
            simulated_dispatches: self.simulated_dispatches,
            real_model_dispatches: 0,
            semantic_acceptance: "pending_independent_gold_and_review".into(),
            request_sha256: permit.request_sha256.clone(),
            context_sha256: permit.context_sha256.clone(),
            source: pending.receipt.as_ref().map(|r| r.source.clone()),
            output_versions: vec![],
            units: vec![],
            compilation: None,
            raw_output_sha256: None,
            resolved_output_sha256: None,
            quote_proofs: Value::Null,
            usage: None,
            reserved_token_bound: self.reserved,
            settled_fixture_tokens: self.settled,
            target_mapping: pending.mapping.clone(),
            deterministic_checks: json!({}),
        };
        if let Some(attempt) = pending.attempt {
            let response = response.ok_or_else(|| error("fixture_response_required"))?;
            if !response.fixture_only
                || self.simulated_dispatches >= 10
                || self.reserved + self.settled + RESERVATION > RESERVATION * 10
            {
                return Err(error("fixture_budget"));
            }
            let current = self.store.authorize_dispatch(
                &self.scope,
                &attempt.job_id,
                &attempt.lease_id,
                10,
            )?;
            if current.configuration != attempt.configuration
                || current.context != attempt.context
                || current.model_input() != attempt.model_input()
            {
                return Err(error("prepared_context_changed"));
            }
            self.simulated_dispatches += 1;
            self.reserved += RESERVATION;
            let outcome = (|| -> Result<CommittedCompilation, NovaError> {
                let usage = response
                    .usage
                    .as_ref()
                    .ok_or_else(|| error("usage_unknown_hold"))?;
                if usage.prompt_tokens > UPPER_INPUT
                    || usage.completion_tokens > UPPER_OUTPUT
                    || usage.prompt_tokens.checked_add(usage.completion_tokens)
                        != Some(usage.total_tokens)
                {
                    return Err(error("usage_outside_bound_hold"));
                }
                self.reserved -= RESERVATION;
                self.settled += usage.total_tokens;
                if response.error_code.is_some()
                    || response.finish_reason != "stop"
                    || !response.sse_done
                {
                    return Err(error("fixture_transport_incomplete"));
                }
                let source = attempt
                    .context
                    .sources
                    .first()
                    .ok_or_else(|| error("source_missing"))?;
                let resolve = match self.profile {
                    FixtureProfile::Legacy => quote_text::resolve,
                    FixtureProfile::ConditionList => quote_text::resolve_condition_list,
                };
                let resolved = resolve(&source.content, response.content.as_bytes())
                    .map_err(|_| error("quote_resolution_failed"))?;
                result.raw_output_sha256 = Some(resolved.raw_sha256().into());
                result.resolved_output_sha256 = Some(resolved.resolved_sha256().into());
                result.quote_proofs = serde_json::to_value(resolved.proofs())?;
                self.store.commit_compilation(
                    &self.scope,
                    &attempt.job_id,
                    &attempt.lease_id,
                    resolved.resolved_bytes(),
                )
            })();
            match outcome {
                Ok(commit) => {
                    result.output_versions = commit.output_versions.clone();
                    result.units = commit
                        .output_versions
                        .iter()
                        .map(|r| {
                            self.store
                                .compiled_unit(&self.scope, &r.object_id)
                                .map(|u| u.unit)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    for sibling in &pending.siblings {
                        if self
                            .store
                            .compiled_unit(&self.scope, &sibling.header.object.object_id)?
                            .unit
                            != *sibling
                        {
                            return Err(error("correction_changed_unselected_sibling"));
                        }
                    }
                    result.deterministic_checks = json!({"unselected_siblings_unchanged":true,"source_preserved":self.store.source(&self.scope,&pending.receipt.as_ref().unwrap().source.object_id)?.content==case.text.clone().unwrap()});
                    self.targets.insert(
                        case.id.clone(),
                        Target {
                            receipt: pending.receipt.unwrap(),
                            versions: commit.output_versions.clone(),
                        },
                    );
                    result.compilation = Some(commit);
                    result.usage = response.usage;
                }
                Err(e) => {
                    let failure = if self.reserved > 0 {
                        AttemptFailure::BudgetExceeded
                    } else if response.error_code.is_some() {
                        AttemptFailure::ModelUnavailable
                    } else {
                        AttemptFailure::InvalidOutput
                    };
                    let _ = self.store.fail_attempt(
                        &self.scope,
                        &attempt.job_id,
                        &attempt.lease_id,
                        failure,
                    );
                    return Err(e);
                }
            }
        } else {
            if response.is_some() {
                return Err(error("deterministic_step_no_response"));
            }
            let target = self
                .targets
                .get(case.target_case.as_ref().unwrap())
                .ok_or_else(|| error("target_missing"))?
                .clone();
            let current_context = json!({"source":target.receipt.source,"units":target.versions.iter().map(|v|self.store.compiled_unit(&self.scope,&v.object_id).map(|u|u.unit)).collect::<Result<Vec<_>,_>>()?});
            if hash_value(&current_context)? != permit.context_sha256 {
                return Err(error("deterministic_context_changed"));
            }
            if case.operation == "undo" {
                if target.versions.len() != 1 {
                    return Err(error("correction_requires_unique_target"));
                }
                let reference = &target.versions[0];
                let unit = self
                    .store
                    .undo_correction(&self.correction, &reference.object_id, reference.revision)?
                    .unit;
                let original = self
                    .targets
                    .get("negation_context")
                    .ok_or_else(|| error("prior_missing"))?
                    .versions
                    .iter()
                    .find(|r| r.object_id == reference.object_id)
                    .ok_or_else(|| error("prior_missing"))?;
                let history = self.store.unit_history(&self.scope, &reference.object_id)?;
                let before = history
                    .iter()
                    .find(|u| u.header.object == *original)
                    .ok_or_else(|| error("prior_missing"))?;
                if unit.content != before.content
                    || unit.valid_time != before.valid_time
                    || unit.origin != before.origin
                    || unit.header.object.revision <= reference.revision
                {
                    return Err(error("undo_not_restored"));
                }
                result.output_versions = vec![unit.header.object.clone()];
                result.units = vec![unit];
                result.deterministic_checks = json!({"prior_content_restored":true,"new_revision":true,"extra_model_calls":0});
            } else {
                let independent = self
                    .targets
                    .get("support_b")
                    .ok_or_else(|| error("parallel_source_missing"))?
                    .clone();
                self.store
                    .withdraw_source(&self.scope, &target.receipt.source.object_id)?;
                self.store.consolidate_pending(&self.scope, 4)?;
                if self
                    .store
                    .source(&self.scope, &target.receipt.source.object_id)
                    .is_ok()
                {
                    return Err(error("withdrawal_not_fenced"));
                }
                self.store
                    .source(&self.scope, &independent.receipt.source.object_id)?;
                for r in &target.versions {
                    if self.store.compiled_unit(&self.scope, &r.object_id).is_ok() {
                        return Err(error("withdrawal_unit_visible"));
                    }
                }
                for r in &independent.versions {
                    self.store.compiled_unit(&self.scope, &r.object_id)?;
                }
                result.deterministic_checks = json!({"source_immediately_fenced":true,"dependent_units_unavailable":true,"independent_source_survives":true,"extra_model_calls":0});
            }
        }
        self.next += 1;
        result.simulated_dispatches = self.simulated_dispatches;
        result.reserved_token_bound = self.reserved;
        result.settled_fixture_tokens = self.settled;
        Ok(result)
    }
    pub fn summary(&self) -> Value {
        json!({"schema_version":PROTOCOL,"profile":self.profile.name(),"fixture_only":true,"completed_steps":self.next,"simulated_dispatches":self.simulated_dispatches,"real_model_dispatches":0,"automatic_retries":0,"reserved_token_bound":self.reserved,"settled_fixture_tokens":self.settled,"aborted":self.aborted,"mechanical_fixture_complete":self.next==12&&!self.aborted,"real_full_suite_accepted":false,"semantic_acceptance":"pending_independent_gold_and_review","cost_total":null})
    }
    /// Fault injection only into this exclusively owned temporary synthetic store.
    pub fn withdraw_prepared_source_for_fixture(&mut self) -> Result<(), NovaError> {
        let p = self.pending.as_ref().ok_or_else(|| error("no_pending"))?;
        let r = p.receipt.as_ref().ok_or_else(|| error("no_source"))?;
        self.store.withdraw_source(&self.scope, &r.source.object_id)
    }
    pub fn change_configuration_for_fixture(&mut self) -> Result<(), NovaError> {
        let p = self.pending.as_ref().ok_or_else(|| error("no_pending"))?;
        let a = p.attempt.as_ref().ok_or_else(|| error("no_attempt"))?;
        self.store
            .configure_model(a.configuration.revision, ModelSelection::Unconfigured)?;
        Ok(())
    }
    /// Advance the currently prepared undo target as a concurrent owner action,
    /// exclusively in this temporary fixture store. Completion must reject its CAS.
    pub fn undo_prepared_target_for_fixture(&mut self) -> Result<(), NovaError> {
        let p = self.pending.as_ref().ok_or_else(|| error("no_pending"))?;
        if p.prepared.operation != "undo" {
            return Err(error("fixture_undo_target_required"));
        }
        let target = self
            .targets
            .get("correct_negation")
            .ok_or_else(|| error("no_target"))?;
        if target.versions.len() != 1 {
            return Err(error("fixture_undo_target_required"));
        }
        let r = &target.versions[0];
        self.store
            .undo_correction(&self.correction, &r.object_id, r.revision)?;
        Ok(())
    }
}
pub fn request_body(input: &Value) -> Result<Value, NovaError> {
    request_body_for(FixtureProfile::Legacy, input)
}

fn request_body_for(profile: FixtureProfile, input: &Value) -> Result<Value, NovaError> {
    let input_text = serde_json::to_string(input)?;
    let (_, prompt) = profile.prompt();
    let endpoint = format!("{ORIGIN}/v1");
    let (effort, expected) = match profile {
        FixtureProfile::Legacy => (
            "none",
            compiler::structured_probe_request_sha256_qwen_quote_text_v01(
                MODEL, &endpoint, &input_text, 120,
            )?,
        ),
        FixtureProfile::ConditionList => (
            "low",
            compiler::structured_probe_request_sha256_qwen_general_v10(
                MODEL, &endpoint, &input_text, 120,
            )?,
        ),
    };
    let request = json!({"model":MODEL,"stream":true,"temperature":0.7,"top_p":0.8,"max_tokens":2048,"reasoning_effort":effort,"stream_options":{"include_usage":true},"response_format":profile.response_format(),"messages":[{"role":"system","content":prompt},{"role":"user","content":input_text}]});
    if canonical(&request)?.len() > 131072 || hash_value(&request)? != expected {
        return Err(error("request_constructor_drift"));
    }
    Ok(request)
}
