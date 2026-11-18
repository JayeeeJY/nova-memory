//! Export the real existing wire schemas; no Vault or network access.
use nova_mcp::{PrepareContextPackInput, SaveToNovaInput, SaveVoiceSessionDigestInput};
use rmcp::schemars::schema_for;
fn main() {
    let schemas = serde_json::json!({
        "status": "current_runtime_contract_not_generic_v01",
        "save_to_nova": schema_for!(SaveToNovaInput),
        "save_voice_session_digest": schema_for!(SaveVoiceSessionDigestInput),
        "prepare_context_pack": schema_for!(PrepareContextPackInput),
    });
    println!("{}", serde_json::to_string_pretty(&schemas).unwrap());
}
