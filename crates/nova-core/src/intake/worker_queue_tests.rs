use super::*;
use crate::VaultKey;
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn denied_admission_never_consumes_attempt_retry_or_lease_and_can_skip_to_another_job() {
    let (_d, _k, mut s) = fixture();
    let a = receive(&mut s, "admit-a");
    let b = receive(&mut s, "admit-b");
    let page = s
        .claim_next_batch_admitted(
            &scope(),
            60,
            None,
            128,
            |job| Ok(job.job.job_id == b.job_id),
        )
        .unwrap();
    assert_eq!(page.admission_deferred, 1);
    assert_eq!(page.claimed.unwrap().job.job_id, b.job_id);
    let old = s.job(&scope(), &a.job_id).unwrap();
    assert_eq!(old.job.attempt, 0);
    assert!(old.lease.is_none() && old.retry_requested);
    assert_eq!(old.job.state, JobState::Received);
    assert_eq!(
        s.claim_next_batch_admitted(&scope(), 60, None, 1, |_| Ok(true))
            .unwrap()
            .claimed
            .unwrap()
            .job
            .job_id,
        a.job_id
    );
}

#[test]
fn admission_error_rolls_back_prior_expired_recovery_in_same_page() {
    let (_d, _k, mut s) = fixture();
    let a = receive(&mut s, "old-admit");
    s.claim_next_batch(&scope(), 1, None, 128).unwrap();
    receive(&mut s, "new-admit");
    let later = OffsetDateTime::now_utc() + Duration::seconds(5);
    assert!(
        s.worker_queue_admitted_at(&scope(), Some(60), None, 128, later, |_| Err(invalid(
            "synthetic_admission_fault"
        )))
        .is_err()
    );
    assert_eq!(
        s.job(&scope(), &a.job_id).unwrap().job.state,
        JobState::Processing
    );
    let mut calls = 0;
    let page = s
        .worker_queue_admitted_at(&scope(), Some(60), None, 128, later, |_| {
            calls += 1;
            Ok(false)
        })
        .unwrap();
    assert_eq!((page.recovered, page.admission_deferred, calls), (1, 1, 1));
}

#[test]
fn admission_cannot_override_quarantine_source_withdrawal_or_page_bound() {
    let (_d, _k, mut s) = fixture();
    let a = receive(&mut s, "withdraw-admit");
    s.withdraw_source(&scope(), &a.source.object_id).unwrap();
    let mut calls = 0;
    let page = s
        .claim_next_batch_admitted(&scope(), 60, None, 1, |_| {
            calls += 1;
            Ok(true)
        })
        .unwrap();
    assert_eq!(calls, 0);
    assert!(page.claimed.is_none());
    assert_eq!(page.scanned, 1);
    assert!(
        s.claim_next_batch_admitted(&scope(), 60, None, 129, |_| panic!("must not call"))
            .is_err()
    );
}

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:queue-fixture".into(),
        space: "synthetic".into(),
    }
}
fn configured(s: &mut IntakeStore, revision: u64) {
    s.configure_model(
        revision,
        ModelSelection::Local {
            model_id: "synthetic".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )
    .unwrap();
}
fn fixture() -> (tempfile::TempDir, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let k = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&d.path().join("queue.sqlite"), k.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    configured(&mut s, 1);
    (d, k, s)
}
fn receive(s: &mut IntakeStore, id: &str) -> IntakeReceipt {
    s.receive(&IntakeAuthority{scope:scope(),actor_id:"fixture".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"QUEUE-PRIVATE-SENTINEL","occurred_at":null})).unwrap()).unwrap()
}
fn failed(s: &mut IntakeStore, id: &str) -> String {
    let r = receive(s, id);
    let j = s.claim_next(&scope(), 60).unwrap().unwrap();
    assert_eq!(j.job.job_id, r.job_id);
    s.fail_attempt(
        &scope(),
        &j.job.job_id,
        &j.lease.unwrap().lease_id,
        AttemptFailure::ModelUnavailable,
    )
    .unwrap();
    r.job_id
}

#[test]
fn one_row_pages_cross_history_and_reopen_without_implicit_retry() {
    let (d, k, mut s) = fixture();
    let old = failed(&mut s, "old");
    let second = failed(&mut s, "second");
    let live = receive(&mut s, "live");
    let page = s.claim_next_batch(&scope(), 60, None, 1).unwrap();
    assert_eq!(page.scanned, 1);
    assert!(page.claimed.is_none());
    assert!(!page.complete);
    drop(s);
    let mut s = IntakeStore::open(&d.path().join("queue.sqlite"), k, scope()).unwrap();
    let two = s
        .claim_next_batch(&scope(), 60, page.next_cursor.as_ref(), 1)
        .unwrap();
    assert_eq!(two.scanned, 1);
    assert!(two.claimed.is_none());
    let three = s
        .claim_next_batch(&scope(), 60, two.next_cursor.as_ref(), 1)
        .unwrap();
    assert_eq!(three.claimed.unwrap().job.job_id, live.job_id);
    assert_eq!(s.job(&scope(), &old).unwrap().job.attempt, 1);
    s.retry(&scope(), &second).unwrap();
    let end = s
        .claim_next_batch(&scope(), 60, three.next_cursor.as_ref(), 1)
        .unwrap();
    assert!(end.complete);
    assert!(end.claimed.is_none());
    let retry = s
        .claim_next_batch(&scope(), 60, None, 128)
        .unwrap()
        .claimed
        .unwrap();
    assert_eq!(retry.job.job_id, second);
    assert_eq!(retry.job.attempt, 2);
}

#[test]
fn unselected_ciphertext_is_not_decrypted_and_sql_uses_rowid_keyset() {
    let (_d, _k, mut s) = fixture();
    failed(&mut s, "one");
    failed(&mut s, "two");
    let third = failed(&mut s, "three");
    s.connection
        .execute(
            "UPDATE intake_objects SET ciphertext=X'00' WHERE id=?",
            [third],
        )
        .unwrap();
    let page = s.claim_next_batch(&scope(), 60, None, 1).unwrap();
    let next = s
        .claim_next_batch(&scope(), 60, page.next_cursor.as_ref(), 1)
        .unwrap();
    assert_eq!(next.scanned, 1);
    assert!(
        s.claim_next_batch(&scope(), 60, next.next_cursor.as_ref(), 1)
            .is_err()
    );
    let mut q = s
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {PAGE}"))
        .unwrap();
    let plan = q
        .query_map(rusqlite::params![0, 128], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH intake_requests USING INTEGER PRIMARY KEY"),
        "{plan}"
    );
    assert!(!plan.contains("TEMP") && !plan.contains("SCAN"), "{plan}");
}

#[test]
fn cursor_scope_and_bounds_refuse_without_claiming_and_debug_is_redacted() {
    let (_d, k, mut s) = fixture();
    let receipt = receive(&mut s, "pending");
    let page = s.claim_next_batch(&scope(), 60, None, 1).unwrap();
    let cursor = page.next_cursor.as_ref().unwrap();
    let d = tempfile::tempdir().unwrap();
    let mut other = IntakeStore::create(&d.path().join("same-key.sqlite"), k, scope()).unwrap();
    configured(&mut other, 1);
    let r = receive(&mut other, "other");
    assert!(
        other
            .claim_next_batch(&scope(), 60, Some(cursor), 1)
            .is_err()
    );
    for limit in [0, 129, usize::MAX] {
        assert!(other.claim_next_batch(&scope(), 60, None, limit).is_err());
    }
    for lease in [0, 901, u32::MAX] {
        assert!(other.claim_next_batch(&scope(), lease, None, 1).is_err());
    }
    assert!(
        other
            .claim_next_batch(
                &AssetScope {
                    owner_id: "owner:foreign".into(),
                    space: "synthetic".into()
                },
                60,
                None,
                1
            )
            .is_err()
    );
    assert_eq!(other.job(&scope(), &r.job_id).unwrap().job.attempt, 0);
    let debug = format!("{page:?} {cursor:?}");
    for private in [
        &receipt.job_id,
        &receipt.source.object_id,
        &scope().owner_id,
        "QUEUE-PRIVATE-SENTINEL",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn expired_lease_recovery_releases_derived_hold_without_auto_retry() {
    let (_d, _k, mut s) = fixture();
    let r = receive(&mut s, "expired");
    let job = s
        .claim_next_batch(&scope(), 60, None, 1)
        .unwrap()
        .claimed
        .unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &r.job_id, &lease.lease_id)
        .unwrap();
    s.reserve_compilation_storage(
        &scope(),
        &r.job_id,
        &lease.lease_id,
        CompilationStorageBudget::new(262144).unwrap(),
        4 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        s.compilation_storage_status(&scope())
            .unwrap()
            .active_reservations,
        1
    );
    let later = OffsetDateTime::now_utc() + Duration::seconds(120);
    let recovered = s.claim_next_batch_at(&scope(), 60, None, 1, later).unwrap();
    assert_eq!(recovered.recovered, 1);
    assert!(recovered.claimed.is_none());
    assert_eq!(
        s.compilation_storage_status(&scope())
            .unwrap()
            .reserved_bytes,
        0
    );
    assert!(
        s.authorize_dispatch(&scope(), &r.job_id, &lease.lease_id, 10)
            .is_err()
    );
    assert!(!s.job(&scope(), &r.job_id).unwrap().retry_requested);
    s.retry(&scope(), &r.job_id).unwrap();
    let next = s
        .claim_next_batch(&scope(), 60, None, 128)
        .unwrap()
        .claimed
        .unwrap();
    assert_eq!(next.job.attempt, 2);
    assert_ne!(next.lease.unwrap().lease_id, lease.lease_id);
}

#[test]
fn failure_on_later_expired_job_rolls_back_all_recoveries_in_the_page() {
    let (_d, _k, mut s) = fixture();
    let a = receive(&mut s, "a");
    let b = receive(&mut s, "b");
    s.claim_next_batch(&scope(), 60, None, 128).unwrap();
    s.claim_next_batch(&scope(), 60, None, 128).unwrap();
    assert!(
        b.job_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    );
    s.connection.execute_batch(&format!("CREATE TRIGGER queue_synthetic_failure BEFORE UPDATE ON intake_objects WHEN OLD.id='{}' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",b.job_id)).unwrap();
    let future = OffsetDateTime::now_utc() + Duration::seconds(120);
    assert!(
        s.claim_next_batch_at(&scope(), 60, None, 128, future)
            .is_err()
    );
    for job in [&a.job_id, &b.job_id] {
        assert_eq!(
            s.job(&scope(), job).unwrap().job.state,
            JobState::Processing
        );
    }
    s.connection
        .execute_batch("DROP TRIGGER queue_synthetic_failure")
        .unwrap();
    assert_eq!(
        s.claim_next_batch_at(&scope(), 60, None, 128, future)
            .unwrap()
            .recovered,
        2
    );
    assert_eq!(
        s.claim_next_batch_at(&scope(), 60, None, 128, future)
            .unwrap()
            .recovered,
        0
    );
}

#[test]
fn unconfigured_model_still_recovers_expired_lease_but_never_claims_received() {
    let (_d, _k, mut s) = fixture();
    let expired = receive(&mut s, "old");
    s.claim_next_batch(&scope(), 60, None, 128).unwrap();
    let received = receive(&mut s, "new");
    s.configure_model(2, ModelSelection::Unconfigured).unwrap();
    let batch = s
        .claim_next_batch_at(
            &scope(),
            60,
            None,
            128,
            OffsetDateTime::now_utc() + Duration::seconds(120),
        )
        .unwrap();
    assert!(!batch.model_configured);
    assert_eq!(batch.recovered, 1);
    assert!(batch.claimed.is_none());
    assert_eq!(
        s.job(&scope(), &expired.job_id).unwrap().job.state,
        JobState::Failed
    );
    assert_eq!(s.job(&scope(), &received.job_id).unwrap().job.attempt, 0);
}

#[test]
fn quarantined_backup_cannot_claim_or_recover_and_integrity_stays_valid() {
    let (d, k, mut s) = fixture();
    let r = receive(&mut s, "backup");
    let path = d.path().join("backup.sqlite");
    s.backup_to(&scope(), &path).unwrap();
    let mut copy = IntakeStore::open(&path, k, scope()).unwrap();
    let before = copy.verify_backup_copy(&scope()).unwrap().manifest;
    assert!(copy.claim_next_batch(&scope(), 60, None, 1).is_err());
    assert!(copy.recover_expired_batch(&scope(), None, 1).is_err());
    let after = copy.verify_backup_copy(&scope()).unwrap().manifest;
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(copy.job(&scope(), &r.job_id).unwrap().job.attempt, 0);
}

#[test]
fn independent_connections_claim_each_job_at_most_once() {
    let (d, k, mut s) = fixture();
    receive(&mut s, "one");
    receive(&mut s, "two");
    let barrier = std::sync::Barrier::new(2);
    let path = d.path().join("queue.sqlite");
    let ids = std::thread::scope(|scope_threads| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let key = k.clone();
            let barrier = &barrier;
            let path = &path;
            handles.push(scope_threads.spawn(move || {
                let mut local = IntakeStore::open(path, key, scope()).unwrap();
                barrier.wait();
                local
                    .claim_next_batch(&scope(), 60, None, 128)
                    .unwrap()
                    .claimed
                    .unwrap()
                    .job
                    .job_id
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<BTreeSet<_>>()
    });
    assert_eq!(ids.len(), 2);
    for id in ids {
        assert_eq!(s.job(&scope(), &id).unwrap().job.attempt, 1);
    }
}

#[test]
fn revoked_rows_advance_and_erased_or_new_rows_do_not_claim_withdrawn_sources() {
    let (_d, _k, mut s) = fixture();
    receive(&mut s, "first");
    let revoked = receive(&mut s, "revoked");
    let erased = receive(&mut s, "erased");
    let first = s.claim_next_batch(&scope(), 60, None, 1).unwrap();
    s.withdraw_source(&scope(), &revoked.source.object_id)
        .unwrap();
    s.delete_source(&scope(), &erased.source.object_id).unwrap();
    let fresh = receive(&mut s, "fresh");
    let next = s
        .claim_next_batch(&scope(), 60, first.next_cursor.as_ref(), 128)
        .unwrap();
    assert_eq!(next.claimed.unwrap().job.job_id, fresh.job_id);
    assert!(next.scanned <= 2);
    assert!(s.source(&scope(), &revoked.source.object_id).is_err());
    assert!(s.source(&scope(), &erased.source.object_id).is_err());
}

#[test]
fn explicit_retries_preserve_maximum_attempts_and_each_lease_is_distinct() {
    let (_d, _k, mut s) = fixture();
    let r = receive(&mut s, "retry-limit");
    let mut leases = BTreeSet::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let job = s
            .claim_next_batch(&scope(), 60, None, 128)
            .unwrap()
            .claimed
            .unwrap();
        assert_eq!(job.job.attempt, attempt);
        let lease = job.lease.unwrap().lease_id;
        assert!(leases.insert(lease.clone()));
        s.fail_attempt(
            &scope(),
            &r.job_id,
            &lease,
            AttemptFailure::ModelUnavailable,
        )
        .unwrap();
        assert!(
            s.claim_next_batch(&scope(), 60, None, 128)
                .unwrap()
                .claimed
                .is_none()
        );
        if attempt < MAX_ATTEMPTS {
            s.retry(&scope(), &r.job_id).unwrap();
        }
    }
    assert!(s.retry(&scope(), &r.job_id).is_err());
    assert_eq!(
        s.job(&scope(), &r.job_id).unwrap().job.attempt,
        MAX_ATTEMPTS
    );
}

#[test]
fn recovery_only_pages_never_claim_even_with_model_and_pending_retry() {
    let (_d, _k, mut s) = fixture();
    let expired = receive(&mut s, "expired-only");
    s.claim_next_batch(&scope(), 60, None, 1).unwrap();
    let failed = failed(&mut s, "failed-only");
    s.retry(&scope(), &failed).unwrap();
    let received = receive(&mut s, "received-only");
    let page = s
        .worker_queue_step_at(
            &scope(),
            None,
            None,
            128,
            OffsetDateTime::now_utc() + Duration::seconds(120),
        )
        .unwrap();
    assert_eq!(page.recovered, 1);
    assert!(page.claimed.is_none());
    assert_eq!(page.scanned, 3);
    assert_eq!(
        s.job(&scope(), &expired.job_id).unwrap().job.state,
        JobState::Failed
    );
    assert_eq!(s.job(&scope(), &received.job_id).unwrap().job.attempt, 0);
    assert_eq!(s.job(&scope(), &failed).unwrap().job.attempt, 1);
    assert!(s.job(&scope(), &failed).unwrap().retry_requested);
    let again = s.recover_expired_batch(&scope(), None, 128).unwrap();
    assert_eq!(again.recovered, 0);
    assert!(again.claimed.is_none());
}
