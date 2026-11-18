//! NM-MCP-001: prove ID/lifecycle assumptions using disposable synthetic Vaults.
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ConfirmRequest,
    CorrectionRequest, MemoryStatus, MemoryType, PrivacyLevel, ProposalStatus, ReviewAction,
    ReviewRequest, ReviewTier, Vault, VaultKey,
};

fn capture() -> CaptureRequest {
    CaptureRequest {
        structure: None,
        request_id: Some("spike:synthetic:capture".into()),
        owner_id: "owner:synthetic".into(),
        source_platform: "chatgpt".into(),
        capture_channel: CaptureChannel::RemoteMcp,
        capture_fidelity: CaptureFidelity::F1AiDigest,
        source_content: "Synthetic selected digest".into(),
        title: "Synthetic handle".into(),
        memory_type: MemoryType::Decision,
        summary: "Synthetic initial decision".into(),
        decisions: vec![],
        privacy_level: PrivacyLevel::Private,
        proposed_origin: AssertionOrigin::ModelInferred,
        review_tier: ReviewTier::Attention,
        permission_scope: vec!["personal_review".into()],
        purpose_limits: vec!["personal_memory_continuity".into()],
        observed_at: None,
    }
}
fn reopen(path: &std::path::Path, key_path: &std::path::Path) -> Vault {
    Vault::open(path, VaultKey::read_file(key_path).unwrap()).unwrap()
}
fn setup() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    Vault,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    let key_path = dir.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    let vault = Vault::create(&path, key).unwrap();
    (dir, path, key_path, vault)
}
#[test]
fn proposal_handle_and_capture_receipt_survive_reopen_and_retry() {
    let (_dir, path, key_path, mut vault) = setup();
    let saved = vault.capture(capture()).unwrap();
    let handle = saved.proposal.proposal_id.clone();
    assert_eq!(saved.proposal.status, ProposalStatus::Pending);
    assert!(vault.memories().unwrap().is_empty());
    drop(vault);
    let mut vault = reopen(&path, &key_path);
    let item = vault.review_inbox("owner:synthetic").unwrap().remove(0);
    assert_eq!(item.proposal.proposal_id, handle);
    assert_eq!(
        item.capture_receipt.unwrap().envelope.receipt_id,
        saved.receipt.envelope.receipt_id
    );
    assert_eq!(
        vault.capture(capture()).unwrap().proposal.proposal_id,
        handle
    );
    assert_eq!(vault.pending_proposals().unwrap().len(), 1);
    assert!(vault.review_inbox("owner:other").unwrap().is_empty());
}
#[test]
fn original_handle_links_to_initial_version_not_automatically_to_corrected_version() {
    let (_dir, path, key_path, mut vault) = setup();
    let saved = vault.capture(capture()).unwrap();
    let handle = saved.proposal.proposal_id;
    let first = vault
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:synthetic".into(),
            proposal_id: handle.clone(),
        })
        .unwrap()
        .memory;
    assert_eq!(
        first.assertion_origin,
        AssertionOrigin::UserConfirmedInference
    );
    let corrected = vault
        .correct_memory(CorrectionRequest {
            request_id: Some("spike:correction".into()),
            subject: "owner:synthetic".into(),
            memory_id: first.memory_id.clone(),
            title: None,
            summary: "Synthetic corrected decision".into(),
            reason: "Synthetic correction test".into(),
        })
        .unwrap()
        .memory;
    assert_ne!(corrected.proposal_id, handle);
    drop(vault);
    let vault = reopen(&path, &key_path);
    let history = vault.memories().unwrap();
    let original = history.iter().find(|m| m.proposal_id == handle).unwrap();
    assert_eq!(original.status, MemoryStatus::Superseded);
    assert_eq!(original.superseded_by, vec![corrected.memory_id.clone()]);
    assert_eq!(corrected.supersedes, vec![first.memory_id]);
    assert_eq!(
        history
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count(),
        1
    );
}
#[test]
fn rejection_removes_pending_lookup_but_capture_replay_retains_rejected_status() {
    let (_dir, path, key_path, mut vault) = setup();
    let saved = vault.capture(capture()).unwrap();
    let item = vault.review_inbox("owner:synthetic").unwrap().remove(0);
    vault
        .review_proposal(ReviewRequest {
            request_id: None,
            subject: "owner:synthetic".into(),
            proposal_id: saved.proposal.proposal_id.clone(),
            expected_snapshot: item.snapshot,
            action: ReviewAction::Reject {
                reason: "Synthetic rejected proposal".into(),
            },
        })
        .unwrap();
    drop(vault);
    let mut vault = reopen(&path, &key_path);
    assert!(vault.review_inbox("owner:synthetic").unwrap().is_empty());
    let replay = vault.capture(capture()).unwrap();
    assert_eq!(replay.proposal.proposal_id, saved.proposal.proposal_id);
    assert_eq!(replay.proposal.status, ProposalStatus::Rejected);
    assert!(vault.memories().unwrap().is_empty());
}
#[test]
fn deletion_cannot_be_resolved_by_listing_live_memories_or_replaying_capture() {
    let (_dir, path, key_path, mut vault) = setup();
    let saved = vault.capture(capture()).unwrap();
    let confirmed = vault
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:synthetic".into(),
            proposal_id: saved.proposal.proposal_id,
        })
        .unwrap();
    vault
        .delete_memory("owner:synthetic", &confirmed.memory.memory_id)
        .unwrap();
    drop(vault);
    let mut vault = reopen(&path, &key_path);
    assert!(vault.memories().unwrap().is_empty());
    assert!(vault.pending_proposals().unwrap().is_empty());
    assert!(vault.capture(capture()).is_err());
    assert!(vault.verify_audit_chain().is_ok());
}
