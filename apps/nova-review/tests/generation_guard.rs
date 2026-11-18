//! Deterministic byte-fragment and protocol tests. No model, network, or Vault access.
use nova_review::generation_guard::{
    CompletionEvidence, GenerationGuard, GenerationGuardError as Error,
    MAX_GENERATION_DECODED_BYTES, MAX_GENERATION_WIRE_BYTES,
};
use serde_json::{Value, json};

fn unit(occurrence: usize, context: &str, value: Value) -> Value {
    json!({
        "origin":"extracted",
        "content":{
            "subject":"user", "predicate":"work:prefers", "value":value,
            "polarity":"negative", "modality":"asserted",
            "conditions":{"scope:context":context}
        },
        "occurred_at":null, "valid_time":{"kind":"unknown"},
        "quotes":[{"quote":"我不参加超过30分钟的会议。", "occurrence":occurrence}]
    })
}
fn output(units: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":"nova.extraction.v0.1", "units":units})).unwrap()
}
fn clean() -> CompletionEvidence<'static> {
    CompletionEvidence {
        finish_reason: Some("stop"),
        sse_done: true,
        ..Default::default()
    }
}
fn feed_all(bytes: &[u8]) -> GenerationGuard {
    let mut guard = GenerationGuard::new();
    guard.record_wire(bytes.len()).unwrap();
    guard.feed_content(bytes).unwrap();
    guard
}

#[test]
fn every_byte_fragment_preserves_utf8_escaping_and_nested_json() {
    let nested = json!({
        "sentence":"我说：\"不参加\"；\\路径；} ] , : { [",
        "array":[1, -2, 3.5, true, false, null, {"units":["not a unit"]}],
        "other":"\n\t\r"
    });
    let bytes = output(vec![unit(0, "work", nested)]);
    let mut guard = GenerationGuard::new();
    for fragment in bytes.chunks(1) {
        guard.record_wire(fragment.len()).unwrap();
        guard.feed_content(fragment).unwrap();
    }
    let accepted = guard.finish(clean()).unwrap();
    assert_eq!(accepted.content, bytes);
    assert_eq!(accepted.completed_units, 1);
    assert_eq!(accepted.wire_bytes, bytes.len());
    assert_eq!(accepted.usage, None);
}

#[test]
fn nested_units_arrays_and_repeated_sentences_do_not_trigger_loop_detection() {
    let nested = json!({"units":[{"value":"same"},{"value":"same"},{"value":"same"}],
        "text":"repeat repeat repeat repeat repeat"});
    let bytes = output(vec![unit(0, "work", nested)]);
    assert_eq!(feed_all(&bytes).finish(clean()).unwrap().completed_units, 1);
}

#[test]
fn third_exact_complete_consecutive_unit_stops_before_array_or_object_closes() {
    let one = unit(0, "work", json!("long meetings"));
    let mut guard = GenerationGuard::new();
    guard
        .feed_content(b"{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[")
        .unwrap();
    for index in 0..3 {
        if index > 0 {
            guard.feed_content(b",").unwrap();
        }
        let result = guard.feed_content(&serde_json::to_vec(&one).unwrap());
        if index < 2 {
            result.unwrap();
        } else {
            assert_eq!(result, Err(Error::RepeatedUnits));
        }
    }
    assert_eq!(guard.completed_units(), 3);
    assert_eq!(guard.feed_content(b"]}"), Err(Error::RepeatedUnits));
    assert_eq!(guard.record_wire(1), Err(Error::RepeatedUnits));
    assert_eq!(guard.finish(clean()).unwrap_err(), Error::RepeatedUnits);
}

#[test]
fn equality_decodes_string_escapes_and_ignores_object_key_order() {
    let one = unit(0, "work", json!("a"));
    let first = one.to_string();
    let second = format!(
        "{{\"quotes\":{},\"valid_time\":{},\"occurred_at\":null,\"content\":{},\"origin\":\"extracted\"}}",
        one["quotes"], one["valid_time"], one["content"]
    );
    let third = first.replace("\"a\"", "\"\\u0061\"");
    let mut guard = GenerationGuard::new();
    let prefix =
        format!("{{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[{first},{second},");
    guard.feed_content(prefix.as_bytes()).unwrap();
    assert_eq!(
        guard.feed_content(third.as_bytes()),
        Err(Error::RepeatedUnits)
    );
}

#[test]
fn different_occurrences_remain_distinct_but_seventeenth_unit_fails_early() {
    let mut guard = GenerationGuard::new();
    guard
        .feed_content(b"{\"schema_version\":\"nova.extraction.v0.1\",\"units\":[")
        .unwrap();
    for occurrence in 0..17 {
        if occurrence > 0 {
            guard.feed_content(b",").unwrap();
        }
        let result = guard
            .feed_content(&serde_json::to_vec(&unit(occurrence, "work", json!("same"))).unwrap());
        if occurrence < 16 {
            result.unwrap();
        } else {
            assert_eq!(result, Err(Error::UnitsLimit));
        }
    }
    assert_eq!(guard.completed_units(), 17);
    assert_eq!(guard.finish(clean()).unwrap_err(), Error::UnitsLimit);
}

#[test]
fn sixteen_supported_variants_are_not_collapsed_by_value() {
    let units = (0..16)
        .map(|occurrence| unit(occurrence, "work", json!("same")))
        .collect();
    assert_eq!(
        feed_all(&output(units))
            .finish(clean())
            .unwrap()
            .completed_units,
        16
    );
}

#[test]
fn different_conditions_or_quotes_never_count_as_exact_repeats() {
    let first = unit(0, "work", json!("same"));
    let second = unit(0, "private", json!("same"));
    let mut third = unit(0, "private", json!("same"));
    third["quotes"][0]["quote"] = json!("A different source anchor.");
    let mut fourth = third.clone();
    fourth["occurred_at"] = json!("2026-10-18T00:00:00Z");
    assert_eq!(
        feed_all(&output(vec![first, second, third, fourth]))
            .finish(clean())
            .unwrap()
            .completed_units,
        4
    );
}

#[test]
fn intervening_distinct_unit_resets_exact_repeat_run() {
    let a = unit(0, "work", json!("A"));
    let b = unit(0, "work", json!("B"));
    assert_eq!(
        feed_all(&output(vec![a.clone(), a.clone(), b, a.clone(), a]))
            .finish(clean())
            .unwrap()
            .completed_units,
        5
    );
}

#[test]
fn duplicate_keys_including_escaped_names_and_opaque_values_are_rejected() {
    for bytes in [
        b"{\"units\":[],\"\\u0075nits\":[]}".as_slice(),
        b"{\"units\":[{\"value\":{\"same\":0,\"same\":1}}]}".as_slice(),
    ] {
        let mut guard = GenerationGuard::new();
        assert_eq!(guard.feed_content(bytes), Err(Error::DuplicateJsonKey));
        assert_eq!(guard.finish(clean()).unwrap_err(), Error::DuplicateJsonKey);
    }
}

#[test]
fn complete_object_prefix_is_never_salvaged_from_extra_content() {
    for suffix in [
        b"{}".as_slice(),
        b" null".as_slice(),
        b"x".as_slice(),
        b"\xc2\xa0".as_slice(),
    ] {
        let mut guard = feed_all(&output(vec![]));
        assert!(guard.feed_content(suffix).is_err() || guard.finish(clean()).is_err());
    }
    let mut guard = GenerationGuard::new();
    assert_eq!(
        guard.feed_content(b"[]"),
        Err(Error::TopLevelObjectRequired)
    );
}

#[test]
fn malformed_structures_and_non_object_unit_members_fail_without_repair() {
    for bytes in [
        b"{\"units\":[{},]}".as_slice(),
        b"{\"units\":[],}".as_slice(),
        b"{\"units\"[]}".as_slice(),
        b"{\"units\":[]]".as_slice(),
        b"{\"units\":[\"not object\"]}".as_slice(),
        b"{\"units\":[null]}".as_slice(),
        b"{\"units\":[[]]}".as_slice(),
        b"{\"units\":{}}".as_slice(),
        b"{\"units\":[],\"text\":\"\\uZZZZ\"}".as_slice(),
        b"{\"units\":[],\"text\":\"\xff\"}".as_slice(),
        b"{\"units\":[],\"text\":\"raw\ncontrol\"}".as_slice(),
        b"{\"units\":[],\"value\":truefalse}".as_slice(),
    ] {
        let mut guard = GenerationGuard::new();
        assert!(
            guard.feed_content(bytes).is_err() || guard.finish(clean()).is_err(),
            "{bytes:?}"
        );
    }
}

#[test]
fn unfinished_objects_strings_primitives_and_utf8_are_not_completed_at_eof() {
    for bytes in [
        b"{\"units\":[".as_slice(),
        b"{\"units\":[],\"v\":tru".as_slice(),
        b"{\"units\":[],\"v\":\"unfinished".as_slice(),
        b"{\"units\":[],\"v\":\"\xe6\x88".as_slice(),
    ] {
        let guard = feed_all(bytes);
        assert!(guard.finish(clean()).is_err());
    }
}

#[test]
fn wire_and_decoded_caps_accept_exact_boundary_and_reject_one_more_or_overflow() {
    let mut bytes = output(vec![]);
    bytes.resize(MAX_GENERATION_DECODED_BYTES, b' ');
    let mut guard = GenerationGuard::new();
    guard.record_wire(MAX_GENERATION_WIRE_BYTES).unwrap();
    guard.feed_content(&bytes).unwrap();
    assert_eq!(guard.decoded_bytes(), MAX_GENERATION_DECODED_BYTES);
    guard.finish(clean()).unwrap();
    let mut guard = GenerationGuard::new();
    guard.record_wire(MAX_GENERATION_WIRE_BYTES).unwrap();
    assert_eq!(guard.record_wire(1), Err(Error::WireLimit));
    assert_eq!(guard.feed_content(b"{}"), Err(Error::WireLimit));
    let mut guard = GenerationGuard::new();
    guard.record_wire(1).unwrap();
    assert_eq!(guard.record_wire(usize::MAX), Err(Error::WireLimit));
    let mut guard = GenerationGuard::new();
    guard.feed_content(&bytes).unwrap();
    assert_eq!(guard.feed_content(b" "), Err(Error::DecodedLimit));
    assert_eq!(guard.finish(clean()).unwrap_err(), Error::DecodedLimit);
}

#[test]
fn protocol_failures_reject_even_a_complete_valid_object_with_distinct_codes() {
    let cases = [
        (
            CompletionEvidence {
                finish_reason: Some("length"),
                ..clean()
            },
            Error::OutputLength,
        ),
        (
            CompletionEvidence {
                finish_reason: Some("content_filter"),
                ..clean()
            },
            Error::NonStopFinish,
        ),
        (
            CompletionEvidence {
                finish_reason: None,
                ..clean()
            },
            Error::MissingFinishReason,
        ),
        (
            CompletionEvidence {
                sse_done: false,
                ..clean()
            },
            Error::MissingDone,
        ),
        (
            CompletionEvidence {
                provider_error: true,
                ..clean()
            },
            Error::ProviderError,
        ),
        (
            CompletionEvidence {
                tools_present: true,
                ..clean()
            },
            Error::ToolOutput,
        ),
    ];
    for (evidence, error) in cases {
        assert_eq!(
            feed_all(&output(vec![])).finish(evidence).unwrap_err(),
            error
        );
        assert!(error.code().starts_with("generation_"));
    }
}

#[test]
fn missing_usage_is_unknown_and_valid_usage_is_preserved() {
    assert_eq!(
        feed_all(&output(vec![])).finish(clean()).unwrap().usage,
        None
    );
    let usage = json!({"prompt_tokens":611,"completion_tokens":2048,"total_tokens":2659});
    let accepted = feed_all(&output(vec![]))
        .finish(CompletionEvidence {
            usage: Some(&usage),
            ..clean()
        })
        .unwrap();
    let observed = accepted.usage.unwrap();
    assert_eq!(observed.prompt_tokens, 611);
    assert_eq!(observed.completion_tokens, 2048);
    assert_eq!(observed.total_tokens, 2659);
    let zero = json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0});
    assert!(
        feed_all(&output(vec![]))
            .finish(CompletionEvidence {
                usage: Some(&zero),
                ..clean()
            })
            .is_ok()
    );
}

#[test]
fn invalid_usage_is_never_coerced_or_substituted_with_zero() {
    for usage in [
        Value::Null,
        json!({}),
        json!({"prompt_tokens":1,"completion_tokens":2}),
        json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":4}),
        json!({"prompt_tokens":-1,"completion_tokens":2,"total_tokens":1}),
        json!({"prompt_tokens":1.0,"completion_tokens":2,"total_tokens":3}),
        json!({"prompt_tokens":"1","completion_tokens":2,"total_tokens":3}),
        json!({"prompt_tokens":true,"completion_tokens":2,"total_tokens":3}),
        json!({"prompt_tokens":u64::MAX,"completion_tokens":1,"total_tokens":0}),
    ] {
        assert_eq!(
            feed_all(&output(vec![]))
                .finish(CompletionEvidence {
                    usage: Some(&usage),
                    ..clean()
                })
                .unwrap_err(),
            Error::InvalidUsage
        );
    }
}

#[test]
fn trusted_schema_parser_checks_the_entire_output_after_syntax_and_protocol() {
    for bytes in [
        br#"{"schema_version":"wrong","units":[]}"#.as_slice(),
        br#"{"schema_version":"nova.extraction.v0.1","units":[],"unknown":true}"#.as_slice(),
        br#"{"schema_version":"nova.extraction.v0.1","units":[{"origin":"extracted","content":{}}]}"#.as_slice(),
    ] {
        assert_eq!(feed_all(bytes).finish(clean()).unwrap_err(), Error::InvalidExtraction);
    }
}

#[test]
fn depth_limit_bounds_opaque_json_before_recursive_decode() {
    let mut bytes = b"{\"units\":[],\"opaque\":".to_vec();
    bytes.extend(std::iter::repeat_n(b'[', 128));
    let mut guard = GenerationGuard::new();
    assert_eq!(guard.feed_content(&bytes), Err(Error::JsonDepthLimit));
}

#[test]
fn quote_text_envelope_is_explicit_and_still_requires_complete_clean_protocol() {
    let bytes = br#"{"schema_version":"nova.extraction.quote-text.v0.1","units":[]}"#;
    assert!(feed_all(bytes).finish(clean()).is_err());
    assert!(feed_all(bytes).finish_quote_text_envelope(clean()).is_ok());
    assert!(
        feed_all(&output(vec![]))
            .finish_quote_text_envelope(clean())
            .is_err()
    );
    assert!(
        feed_all(br#"{"schema_version":"nova.extraction.quote-text.v0.1","units":["#)
            .finish_quote_text_envelope(clean())
            .is_err()
    );
    assert_eq!(
        feed_all(bytes)
            .finish_quote_text_envelope(CompletionEvidence {
                finish_reason: Some("length"),
                sse_done: true,
                ..Default::default()
            })
            .unwrap_err(),
        Error::OutputLength
    );
    assert_eq!(
        feed_all(bytes)
            .finish_quote_text_envelope(CompletionEvidence {
                finish_reason: Some("stop"),
                sse_done: false,
                ..Default::default()
            })
            .unwrap_err(),
        Error::MissingDone
    );
}
