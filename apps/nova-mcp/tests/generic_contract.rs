use nova_core::*;
use nova_mcp::generic::*;
use nova_mcp::{NovaMcpServer, PrepareContextPackInput};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::json;
struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    key: std::path::PathBuf,
    server: GenericMcpServer,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let key = dir.path().join("test.key");
        let k = VaultKey::generate().unwrap();
        k.write_new_file(&key).unwrap();
        drop(Vault::create(&path, k).unwrap());
        Self {
            server: GenericMcpServer::new(&path, &key),
            _dir: dir,
            path,
            key,
        }
    }
    fn vault(&self) -> Vault {
        Vault::open(&self.path, VaultKey::read_file(&self.key).unwrap()).unwrap()
    }
    fn grant(
        &self,
        request_id: &str,
        selection: QuerySelection,
        ids: Vec<String>,
    ) -> DisclosureGrant {
        self.vault()
            .issue_disclosure_grant(
                query_spec(request_id, "personal_memory_continuity", selection),
                ids,
                PrivacyLevel::Private,
                600,
            )
            .unwrap()
    }
    fn get_input(
        &self,
        request_id: &str,
        id: &str,
        status: bool,
        approval: &str,
    ) -> GetMemoryInput {
        serde_json::from_value(json!({"schema_version":"nova.query.v0.1","request_id":request_id,"approval_id":approval,"purpose":"personal_memory_continuity","memory_id":id,"view":if status{"capture_status"}else{"confirmed"}})).unwrap()
    }
}
fn input() -> SaveMemoryInput {
    serde_json::from_value(json!({"schema_version":"nova.capture.v0.3","assertion_origin":"model_inferred","surface":"chat_text","capture_fidelity":"f1_ai_digest","user_requested_save":true,"request_id":"synthetic-save-1","title":"Synthetic P1 decision","memory_type":"decision","summary":"Synthetic P1 scope decision","session_digest":"Synthetic agreed scope","privacy_level":"private","occurred_at":null})).unwrap()
}
async fn save(f: &Fixture) -> SaveMemoryOutput {
    f.server.save_memory(Parameters(input())).await.unwrap().0
}
fn confirm(f: &Fixture, id: &str) -> CanonicalMemory {
    f.vault()
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:founder".into(),
            proposal_id: id.into(),
        })
        .unwrap()
        .memory
}
#[tokio::test]
async fn proposal_only_restart_replay_and_conflict() {
    let f = Fixture::new();
    let first = save(&f).await;
    assert!(first.needs_confirmation);
    assert_eq!(first.proposal_status, "pending");
    assert_eq!(first.occurred_at, None);
    assert!(f.vault().memories().unwrap().is_empty());
    let before = f.vault().verify_audit_chain().unwrap();
    let restarted = GenericMcpServer::new(&f.path, &f.key);
    let retry = restarted.save_memory(Parameters(input())).await.unwrap().0;
    assert_eq!(first.receipt_id, retry.receipt_id);
    assert_eq!(first.memory_id, retry.memory_id);
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
    let mut changed = input();
    changed.summary = "different".into();
    assert!(
        f.server
            .save_memory(Parameters(changed))
            .await
            .err()
            .unwrap()
            .starts_with("NOVA_IDEMPOTENCY_CONFLICT")
    );
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
}
#[tokio::test]
async fn save_rejects_spoofed_contract_and_invalid_evidence_without_writes() {
    let f = Fixture::new();
    let base = serde_json::to_value(input()).unwrap();
    for (field, value) in [
        ("host", json!("codex")),
        ("owner", json!("owner:other")),
        ("schema_version", json!("nova.capture.v9")),
        ("assertion_origin", json!("source_evidence")),
        ("capture_fidelity", json!("f2_visible_transcript")),
    ] {
        let mut v = base.clone();
        v[field] = value;
        assert!(serde_json::from_value::<SaveMemoryInput>(v).is_err());
    }
    let before = f.vault().verify_audit_chain().unwrap();
    for variant in 0..5 {
        let mut i = input();
        match variant {
            0 => i.user_requested_save = false,
            1 => i.source_ref = Some("https://secret.example/path".into()),
            2 => i.occurred_at = Some("yesterday".into()),
            3 => i.session_digest = "字".repeat(9000),
            _ => i.request_id = "x".repeat(97),
        };
        assert!(f.server.save_memory(Parameters(i)).await.is_err());
    }
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
}
#[tokio::test]
async fn exact_granted_status_then_confirmed_summary_and_new_session() {
    let f = Fixture::new();
    let captured = save(&f).await;
    let grant = f.grant(
        "status",
        QuerySelection::Get {
            memory_id: captured.memory_id.clone(),
            capture_status: true,
        },
        vec![captured.memory_id.clone()],
    );
    let status = f
        .server
        .get_memory(Parameters(f.get_input(
            "status",
            &captured.memory_id,
            true,
            &grant.approval_id,
        )))
        .await
        .unwrap()
        .0;
    assert_eq!(status["items"][0]["status"], "pending");
    assert!(status.get("source").is_none());
    assert_eq!(status["items"][0]["capture_fidelity"], "f1_ai_digest");
    assert_eq!(
        status["items"][0]["source_artifact_ids"][0],
        captured.source_id
    );
    let before = f.vault().verify_audit_chain().unwrap();
    let replay = f
        .server
        .get_memory(Parameters(f.get_input(
            "status",
            &captured.memory_id,
            true,
            &grant.approval_id,
        )))
        .await
        .unwrap()
        .0;
    assert_eq!(status, replay);
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
    let memory = confirm(&f, &captured.memory_id);
    assert_eq!(
        memory.assertion_origin,
        AssertionOrigin::UserConfirmedInference
    );
    assert!(
        f.server
            .get_memory(Parameters(f.get_input(
                "status",
                &captured.memory_id,
                true,
                &grant.approval_id
            )))
            .await
            .is_err()
    );
    let grant = f.grant(
        "read",
        QuerySelection::Get {
            memory_id: captured.memory_id.clone(),
            capture_status: false,
        },
        vec![captured.memory_id.clone()],
    );
    let restarted = GenericMcpServer::new(&f.path, &f.key);
    let result = restarted
        .get_memory(Parameters(f.get_input(
            "read",
            &captured.memory_id,
            false,
            &grant.approval_id,
        )))
        .await
        .unwrap()
        .0;
    assert_eq!(result["items"][0]["canonical_memory_id"], memory.memory_id);
    assert_eq!(
        result["items"][0]["assertion_origin"],
        "user_confirmed_inference"
    );
    assert_eq!(result["receipt"]["recipient"], "codex");
}
#[tokio::test]
async fn grant_cannot_expand_ids_operation_request_purpose_or_view() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let grant = f.grant(
        "status",
        QuerySelection::Get {
            memory_id: saved.memory_id.clone(),
            capture_status: true,
        },
        vec![saved.memory_id.clone()],
    );
    let before = f.vault().verify_audit_chain().unwrap();
    for variant in 0..5 {
        let mut i = f.get_input("status", &saved.memory_id, true, &grant.approval_id);
        match variant {
            0 => i.memory_id = "proposal_unknown".into(),
            1 => i.request_id = "other".into(),
            2 => i.purpose = "different".into(),
            3 => i.view = QueryView::Confirmed,
            _ => i.approval_id = "approval_unknown".into(),
        };
        assert!(
            f.server
                .get_memory(Parameters(i))
                .await
                .err()
                .unwrap()
                .starts_with("NOVA_RESOURCE_UNAVAILABLE")
        );
    }
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
    let mut spec = grant.spec.clone();
    spec.client = "chatgpt".into();
    assert!(f.vault().query_memory(spec, &grant.approval_id).is_err());
}
#[tokio::test]
async fn search_is_fixed_scope_confirmed_only_and_legacy_cannot_read_codex() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let memory = confirm(&f, &saved.memory_id);
    let mut pending = input();
    pending.request_id = "pending2".into();
    let pending = f.server.save_memory(Parameters(pending)).await.unwrap().0;
    let spec = QuerySelection::Search {
        query: "Synthetic".into(),
        max_items: 5,
    };
    assert!(
        f.vault()
            .issue_disclosure_grant(
                query_spec("badsearch", "personal_memory_continuity", spec.clone()),
                vec![pending.memory_id],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    assert!(
        f.vault()
            .issue_disclosure_grant(
                query_spec("duplicate", "personal_memory_continuity", spec.clone()),
                vec![memory.memory_id.clone(), memory.memory_id.clone()],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    let grant = f.grant("search", spec, vec![memory.memory_id.clone()]);
    let i = SearchMemoryInput {
        schema_version: QueryVersion::V01,
        request_id: "search".into(),
        approval_id: grant.approval_id,
        purpose: "personal_memory_continuity".into(),
        query: "Synthetic".into(),
        max_items: 5,
    };
    let result = f.server.search_memory(Parameters(i)).await.unwrap().0;
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["canonical_memory_id"], memory.memory_id);
    let legacy = NovaMcpServer::new(&f.path, &f.key);
    let ci:PrepareContextPackInput=serde_json::from_value(json!({"request_id":"legacy","surface":"chat_text","user_approved":true,"purpose":"personal_memory_continuity","query":"Synthetic","maximum_privacy_level":"private","max_items":5,"expires_in_seconds":600})).unwrap();
    assert!(
        legacy
            .prepare_context_pack(Parameters(ci))
            .await
            .unwrap()
            .0
            .items
            .is_empty()
    );
}
#[tokio::test]
async fn revoked_grant_and_disputed_or_deleted_records_never_replay() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let m = confirm(&f, &saved.memory_id);
    let grant = f.grant(
        "read",
        QuerySelection::Get {
            memory_id: m.memory_id.clone(),
            capture_status: false,
        },
        vec![m.memory_id.clone()],
    );
    let read = f.get_input("read", &m.memory_id, false, &grant.approval_id);
    f.server.get_memory(Parameters(read.clone())).await.unwrap();
    f.vault()
        .dispute_memory(DisputeRequest {
            request_id: None,
            subject: "owner:founder".into(),
            memory_id: m.memory_id.clone(),
            reason: "Synthetic dispute".into(),
        })
        .unwrap();
    assert!(f.server.get_memory(Parameters(read.clone())).await.is_err());
    f.vault()
        .delete_memory("owner:founder", &m.memory_id)
        .unwrap();
    assert!(f.server.get_memory(Parameters(read)).await.is_err());
    let mut i = input();
    i.request_id = "second".into();
    let saved = f.server.save_memory(Parameters(i)).await.unwrap().0;
    let grant = f.grant(
        "status2",
        QuerySelection::Get {
            memory_id: saved.memory_id.clone(),
            capture_status: true,
        },
        vec![saved.memory_id.clone()],
    );
    let read = f.get_input("status2", &saved.memory_id, true, &grant.approval_id);
    f.server.get_memory(Parameters(read.clone())).await.unwrap();
    f.vault()
        .revoke_disclosure_grant("owner:founder", &grant.approval_id)
        .unwrap();
    assert!(f.server.get_memory(Parameters(read)).await.is_err());
}
#[tokio::test]
async fn superseded_version_reports_link_but_never_old_or_unapproved_new_content() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let m = confirm(&f, &saved.memory_id);
    let successor = f
        .vault()
        .correct_memory(CorrectionRequest {
            request_id: None,
            subject: "owner:founder".into(),
            memory_id: m.memory_id.clone(),
            title: None,
            summary: "Synthetic correction".into(),
            reason: "Synthetic reason".into(),
        })
        .unwrap()
        .memory;
    let grant = f.grant(
        "old",
        QuerySelection::Get {
            memory_id: saved.memory_id.clone(),
            capture_status: false,
        },
        vec![saved.memory_id.clone()],
    );
    let result = f
        .server
        .get_memory(Parameters(f.get_input(
            "old",
            &saved.memory_id,
            false,
            &grant.approval_id,
        )))
        .await
        .unwrap()
        .0;
    assert_eq!(result["items"][0]["status"], "superseded");
    assert!(result["items"][0]["summary"].is_null());
    assert_eq!(
        result["items"][0]["active_successor_ids"][0],
        successor.memory_id
    );
    assert!(
        f.server
            .get_memory(Parameters(f.get_input(
                "old",
                &successor.memory_id,
                false,
                &grant.approval_id
            )))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn sensitive_capture_and_legacy_records_do_not_gain_codex_read_authority() {
    let f = Fixture::new();
    let mut i = input();
    i.privacy_level = nova_mcp::ToolPrivacyLevel::Sensitive;
    let saved = f.server.save_memory(Parameters(i)).await.unwrap().0;
    assert!(
        f.vault()
            .issue_disclosure_grant(
                query_spec(
                    "status",
                    "personal_memory_continuity",
                    QuerySelection::Get {
                        memory_id: saved.memory_id.clone(),
                        capture_status: true
                    }
                ),
                vec![saved.memory_id],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    let legacy = nova_mcp::SaveVoiceSessionDigestInput {
        user_requested_save: true,
        request_id: "legacy-own".into(),
        title: "Synthetic legacy".into(),
        summary: "Synthetic legacy".into(),
        session_digest: "Synthetic legacy".into(),
        privacy_level: nova_mcp::ToolPrivacyLevel::Private,
        ..Default::default()
    };
    let legacy = NovaMcpServer::new(&f.path, &f.key)
        .save_voice_session_digest(Parameters(legacy))
        .await
        .unwrap()
        .0;
    assert!(
        f.vault()
            .issue_disclosure_grant(
                query_spec(
                    "legacy-read",
                    "personal_memory_continuity",
                    QuerySelection::Get {
                        memory_id: legacy.proposal_id.clone(),
                        capture_status: true
                    }
                ),
                vec![legacy.proposal_id],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
}
#[tokio::test]
async fn exact_namespaces_and_request_bounds_preserve_unknown_time() {
    assert!(namespaced_request("save", &"字".repeat(32)).len() <= 128);
    assert_ne!(
        namespaced_request("save", "same"),
        namespaced_request("query", "same")
    );
    let f = Fixture::new();
    let mut i = input();
    i.request_id = "字".repeat(32);
    let saved = f.server.save_memory(Parameters(i)).await.unwrap().0;
    assert!(saved.occurred_at.is_none());
    let p = f.vault().pending_proposals().unwrap().remove(0);
    assert_eq!(
        p.permission_scope,
        vec!["personal_review", "client:codex", "space:personal"]
    );
}

#[tokio::test]
async fn rejected_status_is_minimum_and_save_replay_reports_current_lifecycle() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let mut vault = f.vault();
    let row = vault.review_inbox("owner:founder").unwrap().remove(0);
    vault
        .review_proposal(ReviewRequest {
            request_id: Some("reject".into()),
            subject: "owner:founder".into(),
            proposal_id: saved.memory_id.clone(),
            expected_snapshot: row.snapshot,
            action: ReviewAction::Reject {
                reason: "synthetic rejection".into(),
            },
        })
        .unwrap();
    let grant = f.grant(
        "rejected",
        QuerySelection::Get {
            memory_id: saved.memory_id.clone(),
            capture_status: true,
        },
        vec![saved.memory_id.clone()],
    );
    let result = f
        .server
        .get_memory(Parameters(f.get_input(
            "rejected",
            &saved.memory_id,
            true,
            &grant.approval_id,
        )))
        .await
        .unwrap()
        .0;
    assert_eq!(result["items"][0]["status"], "rejected");
    assert!(result["items"][0]["summary"].is_null());
    let replay = save(&f).await;
    assert_eq!(replay.proposal_status, "rejected");
    assert!(!replay.needs_confirmation);
    assert_eq!(replay.receipt_id, saved.receipt_id);
}
#[tokio::test]
async fn expired_grant_denies_replay_and_confirmed_capture_replay_links_canonical() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let m = confirm(&f, &saved.memory_id);
    let replay = save(&f).await;
    assert_eq!(replay.canonical_memory_id, Some(m.memory_id.clone()));
    assert!(!replay.needs_confirmation);
    let grant = f
        .vault()
        .issue_disclosure_grant(
            query_spec(
                "expiry",
                "personal_memory_continuity",
                QuerySelection::Get {
                    memory_id: m.memory_id.clone(),
                    capture_status: false,
                },
            ),
            vec![m.memory_id.clone()],
            PrivacyLevel::Private,
            1,
        )
        .unwrap();
    let read = f.get_input("expiry", &m.memory_id, false, &grant.approval_id);
    f.server.get_memory(Parameters(read.clone())).await.unwrap();
    let before = f.vault().verify_audit_chain().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(f.server.get_memory(Parameters(read)).await.is_err());
    assert_eq!(before, f.vault().verify_audit_chain().unwrap());
}
#[tokio::test]
async fn owner_cli_issues_revokes_and_never_discloses_client_token() {
    let f = Fixture::new();
    let saved = save(&f).await;
    let token = f._dir.path().join("client.key");
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nova-authorize"))
            .args([
                "--vault",
                f.path.to_str().unwrap(),
                "--key-file",
                f.key.to_str().unwrap(),
            ])
            .args(args)
            .output()
            .unwrap()
    };
    let result = run(&["create-client-token", "--file", token.to_str().unwrap()]);
    assert!(result.status.success());
    let content = std::fs::read_to_string(&token).unwrap();
    assert!(!String::from_utf8_lossy(&result.stdout).contains(content.trim()));
    assert!(nova_mcp::transport::ClientCredential::read_for_vault(&token, &f.key).is_ok());
    assert!(nova_mcp::transport::ClientCredential::read_for_vault(&f.key, &f.key).is_err());
    let request = f._dir.path().join("approval.json");
    std::fs::write(&request,json!({"request_id":"cli-status","purpose":"personal_memory_continuity","selection":{"kind":"get","memory_id":saved.memory_id,"capture_status":true},"allowed_ids":[saved.memory_id],"maximum_privacy":"private","ttl_seconds":600}).to_string()).unwrap();
    assert!(
        !run(&["issue", "--request-file", request.to_str().unwrap()])
            .status
            .success()
    );
    let result = run(&[
        "issue",
        "--request-file",
        request.to_str().unwrap(),
        "--acknowledge",
    ]);
    assert!(result.status.success());
    let grant: DisclosureGrant = serde_json::from_slice(&result.stdout).unwrap();
    f.server
        .get_memory(Parameters(f.get_input(
            "cli-status",
            &saved.memory_id,
            true,
            &grant.approval_id,
        )))
        .await
        .unwrap();
    assert!(
        run(&[
            "revoke",
            "--approval-id",
            &grant.approval_id,
            "--acknowledge"
        ])
        .status
        .success()
    );
    assert!(
        f.server
            .get_memory(Parameters(f.get_input(
                "cli-status",
                &saved.memory_id,
                true,
                &grant.approval_id
            )))
            .await
            .is_err()
    );
}
