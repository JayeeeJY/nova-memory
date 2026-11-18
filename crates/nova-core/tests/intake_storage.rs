use nova_core::{compilation::*, intake::*, *};
use serde_json::{Value, json};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:test".into(),
        space: "private".into(),
    }
}
fn authority() -> IntakeAuthority {
    IntakeAuthority {
        scope: scope(),
        actor_id: "client-a".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level: PrivacyLevel::Private,
    }
}
fn input(id: &str) -> Value {
    json!({"schema_version":"nova.intake.v0.1","request_id":id,"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"Synthetic private original: N1 中文😀 marker 7f92","occurred_at":null})
}
fn receive(store: &mut IntakeStore, id: &str) -> IntakeReceipt {
    store
        .receive(&authority(), &serde_json::to_vec(&input(id)).unwrap())
        .unwrap()
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("intake.sqlite");
    let key = VaultKey::generate().unwrap();
    let store = IntakeStore::create(&path, key.clone(), scope()).unwrap();
    (d, path, key, store)
}
#[test]
fn durable_encrypted_original_receipt_and_idempotence_survive_reopen_and_backup() {
    let (d, path, key, mut store) = setup();
    let receipt = receive(&mut store, "original");
    let source = store.source(&scope(), &receipt.source.object_id).unwrap();
    assert_eq!(source.occurred_at, None);
    assert_eq!(source.content, input("original")["content"]);
    assert_eq!(source.privacy_level, PrivacyLevel::Private);
    assert_eq!(source.role, SourceRole::User);
    let backup = d.path().join("backup.sqlite");
    store.backup_to(&scope(), &backup).unwrap();
    assert!(store.backup_to(&scope(), &backup).is_err());
    drop(store);
    let mut reopened = IntakeStore::open(&path, key.clone(), scope()).unwrap();
    assert_eq!(receive(&mut reopened, "original"), receipt);
    assert_eq!(reopened.jobs(&scope(), None).unwrap().len(), 1);
    let mut restored = IntakeStore::open(&backup, key, scope()).unwrap();
    assert_eq!(receive(&mut restored, "original"), receipt);
    assert_eq!(
        restored
            .source(&scope(), &receipt.source.object_id)
            .unwrap(),
        source
    );
    for file in std::fs::read_dir(d.path()).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        assert!(
            !bytes
                .windows(b"private original".len())
                .any(|w| w == b"private original")
        );
        assert!(!bytes.windows(b"client-a".len()).any(|w| w == b"client-a"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn actor_scope_capture_permission_and_input_binding_are_enforced() {
    let (_d, _path, _key, mut store) = setup();
    let receipt = receive(&mut store, "same");
    let mut changed = input("same");
    changed["content"] = json!("different");
    assert!(matches!(
        store.receive(&authority(), &serde_json::to_vec(&changed).unwrap()),
        Err(NovaError::IdempotencyConflict { .. })
    ));
    let mut actor = authority();
    actor.actor_id = "client-b".into();
    assert!(store.actor_job(&actor, &receipt.job_id).is_err());
    let other = store
        .receive(&actor, &serde_json::to_vec(&input("same")).unwrap())
        .unwrap();
    assert_ne!(other.job_id, receipt.job_id);
    actor.capture_allowed = false;
    assert!(matches!(
        store.receive(&actor, &serde_json::to_vec(&input("same")).unwrap()),
        Err(NovaError::PolicyDenied(_))
    ));
    let mut cross = authority();
    cross.scope.space = "other".into();
    assert!(
        store
            .receive(&cross, &serde_json::to_vec(&input("same")).unwrap())
            .is_err()
    );
    assert!(store.job(&cross.scope, &receipt.job_id).is_err());
    assert!(
        store
            .source(&cross.scope, &receipt.source.object_id)
            .is_err()
    );
    cross = authority();
    cross.scope.owner_id = "owner:other".into();
    assert!(
        store
            .receive(&cross, &serde_json::to_vec(&input("same")).unwrap())
            .is_err()
    );
    let mut elevated = authority();
    elevated.privacy_level = PrivacyLevel::Shareable;
    assert!(matches!(
        store.receive(&elevated, &serde_json::to_vec(&input("same")).unwrap()),
        Err(NovaError::IdempotencyConflict { .. })
    ));
}
#[test]
fn strict_raw_parser_rejects_duplicates_authority_fields_and_large_content() {
    let (_d, _path, _key, mut store) = setup();
    let serialized = input("strict").to_string();
    let duplicate = serialized.replacen("{", "{\"content\":\"hidden\",", 1);
    assert!(store.receive(&authority(), duplicate.as_bytes()).is_err());
    for field in ["owner_id", "privacy_level", "title", "approval_id"] {
        let mut data = input("strict");
        data[field] = json!("injected");
        assert!(
            store
                .receive(&authority(), &serde_json::to_vec(&data).unwrap())
                .is_err()
        );
    }
    let mut oversized = input("strict");
    oversized["content"] = json!("x".repeat(65537));
    assert!(
        store
            .receive(&authority(), &serde_json::to_vec(&oversized).unwrap())
            .is_err()
    );
    assert!(store.jobs(&scope(), None).unwrap().is_empty());
}
#[test]
fn rollback_on_last_write_failure_leaves_no_source_job_or_receipt() {
    let (_d, path, _key, mut store) = setup();
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch("CREATE TRIGGER injected_failure BEFORE INSERT ON intake_requests BEGIN SELECT RAISE(ABORT,'synthetic injected failure'); END;").unwrap();
    assert!(
        store
            .receive(
                &authority(),
                &serde_json::to_vec(&input("rollback")).unwrap()
            )
            .is_err()
    );
    assert!(store.jobs(&scope(), None).unwrap().is_empty());
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM intake_objects WHERE kind IN ('job','source')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    c.execute_batch("DROP TRIGGER injected_failure").unwrap();
    receive(&mut store, "rollback");
    assert_eq!(store.jobs(&scope(), None).unwrap().len(), 1);
}
#[test]
fn simultaneous_connections_receive_one_committed_receipt() {
    let (_d, path, key, store) = setup();
    drop(store);
    let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            let key = key.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let mut s = IntakeStore::open(&path, key, scope()).unwrap();
                gate.wait();
                receive(&mut s, "racing")
            })
        })
        .collect();
    let receipts: Vec<_> = handles.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(receipts.iter().all(|r| r == &receipts[0]));
    assert_eq!(
        IntakeStore::open(&path, key, scope())
            .unwrap()
            .jobs(&scope(), None)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn jobs_wait_without_model_and_retry_is_bounded_with_fencing_and_persistent_journal() {
    let (_d, path, key, mut store) = setup();
    let receipt = receive(&mut store, "job");
    assert!(store.claim_next(&scope(), 60).unwrap().is_none());
    assert_eq!(
        store.job(&scope(), &receipt.job_id).unwrap().job.state,
        JobState::Received
    );
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-no-network".into(),
                endpoint: "http://127.0.0.1:11434/v1".into(),
            },
        )
        .unwrap();
    assert!(
        store
            .configure_model(1, ModelSelection::Unconfigured)
            .is_err()
    );
    for attempt in 1..=MAX_ATTEMPTS {
        let claimed = store.claim_next(&scope(), 60).unwrap().unwrap();
        assert_eq!(claimed.job.attempt, attempt);
        assert!(store.claim_next(&scope(), 60).unwrap().is_none());
        let lease = claimed.lease.unwrap();
        assert!(
            store
                .fail_attempt(
                    &scope(),
                    &receipt.job_id,
                    "fake-lease",
                    AttemptFailure::InvalidOutput
                )
                .is_err()
        );
        store
            .fail_attempt(
                &scope(),
                &receipt.job_id,
                &lease.lease_id,
                AttemptFailure::ModelUnavailable,
            )
            .unwrap();
        assert!(
            store
                .fail_attempt(
                    &scope(),
                    &receipt.job_id,
                    &lease.lease_id,
                    AttemptFailure::InvalidOutput
                )
                .is_err()
        );
        if attempt < MAX_ATTEMPTS {
            store.retry(&scope(), &receipt.job_id).unwrap();
            store.retry(&scope(), &receipt.job_id).unwrap();
        }
    }
    assert!(store.retry(&scope(), &receipt.job_id).is_err());
    drop(store);
    let mut store = IntakeStore::open(&path, key, scope()).unwrap();
    assert_eq!(receive(&mut store, "job"), receipt);
    let record = store.job(&scope(), &receipt.job_id).unwrap();
    assert_eq!(record.history.len(), 15);
    assert_eq!(record.job.state, JobState::Failed);
    assert_eq!(record.job.committed_change_id, None);
    assert!(store.claim_next(&scope(), 60).unwrap().is_none());
    assert_eq!(
        store
            .source(&scope(), &receipt.source.object_id)
            .unwrap()
            .content,
        input("job")["content"]
    );
}
#[test]
fn no_implicit_initialization_migration_wrong_key_or_scope() {
    let (d, path, _key, store) = setup();
    drop(store);
    let before = std::fs::read(&path).unwrap();
    assert!(IntakeStore::open(&path, VaultKey::generate().unwrap(), scope()).is_err());
    assert_eq!(before, std::fs::read(&path).unwrap());
    assert!(IntakeStore::create(&path, VaultKey::generate().unwrap(), scope()).is_err());
    let missing = d.path().join("missing.sqlite");
    assert!(IntakeStore::open(&missing, VaultKey::generate().unwrap(), scope()).is_err());
    assert!(!missing.exists());
    let legacy = d.path().join("legacy.sqlite");
    let k = VaultKey::generate().unwrap();
    drop(Vault::create(&legacy, k.clone()).unwrap());
    let before = std::fs::read(&legacy).unwrap();
    assert!(IntakeStore::open(&legacy, k.clone(), scope()).is_err());
    assert_eq!(before, std::fs::read(&legacy).unwrap());
    assert!(
        Vault::open(&legacy, k)
            .unwrap()
            .memories()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn model_configuration_has_no_credential_fields_or_implicit_cloud_permission() {
    let (_d, _path, _key, mut store) = setup();
    for selection in [
        ModelSelection::Remote {
            model_id: "test".into(),
            endpoint: "https://example.test/v1".into(),
            allow_selected_source: false,
        },
        ModelSelection::Local {
            model_id: "test".into(),
            endpoint: "http://example.test:8080/v1".into(),
        },
        ModelSelection::Remote {
            model_id: "test".into(),
            endpoint: "https://user:secret@example.test/v1".into(),
            allow_selected_source: true,
        },
    ] {
        assert!(store.configure_model(1, selection).is_err());
    }
    let good = ModelSelection::Remote {
        model_id: "test".into(),
        endpoint: "https://api.example.test/v1".into(),
        allow_selected_source: true,
    };
    assert_eq!(
        store.configure_model(1, good.clone()).unwrap().selection,
        good
    );
}
#[test]
fn stable_pagination_does_not_hide_older_jobs() {
    let (_d, _path, _key, mut store) = setup();
    for n in 0..51 {
        receive(&mut store, &format!("page-{n}"));
    }
    let first = store.jobs(&scope(), None).unwrap();
    assert_eq!(first.len(), 50);
    let next = store.jobs(&scope(), Some(first.last().unwrap().0)).unwrap();
    assert_eq!(next.len(), 1);
    assert!(
        first
            .iter()
            .all(|(_, r)| r.job.job_id != next[0].1.job.job_id)
    );
}
