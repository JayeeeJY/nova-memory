//! Offline fixed responses only. No sockets, model services, or user stores.
use nova_review::structured_suite::*;
use serde_json::{Value, json};
fn fp(v: &impl serde::Serialize) -> String {
    hash(&serde_json::to_vec(&serde_json::to_value(v).unwrap()).unwrap())
}
fn unit(source: &str, predicate: &str, value: Value, polarity: &str, modality: &str) -> Value {
    json!({"origin":"extracted","content":{"subject":"user","predicate":predicate,"value":value,"polarity":polarity,"modality":modality,"conditions":{}},"valid_time":{"kind":"unknown"},"quotes":[{"quote":source}]})
}
fn reply(p: &PreparedStep, split: bool) -> FixtureResponse {
    let source = p.model_input.as_ref().unwrap()["source_text"]
        .as_str()
        .unwrap();
    let units = match p.case_id.as_str() {
        "negation_context" => {
            if split {
                vec![
                    unit(
                        "我在工作场景不参加超过30分钟的会议",
                        "work:meeting_duration",
                        json!(30),
                        "negative",
                        "asserted",
                    ),
                    unit(
                        "私人聚会没有这个限制。",
                        "private:exception",
                        json!(true),
                        "positive",
                        "asserted",
                    ),
                ]
            } else {
                vec![unit(
                    source,
                    "work:meeting_duration",
                    json!({"minutes":30,"private_exception":true}),
                    "negative",
                    "asserted",
                )]
            }
        }
        "unknown_not_invented" | "injection_abstain" => vec![],
        "plan_not_decision" => vec![unit(
            source,
            "learning:plan",
            json!("piano"),
            "positive",
            "planned",
        )],
        "temporal_before" => {
            let mut u = unit(source, "work:office", json!("A"), "positive", "asserted");
            u["valid_time"] = json!({"kind":"interval","start":"2026-01-01T00:00:00+08:00","end":"2026-06-01T00:00:00+08:00"});
            vec![u]
        }
        "temporal_after" => {
            let mut u = unit(source, "work:office", json!("B"), "positive", "asserted");
            u["valid_time"] =
                json!({"kind":"interval","start":"2026-06-01T00:00:00+08:00","end":null});
            vec![u]
        }
        "support_a" | "support_b" => vec![unit(
            source,
            "weekend:activity",
            json!("walking"),
            "positive",
            "asserted",
        )],
        "potential_difference" => vec![unit(
            source,
            "weekend:activity",
            json!("cycling"),
            "positive",
            "asserted",
        )],
        "correct_negation" => {
            let mut u = unit(
                source,
                "unused:placeholder",
                Value::Null,
                "negative",
                "asserted",
            );
            u["content"] = p.model_input.as_ref().unwrap()["correction_target"].clone();
            if u["content"]["value"].is_object() {
                u["content"]["value"]["minutes"] = json!(60)
            } else {
                u["content"]["value"] = json!(60)
            }
            vec![u]
        }
        _ => panic!("not a model step"),
    };
    FixtureResponse {
        fixture_only: true,
        content: json!({"schema_version":"nova.extraction.quote-text.v0.1","units":units})
            .to_string(),
        usage: Some(FixtureUsage {
            prompt_tokens: 100,
            completion_tokens: 100,
            total_tokens: 200,
        }),
        finish_reason: "stop".into(),
        sse_done: true,
        error_code: None,
    }
}
fn mapping(first: &StepResult) -> TargetMapping {
    // Explicit synthetic fixture author selection. This helper does not become a
    // production resolver or a rule that infers a target from arbitrary output.
    let chosen = first
        .units
        .iter()
        .filter(|u| u.content.polarity == nova_core::compilation::Polarity::Negative)
        .collect::<Vec<_>>();
    assert_eq!(chosen.len(), 1);
    TargetMapping {
        schema_version: "nova.acceptance.explicit-target-map.v0.1".into(),
        target_case: "negation_context".into(),
        candidates: first.output_versions.clone(),
        selected: vec![chosen[0].header.object.clone()],
        selected_unit_sha256: fp(chosen[0]),
        selection_evidence_sha256: hash(
            b"independent synthetic fixture author selected the work constraint",
        ),
        criterion: "work_meeting_duration_constraint".into(),
    }
}
fn through_nine(s: &mut FixtureSuite, split: bool) -> StepResult {
    let mut first = None;
    for _ in 0..9 {
        let p = s.prepare_next(None).unwrap();
        let result = s
            .complete_fixture(&p.permit().unwrap(), Some(reply(&p, split)))
            .unwrap();
        if first.is_none() {
            first = Some(result);
        }
    }
    first.unwrap()
}
#[test]
fn target_revision_cas_rejects_a_concurrent_undo_without_extra_dispatch() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let first = through_nine(&mut s, true);
    let p = s.prepare_next(Some(mapping(&first))).unwrap();
    s.complete_fixture(&p.permit().unwrap(), Some(reply(&p, true)))
        .unwrap();
    let undo = s.prepare_next(None).unwrap();
    s.undo_prepared_target_for_fixture().unwrap();
    assert!(s.complete_fixture(&undo.permit().unwrap(), None).is_err());
    assert_eq!(s.summary()["simulated_dispatches"], 10);
    assert_eq!(s.summary()["real_model_dispatches"], 0);
    assert_eq!(s.summary()["completed_steps"], 10);
    assert_eq!(s.summary()["aborted"], true);
}
#[test]
fn complete_twelve_steps_with_split_units_records_all_targets_and_preserves_exception() {
    for split in [false, true] {
        let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
        let first = through_nine(&mut s, split);
        assert_eq!(first.output_versions.len(), if split { 2 } else { 1 });
        let p = s.prepare_next(Some(mapping(&first))).unwrap();
        let corrected = s
            .complete_fixture(&p.permit().unwrap(), Some(reply(&p, split)))
            .unwrap();
        assert_eq!(corrected.output_versions.len(), 1);
        assert_eq!(
            corrected.deterministic_checks["unselected_siblings_unchanged"],
            true
        );
        let p = s.prepare_next(None).unwrap();
        let undone = s.complete_fixture(&p.permit().unwrap(), None).unwrap();
        assert_eq!(undone.deterministic_checks["prior_content_restored"], true);
        assert_eq!(undone.simulated_dispatches, 10);
        let p = s.prepare_next(None).unwrap();
        let withdrawn = s.complete_fixture(&p.permit().unwrap(), None).unwrap();
        assert_eq!(
            withdrawn.deterministic_checks["independent_source_survives"],
            true
        );
        let summary = s.summary();
        assert_eq!(summary["completed_steps"], 12);
        assert_eq!(summary["simulated_dispatches"], 10);
        assert_eq!(summary["real_model_dispatches"], 0);
        assert_eq!(summary["mechanical_fixture_complete"], true);
        assert_eq!(summary["real_full_suite_accepted"], false);
        assert_eq!(summary["reserved_token_bound"], 0);
        assert_eq!(summary["settled_fixture_tokens"], 2000);
    }
}
#[test]
fn correction_without_explicit_target_is_not_selected_by_first_array_position() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    through_nine(&mut s, true);
    assert!(s.prepare_next(None).is_err());
    assert_eq!(s.summary()["simulated_dispatches"], 9);
    assert_eq!(s.summary()["aborted"], true);
}
#[test]
fn ambiguous_missing_changed_or_incomplete_target_mapping_stops_before_dispatch() {
    for mutation in 0..5 {
        let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
        let first = through_nine(&mut s, true);
        let mut m = mapping(&first);
        match mutation {
            0 => m.selected = first.output_versions.clone(),
            1 => m.selected.clear(),
            2 => m.selected_unit_sha256 = "b".repeat(64),
            3 => m.candidates.pop().map(|_| ()).unwrap(),
            _ => m.selected[0].revision += 1,
        };
        assert!(s.prepare_next(Some(m)).is_err());
        assert_eq!(s.summary()["simulated_dispatches"], 9);
    }
}
#[test]
fn prepared_dynamic_request_includes_actual_selected_content_and_all_sources() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let first = through_nine(&mut s, true);
    let p = s.prepare_next(Some(mapping(&first))).unwrap();
    assert_eq!(p.context["sources"].as_array().unwrap().len(), 2);
    assert_eq!(p.context["units"].as_array().unwrap().len(), 1);
    assert_eq!(
        p.model_input.as_ref().unwrap()["correction_target"]["value"],
        30
    );
    assert_eq!(p.request_sha256, fp(&p.model_request));
    assert_eq!(p.context_sha256, fp(&p.context));
}
#[test]
fn request_instance_profile_plan_nonce_and_sequence_tampering_rejected_without_dispatch() {
    for field in [
        "request_sha256",
        "context_sha256",
        "target_mapping_sha256",
        "prepared_sha256",
        "instance_id",
        "profile_id",
        "plan_id",
        "run_nonce",
        "profile",
        "case_id",
    ] {
        let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
        let p = s.prepare_next(None).unwrap();
        let mut v = serde_json::to_value(p.permit().unwrap()).unwrap();
        v[field] = json!("tampered");
        let permit: StepPermit = serde_json::from_value(v).unwrap();
        assert!(
            s.complete_fixture(&permit, Some(reply(&p, true))).is_err(),
            "{field}"
        );
        assert_eq!(s.summary()["simulated_dispatches"], 0);
    }
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let p = s.prepare_next(None).unwrap();
    let mut permit = p.permit().unwrap();
    permit.sequence += 1;
    assert!(s.complete_fixture(&permit, Some(reply(&p, true))).is_err());
}
#[test]
fn replay_and_out_of_order_steps_never_repeat_a_simulated_dispatch() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let p = s.prepare_next(None).unwrap();
    let permit = p.permit().unwrap();
    s.complete_fixture(&permit, Some(reply(&p, true))).unwrap();
    assert!(s.complete_fixture(&permit, Some(reply(&p, true))).is_err());
    assert_eq!(s.summary()["simulated_dispatches"], 1);
    assert!(s.prepare_next(None).is_err());
}
#[test]
fn source_withdrawal_between_preparation_and_permission_is_fenced() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let p = s.prepare_next(None).unwrap();
    s.withdraw_prepared_source_for_fixture().unwrap();
    assert!(
        s.complete_fixture(&p.permit().unwrap(), Some(reply(&p, true)))
            .is_err()
    );
    assert_eq!(s.summary()["simulated_dispatches"], 0);
}
#[test]
fn configuration_cas_between_preparation_and_permission_stops_dispatch() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let p = s.prepare_next(None).unwrap();
    s.change_configuration_for_fixture().unwrap();
    assert!(
        s.complete_fixture(&p.permit().unwrap(), Some(reply(&p, true)))
            .is_err()
    );
    assert_eq!(s.summary()["simulated_dispatches"], 0);
}
#[test]
fn missing_usage_and_over_bound_usage_hold_reservation_and_stop_first_failure() {
    for bad in 0..3 {
        let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
        let p = s.prepare_next(None).unwrap();
        let mut r = reply(&p, true);
        match bad {
            0 => r.usage = None,
            1 => {
                r.usage = Some(FixtureUsage {
                    prompt_tokens: 131201,
                    completion_tokens: 1,
                    total_tokens: 131202,
                })
            }
            _ => {
                r.usage = Some(FixtureUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 3,
                })
            }
        };
        assert!(s.complete_fixture(&p.permit().unwrap(), Some(r)).is_err());
        assert_eq!(s.summary()["simulated_dispatches"], 1);
        assert_eq!(s.summary()["reserved_token_bound"], 133248);
        assert!(s.prepare_next(None).is_err());
    }
}
#[test]
fn partial_invalid_or_positioned_output_never_commits_and_keeps_known_usage() {
    for bad in 0..5 {
        let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
        let p = s.prepare_next(None).unwrap();
        let mut r = reply(&p, true);
        match bad {
            0 => r.finish_reason = "length".into(),
            1 => r.sse_done = false,
            2 => r.content.push_str("trailing"),
            3 => {
                r.content = r
                    .content
                    .replace("\"quote\":", "\"occurrence\":0,\"quote\":")
            }
            _ => {
                r.content = r
                    .content
                    .replace("私人聚会没有这个限制。", "私人聚会没有这个限制.")
            }
        };
        assert!(s.complete_fixture(&p.permit().unwrap(), Some(r)).is_err());
        assert_eq!(s.summary()["completed_steps"], 0);
        assert_eq!(s.summary()["settled_fixture_tokens"], 200);
        assert_eq!(s.summary()["reserved_token_bound"], 0);
    }
}
#[test]
fn no_model_steps_reject_injected_fixture_response() {
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let first = through_nine(&mut s, true);
    let p = s.prepare_next(Some(mapping(&first))).unwrap();
    let r = reply(&p, true);
    s.complete_fixture(&p.permit().unwrap(), Some(r.clone()))
        .unwrap();
    let p = s.prepare_next(None).unwrap();
    assert!(s.complete_fixture(&p.permit().unwrap(), Some(r)).is_err());
    assert_eq!(s.summary()["simulated_dispatches"], 10);
}
#[test]
fn fixture_protocol_does_not_enable_real_model_or_freeform_catalog() {
    let meta = metadata();
    assert_eq!(meta["real_model_path_available"], false);
    assert_eq!(meta["real_model_dispatches"], 0);
    assert_eq!(meta["automatic_retries"], 0);
    assert!(FixtureSuite::new("arbitrary").is_err());
    let mut s = FixtureSuite::new(&"a".repeat(64)).unwrap();
    let p = s.prepare_next(None).unwrap();
    let mut r = reply(&p, true);
    r.fixture_only = false;
    assert!(s.complete_fixture(&p.permit().unwrap(), Some(r)).is_err());
    assert_eq!(s.summary()["simulated_dispatches"], 0);
}
