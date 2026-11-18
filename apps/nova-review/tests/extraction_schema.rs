//! Independent JSON Schema validation plus unchanged core wire decoding.
//! No model, network, personal Vault or credentials are involved. Quote-boundary
//! tests use disposable synthetic stores and the actual core commit transaction.
use nova_core::{CaptureFidelity, PrivacyLevel, VaultKey, compilation::*, intake::*};
use nova_review::compiler_schema::{
    PROVIDER_SCHEMA_CONTRACT_VERSION, PROVIDER_SCHEMA_REMOVED_POINTER, SCHEMA_JSON,
    extraction_response_format, provider_schema, provider_schema_json, schema,
};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn fixture() -> Value {
    json!({
        "schema_version": "nova.extraction.v0.1",
        "units": [{
            "origin": "extracted",
            "content": {
                "subject": "user",
                "predicate": "custom.namespace:future_relation-2",
                "value": null,
                "polarity": "positive",
                "modality": "asserted",
                "conditions": {}
            },
            "occurred_at": null,
            "valid_time": {"kind": "unknown"},
            "quotes": [{"quote": "synthetic source", "occurrence": 0}]
        }]
    })
}

/// Use an installed independent validator, never a handwritten mirror of the schema.
/// Missing jsonschema is a clear failure, not a skipped passing test or a dependency download.
fn validate_cases(cases: &[(String, Value)]) -> Vec<bool> {
    validate_against(schema(), cases)
}

fn validate_against(selected_schema: Value, cases: &[(String, Value)]) -> Vec<bool> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("NOVA_JSON_SCHEMA_PYTHON") {
        candidates.push(path);
    }
    candidates.extend([
        "/opt/homebrew/bin/python3.14".into(),
        "/opt/homebrew/bin/python3".into(),
        "python3".into(),
    ]);
    let python = candidates.into_iter().find(|path| {
        Command::new(path)
            .args(["-c", "from jsonschema import Draft202012Validator"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status().is_ok_and(|status| status.success())
    }).expect("JSON Schema tests need an already installed Python jsonschema package; set NOVA_JSON_SCHEMA_PYTHON to that interpreter. Tests never install dependencies.");
    let script = r#"
import json, sys
from jsonschema import Draft202012Validator
request = json.load(sys.stdin)
Draft202012Validator.check_schema(request['schema'])
validator = Draft202012Validator(request['schema'])
print(json.dumps([validator.is_valid(case[1]) for case in request['cases']]))
"#;
    let mut child = Command::new(python)
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start independent JSON Schema validator");
    let input = serde_json::to_vec(&json!({"schema": selected_schema, "cases": cases})).unwrap();
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "schema validator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("validator returns boolean results")
}

#[test]
fn response_format_embeds_fixed_provider_projection_without_strict_claim() {
    let format = extraction_response_format();
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["schema"], provider_schema());
    assert!(format["json_schema"].get("strict").is_none());
    assert_eq!(
        schema(),
        serde_json::from_str::<Value>(SCHEMA_JSON).unwrap()
    );
    assert_eq!(
        schema()["properties"]["schema_version"]["const"],
        nova_core::intake::EXTRACTION_VERSION
    );
}

#[test]
fn provider_projection_removes_exactly_the_quote_generation_maximum() {
    let canonical = schema();
    let projected = provider_schema();
    assert_eq!(
        PROVIDER_SCHEMA_CONTRACT_VERSION,
        "nova.extraction.ollama-schema.v0.1"
    );
    assert_eq!(
        canonical.pointer(PROVIDER_SCHEMA_REMOVED_POINTER),
        Some(&json!(4096))
    );
    assert!(projected.pointer(PROVIDER_SCHEMA_REMOVED_POINTER).is_none());
    assert_eq!(
        projected["$defs"]["quote"]["properties"]["quote"]["minLength"],
        1
    );
    // Restoring this one key must reproduce every canonical field and value.
    let mut restored = projected.clone();
    restored["$defs"]["quote"]["properties"]["quote"]["maxLength"] = json!(4096);
    assert_eq!(restored, canonical);
    assert_eq!(
        serde_json::from_str::<Value>(&provider_schema_json()).unwrap(),
        projected
    );
    assert_eq!(provider_schema_json(), projected.to_string());
    // Calling the projection cannot mutate the embedded canonical schema.
    assert_eq!(schema(), canonical);
}

#[test]
fn provider_subset_uses_explicit_local_recursive_json_union() {
    fn inspect(value: &Value) {
        match value {
            Value::Object(fields) => {
                assert!(
                    !fields.is_empty(),
                    "empty schema narrows to object in Ollama 0.20.2"
                );
                for forbidden in ["strict", "patternProperties", "uniqueItems"] {
                    assert!(
                        !fields.contains_key(forbidden),
                        "unsupported enforcement keyword {forbidden}"
                    );
                }
                if let Some(reference) = fields.get("$ref") {
                    assert!(
                        reference.as_str().unwrap().starts_with("#/$defs/"),
                        "schema must not fetch remote references"
                    );
                }
                fields.values().for_each(inspect);
            }
            Value::Array(items) => items.iter().for_each(inspect),
            _ => {}
        }
    }
    let schema = schema();
    inspect(&schema);
    let branches = schema["$defs"]["json_value"]["anyOf"].as_array().unwrap();
    let types: std::collections::BTreeSet<_> = branches
        .iter()
        .map(|branch| branch["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["null", "boolean", "number", "string", "array", "object"].into()
    );
    assert_eq!(
        schema["$defs"]["content"]["properties"]["conditions"]["additionalProperties"]["$ref"],
        "#/$defs/json_value"
    );
}

#[test]
fn open_semantics_preserve_every_json_value_type_and_decode_with_core() {
    let values = [
        json!(null),
        json!(true),
        json!(false),
        json!(42),
        json!(-3.25),
        json!("Unicode 中文 and new vocabulary"),
        json!([]),
        json!({}),
        json!([null, true, 4, "text", {"nested": [false, null, {"deeper": 2.5}]}]),
        json!({"open key": null, "another": [1, {"custom": "value"}]}),
    ];
    let mut cases = Vec::new();
    for (i, value) in values.into_iter().enumerate() {
        let mut output = fixture();
        output["units"][0]["content"]["value"] = value.clone();
        output["units"][0]["content"]["conditions"]["novel:dynamic_condition"] = value;
        cases.push((format!("json-value-{i}"), output));
    }
    let mut absent_optional = fixture();
    absent_optional["units"][0]
        .as_object_mut()
        .unwrap()
        .remove("occurred_at");
    cases.push(("optional-occurred-at-absent".into(), absent_optional));
    for end in [None, Some(Value::Null), Some(json!("2027-01-01T00:00:00Z"))] {
        let mut output = fixture();
        output["units"][0]["valid_time"] =
            json!({"kind":"interval", "start":"2026-01-01T00:00:00Z"});
        if let Some(end) = end {
            output["units"][0]["valid_time"]["end"] = end;
        }
        cases.push(("interval-optional-end".into(), output));
    }
    for origin in [
        "extracted",
        "inferred",
        "external_claim",
        "agent_experience",
    ] {
        let mut output = fixture();
        output["units"][0]["origin"] = json!(origin);
        cases.push((format!("automatic-origin-{origin}"), output));
    }
    for modality in ["asserted", "planned", "hypothetical", "unknown"] {
        let mut output = fixture();
        output["units"][0]["content"]["modality"] = json!(modality);
        output["units"][0]["content"]["polarity"] = json!("negative");
        cases.push((format!("modality-{modality}"), output));
    }
    cases.push((
        "empty-supported-output".into(),
        json!({"schema_version":"nova.extraction.v0.1","units":[]}),
    ));
    let mut sixteen = fixture();
    sixteen["units"] = json!(vec![sixteen["units"][0].clone(); 16]);
    cases.push(("sixteen-typed-units-no-commit-claim".into(), sixteen));
    for ((name, output), valid) in cases.iter().zip(validate_cases(&cases)) {
        assert!(valid, "independent schema rejects {name}");
        ExtractionOutput::parse(&serde_json::to_vec(output).unwrap())
            .unwrap_or_else(|error| panic!("unchanged core wire parser rejects {name}: {error}"));
    }
}

#[test]
fn structural_schema_rejects_closed_field_enum_and_bound_violations() {
    let mut cases = Vec::new();
    for pointer in [
        "",
        "/units/0",
        "/units/0/content",
        "/units/0/valid_time",
        "/units/0/quotes/0",
    ] {
        let mut output = fixture();
        output
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        cases.push((format!("unknown-field-{pointer}"), output));
    }
    for pointer in [
        "/schema_version",
        "/units",
        "/units/0/origin",
        "/units/0/content",
        "/units/0/content/value",
        "/units/0/content/conditions",
        "/units/0/valid_time",
        "/units/0/quotes",
        "/units/0/quotes/0/occurrence",
    ] {
        let mut output = fixture();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        output
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        cases.push((format!("missing-field-{pointer}"), output));
    }
    for (pointer, bad) in [
        ("/schema_version", json!("wrong")),
        ("/units/0/origin", json!("user_statement")),
        ("/units/0/origin", json!("user_confirmed")),
        ("/units/0/origin", json!("new_origin")),
        ("/units/0/content/polarity", json!("neutral")),
        ("/units/0/content/modality", json!("certain")),
        ("/units/0/content/predicate", json!("one_of_seven")),
        ("/units/0/content/conditions", json!([])),
        (
            "/units/0/valid_time",
            json!({"kind":"unknown", "start":"invented"}),
        ),
        ("/units/0/valid_time", json!({"kind":"interval"})),
        ("/units/0/quotes", json!([])),
        ("/units/0/quotes/0/quote", json!("")),
        ("/units/0/quotes/0/occurrence", json!(-1)),
        ("/units/0/quotes/0/occurrence", json!(65)),
        ("/units/0/quotes/0/occurrence", json!(0.5)),
    ] {
        let mut output = fixture();
        *output.pointer_mut(pointer).unwrap() = bad;
        cases.push((format!("invalid-{pointer}"), output));
    }
    let mut seventeen = fixture();
    seventeen["units"] = json!(vec![seventeen["units"][0].clone(); 17]);
    cases.push(("seventeen-units".into(), seventeen));
    let mut seventeen_quotes = fixture();
    seventeen_quotes["units"][0]["quotes"] =
        json!(vec![json!({"quote":"synthetic", "occurrence":0}); 17]);
    cases.push(("seventeen-quotes".into(), seventeen_quotes));
    for ((name, _), valid) in cases.iter().zip(validate_cases(&cases)) {
        assert!(!valid, "independent schema accepts {name}");
    }
}

#[test]
fn structural_acceptance_never_claims_grounding_or_semantic_commit() {
    // JSON schema cannot relate this quote to any selected source. It also cannot
    // establish support for the claim; only the existing core/context and review can.
    let mut ungrounded = fixture();
    ungrounded["units"][0]["quotes"][0]["quote"] = json!("not present in selected source");
    let cases = vec![("structurally-valid-ungrounded".into(), ungrounded)];
    assert_eq!(validate_cases(&cases), vec![true]);
    ExtractionOutput::parse(&serde_json::to_vec(&cases[0].1).unwrap()).unwrap();
    let duplicate_keys = br#"{"schema_version":"nova.extraction.v0.1","schema_version":"nova.extraction.v0.1","units":[]}"#;
    assert!(ExtractionOutput::parse(duplicate_keys).is_err());
}

#[test]
fn canonical_character_bounds_remain_independent_of_provider_projection() {
    let cases: Vec<_> = [
        ("ascii-at-character-limit", "a".repeat(4096)),
        ("ascii-over-character-limit", "a".repeat(4097)),
        ("multibyte-at-character-limit", "中".repeat(4096)),
        ("multibyte-over-character-limit", "中".repeat(4097)),
        (
            "multibyte-under-character-over-byte-limit",
            "中".repeat(1366),
        ),
        ("empty-quote", String::new()),
    ]
    .into_iter()
    .map(|(name, quote)| {
        let mut output = fixture();
        output["units"][0]["quotes"][0]["quote"] = json!(quote);
        (name.into(), output)
    })
    .collect();
    assert_eq!(
        validate_cases(&cases),
        vec![true, false, true, false, true, false]
    );
    assert_eq!(
        validate_against(provider_schema(), &cases),
        vec![true, true, true, true, true, false]
    );
    // The wire decoder alone must not be described as enforcing quote bounds.
    for (_, output) in &cases {
        ExtractionOutput::parse(&serde_json::to_vec(output).unwrap()).unwrap();
    }
}

#[test]
fn actual_core_commit_retains_utf8_byte_limit_after_provider_relaxation() {
    for (name, quote, accepted) in [
        ("ascii-at-byte-limit", "a".repeat(4096), true),
        ("ascii-over-byte-limit", "a".repeat(4097), false),
        (
            "multibyte-at-byte-limit",
            format!("{}a", "中".repeat(1365)),
            true,
        ),
        ("multibyte-over-byte-limit", "中".repeat(1366), false),
        ("multibyte-at-character-limit", "中".repeat(4096), false),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let scope = AssetScope {
            owner_id: "owner:synthetic-schema-boundary".into(),
            space: "synthetic-schema-boundary".into(),
        };
        let key = VaultKey::generate().unwrap();
        let mut store =
            IntakeStore::create(&temp.path().join("intake.sqlite"), key, scope.clone()).unwrap();
        store.enable_compilation(&scope).unwrap();
        store
            .configure_model(
                1,
                ModelSelection::Local {
                    model_id: "synthetic-schema-boundary".into(),
                    endpoint: "http://127.0.0.1:11434/v1".into(),
                },
            )
            .unwrap();
        let receipt = store
            .receive(
                &IntakeAuthority {
                    scope: scope.clone(),
                    actor_id: "actor:synthetic-schema-boundary".into(),
                    capture_allowed: true,
                    role: SourceRole::User,
                    privacy_level: PrivacyLevel::Private,
                },
                &serde_json::to_vec(&IntakeRequest {
                    schema_version: INTAKE_VERSION.into(),
                    request_id: name.into(),
                    source_platform: "synthetic-schema-boundary".into(),
                    capture_fidelity: CaptureFidelity::F2VisibleTranscript,
                    content: quote.clone(),
                    occurred_at: None,
                })
                .unwrap(),
            )
            .unwrap();
        let job = store.claim_next(&scope, 60).unwrap().unwrap();
        let lease_id = job.lease.unwrap().lease_id;
        store
            .prepare_compilation(&scope, &receipt.job_id, &lease_id)
            .unwrap();
        // This authorizes a synthetic attempt in the disposable store; no adapter,
        // HTTP request, model process, or credentials are used by this test.
        store
            .authorize_dispatch(&scope, &receipt.job_id, &lease_id, 1)
            .unwrap();
        let mut output = fixture();
        output["units"][0]["quotes"][0]["quote"] = json!(quote);
        let committed = store.commit_compilation(
            &scope,
            &receipt.job_id,
            &lease_id,
            &serde_json::to_vec(&output).unwrap(),
        );
        match (accepted, committed) {
            (true, Ok(committed)) => {
                assert_eq!(committed.output_versions.len(), 1, "{name}");
                let unit = store
                    .compiled_unit(&scope, &committed.output_versions[0].object_id)
                    .unwrap();
                assert_eq!(unit.unit.evidence[0].quote, quote, "{name}");
                assert_eq!(
                    unit.unit.evidence[0].end_char,
                    quote.chars().count(),
                    "{name}"
                );
                assert_eq!(
                    store.compiled_units(&scope, None).unwrap().len(),
                    1,
                    "{name}"
                );
            }
            (false, Err(error)) => {
                assert!(
                    error.to_string().contains("quote_bounds"),
                    "{name}: {error}"
                );
                assert!(
                    store.compiled_units(&scope, None).unwrap().is_empty(),
                    "{name}"
                );
            }
            (expected, actual) => panic!("{name}: expected accepted={expected}, got {actual:?}"),
        }
        assert_eq!(
            store
                .source(&scope, &receipt.source.object_id)
                .unwrap()
                .content,
            quote,
            "{name}"
        );
    }
}
