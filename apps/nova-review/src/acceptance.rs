//! An isolated, finite JSONL acceptance driver; no Owner HTTP routes or background worker.
//! Monetary limits are a configured rate envelope, never a provider-account billing guarantee.
use crate::compiler;
use nova_core::{CaptureFidelity, NovaError, PrivacyLevel, VaultKey, compilation::*, intake::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, Read, Write},
    path::{Component, Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
use uuid::Uuid;
const CASES: &str = include_str!("../../../eval/model-acceptance/cases.json");
const MAX_LINE: usize = 512 * 1024;
const MAX_MODEL_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const PROTOCOL: &str = "nova.acceptance.execute.v0.1";
fn err(code: &'static str) -> NovaError {
    NovaError::Validation(format!("acceptance: {code}"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn id() -> String {
    Uuid::now_v7().to_string()
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn alias(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && s.len() <= 60
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn now() -> Result<String, NovaError> {
    nova_time::OffsetDateTime::now_utc()
        .format(&nova_time::format_description::well_known::Rfc3339)
        .map_err(|_| err("clock"))
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub maximum_dispatches: u64,
    pub maximum_seconds: u64,
    pub per_call_seconds: u64,
    pub maximum_input_bytes: u64,
    pub maximum_output_tokens: u64,
    pub currency: String,
    pub input_rate_micros_per_million: u64,
    pub output_rate_micros_per_million: u64,
    pub maximum_cost_micros: u64,
    pub maximum_charged_input_tokens: u64,
    pub maximum_charged_output_tokens: u64,
    pub pricing_attestation: String,
}
impl Budget {
    fn validate(&self) -> Result<(), NovaError> {
        if self.maximum_dispatches != 10
            || !(1..=1200).contains(&self.maximum_seconds)
            || !(1..=120).contains(&self.per_call_seconds)
            || self.per_call_seconds > self.maximum_seconds
            || !(1024..=131072).contains(&self.maximum_input_bytes)
            || !(1..=2048).contains(&self.maximum_output_tokens)
            || !matches!(self.currency.as_str(), "USD" | "CNY")
            || self.maximum_charged_input_tokens < self.maximum_input_bytes + 128
            || self.maximum_charged_input_tokens > 1_000_000
            || self.maximum_charged_output_tokens < self.maximum_output_tokens
            || self.maximum_charged_output_tokens > 65536
            || self.input_rate_micros_per_million > 1_000_000_000
            || self.output_rate_micros_per_million > 1_000_000_000
            || self.maximum_cost_micros > 1_000_000_000_000
            || self.pricing_attestation != "configured_envelope_includes_all_billed_tokens"
        {
            return Err(err("budget_invalid"));
        }
        Ok(())
    }
    fn lease_seconds(&self) -> u32 {
        // Validated acceptance budgets cap this at 150 seconds. Keep a commit margin
        // beyond the provider deadline; ordinary worker lease policy is unchanged.
        (self.per_call_seconds + 30) as u32
    }
    fn cost(&self, input: u64, output: u64) -> u64 {
        let n = input as u128 * self.input_rate_micros_per_million as u128
            + output as u128 * self.output_rate_micros_per_million as u128;
        n.div_ceil(1_000_000) as u64
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    command: String,
    schema_version: String,
    plan_id: String,
    cases_sha256: String,
    prompt_sha256: String,
    budget: Budget,
    selection: ModelSelection,
    provider_alias: String,
    model_alias: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    command: String,
    case_id: String,
    sequence: usize,
    run_nonce: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    command: String,
    run_nonce: String,
}
#[derive(Deserialize)]
struct Catalog {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    operation: String,
    text: Option<String>,
    target_case: Option<String>,
    model_calls: u64,
}
#[derive(Default)]
struct Ledger {
    dispatches: u64,
    reserved: u64,
    settled: u64,
}
impl Ledger {
    fn reserve(&mut self, budget: &Budget) -> Result<u64, NovaError> {
        let cost = budget.cost(
            budget.maximum_charged_input_tokens,
            budget.maximum_charged_output_tokens,
        );
        if self.dispatches >= budget.maximum_dispatches
            || self.settled as u128 + self.reserved as u128 + cost as u128
                > budget.maximum_cost_micros as u128
        {
            return Err(err("budget_envelope_exhausted"));
        }
        self.reserved += cost;
        self.dispatches += 1;
        Ok(cost)
    }
    fn settle(&mut self, budget: &Budget, reserve: u64, usage: &Usage) -> Result<(), NovaError> {
        let input = usage
            .prompt_tokens
            .ok_or_else(|| err("provider_usage_missing"))?;
        let output = usage
            .completion_tokens
            .ok_or_else(|| err("provider_usage_missing"))?;
        if input > budget.maximum_charged_input_tokens
            || output > budget.maximum_charged_output_tokens
        {
            return Err(err("provider_usage_outside_envelope"));
        }
        self.reserved -= reserve;
        self.settled += budget.cost(input, output);
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<InputDetails>,
}
#[derive(Deserialize, Serialize)]
struct InputDetails {
    cached_tokens: Option<u64>,
}
#[derive(Deserialize)]
struct UsageEnvelope {
    usage: Option<Usage>,
}
fn usage(raw: &[u8]) -> Option<Usage> {
    let envelope: UsageEnvelope = serde_json::from_slice(raw).ok()?;
    let u = envelope.usage?;
    let input = u.prompt_tokens?;
    let output = u.completion_tokens?;
    let cached = u
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    if cached > input
        || u.total_tokens
            .is_some_and(|n| input.checked_add(output) != Some(n))
    {
        return None;
    }
    Some(u)
}
fn usage_metadata(u: Option<&Usage>) -> Value {
    if let Some(u) = u {
        if let Some(cached) = u
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
        {
            json!({"status":"observed","input_total_tokens":null,"input_uncached_tokens":(u.prompt_tokens.unwrap()-cached).to_string(),"input_cached_tokens":cached.to_string(),"output_tokens":u.completion_tokens.unwrap().to_string()})
        } else {
            json!({"status":"partial","input_total_tokens":u.prompt_tokens.unwrap().to_string(),"input_uncached_tokens":null,"input_cached_tokens":null,"output_tokens":u.completion_tokens.unwrap().to_string()})
        }
    } else {
        json!({"status":"missing","input_total_tokens":null,"input_uncached_tokens":null,"input_cached_tokens":null,"output_tokens":null})
    }
}
#[derive(Deserialize)]
struct DiagnosticResponse {
    choices: Vec<DiagnosticChoice>,
}
#[derive(Deserialize)]
struct DiagnosticChoice {
    message: DiagnosticMessage,
}
#[derive(Deserialize)]
struct DiagnosticMessage {
    content: String,
}
/// Only the selected message content is retained, never the provider body or unknown fields.
/// The private synthetic-run report is evidence for review, not a semantic acceptance result.
fn model_diagnostic(raw: &[u8]) -> Value {
    let mut diagnostic = json!({"mechanical_only":true,"model_output_utf8":null,
        "model_output_bytes":null,"model_output_sha256":null,"truncated":false});
    let Ok(response) = serde_json::from_slice::<DiagnosticResponse>(raw) else {
        return diagnostic;
    };
    if response.choices.len() != 1 {
        return diagnostic;
    }
    let content = &response.choices[0].message.content;
    let mut end = content.len().min(MAX_MODEL_DIAGNOSTIC_BYTES);
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    diagnostic["model_output_utf8"] = json!(&content[..end]);
    diagnostic["model_output_bytes"] = json!(content.len());
    diagnostic["model_output_sha256"] = json!(hash(content.as_bytes()));
    diagnostic["truncated"] = json!(end != content.len());
    diagnostic
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailurePhase {
    Transport,
    Usage,
    Decode,
    Commit,
}
fn source_hashes() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("crates/nova-model-runtime/Cargo.toml", hash(include_bytes!("../../../crates/nova-model-runtime/Cargo.toml"))),
        ("crates/nova-model-runtime/src/lib.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/lib.rs"))),
        ("crates/nova-model-runtime/src/compiler.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/compiler.rs"))),
        ("crates/nova-model-runtime/src/streaming_compiler.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/streaming_compiler.rs"))),
        ("crates/nova-model-runtime/src/provider_capabilities.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/provider_capabilities.rs"))),
        ("crates/nova-model-runtime/src/compiler_schema.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/compiler_schema.rs"))),
        ("crates/nova-model-runtime/src/quote_text_schema.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/quote_text_schema.rs"))),
        ("crates/nova-model-runtime/src/generation_guard.rs", hash(include_bytes!("../../../crates/nova-model-runtime/src/generation_guard.rs"))),
        (
            "scripts/model-acceptance.py",
            hash(include_bytes!("../../../scripts/model-acceptance.py")),
        ),
        (
            "scripts/n2a-real-model-acceptance.py",
            hash(include_bytes!(
                "../../../scripts/n2a-real-model-acceptance.py"
            )),
        ),
        (
            "apps/nova-review/src/compiler-prompt.txt",
            hash(compiler::SYSTEM_PROMPT.as_bytes()),
        ),
        (
            "apps/nova-review/src/compiler.rs",
            hash(include_bytes!("compiler.rs")),
        ),
        (
            "crates/nova-model-runtime/src/compiler_metering.rs",
            hash(include_bytes!("../../../crates/nova-model-runtime/src/compiler_metering.rs")),
        ),
        (
            "crates/nova-core/src/intake/metering.rs",
            hash(include_bytes!("../../../crates/nova-core/src/intake/metering.rs")),
        ),
        (
            "crates/nova-core/src/intake/capacity.rs",
            hash(include_bytes!("../../../crates/nova-core/src/intake/capacity.rs")),
        ),
        (
            "crates/nova-core/src/intake/compilation_capacity.rs",
            hash(include_bytes!("../../../crates/nova-core/src/intake/compilation_capacity.rs")),
        ),
        (
            "crates/nova-core/src/intake/storage.rs",
            hash(include_bytes!("../../../crates/nova-core/src/intake/storage.rs")),
        ),
        (
            "apps/nova-review/src/provider_capabilities.rs",
            hash(include_bytes!("provider_capabilities.rs")),
        ),
        (
            "apps/nova-review/src/intake.rs",
            hash(include_bytes!("intake.rs")),
        ),
        (
            "crates/nova-core/src/intake/compiler.rs",
            hash(include_bytes!(
                "../../../crates/nova-core/src/intake/compiler.rs"
            )),
        ),
        (
            "crates/nova-core/src/intake/consolidation.rs",
            hash(include_bytes!(
                "../../../crates/nova-core/src/intake/consolidation.rs"
            )),
        ),
        (
            "apps/nova-review/src/acceptance.rs",
            hash(include_bytes!("acceptance.rs")),
        ),
        (
            "apps/nova-review/src/bin/nova-review-acceptance.rs",
            hash(include_bytes!("bin/nova-review-acceptance.rs")),
        ),
        (
            "apps/nova-review/Cargo.toml",
            hash(include_bytes!("../Cargo.toml")),
        ),
    ])
}
/// Metadata is available without initializing a profile or opening any network connection.
pub fn metadata() -> Value {
    json!({"schema_version":PROTOCOL,"cases_sha256":hash(CASES.as_bytes()),"prompt_sha256":hash(compiler::SYSTEM_PROMPT.as_bytes()),"compiled_source_hashes":source_hashes(),"remote_supported":false,"provider_billing_hard_cap":false})
}
fn private_profile(path: &Path) -> Result<File, NovaError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(err("new_absolute_profile_required"));
    }
    let mut current = PathBuf::new();
    let components = path.components().collect::<Vec<_>>();
    for c in &components[..components.len() - 1] {
        current.push(c.as_os_str());
        let meta = std::fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(err("profile_ancestor_invalid"));
        }
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path.join("exclusive-session.lock"))?;
    file.try_lock()
        .map_err(|_| err("exclusive_session_unavailable"))?;
    // This profile has a single writer and no network-facing Owner interface. Lock sentinel is
    // never removed; existing profiles cannot resume or be attached by another acceptance process.
    Ok(file)
}
fn checked_binary_hash() -> Result<String, NovaError> {
    let path = std::env::current_exe()?;
    let mut file = File::open(path)?;
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        return Err(err("binary_size"));
    }
    let mut sha = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
fn emit(value: &Value) -> Result<(), NovaError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_LINE {
        return Err(err("output_limit"));
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
fn input() -> mpsc::Receiver<Result<Vec<u8>, ()>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut line = Vec::new();
            let outcome = loop {
                let buf = match reader.fill_buf() {
                    Ok(b) => b,
                    Err(_) => break Err(()),
                };
                if buf.is_empty() {
                    break if line.is_empty() {
                        return;
                    } else {
                        Err(())
                    };
                }
                let n = buf
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(buf.len(), |i| i + 1);
                if line.len() + n > MAX_LINE {
                    break Err(());
                }
                line.extend_from_slice(&buf[..n]);
                reader.consume(n);
                if line.last() == Some(&b'\n') {
                    break Ok(line);
                }
            };
            let failed = outcome.is_err();
            if tx.send(outcome).is_err() || failed {
                return;
            }
        }
    });
    rx
}
fn read_command(
    rx: &mpsc::Receiver<Result<Vec<u8>, ()>>,
    wait: Duration,
) -> Result<Vec<u8>, NovaError> {
    rx.recv_timeout(wait)
        .map_err(|_| err("input_eof_or_deadline"))?
        .map_err(|_| err("input_limit"))
}
struct Target {
    receipt: IntakeReceipt,
    unit: Option<RevisionRef>,
}
/// No real model is invoked unless a parent sends a validated explicit begin and then each step.
pub fn serve(profile: PathBuf) -> Result<(), NovaError> {
    let rx = input();
    let first = read_command(&rx, Duration::from_secs(30))?;
    let begin: Begin = serde_json::from_slice(&first).map_err(|_| err("begin_invalid"))?;
    if begin.command != "begin"
        || begin.schema_version != PROTOCOL
        || !digest(&begin.plan_id)
        || begin.cases_sha256 != hash(CASES.as_bytes())
        || begin.prompt_sha256 != hash(compiler::SYSTEM_PROMPT.as_bytes())
        || !alias(&begin.provider_alias)
        || !alias(&begin.model_alias)
    {
        return Err(err("binding_invalid"));
    }
    begin.budget.validate()?;
    if !matches!(begin.selection, ModelSelection::Local { .. }) {
        return Err(err("remote_acceptance_not_supported"));
    }
    ModelConfiguration {
        revision: 1,
        selection: begin.selection.clone(),
    }
    .validate()?;
    let binary_hash = checked_binary_hash()?;
    let _exclusive = private_profile(&profile)?;
    let profile_meta = std::fs::symlink_metadata(&profile)?;
    #[cfg(unix)]
    let profile_identity = {
        use std::os::unix::fs::MetadataExt;
        (profile_meta.dev(), profile_meta.ino())
    };
    let key = VaultKey::generate()?;
    key.write_new_file(&profile.join("vault.key"))?;
    let run_id = id();
    let instance_id = id();
    let profile_id = id();
    let nonce = hash(format!("{}{}", id(), id()).as_bytes());
    let scope = AssetScope {
        owner_id: "owner:synthetic-acceptance".into(),
        space: format!("acceptance-{profile_id}"),
    };
    let mut store = IntakeStore::create(&profile.join("intake.sqlite"), key, scope.clone())?;
    if !store.jobs(&scope, None)?.is_empty() {
        return Err(err("fresh_inventory_failed"));
    }
    store.configure_model(1, begin.selection.clone())?;
    store.enable_compilation(&scope)?;
    let authority = IntakeAuthority {
        scope: scope.clone(),
        actor_id: "actor:synthetic-acceptance".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    };
    let correction = CorrectionAuthority {
        scope: scope.clone(),
        actor_id: authority.actor_id.clone(),
        may_correct: true,
    };
    let started = Instant::now();
    let total = Duration::from_secs(begin.budget.maximum_seconds);
    let mut ledger = Ledger::default();
    let catalog: Catalog = serde_json::from_str(CASES)?;
    let mut targets: BTreeMap<String, Target> = BTreeMap::new();
    let mut bound = metadata();
    for (k,v) in json!({"event":"bound","run_id":run_id,"instance_id":instance_id,"profile_id":profile_id,"run_nonce":nonce,"plan_id":begin.plan_id,"binary_sha256":binary_hash,"fresh_inventory":true,"dispatches":0,"budget":begin.budget,"selection":begin.selection,"provider_alias":begin.provider_alias,"model_alias":begin.model_alias,"scope":"synthetic_only","cost_basis":"configured_rate_envelope_estimate"}).as_object().unwrap(){bound[k]=v.clone();}
    emit(&bound)?;
    for (index, case) in catalog.cases.iter().enumerate() {
        let remaining = total
            .checked_sub(started.elapsed())
            .ok_or_else(|| err("total_deadline"))?;
        let command = read_command(&rx, remaining)?;
        if started.elapsed() >= total {
            return Err(err("total_deadline"));
        }
        let step: Step = serde_json::from_slice(&command).map_err(|_| err("step_invalid"))?;
        if step.command != "step"
            || step.sequence != index + 1
            || step.case_id != case.id
            || step.run_nonce != nonce
        {
            return Err(err("step_binding_invalid"));
        }
        let meta = std::fs::symlink_metadata(&profile)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.file_type().is_symlink() || (meta.dev(), meta.ino()) != profile_identity {
                return Err(err("profile_identity_changed"));
            }
        }
        let mut event = json!({"event":"step_completed","run_id":run_id,"instance_id":instance_id,"case_id":case.id,"sequence":index+1,"status":"pass","dispatches":ledger.dispatches,"attempt":null,"semantic_acceptance":"pending_independent_gold_and_review","provider_billing_hard_cap":false});
        let result = (|| -> Result<(), NovaError> {
            store.consolidate_pending(&scope, 4)?;
            if case.operation == "undo" {
                let target = targets
                    .get(case.target_case.as_ref().unwrap())
                    .ok_or_else(|| err("target_missing"))?;
                let unit = target
                    .unit
                    .as_ref()
                    .ok_or_else(|| err("single_target_required"))?;
                let revised = store.undo_correction(&correction, &unit.object_id, unit.revision)?;
                event["semantic_observation"] =
                    json!({"mechanical_only":true,"units":[revised.unit],"model_output_utf8":null});
                return Ok(());
            }
            if case.operation == "withdraw" {
                let target = targets
                    .get(case.target_case.as_ref().unwrap())
                    .ok_or_else(|| err("target_missing"))?;
                store.withdraw_source(&scope, &target.receipt.source.object_id)?;
                store.consolidate_pending(&scope, 4)?;
                event["semantic_observation"] = json!({"mechanical_only":true,"units":store.compiled_units(&scope,None)?.into_iter().map(|(_,u)|u.unit).collect::<Vec<_>>(),"model_output_utf8":null});
                return Ok(());
            }
            if case.model_calls != 1 {
                return Err(err("catalog_dispatch_mismatch"));
            }
            let receipt = if case.operation == "intake" {
                store.receive(
                    &authority,
                    &serde_json::to_vec(&IntakeRequest {
                        schema_version: INTAKE_VERSION.into(),
                        request_id: format!("acceptance-{}", case.id),
                        source_platform: "synthetic-acceptance".into(),
                        capture_fidelity: CaptureFidelity::F2VisibleTranscript,
                        content: case.text.clone().unwrap(),
                        occurred_at: None,
                    })?,
                )?
            } else if case.operation == "correct" {
                let target = targets
                    .get(case.target_case.as_ref().unwrap())
                    .and_then(|t| t.unit.clone())
                    .ok_or_else(|| err("single_target_required"))?;
                store.request_correction(
                    &correction,
                    target,
                    format!("acceptance-{}", case.id),
                    case.text.clone().unwrap(),
                )?
            } else {
                return Err(err("catalog_operation"));
            };
            let job = store
                .claim_next(&scope, begin.budget.lease_seconds())?
                .ok_or_else(|| err("job_missing"))?;
            if job.job.job_id != receipt.job_id {
                return Err(err("job_identity_mismatch"));
            }
            let lease = &job
                .lease
                .as_ref()
                .ok_or_else(|| err("lease_missing"))?
                .lease_id;
            let prepared = store.prepare_compilation(&scope, &receipt.job_id, lease)?;
            let remaining = total
                .checked_sub(started.elapsed())
                .ok_or_else(|| err("total_deadline"))?
                .as_secs();
            let timeout = remaining.min(begin.budget.per_call_seconds);
            if timeout == 0 {
                return Err(err("total_deadline"));
            }
            let transport = compiler::acceptance_transport(
                &prepared,
                begin.budget.maximum_output_tokens,
                timeout,
            )?;
            if transport.input_bytes() as u64 > begin.budget.maximum_input_bytes {
                return Err(err("input_byte_budget"));
            }
            let reservation = ledger.reserve(&begin.budget)?;
            let attempt = store.authorize_dispatch(&scope, &receipt.job_id, lease, 10)?;
            if attempt.configuration != prepared.configuration {
                return Err(err("configuration_changed"));
            }
            let dispatched_at = now()?;
            emit(
                &json!({"event":"dispatch_started","run_id":run_id,"instance_id":instance_id,"case_id":case.id,"sequence":index+1,"dispatches":ledger.dispatches,"envelope_reserved_micros":ledger.reserved,"estimated_settled_cost_micros":ledger.settled,"provider_billing_hard_cap":false,"attempt":{"attempt_id":format!("attempt-{:04}",ledger.dispatches),"task_id":case.id,"parent_attempt_id":null,"provider_id":begin.provider_alias,"model_id":begin.model_alias,"kind":"normal","status":"unknown","dispatched_at":dispatched_at,"completed_at":null,"usage":usage_metadata(None),"usage_evidence":{"kind":"unavailable","sha256":null}}}),
            )?;
            let raw = compiler::send_raw_diagnostic(transport).map_err(|failure| {
                event["transport_diagnostic"] = json!(failure.diagnostic);
                failure.error
            });
            let completed_at = now()?;
            if let Ok(body) = &raw {
                event["model_diagnostic"] = model_diagnostic(body);
            }
            let observed = raw.as_ref().ok().and_then(|b| usage(b));
            let usage_value = usage_metadata(observed.as_ref());
            let evidence = if let Some(u) = &observed {
                json!({"kind":"provider_usage_metadata","sha256":hash(&serde_json::to_vec(u)?)})
            } else {
                json!({"kind":"unavailable","sha256":null})
            };
            let mut observation = json!({"attempt_id":format!("attempt-{:04}",ledger.dispatches),"task_id":case.id,"parent_attempt_id":null,"provider_id":begin.provider_alias,"model_id":begin.model_alias,"kind":"normal","status":"failed","dispatched_at":dispatched_at,"completed_at":completed_at,"usage":usage_value,"usage_evidence":evidence});
            let mut phase = FailurePhase::Transport;
            let outcome = (|| -> Result<(), NovaError> {
                let raw = raw?;
                phase = FailurePhase::Usage;
                let u = observed
                    .as_ref()
                    .ok_or_else(|| err("provider_usage_missing"))?;
                ledger.settle(&begin.budget, reservation, u)?;
                phase = FailurePhase::Decode;
                let output = compiler::decode_response(&raw)?;
                // The transaction revalidates this same schema. Parsing here only identifies
                // a schema refusal separately from quote/context/transaction validation.
                ExtractionOutput::parse(&output)?;
                if started.elapsed() >= total {
                    return Err(err("total_deadline"));
                }
                phase = FailurePhase::Commit;
                let committed =
                    store.commit_compilation(&scope, &receipt.job_id, lease, &output)?;
                let units = committed
                    .output_versions
                    .iter()
                    .map(|r| store.compiled_unit(&scope, &r.object_id).map(|u| u.unit))
                    .collect::<Result<Vec<_>, _>>()?;
                let unit = if committed.output_versions.len() == 1 {
                    Some(committed.output_versions[0].clone())
                } else {
                    None
                };
                targets.insert(
                    case.id.clone(),
                    Target {
                        receipt: receipt.clone(),
                        unit,
                    },
                );
                event["semantic_observation"] = json!({"mechanical_only":true,"units":units,"model_output_utf8":String::from_utf8(output).map_err(|_|err("model_utf8"))?});
                Ok(())
            })();
            if outcome.is_ok() {
                observation["status"] = json!("success");
            } else if let Err(error) = &outcome {
                event["execution_failure"] = json!({"mechanical_only":true,"phase":phase,
                    "error_code":error.code()});
                let _ = store.fail_attempt(
                    &scope,
                    &receipt.job_id,
                    lease,
                    AttemptFailure::WorkerInterrupted,
                );
            }
            event["attempt"] = observation;
            outcome
        })();
        event["dispatches"] = json!(ledger.dispatches);
        event["envelope_reserved_micros"] = json!(ledger.reserved);
        event["estimated_settled_cost_micros"] = json!(ledger.settled);
        event["currency"] = json!(begin.budget.currency);
        if result.is_err() {
            event["status"] = json!("fail");
            event["stop_reason"] = json!("step_failed_budget_or_model_or_state");
            emit(&event)?;
            emit(
                &json!({"event":"aborted","run_id":run_id,"instance_id":instance_id,"dispatches":ledger.dispatches,"envelope_reserved_micros":ledger.reserved,"estimated_settled_cost_micros":ledger.settled,"provider_billing_hard_cap":false,"real_model_semantic_acceptance":false}),
            )?;
            return Err(err("step_failed"));
        }
        emit(&event)?;
    }
    let remaining = total
        .checked_sub(started.elapsed())
        .ok_or_else(|| err("total_deadline"))?;
    let finish: Finish = serde_json::from_slice(&read_command(&rx, remaining)?)
        .map_err(|_| err("finish_invalid"))?;
    if finish.command != "finish" || finish.run_nonce != nonce || started.elapsed() >= total {
        return Err(err("finish_binding_invalid"));
    }
    emit(
        &json!({"event":"finished","status":"pass","run_id":run_id,"instance_id":instance_id,"dispatches":ledger.dispatches,"envelope_reserved_micros":ledger.reserved,"estimated_settled_cost_micros":ledger.settled,"real_model_semantic_acceptance":false,"semantic_acceptance":"pending_independent_gold_and_review","provider_billing_hard_cap":false}),
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn budget() -> Budget {
        Budget {
            maximum_dispatches: 10,
            maximum_seconds: 600,
            per_call_seconds: 45,
            maximum_input_bytes: 131072,
            maximum_output_tokens: 2048,
            currency: "USD".into(),
            input_rate_micros_per_million: 1_000_000,
            output_rate_micros_per_million: 2_000_000,
            maximum_cost_micros: 2_000_000,
            maximum_charged_input_tokens: 131200,
            maximum_charged_output_tokens: 2048,
            pricing_attestation: "configured_envelope_includes_all_billed_tokens".into(),
        }
    }
    #[test]
    fn local_deadline_is_bounded_and_lease_covers_call_plus_commit() {
        let mut b = budget();
        assert!(b.validate().is_ok());
        assert_eq!(b.lease_seconds(), 75);
        b.maximum_seconds = 1200;
        b.per_call_seconds = 120;
        assert!(b.validate().is_ok());
        assert_eq!(b.lease_seconds(), 150);
        b.per_call_seconds = 121;
        assert!(b.validate().is_err());
        b.per_call_seconds = 120;
        b.maximum_seconds = 1201;
        assert!(b.validate().is_err());
        b.maximum_seconds = 119;
        assert!(b.validate().is_err());
    }
    #[test]
    fn ten_calls_cannot_be_confused_with_currency_budget() {
        let b = budget();
        let mut l = Ledger::default();
        for _ in 0..10 {
            let r = l.reserve(&b).unwrap();
            l.settle(
                &b,
                r,
                &Usage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    total_tokens: Some(2),
                    prompt_tokens_details: None,
                },
            )
            .unwrap();
        }
        assert!(l.reserve(&b).is_err());
        assert_eq!(l.dispatches, 10);
    }
    #[test]
    fn monetary_reservation_is_before_dispatch_and_unknown_usage_is_held() {
        let mut b = budget();
        b.maximum_cost_micros = 1;
        let mut l = Ledger::default();
        assert!(l.reserve(&b).is_err());
        assert_eq!(l.dispatches, 0);
        b.maximum_cost_micros = 2_000_000;
        let r = l.reserve(&b).unwrap();
        let missing = Usage {
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            prompt_tokens_details: None,
        };
        assert!(l.settle(&b, r, &missing).is_err());
        assert_eq!(l.reserved, r);
    }
    #[test]
    fn excessive_usage_retains_full_reservation() {
        let b = budget();
        let mut l = Ledger::default();
        let r = l.reserve(&b).unwrap();
        assert!(
            l.settle(
                &b,
                r,
                &Usage {
                    prompt_tokens: Some(1_000_001),
                    completion_tokens: Some(0),
                    total_tokens: None,
                    prompt_tokens_details: None
                }
            )
            .is_err()
        );
        assert_eq!(l.reserved, r);
        assert_eq!(l.settled, 0);
    }
    #[test]
    fn usage_rejects_overlap_inconsistent_total_duplicate_and_negative() {
        for raw in [
            r#"{"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":9}}"#,
            r#"{"usage":{"prompt_tokens":2,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":3}}}"#,
            r#"{"usage":{"prompt_tokens":2,"prompt_tokens":3,"completion_tokens":1}}"#,
            r#"{"usage":{"prompt_tokens":-1,"completion_tokens":1}}"#,
        ] {
            assert!(usage(raw.as_bytes()).is_none());
        }
    }
    #[test]
    fn usage_is_exclusive_and_evidence_contains_no_body() {
        let u=usage(br#"{"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"prompt_tokens_details":{"cached_tokens":3}},"choices":[{"secret":"ignored"}]}"#).unwrap();
        assert_eq!(
            usage_metadata(Some(&u)),
            json!({"status":"observed","input_total_tokens":null,"input_uncached_tokens":"7","input_cached_tokens":"3","output_tokens":"2"})
        );
        let partial = usage(br#"{"usage":{"prompt_tokens":10,"completion_tokens":2}}"#).unwrap();
        assert_eq!(
            usage_metadata(Some(&partial))["input_cached_tokens"],
            Value::Null
        );
        assert_eq!(usage_metadata(Some(&partial))["input_total_tokens"], "10");
    }
    #[test]
    fn model_diagnostic_bounds_utf8_and_excludes_provider_fields() {
        let content = "界".repeat(MAX_MODEL_DIAGNOSTIC_BYTES);
        let raw = serde_json::to_vec(&json!({"choices":[{"message":{"content":content,"private":"provider-secret"}}],"private":"provider-secret"})).unwrap();
        let diagnostic = model_diagnostic(&raw);
        let retained = diagnostic["model_output_utf8"].as_str().unwrap();
        assert!(retained.len() <= MAX_MODEL_DIAGNOSTIC_BYTES);
        assert!(retained.chars().all(|c| c == '界'));
        assert_eq!(diagnostic["truncated"], true);
        assert_eq!(diagnostic["model_output_bytes"], content.len());
        assert_eq!(diagnostic["model_output_sha256"], hash(content.as_bytes()));
        assert!(!diagnostic.to_string().contains("provider-secret"));
        for raw in [
            br#"{"choices":[{"message":{"content":"one","content":"two"}}]}"#.as_slice(),
            br#"{"choices":[{"message":{"content":"one"}},{"message":{"content":"two"}}]}"#
                .as_slice(),
        ] {
            assert!(model_diagnostic(raw)["model_output_utf8"].is_null());
        }
    }
    #[test]
    fn profile_is_create_new_and_rejects_symlink_ancestors() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let p = root.join("profile");
        let _f = private_profile(&p).unwrap();
        assert!(private_profile(&p).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.path(), d.path().join("link")).unwrap();
            assert!(private_profile(&root.join("link/new")).is_err());
        }
    }
    #[test]
    fn metadata_fixes_sources_prompt_and_synthetic_catalog() {
        let m = metadata();
        assert_eq!(m["remote_supported"], false);
        assert_eq!(m["compiled_source_hashes"].as_object().unwrap().len(), 24);
        for path in ["crates/nova-model-runtime/Cargo.toml", "crates/nova-model-runtime/src/lib.rs",
            "crates/nova-model-runtime/src/compiler.rs", "crates/nova-model-runtime/src/compiler_metering.rs",
            "crates/nova-model-runtime/src/streaming_compiler.rs", "crates/nova-model-runtime/src/generation_guard.rs",
            "crates/nova-model-runtime/src/compiler_schema.rs", "crates/nova-model-runtime/src/provider_capabilities.rs",
            "crates/nova-model-runtime/src/quote_text_schema.rs"] {
            assert!(m["compiled_source_hashes"][path].as_str().is_some_and(|s|s.len()==64));
        }
        let c: Catalog = serde_json::from_str(CASES).unwrap();
        assert_eq!(c.cases.len(), 12);
        assert_eq!(c.cases.iter().map(|c| c.model_calls).sum::<u64>(), 10);
    }
    #[test]
    fn strict_budget_rejects_unsupported_caps() {
        let mut b = budget();
        b.maximum_dispatches = 11;
        assert!(b.validate().is_err());
        b.maximum_dispatches = 10;
        b.maximum_charged_input_tokens = 1;
        assert!(b.validate().is_err());
        b = budget();
        b.currency = "EUR".into();
        assert!(b.validate().is_err());
    }
}
