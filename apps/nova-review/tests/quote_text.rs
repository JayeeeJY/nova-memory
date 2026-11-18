//! Hand-authored, offline fixtures only. No model inference, network, or personal Vault.
//! The temporary-store test exercises the real core transaction, not model semantics.
use nova_core::{CaptureFidelity, PrivacyLevel, VaultKey, compilation::*, intake::*};
use nova_review::quote_text::{
    MAX_SOURCE_BYTES, QUOTE_TEXT_VERSION, QuoteTextError, SemanticAcceptance, resolve,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn fixture(quote: &str) -> Value {
    json!({
        "schema_version": QUOTE_TEXT_VERSION,
        "units": [{
            "origin": "extracted",
            "content": {"subject":"user", "predicate":"meeting:attends",
                "value":{"duration_minutes":{"greater_than":30}}, "polarity":"negative",
                "modality":"asserted", "conditions":{"scope:context":"work"}},
            "occurred_at":null, "valid_time":{"kind":"unknown"},
            "quotes":[{"quote":quote}]
        }]
    })
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn sha(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn expect(source: &str, value: &Value, error: QuoteTextError) {
    assert_eq!(resolve(source, &bytes(value)).unwrap_err(), error);
}

#[test]
fn unique_chinese_and_emoji_offsets_bind_original_and_actual_resolved_bytes() {
    let source = "前缀🧠：工作中🙂保持原话。尾部";
    let quote = "工作中🙂保持原话。";
    let input = fixture(quote);
    let raw = serde_json::to_vec_pretty(&input).unwrap();
    let resolved = resolve(source, &raw).unwrap();
    let proof = &resolved.proofs()[0];
    assert_eq!((proof.unit_index, proof.quote_index), (0, 0));
    assert_eq!(proof.start_char, 4);
    assert_eq!(proof.end_char, 4 + quote.chars().count());
    assert_eq!(proof.start_byte, "前缀🧠：".len());
    assert_eq!(&source[proof.start_byte..proof.end_byte], quote);
    assert_eq!(resolved.source_sha256(), sha(source.as_bytes()));
    assert_eq!(resolved.raw_sha256(), sha(&raw));
    assert_ne!(resolved.raw_sha256(), sha(&bytes(&input)));
    assert_eq!(resolved.resolved_sha256(), sha(resolved.resolved_bytes()));
    assert_eq!(resolved.extraction().units[0].quotes[0].occurrence, 0);
    assert_eq!(resolved.extraction().units[0].quotes[0].quote, quote);
    assert_eq!(resolved.extraction().schema_version, EXTRACTION_VERSION);
    assert_eq!(
        bytes(&serde_json::to_value(resolved.extraction()).unwrap()).len(),
        resolved.resolved_bytes().len()
    );
    assert_eq!(
        serde_json::to_value(ExtractionOutput::parse(resolved.resolved_bytes()).unwrap()).unwrap(),
        serde_json::to_value(resolved.extraction()).unwrap()
    );
    assert_eq!(
        resolved.semantic_acceptance(),
        SemanticAcceptance::PendingIndependentReview
    );
}

#[test]
fn repeated_and_overlapping_exact_matches_are_ambiguous() {
    for (source, quote) in [
        ("first first", "first"),
        ("aaa", "aa"),
        ("中中中", "中中"),
        ("🙂🙂🙂", "🙂🙂"),
        ("ababa", "aba"),
        ("同句。其他。同句。", "同句。"),
    ] {
        expect(source, &fixture(quote), QuoteTextError::QuoteAmbiguous);
    }
    // The full repeated-looking phrase is itself unique; do not reject by word repetition.
    resolve("前缀中中中后缀", &bytes(&fixture("中中中"))).unwrap();
}

#[test]
fn no_punctuation_unicode_whitespace_or_case_normalization() {
    for (source, quote) in [
        ("原话。", "原话."),
        ("café", "cafe\u{301}"),
        ("cafe\u{301}", "café"),
        ("A B", "a b"),
        ("a  b", "a b"),
        ("中文", "其他"),
        ("👩‍💻", "👩💻"),
    ] {
        expect(source, &fixture(quote), QuoteTextError::QuoteAbsent);
    }
}

#[test]
fn old_contract_and_occurrence_fields_are_rejected_never_repaired() {
    let mut old = fixture("原话");
    old["schema_version"] = json!(EXTRACTION_VERSION);
    expect("原话", &old, QuoteTextError::UnsupportedVersion);
    for occurrence in [json!(0), json!(12), json!(null), json!(-1)] {
        let mut input = fixture("原话");
        input["units"][0]["quotes"][0]["occurrence"] = occurrence;
        expect("原话", &input, QuoteTextError::InvalidShape);
    }
    old["units"][0]["quotes"][0]["occurrence"] = json!(12);
    assert!(resolve("原话", &bytes(&old)).is_err());
}

#[test]
fn all_closed_objects_and_invalid_json_are_rejected() {
    for pointer in [
        "",
        "/units/0",
        "/units/0/content",
        "/units/0/valid_time",
        "/units/0/quotes/0",
    ] {
        let mut input = fixture("原话");
        input
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        expect("原话", &input, QuoteTextError::InvalidShape);
    }
    for raw in [b"{}".as_slice(), b"[]", b"null", b"{", b"\xff", b"{} {}"] {
        assert_eq!(
            resolve("原话", raw).unwrap_err(),
            QuoteTextError::InvalidShape
        );
    }
}

#[test]
fn duplicate_keys_rejected_recursively_including_escaped_equivalent_keys() {
    let original = String::from_utf8(bytes(&fixture("原话"))).unwrap();
    let duplicate_root = original.replace(
        "\"schema_version\":",
        "\"schema_version\":\"ignored\",\"schema_version\":",
    );
    let duplicate_unit = original.replace("\"origin\":", "\"origin\":\"inferred\",\"origin\":");
    let duplicate_quote = original.replace(
        "\"quote\":\"原话\"",
        "\"quote\":\"原话\",\"qu\\u006fte\":\"原话\"",
    );
    let duplicate_dynamic = original.replace(
        "\"greater_than\":30",
        "\"greater_than\":30,\"greater_than\":60",
    );
    let escaped_dynamic = original.replace(
        "\"scope:context\":\"work\"",
        "\"scope:context\":{\"k\":1,\"\\u006b\":2}",
    );
    for raw in [
        duplicate_root,
        duplicate_unit,
        duplicate_quote,
        duplicate_dynamic,
        escaped_dynamic,
    ] {
        assert_ne!(raw, original);
        assert_eq!(
            resolve("原话", raw.as_bytes()).unwrap_err(),
            QuoteTextError::InvalidShape
        );
    }
}

#[test]
fn quotes_use_utf8_byte_bounds_and_reject_blank_evidence() {
    for quote in [
        "".into(),
        " \n\t".into(),
        "a".repeat(4097),
        "中".repeat(1366),
    ] {
        expect(&quote, &fixture(&quote), QuoteTextError::QuoteBounds);
    }
    for quote in ["a".repeat(4096), format!("{}a", "中".repeat(1365))] {
        assert_eq!(quote.len(), 4096);
        resolve(&quote, &bytes(&fixture(&quote))).unwrap();
    }
}

#[test]
fn unit_quote_source_raw_and_resolved_bounds_are_separate() {
    let mut input = fixture("字");
    let unit = input["units"][0].clone();
    input["units"] = json!(vec![unit.clone(); 17]);
    expect("字", &input, QuoteTextError::UnitLimit);
    input["units"] = json!(vec![unit.clone(); 16]);
    assert_eq!(resolve("字", &bytes(&input)).unwrap().proofs().len(), 16);
    for count in [0, 17] {
        input["units"] = json!([unit.clone()]);
        input["units"][0]["quotes"] = json!(vec![json!({"quote":"字"}); count]);
        expect("字", &input, QuoteTextError::QuoteCount);
    }
    input["units"][0]["quotes"] = json!(vec![json!({"quote":"字"}); 16]);
    assert_eq!(resolve("字", &bytes(&input)).unwrap().proofs().len(), 16);
    let empty = json!({"schema_version":QUOTE_TEXT_VERSION,"units":[]});
    resolve(&"a".repeat(MAX_SOURCE_BYTES), &bytes(&empty)).unwrap();
    expect(
        &"a".repeat(MAX_SOURCE_BYTES + 1),
        &empty,
        QuoteTextError::SourceLimit,
    );
    assert_eq!(
        resolve("字", &vec![b' '; MAX_MODEL_BYTES + 1]).unwrap_err(),
        QuoteTextError::RawLimit
    );
    let mut large = fixture("字");
    large["units"][0]["content"]["value"] = json!("");
    let padding = MAX_MODEL_BYTES - 1 - bytes(&large).len();
    large["units"][0]["content"]["value"] = json!("x".repeat(padding));
    assert_eq!(bytes(&large).len(), MAX_MODEL_BYTES - 1);
    expect("字", &large, QuoteTextError::ResolvedLimit);
}

#[test]
fn origin_allowlist_and_empty_abstention_preserve_pending_semantics() {
    for origin in [
        "extracted",
        "inferred",
        "external_claim",
        "agent_experience",
    ] {
        let mut input = fixture("字");
        input["units"][0]["origin"] = json!(origin);
        resolve("字", &bytes(&input)).unwrap();
    }
    let mut input = fixture("字");
    input["units"][0]["origin"] = json!("user_statement");
    expect("字", &input, QuoteTextError::OriginNotAllowed);
    input["units"] = json!([]);
    let result = resolve("", &bytes(&input)).unwrap();
    assert!(result.extraction().units.is_empty());
    assert!(result.proofs().is_empty());
    assert_eq!(
        result.semantic_acceptance(),
        SemanticAcceptance::PendingIndependentReview
    );
}

fn meeting_fixture() -> (&'static str, Value) {
    let source = "🧠 我在工作场景不参加超过30分钟的会议；私人聚会没有这个限制。";
    let mut input = fixture("我在工作场景不参加超过30分钟的会议");
    let mut exception = input["units"][0].clone();
    exception["content"]["predicate"] = json!("meeting:duration_restriction");
    exception["content"]["value"] = json!(false);
    exception["content"]["polarity"] = json!("positive");
    exception["content"]["conditions"]["scope:context"] = json!("private_gathering");
    exception["quotes"] = json!([{"quote":"私人聚会没有这个限制。"}]);
    input["units"].as_array_mut().unwrap().push(exception);
    (source, input)
}

#[test]
fn hand_authored_negative_rule_and_exception_are_preserved_without_semantic_approval() {
    let (source, input) = meeting_fixture();
    let resolved = resolve(source, &bytes(&input)).unwrap();
    let output = serde_json::to_value(resolved.extraction()).unwrap();
    assert_eq!(output["units"].as_array().unwrap().len(), 2);
    for i in 0..2 {
        for name in ["origin", "content", "occurred_at", "valid_time"] {
            assert_eq!(output["units"][i][name], input["units"][i][name]);
        }
        assert_eq!(resolved.proofs()[i].unit_index, i);
    }
    assert_eq!(output["units"][0]["content"]["polarity"], "negative");
    assert_eq!(output["units"][1]["content"]["value"], false);
    assert_eq!(
        resolved.semantic_acceptance(),
        SemanticAcceptance::PendingIndependentReview
    );
}

#[test]
fn recursive_semantic_values_times_and_qualifiers_survive_translation() {
    let mut input = fixture("字");
    input["units"][0]["content"]["value"] = json!([null, true, -2, 3.25, "文本", {"x": [false]}]);
    input["units"][0]["content"]["conditions"]["custom:context"] = json!({"开放":null});
    input["units"][0]["content"]["modality"] = json!("planned");
    input["units"][0]["occurred_at"] = json!("2026-01-02T00:00:00+08:00");
    input["units"][0]["valid_time"] =
        json!({"kind":"interval","start":"2026-02-01T00:00:00Z","end":null});
    let resolved = resolve("字", &bytes(&input)).unwrap();
    let out = serde_json::to_value(resolved.extraction()).unwrap();
    for name in ["origin", "content", "occurred_at", "valid_time"] {
        assert_eq!(out["units"][0][name], input["units"][0][name]);
    }
}

fn numeric_fixture(literal: &str) -> Vec<u8> {
    let mut input = fixture("字");
    input["units"][0]["content"]["value"] = json!("NUMERIC_SLOT");
    String::from_utf8(bytes(&input))
        .unwrap()
        .replace("\"NUMERIC_SLOT\"", literal)
        .into_bytes()
}

#[test]
fn decimal_equivalent_serialization_passes_but_numeric_precision_loss_fails_closed() {
    for literal in [
        "1.50",
        "1.5",
        "1e+0",
        "1E3",
        "0.00012",
        "-0",
        "-0.0",
        "0e999",
        "9007199254740993",
        "18446744073709551615",
        "-9223372036854775808",
    ] {
        resolve("字", &numeric_fixture(literal))
            .unwrap_or_else(|e| panic!("exact decimal {literal} refused: {e}"));
    }
    for literal in [
        "18446744073709551617",
        "-9223372036854775809",
        "0.12345678901234567890123456789",
        "1e-400",
        "0.10000000000000001",
    ] {
        assert_eq!(
            resolve("字", &numeric_fixture(literal)).unwrap_err(),
            QuoteTextError::NumericPrecision,
            "{literal}"
        );
    }
    for literal in ["1e309", "1e-9223372036854775809", "0e9223372036854775808"] {
        assert!(
            resolve("字", &numeric_fixture(literal)).is_err(),
            "{literal}"
        );
    }
}

#[test]
fn numeric_guard_ignores_strings_and_checks_nested_numbers() {
    let mut input = fixture("字");
    input["units"][0]["content"]["value"] = json!({
        "18446744073709551617": "quoted \" 18446744073709551617 and slash \\ 1e-400",
        "长小数": "0.12345678901234567890123456789"
    });
    let resolved = resolve("字", &bytes(&input)).unwrap();
    assert_eq!(
        serde_json::to_value(&resolved.extraction().units[0].content).unwrap()["value"],
        input["units"][0]["content"]["value"]
    );
    let raw = numeric_fixture(r#"{"x":[1.5,{"nested":18446744073709551617}]}"#);
    assert_eq!(
        resolve("字", &raw).unwrap_err(),
        QuoteTextError::NumericPrecision
    );
}

#[test]
fn shared_decoder_container_and_recursion_limits_remain_enforced() {
    let mut input = fixture("字");
    input["units"][0]["content"]["value"] = json!(vec![Value::Null; 1025]);
    expect("字", &input, QuoteTextError::InvalidShape);
    let object: serde_json::Map<String, Value> =
        (0..1025).map(|i| (format!("k{i}"), Value::Null)).collect();
    input["units"][0]["content"]["value"] = json!(object);
    expect("字", &input, QuoteTextError::InvalidShape);
    let deep = format!("{}0{}", "[".repeat(140), "]".repeat(140));
    assert_eq!(
        resolve("字", &numeric_fixture(&deep)).unwrap_err(),
        QuoteTextError::InvalidShape
    );
}

#[test]
fn actual_disposable_core_commit_and_source_withdrawal_keep_semantic_unverified() {
    let (source, input) = meeting_fixture();
    let resolved = resolve(source, &bytes(&input)).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let scope = AssetScope {
        owner_id: "owner:synthetic-quote-text".into(),
        space: "quote-text".into(),
    };
    let mut store = IntakeStore::create(
        &temp.path().join("intake.sqlite"),
        VaultKey::generate().unwrap(),
        scope.clone(),
    )
    .unwrap();
    store.enable_compilation(&scope).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-offline-fixture".into(),
                endpoint: "http://127.0.0.1:11434/v1".into(),
            },
        )
        .unwrap();
    let receipt = store
        .receive(
            &IntakeAuthority {
                scope: scope.clone(),
                actor_id: "actor:synthetic-quote-text".into(),
                capture_allowed: true,
                role: SourceRole::User,
                privacy_level: PrivacyLevel::Private,
            },
            &serde_json::to_vec(&IntakeRequest {
                schema_version: INTAKE_VERSION.into(),
                request_id: "synthetic-quote-text".into(),
                source_platform: "synthetic-offline".into(),
                capture_fidelity: CaptureFidelity::F2VisibleTranscript,
                content: source.into(),
                occurred_at: None,
            })
            .unwrap(),
        )
        .unwrap();
    let lease = store
        .claim_next(&scope, 60)
        .unwrap()
        .unwrap()
        .lease
        .unwrap()
        .lease_id;
    store
        .prepare_compilation(&scope, &receipt.job_id, &lease)
        .unwrap();
    // Authorization is local synthetic bookkeeping. No transport or inference is invoked.
    store
        .authorize_dispatch(&scope, &receipt.job_id, &lease, 1)
        .unwrap();
    let committed = store
        .commit_compilation(&scope, &receipt.job_id, &lease, resolved.resolved_bytes())
        .unwrap();
    assert_eq!(committed.output_versions.len(), 2);
    assert_eq!(committed.semantic_support, "model_output_unverified");
    for (i, revision) in committed.output_versions.iter().enumerate() {
        let unit = store.compiled_unit(&scope, &revision.object_id).unwrap();
        assert_eq!(unit.unit.content, resolved.extraction().units[i].content);
        assert_eq!(
            unit.unit.evidence[0].start_char,
            resolved.proofs()[i].start_char
        );
        assert_eq!(
            unit.unit.evidence[0].end_char,
            resolved.proofs()[i].end_char
        );
    }
    store
        .withdraw_source(&scope, &receipt.source.object_id)
        .unwrap();
    assert!(store.compiled_units(&scope, None).unwrap().is_empty());
    for revision in &committed.output_versions {
        assert!(store.compiled_unit(&scope, &revision.object_id).is_err());
    }
}

#[test]
fn failures_return_only_stable_safe_codes() {
    let error = resolve("secret source", &bytes(&fixture("private missing quote"))).unwrap_err();
    assert_eq!(error.code(), "quote_text_quote_absent");
    assert_eq!(error.to_string(), error.code());
    assert!(!error.to_string().contains("secret"));
    assert!(!error.to_string().contains("private"));
}
