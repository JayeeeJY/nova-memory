//! One immutable synthetic case through the real structured adapter and core transaction.
//! The JSONL handshake binds the current process, binary, source request and fresh profile.
use crate::{compiler, compiler_schema, quote_text, quote_text_schema};
use nova_core::{CaptureFidelity, NovaError, PrivacyLevel, VaultKey, compilation::*, intake::*};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, Read, Write},
    sync::mpsc,
    time::Duration,
};
use uuid::Uuid;

const PROTOCOL: &str = "nova.structured.acceptance.v0.1";
#[derive(Clone, Copy)]
enum Profile {
    LlamaV02,
    QwenV02,
    QwenQuoteTextV01,
    QwenQuoteTextV02,
    QwenQuoteTextV03,
    QwenQuoteTextV04,
    QwenQuoteTextV05,
    QwenGeneral(&'static str),
}
impl Profile {
    fn parse(name: &str) -> Result<Self, NovaError> {
        match name {
            "llama-v02" => Ok(Self::LlamaV02),
            "qwen-v02" => Ok(Self::QwenV02),
            "qwen-quote-text-v01" => Ok(Self::QwenQuoteTextV01),
            "qwen-quote-text-v02" => Ok(Self::QwenQuoteTextV02),
            "qwen-quote-text-v03" => Ok(Self::QwenQuoteTextV03),
            "qwen-quote-text-v04" => Ok(Self::QwenQuoteTextV04),
            "qwen-quote-text-v05" => Ok(Self::QwenQuoteTextV05),
            _ => GENERAL_CASES
                .iter()
                .copied()
                .find(|case| *case == name)
                .map(Self::QwenGeneral)
                .ok_or_else(|| error("profile_invalid")),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::LlamaV02 => "llama-v02",
            Self::QwenV02 => "qwen-v02",
            Self::QwenQuoteTextV01 => "qwen-quote-text-v01",
            Self::QwenQuoteTextV02 => "qwen-quote-text-v02",
            Self::QwenQuoteTextV03 => "qwen-quote-text-v03",
            Self::QwenQuoteTextV04 => "qwen-quote-text-v04",
            Self::QwenQuoteTextV05 => "qwen-quote-text-v05",
            Self::QwenGeneral(case) => case,
        }
    }
    fn case_id(self) -> &'static str {
        match self {
            Self::QwenGeneral(case) => case,
            _ => "negation_context",
        }
    }
    fn model(self) -> &'static str {
        match self {
            Self::LlamaV02 => "llama3.2:latest",
            Self::QwenV02 | Self::QwenQuoteTextV01 | Self::QwenQuoteTextV02
            | Self::QwenQuoteTextV03 | Self::QwenQuoteTextV04 | Self::QwenQuoteTextV05
            | Self::QwenGeneral(_) => "qwen3:8b-q4_K_M",
        }
    }
    fn reasoning_effort(self) -> Option<&'static str> {
        match self {
            Self::LlamaV02 => None,
            Self::QwenV02 | Self::QwenQuoteTextV01 | Self::QwenQuoteTextV02
            | Self::QwenQuoteTextV03 | Self::QwenQuoteTextV04 | Self::QwenQuoteTextV05 => Some("none"),
            Self::QwenGeneral(_) => Some("low"),
        }
    }
    fn quote_text(self) -> bool {
        self.quote_prompt().is_some()
    }
    fn quote_prompt(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::QwenQuoteTextV01 => {
                Some((quote_text_schema::PROMPT_VERSION, quote_text_schema::PROMPT))
            }
            Self::QwenGeneral(_) => Some((
                quote_text_schema::GENERAL_PROMPT_V10_VERSION,
                quote_text_schema::GENERAL_PROMPT_V10,
            )),
            Self::QwenQuoteTextV02 => Some((
                quote_text_schema::PROMPT_V02_VERSION,
                quote_text_schema::PROMPT_V02,
            )),
            Self::QwenQuoteTextV03 => Some((
                quote_text_schema::PROMPT_V03_VERSION,
                quote_text_schema::PROMPT_V03,
            )),
            Self::QwenQuoteTextV04 => Some((
                quote_text_schema::PROMPT_V04_VERSION,
                quote_text_schema::PROMPT_V04,
            )),
            Self::QwenQuoteTextV05 => Some((
                quote_text_schema::PROMPT_V05_VERSION,
                quote_text_schema::PROMPT_V05,
            )),
            _ => None,
        }
    }
    fn quote_provider(self) -> (&'static str, Value) {
        if self.condition_list() {
            (
                quote_text_schema::CONDITION_LIST_PROVIDER_VERSION,
                quote_text_schema::condition_list_provider_schema(),
            )
        } else if self.linked_rule() {
            (quote_text_schema::LINKED_PROVIDER_VERSION, quote_text_schema::linked_provider_schema())
        } else {
            (quote_text_schema::PROVIDER_VERSION, quote_text_schema::provider_schema())
        }
    }
    fn linked_rule(self) -> bool {
        matches!(self, Self::QwenQuoteTextV04 | Self::QwenQuoteTextV05)
    }
    fn condition_list(self) -> bool {
        matches!(self, Self::QwenGeneral(_))
    }
    fn request_hash(self, endpoint: &str, input: &str, timeout: u64) -> Result<String, NovaError> {
        match self {
            Self::LlamaV02 => compiler::structured_probe_request_sha256_v02(
                self.model(),
                endpoint,
                input,
                timeout,
            ),
            Self::QwenQuoteTextV01 => {
                compiler::structured_probe_request_sha256_qwen_quote_text_v01(
                    self.model(),
                    endpoint,
                    input,
                    timeout,
                )
            }
            Self::QwenQuoteTextV02 => compiler::structured_probe_request_sha256_qwen_quote_text_v02(
                self.model(),
                endpoint,
                input,
                timeout,
            ),
            Self::QwenQuoteTextV03 => compiler::structured_probe_request_sha256_qwen_quote_text_v03(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenQuoteTextV04 => compiler::structured_probe_request_sha256_qwen_quote_text_v04(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenQuoteTextV05 => compiler::structured_probe_request_sha256_qwen_quote_text_v05(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenGeneral(_) => compiler::structured_probe_request_sha256_qwen_general_v10(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenV02 => compiler::structured_probe_request_sha256_qwen_v02(
                self.model(),
                endpoint,
                input,
                timeout,
            ),
        }
    }
    fn probe(self, endpoint: &str, input: &str, timeout: u64) -> compiler::StructuredProbeReport {
        match self {
            Self::LlamaV02 => {
                compiler::structured_probe_v02(self.model(), endpoint, input, timeout)
            }
            Self::QwenQuoteTextV01 => compiler::structured_probe_qwen_quote_text_v01(
                self.model(),
                endpoint,
                input,
                timeout,
            ),
            Self::QwenQuoteTextV02 => compiler::structured_probe_qwen_quote_text_v02(
                self.model(),
                endpoint,
                input,
                timeout,
            ),
            Self::QwenQuoteTextV03 => compiler::structured_probe_qwen_quote_text_v03(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenQuoteTextV04 => compiler::structured_probe_qwen_quote_text_v04(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenQuoteTextV05 => compiler::structured_probe_qwen_quote_text_v05(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenGeneral(_) => compiler::structured_probe_qwen_general_v10(
                self.model(), endpoint, input, timeout,
            ),
            Self::QwenV02 => {
                compiler::structured_probe_qwen_v02(self.model(), endpoint, input, timeout)
            }
        }
    }
}
const SOURCE: &str = "合成验收材料：我在工作场景不参加超过30分钟的会议；私人聚会没有这个限制。";
const GENERAL_CASES: [&str; 4] = [
    "plan_not_decision",
    "unknown_not_invented",
    "temporal_before",
    "injection_abstain",
];
const MAX_INPUT: usize = 16 * 1024;
const CASES: &str = include_str!("../../../eval/model-acceptance/cases.json");

fn error(code: &'static str) -> NovaError {
    NovaError::Validation(format!("structured_acceptance: {code}"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn fixed_source(profile: Profile) -> Result<String, NovaError> {
    let catalog: Value = serde_json::from_str(CASES)?;
    let row = catalog["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|case| case["id"] == profile.case_id()))
        .ok_or_else(|| error("fixed_case_missing"))?;
    let source = row["text"]
        .as_str()
        .ok_or_else(|| error("fixed_case_text_invalid"))?;
    if row["operation"] != "intake"
        || row["model_calls"] != 1
        || (!matches!(profile, Profile::QwenGeneral(_)) && source != SOURCE)
    {
        return Err(error("fixed_case_changed"));
    }
    Ok(source.to_owned())
}
fn fixed_input(source: &str) -> Value {
    json!({"schema_version":"nova.extraction.v0.1","source_text":source,"source_role":"user","occurred_at":null,"correction_target":null})
}

fn linked_rule_shape_valid(output: &ExtractionOutput) -> bool {
    match output.units.as_slice() {
        [] => true, // Abstention is legal wire output. Review still decides acceptance.
        [unit] => {
            let conditions = &unit.content.conditions;
            let Some(exception) = conditions.get("scope:exception").and_then(Value::as_object) else {
                return false;
            };
            conditions.len() == 2
                && conditions.get("scope:context").and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
                && exception.len() == 2
                && exception.get("context").and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
                && exception.get("effect").and_then(Value::as_str) == Some("rule_not_applicable")
        }
        _ => false,
    }
}

// This gate belongs to the immutable negation_context diagnostic. Its expected
// boundary comes from the case rubric, and is never inserted into the model output.
fn fixed_comparison_valid(output: &ExtractionOutput) -> bool {
    output.units.iter().all(|unit| {
        let content = &unit.content;
        content.polarity == Polarity::Negative
            && content.value["operator"].as_str() == Some("greater_than")
            && content.value["amount"].as_f64() == Some(30.0)
            && content.value["unit"].as_str() == Some("minutes")
    })
}
fn request_hash(profile: Profile, source: &str) -> Result<String, NovaError> {
    profile.request_hash(
        "http://127.0.0.1:11434/v1",
        &serde_json::to_string(&fixed_input(source))?,
        120,
    )
}

fn source_hashes() -> BTreeMap<&'static str, String> {
    macro_rules! pin {
        ($name:literal, $path:literal) => {
            ($name, hash(include_bytes!($path)))
        };
    }
    BTreeMap::from([
        pin!("crates/nova-model-runtime/Cargo.toml", "../../../crates/nova-model-runtime/Cargo.toml"),
        pin!("crates/nova-model-runtime/src/lib.rs", "../../../crates/nova-model-runtime/src/lib.rs"),
        pin!("crates/nova-model-runtime/src/compiler.rs", "../../../crates/nova-model-runtime/src/compiler.rs"),
        pin!("crates/nova-model-runtime/src/provider_capabilities.rs", "../../../crates/nova-model-runtime/src/provider_capabilities.rs"),
        pin!("crates/nova-model-runtime/src/compiler_schema.rs", "../../../crates/nova-model-runtime/src/compiler_schema.rs"),
        pin!("crates/nova-model-runtime/src/quote_text_schema.rs", "../../../crates/nova-model-runtime/src/quote_text_schema.rs"),
        pin!("crates/nova-model-runtime/src/generation_guard.rs", "../../../crates/nova-model-runtime/src/generation_guard.rs"),
        pin!("Cargo.toml", "../../../Cargo.toml"),
        pin!("Cargo.lock", "../../../Cargo.lock"),
        pin!("apps/nova-review/Cargo.toml", "../Cargo.toml"),
        pin!("apps/nova-review/src/lib.rs", "lib.rs"),
        pin!("apps/nova-review/src/compiler.rs", "compiler.rs"),
        pin!("crates/nova-model-runtime/src/compiler_metering.rs", "../../../crates/nova-model-runtime/src/compiler_metering.rs"),
        pin!("crates/nova-core/src/intake/metering.rs", "../../../crates/nova-core/src/intake/metering.rs"),
        pin!("crates/nova-core/src/intake/capacity.rs", "../../../crates/nova-core/src/intake/capacity.rs"),
        pin!("crates/nova-core/src/intake/compilation_capacity.rs", "../../../crates/nova-core/src/intake/compilation_capacity.rs"),
        pin!("apps/nova-review/src/provider_capabilities.rs", "provider_capabilities.rs"),
        pin!("apps/nova-review/src/quote_text.rs", "quote_text.rs"),
        pin!(
            "apps/nova-review/src/quote_text_schema.rs",
            "quote_text_schema.rs"
        ),
        pin!(
            "apps/nova-review/src/quote-text-schema.json",
            "quote-text-schema.json"
        ),
        pin!(
            "apps/nova-review/src/quote-text-prompt.txt",
            "quote-text-prompt.txt"
        ),
        pin!(
            "apps/nova-review/src/quote-text-prompt-v02.txt",
            "quote-text-prompt-v02.txt"
        ),
        pin!(
            "apps/nova-review/src/quote-text-prompt-v03.txt",
            "quote-text-prompt-v03.txt"
        ),
        pin!("apps/nova-review/src/quote-text-prompt-v04.txt", "quote-text-prompt-v04.txt"),
        pin!("apps/nova-review/src/quote-text-prompt-v05.txt", "quote-text-prompt-v05.txt"),
        pin!(
            "apps/nova-review/src/quote-text-prompt-general-v10.txt",
            "quote-text-prompt-general-v10.txt"
        ),
        pin!("apps/nova-review/src/quote-text-linked-conditions-v04.json", "quote-text-linked-conditions-v04.json"),
        pin!(
            "crates/nova-model-runtime/src/streaming_compiler.rs",
            "../../../crates/nova-model-runtime/src/streaming_compiler.rs"
        ),
        pin!(
            "apps/nova-review/src/generation_guard.rs",
            "generation_guard.rs"
        ),
        pin!(
            "apps/nova-review/src/compiler_schema.rs",
            "compiler_schema.rs"
        ),
        pin!(
            "apps/nova-review/src/compiler-schema.json",
            "compiler-schema.json"
        ),
        pin!(
            "apps/nova-review/src/compiler-prompt.txt",
            "compiler-prompt.txt"
        ),
        pin!(
            "apps/nova-review/src/compiler-structured-prompt-v02.txt",
            "compiler-structured-prompt-v02.txt"
        ),
        pin!(
            "apps/nova-review/src/structured_acceptance.rs",
            "structured_acceptance.rs"
        ),
        pin!(
            "apps/nova-review/src/bin/nova-structured-acceptance.rs",
            "bin/nova-structured-acceptance.rs"
        ),
        pin!(
            "crates/nova-core/Cargo.toml",
            "../../../crates/nova-core/Cargo.toml"
        ),
        pin!(
            "crates/nova-core/src/intake.rs",
            "../../../crates/nova-core/src/intake.rs"
        ),
        pin!(
            "crates/nova-core/src/intake/storage.rs",
            "../../../crates/nova-core/src/intake/storage.rs"
        ),
        pin!(
            "crates/nova-core/src/intake/compiler.rs",
            "../../../crates/nova-core/src/intake/compiler.rs"
        ),
        pin!(
            "crates/nova-core/src/intake/consolidation.rs",
            "../../../crates/nova-core/src/intake/consolidation.rs"
        ),
        pin!(
            "crates/nova-core/src/compilation/mod.rs",
            "../../../crates/nova-core/src/compilation/mod.rs"
        ),
        pin!(
            "crates/nova-core/src/compilation/model.rs",
            "../../../crates/nova-core/src/compilation/model.rs"
        ),
        pin!(
            "crates/nova-core/src/compilation/wire.rs",
            "../../../crates/nova-core/src/compilation/wire.rs"
        ),
        pin!(
            "eval/model-acceptance/cases.json",
            "../../../eval/model-acceptance/cases.json"
        ),
    ])
}

/// Offline metadata embeds the sources actually compiled into this binary.
pub fn metadata() -> Result<Value, NovaError> {
    metadata_for_profile("llama-v02")
}

/// Only compiled-in synthetic comparison profiles are selectable.
pub fn metadata_for_profile(name: &str) -> Result<Value, NovaError> {
    let profile = Profile::parse(name)?;
    metadata_for_case(profile, &fixed_source(profile)?)
}

fn metadata_for_case(profile: Profile, source: &str) -> Result<Value, NovaError> {
    let mut metadata = json!({"schema_version":PROTOCOL,"case_id":profile.case_id(),"model":profile.model(),
        "profile":profile.name(),"reasoning_effort":profile.reasoning_effort(),
        "output_mode":"structured_local_prompt_v02",
        "prompt_sha256":hash(compiler::STRUCTURED_SYSTEM_PROMPT.as_bytes()),
        "prompt_version":compiler::STRUCTURED_PROMPT_VERSION,
        "schema_sha256":hash(compiler_schema::SCHEMA_JSON.as_bytes()),
        "provider_schema_sha256":hash(compiler_schema::provider_schema_json().as_bytes()),
        "provider_schema_version":compiler_schema::PROVIDER_SCHEMA_CONTRACT_VERSION,
        "request_sha256":request_hash(profile, source)?,"compiled_source_hashes":source_hashes(),
        "maximum_dispatches":1,"maximum_seconds":120,"maximum_output_tokens":2048,
        "automatic_retries":0,"remote_supported":false,"provider_billing_hard_cap":false});
    if let Some((version, prompt)) = profile.quote_prompt() {
        let (provider_version, provider_schema) = profile.quote_provider();
        metadata["output_mode"] = json!("structured_quote_text_v01");
        metadata["prompt_sha256"] = json!(hash(prompt.as_bytes()));
        metadata["prompt_version"] = json!(version);
        metadata["schema_sha256"] = json!(hash(quote_text_schema::SCHEMA_JSON.as_bytes()));
        metadata["provider_schema_sha256"] = json!(hash(
            serde_json::to_string(&provider_schema)?.as_bytes()
        ));
        metadata["provider_schema_version"] = json!(provider_version);
        metadata["resolver_version"] = json!("nova.quote-text.resolver.v0.1");
        metadata["core_schema_version"] = json!(EXTRACTION_VERSION);
        metadata["core_schema_sha256"] = json!(hash(compiler_schema::SCHEMA_JSON.as_bytes()));
        metadata["source_text_sha256"] = json!(hash(source.as_bytes()));
    }
    Ok(metadata)
}
fn binary_hash() -> Result<String, NovaError> {
    let mut file = File::open(std::env::current_exe()?)?;
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        return Err(error("binary_size"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn emit(value: &Value) -> Result<(), NovaError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > 1024 * 1024 {
        return Err(error("report_size"));
    }
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
fn commands() -> Result<mpsc::Receiver<Result<Vec<u8>, ()>>, NovaError> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("structured-acceptance-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                let mut line = Vec::new();
                let result = loop {
                    let data = match input.fill_buf() {
                        Ok(data) => data,
                        Err(_) => break Err(()),
                    };
                    if data.is_empty() {
                        if line.is_empty() {
                            return;
                        }
                        break Err(());
                    }
                    let count = data
                        .iter()
                        .position(|b| *b == b'\n')
                        .map_or(data.len(), |n| n + 1);
                    if count + line.len() > MAX_INPUT {
                        break Err(());
                    }
                    line.extend_from_slice(&data[..count]);
                    input.consume(count);
                    if line.last() == Some(&b'\n') {
                        break Ok(line);
                    }
                };
                let stop = result.is_err();
                if tx.send(result).is_err() || stop {
                    return;
                }
            }
        })
        .map_err(|_| error("input_thread"))?;
    Ok(rx)
}
fn read<T: serde::de::DeserializeOwned>(
    rx: &mpsc::Receiver<Result<Vec<u8>, ()>>,
) -> Result<T, NovaError> {
    let bytes = rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| error("input_eof_or_deadline"))?
        .map_err(|_| error("input_limit"))?;
    serde_json::from_slice(&bytes).map_err(|_| error("invalid_command"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    command: String,
    schema_version: String,
    instance_id: String,
    binary_sha256: String,
    plan_id: String,
    request_sha256: String,
    endpoint: String,
    timeout_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dispatch {
    command: String,
    instance_id: String,
    binary_sha256: String,
    plan_id: String,
    request_sha256: String,
    profile_id: String,
    run_nonce: String,
    source_snapshot_sha256: Option<String>,
}
fn checked_endpoint(endpoint: &str) -> Result<String, NovaError> {
    // Require the literal numeric loopback origin. No credentials, aliases, paths or redirects.
    let port = endpoint
        .strip_prefix("http://127.0.0.1:")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| error("endpoint_invalid"))?;
    if endpoint != format!("http://127.0.0.1:{port}") {
        return Err(error("endpoint_invalid"));
    }
    Ok(format!("{endpoint}/v1"))
}

// Keep the acceptance job taxonomy aligned with the ordinary compiler worker. A provider
// rejection or lost transport is not evidence that the model generated invalid memory.
fn transport_failure(code: &str) -> AttemptFailure {
    if code.contains("budget") {
        AttemptFailure::BudgetExceeded
    } else if code.starts_with("generation_")
        && ![
            "client_deadline",
            "transport_",
            "http_status",
            "owned_child_",
            "provider_error",
        ]
        .iter()
        .any(|part| code.contains(part))
    {
        AttemptFailure::InvalidOutput
    } else {
        AttemptFailure::ModelUnavailable
    }
}
fn core_failure(error: &NovaError) -> AttemptFailure {
    match error {
        NovaError::InvalidState(_) => AttemptFailure::ConfigurationChanged,
        NovaError::Validation(message) if message.contains("budget") => {
            AttemptFailure::BudgetExceeded
        }
        _ => AttemptFailure::InvalidOutput,
    }
}

/// No model call can occur before both bound begin and dispatch commands validate.
pub fn serve() -> Result<(), NovaError> {
    serve_for_profile("llama-v02")
}

/// Execute the selected fixed comparison without permitting arbitrary model overrides.
pub fn serve_for_profile(name: &str) -> Result<(), NovaError> {
    let selected = Profile::parse(name)?;
    let source = fixed_source(selected)?;
    let rx = commands()?;
    let instance_id = Uuid::now_v7().to_string();
    let binary_sha256 = binary_hash()?;
    let mut hello = metadata_for_case(selected, &source)?;
    hello["event"] = json!("hello");
    hello["instance_id"] = json!(instance_id);
    hello["binary_sha256"] = json!(binary_sha256);
    emit(&hello)?;
    let begin: Begin = read(&rx)?;
    if begin.command != "begin"
        || begin.schema_version != PROTOCOL
        || begin.instance_id != instance_id
        || begin.binary_sha256 != binary_sha256
        || !digest(&begin.plan_id)
        || begin.request_sha256 != request_hash(selected, &source)?
        || !(1..=120).contains(&begin.timeout_seconds)
    {
        return Err(error("begin_binding_invalid"));
    }
    let endpoint = checked_endpoint(&begin.endpoint)?;
    let profile = tempfile::Builder::new()
        .prefix("nova-structured-single-")
        .tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let profile_id = Uuid::now_v7().to_string();
    let run_nonce = hash(format!("{}{}", Uuid::now_v7(), Uuid::now_v7()).as_bytes());
    let scope = AssetScope {
        owner_id: "owner:synthetic-structured-acceptance".into(),
        space: format!("single-{profile_id}"),
    };
    let key = VaultKey::generate()?;
    key.write_new_file(&profile.path().join("vault.key"))?;
    let mut store = IntakeStore::create(&profile.path().join("intake.sqlite"), key, scope.clone())?;
    if !store.jobs(&scope, None)?.is_empty() {
        return Err(error("fresh_inventory_failed"));
    }
    store.configure_model(
        1,
        ModelSelection::Local {
            model_id: selected.model().into(),
            endpoint: endpoint.clone(),
        },
    )?;
    store.enable_compilation(&scope)?;
    let authority = IntakeAuthority {
        scope: scope.clone(),
        actor_id: "actor:synthetic-structured-acceptance".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    };
    let receipt = store.receive(
        &authority,
        &serde_json::to_vec(&IntakeRequest {
            schema_version: INTAKE_VERSION.into(),
            request_id: "structured-single-negation-context".into(),
            source_platform: "synthetic-acceptance".into(),
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content: source.clone(),
            occurred_at: None,
        })?,
    )?;
    let job = store
        .claim_next(&scope, (begin.timeout_seconds + 30) as u32)?
        .ok_or_else(|| error("job_missing"))?;
    if job.job.job_id != receipt.job_id {
        return Err(error("job_identity"));
    }
    let lease = job
        .lease
        .as_ref()
        .ok_or_else(|| error("lease_missing"))?
        .lease_id
        .clone();
    let prepared = store.prepare_compilation(&scope, &receipt.job_id, &lease)?;
    let input = prepared.model_input();
    if input != fixed_input(&source) {
        return Err(error("model_input_changed"));
    }
    let input = serde_json::to_string(&input)?;
    if selected.request_hash(&endpoint, &input, begin.timeout_seconds)? != begin.request_sha256 {
        return Err(error("request_binding_invalid"));
    }
    let trusted_source = prepared
        .context
        .sources
        .first()
        .ok_or_else(|| error("source_missing"))?;
    if prepared.context.sources.len() != 1
        || trusted_source.content != source
        || trusted_source.source != receipt.source
    {
        return Err(error("source_snapshot_invalid"));
    }
    let snapshot_hash = hash(&serde_json::to_vec(&serde_json::to_value(trusted_source)?)?);
    let mut identity = json!({"schema_version":PROTOCOL,"instance_id":instance_id,"binary_sha256":binary_sha256,"plan_id":begin.plan_id,"request_sha256":begin.request_sha256,"profile_id":profile_id,"run_nonce":run_nonce});
    if selected.quote_text() {
        identity["source_snapshot_sha256"] = json!(snapshot_hash);
    }
    let mut ready = identity.clone();
    ready["event"] = json!("ready");
    ready["fresh_inventory"] = json!(true);
    ready["dispatches"] = json!(0);
    emit(&ready)?;
    let dispatch: Dispatch = read(&rx)?;
    if dispatch.command != "dispatch"
        || dispatch.instance_id != instance_id
        || dispatch.binary_sha256 != binary_sha256
        || dispatch.plan_id != begin.plan_id
        || dispatch.request_sha256 != begin.request_sha256
        || dispatch.profile_id != profile_id
        || dispatch.run_nonce != run_nonce
        || dispatch.source_snapshot_sha256.as_deref()
            != selected.quote_text().then_some(snapshot_hash.as_str())
    {
        return Err(error("dispatch_binding_invalid"));
    }
    let attempt = store.authorize_dispatch(&scope, &receipt.job_id, &lease, 1)?;
    if attempt.configuration != prepared.configuration
        || attempt.model_input() != fixed_input(&source)
        || attempt.context != prepared.context
    {
        return Err(error("dispatch_state_changed"));
    }
    let transport = selected.probe(&endpoint, &input, begin.timeout_seconds);
    let mut result = identity;
    result["event"] = json!("result");
    result["dispatches"] = json!(u8::from(transport.dispatch_started));
    result["transport"] = serde_json::to_value(&transport)?;
    result["semantic_acceptance"] = json!("pending_independent_review");
    result["core_committed"] = json!(false);
    result["core_validation_passed"] = json!(false);
    result["quote_grounding_passed"] = json!(false);
    result["core_error_code"] = Value::Null;
    result["units"] = json!([]);
    result["quote_resolution"] = Value::Null;
    result["committed_compilation"] = Value::Null;
    let committed = if transport.request_sha256 != begin.request_sha256 {
        Err((
            error("response_request_binding"),
            AttemptFailure::InvalidOutput,
        ))
    } else if let Some(code) = transport.error_code.as_deref() {
        Err((error("transport_failed"), transport_failure(code)))
    } else if let Some(content) = transport.content.as_ref() {
        let commit_bytes = if let Some((version, prompt)) = selected.quote_prompt() {
            let resolve = if selected.condition_list() {
                quote_text::resolve_condition_list
            } else {
                quote_text::resolve
            };
            match resolve(&trusted_source.content, content.as_bytes()) {
                Ok(resolved) => {
                    let (provider_version, provider_schema) = selected.quote_provider();
                    result["quote_resolution"] = json!({
                        "resolver_version":"nova.quote-text.resolver.v0.1",
                        "source_snapshot":trusted_source,
                        "source_snapshot_sha256":snapshot_hash,
                        "source_text_sha256":resolved.source_sha256(),
                        "raw_output_sha256":resolved.raw_sha256(),
                        "resolved_output_sha256":resolved.resolved_sha256(),
                        "resolved_output":std::str::from_utf8(resolved.resolved_bytes()).expect("serialized UTF-8"),
                        "proofs":resolved.proofs(),
                        "semantic_acceptance":resolved.semantic_acceptance(),
                        "core_schema_version":EXTRACTION_VERSION,
                        "core_schema_sha256":hash(compiler_schema::SCHEMA_JSON.as_bytes()),
                        "prompt_version":version,
                        "prompt_sha256":hash(prompt.as_bytes()),
                        "generation_schema_version":quote_text_schema::SCHEMA_VERSION,
                        "generation_schema_sha256":hash(quote_text_schema::SCHEMA_JSON.as_bytes()),
                        "provider_schema_version":provider_version,
                        "provider_schema_sha256":hash(serde_json::to_string(&provider_schema)?.as_bytes()),
                        "resolver_source_sha256":hash(include_bytes!("quote_text.rs"))});
                    if selected.linked_rule() && !linked_rule_shape_valid(resolved.extraction())
                    {
                        result["linked_rule_shape_error"] = json!("invalid_linked_rule_shape");
                        Err(error("invalid_linked_rule_shape"))
                    } else if matches!(selected, Profile::QwenQuoteTextV05)
                        && !fixed_comparison_valid(resolved.extraction())
                    {
                        result["fixed_comparison_error"] = json!("fixed_case_comparison_mismatch");
                        Err(error("fixed_case_comparison_mismatch"))
                    } else {
                        Ok(resolved.resolved_bytes().to_vec())
                    }
                }
                Err(failure) => {
                    result["quote_resolution_error"] = json!(failure.code());
                    Err(error("quote_resolution_failed"))
                }
            }
        } else {
            ExtractionOutput::parse(content.as_bytes()).map(|_| content.as_bytes().to_vec())
        };
        commit_bytes
            .and_then(|bytes| store.commit_compilation(&scope, &receipt.job_id, &lease, &bytes))
            .map_err(|error| {
                let failure = core_failure(&error);
                (error, failure)
            })
    } else {
        Err((
            error("complete_output_missing"),
            AttemptFailure::InvalidOutput,
        ))
    };
    match committed {
        Ok(committed) => {
            result["committed_compilation"] = serde_json::to_value(&committed)?;
            let units = committed
                .output_versions
                .iter()
                .map(|r| store.compiled_unit(&scope, &r.object_id).map(|u| u.unit))
                .collect::<Result<Vec<_>, _>>()?;
            result["core_committed"] = json!(true);
            result["core_validation_passed"] = json!(true);
            result["quote_grounding_passed"] = json!(true);
            result["units"] = json!(units);
        }
        Err((error, failure)) => {
            result["core_error_code"] = json!(error.code());
            store.fail_attempt(&scope, &receipt.job_id, &lease, failure)?;
        }
    }
    result["source_preserved"] =
        json!(store.source(&scope, &receipt.source.object_id)?.content == source);
    result["job"] = json!(store.job(&scope, &receipt.job_id)?.job);
    result["compiled_unit_count"] = json!(store.compiled_units(&scope, None)?.len());
    result["profile_retained"] = json!(false);
    result["temporary_scope"] = json!(true);
    // Every diagnostic is scoped to this synthetic case; the private temporary key/store is
    // deleted on process exit. The unchanged source and output evidence remain in the report.
    emit(&result)?;
    Ok(())
}
