use nova_core::{compilation::*, intake::*, *};
use serde_json::json;
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:portable-test".into(),
        space: "personal".into(),
    }
}
fn setup() -> (tempfile::TempDir, IntakeStore, VaultKey, IntakeReceipt) {
    let d = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&d.path().join("live.sqlite"), key.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.enable_connections(&scope()).unwrap();
    let a = IntakeAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    };
    let r=s.receive(&a,&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"one","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC PORTABLE secret 原件","occurred_at":null})).unwrap()).unwrap();
    (d, s, key, r)
}
fn restored(path: &std::path::Path) -> IntakeStore {
    IntakeStore::open_read_only(
        &path.join("intake.sqlite"),
        VaultKey::read_file(&path.join("vault.key")).unwrap(),
        scope(),
    )
    .unwrap()
}
#[test]
fn hosted_scratch_is_private_selected_and_cleaned_on_success_or_verification_failure() {
    let (d, s, _key, _) = setup();
    let scratch=d.path().join("managed scratch");
    std::fs::create_dir(&scratch).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch,std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let scratch=scratch.canonicalize().unwrap();
    let key=VaultKey::generate().unwrap();
    let package=s.export_portable_in(&scope(),&key,&scratch).unwrap();
    assert_eq!(verify_intake_portable_envelope(&package,&key).unwrap(),scope());
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(),0);
    #[cfg(unix)] {
        use std::os::unix::fs::{PermissionsExt,symlink};
        let link=d.path().join("linked");symlink(&scratch,&link).unwrap();
        assert!(s.export_portable_in(&scope(),&key,&link).is_err());
        std::fs::set_permissions(&scratch,std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(s.export_portable_in(&scope(),&key,&scratch).is_err());
        std::fs::set_permissions(&scratch,std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let db=rusqlite::Connection::open(d.path().join("live.sqlite")).unwrap();
    db.execute("UPDATE intake_objects SET ciphertext=randomblob(length(ciphertext)) WHERE kind='source'",[]).unwrap();
    assert!(s.export_portable_in(&scope(),&key,&scratch).is_err());
    assert_eq!(std::fs::read_dir(&scratch).unwrap().count(),0);
}
#[test]
fn export_restores_without_kms_keeps_original_and_quarantine_but_not_connections() {
    let (d, mut s, key, r) = setup();
    s.create_connection(
        &scope(),
        ConnectionRequest {
            request_id: "connect".into(),
            label: "synthetic".into(),
            host: ConnectionHost::Codex,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: true,
        },
    )
    .unwrap();
    let export_key = VaultKey::generate().unwrap();
    let bytes = s.export_portable(&scope(), &export_key).unwrap();
    for secret in [
        key.to_hex(),
        export_key.to_hex(),
        "SYNTHETIC PORTABLE".into(),
        scope().owner_id,
    ] {
        assert!(!bytes.windows(secret.len()).any(|b| b == secret.as_bytes()));
    }
    let out = d.path().join("restored");
    let report = restore_intake_portable(&bytes, &export_key, &out).unwrap();
    assert!(
        report.verification.envelopes_authenticated && report.verification.control_state_matches
    );
    assert!(
        report
            .verification
            .authority_freshness
            .starts_with("unknown_")
    );
    assert!(!report.verification.may_automate);
    assert!(!report.verification.off_device_restore_verified);
    drop(s);
    drop(key); // No original process or KMS is needed for inspection.
    let store = restored(&out);
    assert_eq!(
        store.source(&scope(), &r.source.object_id).unwrap().content,
        "SYNTHETIC PORTABLE secret 原件"
    );
    assert!(store.connections(&scope()).unwrap().is_empty());
    drop(store);
    let mut rw = IntakeStore::open(
        &out.join("intake.sqlite"),
        VaultKey::read_file(&out.join("vault.key")).unwrap(),
        scope(),
    )
    .unwrap();
    assert!(rw.claim_next(&scope(), 60).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["intake.sqlite", "vault.key", "inspection.json"] {
            assert_eq!(
                std::fs::metadata(out.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
#[test]
fn wrong_key_tampering_truncation_and_public_version_never_create_destination() {
    let (d, s, _, _) = setup();
    let key = VaultKey::generate().unwrap();
    let bytes = s.export_portable(&scope(), &key).unwrap();
    let out = d.path().join("restored");
    assert!(restore_intake_portable(&bytes, &VaultKey::generate().unwrap(), &out).is_err());
    assert!(!out.exists());
    for pos in [0, 22, bytes.len() - 1] {
        let mut bad = bytes.clone();
        bad[pos] ^= 1;
        assert!(restore_intake_portable(&bad, &key, &out).is_err());
        assert!(!out.exists());
    }
    for end in [0, 22, 48, bytes.len() - 1] {
        assert!(restore_intake_portable(&bytes[..end], &key, &out).is_err());
        assert!(!out.exists());
    }
}
#[test]
fn refuses_existing_paths_and_preserves_withdrawal_when_restored_or_reexported() {
    let (d, mut s, _, r) = setup();
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    let key = VaultKey::generate().unwrap();
    let bytes = s.export_portable(&scope(), &key).unwrap();
    let collision = d.path().join("existing");
    std::fs::create_dir(&collision).unwrap();
    std::fs::write(collision.join("keep"), b"keep").unwrap();
    assert!(restore_intake_portable(&bytes, &key, &collision).is_err());
    assert_eq!(std::fs::read(collision.join("keep")).unwrap(), b"keep");
    let out = d.path().join("restored");
    restore_intake_portable(&bytes, &key, &out).unwrap();
    let s = restored(&out);
    assert!(s.source(&scope(), &r.source.object_id).is_err());
    let again = s.export_portable(&scope(), &key).unwrap();
    let out2 = d.path().join("again");
    let report = restore_intake_portable(&again, &key, &out2).unwrap();
    assert!(!report.verification.may_automate);
    assert!(
        restored(&out2)
            .source(&scope(), &r.source.object_id)
            .is_err()
    );
}
#[test]
fn foreign_scope_and_oversized_file_are_rejected_without_new_assets() {
    let (d, s, _, _) = setup();
    let key = VaultKey::generate().unwrap();
    let mut foreign = scope();
    foreign.owner_id = "owner:other".into();
    assert!(s.export_portable(&foreign, &key).is_err());
    let input = d.path().join("oversized");
    std::fs::File::create(&input)
        .unwrap()
        .set_len(MAX_INTAKE_PORTABLE_BYTES as u64 + 1)
        .unwrap();
    let out = d.path().join("restored");
    assert!(restore_intake_portable_file(&input, &key, &out).is_err());
    assert!(!out.exists());
}

#[test]
fn envelope_verification_authenticates_key_scope_and_bytes_without_restoring() {
    let (_d, s, _, _) = setup();
    let key = VaultKey::generate().unwrap();
    let mut package = s.export_portable(&scope(), &key).unwrap();
    assert_eq!(
        verify_intake_portable_envelope(&package, &key).unwrap(),
        scope()
    );
    assert!(verify_intake_portable_envelope(&package, &VaultKey::generate().unwrap()).is_err());
    package[30] ^= 1;
    assert!(verify_intake_portable_envelope(&package, &key).is_err());
    assert!(verify_intake_portable_envelope(&package[..30], &key).is_err());
}

#[test]
fn rekey_retains_snapshot_and_quarantine_and_rejects_the_previous_envelope_key() {
    let (d, s, _, r) = setup();
    let old = VaultKey::generate().unwrap();
    let new = VaultKey::generate().unwrap();
    let package = s.export_portable(&scope(), &old).unwrap();
    let (got, changed) = rekey_intake_portable(&package, &old, &new).unwrap();
    assert_eq!(got, scope());
    assert_ne!(package, changed);
    assert!(verify_intake_portable_envelope(&changed, &old).is_err());
    assert!(rekey_intake_portable(&package, &new, &old).is_err());
    let dest = d.path().join("rekeyed");
    let report = restore_intake_portable(&changed, &new, &dest).unwrap();
    assert!(!report.verification.may_automate);
    assert!(!report.verification.off_device_restore_verified);
    assert_eq!(
        restored(&dest)
            .source(&scope(), &r.source.object_id)
            .unwrap()
            .content,
        "SYNTHETIC PORTABLE secret 原件"
    );
}
