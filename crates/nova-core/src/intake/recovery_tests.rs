use super::*;
use serde_json::json;
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:backup-fixture".into(),
        space: "synthetic".into(),
    }
}
fn setup() -> (tempfile::TempDir, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let k = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&d.path().join("live.sqlite"), k.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.enable_connections(&scope()).unwrap();
    s.enable_capture(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "fixture".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )
    .unwrap();
    (d, k, s)
}
fn receive(s: &mut IntakeStore) -> IntakeReceipt {
    s.receive(&IntakeAuthority{scope:scope(),actor_id:"fixture".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"one","source_platform":"fixture","capture_fidelity":"f2_visible_transcript","content":"BACKUP-PRIVATE-SENTINEL","occurred_at":null})).unwrap()).unwrap()
}
fn connection() -> ConnectionRequest {
    ConnectionRequest {
        request_id: "r".into(),
        label: "fixture".into(),
        host: ConnectionHost::Codex,
        preset: ConnectionPreset::ReadOnly,
        topic_id: None,
        include_future: true,
    }
}
fn grant() -> CaptureGrantRequest {
    CaptureGrantRequest {
        request_id: "r".into(),
        source_namespace: "fixture".into(),
        host: "codex".into(),
        surface: "cli_hooks".into(),
        host_version: "fixture".into(),
        events: vec![CaptureEventKind::UserPrompt],
        lifetime_seconds: 60,
        event_budget: 10,
        byte_budget: 1000,
    }
}
#[test]
fn backup_authenticates_envelopes_without_claiming_fresh_authority_or_off_device_restore() {
    let (d, k, mut s) = setup();
    let r = receive(&mut s);
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let generation = s.last_backup_generation(&scope()).unwrap().unwrap();
    assert!(!generation.external_copy_verified);
    assert_eq!(
        s.source(&scope(), &r.source.object_id).unwrap().content,
        "BACKUP-PRIVATE-SENTINEL"
    );
    assert!(s.backup_recovery_manifest(&scope()).unwrap().is_none());
    let mut copy = IntakeStore::open(&file, k, scope()).unwrap();
    let report = copy.verify_backup_copy(&scope()).unwrap();
    assert!(report.envelopes_authenticated);
    assert!(report.manifest.quarantined);
    assert!(!report.may_automate);
    assert!(!report.off_device_restore_verified);
    assert_eq!(report.manifest.envelope_counts["source"], 1);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("BACKUP-PRIVATE-SENTINEL")
    );
    assert!(copy.create_connection(&scope(), connection()).is_err());
    assert!(copy.create_capture_grant(&scope(), grant()).is_err());
    assert!(copy.claim_next(&scope(), 60).is_err());
    assert!(copy.source(&scope(), &r.source.object_id).is_ok());
    assert!(s.create_connection(&scope(), connection()).is_ok());
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    let report = copy.verify_backup_copy(&scope()).unwrap();
    assert!(report.authority_freshness.starts_with("unknown_"));
    assert!(!report.may_automate);
}
#[test]
fn copied_in_flight_attempt_cannot_dispatch_or_commit() {
    let (d, k, mut s) = setup();
    receive(&mut s);
    let job = s.claim_next(&scope(), 60).unwrap().unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)
        .unwrap();
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let mut copy = IntakeStore::open(&file, k, scope()).unwrap();
    assert!(
        copy.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)
            .is_err()
    );
    assert!(
        copy.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 1)
            .is_err()
    );
    let bytes =
        serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[]})).unwrap();
    assert!(
        copy.commit_compilation(&scope(), &job.job.job_id, &lease.lease_id, &bytes)
            .is_err()
    );
    assert!(
        s.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 1)
            .is_ok()
    );
}
#[test]
fn integrity_mismatch_wrong_scope_and_unmarked_files_do_not_pass() {
    let (d, k, mut s) = setup();
    let r = receive(&mut s);
    assert!(s.verify_backup_copy(&scope()).is_err());
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let copy = IntakeStore::open(&file, k, scope()).unwrap();
    assert!(
        copy.verify_backup_copy(&AssetScope {
            space: "other".into(),
            ..scope()
        })
        .is_err()
    );
    copy.connection
        .execute(
            "UPDATE intake_objects SET ciphertext=x'00' WHERE id=?1",
            [r.source.object_id],
        )
        .unwrap();
    assert!(copy.verify_backup_copy(&scope()).is_err());
    assert!(require_automation(&copy.connection, &copy.key).is_err());
    let repack = d.path().join("repacked.sqlite");
    assert!(copy.backup_to(&scope(), &repack).is_err());
    assert!(!repack.exists());
}
#[test]
fn verification_is_bounded_and_failed_snapshot_is_removed() {
    let (d, _k, mut s) = setup();
    let r = receive(&mut s);
    s.connection
        .execute(
            "UPDATE intake_objects SET ciphertext=zeroblob(?1) WHERE id=?2",
            rusqlite::params![MAX_BYTES + 1, r.source.object_id],
        )
        .unwrap();
    let file = d.path().join("backup.sqlite");
    assert!(s.backup_to(&scope(), &file).is_err());
    assert!(!file.exists());
}
#[test]
fn rebackup_does_not_refresh_the_original_ancestry_or_remove_quarantine() {
    let (d, k, mut s) = setup();
    receive(&mut s);
    let a = d.path().join("a.sqlite");
    s.backup_to(&scope(), &a).unwrap();
    let copy = IntakeStore::open(&a, k.clone(), scope()).unwrap();
    let mut m = copy.backup_recovery_manifest(&scope()).unwrap().unwrap();
    m.origin_backup_created_at = "2000-01-01T00:00:00Z".into();
    update(&copy.connection, &copy.key, MANIFEST_KIND, MANIFEST_ID, &m).unwrap();
    let b = d.path().join("b.sqlite");
    copy.backup_to(&scope(), &b).unwrap();
    let recopy = IntakeStore::open(&b, k, scope()).unwrap();
    let report = recopy.verify_backup_copy(&scope()).unwrap();
    assert_eq!(
        report.manifest.origin_backup_created_at,
        "2000-01-01T00:00:00Z"
    );
    assert_ne!(report.manifest.backup_id, m.backup_id);
    assert!(report.manifest.quarantined);
}
#[test]
fn manifest_kind_cannot_hide_extra_records_from_inventory() {
    let (d, k, mut s) = setup();
    receive(&mut s);
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let copy = IntakeStore::open(&file, k, scope()).unwrap();
    insert(
        &copy.connection,
        &copy.key,
        MANIFEST_KIND,
        "extra",
        &json!({"unexpected":true}),
    )
    .unwrap();
    assert!(copy.verify_backup_copy(&scope()).is_err());
}
#[test]
fn expired_lease_is_not_recovered_just_by_inspecting_a_copy() {
    let (d, k, mut s) = setup();
    receive(&mut s);
    let mut job = s.claim_next(&scope(), 60).unwrap().unwrap();
    job.lease.as_mut().unwrap().expires_at = "2000-01-01T00:00:00Z".into();
    update(&s.connection, &s.key, "job", &job.job.job_id, &job).unwrap();
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let mut copy = IntakeStore::open(&file, k, scope()).unwrap();
    let before = copy.connection.total_changes();
    assert_eq!(copy.recover_expired(&scope()).unwrap(), 0);
    assert_eq!(copy.consolidate_pending(&scope(), 4).unwrap(), 0);
    assert!(copy.verify_backup_copy(&scope()).is_ok());
    assert_eq!(copy.connection.total_changes(), before);
    assert_eq!(s.recover_expired(&scope()).unwrap(), 1);
}
#[test]
fn lifecycle_index_and_schema_changes_fail_integrity_without_unlocking_copy() {
    let (d, k, mut s) = setup();
    let r = receive(&mut s);
    s.withdraw_source(&scope(), &r.source.object_id).unwrap();
    let file = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let copy = IntakeStore::open(&file, k, scope()).unwrap();
    assert!(
        copy.verify_backup_copy(&scope())
            .unwrap()
            .control_state_matches
    );
    let n = copy
        .connection
        .execute("DELETE FROM intake_revocations", [])
        .unwrap();
    assert!(n > 0);
    assert!(copy.verify_backup_copy(&scope()).is_err());
    assert!(require_automation(&copy.connection, &copy.key).is_err());
    let file = d.path().join("backup2.sqlite");
    s.backup_to(&scope(), &file).unwrap();
    let copy = IntakeStore::open(&file, s.key.clone(), scope()).unwrap();
    copy.connection
        .execute_batch("CREATE TABLE intake_unexpected(id TEXT)")
        .unwrap();
    assert!(copy.verify_backup_copy(&scope()).is_err());
}
#[test]
fn readonly_format_inspection_cannot_mutate_configuration_or_receive_data() {
    let (d, k, mut s) = setup();
    let receipt = receive(&mut s);
    let mut reader =
        IntakeStore::open_read_only(&d.path().join("live.sqlite"), k, scope()).unwrap();
    let before = reader.configuration().unwrap();
    assert!(
        reader
            .configure_model(before.revision, ModelSelection::Unconfigured)
            .is_err()
    );
    assert_eq!(reader.connection.total_changes(), 0);
    assert_eq!(reader.configuration().unwrap(), before);
    assert!(reader.source(&scope(), &receipt.source.object_id).is_ok());
}
