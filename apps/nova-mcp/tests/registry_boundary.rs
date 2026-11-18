use nova_core::*;
use nova_mcp::{
    generic::{GenericMcpServer, query_spec_bound},
    registry::{Principal, RegistryCredential},
    transport::{CredentialPolicy, protected_router},
};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
#[tokio::test]
async fn registry_principal_full_sdk_loop_and_live_revocation() -> anyhow::Result<()> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("vault.sqlite");
    let key = d.path().join("vault.key");
    let token_path = d.path().join("client.key");
    let registry = d.path().join("registry.json");
    let k = VaultKey::generate()?;
    k.write_new_file(&key)?;
    drop(Vault::create(&db, k)?);
    VaultKey::generate()?.write_new_file(&token_path)?;
    let principal = Principal {
        owner_id: "owner:synthetic-alice".into(),
        space_id: "work".into(),
        client_id: "cursor-fixture".into(),
        capabilities: vec!["capture".into(), "query".into()],
    };
    let mut config = json!({"schema_version":"nova.clients.v0.1","clients":[{"principal":principal,"token_file":token_path,"expires_at":"2099-01-01T00:00:00Z","revoked":false}]});
    write(&registry, &config);
    let (credential, p) = RegistryCredential::load(&registry, "cursor-fixture", &key)?;
    let bind = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = bind.local_addr()?;
    let cancel = CancellationToken::new();
    let router = protected_router(
        GenericMcpServer::with_binding(&db, &key, p.binding()?),
        CredentialPolicy::Registry(credential),
        addr.to_string(),
        cancel.clone(),
    );
    let handle = tokio::spawn(async move { axum::serve(bind, router).await });
    let token = std::fs::read_to_string(&token_path)?;
    let url = format!("http://{addr}/mcp");
    let c = ClientInfo::default()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(token.trim()),
        ))
        .await?;
    let args = json!({"schema_version":"nova.capture.v0.3","assertion_origin":"ai_extracted_proposal","surface":"chat_text","capture_fidelity":"f1_ai_digest","user_requested_save":true,"request_id":"registry-save","title":"Synthetic","memory_type":"decision","summary":"Synthetic registry principal","session_digest":"Synthetic only","privacy_level":"private"});
    let call = CallToolRequestParams::new("save_memory")
        .with_arguments(serde_json::from_value(args.clone())?);
    let saved = c.call_tool(call.clone()).await?.structured_content.unwrap();
    assert_eq!(saved["proposal_status"], "pending");
    let mut spoof = args.clone();
    spoof["owner_id"] = json!("owner:founder");
    assert_eq!(
        c.call_tool(
            CallToolRequestParams::new("save_memory")
                .with_arguments(serde_json::from_value(spoof)?)
        )
        .await?
        .is_error,
        Some(true)
    );
    let mut v = Vault::open(&db, VaultKey::read_file(&key)?)?;
    assert!(v.review_inbox("owner:founder")?.is_empty());
    let rows = v.review_inbox(&p.owner_id)?;
    assert_eq!(rows.len(), 1);
    let confirmed = v.review_proposal(ReviewRequest {
        request_id: Some("registry-confirm".into()),
        subject: p.owner_id.clone(),
        proposal_id: saved["proposal_id"].as_str().unwrap().into(),
        expected_snapshot: rows[0].snapshot.clone(),
        action: ReviewAction::Confirm,
    })?;
    let mid = confirmed.memory_id.unwrap();
    let spec = query_spec_bound(
        &p.binding()?,
        "registry-read",
        "personal_memory_continuity",
        QuerySelection::Get {
            memory_id: mid.clone(),
            capture_status: false,
        },
    );
    let grant =
        v.issue_disclosure_grant(spec.clone(), vec![mid.clone()], PrivacyLevel::Private, 600)?;
    let get=CallToolRequestParams::new("get_memory").with_arguments(serde_json::from_value(json!({"schema_version":"nova.query.v0.1","request_id":"registry-read","approval_id":grant.approval_id,"purpose":"personal_memory_continuity","memory_id":mid,"view":"confirmed"}))?);
    let result = c.call_tool(get.clone()).await?.structured_content.unwrap();
    assert_eq!(result["receipt"]["recipient"], "cursor-fixture");
    assert_eq!(result["receipt"]["envelope"]["subject"], p.owner_id);
    let mut wrong = spec;
    wrong.space = "personal".into();
    assert!(v.query_memory(wrong, &grant.approval_id).is_err());
    v.revoke_disclosure_grant(&p.owner_id, &grant.approval_id)?;
    assert_eq!(c.call_tool(get).await?.is_error, Some(true));
    config["clients"][0]["revoked"] = json!(true);
    write(&registry, &config);
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(token.trim())
        .json(&json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}))
        .send()
        .await?;
    assert_eq!(response.status(), 401);
    drop(c);
    cancel.cancel();
    handle.abort();
    let _ = handle.await;
    assert!(v.verify_audit_chain()?.valid);
    Ok(())
}
#[test]
fn registry_rejects_expiry_duplicates_capability_changes_and_identity_rebinding() {
    let d = tempfile::tempdir().unwrap();
    let key = d.path().join("key.key");
    let token = d.path().join("token.key");
    let registry = d.path().join("registry.json");
    VaultKey::generate().unwrap().write_new_file(&key).unwrap();
    VaultKey::generate()
        .unwrap()
        .write_new_file(&token)
        .unwrap();
    let base = json!({"schema_version":"nova.clients.v0.1","clients":[{"principal":{"owner_id":"owner:a","space_id":"personal","client_id":"claude-code","capabilities":["capture"]},"token_file":token,"expires_at":"2099-01-01T00:00:00Z","revoked":false}]});
    write(&registry, &base);
    let (auth, _) = RegistryCredential::load(&registry, "claude-code", &key).unwrap();
    let secret = std::fs::read_to_string(&token).unwrap();
    let header = format!("Bearer {}", secret.trim());
    assert!(auth.permits(Some(&header)));
    for i in 0..6 {
        let mut c = base.clone();
        match i {
            0 => c["clients"][0]["expires_at"] = json!("2000-01-01T00:00:00Z"),
            1 => c["clients"][0]["revoked"] = json!(true),
            2 => c["clients"][0]["principal"]["owner_id"] = json!("owner:b"),
            3 => c["clients"][0]["principal"]["capabilities"] = json!(["query"]),
            4 => {
                let row = c["clients"][0].clone();
                c["clients"].as_array_mut().unwrap().push(row);
            }
            _ => c["clients"][0]["principal"]["capabilities"] = json!(["confirm"]),
        };
        write(&registry, &c);
        assert!(!auth.permits(Some(&header)));
    }
    write(&registry, &base);
    assert!(!auth.permits(None));
    assert!(!auth.permits(Some("Bearer forged")));
}
