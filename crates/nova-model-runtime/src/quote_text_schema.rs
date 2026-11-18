//! Experimental quote-text generation contract, separate from all runtime defaults.
//! The resolver supplies evidence positions only after a unique exact match.
use serde_json::{Value, json};

pub const SCHEMA_VERSION: &str = "nova.extraction.quote-text.v0.1";
pub const PROVIDER_VERSION: &str = "nova.extraction.quote-text.ollama.v0.1";
pub const PROMPT_VERSION: &str = "nova.extraction.quote-text-prompt.v0.1";
pub const SCHEMA_JSON: &str = include_str!("../../../apps/nova-review/src/quote-text-schema.json");
pub const PROMPT: &str = include_str!("../../../apps/nova-review/src/quote-text-prompt.txt");
pub const PROMPT_V02_VERSION: &str = "nova.extraction.quote-text-prompt.v0.2";
pub const PROMPT_V02: &str = include_str!("../../../apps/nova-review/src/quote-text-prompt-v02.txt");
pub const PROMPT_V03_VERSION: &str = "nova.extraction.quote-text-prompt.v0.3";
pub const PROMPT_V03: &str = include_str!("../../../apps/nova-review/src/quote-text-prompt-v03.txt");
pub const PROMPT_V04_VERSION: &str = "nova.extraction.quote-text-prompt.v0.4";
pub const PROMPT_V04: &str = include_str!("../../../apps/nova-review/src/quote-text-prompt-v04.txt");
pub const PROMPT_V05_VERSION: &str = "nova.extraction.quote-text-prompt.v0.5";
pub const PROMPT_V05: &str = include_str!("../../../apps/nova-review/src/quote-text-prompt-v05.txt");
pub const GENERAL_PROMPT_V10_VERSION: &str = "nova.extraction.quote-text-general-prompt.v0.10";
pub const GENERAL_PROMPT_V10: &str =
    include_str!("../../../apps/nova-review/src/quote-text-prompt-general-v10.txt");
pub const LINKED_PROVIDER_VERSION: &str = "nova.extraction.quote-text.ollama.linked-rule.v0.4";
pub const CONDITION_LIST_PROVIDER_VERSION: &str = "nova.extraction.quote-text.ollama.condition-list.v0.1";
const LINKED_CONDITIONS: &str =
    include_str!("../../../apps/nova-review/src/quote-text-linked-conditions-v04.json");

pub fn schema() -> Value {
    serde_json::from_str(SCHEMA_JSON).expect("embedded quote-text schema is valid")
}

/// Only the provider generation maximum is removed. The resolver still enforces
/// 4096 UTF-8 bytes, and the unchanged core repeats its own evidence checks.
pub fn provider_schema() -> Value {
    let mut value = schema();
    let removed = value
        .pointer_mut("/$defs/quote/properties/quote")
        .and_then(Value::as_object_mut)
        .expect("fixed quote schema")
        .remove("maxLength");
    assert_eq!(removed, Some(json!(4096)), "quote-text schema drift");
    value
}

pub fn response_format() -> Value {
    json!({"type":"json_schema", "json_schema":{
        "name":"nova_quote_text_v0_1", "schema":provider_schema()
    }})
}

pub fn linked_provider_schema() -> Value {
    let mut value = provider_schema();
    value["properties"]["units"]["maxItems"] = json!(1);
    value["$defs"]["content"]["properties"]["conditions"] =
        serde_json::from_str(LINKED_CONDITIONS).expect("embedded linked conditions schema");
    value
}

pub fn linked_response_format() -> Value {
    json!({"type":"json_schema", "json_schema":{
        "name":"nova_quote_text_linked_v0_4", "schema":linked_provider_schema()
    }})
}

pub fn condition_list_provider_schema() -> Value {
    let mut value = provider_schema();
    let content = &mut value["$defs"]["content"]["properties"];
    let key_pattern = content["predicate"]["pattern"].clone();
    content["conditions"] = json!({
        "type":"array", "minItems":0, "maxItems":16,
        "items":{
            "type":"object", "additionalProperties":false, "required":["key","value"],
            "properties":{
                "key":{"type":"string","pattern":key_pattern},
                "value":{"$ref":"#/$defs/json_value"}
            }
        }
    });
    value
}

pub fn condition_list_response_format() -> Value {
    json!({"type":"json_schema", "json_schema":{
        "name":"nova_quote_text_condition_list_v0_1", "schema":condition_list_provider_schema()
    }})
}
