//! No Vault/network access. Export the schemas actually used by the registered generic tools.
use nova_mcp::generic::{GetMemoryInput, SaveMemoryInput, SaveMemoryOutput, SearchMemoryInput};
use rmcp::schemars::schema_for;
fn main() {
    let contract = serde_json::json!({
        "status":"implemented_local_contract_real_codex_pending",
        "save_memory":schema_for!(SaveMemoryInput),
        "save_memory_output":schema_for!(SaveMemoryOutput),
        "get_memory":schema_for!(GetMemoryInput),
        "search_memory":schema_for!(SearchMemoryInput),
        "query_output":{
            "description":"nova.query.result.v0.1: minimum items plus encrypted/audited ContextDeliveryReceipt; no raw Source",
            "required":["schema_version","items","receipt"],
            "additionalProperties":false,
            "type":"object",
            "properties":{
                "schema_version":{"const":"nova.query.result.v0.1"},
                "items":{"type":"array","maxItems":10,"items":{"type":"object","additionalProperties":false,"required":["memory_id","canonical_memory_id","active_successor_ids","status","title","summary","memory_type","assertion_origin","capture_fidelity","source_artifact_ids","occurred_at"],"properties":{
                    "memory_id":{"type":"string"},"canonical_memory_id":{"type":["string","null"]},"active_successor_ids":{"type":"array","items":{"type":"string"}},"status":{"enum":["pending","confirmed","rejected","active","superseded"]},"title":{"type":["string","null"]},"summary":{"type":["string","null"]},"memory_type":{"type":"string"},"assertion_origin":{"type":"string"},"capture_fidelity":{"type":"string"},"source_artifact_ids":{"type":"array","items":{"type":"string"}},"occurred_at":{"type":["string","null"]}
                }}},
                "receipt":{"type":"object","required":["envelope","context_pack_id","recipient","surface","disclosed_memory_ids","disclosed_fields","expires_at"]}
            }
        }
    });
    println!("{}", serde_json::to_string_pretty(&contract).unwrap());
}
