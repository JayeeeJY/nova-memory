use nova_core::engine::{
    AssertionEvidence, AssertionTime, AtomicAssertion, CandidateKind, CandidateReason,
    analyze_assertions,
};
use nova_core::{
    AssertionOrigin, CaptureFidelity, MemoryStatus, MemoryType, NovaError, PrivacyLevel,
};

fn assertion(id: &str) -> AtomicAssertion {
    AtomicAssertion {
        assertion_id: id.into(),
        owner_id: "owner:synthetic".into(),
        entity_id: "project:nova".into(),
        memory_type: MemoryType::Decision,
        predicate: "current_host".into(),
        context_key: "founder_alpha:affirmative".into(),
        value: "chatgpt".into(),
        single_valued: true,
        valid_time: AssertionTime::Interval {
            start: "2026-10-01T00:00:00Z".into(),
            end: None,
        },
        observed_at: "2026-10-08T00:00:00Z".into(),
        confirmed_at: None,
        origin: AssertionOrigin::AiExtractedProposal,
        privacy_level: PrivacyLevel::Private,
        status: MemoryStatus::Active,
        evidence: vec![AssertionEvidence {
            source_artifact_id: format!("source:{id}"),
            locator: "/decisions/0".into(),
            fidelity: CaptureFidelity::F1AiDigest,
        }],
    }
}
fn candidate(a: &AtomicAssertion, b: &AtomicAssertion) -> (CandidateKind, CandidateReason) {
    let result = analyze_assertions(
        "owner:synthetic",
        std::slice::from_ref(a),
        std::slice::from_ref(b),
    )
    .unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert!(result.candidates[0].requires_review);
    assert_eq!(result.candidates[0].incoming_evidence, a.evidence);
    assert_eq!(result.candidates[0].existing_evidence, b.evidence);
    (result.candidates[0].kind, result.candidates[0].reason)
}
#[test]
fn exact_duplicate_keeps_both_evidence_chains_and_never_mutates_input() {
    let a = assertion("a");
    let mut b = assertion("b");
    b.valid_time = AssertionTime::Interval {
        start: "2026-10-01T08:00:00+08:00".into(),
        end: None,
    };
    let original = a.clone();
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::Duplicate,
            CandidateReason::SameClaimAndInterval
        )
    );
    assert_eq!(a, original);
    assert_eq!(a.origin, AssertionOrigin::AiExtractedProposal);
}
#[test]
fn overlapping_same_value_only_proposes_merge() {
    let a = assertion("a");
    let mut b = assertion("b");
    b.valid_time = AssertionTime::Interval {
        start: "2026-10-02T00:00:00Z".into(),
        end: None,
    };
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::MergeProposal,
            CandidateReason::SameValueOverlappingIntervals
        )
    );
}
#[test]
fn differing_values_require_exclusive_slot_for_conflict() {
    let a = assertion("a");
    let mut b = assertion("b");
    b.value = "other_host".into();
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::ConflictSet,
            CandidateReason::DifferentValuesInExclusiveSlot
        )
    );
    b.single_valued = false;
    assert_eq!(
        candidate(&a, &b),
        (CandidateKind::NeedsReview, CandidateReason::MultiValuedSlot)
    );
}
#[test]
fn adjacent_intervals_are_evolution_not_contradiction_or_merge() {
    let mut a = assertion("a");
    let mut b = assertion("b");
    b.value = "other_host".into();
    a.valid_time = AssertionTime::Interval {
        start: "2026-09-01T00:00:00Z".into(),
        end: Some("2026-10-01T00:00:00Z".into()),
    };
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::TemporalEvolution,
            CandidateReason::NonOverlappingIntervals
        )
    );
    b.value = a.value.clone();
    assert_eq!(candidate(&a, &b).0, CandidateKind::TemporalEvolution);
}
#[test]
fn unknown_time_never_becomes_infinite_time_or_exact_duplicate() {
    let mut a = assertion("a");
    let mut b = assertion("b");
    a.valid_time = AssertionTime::Unknown;
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::NeedsReview,
            CandidateReason::UnknownValidTime
        )
    );
    b.valid_time = AssertionTime::Unknown;
    assert_eq!(candidate(&a, &b).0, CandidateKind::NeedsReview);
}
#[test]
fn inference_and_privacy_boundaries_prevent_duplicate_or_merge_labels() {
    let a = assertion("a");
    let mut b = assertion("b");
    b.origin = AssertionOrigin::ModelInferred;
    assert_eq!(
        candidate(&a, &b),
        (
            CandidateKind::NeedsReview,
            CandidateReason::EpistemicOrPrivacyDifference
        )
    );
    b.origin = a.origin;
    b.privacy_level = PrivacyLevel::Sealed;
    assert_eq!(candidate(&a, &b).0, CandidateKind::NeedsReview);
}
#[test]
fn unrelated_entities_contexts_types_and_predicates_do_not_match() {
    let a = assertion("a");
    for variant in 0..4 {
        let mut b = assertion("b");
        match variant {
            0 => b.entity_id = "project:other".into(),
            1 => b.context_key = "later_phase:affirmative".into(),
            2 => b.memory_type = MemoryType::Goal,
            _ => b.predicate = "desired_host".into(),
        }
        let result = analyze_assertions("owner:synthetic", std::slice::from_ref(&a), &[b]).unwrap();
        assert!(result.candidates.is_empty());
        assert_eq!(result.unmatched_incoming_ids, vec!["a"]);
    }
}
#[test]
fn withdrawn_and_disputed_records_cannot_reenter_candidates() {
    let a = assertion("a");
    for status in [
        MemoryStatus::Deleted,
        MemoryStatus::Superseded,
        MemoryStatus::Disputed,
    ] {
        let mut b = assertion("b");
        b.status = status;
        let result = analyze_assertions("owner:synthetic", std::slice::from_ref(&a), &[b]).unwrap();
        assert!(result.candidates.is_empty());
        assert_eq!(result.excluded_existing_ids, vec!["b"]);
    }
    let mut b = assertion("b");
    b.origin = AssertionOrigin::Disputed;
    assert!(
        analyze_assertions("owner:synthetic", &[a], &[b])
            .unwrap()
            .candidates
            .is_empty()
    );
}
#[test]
fn validation_fails_closed_for_owner_ids_evidence_time_and_batch_size() {
    let a = assertion("a");
    let mut b = assertion("b");
    b.owner_id = "owner:other".into();
    assert!(matches!(
        analyze_assertions("owner:synthetic", std::slice::from_ref(&a), &[b]),
        Err(NovaError::PolicyDenied(_))
    ));
    assert!(
        analyze_assertions(
            "owner:synthetic",
            std::slice::from_ref(&a),
            std::slice::from_ref(&a)
        )
        .is_err()
    );
    for variant in 0..5 {
        let mut invalid = assertion("invalid");
        match variant {
            0 => invalid.evidence.clear(),
            1 => invalid.observed_at = "yesterday".into(),
            2 => {
                invalid.valid_time = AssertionTime::Interval {
                    start: "2026-10-01T00:00:00Z".into(),
                    end: Some("2026-09-01T00:00:00Z".into()),
                }
            }
            3 => invalid.status = MemoryStatus::Deleted,
            _ => invalid.predicate = " \n ".into(),
        }
        assert!(analyze_assertions("owner:synthetic", &[invalid], &[]).is_err());
    }
    assert!(analyze_assertions("owner:synthetic", &vec![a; 257], &[]).is_err());
}
#[test]
fn deterministic_report_roundtrips_with_all_reasons_and_evidence() {
    let a = assertion("a");
    let b = assertion("b");
    let result = analyze_assertions(
        "owner:synthetic",
        std::slice::from_ref(&a),
        std::slice::from_ref(&b),
    )
    .unwrap();
    assert_eq!(
        result,
        analyze_assertions("owner:synthetic", &[a], &[b]).unwrap()
    );
    let encoded = serde_json::to_string(&result).unwrap();
    assert_eq!(result, serde_json::from_str(&encoded).unwrap());
}

#[test]
fn dense_batches_fail_without_returning_a_partial_report() {
    let incoming: Vec<_> = (0..33).map(|i| assertion(&format!("in:{i}"))).collect();
    let existing: Vec<_> = (0..33).map(|i| assertion(&format!("old:{i}"))).collect();
    assert!(matches!(
        analyze_assertions("owner:synthetic", &incoming, &existing),
        Err(NovaError::Validation(_))
    ));
}
