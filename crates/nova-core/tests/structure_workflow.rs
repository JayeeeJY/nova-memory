use nova_core::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
const OWNER: &str = "owner:synthetic";
fn draft() -> StructureTemplate {
    serde_json::from_value(json!({"template_id":"","owner_id":OWNER,"revision":0,"title":"Decision review","memory_type":"decision","privacy_level":"private","guidance":"Keep unknown facts unknown.","fields":[
        {"id":"constraint","label":"Constraint","kind":"text","required":true,"source_required":true,"max_length":100},
        {"id":"reason","label":"Reason","kind":"long_text"},
        {"id":"date","label":"Date","kind":"date"},
        {"id":"cost","label":"Cost","kind":"number","minimum":0,"maximum":100},
        {"id":"decision","label":"Decision","kind":"enum","options":["wait","proceed"]},
        {"id":"risks","label":"Risks","kind":"multiselect","options":["time","scope"]}
    ]})).unwrap()
}
fn values() -> BTreeMap<String, Value> {
    serde_json::from_value(json!({"constraint":"budget","reason":null,"date":"2026-10-11","cost":20,"decision":"wait","risks":["scope"]})).unwrap()
}
fn evidence() -> BTreeMap<String, String> {
    BTreeMap::from([("constraint".into(), "budget".into())])
}
fn request(t: &StructureTemplate) -> CaptureRequest {
    CaptureRequest {
        structure: Some(StructureInput {
            template_id: t.template_id.clone(),
            template_revision: t.revision,
            values: values(),
            evidence: evidence(),
        }),
        request_id: Some("structured-save".into()),
        owner_id: OWNER.into(),
        source_platform: "synthetic".into(),
        capture_channel: CaptureChannel::WebAdapter,
        capture_fidelity: CaptureFidelity::F2VisibleTranscript,
        source_content: "Our budget is constrained. We wait.".into(),
        title: "Decision".into(),
        memory_type: MemoryType::Decision,
        summary: "We wait.".into(),
        decisions: vec![],
        privacy_level: PrivacyLevel::Private,
        proposed_origin: AssertionOrigin::UserSelfReport,
        review_tier: ReviewTier::Attention,
        permission_scope: vec![
            "personal_review".into(),
            "client:unapproved".into(),
            "space:personal".into(),
        ],
        purpose_limits: vec!["personal_memory_continuity".into()],
        observed_at: None,
    }
}
#[test]
fn sample_validation_is_typed_bounded_and_honest_about_unknowns_and_evidence() {
    let t = draft();
    let report = t.validate_sample("budget", &values(), &evidence()).unwrap();
    assert!(report.valid);
    assert_eq!(report.engine, "manual_validation");
    assert!(report.model.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.field_id == "reason" && d.code == "value_unknown")
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == "quotation_found_semantics_unverified")
    );
    for (id, value) in [
        ("date", json!("2026-02-30")),
        ("cost", json!(101)),
        ("decision", json!("made_up")),
        ("risks", json!(["scope", "scope"])),
        ("constraint", json!(42)),
        ("reason", json!(["not text"])),
    ] {
        let mut data = values();
        data.insert(id.into(), value);
        assert!(
            !t.validate_sample("budget", &data, &evidence())
                .unwrap()
                .valid,
            "{id}"
        );
    }
    let mut data = values();
    data.remove("constraint");
    assert!(
        !t.validate_sample("budget", &data, &BTreeMap::new())
            .unwrap()
            .valid
    );
    data.insert("constraint".into(), Value::Null);
    assert!(
        t.validate_sample("budget", &data, &BTreeMap::new())
            .unwrap()
            .valid
    );
    assert!(
        !t.validate_sample("different source", &values(), &evidence())
            .unwrap()
            .valid
    );
    assert!(
        !t.validate_sample("budget", &values(), &BTreeMap::new())
            .unwrap()
            .valid
    );
    data.insert("owner_id".into(), json!("attacker"));
    assert!(
        !t.validate_sample("budget", &data, &BTreeMap::new())
            .unwrap()
            .valid
    );
}
#[test]
fn invalid_definitions_and_unsupported_types_fail_closed() {
    for id in [
        "owner_id",
        "status",
        "memory_id",
        "../x",
        "Field",
        "",
        "source_artifact_id",
    ] {
        let mut t = draft();
        t.fields[0].id = id.into();
        assert!(t.validate_definition().is_err());
    }
    let mut t = draft();
    t.fields.push(t.fields[0].clone());
    assert!(t.validate_definition().is_err());
    let mut value = serde_json::to_value(draft()).unwrap();
    value["fields"][0]["kind"] = json!("object_reference");
    assert!(serde_json::from_value::<StructureTemplate>(value).is_err());
    let mut value = serde_json::to_value(draft()).unwrap();
    value["fields"][0]["script"] = json!("unsupported");
    assert!(serde_json::from_value::<StructureTemplate>(value).is_err());
}
#[test]
fn published_versions_capture_confirmation_and_recovery_preserve_exact_structure() {
    let d = tempfile::tempdir().unwrap();
    let mut v = Vault::create(&d.path().join("source"), VaultKey::generate().unwrap()).unwrap();
    let first = v.put_template(OWNER, draft(), None).unwrap();
    let saved = v.capture(request(&first)).unwrap();
    assert_eq!(saved.proposal.status, ProposalStatus::Pending);
    assert!(v.memories().unwrap().is_empty());
    let mut second = first.clone();
    second.title = "Decision review 2".into();
    second.fields[0].label = "Explicit constraint".into();
    let second = v.put_template(OWNER, second, Some(1)).unwrap();
    assert_eq!(second.revision, 2);
    assert!(v.put_template(OWNER, second.clone(), Some(1)).is_err());
    assert!(
        v.owner_template_versions("other-owner", &first.template_id)
            .is_err()
    );
    assert_eq!(
        v.owner_template_versions(OWNER, &first.template_id)
            .unwrap()
            .len(),
        2
    );
    let replay = v.capture(request(&first)).unwrap();
    assert_eq!(replay.receipt, saved.receipt);
    let mut changed = request(&first);
    changed
        .structure
        .as_mut()
        .unwrap()
        .values
        .insert("constraint".into(), json!("other"));
    assert!(v.capture(changed).is_err());
    let mut bad = request(&first);
    bad.request_id = Some("bad-evidence".into());
    bad.structure
        .as_mut()
        .unwrap()
        .evidence
        .insert("constraint".into(), "absent".into());
    assert!(v.capture(bad).is_err());
    assert_eq!(v.pending_proposals().unwrap().len(), 1);
    let confirmed = v
        .confirm_proposal(ConfirmRequest {
            request_id: None,
            subject: OWNER.into(),
            proposal_id: saved.proposal.proposal_id.clone(),
        })
        .unwrap()
        .memory;
    let stored = confirmed.structure.as_ref().unwrap();
    assert_eq!(stored.template, first);
    assert_eq!(stored.values, values());
    assert_eq!(stored.source_artifact_id, saved.source.source_id);
    let corrected = v
        .correct_memory(CorrectionRequest {
            request_id: None,
            subject: OWNER.into(),
            memory_id: confirmed.memory_id,
            title: None,
            summary: "Owner revised the summary".into(),
            reason: "Synthetic correction".into(),
        })
        .unwrap();
    assert_eq!(corrected.memory.structure, confirmed.structure);
    let key = VaultKey::generate().unwrap();
    let package = v.export_encrypted(OWNER, &key).unwrap();
    assert!(!package.windows(10).any(|w| w == b"constraint"));
    let mut restored =
        Vault::create(&d.path().join("restored"), VaultKey::generate().unwrap()).unwrap();
    restored.import_encrypted(&package, &key, OWNER).unwrap();
    assert_eq!(
        restored
            .owner_template_versions(OWNER, &first.template_id)
            .unwrap(),
        vec![first.clone(), second]
    );
    assert!(
        restored
            .memories()
            .unwrap()
            .iter()
            .all(|m| m.structure.as_ref().unwrap().template == first)
    );
    assert!(
        restored
            .memories()
            .unwrap()
            .iter()
            .all(|m| m.permission_scope.contains(&"client:unapproved".into()))
    );
    restored.verify_audit_chain().unwrap();
}
#[test]
fn stable_field_id_cannot_change_type_even_after_removal() {
    let d = tempfile::tempdir().unwrap();
    let mut v = Vault::create(&d.path().join("vault"), VaultKey::generate().unwrap()).unwrap();
    let first = v.put_template(OWNER, draft(), None).unwrap();
    let mut changed = first.clone();
    changed.fields[0].kind = StructureFieldKind::LongText;
    assert!(v.put_template(OWNER, changed, Some(1)).is_err());
    let mut removed = first.clone();
    removed.fields.remove(0);
    let second = v.put_template(OWNER, removed, Some(1)).unwrap();
    let mut changed = second.clone();
    let mut field = first.fields[0].clone();
    field.kind = StructureFieldKind::LongText;
    changed.fields.push(field);
    assert!(
        v.put_template(OWNER, changed, Some(second.revision))
            .is_err()
    );
}
#[test]
fn structure_review_requires_current_owner_snapshot_and_preserves_pending_state() {
    let d = tempfile::tempdir().unwrap();
    let mut v = Vault::create(&d.path().join("vault"), VaultKey::generate().unwrap()).unwrap();
    let t = v.put_template(OWNER, draft(), None).unwrap();
    let saved = v.capture(request(&t)).unwrap();
    let row = v.review_inbox(OWNER).unwrap().remove(0);
    let mut input = request(&t).structure.unwrap();
    input
        .values
        .insert("reason".into(), json!("Owner provided rationale"));
    let r = ReviewRequest {
        request_id: Some("edit-fields".into()),
        subject: OWNER.into(),
        proposal_id: saved.proposal.proposal_id.clone(),
        expected_snapshot: row.snapshot,
        action: ReviewAction::EditStructure {
            structure: input.clone(),
            reason: "Synthetic owner correction".into(),
        },
    };
    let mut other = r.clone();
    other.subject = "owner:other".into();
    assert!(v.review_proposal(other).is_err());
    v.review_proposal(r.clone()).unwrap();
    assert!(v.review_proposal(r).is_err());
    let updated = v.review_inbox(OWNER).unwrap().remove(0);
    assert_eq!(updated.proposal.status, ProposalStatus::Pending);
    assert_eq!(
        updated.proposal.structure.as_ref().unwrap().values,
        input.values
    );
    assert!(v.memories().unwrap().is_empty());
    v.review_proposal(ReviewRequest {
        request_id: None,
        subject: OWNER.into(),
        proposal_id: saved.proposal.proposal_id,
        expected_snapshot: updated.snapshot,
        action: ReviewAction::Confirm,
    })
    .unwrap();
    assert_eq!(
        v.memories().unwrap()[0].structure.as_ref().unwrap().values,
        input.values
    );
}
#[test]
fn template_limits_are_enforced_by_core_even_without_http_adapter() {
    let d = tempfile::tempdir().unwrap();
    let mut v = Vault::create(&d.path().join("vault"), VaultKey::generate().unwrap()).unwrap();
    for _ in 0..100 {
        v.put_template(OWNER, draft(), None).unwrap();
    }
    assert!(v.put_template(OWNER, draft(), None).is_err());
    assert_eq!(v.owner_templates(OWNER).unwrap().len(), 100);
}
