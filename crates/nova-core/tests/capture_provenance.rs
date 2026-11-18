use nova_core::{VaultKey, compilation::*, intake::*};
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:provenance-fixture".into(),
        space: "synthetic".into(),
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    store: IntakeStore,
    grant: IssuedCaptureGrant,
    access: CaptureAccess,
    session: CaptureSession,
    ack: CaptureAck,
    bytes: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = IntakeStore::create(
            &dir.path().join("store.sqlite"),
            VaultKey::generate().unwrap(),
            scope(),
        )
        .unwrap();
        store.enable_compilation(&scope()).unwrap();
        store.enable_capture(&scope()).unwrap();
        store.enable_connections(&scope()).unwrap();
        store.enable_continuity(&scope()).unwrap();
        let grant = store
            .create_capture_grant(
                &scope(),
                CaptureGrantRequest {
                    request_id: "provenance".into(),
                    source_namespace: "capture-fixture".into(),
                    host: "codex".into(),
                    surface: "cli_hooks".into(),
                    host_version: "unverified".into(),
                    events: vec![CaptureEventKind::UserPrompt],
                    lifetime_seconds: 3600,
                    event_budget: 100,
                    byte_budget: 1024 * 1024,
                },
            )
            .unwrap();
        let access = store.authenticate_capture(&scope(), &grant.token).unwrap();
        let session = store
            .register_capture_session(&scope(), &access, "origin")
            .unwrap();
        let bytes = serde_json::to_vec(&CaptureEnvelope {
            schema_version: CAPTURE_VERSION.into(),
            session_id: session.session_id.clone(),
            event_id: "event".into(),
            sequence: 1,
            kind: CaptureEventKind::UserPrompt,
            content: "SYNTHETIC private visible text".into(),
            occurred_at: None,
            loss: CaptureLoss {
                redacted: false,
                truncated: false,
                partial_history: true,
            },
        })
        .unwrap();
        let ack = store.capture_event(&scope(), &access, &bytes).unwrap();
        Self {
            _dir: dir,
            store,
            grant,
            access,
            session,
            ack,
            bytes,
        }
    }
    fn append(
        &mut self,
        origin: CaptureDeliveryOrigin,
    ) -> nova_core::Result<CaptureProvenanceRecord> {
        self.store.record_capture_provenance(
            &scope(),
            &self.access,
            CaptureProvenanceRequest {
                session_id: self.session.session_id.clone(),
                event_id: "event".into(),
                kind: CaptureEventKind::UserPrompt,
                origin,
            },
        )
    }
    fn reader(&mut self) -> (IssuedConnection, SessionRef) {
        let r = self
            .store
            .create_connection(
                &scope(),
                ConnectionRequest {
                    request_id: "reader".into(),
                    label: "metadata recipient".into(),
                    host: ConnectionHost::Claude,
                    preset: ConnectionPreset::ReadOnly,
                    topic_id: None,
                    include_future: true,
                },
            )
            .unwrap();
        let s = self
            .store
            .register_continuity_session(
                &scope(),
                SessionRegistration {
                    request_id: "recipient".into(),
                    connection_id: r.connection.id.clone(),
                    lifetime_seconds: 600,
                },
            )
            .unwrap();
        (r, s)
    }
}
#[test]
fn delivery_origins_append_without_changing_canonical_source_or_budget() {
    let mut f = Fixture::new();
    let source = f
        .store
        .source(&scope(), &f.ack.receipt.source.object_id)
        .unwrap();
    assert_eq!(
        f.append(CaptureDeliveryOrigin::HookUnverified)
            .unwrap()
            .origins
            .len(),
        1
    );
    let p = f.append(CaptureDeliveryOrigin::UserProvidedExport).unwrap();
    assert_eq!(p.origins.len(), 2);
    assert_eq!(
        f.append(CaptureDeliveryOrigin::UserProvidedExport)
            .unwrap()
            .origins
            .len(),
        2
    );
    let replay = f
        .store
        .capture_event(&scope(), &f.access, &f.bytes)
        .unwrap();
    assert_eq!(replay.receipt, f.ack.receipt);
    assert_eq!(
        f.store
            .capture_grant(&scope(), &f.grant.grant.grant_id)
            .unwrap()
            .received_events,
        1
    );
    let unchanged = f
        .store
        .source(&scope(), &f.ack.receipt.source.object_id)
        .unwrap();
    assert_eq!(unchanged.content, source.content);
    assert_eq!(unchanged.role, source.role);
    assert_eq!(unchanged.capture_fidelity, source.capture_fidelity);
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("private visible text")
    );
}
#[test]
fn provenance_rejects_unreceived_cross_session_revocation_and_source_withdrawal() {
    let mut f = Fixture::new();
    let mut input = CaptureProvenanceRequest {
        session_id: f.session.session_id.clone(),
        event_id: "missing".into(),
        kind: CaptureEventKind::UserPrompt,
        origin: CaptureDeliveryOrigin::UserProvidedExport,
    };
    assert!(
        f.store
            .record_capture_provenance(&scope(), &f.access, input.clone())
            .is_err()
    );
    input.event_id = "event".into();
    input.session_id = f
        .store
        .register_capture_session(&scope(), &f.access, "other")
        .unwrap()
        .session_id;
    assert!(
        f.store
            .record_capture_provenance(&scope(), &f.access, input)
            .is_err()
    );
    f.append(CaptureDeliveryOrigin::UserProvidedExport).unwrap();
    f.store
        .withdraw_source(&scope(), &f.ack.receipt.source.object_id)
        .unwrap();
    assert!(f.append(CaptureDeliveryOrigin::UserProvidedExport).is_err());
    assert!(
        f.store
            .capture_provenance_for_owner(&scope(), &f.ack.event_key)
            .is_err()
    );
    let mut f = Fixture::new();
    f.store
        .revoke_capture(&scope(), &f.grant.grant.grant_id)
        .unwrap();
    assert!(f.append(CaptureDeliveryOrigin::HookUnverified).is_err());
}
#[test]
fn owner_association_is_metadata_only_and_preserves_actor_gate() {
    let mut f = Fixture::new();
    f.append(CaptureDeliveryOrigin::UserProvidedExport).unwrap();
    let (reader, recipient) = f.reader();
    let input = CaptureContinuityLinkRequest {
        event_key: f.ack.event_key.clone(),
        recipient_session: recipient.clone(),
    };
    let a = f
        .store
        .link_capture_source_for_owner(&scope(), input.clone())
        .unwrap();
    let replay = f
        .store
        .link_capture_source_for_owner(&scope(), input)
        .unwrap();
    assert_eq!(a.association_id, replay.association_id);
    assert_ne!(a.source_session.session_id, a.recipient_session.session_id);
    assert_eq!(a.origin, "owner_linked_unverified");
    assert!(!a.confers_read_authority && !a.recap_completed);
    assert_eq!(
        f.store
            .capture_continuity_associations_for_owner(&scope(), &recipient)
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.store
            .record_continuity_event(
                &scope(),
                SessionEventRegistration {
                    session: recipient.clone(),
                    sequence: 1,
                    source: f.ack.receipt.source.clone()
                }
            )
            .is_err()
    );
    assert!(
        f.store
            .authenticate_connection(&scope(), &f.grant.token)
            .is_err()
    );
    let policy = f
        .store
        .connections(&scope())
        .unwrap()
        .into_iter()
        .find(|c| c.id == reader.connection.id)
        .unwrap();
    assert_eq!(policy.preset, ConnectionPreset::ReadOnly);
    assert_eq!(policy.used_calls, 0);
    assert!(policy.member_snapshot.is_empty());
    f.store
        .close_continuity_session(&scope(), &recipient)
        .unwrap();
    assert!(
        f.store
            .capture_continuity_associations_for_owner(&scope(), &recipient)
            .is_err()
    );
}
#[test]
fn associations_hide_withdrawn_source_and_do_not_survive_recipient_scope_forgery() {
    let mut f = Fixture::new();
    f.append(CaptureDeliveryOrigin::UserProvidedExport).unwrap();
    let (_, recipient) = f.reader();
    let mut bad = recipient.clone();
    bad.connection_id = "other".into();
    assert!(
        f.store
            .link_capture_source_for_owner(
                &scope(),
                CaptureContinuityLinkRequest {
                    event_key: f.ack.event_key.clone(),
                    recipient_session: bad
                }
            )
            .is_err()
    );
    f.store
        .link_capture_source_for_owner(
            &scope(),
            CaptureContinuityLinkRequest {
                event_key: f.ack.event_key.clone(),
                recipient_session: recipient.clone(),
            },
        )
        .unwrap();
    f.store
        .withdraw_source(&scope(), &f.ack.receipt.source.object_id)
        .unwrap();
    assert!(
        f.store
            .capture_continuity_associations_for_owner(&scope(), &recipient)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn owner_inventory_is_bounded_metadata_only_and_withdrawal_advances_cursor() {
    let mut f = Fixture::new();
    f.append(CaptureDeliveryOrigin::HookUnverified).unwrap();
    for n in 2..=25 {
        let bytes = serde_json::to_vec(&CaptureEnvelope {
            schema_version: CAPTURE_VERSION.into(),
            session_id: f.session.session_id.clone(),
            event_id: format!("event-{n}"),
            sequence: n,
            kind: CaptureEventKind::UserPrompt,
            content: "SYNTHETIC MUST NOT APPEAR IN INVENTORY".into(),
            occurred_at: None,
            loss: CaptureLoss {
                redacted: false,
                truncated: false,
                partial_history: true,
            },
        })
        .unwrap();
        f.store.capture_event(&scope(), &f.access, &bytes).unwrap();
        f.store
            .record_capture_provenance(
                &scope(),
                &f.access,
                CaptureProvenanceRequest {
                    session_id: f.session.session_id.clone(),
                    event_id: format!("event-{n}"),
                    kind: CaptureEventKind::UserPrompt,
                    origin: CaptureDeliveryOrigin::UserProvidedExport,
                },
            )
            .unwrap();
    }
    let first = f
        .store
        .list_capture_provenance_for_owner(&scope(), None)
        .unwrap();
    assert_eq!(first.items.len(), 20);
    assert_eq!(first.scanned_records, 20);
    assert!(!first.full_history);
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("MUST NOT APPEAR")
    );
    let second = f
        .store
        .list_capture_provenance_for_owner(&scope(), first.next_before)
        .unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(second.next_before.is_none());
    assert!(
        second
            .items
            .iter()
            .all(|b| first.items.iter().all(|a| a.event_key != b.event_key))
    );
    for item in first.items.iter().take(2) {
        f.store
            .withdraw_source(&scope(), &item.source.object_id)
            .unwrap();
    }
    let filtered = f
        .store
        .list_capture_provenance_for_owner(&scope(), None)
        .unwrap();
    assert_eq!(filtered.items.len(), 18);
    assert_eq!(filtered.next_before, first.next_before);
    assert!(
        f.store
            .list_capture_provenance_for_owner(&scope(), Some(-1))
            .is_err()
    );
    let foreign = AssetScope {
        owner_id: "other".into(),
        ..scope()
    };
    assert!(
        f.store
            .list_capture_provenance_for_owner(&foreign, None)
            .is_err()
    );
}

#[test]
fn owner_can_remove_expired_metadata_relation_without_restoring_or_deleting_source() {
    let mut f = Fixture::new();
    f.append(CaptureDeliveryOrigin::UserProvidedExport).unwrap();
    let (reader, recipient) = f.reader();
    let input = CaptureContinuityLinkRequest {
        event_key: f.ack.event_key.clone(),
        recipient_session: recipient.clone(),
    };
    let link = f
        .store
        .link_capture_source_for_owner(&scope(), input.clone())
        .unwrap();
    let foreign = AssetScope {
        owner_id: "other".into(),
        ..scope()
    };
    assert!(
        f.store
            .remove_capture_continuity_association_for_owner(&foreign, &link.association_id)
            .is_err()
    );
    f.store
        .close_continuity_session(&scope(), &recipient)
        .unwrap();
    assert!(
        f.store
            .remove_capture_continuity_association_for_owner(&scope(), &link.association_id)
            .unwrap()
    );
    assert!(
        !f.store
            .remove_capture_continuity_association_for_owner(&scope(), &link.association_id)
            .unwrap()
    );
    assert!(
        f.store
            .link_capture_source_for_owner(&scope(), input)
            .is_err()
    );
    assert!(
        f.store
            .source(&scope(), &f.ack.receipt.source.object_id)
            .is_ok()
    );
    let connection = f
        .store
        .connections(&scope())
        .unwrap()
        .into_iter()
        .find(|c| c.id == reader.connection.id)
        .unwrap();
    assert_eq!(connection.used_calls, 0);
    assert_eq!(
        f.store
            .capture_grant(&scope(), &f.grant.grant.grant_id)
            .unwrap()
            .received_events,
        1
    );
}
