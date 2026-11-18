use nova_core::*;
struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    key: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("vault.sqlite");
        let key = d.path().join("vault.key");
        let k = VaultKey::generate().unwrap();
        k.write_new_file(&key).unwrap();
        drop(Vault::create(&path, k).unwrap());
        Self { _dir: d, path, key }
    }
    fn open(&self) -> Vault {
        Vault::open(&self.path, VaultKey::read_file(&self.key).unwrap()).unwrap()
    }
    fn memory(&self, title: &str) -> CanonicalMemory {
        let mut v = self.open();
        let o = v
            .capture(CaptureRequest {
                structure: None,
                request_id: None,
                owner_id: "owner:alice".into(),
                source_platform: "client-a".into(),
                capture_channel: CaptureChannel::RemoteMcp,
                capture_fidelity: CaptureFidelity::F1AiDigest,
                source_content: title.into(),
                title: title.into(),
                memory_type: MemoryType::Decision,
                summary: format!("synthetic {title}"),
                decisions: vec![],
                privacy_level: PrivacyLevel::Private,
                proposed_origin: AssertionOrigin::AiExtractedProposal,
                review_tier: ReviewTier::Attention,
                permission_scope: vec![
                    "personal_review".into(),
                    "client:client-a".into(),
                    "space:work".into(),
                ],
                purpose_limits: vec!["personal_memory_continuity".into()],
                observed_at: None,
            })
            .unwrap();
        v.confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:alice".into(),
            proposal_id: o.proposal.proposal_id,
        })
        .unwrap()
        .memory
    }
    fn collection(&self, ids: Vec<String>) -> MemoryCollection {
        self.open()
            .put_collection("owner:alice", "work", None, None, "Synthetic", ids)
            .unwrap()
    }
    fn grant(&self, c: &MemoryCollection, quota: usize, future: bool) -> CollectionGrant {
        self.open()
            .issue_collection_grant(
                "owner:alice",
                CollectionGrantRequest {
                    collection_id: c.collection_id.clone(),
                    client: "client-b".into(),
                    purpose: "personal_memory_continuity".into(),
                    ttl_seconds: 600,
                    per_call_limit: 2,
                    call_quota: quota,
                    maximum_privacy: PrivacyLevel::Private,
                    include_future_members: future,
                },
            )
            .unwrap()
    }
}
fn spec(id: &str, selection: QuerySelection) -> QuerySpec {
    QuerySpec {
        subject: "owner:alice".into(),
        client: "client-b".into(),
        space: "work".into(),
        request_id: id.into(),
        purpose: "personal_memory_continuity".into(),
        selection,
    }
}
fn get(id: &str, mid: &str) -> QuerySpec {
    spec(
        id,
        QuerySelection::Get {
            memory_id: mid.into(),
            capture_status: false,
        },
    )
}
#[test]
fn cross_client_snapshot_future_members_removal_and_bounds() {
    let f = Fixture::new();
    let a = f.memory("first");
    let b = f.memory("second");
    let c = f.collection(vec![a.memory_id.clone()]);
    let g = f.grant(&c, 10, false);
    let future = f.grant(&c, 10, true);
    assert!(
        f.open()
            .query_memory(get("read-a", &a.memory_id), &g.approval_id)
            .is_ok()
    );
    let mut bad = get("bad-owner", &a.memory_id);
    bad.subject = "owner:bob".into();
    assert!(f.open().query_memory(bad, &g.approval_id).is_err());
    let mut bad = get("bad-space", &a.memory_id);
    bad.space = "personal".into();
    assert!(f.open().query_memory(bad, &g.approval_id).is_err());
    let mut bad = get("bad-client", &a.memory_id);
    bad.client = "client-a".into();
    assert!(f.open().query_memory(bad, &g.approval_id).is_err());
    let c = f
        .open()
        .put_collection(
            "owner:alice",
            "work",
            Some(&c.collection_id),
            Some(c.revision),
            "Synthetic",
            vec![a.memory_id.clone(), b.memory_id.clone()],
        )
        .unwrap();
    assert!(
        f.open()
            .query_memory(get("snapshot-new", &b.memory_id), &g.approval_id)
            .is_err()
    );
    assert!(
        f.open()
            .query_memory(get("future-new", &b.memory_id), &future.approval_id)
            .is_ok()
    );
    assert!(
        f.open()
            .query_memory(
                spec(
                    "too-many",
                    QuerySelection::Search {
                        query: "synthetic".into(),
                        max_items: 3
                    }
                ),
                &future.approval_id
            )
            .is_err()
    );
    f.open()
        .put_collection(
            "owner:alice",
            "work",
            Some(&c.collection_id),
            Some(c.revision),
            "Synthetic",
            vec![],
        )
        .unwrap();
    assert!(
        f.open()
            .query_memory(get("read-a", &a.memory_id), &g.approval_id)
            .is_err()
    );
}
#[test]
fn retries_conflicts_quota_and_revocation_are_atomic() {
    let f = Fixture::new();
    let m = f.memory("first");
    let c = f.collection(vec![m.memory_id.clone()]);
    let g = f.grant(&c, 1, false);
    let q = get("repeat", &m.memory_id);
    let first = f.open().query_memory(q.clone(), &g.approval_id).unwrap();
    let replay = f.open().query_memory(q.clone(), &g.approval_id).unwrap();
    assert_eq!(first.receipt, replay.receipt);
    assert!(
        f.open()
            .query_memory(get("next", &m.memory_id), &g.approval_id)
            .is_err()
    );
    assert!(matches!(
        f.open().query_memory(
            spec(
                "repeat",
                QuerySelection::Search {
                    query: "synthetic".into(),
                    max_items: 1
                }
            ),
            &g.approval_id
        ),
        Err(NovaError::IdempotencyConflict { .. })
    ));
    let o = f.open().owner_collections("owner:alice").unwrap();
    assert_eq!(o["grants"][0]["used_calls"], 1);
    f.open()
        .revoke_disclosure_grant("owner:alice", &g.approval_id)
        .unwrap();
    assert!(f.open().query_memory(q, &g.approval_id).is_err());
    assert!(f.open().verify_audit_chain().is_ok());
}
#[test]
fn parallel_calls_never_exceed_quota() {
    let f = Fixture::new();
    let m = f.memory("parallel");
    let c = f.collection(vec![m.memory_id.clone()]);
    let g = f.grant(&c, 3, false);
    let handles = (0..16)
        .map(|i| {
            let p = f.path.clone();
            let k = f.key.clone();
            let grant = g.approval_id.clone();
            let mid = m.memory_id.clone();
            std::thread::spawn(move || {
                Vault::open(&p, VaultKey::read_file(&k).unwrap())
                    .unwrap()
                    .query_memory(get(&format!("parallel-{i}"), &mid), &grant)
                    .is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        3
    );
    assert_eq!(
        f.open().owner_collections("owner:alice").unwrap()["grants"][0]["used_calls"],
        3
    );
    f.open().verify_audit_chain().unwrap();
}
#[test]
fn correction_dispute_delete_expiry_block_replay() {
    let f = Fixture::new();
    for action in ["correct", "dispute", "delete"] {
        let m = f.memory(action);
        let c = f.collection(vec![m.memory_id.clone()]);
        let g = f.grant(&c, 10, false);
        let q = get(action, &m.memory_id);
        f.open().query_memory(q.clone(), &g.approval_id).unwrap();
        match action {
            "correct" => {
                let n = f
                    .open()
                    .correct_memory(CorrectionRequest {
                        request_id: None,
                        subject: "owner:alice".into(),
                        memory_id: m.memory_id.clone(),
                        title: None,
                        summary: "new version".into(),
                        reason: "synthetic".into(),
                    })
                    .unwrap()
                    .memory;
                assert!(
                    f.open()
                        .query_memory(get("new-version", &n.memory_id), &g.approval_id)
                        .is_err()
                );
            }
            "dispute" => {
                f.open()
                    .dispute_memory(DisputeRequest {
                        request_id: None,
                        subject: "owner:alice".into(),
                        memory_id: m.memory_id.clone(),
                        reason: "synthetic".into(),
                    })
                    .unwrap();
            }
            _ => {
                f.open().delete_memory("owner:alice", &m.memory_id).unwrap();
            }
        }
        assert!(f.open().query_memory(q, &g.approval_id).is_err());
    }
    let m = f.memory("expires");
    let c = f.collection(vec![m.memory_id.clone()]);
    let g = f
        .open()
        .issue_collection_grant(
            "owner:alice",
            CollectionGrantRequest {
                collection_id: c.collection_id,
                client: "client-b".into(),
                purpose: "personal_memory_continuity".into(),
                ttl_seconds: 1,
                per_call_limit: 1,
                call_quota: 1,
                maximum_privacy: PrivacyLevel::Private,
                include_future_members: false,
            },
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(
        f.open()
            .query_memory(get("expired", &m.memory_id), &g.approval_id)
            .is_err()
    );
}
#[test]
fn encrypted_backup_restores_collections_without_authority() {
    let f = Fixture::new();
    let m = f.memory("backup");
    let c = f.collection(vec![m.memory_id.clone()]);
    let g = f.grant(&c, 2, false);
    let package_key = VaultKey::generate().unwrap();
    let bytes = f
        .open()
        .export_encrypted("owner:alice", &package_key)
        .unwrap();
    assert!(!bytes.windows(6).any(|b| b == b"backup"));
    let d = tempfile::tempdir().unwrap();
    let mut v = Vault::create(&d.path().join("new.sqlite"), VaultKey::generate().unwrap()).unwrap();
    v.import_encrypted(&bytes, &package_key, "owner:alice")
        .unwrap();
    let data = v.owner_collections("owner:alice").unwrap();
    assert_eq!(data["collections"][0]["collection_id"], c.collection_id);
    assert!(data["grants"].as_array().unwrap().is_empty());
    assert!(
        v.query_memory(get("restored", &m.memory_id), &g.approval_id)
            .is_err()
    );
    assert!(
        v.preflight_import_encrypted(&bytes, &package_key, "owner:bob")
            .is_err()
    );
    assert!(v.owner_recovery_overview("owner:alice").is_ok());
    v.verify_audit_chain().unwrap();
}

#[test]
fn collection_rejects_pending_foreign_scope_and_sensitive_grants() {
    let f = Fixture::new();
    let m = f.memory("bounds");
    assert!(
        f.open()
            .put_collection(
                "owner:bob",
                "work",
                None,
                None,
                "Other",
                vec![m.memory_id.clone()]
            )
            .is_err()
    );
    assert!(
        f.open()
            .put_collection(
                "owner:alice",
                "personal",
                None,
                None,
                "Other",
                vec![m.memory_id.clone()]
            )
            .is_err()
    );
    let c = f.collection(vec![m.memory_id.clone()]);
    assert!(
        f.open()
            .issue_collection_grant(
                "owner:alice",
                CollectionGrantRequest {
                    collection_id: c.collection_id.clone(),
                    client: "client-b".into(),
                    purpose: "personal_memory_continuity".into(),
                    ttl_seconds: 600,
                    per_call_limit: 1,
                    call_quota: 1,
                    maximum_privacy: PrivacyLevel::Shareable,
                    include_future_members: false
                }
            )
            .is_err()
    );
    assert!(
        f.open()
            .put_collection(
                "owner:alice",
                "work",
                Some(&c.collection_id),
                Some(c.revision + 1),
                "stale",
                vec![m.memory_id.clone()]
            )
            .is_err()
    );
    let mut v = f.open();
    let p = v
        .capture(CaptureRequest {
            structure: None,
            request_id: None,
            owner_id: "owner:alice".into(),
            source_platform: "client-a".into(),
            capture_channel: CaptureChannel::RemoteMcp,
            capture_fidelity: CaptureFidelity::F1AiDigest,
            source_content: "sensitive synthetic".into(),
            title: "sensitive".into(),
            memory_type: MemoryType::Decision,
            summary: "sensitive synthetic".into(),
            decisions: vec![],
            privacy_level: PrivacyLevel::Sensitive,
            proposed_origin: AssertionOrigin::AiExtractedProposal,
            review_tier: ReviewTier::Restricted,
            permission_scope: vec![
                "personal_review".into(),
                "client:client-a".into(),
                "space:work".into(),
            ],
            purpose_limits: vec!["personal_memory_continuity".into()],
            observed_at: None,
        })
        .unwrap();
    assert!(
        v.put_collection(
            "owner:alice",
            "work",
            None,
            None,
            "Pending",
            vec![p.proposal.proposal_id.clone()]
        )
        .is_err()
    );
    let m = v
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: "owner:alice".into(),
            proposal_id: p.proposal.proposal_id,
        })
        .unwrap()
        .memory;
    assert!(
        v.put_collection(
            "owner:alice",
            "work",
            None,
            None,
            "Sensitive",
            vec![m.memory_id]
        )
        .is_err()
    );
}
