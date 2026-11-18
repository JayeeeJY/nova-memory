//! Offline-only contract/check utility. No model transport, Vault or file inputs.
use nova_core::compilation::{MAX_DOCUMENT_BYTES, decode_untrusted_json};
use nova_review::{quote_text, quote_text_schema};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{self, Read};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    source_text: String,
    model_output: String,
}

fn run() -> Result<Value, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--describe"] {
        return Ok(json!({
            "schema_version":"nova.quote-text.offline-check.v0.1",
            "generation_contract":quote_text_schema::SCHEMA_VERSION,
            "provider_schema_version":quote_text_schema::PROVIDER_VERSION,
            "canonical_schema":quote_text_schema::schema(),
            "provider_schema":quote_text_schema::provider_schema(),
            "prompt_version":quote_text_schema::PROMPT_VERSION,
            "prompt":quote_text_schema::PROMPT,
            "prompt_sha256":format!("{:x}",Sha256::digest(quote_text_schema::PROMPT.as_bytes())),
            "model_dispatch_supported":false, "vault_access_supported":false,
            "semantic_acceptance":"pending"
        }));
    }
    if args != ["--check"] {
        return Err("usage_describe_or_check_stdin_only".into());
    }
    let mut bytes = Vec::new();
    io::stdin().take((MAX_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes).map_err(|_| "stdin_read_failed")?;
    let input: Input = decode_untrusted_json(&bytes).map_err(|_| "invalid_check_input")?;
    let resolved = quote_text::resolve(&input.source_text, input.model_output.as_bytes())
        .map_err(|failure| failure.code().to_owned())?;
    Ok(json!({
        "schema_version":"nova.quote-text.offline-check.v0.1",
        "source_sha256":resolved.source_sha256(),
        "raw_output_sha256":resolved.raw_sha256(),
        "resolved_output_sha256":resolved.resolved_sha256(),
        "resolved_output":std::str::from_utf8(resolved.resolved_bytes()).expect("serialized JSON is UTF-8"),
        "quote_proofs":resolved.proofs(),
        "semantic_acceptance":"pending", "core_committed":false,
        "model_dispatches":0, "vault_accessed":false
    }))
}

fn main() {
    match run() {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).expect("serializable report")),
        Err(code) => {
            println!("{}", json!({"error_code":code, "model_dispatches":0, "core_committed":false}));
            std::process::exit(1);
        }
    }
}
