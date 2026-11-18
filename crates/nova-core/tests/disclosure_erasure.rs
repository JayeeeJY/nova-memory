use nova_core::*;
#[test]
fn pending_disclosure_key_is_destroyed_when_its_later_canonical_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sqlite");
    let key = VaultKey::generate().unwrap();
    let mut vault = Vault::create(&path, key).unwrap();
    let captured = vault
        .capture(CaptureRequest {
            structure: None,
            request_id: Some("save".into()),
            owner_id: "owner:founder".into(),
            source_platform: "codex".into(),
            capture_channel: CaptureChannel::RemoteMcp,
            capture_fidelity: CaptureFidelity::F1AiDigest,
            source_content: r#"{"occurred_at":null,"session_digest":"synthetic private source"}"#
                .into(),
            title: "unique private synthetic title".into(),
            memory_type: MemoryType::Decision,
            summary: "unique private synthetic summary".into(),
            decisions: vec![],
            privacy_level: PrivacyLevel::Private,
            proposed_origin: AssertionOrigin::AiExtractedProposal,
            review_tier: ReviewTier::Attention,
            permission_scope: vec!["client:codex".into(), "space:personal".into()],
            purpose_limits: vec!["personal_memory_continuity".into()],
            observed_at: None,
        })
        .unwrap();
    let spec = QuerySpec {
        subject: "owner:founder".into(),
        client: "codex".into(),
        space: "personal".into(),
        request_id: "query".into(),
        purpose: "personal_memory_continuity".into(),
        selection: QuerySelection::Get {
            memory_id: captured.proposal.proposal_id.clone(),
            capture_status: true,
        },
    };
    let grant = vault
        .issue_disclosure_grant(
            spec.clone(),
            vec![captured.proposal.proposal_id.clone()],
            PrivacyLevel::Private,
            600,
        )
        .unwrap();
    let result = vault
        .query_memory(spec.clone(), &grant.approval_id)
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !bytes
            .windows(b"unique private synthetic".len())
            .any(|s| s == b"unique private synthetic")
    );
    let memory = vault
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:founder".into(),
            proposal_id: captured.proposal.proposal_id,
        })
        .unwrap()
        .memory;
    vault
        .delete_memory("owner:founder", &memory.memory_id)
        .unwrap();
    assert!(vault.query_memory(spec, &grant.approval_id).is_err());
    assert!(vault.verify_audit_chain().unwrap().valid);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM data_keys WHERE aggregate_id=?1",
            [result.receipt.context_pack_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
