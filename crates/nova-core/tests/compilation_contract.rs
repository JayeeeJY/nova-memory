//! N0 contract checks on explicitly annotated synthetic material, not model evaluation.
use nova_core::{CaptureFidelity, PrivacyLevel, compilation::*};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct Fixtures {
    scope: AssetScope,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    case_id: String,
    dataset_kind: String,
    expected_valid: bool,
    expected_reason: Option<String>,
    snapshot: CompilationSnapshot,
}
fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!("fixtures/compilation-v01.json")).unwrap()
}
fn fixture(name: &str) -> CompilationSnapshot {
    fixtures()
        .cases
        .into_iter()
        .find(|c| c.case_id == name)
        .unwrap()
        .snapshot
}
fn base() -> CompilationSnapshot {
    fixture("time_and_project_exception")
}
fn change(current: &CompilationSnapshot, operations: Vec<ChangeOperation>) -> ChangeSet {
    ChangeSet {
        schema_version: COMPILATION_VERSION.into(),
        scope: current.scope.clone(),
        change_id: "change_synthetic".into(),
        compiler: CompilerVersion {
            model_id: "synthetic:no-model-called".into(),
            extractor_version: "n0-fixture-v1".into(),
            rules_version: "n0-contract-v1".into(),
        },
        operations,
    }
}
fn revise(unit: &MemoryUnit) -> MemoryUnit {
    let mut next = unit.clone();
    next.header.object.revision += 1;
    next.header.recorded_at = "2026-10-17T05:00:00Z".into();
    next
}
fn assert_error<T: std::fmt::Debug>(result: nova_core::Result<T>, expected: &str) {
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains(expected),
        "{error}: expected {expected}"
    );
}

#[test]
fn annotated_cases_cover_time_conditions_corrections_unknowns_and_rejections() {
    let data = fixtures();
    assert_eq!(data.cases.len(), 12);
    for case in data.cases {
        assert_eq!(case.dataset_kind, "synthetic_annotated_contract");
        let result = case.snapshot.validate(&data.scope);
        assert_eq!(
            result.is_ok(),
            case.expected_valid,
            "{}: {result:?}",
            case.case_id
        );
        if let Some(reason) = case.expected_reason {
            assert_error(result, &reason);
        }
    }
}

#[test]
fn intake_requires_no_title_summary_type_template_or_confirmation() {
    let input = json!({"schema_version":INTAKE_VERSION,"request_id":"intake_1","source_platform":"web-console","capture_fidelity":"f2_visible_transcript","content":"合成材料🙂，不需要先填表。","occurred_at":null});
    let request = IntakeRequest::parse(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(
        request.capture_fidelity,
        CaptureFidelity::F2VisibleTranscript
    );
    for key in [
        "owner_id",
        "privacy_level",
        "confirmed",
        "approval_id",
        "title",
        "template_id",
    ] {
        let mut attack = input.clone();
        attack[key] = json!("PRIVATE_CANARY_NOT_FOR_ERRORS");
        let error = IntakeRequest::parse(&serde_json::to_vec(&attack).unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid_intake_shape"));
        assert!(!error.contains("PRIVATE_CANARY"));
    }
}

#[test]
fn intake_rejects_version_format_time_and_size_confusion() {
    let mut request = IntakeRequest {
        schema_version: INTAKE_VERSION.into(),
        request_id: "intake_1".into(),
        source_platform: "synthetic".into(),
        capture_fidelity: CaptureFidelity::F1AiDigest,
        content: "synthetic selected digest".into(),
        occurred_at: None,
    };
    request.validate().unwrap();
    request.schema_version = "nova.capture.v0.3".into();
    assert_error(request.validate(), "unsupported_intake_version");
    request.schema_version = INTAKE_VERSION.into();
    request.capture_fidelity = CaptureFidelity::F3RawAudio;
    assert_error(request.validate(), "unsupported_source_format");
    request.capture_fidelity = CaptureFidelity::F2VisibleTranscript;
    request.occurred_at = Some("2026-10-17".into());
    assert_error(request.validate(), "invalid_time");
    assert_error(
        IntakeRequest::parse(&vec![b' '; 128 * 1024 + 1]),
        "document_limit",
    );
}

#[test]
fn unicode_spans_use_code_points_and_preserve_the_source_exactly() {
    let mut snapshot = base();
    let source = &snapshot.sources[0];
    let quote = "🙂，A项目";
    let start = source.content.find(quote).unwrap();
    let start_char = source.content[..start].chars().count();
    snapshot.units[0].evidence = vec![EvidenceSpan {
        source: source.source.clone(),
        start_char,
        end_char: start_char + quote.chars().count(),
        quote: quote.into(),
    }];
    snapshot.validate(&snapshot.scope).unwrap();
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    assert_eq!(
        CompilationSnapshot::parse(&bytes, &snapshot.scope).unwrap(),
        snapshot
    );
    snapshot.units[0].evidence[0].start_char = start;
    assert_error(snapshot.validate(&snapshot.scope), "invalid_evidence");
}

#[test]
fn future_plans_unknowns_and_project_exceptions_do_not_collapse_to_a_tag() {
    let snapshot = base();
    assert_eq!(snapshot.units[1].content.conditions["work:project"], "A");
    assert_eq!(snapshot.units[2].content.modality, Modality::Planned);
    assert!(snapshot.units.iter().all(|u| u.occurred_at.is_none()));
    let unknown = fixture("unknown_and_negation");
    assert_eq!(unknown.units[0].valid_time, ValidTime::Unknown);
    assert_eq!(unknown.units[0].content.polarity, Polarity::Negative);
    assert!(unknown.units[0].content.value.is_null());
    let recovered =
        CompilationSnapshot::parse(&serde_json::to_vec(&unknown).unwrap(), &unknown.scope).unwrap();
    assert_eq!(recovered, unknown);
}

#[test]
fn unseen_semantics_roundtrip_but_bundled_definitions_do_not_activate_them() {
    let snapshot = fixture("unseen_domain_extension");
    let mut consumer = ConsumerCapabilities::default();
    let report = snapshot
        .compatibility_for(&snapshot.scope, ContractTarget::CompilationV01, &consumer)
        .unwrap();
    assert_eq!(
        report.disposition,
        CompatibilityDisposition::OpaqueRoundTripOnly
    );
    assert_eq!(report.unknown_modules.len(), 1);
    assert!(report.opaque_extension_keys.contains("coffee:cupping"));
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    assert_eq!(
        CompilationSnapshot::parse(&bytes, &snapshot.scope).unwrap(),
        snapshot
    );
    consumer.modules.insert(snapshot.modules[0].module.clone());
    consumer.extension_keys.insert("coffee:cupping".into());
    assert_eq!(
        snapshot
            .compatibility_for(&snapshot.scope, ContractTarget::CompilationV01, &consumer)
            .unwrap()
            .disposition,
        CompatibilityDisposition::Supported
    );
    let mut control = serde_json::to_value(&snapshot).unwrap();
    control["modules"][0]["execute"] = json!("not executable");
    assert_error(
        CompilationSnapshot::parse(&serde_json::to_vec(&control).unwrap(), &snapshot.scope),
        "invalid_snapshot_shape",
    );
}

#[test]
fn instructions_are_inert_and_model_control_operations_are_rejected() {
    let snapshot = fixture("instructions_remain_data");
    snapshot.validate(&snapshot.scope).unwrap();
    for operation in [
        "set_permissions",
        "delete_source",
        "delete_unit",
        "activate_module",
        "confirm",
        "write_source",
    ] {
        let attack = json!({"schema_version":COMPILATION_VERSION,"scope":snapshot.scope,"change_id":"change_attack","compiler":{"model_id":"synthetic","extractor_version":"1","rules_version":"1"},"operations":[{"operation":operation}]});
        assert_error(
            ChangeSet::parse(&serde_json::to_vec(&attack).unwrap()),
            "invalid_change_shape",
        );
    }
    let mut origin = serde_json::to_value(&snapshot).unwrap();
    origin["units"][0]["origin"] = json!("user_confirmed_inference");
    assert_error(
        CompilationSnapshot::parse(&serde_json::to_vec(&origin).unwrap(), &snapshot.scope),
        "invalid_snapshot_shape",
    );
}

#[test]
fn cross_scope_and_source_revision_borrowing_fail_closed() {
    let snapshot = base();
    let mut other = snapshot.scope.clone();
    other.owner_id = "owner:other".into();
    assert_error(snapshot.validate(&other), "scope_mismatch");
    let mut bad = snapshot.clone();
    bad.sources[0].scope.space = "work".into();
    assert_error(bad.validate(&bad.scope), "scope_mismatch");
    let mut bad = snapshot.clone();
    bad.units[0].evidence[0].source.revision = 2;
    assert_error(bad.validate(&bad.scope), "invalid_evidence");
    let mut bad = snapshot;
    bad.sources[0].recorded_at = "2026-10-18T03:00:00Z".into();
    assert_error(bad.validate(&bad.scope), "invalid_evidence");
}

#[test]
fn content_graph_cycles_are_allowed_but_derivation_cycles_are_not() {
    let mut snapshot = base();
    let mut reverse = snapshot.relations[0].clone();
    reverse.header.object.object_id = "relation_reverse".into();
    std::mem::swap(&mut reverse.from, &mut reverse.to);
    snapshot.relations.push(reverse);
    snapshot.validate(&snapshot.scope).unwrap();
    snapshot.units[0].derived_from = vec![snapshot.units[1].header.object.clone()];
    snapshot.units[1].derived_from = vec![snapshot.units[0].header.object.clone()];
    assert_error(snapshot.validate(&snapshot.scope), "cyclic_derivation");
}

#[test]
fn derived_units_and_links_cannot_launder_sensitive_inputs() {
    let mut snapshot = base();
    snapshot.units[0].header.privacy_level = PrivacyLevel::Shareable;
    assert_error(snapshot.validate(&snapshot.scope), "privacy_downgrade");
    let mut snapshot = base();
    snapshot.units[0].header.privacy_level = PrivacyLevel::Sensitive;
    snapshot.relations.clear();
    snapshot.units[1].derived_from = vec![snapshot.units[0].header.object.clone()];
    assert_error(snapshot.validate(&snapshot.scope), "privacy_downgrade");
    snapshot.units[1].header.privacy_level = PrivacyLevel::Sensitive;
    snapshot.validate(&snapshot.scope).unwrap();
    snapshot.relations = base().relations;
    assert_error(snapshot.validate(&snapshot.scope), "privacy_downgrade");
}

#[test]
fn compiler_read_set_is_trusted_context_not_just_model_citations() {
    let mut current = base();
    current.relations.clear();
    current.units[1].header.privacy_level = PrivacyLevel::Sensitive;
    current.validate(&current.scope).unwrap();
    let mut added = current.units[0].clone();
    added.header.object.object_id = "unit_new".into();
    let proposal = change(
        &current,
        vec![ChangeOperation::AddUnit {
            unit: added.clone(),
        }],
    );
    assert_error(
        proposal.validate_against(&current, &current.scope),
        "compiler_input_privacy_downgrade",
    );
    added.header.privacy_level = PrivacyLevel::Sensitive;
    let proposal = change(&current, vec![ChangeOperation::AddUnit { unit: added }]);
    let impact = proposal.validate_against(&current, &current.scope).unwrap();
    assert_eq!(impact.privacy_floor, PrivacyLevel::Sensitive);
    assert_eq!(
        impact.read_set.len(),
        current.sources.len() + current.units.len()
    );
    assert!(impact.created_ids.contains("unit_new"));
    assert_eq!(current.units.len(), 3);
}

#[test]
fn revision_preview_requires_cas_and_updates_all_stale_relations() {
    let current = base();
    let next = revise(&current.units[0]);
    let revision = ChangeOperation::ReviseUnit {
        base_revision: 1,
        intent: RevisionIntent::Qualify,
        unit: next.clone(),
    };
    let incomplete = change(&current, vec![revision.clone()]);
    assert_error(
        incomplete.validate_against(&current, &current.scope),
        "stale_relation_endpoint",
    );
    let mut relation = current.relations[0].clone();
    relation.header.object.revision = 2;
    relation.header.recorded_at = next.header.recorded_at.clone();
    relation.to = next.header.object.clone();
    let complete = change(
        &current,
        vec![
            revision,
            ChangeOperation::ReviseRelation {
                base_revision: 1,
                relation,
            },
        ],
    );
    let impact = complete.validate_against(&current, &current.scope).unwrap();
    assert!(impact.revised_ids.contains("unit_remote"));
    assert!(impact.affected_ids.contains("relation_exception"));
    assert_eq!(current.units[0].header.object.revision, 1);
    let mut stale = complete;
    if let ChangeOperation::ReviseUnit { base_revision, .. } = &mut stale.operations[0] {
        *base_revision = 2;
    }
    assert_error(
        stale.validate_against(&current, &current.scope),
        "stale_or_invalid_revision",
    );
}

#[test]
fn compiler_cannot_overwrite_protected_corrections_or_reclassify_origin() {
    let current = fixture("user_correction_protected");
    let proposal = change(
        &current,
        vec![ChangeOperation::ReviseUnit {
            base_revision: 1,
            intent: RevisionIntent::Correct,
            unit: revise(&current.units[0]),
        }],
    );
    assert_error(
        proposal.validate_against(&current, &current.scope),
        "protected_user_correction",
    );
    let mut current = base();
    current.relations.clear();
    let mut next = revise(&current.units[0]);
    next.origin = SemanticOrigin::Inferred;
    let proposal = change(
        &current,
        vec![ChangeOperation::ReviseUnit {
            base_revision: 1,
            intent: RevisionIntent::Correct,
            unit: next,
        }],
    );
    assert_error(
        proposal.validate_against(&current, &current.scope),
        "origin_reclassification",
    );
}

#[test]
fn deleted_ids_and_inactive_versions_cannot_be_resurrected_by_compiler() {
    let mut current = base();
    current.relations.clear();
    let mut added = current.units[0].clone();
    added.header.object.object_id = "unit_deleted".into();
    current.tombstones.push(Tombstone {
        object: RevisionRef {
            object_id: "unit_deleted".into(),
            revision: 3,
        },
        deleted_at: "2026-10-17T03:30:00Z".into(),
    });
    let proposal = change(&current, vec![ChangeOperation::AddUnit { unit: added }]);
    assert_error(
        proposal.validate_against(&current, &current.scope),
        "duplicate_or_deleted_object",
    );
    current.units[0].header.state = UnitState::Superseded;
    let mut next = revise(&current.units[0]);
    next.header.state = UnitState::Active;
    let proposal = change(
        &current,
        vec![ChangeOperation::ReviseUnit {
            base_revision: 1,
            intent: RevisionIntent::StateChange,
            unit: next,
        }],
    );
    assert_error(
        proposal.validate_against(&current, &current.scope),
        "lifecycle_resurrection",
    );
}

#[test]
fn source_change_marks_transitive_units_and_relations_for_invalidation() {
    let mut snapshot = base();
    snapshot.units[2].derived_from = vec![snapshot.units[0].header.object.clone()];
    snapshot.validate(&snapshot.scope).unwrap();
    let impact = snapshot.affected_by(&BTreeSet::from(["src_remote".into()]));
    assert_eq!(
        impact,
        BTreeSet::from([
            "src_remote".into(),
            "unit_remote".into(),
            "unit_exception".into(),
            "unit_plan".into(),
            "relation_exception".into()
        ])
    );
    let impact = snapshot.affected_by(&BTreeSet::from(["unit_remote".into()]));
    assert!(impact.contains("unit_plan") && impact.contains("relation_exception"));
    assert!(!impact.contains("unit_exception"));
}

#[test]
fn inactive_dependencies_are_not_silently_available_as_current_understanding() {
    let mut snapshot = base();
    snapshot.relations.clear();
    snapshot.units[0].header.state = UnitState::Disputed;
    snapshot.units[2].derived_from = vec![snapshot.units[0].header.object.clone()];
    assert_error(snapshot.validate(&snapshot.scope), "inactive_dependency");
    snapshot.units[2].header.state = UnitState::Disputed;
    snapshot.validate(&snapshot.scope).unwrap();
}

#[test]
fn job_states_support_honest_failures_retries_and_no_direct_ready_transition() {
    let snapshot = base();
    let received = CompilationJob {
        job_id: "job_synthetic".into(),
        scope: snapshot.scope.clone(),
        source: snapshot.sources[0].source.clone(),
        state: JobState::Received,
        attempt: 0,
        error_code: None,
        committed_change_id: None,
    };
    let mut processing = received.clone();
    processing.state = JobState::Processing;
    processing.attempt = 1;
    received.validate_successor(&processing, &snapshot).unwrap();
    let mut failed = processing.clone();
    failed.state = JobState::Failed;
    failed.error_code = Some("MODEL_UNAVAILABLE".into());
    processing.validate_successor(&failed, &snapshot).unwrap();
    assert_error(
        failed.validate_successor(&processing, &snapshot),
        "invalid_job_transition",
    );
    let mut retry = processing;
    retry.attempt = 2;
    failed.validate_successor(&retry, &snapshot).unwrap();
    let mut ready = retry.clone();
    ready.state = JobState::Ready;
    ready.committed_change_id = Some("change_committed".into());
    retry.validate_successor(&ready, &snapshot).unwrap();
    assert_error(
        received.validate_successor(&ready, &snapshot),
        "invalid_job_transition",
    );
    ready.committed_change_id = None;
    assert_error(ready.validate(&snapshot), "invalid_job_state");
    let mut bad: Value = serde_json::to_value(received).unwrap();
    bad["confirmed"] = json!(true);
    assert!(serde_json::from_value::<CompilationJob>(bad).is_err());
}

#[test]
fn durable_acceptance_receipt_binds_source_request_and_job_without_claiming_compilation() {
    use sha2::{Digest, Sha256};
    let snapshot = base();
    let source = &snapshot.sources[0];
    let request = IntakeRequest {
        schema_version: INTAKE_VERSION.into(),
        request_id: "receipt_test".into(),
        source_platform: source.source_platform.clone(),
        capture_fidelity: source.capture_fidelity,
        content: source.content.clone(),
        occurred_at: source.occurred_at.clone(),
    };
    let job = CompilationJob {
        job_id: "job_synthetic".into(),
        scope: snapshot.scope.clone(),
        source: source.source.clone(),
        state: JobState::Received,
        attempt: 0,
        error_code: None,
        committed_change_id: None,
    };
    let receipt = IntakeReceipt {
        schema_version: INTAKE_VERSION.into(),
        request_id: request.request_id.clone(),
        source: source.source.clone(),
        job_id: job.job_id.clone(),
        saved_at: source.recorded_at.clone(),
        content_sha256: hex::encode(Sha256::digest(source.content.as_bytes())),
        status: IntakeAcceptance::Received,
    };
    receipt.validate(&request, &job, &snapshot).unwrap();
    let mut bad = receipt.clone();
    bad.content_sha256 = "0".repeat(64);
    assert_error(
        bad.validate(&request, &job, &snapshot),
        "receipt_binding_mismatch",
    );
    let mut bad = receipt.clone();
    bad.request_id = "other_request".into();
    assert_error(
        bad.validate(&request, &job, &snapshot),
        "receipt_binding_mismatch",
    );
    let mut bad = receipt.clone();
    bad.job_id = "other_job".into();
    assert_error(
        bad.validate(&request, &job, &snapshot),
        "receipt_binding_mismatch",
    );
    let mut bad: Value = serde_json::to_value(&receipt).unwrap();
    bad["status"] = json!("ready");
    assert!(serde_json::from_value::<IntakeReceipt>(bad).is_err());
    let mut bad = request;
    bad.content.push_str(" changed");
    assert_error(
        receipt.validate(&bad, &job, &snapshot),
        "receipt_binding_mismatch",
    );
}

#[test]
fn no_novel_facts_can_be_a_valid_noop_without_inventing_memories() {
    let current = base();
    let proposal = change(&current, vec![]);
    let impact = proposal.validate_against(&current, &current.scope).unwrap();
    assert!(impact.created_ids.is_empty() && impact.revised_ids.is_empty());
    assert_eq!(current, base());
}

#[test]
fn resource_limits_reject_deep_payloads_and_unknown_envelope_fields() {
    let mut snapshot = base();
    let mut nested = Value::Null;
    for _ in 0..10 {
        nested = json!([nested]);
    }
    snapshot.units[0]
        .extensions
        .insert("future:payload".into(), nested);
    assert_error(snapshot.validate(&snapshot.scope), "semantic_payload_limit");
    let mut invalid = serde_json::to_value(base()).unwrap();
    invalid["permissions"] = json!(["all"]);
    assert_error(
        CompilationSnapshot::parse(&serde_json::to_vec(&invalid).unwrap(), &base().scope),
        "invalid_snapshot_shape",
    );
    assert_error(
        CompilationSnapshot::parse(&vec![b' '; MAX_DOCUMENT_BYTES + 1], &base().scope),
        "document_limit",
    );
}

#[test]
fn duplicate_semantic_keys_are_rejected_instead_of_silently_losing_values() {
    let snapshot = fixture("unseen_domain_extension");
    let text = serde_json::to_string(&snapshot).unwrap();
    let text = text.replace(
        "\"acidity\":\"high\"",
        "\"acidity\":\"high\",\"acidity\":\"low\"",
    );
    assert!(text.contains("\"acidity\":\"low\""));
    assert_error(
        CompilationSnapshot::parse(text.as_bytes(), &snapshot.scope),
        "invalid_snapshot_shape",
    );
    let duplicate = br#"{"schema_version":"nova.intake.v0.1","request_id":"first","request_id":"second","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"synthetic","occurred_at":null}"#;
    assert_error(IntakeRequest::parse(duplicate), "invalid_intake_shape");
}

#[test]
fn legacy_consumers_get_explicit_rejection_instead_of_lossy_or_confirmed_conversion() {
    let snapshot = base();
    for target in [
        ContractTarget::LegacyCanonicalV02,
        ContractTarget::LegacyPortableV04,
    ] {
        let report = snapshot
            .compatibility_for(&snapshot.scope, target, &ConsumerCapabilities::default())
            .unwrap();
        assert_eq!(report.disposition, CompatibilityDisposition::Rejected);
        assert!(
            report
                .reasons
                .contains("automatic_processing_is_not_user_confirmation")
        );
        assert!(
            report
                .reasons
                .contains("time_or_conditions_require_explicit_mapping")
        );
    }
    assert_eq!(nova_core::NMDS_VERSION, "0.2.0");
    let encoded = serde_json::to_value(snapshot).unwrap();
    assert!(serde_json::from_value::<nova_core::portable::PortableInventory>(encoded).is_err());
    let bad: Value = json!({"schema_version":INTAKE_VERSION,"request_id":"n0","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"Synthetic text","occurred_at":null});
    assert!(serde_json::from_value::<nova_core::CaptureRequest>(bad).is_err());
}

#[test]
fn quotation_integrity_does_not_claim_to_prove_semantic_entailment() {
    let mut snapshot = base();
    // Deliberately unsupported meaning: N2's semantic evaluation must catch this.
    // N0 proves the declared source coordinates and structure only.
    snapshot.units[0].content.value = json!("permanent_personality_trait");
    snapshot.validate(&snapshot.scope).unwrap();
}
