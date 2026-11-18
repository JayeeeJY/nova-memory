use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ConfirmRequest,
    ContextRequest, ContextSurface, MemoryType, NovaError, PrivacyLevel, ReviewTier, Vault,
    VaultKey,
};
use nova_mcp::service::{
    ApplicationService, LocalClientBinding, LocalVaultPort, VaultPort, VaultSession,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct RefusingPort {
    opens: AtomicUsize,
}
impl VaultPort for RefusingPort {
    fn open(&self) -> nova_core::Result<Box<dyn VaultSession>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Err(NovaError::InvalidState("unexpected store access".into()))
    }
}
fn capture() -> CaptureRequest {
    CaptureRequest {
        structure: None,
        request_id: Some("legacy-original-id".into()),
        owner_id: "owner:founder".into(),
        source_platform: "chatgpt".into(),
        capture_channel: CaptureChannel::RemoteMcp,
        capture_fidelity: CaptureFidelity::F1AiDigest,
        source_content: "Original serialized source bytes".into(),
        title: "Synthetic legacy record".into(),
        memory_type: MemoryType::Decision,
        summary: "Synthetic summary".into(),
        decisions: vec![],
        privacy_level: PrivacyLevel::Private,
        proposed_origin: AssertionOrigin::AiExtractedProposal,
        review_tier: ReviewTier::Attention,
        permission_scope: vec!["personal_review".into()],
        purpose_limits: vec!["personal_memory_continuity".into()],
        observed_at: Some("2026-10-01T00:00:00Z".into()),
    }
}
fn context() -> ContextRequest {
    ContextRequest {
        request_id: Some("legacy-context-id".into()),
        subject: "owner:founder".into(),
        recipient: "chatgpt".into(),
        purpose: "personal_memory_continuity".into(),
        query: "Synthetic".into(),
        surface: ContextSurface::ChatText,
        allowed_memory_types: vec![],
        maximum_privacy_level: PrivacyLevel::Private,
        max_items: 5,
        expires_in_seconds: 600,
        user_approved: true,
    }
}
fn isolated() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    ApplicationService,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.sqlite");
    let key_path = dir.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    drop(Vault::create(&path, key).unwrap());
    let service = ApplicationService::new(
        LocalClientBinding::legacy_chatgpt(),
        Arc::new(LocalVaultPort::new(&path, &key_path)),
    );
    (dir, path, key_path, service)
}
#[test]
fn capture_identity_intent_and_evidence_denials_precede_storage() {
    let port = Arc::new(RefusingPort::default());
    let service = ApplicationService::new(LocalClientBinding::legacy_chatgpt(), port.clone());
    assert!(matches!(
        service.capture_legacy(capture(), false),
        Err(NovaError::PolicyDenied(_))
    ));
    for variant in 0..5 {
        let mut request = capture();
        match variant {
            0 => request.owner_id = "owner:other".into(),
            1 => request.source_platform = "codex".into(),
            2 => request.capture_fidelity = CaptureFidelity::F2VisibleTranscript,
            3 => request.proposed_origin = AssertionOrigin::SourceEvidence,
            _ => request.capture_channel = CaptureChannel::OfficialExport,
        }
        assert!(matches!(
            service.capture_legacy(request, true),
            Err(NovaError::PolicyDenied(_))
        ));
    }
    assert_eq!(port.opens.load(Ordering::SeqCst), 0);
}
#[test]
fn disclosure_subject_recipient_approval_and_privacy_denials_precede_storage() {
    let port = Arc::new(RefusingPort::default());
    let service = ApplicationService::new(LocalClientBinding::legacy_chatgpt(), port.clone());
    for variant in 0..4 {
        let mut request = context();
        match variant {
            0 => request.user_approved = false,
            1 => request.subject = "owner:other".into(),
            2 => request.recipient = "codex".into(),
            _ => request.maximum_privacy_level = PrivacyLevel::Sensitive,
        }
        assert!(matches!(
            service.context_legacy(request),
            Err(NovaError::PolicyDenied(_))
        ));
    }
    assert_eq!(port.opens.load(Ordering::SeqCst), 0);
}
#[test]
fn codex_pending_binding_cannot_reuse_legacy_save_or_context() {
    let port = Arc::new(RefusingPort::default());
    let service = ApplicationService::new(LocalClientBinding::codex_pending(), port.clone());
    assert!(matches!(
        service.capture_legacy(capture(), true),
        Err(NovaError::PolicyDenied(_))
    ));
    assert!(matches!(
        service.context_legacy(context()),
        Err(NovaError::PolicyDenied(_))
    ));
    assert_eq!(port.opens.load(Ordering::SeqCst), 0);
}
#[test]
fn local_port_never_initializes_missing_store_or_exposes_paths_in_debug() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private-test.sqlite");
    let key_path = dir.path().join("private-key.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&key_path).unwrap();
    let port = LocalVaultPort::new(&path, &key_path);
    assert!(port.open().is_err());
    assert!(!path.exists());
    let debug = format!("{port:?}");
    assert!(!debug.contains("private-test"));
    assert!(!debug.contains("private-key"));
}
#[test]
fn service_replays_preexisting_core_capture_without_changing_fingerprint_or_source() {
    let (_dir, path, key_path, service) = isolated();
    let mut vault = Vault::open(&path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    let original = vault.capture(capture()).unwrap();
    let before = vault.verify_audit_chain().unwrap();
    drop(vault);
    let replay = service.capture_legacy(capture(), true).unwrap();
    assert_eq!(replay.source, original.source);
    assert_eq!(replay.proposal, original.proposal);
    assert_eq!(replay.receipt, original.receipt);
    let vault = Vault::open(&path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert_eq!(vault.verify_audit_chain().unwrap(), before);
    assert!(vault.memories().unwrap().is_empty());
}
#[test]
fn service_context_replays_existing_receipt_and_preserves_confirmed_only_disclosure() {
    let (_dir, path, key_path, service) = isolated();
    let pending = service.capture_legacy(capture(), true).unwrap();
    let mut vault = Vault::open(&path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    vault
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:founder".into(),
            proposal_id: pending.proposal.proposal_id,
        })
        .unwrap();
    let original = vault.prepare_context(context()).unwrap();
    let before = vault.verify_audit_chain().unwrap();
    drop(vault);
    let replay = service.context_legacy(context()).unwrap();
    assert_eq!(replay, original);
    assert_eq!(replay.pack.items.len(), 1);
    let vault = Vault::open(&path, VaultKey::read_file(&key_path).unwrap()).unwrap();
    assert_eq!(vault.verify_audit_chain().unwrap(), before);
}

#[test]
fn registered_capabilities_are_checked_before_store_access() {
    let port = Arc::new(RefusingPort::default());
    let binding = nova_mcp::registry::Principal {
        owner_id: "owner:alice".into(),
        space_id: "work".into(),
        client_id: "cursor".into(),
        capabilities: vec!["capture".into()],
    }
    .binding()
    .unwrap();
    let service = ApplicationService::new(binding.clone(), port.clone());
    let spec = nova_mcp::generic::query_spec_bound(
        &binding,
        "no-query",
        "personal_memory_continuity",
        nova_core::QuerySelection::Search {
            query: "synthetic".into(),
            max_items: 1,
        },
    );
    assert!(matches!(
        service.query_generic(spec, "missing"),
        Err(NovaError::PolicyDenied(_))
    ));
    assert_eq!(port.opens.load(Ordering::SeqCst), 0);
    let service = ApplicationService::new(
        LocalClientBinding::registered(
            "owner:founder".into(),
            "personal".into(),
            "codex".into(),
            vec!["query".into()],
        ),
        port.clone(),
    );
    let mut request = capture();
    request.source_platform = "codex".into();
    request.permission_scope = vec![
        "personal_review".into(),
        "client:codex".into(),
        "space:personal".into(),
    ];
    request.purpose_limits = vec!["personal_memory_continuity".into()];
    assert!(matches!(
        service.capture_generic(request, true),
        Err(NovaError::PolicyDenied(_))
    ));
    assert_eq!(port.opens.load(Ordering::SeqCst), 0);
}
