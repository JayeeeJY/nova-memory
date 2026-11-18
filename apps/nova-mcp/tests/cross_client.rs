use nova_core::*;
use nova_mcp::generic::*;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;
fn input() -> SaveMemoryInput {
    serde_json::from_value(json!({"schema_version":"nova.capture.v0.3","assertion_origin":"ai_extracted_proposal","surface":"chat_text","capture_fidelity":"f1_ai_digest","user_requested_save":true,"request_id":"same-request","title":"Synthetic cross Host","memory_type":"decision","summary":"Synthetic scoped summary","session_digest":"Synthetic only","privacy_level":"private"})).unwrap()
}
#[tokio::test]
async fn independent_clients_namespace_saves_and_require_separate_scope_and_grant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let key_path = dir.path().join("key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    let mut vault = Vault::create(&path, key).unwrap();
    let codex = GenericMcpServer::new(&path, &key_path);
    let chatgpt = GenericMcpServer::new_chatgpt(&path, &key_path);
    let c = codex.save_memory(Parameters(input())).await.unwrap().0;
    let g = chatgpt.save_memory(Parameters(input())).await.unwrap().0;
    assert_ne!(c.memory_id, g.memory_id);
    assert_eq!(vault.pending_proposals().unwrap().len(), 2);
    let pending = query_spec_for(
        "chatgpt",
        "pending",
        "personal_memory_continuity",
        QuerySelection::Get {
            memory_id: c.memory_id.clone(),
            capture_status: true,
        },
    );
    assert!(
        vault
            .issue_disclosure_grant(
                pending,
                vec![c.memory_id.clone()],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    let memory = vault
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:founder".into(),
            proposal_id: c.memory_id,
        })
        .unwrap()
        .memory;
    let q = query_spec_for(
        "chatgpt",
        "read",
        "personal_memory_continuity",
        QuerySelection::Get {
            memory_id: memory.memory_id.clone(),
            capture_status: false,
        },
    );
    assert!(
        vault
            .issue_disclosure_grant(
                q.clone(),
                vec![memory.memory_id.clone()],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    let snapshot = vault
        .memory_access_snapshot("owner:founder", &memory.memory_id)
        .unwrap();
    vault
        .authorize_memory_client("owner:founder", &memory.memory_id, "chatgpt", &snapshot)
        .unwrap();
    let grant = vault
        .issue_disclosure_grant(
            q,
            vec![memory.memory_id.clone()],
            PrivacyLevel::Private,
            600,
        )
        .unwrap();
    let args=serde_json::from_value(json!({"schema_version":"nova.query.v0.1","request_id":"read","purpose":"personal_memory_continuity","approval_id":grant.approval_id,"memory_id":memory.memory_id,"view":"confirmed"})).unwrap();
    assert!(codex.get_memory(Parameters(args)).await.is_err());
    let args=serde_json::from_value(json!({"schema_version":"nova.query.v0.1","request_id":"read","purpose":"personal_memory_continuity","approval_id":grant.approval_id,"memory_id":memory.memory_id,"view":"confirmed"})).unwrap();
    let result = chatgpt.get_memory(Parameters(args)).await.unwrap().0;
    assert_eq!(result["receipt"]["recipient"], "chatgpt");
    assert_eq!(result["items"][0]["summary"], "Synthetic scoped summary");
    assert!(vault.verify_audit_chain().unwrap().valid);
}
