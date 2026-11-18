use super::*;
const CEILING: u64 = 4 * 1024 * 1024;
fn pending(s: &mut IntakeStore, id: &str, seconds: u32) -> CompilationAttempt {
    receive(s, id, "synthetic storage source");
    let job = s.claim_next(&scope(), seconds).unwrap().unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &job.lease.unwrap().lease_id)
        .unwrap()
}
fn reserve(s: &mut IntakeStore, a: &CompilationAttempt, bytes: u64, ceiling: u64) -> Result<()> {
    s.reserve_compilation_storage(
        &scope(),
        &a.job_id,
        &a.lease_id,
        CompilationStorageBudget::new(bytes)?,
        ceiling,
    )
}
fn dispatch(s: &mut IntakeStore, a: &CompilationAttempt) {
    s.authorize_dispatch(&scope(), &a.job_id, &a.lease_id, 100)
        .unwrap();
}
fn assert_held(s: &IntakeStore, count: usize, bytes: u64) {
    let state = s.compilation_storage_status(&scope()).unwrap();
    assert_eq!(state.active_reservations, count);
    assert_eq!(state.reserved_bytes, bytes);
}
#[test]
fn exact_reservation_survives_restart_and_rejects_rebinding_or_post_dispatch_growth() {
    let (_d, path, key, mut s) = setup();
    let a = pending(&mut s, "binding", 60);
    reserve(&mut s, &a, 65536, CEILING).unwrap();
    reserve(&mut s, &a, 65536, CEILING).unwrap();
    assert!(reserve(&mut s, &a, 131072, CEILING).is_err());
    assert_held(&s, 1, 65536);
    let other = AssetScope {
        owner_id: "owner:other".into(),
        space: scope().space,
    };
    assert!(s.compilation_storage_status(&other).is_err());
    assert!(
        s.compilation_commit_capacity(&scope(), &a.job_id, "wrong-lease", CEILING)
            .is_err()
    );
    drop(s);
    let mut s = IntakeStore::open(&path, key, scope()).unwrap();
    assert_held(&s, 1, 65536);
    dispatch(&mut s, &a);
    assert!(reserve(&mut s, &a, 65536, CEILING).is_err());
    let wire = std::fs::read(path).unwrap();
    assert!(
        !wire
            .windows(a.lease_id.len())
            .any(|w| w == a.lease_id.as_bytes())
    );
}
#[test]
fn concurrent_connections_cannot_reserve_the_same_remaining_space() {
    let (_d, path, key, mut s) = setup();
    let a = pending(&mut s, "race-a", 60);
    let b = pending(&mut s, "race-b", 60);
    let ceiling = s
        .database_allocation(&scope())
        .unwrap()
        .allocated_bytes
        .div_ceil(65536)
        * 65536
        + 3 * 65536;
    let start = std::sync::Arc::new(std::sync::Barrier::new(3));
    let joins: Vec<_> = [a, b]
        .into_iter()
        .map(|a| {
            let (p, k, gate) = (path.clone(), key.clone(), start.clone());
            std::thread::spawn(move || {
                let mut s = IntakeStore::open(&p, k, scope()).unwrap();
                gate.wait();
                reserve(&mut s, &a, 131072, ceiling).is_ok()
            })
        })
        .collect();
    start.wait();
    assert_eq!(
        joins
            .into_iter()
            .filter_map(|j| j.join().unwrap().then_some(()))
            .count(),
        1
    );
    assert_held(&s, 1, 131072);
}
#[test]
fn release_failure_rolls_back_units_job_and_hold_then_replay_releases_once() {
    let (_d, path, key, mut s) = setup();
    let a = pending(&mut s, "commit", 60);
    reserve(&mut s, &a, 262144, CEILING).unwrap();
    dispatch(&mut s, &a);
    let out = output("walking", "synthetic storage source");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER release_fault BEFORE INSERT ON intake_objects WHEN NEW.kind='compilation_storage_release' BEGIN SELECT RAISE(ABORT,'synthetic release failure'); END;").unwrap();
    assert!(
        s.commit_compilation(&scope(), &a.job_id, &a.lease_id, &out)
            .is_err()
    );
    assert!(s.compiled_units(&scope(), None).unwrap().is_empty());
    assert_eq!(
        s.job(&scope(), &a.job_id).unwrap().job.state,
        JobState::Processing
    );
    assert_held(&s, 1, 262144);
    db.execute_batch("DROP TRIGGER release_fault;").unwrap();
    let c = commit(&mut s, &a, &out);
    assert_held(&s, 0, 0);
    drop(s);
    let mut s = IntakeStore::open(&path, key, scope()).unwrap();
    assert_eq!(
        commit(&mut s, &a, &out).change.change_id,
        c.change.change_id
    );
    assert_eq!(s.compiled_units(&scope(), None).unwrap().len(), 1);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM intake_objects WHERE kind='compilation_storage_release'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn oversized_derived_output_rolls_back_and_cannot_expand_its_reservation() {
    let (_d, _path, _key, mut s) = setup();
    let a = pending(&mut s, "too-large", 60);
    reserve(&mut s, &a, 65536, CEILING).unwrap();
    dispatch(&mut s, &a);
    let mut value: Value =
        serde_json::from_slice(&output("walking", "synthetic storage source")).unwrap();
    let unit = value["units"][0].clone();
    value["units"] = (0..16)
        .map(|i| {
            let mut u = unit.clone();
            u["content"]["subject"] = json!(format!("synthetic-{i}"));
            u["content"]["value"] = json!("x".repeat(3000));
            u
        })
        .collect();
    let error = s
        .commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap_err();
    assert!(
        matches!(error, NovaError::PolicyDenied(ref message) if message == COMPILATION_STORAGE_CAPACITY),
        "{error}"
    );
    assert!(s.compiled_units(&scope(), None).unwrap().is_empty());
    assert_held(&s, 1, 65536);
    assert!(reserve(&mut s, &a, 262144, CEILING).is_err());
    commit(&mut s, &a, &output("walking", "synthetic storage source"));
    assert_held(&s, 0, 0);
}
#[test]
fn failed_expired_and_withdrawn_attempts_release_only_after_fencing() {
    for event in ["failed", "expired", "withdrawn", "deleted"] {
        let (_d, _path, _key, mut s) = setup();
        let a = pending(&mut s, event, if event == "expired" { 1 } else { 60 });
        reserve(&mut s, &a, 65536, CEILING).unwrap();
        dispatch(&mut s, &a);
        match event {
            "failed" => {
                s.fail_attempt(
                    &scope(),
                    &a.job_id,
                    &a.lease_id,
                    AttemptFailure::InvalidOutput,
                )
                .unwrap();
            }
            "expired" => {
                std::thread::sleep(std::time::Duration::from_millis(1100));
                assert_held(&s, 1, 65536);
                assert_eq!(s.recover_expired(&scope()).unwrap(), 1);
            }
            "withdrawn" => {
                s.withdraw_source(&scope(), &a.context.sources[0].source.object_id)
                    .unwrap();
            }
            _ => {
                s.delete_source(&scope(), &a.context.sources[0].source.object_id)
                    .unwrap();
            }
        }
        assert_held(&s, 0, 0);
        assert!(
            s.commit_compilation(
                &scope(),
                &a.job_id,
                &a.lease_id,
                &output("walking", "synthetic storage source")
            )
            .is_err()
        );
        if event == "failed" || event == "expired" {
            s.retry(&scope(), &a.job_id).unwrap();
            let job = s.claim_next(&scope(), 60).unwrap().unwrap();
            let next = s
                .prepare_compilation(&scope(), &job.job.job_id, &job.lease.unwrap().lease_id)
                .unwrap();
            reserve(&mut s, &next, 65536, CEILING).unwrap();
            assert!(
                s.fail_attempt(
                    &scope(),
                    &a.job_id,
                    &a.lease_id,
                    AttemptFailure::InvalidOutput
                )
                .is_err()
            );
            assert_held(&s, 1, 65536);
        }
    }
}
