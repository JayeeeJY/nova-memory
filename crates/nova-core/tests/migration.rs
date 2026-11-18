use nova_core::*;
fn capture(v: &mut Vault, id: &str) -> CaptureOutcome {
    v.capture(CaptureRequest {
        structure: None,
        request_id: Some(id.into()),
        owner_id: "owner:founder".into(),
        source_platform: "codex".into(),
        capture_channel: CaptureChannel::RemoteMcp,
        capture_fidelity: CaptureFidelity::F1AiDigest,
        source_content: format!("synthetic source {id}"),
        title: id.into(),
        memory_type: MemoryType::Decision,
        summary: format!("synthetic summary {id}"),
        decisions: vec![],
        privacy_level: PrivacyLevel::Private,
        proposed_origin: AssertionOrigin::AiExtractedProposal,
        review_tier: ReviewTier::Attention,
        permission_scope: vec!["client:codex".into(), "space:personal".into()],
        purpose_limits: vec!["personal_memory_continuity".into()],
        observed_at: None,
    })
    .unwrap()
}
fn confirm(v: &mut Vault, p: CaptureOutcome) -> CanonicalMemory {
    v.confirm_proposal(ConfirmRequest {
        request_id: None,
        subject: "owner:founder".into(),
        proposal_id: p.proposal.proposal_id,
    })
    .unwrap()
    .memory
}
fn vault(dir: &std::path::Path, name: &str) -> Vault {
    Vault::create(&dir.join(name), VaultKey::generate().unwrap()).unwrap()
}
fn spec(id: &str, client: &str, request: &str) -> QuerySpec {
    QuerySpec {
        subject: "owner:founder".into(),
        client: client.into(),
        space: "personal".into(),
        request_id: request.into(),
        purpose: "personal_memory_continuity".into(),
        selection: QuerySelection::Get {
            memory_id: id.into(),
            capture_status: false,
        },
    }
}
#[test]
fn two_environment_recovery_preserves_lifecycle_blocks_access_and_replays_without_resurrection() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vault(dir.path(), "source");
    let key = VaultKey::generate().unwrap();
    let p = capture(&mut source, "active");
    let active = confirm(&mut source, p);
    let _pending = capture(&mut source, "pending");
    let p = capture(&mut source, "disputed");
    let disputed = confirm(&mut source, p);
    source
        .dispute_memory(DisputeRequest {
            request_id: None,
            subject: "owner:founder".into(),
            memory_id: disputed.memory_id,
            reason: "synthetic dispute".into(),
        })
        .unwrap();
    let p = capture(&mut source, "deleted");
    let deleted = confirm(&mut source, p);
    source
        .delete_memory("owner:founder", &deleted.memory_id)
        .unwrap();
    let original = source.memories().unwrap();
    let bytes = source.export_encrypted("owner:founder", &key).unwrap();
    assert!(
        !bytes
            .windows(b"synthetic summary".len())
            .any(|b| b == b"synthetic summary")
    );
    let mut target = vault(dir.path(), "target");
    let report = target
        .import_encrypted(&bytes, &key, "owner:founder")
        .unwrap();
    assert_eq!(report.tombstone_count, 1);
    assert_eq!(report.memory_count, 2);
    let restored = target.memories().unwrap();
    for m in &restored {
        let old = original
            .iter()
            .find(|o| o.memory_id == m.memory_id)
            .unwrap();
        assert_eq!(m.summary, old.summary);
        assert_eq!(m.status, old.status);
        assert_eq!(m.source_artifact_ids, old.source_artifact_ids);
        assert_eq!(m.assertion_origin, old.assertion_origin);
    }
    assert_eq!(target.pending_proposals().unwrap().len(), 1);
    let q = spec(&active.memory_id, "codex", "read");
    assert!(
        target
            .issue_disclosure_grant(
                q,
                vec![active.memory_id.clone()],
                PrivacyLevel::Private,
                600
            )
            .is_err()
    );
    let snapshot = target
        .memory_access_snapshot("owner:founder", &active.memory_id)
        .unwrap();
    target
        .authorize_memory_client("owner:founder", &active.memory_id, "codex", &snapshot)
        .unwrap();
    assert!(
        target
            .authorize_memory_client("owner:founder", &active.memory_id, "chatgpt", &snapshot)
            .is_err()
    );
    let q = spec(&active.memory_id, "codex", "read-approved");
    let grant = target
        .issue_disclosure_grant(
            q.clone(),
            vec![active.memory_id.clone()],
            PrivacyLevel::Private,
            600,
        )
        .unwrap();
    assert_eq!(
        target.query_memory(q, &grant.approval_id).unwrap().items[0]
            .summary
            .as_deref(),
        Some(active.summary.as_str())
    );
    target
        .delete_memory("owner:founder", &active.memory_id)
        .unwrap();
    assert!(
        target
            .import_encrypted(&bytes, &key, "owner:founder")
            .unwrap()
            .replayed
    );
    assert!(
        target
            .memories()
            .unwrap()
            .iter()
            .all(|m| m.memory_id != active.memory_id)
    );
    assert!(source.verify_audit_chain().unwrap().valid);
    assert!(target.verify_audit_chain().unwrap().valid);
    let roundtrip = target.export_encrypted("owner:founder", &key).unwrap();
    assert_eq!(
        MigrationPackage::inspect_encrypted(&roundtrip, &key, "owner:founder")
            .unwrap()
            .tombstone_count,
        2
    );
}
#[test]
fn wrong_keys_owner_tampering_and_used_target_never_partially_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vault(dir.path(), "source");
    let p = capture(&mut source, "one");
    confirm(&mut source, p);
    let key = VaultKey::generate().unwrap();
    let mut bytes = source.export_encrypted("owner:founder", &key).unwrap();
    let mut target = vault(dir.path(), "target");
    assert!(
        target
            .import_encrypted(&bytes, &VaultKey::generate().unwrap(), "owner:founder")
            .is_err()
    );
    assert!(
        target
            .import_encrypted(&bytes, &key, "owner:other")
            .is_err()
    );
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    assert!(
        target
            .import_encrypted(&bytes, &key, "owner:founder")
            .is_err()
    );
    assert!(target.memories().unwrap().is_empty());
    assert!(target.pending_proposals().unwrap().is_empty());
    let bytes = source.export_encrypted("owner:founder", &key).unwrap();
    capture(&mut target, "existing");
    assert!(
        target
            .import_encrypted(&bytes, &key, "owner:founder")
            .is_err()
    );
    assert_eq!(target.pending_proposals().unwrap().len(), 1);
}
#[test]
fn corrected_versions_survive_and_old_grants_do_not_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vault(dir.path(), "source");
    let p = capture(&mut source, "old");
    let old = confirm(&mut source, p);
    let corrected = source
        .correct_memory(CorrectionRequest {
            request_id: None,
            subject: "owner:founder".into(),
            memory_id: old.memory_id.clone(),
            title: None,
            summary: "synthetic corrected summary".into(),
            reason: "synthetic reason".into(),
        })
        .unwrap()
        .memory;
    let q = spec(&corrected.memory_id, "codex", "grant-before-migration");
    let grant = source
        .issue_disclosure_grant(
            q.clone(),
            vec![corrected.memory_id],
            PrivacyLevel::Private,
            600,
        )
        .unwrap();
    let key = VaultKey::generate().unwrap();
    let bytes = source.export_encrypted("owner:founder", &key).unwrap();
    let mut target = vault(dir.path(), "target");
    target
        .import_encrypted(&bytes, &key, "owner:founder")
        .unwrap();
    assert!(target.query_memory(q, &grant.approval_id).is_err());
    let memories = target.memories().unwrap();
    assert_eq!(memories.len(), 2);
    assert_eq!(
        memories
            .iter()
            .find(|m| m.memory_id == old.memory_id)
            .unwrap()
            .status,
        MemoryStatus::Superseded
    );
}

#[test]
fn insertion_failure_rolls_back_every_object_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vault(dir.path(), "source");
    let p = capture(&mut source, "transaction");
    confirm(&mut source, p);
    let key = VaultKey::generate().unwrap();
    let bytes = source.export_encrypted("owner:founder", &key).unwrap();
    let mut target = vault(dir.path(), "target");
    let conn = rusqlite::Connection::open(dir.path().join("target")).unwrap();
    conn.execute_batch("CREATE TRIGGER synthetic_failure BEFORE INSERT ON canonical_memories BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;").unwrap();
    assert!(
        target
            .import_encrypted(&bytes, &key, "owner:founder")
            .is_err()
    );
    assert!(target.memories().unwrap().is_empty());
    assert!(target.pending_proposals().unwrap().is_empty());
    assert_eq!(target.verify_audit_chain().unwrap().event_count, 0);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM source_artifacts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM data_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    conn.execute_batch("DROP TRIGGER synthetic_failure")
        .unwrap();
    target
        .import_encrypted(&bytes, &key, "owner:founder")
        .unwrap();
    assert_eq!(target.memories().unwrap().len(), 1);
}

#[test]
fn readonly_preflight_checks_empty_replayed_and_conflicting_targets_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = vault(dir.path(), "preflight_source");
    let p = capture(&mut source, "preflight_seed");
    confirm(&mut source, p);
    let package_key = VaultKey::generate().unwrap();
    let bytes = source
        .export_encrypted("owner:founder", &package_key)
        .unwrap();
    let target_path = dir.path().join("readonly.sqlite");
    let target_key = VaultKey::generate().unwrap();
    let mut target = Vault::create(&target_path, target_key.clone()).unwrap();
    let before = std::fs::read(&target_path).unwrap();
    let readonly = Vault::open_readonly(&target_path, target_key.clone()).unwrap();
    let report = readonly
        .preflight_import_encrypted(&bytes, &package_key, "owner:founder")
        .unwrap();
    assert!(!report.replayed);
    assert!(readonly.memories().unwrap().is_empty());
    assert_eq!(before, std::fs::read(&target_path).unwrap());
    assert!(
        readonly
            .preflight_import_encrypted(&bytes, &VaultKey::generate().unwrap(), "owner:founder")
            .is_err()
    );
    assert!(
        readonly
            .preflight_import_encrypted(&bytes, &target_key, "owner:founder")
            .is_err()
    );
    assert!(
        readonly
            .preflight_import_encrypted(&bytes, &package_key, "owner:other")
            .is_err()
    );
    drop(readonly);
    target
        .import_encrypted(&bytes, &package_key, "owner:founder")
        .unwrap();
    let head = target.verify_audit_chain().unwrap().head_hash;
    let readonly = Vault::open_readonly(&target_path, target_key).unwrap();
    let recovery = readonly.owner_recovery_overview("owner:founder").unwrap();
    assert_eq!(recovery["imports"].as_array().unwrap().len(), 1);
    assert!(!recovery.to_string().contains("synthetic summary"));
    assert!(
        readonly
            .preflight_import_encrypted(&bytes, &package_key, "owner:founder")
            .unwrap()
            .replayed
    );
    assert_eq!(head, readonly.verify_audit_chain().unwrap().head_hash);
    let p = capture(&mut source, "new_snapshot");
    confirm(&mut source, p);
    let different = source
        .export_encrypted("owner:founder", &package_key)
        .unwrap();
    assert!(
        readonly
            .preflight_import_encrypted(&different, &package_key, "owner:founder")
            .is_err()
    );
    assert_eq!(head, readonly.verify_audit_chain().unwrap().head_hash);
}
