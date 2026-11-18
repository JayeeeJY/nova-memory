use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, MemoryType, PrivacyLevel,
    ReviewTier, Vault, VaultKey,
};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;

fn setup() -> (
    tempfile::TempDir,
    axum::Router,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let directory = tempfile::tempdir().unwrap();
    let vault_path = directory.path().join("vault.sqlite");
    let key_path = directory.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    let mut vault = Vault::create(&vault_path, key).unwrap();
    vault
        .capture(CaptureRequest {
            structure: None,
            request_id: Some("http_review_seed".into()),
            owner_id: "owner:founder".into(),
            source_platform: "chatgpt".into(),
            capture_channel: CaptureChannel::RemoteMcp,
            capture_fidelity: CaptureFidelity::F1AiDigest,
            source_content: "synthetic source".into(),
            title: "Synthetic pending".into(),
            memory_type: MemoryType::Decision,
            summary: "Test proposal".into(),
            decisions: vec![],
            privacy_level: PrivacyLevel::Private,
            proposed_origin: AssertionOrigin::ModelInferred,
            review_tier: ReviewTier::Attention,
            permission_scope: vec!["personal_review".into()],
            purpose_limits: vec![],
            observed_at: None,
        })
        .unwrap();
    let router = ReviewServer::new(
        vault_path.clone(),
        key_path.clone(),
        "127.0.0.1:3031".into(),
        false,
    )
    .router();
    (directory, router, vault_path, key_path)
}
fn request(
    method: &str,
    path: &str,
    origin: Option<&str>,
    guard: bool,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:3031");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    if guard {
        builder = builder.header("x-nova-review", "owner-review");
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap()
}
async fn body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 100_000).await.unwrap()).unwrap()
}
#[tokio::test]
async fn license_page_uses_the_existing_local_boundary_without_vault_disclosure() {
    let (_directory, router, vault_path, key_path) = setup();
    let before = std::fs::read(&vault_path).unwrap();
    for (path, content_type, expected) in [
        ("/app/about", "text/html; charset=utf-8", "AGPL-3.0-only"),
        ("/app/license.txt", "text/plain; charset=utf-8", "GNU AFFERO GENERAL PUBLIC LICENSE"),
    ] {
        let response = router.clone().oneshot(request("GET", path, None, false, None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], content_type);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let raw = to_bytes(response.into_body(), 100_000).await.unwrap();
        let text = std::str::from_utf8(&raw).unwrap();
        assert!(text.contains(expected));
        assert!(!text.contains("synthetic source"));
        assert!(!text.contains(std::str::from_utf8(&std::fs::read(&key_path).unwrap()).unwrap()));

        let cross_site = Request::builder().uri(path)
            .header("host", "127.0.0.1:3031")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty()).unwrap();
        assert_eq!(router.clone().oneshot(cross_site).await.unwrap().status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(std::fs::read(vault_path).unwrap(), before);
}

#[tokio::test]
async fn same_origin_guard_blocks_rebinding_cross_site_and_unguarded_reads() {
    let (_directory, router, vault_path, key_path) = setup();
    for req in [
        request("GET", "/api/inbox", None, false, None),
        request(
            "GET",
            "/api/inbox",
            Some("https://evil.example"),
            true,
            None,
        ),
        Request::builder()
            .uri("/")
            .header("host", "evil.example:3031")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri("/api/inbox")
            .header("host", "127.0.0.1:3031")
            .header("x-nova-review", "owner-review")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap(),
        request("POST", "/api/review", None, true, Some(json!({}))),
        request(
            "POST",
            "/api/review",
            Some("https://evil.example"),
            true,
            Some(json!({})),
        ),
    ] {
        let response = router.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
    }
    let vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert_eq!(vault.pending_proposals().unwrap().len(), 1);
    assert!(vault.memories().unwrap().is_empty());
}
#[tokio::test]
async fn owner_review_edit_confirm_and_stale_retry_are_truthful() {
    let (_directory, router, vault_path, key_path) = setup();
    let response = router
        .clone()
        .oneshot(request("GET", "/api/inbox", None, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let inbox = body(response).await;
    let item = &inbox["items"][0];
    let payload = json!({"request_id":"owner_edit_1", "proposal_id":item["proposal"]["proposal_id"],
        "expected_snapshot":item["snapshot"], "action":{"kind":"edit","title":"Edited", "summary":"User-reviewed inference",
        "decisions":[],"reason":"Clarify inference"}});
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["status"], "committed_local");
    assert_eq!(result["receipt"]["mutation"], "edit_proposal");
    assert!(result["receipt"]["memory_id"].is_null());
    let retry = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::CONFLICT);
    let inbox = body(
        router
            .clone()
            .oneshot(request("GET", "/api/inbox", None, true, None))
            .await
            .unwrap(),
    )
    .await;
    let item = &inbox["items"][0];
    assert_eq!(item["source"]["content"], "synthetic source");
    assert_eq!(item["proposal"]["review_revision"], 1);
    let payload = json!({"request_id":"owner_confirm_1", "proposal_id":item["proposal"]["proposal_id"],
        "expected_snapshot":item["snapshot"], "action":{"kind":"confirm"}});
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["receipt"]["mutation"], "confirm");
    assert!(result["receipt"]["memory_id"].is_string());
    let retry = router
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::CONFLICT);
    let vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert_eq!(vault.memories().unwrap().len(), 1);
    assert_eq!(
        vault.memories().unwrap()[0].assertion_origin,
        AssertionOrigin::UserConfirmedInference
    );
    assert!(vault.verify_audit_chain().unwrap().valid);
}

#[tokio::test]
async fn rejected_candidate_cannot_be_confirmed_or_reintroduced() {
    let (_directory, router, vault_path, key_path) = setup();
    let inbox = body(
        router
            .clone()
            .oneshot(request("GET", "/api/inbox", None, true, None))
            .await
            .unwrap(),
    )
    .await;
    let item = &inbox["items"][0];
    let payload = json!({"request_id":"owner_reject_1", "proposal_id":item["proposal"]["proposal_id"],
        "expected_snapshot":item["snapshot"], "action":{"kind":"reject","reason":"Not the user's statement"}});
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["receipt"]["mutation"], "reject_proposal");
    assert!(result["receipt"]["memory_id"].is_null());
    let payload = json!({"request_id":"invalid_confirm", "proposal_id":item["proposal"]["proposal_id"],
        "expected_snapshot":item["snapshot"], "action":{"kind":"confirm"}});
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/review",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let inbox = body(
        router
            .oneshot(request("GET", "/api/inbox", None, true, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(inbox["items"].as_array().unwrap().is_empty());
    let vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert!(vault.memories().unwrap().is_empty());
    assert!(vault.verify_audit_chain().unwrap().valid);
}

#[tokio::test]
async fn asset_view_excludes_pending_other_owners_and_deleted_content_without_mutation() {
    let (_directory, router, vault_path, key_path) = setup();
    let mut vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    let seed = vault.pending_proposals().unwrap()[0].clone();
    let mut ids = Vec::new();
    for (index, owner) in ["owner:founder", "owner:founder", "owner:other"]
        .into_iter()
        .enumerate()
    {
        let captured = vault
            .capture(CaptureRequest {
                structure: None,
                request_id: Some(format!("asset_seed_{index}")),
                owner_id: owner.into(),
                source_platform: "codex".into(),
                capture_channel: CaptureChannel::RemoteMcp,
                capture_fidelity: CaptureFidelity::F1AiDigest,
                source_content: format!("RAW_SOURCE_MUST_NOT_LEAK_{index}"),
                title: format!("Asset {index}"),
                memory_type: MemoryType::Decision,
                summary: format!("Summary {index}"),
                decisions: vec![],
                privacy_level: PrivacyLevel::Private,
                proposed_origin: AssertionOrigin::AiExtractedProposal,
                review_tier: ReviewTier::Routine,
                permission_scope: vec!["personal_review".into()],
                purpose_limits: vec![],
                observed_at: None,
            })
            .unwrap();
        ids.push(
            vault
                .confirm_proposal(nova_core::ConfirmRequest {
                    request_id: None,
                    subject: owner.into(),
                    proposal_id: captured.proposal.proposal_id,
                })
                .unwrap()
                .memory
                .memory_id,
        );
    }
    vault
        .dispute_memory(nova_core::DisputeRequest {
            request_id: None,
            subject: "owner:founder".into(),
            memory_id: ids[0].clone(),
            reason: "Synthetic disputed".into(),
        })
        .unwrap();
    vault.delete_memory("owner:founder", &ids[1]).unwrap();
    let before = vault.verify_audit_chain().unwrap();
    drop(vault);
    for origin in [None, Some("https://evil.example")] {
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/assets",
                origin,
                origin.is_some(),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = router
        .oneshot(request("GET", "/api/assets", None, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let result = body(response).await;
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["memory_id"], ids[0]);
    assert_eq!(items[0]["status"], "disputed");
    assert_eq!(items[0]["source_platform"], "codex");
    let output = result.to_string();
    assert!(!output.contains("RAW_SOURCE_MUST_NOT_LEAK"));
    assert!(!output.contains("owner:other"));
    assert!(!output.contains(&seed.proposal_id));
    assert!(!output.contains("Summary 1"));
    assert!(!output.contains("Summary 2"));
    let vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    let after = vault.verify_audit_chain().unwrap();
    assert_eq!(before.head_hash, after.head_hash);
    assert_eq!(before.event_count, after.event_count);
}

#[tokio::test]
async fn owner_can_revoke_exact_approval_and_preserve_delivery_metadata() {
    let (_directory, router, vault_path, key_path) = setup();
    let mut vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    let proposal = vault
        .capture(CaptureRequest {
            structure: None,
            request_id: Some("access_seed".into()),
            owner_id: "owner:founder".into(),
            source_platform: "chatgpt".into(),
            capture_channel: CaptureChannel::RemoteMcp,
            capture_fidelity: CaptureFidelity::F1AiDigest,
            source_content: "SECRET RAW SOURCE".into(),
            title: "SECRET TITLE".into(),
            memory_type: MemoryType::Decision,
            summary: "SECRET SUMMARY".into(),
            decisions: vec![],
            privacy_level: PrivacyLevel::Private,
            proposed_origin: AssertionOrigin::AiExtractedProposal,
            review_tier: ReviewTier::Routine,
            permission_scope: vec!["personal_review".into()],
            purpose_limits: vec!["personal_memory_continuity".into()],
            observed_at: None,
        })
        .unwrap()
        .proposal;
    let memory = vault
        .confirm_proposal(nova_core::ConfirmRequest {
            request_id: None,
            subject: "owner:founder".into(),
            proposal_id: proposal.proposal_id,
        })
        .unwrap()
        .memory;
    let spec = nova_core::QuerySpec {
        subject: "owner:founder".into(),
        client: "chatgpt".into(),
        space: "personal".into(),
        request_id: "access_query".into(),
        purpose: "personal_memory_continuity".into(),
        selection: nova_core::QuerySelection::Get {
            memory_id: memory.memory_id.clone(),
            capture_status: false,
        },
    };
    let grant = vault
        .issue_disclosure_grant(
            spec.clone(),
            vec![memory.memory_id.clone()],
            PrivacyLevel::Private,
            600,
        )
        .unwrap();
    let delivery = vault
        .query_memory(spec.clone(), &grant.approval_id)
        .unwrap();
    let before = vault.verify_audit_chain().unwrap();
    assert!(vault.owner_access_overview("owner:other").is_err());
    drop(vault);
    let overview = body(
        router
            .clone()
            .oneshot(request("GET", "/api/access", None, true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(overview["approvals"][0]["status"], "active");
    assert_eq!(
        overview["deliveries"][0]["receipt_id"],
        delivery.receipt.envelope.receipt_id
    );
    assert!(!overview.to_string().contains("SECRET"));
    let vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert_eq!(
        before.head_hash,
        vault.verify_audit_chain().unwrap().head_hash
    );
    drop(vault);
    let payload = json!({"approval_id":grant.approval_id,"acknowledge":true});
    for (origin, ack, status) in [
        (Some("https://evil.example"), true, StatusCode::FORBIDDEN),
        (None, true, StatusCode::FORBIDDEN),
        (
            Some("http://127.0.0.1:3031"),
            false,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut data = payload.clone();
        data["acknowledge"] = json!(ack);
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/access/revoke",
                origin,
                true,
                Some(data),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/access/revoke",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload.clone()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert!(vault.query_memory(spec, &grant.approval_id).is_err());
    let revoked = vault.verify_audit_chain().unwrap();
    drop(vault);
    let retry = router
        .clone()
        .oneshot(request(
            "POST",
            "/api/access/revoke",
            Some("http://127.0.0.1:3031"),
            true,
            Some(payload),
        ))
        .await
        .unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    let overview = body(
        router
            .oneshot(request("GET", "/api/access", None, true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(overview["approvals"][0]["status"], "revoked");
    assert_eq!(overview["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(overview["audit"]["head_hash"], json!(revoked.head_hash));
    let mut vault = Vault::open(&vault_path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    vault
        .delete_memory("owner:founder", &memory.memory_id)
        .unwrap();
    let after_delete = vault.owner_access_overview("owner:founder").unwrap();
    assert_eq!(after_delete["deliveries"].as_array().unwrap().len(), 1);
    assert!(!after_delete.to_string().contains("SECRET"));
    let recovery = vault.owner_recovery_overview("owner:founder").unwrap();
    assert_eq!(recovery["deletions"].as_array().unwrap().len(), 1);
    assert_eq!(recovery["deletions"][0]["memory_id"], memory.memory_id);
    assert!(!recovery.to_string().contains("SECRET"));
    assert!(vault.owner_recovery_overview("owner:other").is_err());
}
