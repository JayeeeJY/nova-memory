//! Fixed structural constraints for automatic extraction generation.
//!
//! This schema deliberately preserves the open namespace/value representation.
//! It does not prove source support, semantic accuracy or permission to commit.
//! `ExtractionOutput::parse` and the existing core commit gates remain authoritative.
use serde_json::{Value, json};

pub const SCHEMA_CONTRACT_VERSION: &str = "nova.extraction.generation-schema.v0.1";
pub const SCHEMA_JSON: &str = include_str!("../../../apps/nova-review/src/compiler-schema.json");
pub const PROVIDER_SCHEMA_CONTRACT_VERSION: &str = "nova.extraction.ollama-schema.v0.1";
pub const PROVIDER_SCHEMA_REMOVED_POINTER: &str = "/$defs/quote/properties/quote/maxLength";

/// Return the fixed schema shipped with this binary; callers cannot supply overrides.
pub fn schema() -> Value {
    serde_json::from_str(SCHEMA_JSON).expect("embedded extraction schema is valid JSON")
}

/// The fixed Ollama 0.20.2 generation projection of the canonical schema.
///
/// Its grammar parser rejects finite repetitions above 2000. Removing just this
/// generation bound leaves `minLength: 1` intact and emits an unbounded repetition.
/// This does not change the canonical schema's 4096-character bound or the stricter
/// core commit gate's 4096 UTF-8-byte bound. Successful generation alone is not a
/// validation or commit claim.
pub fn provider_schema() -> Value {
    let mut projected = schema();
    let (parent, field) = PROVIDER_SCHEMA_REMOVED_POINTER
        .rsplit_once('/')
        .expect("fixed projection pointer has a parent");
    let removed = projected
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("fixed canonical quote schema is an object")
        .remove(field);
    assert_eq!(removed, Some(json!(4096)), "canonical quote bound changed");
    projected
}

/// Compact deterministic JSON bytes for independent provider-schema hash binding.
pub fn provider_schema_json() -> String {
    serde_json::to_string(&provider_schema()).expect("fixed provider schema serializes")
}

/// OpenAI-compatible structured output request, including Ollama's supported schema field.
///
/// No `strict` flag is used as an enforcement claim. Ollama 0.20.2 forwards
/// `json_schema.schema` to its grammar converter; independent validation is still required.
pub fn extraction_response_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "nova_extraction_v0_1",
            "schema": provider_schema()
        }
    })
}
