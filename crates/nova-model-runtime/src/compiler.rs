//! Bounded model transport shared by local and Hosted execution. No Vault, identity,
//! budget or background worker authority is created by this crate.
use nova_core::{NovaError, PrivacyLevel, intake::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{IpAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};
use zeroize::Zeroizing;

pub const SYSTEM_PROMPT: &str = include_str!("../../../apps/nova-review/src/compiler-prompt.txt");
pub const STRUCTURED_PROMPT_VERSION: &str = "nova.extraction.structured-prompt.v0.2";
pub const STRUCTURED_SYSTEM_PROMPT: &str =
    include_str!("../../../apps/nova-review/src/compiler-structured-prompt-v02.txt");
#[path = "compiler_metering.rs"]
mod metering;
#[path = "streaming_compiler.rs"]
mod streaming;
pub use metering::ObservedGeneration;
/// Performs a single bounded request; caller must hold live dispatch authority.
pub fn generate(transport: Transport) -> ObservedGeneration {
    metering::send(transport)
}

/// Existing configurations retain their original JSON-object transport. The new local
/// schema/stream path is an explicit runtime choice until real-model acceptance is complete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    #[default]
    JsonObject,
    StructuredLocal,
}
#[derive(Clone)]
pub struct ModelRuntime {
    pub maximum_calls: u64,
    pub credential_file: Option<PathBuf>,
    pub credential_origin: Option<String>,
    pub output_mode: OutputMode,
}
impl ModelRuntime {
    pub fn local(maximum_calls: u64) -> Self {
        Self {
            maximum_calls,
            credential_file: None,
            credential_origin: None,
            output_mode: OutputMode::JsonObject,
        }
    }
    pub fn structured_local(maximum_calls: u64) -> Self {
        Self {
            output_mode: OutputMode::StructuredLocal,
            ..Self::local(maximum_calls)
        }
    }
    pub fn validate(&self) -> Result<(), NovaError> {
        if !(1..=10_000).contains(&self.maximum_calls)
            || self.credential_file.is_some() != self.credential_origin.is_some()
            || (self.output_mode == OutputMode::StructuredLocal && self.credential_file.is_some())
        {
            return Err(error("invalid_runtime_configuration"));
        }
        if let Some(origin) = &self.credential_origin {
            let url = url::Url::parse(origin).map_err(|_| error("invalid_credential_origin"))?;
            if url.scheme() != "https" || url.origin().ascii_serialization() != *origin {
                return Err(error("exact_https_credential_origin_required"));
            }
        }
        Ok(())
    }
}
fn error(code: &'static str) -> NovaError {
    NovaError::Validation(format!("compiler: {code}"))
}
pub struct Transport {
    url: String,
    resolve: Option<String>,
    body: Zeroizing<String>,
    credential: Option<Zeroizing<String>>,
    timeout_seconds: u64,
    output_mode: OutputMode,
}
pub fn prepare_transport(
    attempt: &CompilationAttempt,
    runtime: &ModelRuntime,
) -> Result<Transport, NovaError> {
    runtime.validate()?;
    attempt.configuration.validate()?;
    if attempt.context.sources.iter().any(|s| {
        matches!(
            s.privacy_level,
            PrivacyLevel::Sensitive | PrivacyLevel::Sealed
        )
    }) {
        return Err(error("privacy_not_permitted"));
    }
    let crate::provider_capabilities::TransportTarget {
        model,
        mut url,
        host,
        port,
        remote,
    } = crate::provider_capabilities::transport_target(&attempt.configuration, runtime)?;
    let credential = if remote {
        if runtime.credential_origin.as_deref() != Some(url.origin().ascii_serialization().as_str())
        {
            return Err(error("credential_origin_mismatch"));
        }
        Some(read_credential(
            runtime
                .credential_file
                .as_deref()
                .ok_or_else(|| error("credential_missing"))?,
        )?)
    } else {
        None
    };
    let resolve = if remote {
        if url.scheme() != "https" || host.parse::<IpAddr>().is_ok() {
            return Err(error("remote_public_dns_required"));
        }
        let addresses = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| error("dns_unavailable"))?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
            return Err(error("remote_private_address_denied"));
        }
        let ip = addresses[0].ip();
        Some(format!(
            "{host}:{port}:{}",
            match ip {
                IpAddr::V4(ip) => ip.to_string(),
                IpAddr::V6(ip) => format!("[{ip}]"),
            }
        ))
    } else {
        if url.scheme() != "http" || !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err(error("local_loopback_required"));
        }
        None
    };
    url.set_path(&format!(
        "{}/chat/completions",
        url.path().trim_end_matches('/')
    ));
    let body = serde_json::to_string(&request_body(
        &model,
        attempt.model_input(),
        runtime.output_mode,
    )?)?;
    if body.len() > 128 * 1024 {
        return Err(error("input_budget_exceeded"));
    }
    Ok(Transport {
        url: url.to_string(),
        resolve,
        body: Zeroizing::new(body),
        credential,
        timeout_seconds: crate::provider_capabilities::DEFAULT_TIMEOUT_SECONDS,
        output_mode: runtime.output_mode,
    })
}

pub fn prepare_workspace_transport(
    attempt: &CompilationAttempt,
    runtime: &ModelRuntime,
) -> Result<Transport, NovaError> {
    let mut transport = prepare_transport(attempt, runtime)?;
    transport.timeout_seconds =
        crate::provider_capabilities::workspace_timeout_seconds(&attempt.configuration.selection);
    Ok(transport)
}

fn request_body(model: &str, input: Value, mode: OutputMode) -> Result<Value, NovaError> {
    let mut body = json!({"model":model,"stream":false,"temperature":0,"max_tokens":crate::provider_capabilities::DEFAULT_MAX_OUTPUT_TOKENS,
        "response_format":{"type":"json_object"},"messages":[
        {"role":"system","content":SYSTEM_PROMPT},
        {"role":"user","content":serde_json::to_string(&input)?}]});
    if mode == OutputMode::StructuredLocal {
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage":true});
        body["response_format"] = crate::compiler_schema::extraction_response_format();
    }
    Ok(body)
}
/// A bounded observation for the dedicated synthetic, single-dispatch acceptance tool.
/// `content` exists only after full guarded validation. Failed diagnostic text is untrusted
/// evidence, capped at 64 KiB, and must never be passed to a commit operation.
#[derive(Debug, Default, Serialize)]
pub struct StructuredProbeReport {
    pub request_sha256: String,
    pub error_code: Option<String>,
    pub content: Option<String>,
    pub diagnostic_content: Option<String>,
    pub diagnostic_truncated: bool,
    pub usage: Option<crate::generation_guard::GenerationUsage>,
    pub finish_reason: Option<String>,
    pub sse_done: bool,
    pub wire_bytes: usize,
    pub decoded_bytes: usize,
    pub completed_units: usize,
    pub elapsed_ms: u64,
    pub first_content_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub curl_exit_code: Option<i32>,
    pub owned_child_reaped: bool,
    pub dispatch_started: bool,
}

/// Restricted diagnostic entrypoint. The caller must bind its immutable synthetic input,
/// actual binary, and one-dispatch authorization before calling; no Vault is opened here.
/// Endpoint is the numeric-loopback OpenAI-compatible base (ending in `/v1`).
pub fn structured_probe(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::Original,
    )
}

/// Explicit prompt v0.2 experiment for the dedicated synthetic acceptance driver.
/// This failed acceptance candidate is never selected by ordinary runtime configuration.
pub fn structured_probe_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::V02Experiment,
    )
}

/// Separate fixed Qwen v0.2 profile comparison, including its non-thinking sampling settings.
/// This is not a model-only ablation and cannot change ordinary runtime configuration.
pub fn structured_probe_qwen_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenV02Experiment,
    )
}

/// Explicit synthetic quote-text experiment. The caller must resolve exact quotes against
/// a trusted source snapshot before passing the derived bytes to core; never a runtime default.
pub fn structured_probe_qwen_quote_text_v01(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenQuoteTextV01,
    )
}

pub fn structured_probe_request_sha256_qwen_quote_text_v01(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenQuoteTextV01,
    )
}

pub fn structured_probe_qwen_quote_text_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenQuoteTextV02,
    )
}

pub fn structured_probe_request_sha256_qwen_quote_text_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenQuoteTextV02,
    )
}

pub fn structured_probe_qwen_quote_text_v03(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV03)
}

pub fn structured_probe_request_sha256_qwen_quote_text_v03(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV03)
}

pub fn structured_probe_qwen_quote_text_v04(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV04)
}

pub fn structured_probe_request_sha256_qwen_quote_text_v04(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV04)
}

pub fn structured_probe_qwen_quote_text_v05(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV05)
}

pub fn structured_probe_request_sha256_qwen_quote_text_v05(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenQuoteTextV05)
}

pub fn structured_probe_qwen_general_v10(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> StructuredProbeReport {
    probe_with_prompt(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenGeneralV10)
}

pub fn structured_probe_request_sha256_qwen_general_v10(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(model, endpoint, input, timeout_seconds, StructuredPrompt::QwenGeneralV10)
}

#[derive(Clone, Copy)]
enum StructuredPrompt {
    Original,
    V02Experiment,
    QwenV02Experiment,
    QwenQuoteTextV01,
    QwenQuoteTextV02,
    QwenQuoteTextV03,
    QwenQuoteTextV04,
    QwenQuoteTextV05,
    QwenGeneralV10,
}

impl StructuredPrompt {
    fn quote_text(self) -> Option<&'static str> {
        match self {
            Self::QwenQuoteTextV01 => Some(crate::quote_text_schema::PROMPT),
            Self::QwenQuoteTextV02 => Some(crate::quote_text_schema::PROMPT_V02),
            Self::QwenQuoteTextV03 => Some(crate::quote_text_schema::PROMPT_V03),
            Self::QwenQuoteTextV04 => Some(crate::quote_text_schema::PROMPT_V04),
            Self::QwenQuoteTextV05 => Some(crate::quote_text_schema::PROMPT_V05),
            Self::QwenGeneralV10 => Some(crate::quote_text_schema::GENERAL_PROMPT_V10),
            _ => None,
        }
    }
}

fn probe_with_prompt(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
    prompt: StructuredPrompt,
) -> StructuredProbeReport {
    use sha2::{Digest, Sha256};
    match prepare_structured_probe(model, endpoint, input, timeout_seconds, prompt) {
        Ok(transport) => {
            let request_sha256 = format!("{:x}", Sha256::digest(transport.body.as_bytes()));
            let mut report = if prompt.quote_text().is_some() {
                streaming::probe_quote_text(transport)
            } else {
                streaming::probe(transport)
            };
            report.request_sha256 = request_sha256;
            report
        }
        Err(failure) => StructuredProbeReport {
            error_code: Some(streaming::code(&failure)),
            ..StructuredProbeReport::default()
        },
    }
}

/// Reconstruct and hash the exact request without dispatch or environment/service access.
pub fn structured_probe_request_sha256(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::Original,
    )
}

/// Hash only the explicitly selected v0.2 experiment; never changes ordinary runtime policy.
pub fn structured_probe_request_sha256_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::V02Experiment,
    )
}

/// Bind the fixed Qwen model, v0.2 prompt, sampling and disabled-thinking request before dispatch.
pub fn structured_probe_request_sha256_qwen_v02(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
) -> Result<String, NovaError> {
    probe_request_sha256(
        model,
        endpoint,
        input,
        timeout_seconds,
        StructuredPrompt::QwenV02Experiment,
    )
}

fn probe_request_sha256(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
    prompt: StructuredPrompt,
) -> Result<String, NovaError> {
    use sha2::{Digest, Sha256};
    let transport = prepare_structured_probe(model, endpoint, input, timeout_seconds, prompt)?;
    Ok(format!("{:x}", Sha256::digest(transport.body.as_bytes())))
}

fn prepare_structured_probe(
    model: &str,
    endpoint: &str,
    input: &str,
    timeout_seconds: u64,
    prompt: StructuredPrompt,
) -> Result<Transport, NovaError> {
    let fixed_model = match prompt {
        StructuredPrompt::Original | StructuredPrompt::V02Experiment => "llama3.2:latest",
        StructuredPrompt::QwenV02Experiment
        | StructuredPrompt::QwenQuoteTextV01
        | StructuredPrompt::QwenQuoteTextV02
        | StructuredPrompt::QwenQuoteTextV03
        | StructuredPrompt::QwenQuoteTextV04
        | StructuredPrompt::QwenQuoteTextV05
        | StructuredPrompt::QwenGeneralV10 => "qwen3:8b-q4_K_M",
    };
    if model != fixed_model || !(1..=120).contains(&timeout_seconds) {
        return Err(error("structured_probe_scope_invalid"));
    }
    if input.len() > 128 * 1024 {
        return Err(error("input_budget_exceeded"));
    }
    let mut url = url::Url::parse(endpoint).map_err(|_| error("endpoint_invalid"))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().trim_end_matches('/') != "/v1"
        || !url
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
    {
        return Err(error("structured_probe_endpoint_invalid"));
    }
    let input: Value =
        serde_json::from_str(input).map_err(|_| error("structured_probe_input_invalid"))?;
    if !input.is_object() {
        return Err(error("structured_probe_input_invalid"));
    }
    let mut body = request_body(model, input, OutputMode::StructuredLocal)?;
    if !matches!(prompt, StructuredPrompt::Original) {
        body["messages"][0]["content"] = json!(STRUCTURED_SYSTEM_PROMPT);
    }
    if let Some(system_prompt) = prompt.quote_text() {
        body["messages"][0]["content"] = json!(system_prompt);
        body["response_format"] = match prompt {
            StructuredPrompt::QwenQuoteTextV04 | StructuredPrompt::QwenQuoteTextV05 => {
                crate::quote_text_schema::linked_response_format()
            }
            StructuredPrompt::QwenGeneralV10 => crate::quote_text_schema::condition_list_response_format(),
            _ => crate::quote_text_schema::response_format(),
        };
    }
    if matches!(prompt, StructuredPrompt::QwenV02Experiment) || prompt.quote_text().is_some() {
        body["reasoning_effort"] = json!(if matches!(prompt, StructuredPrompt::QwenGeneralV10) {
            "low"
        } else {
            "none"
        });
        body["temperature"] = json!(0.7);
        body["top_p"] = json!(0.8);
    }
    let body = serde_json::to_string(&body)?;
    if body.len() > 128 * 1024 {
        return Err(error("input_budget_exceeded"));
    }
    url.set_path("/v1/chat/completions");
    Ok(Transport {
        url: url.to_string(),
        resolve: None,
        body: Zeroizing::new(body),
        credential: None,
        timeout_seconds,
        output_mode: OutputMode::StructuredLocal,
    })
}

/// Restricted acceptance driver reuses this transport without enabling the background worker.
pub fn acceptance_transport(
    attempt: &CompilationAttempt,
    maximum_output_tokens: u64,
    timeout_seconds: u64,
) -> Result<Transport, NovaError> {
    if !matches!(
        attempt.configuration.selection,
        ModelSelection::Local { .. }
    ) {
        return Err(error("acceptance_local_only"));
    }
    let mut transport = prepare_transport(attempt, &ModelRuntime::local(10))?;
    let mut body: Value = serde_json::from_str(&transport.body)?;
    body["max_tokens"] = json!(maximum_output_tokens);
    transport.body = Zeroizing::new(serde_json::to_string(&body)?);
    transport.timeout_seconds = timeout_seconds;
    Ok(transport)
}
/// A prepared request is consumed once; it grants no dispatch authorization. Callers must
/// check live source, configuration, lease and budget before handing it to `generate`.
impl Transport {
    /// Validate separation from application secrets without returning a retained credential.
    pub fn validate_credential(
        &self,
        validate: impl FnOnce(&str) -> Result<(), NovaError>,
    ) -> Result<(), NovaError> {
        if let Some(credential) = &self.credential {
            validate(credential.as_str())?;
        }
        Ok(())
    }
    pub fn protocol(&self) -> CompilationTransport {
        match self.output_mode {
            OutputMode::JsonObject => CompilationTransport::OpenaiCompatibleJson,
            OutputMode::StructuredLocal => CompilationTransport::OpenaiCompatibleLocalSse,
        }
    }
    pub fn input_bytes(&self) -> usize {
        self.body.len()
    }
}
fn read_credential(path: &Path) -> Result<Zeroizing<String>, NovaError> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err(error("credential_private_file_required"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(error("credential_private_file_required"));
        }
    }
    let value = Zeroizing::new(std::fs::read_to_string(path)?);
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|c| c.is_ascii_graphic() && !b"\"\\".contains(&c))
    {
        return Err(error("credential_invalid"));
    }
    Ok(Zeroizing::new(trimmed.into()))
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && !ip.is_unspecified()
                && o[0] != 0
                && o[0] < 224
                && !(o[0] == 100 && (64..=127).contains(&o[1]))
                && !(o[0] == 198 && matches!(o[1], 18 | 19))
        }
        IpAddr::V6(ip) => {
            let o = ip.octets();
            o[0] & 0xe0 == 0x20
                && !(o[0] == 0x20 && o[1] == 0x01 && o[2] == 0x0d && o[3] == 0xb8)
                && !(o[0] == 0x20 && o[1] == 0x02)
                && ip.to_ipv4_mapped().is_none()
        }
    }
}
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}
/// Transport metadata is an allowlist, never curl stderr, an endpoint, headers, or a body.
/// In particular, exit 28 records a client deadline; an absent HTTP status is not HTTP 500.
#[derive(Debug, Serialize)]
pub struct TransportDiagnostic {
    mechanical_only: bool,
    classification: &'static str,
    phase: &'static str,
    curl_exit_code: Option<i32>,
    signal: Option<i32>,
    http_status: Option<u16>,
    elapsed_millis: u64,
    timeout_seconds: u64,
    response_body_bytes: Option<usize>,
    automatic_retries: u8,
}

#[derive(Debug)]
pub struct TransportFailure {
    pub error: NovaError,
    pub diagnostic: Box<TransportDiagnostic>,
}

fn transport_failure(
    code: &'static str,
    phase: &'static str,
    classification: &'static str,
    started: Instant,
    timeout_seconds: u64,
) -> TransportFailure {
    TransportFailure {
        error: error(code),
        diagnostic: Box::new(TransportDiagnostic {
            mechanical_only: true,
            classification,
            phase,
            curl_exit_code: None,
            signal: None,
            http_status: None,
            elapsed_millis: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            timeout_seconds,
            response_body_bytes: None,
            automatic_retries: 0,
        }),
    }
}

pub fn send_raw_diagnostic(transport: Transport) -> Result<Vec<u8>, TransportFailure> {
    send_raw_observed(transport).map(|response| response.bytes)
}
struct RawProviderResponse {
    bytes: Vec<u8>,
    http_status: u16,
    elapsed_millis: u64,
}
fn send_raw_observed(transport: Transport) -> Result<RawProviderResponse, TransportFailure> {
    let started = Instant::now();
    let failure = |code, phase, classification| {
        transport_failure(
            code,
            phase,
            classification,
            started,
            transport.timeout_seconds,
        )
    };
    if transport.output_mode != OutputMode::JsonObject {
        return Err(failure(
            "nonstream_transport_required",
            "preflight",
            "wrong_transport_mode",
        ));
    }
    // The system curl provides native certificate validation; no shell, .curlrc, proxies, redirects,
    // automatic retries, arbitrary tools or secret argv. curl >=8.4 enforces chunked max-filesize.
    let version = Command::new("/usr/bin/curl")
        .args(["--disable", "--version"])
        .output()
        .map_err(|_| failure("curl_unavailable", "preflight", "curl_unavailable"))?;
    let v = String::from_utf8_lossy(&version.stdout);
    let n = v
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect::<Vec<_>>();
    if !version.status.success() || n.len() < 2 || (n[0], n[1]) < (8, 4) {
        return Err(failure(
            "curl_8_4_required",
            "preflight",
            "unsupported_curl",
        ));
    }
    let mut config = Zeroizing::new(format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\ndata-binary = {}\n",
        quote(&transport.url),
        quote(&transport.body)
    ));
    if let Some(credential) = transport.credential {
        config.push_str(&format!(
            "header = {}\n",
            quote(&format!("Authorization: Bearer {}", credential.as_str()))
        ));
    }
    let mut command = Command::new("/usr/bin/curl");
    command.args([
        "--disable",
        "--silent",
        "--fail",
        "--max-time",
        &transport.timeout_seconds.to_string(),
        "--connect-timeout",
        "3",
        "--max-filesize",
        "262144",
        "--write-out",
        "%{http_code}",
        "--max-redirs",
        "0",
        "--retry",
        "0",
        "--proto",
        "=http,https",
        "--proxy",
        "",
        "--noproxy",
        "*",
        "--config",
        "-",
    ]);
    if let Some(resolve) = transport.resolve {
        command.args(["--resolve", &resolve]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| failure("transport_start_failed", "start", "process_start_failed"))?;
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failure(
            "transport_stdin",
            "configuration_write",
            "stdin_unavailable",
        ));
    };
    let write = input.write_all(config.as_bytes());
    // EOF is required before curl can finish reading its configuration from standard input.
    drop(input);
    if write.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(failure(
            "transport_write_failed",
            "configuration_write",
            "stdin_write_failed",
        ));
    }
    let output = child
        .wait_with_output()
        .map_err(|_| failure("transport_failed", "wait", "process_wait_failed"))?;
    classify_observed_output(output, started, transport.timeout_seconds)
}

#[cfg(test)]
fn classify_output(
    output: std::process::Output,
    started: Instant,
    timeout_seconds: u64,
) -> Result<Vec<u8>, TransportFailure> {
    classify_observed_output(output, started, timeout_seconds).map(|response| response.bytes)
}
fn classify_observed_output(
    output: std::process::Output,
    started: Instant,
    timeout_seconds: u64,
) -> Result<RawProviderResponse, TransportFailure> {
    let suffix = output.stdout.len().checked_sub(3).and_then(|offset| {
        let status = &output.stdout[offset..];
        if !status.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let status = std::str::from_utf8(status).ok()?.parse::<u16>().ok()?;
        Some((offset, status))
    });
    let http_status =
        suffix.and_then(|(_, status)| (100..=599).contains(&status).then_some(status));
    if output.status.success()
        && output.stdout.len() <= 262147
        && http_status.is_some_and(|status| (200..300).contains(&status))
    {
        return Ok(RawProviderResponse {
            bytes: output.stdout[..suffix.unwrap().0].to_vec(),
            http_status: http_status.unwrap(),
            elapsed_millis: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        });
    }
    let code = output.status.code();
    let classification = match code {
        Some(28) => "client_deadline_exceeded",
        Some(22) => "http_status_rejected",
        Some(63) => "response_size_limit",
        Some(5 | 6) => "name_resolution_failed",
        Some(7) => "connection_failed",
        Some(35 | 51 | 58 | 60 | 77 | 83 | 90 | 91) => "tls_failed",
        Some(52) => "empty_response",
        Some(56) => "receive_failed",
        None => "process_terminated",
        Some(0) if output.stdout.len() > 262147 => "response_size_limit",
        Some(0) if http_status.is_some() => "http_status_rejected",
        Some(0) => "status_metadata_invalid",
        _ => "curl_failed",
    };
    let mut failure = transport_failure(
        "model_http_failed",
        "request",
        classification,
        started,
        timeout_seconds,
    );
    failure.diagnostic.curl_exit_code = code;
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        failure.diagnostic.signal = output.status.signal();
    }
    failure.diagnostic.http_status = http_status;
    failure.diagnostic.response_body_bytes = suffix.map(|(offset, _)| offset);
    Err(failure)
}
#[derive(Deserialize)]
struct ProviderResponse {
    choices: Vec<ProviderChoice>,
}
#[derive(Deserialize)]
struct ProviderChoice {
    finish_reason: String,
    message: ProviderMessage,
}
#[derive(Deserialize)]
struct ProviderMessage {
    content: String,
    tool_calls: Option<Value>,
}
pub fn decode_response(bytes: &[u8]) -> Result<Vec<u8>, NovaError> {
    // Known fields reject duplicates instead of silently taking the last value.
    let response: ProviderResponse =
        serde_json::from_slice(bytes).map_err(|_| error("model_response_invalid"))?;
    if response.choices.len() != 1 {
        return Err(error("model_response_invalid"));
    }
    let choice = &response.choices[0];
    if choice.finish_reason != "stop" || choice.message.tool_calls.is_some() {
        return Err(error("model_response_incomplete_or_tools"));
    }
    if choice.message.content.len() > MAX_MODEL_BYTES {
        return Err(error("model_response_limit"));
    }
    Ok(choice.message.content.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn reserved_addresses_are_denied() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("1.1.1.1".parse().unwrap()));
    }
    #[test]
    fn curl_configuration_cannot_inject_directives() {
        assert_eq!(quote("a\"\nurl = evil\\x"), "\"a\\\"\\nurl = evil\\\\x\"");
    }

    #[cfg(unix)]
    fn output(code: i32, stdout: &[u8]) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: b"Authorization: Bearer never-copy-credential\nhttp://never-copy-endpoint"
                .to_vec(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_retains_distinct_exit_status_and_partial_http_metadata() {
        let failed = classify_output(output(28, b"private-partial-body200"), Instant::now(), 45)
            .unwrap_err();
        assert_eq!(
            failed.error.to_string(),
            "invalid request: compiler: model_http_failed"
        );
        let diagnostic = serde_json::to_value(failed.diagnostic).unwrap();
        assert_eq!(diagnostic["classification"], "client_deadline_exceeded");
        assert_eq!(diagnostic["curl_exit_code"], 28);
        assert_eq!(diagnostic["http_status"], 200);
        assert_eq!(diagnostic["response_body_bytes"], 20);
        assert_eq!(diagnostic["timeout_seconds"], 45);
        assert_eq!(diagnostic["automatic_retries"], 0);
        let encoded = diagnostic.to_string();
        assert!(!encoded.contains("private-partial-body"));
        assert!(!encoded.contains("never-copy"));
    }

    #[cfg(unix)]
    #[test]
    fn http_error_is_distinct_from_timeout_and_unknown_status() {
        let failed = classify_output(output(22, b"500"), Instant::now(), 45).unwrap_err();
        let diagnostic = serde_json::to_value(failed.diagnostic).unwrap();
        assert_eq!(diagnostic["classification"], "http_status_rejected");
        assert_eq!(diagnostic["curl_exit_code"], 22);
        assert_eq!(diagnostic["http_status"], 500);
        assert_eq!(diagnostic["response_body_bytes"], 0);
        let failed = classify_output(output(28, b"000"), Instant::now(), 45).unwrap_err();
        assert!(serde_json::to_value(failed.diagnostic).unwrap()["http_status"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn response_limits_and_missing_metadata_fail_closed() {
        for (code, bytes, classification) in [
            (63, b"200".to_vec(), "response_size_limit"),
            (7, b"000".to_vec(), "connection_failed"),
            (0, b"bad".to_vec(), "status_metadata_invalid"),
            (0, b"302".to_vec(), "http_status_rejected"),
            (
                0,
                [vec![b'x'; 262145], b"200".to_vec()].concat(),
                "response_size_limit",
            ),
        ] {
            let failed = classify_output(output(code, &bytes), Instant::now(), 45).unwrap_err();
            assert_eq!(failed.diagnostic.classification, classification);
        }
        assert_eq!(
            classify_output(output(0, b"response-content200"), Instant::now(), 45).unwrap(),
            b"response-content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_signal_is_preserved_without_raw_diagnostic_text() {
        use std::os::unix::process::ExitStatusExt;
        let mut terminated = output(0, b"");
        terminated.status = std::process::ExitStatus::from_raw(9);
        let failed = classify_output(terminated, Instant::now(), 45).unwrap_err();
        assert_eq!(failed.diagnostic.signal, Some(9));
        assert_eq!(failed.diagnostic.curl_exit_code, None);
        assert_eq!(failed.diagnostic.classification, "process_terminated");
        assert!(
            !serde_json::to_string(&failed.diagnostic)
                .unwrap()
                .contains("never-copy")
        );
    }

    /// Actual system curl with only an isolated loopback fixture; never an installed model service.
    fn fixture_request(
        respond: impl FnOnce(&mut std::net::TcpStream) + Send + 'static,
        timeout_seconds: u64,
    ) -> Result<Vec<u8>, TransportFailure> {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 4096);
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let length: usize = String::from_utf8(headers)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 4096);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            respond(&mut stream);
        });
        let result = send_raw_diagnostic(Transport {
            url,
            resolve: None,
            body: Zeroizing::new("{\"fixture\":true}".into()),
            credential: None,
            timeout_seconds,
            output_mode: OutputMode::JsonObject,
        });
        server.join().unwrap();
        result
    }

    #[test]
    fn actual_curl_http_500_has_status_and_never_copies_error_body() {
        let failed = fixture_request(|stream| {
            let secret = b"never-copy-provider-error-body";
            write!(stream, "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", secret.len()).unwrap();
            let _ = stream.write_all(secret);
        }, 2).unwrap_err();
        assert_eq!(failed.diagnostic.curl_exit_code, Some(22));
        assert_eq!(failed.diagnostic.http_status, Some(500));
        assert_eq!(failed.diagnostic.classification, "http_status_rejected");
        assert!(
            !serde_json::to_string(&failed.diagnostic)
                .unwrap()
                .contains("never-copy")
        );
    }

    #[test]
    fn actual_curl_timeout_keeps_partial_http_status_and_does_not_retry() {
        let failed = fixture_request(|stream| {
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial").unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(1300));
        }, 1).unwrap_err();
        assert_eq!(failed.diagnostic.curl_exit_code, Some(28));
        assert_eq!(failed.diagnostic.http_status, Some(200));
        assert_eq!(failed.diagnostic.classification, "client_deadline_exceeded");
        assert_eq!(failed.diagnostic.automatic_retries, 0);
        assert_eq!(failed.diagnostic.response_body_bytes, Some(7));
        assert!(failed.diagnostic.elapsed_millis >= 900);
    }

    #[test]
    fn actual_curl_success_returns_only_body_with_literal_config_escaping() {
        let bytes = fixture_request(
            |stream| {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
                    )
                    .unwrap();
            },
            2,
        )
        .unwrap();
        assert_eq!(bytes, b"body");
    }
}
