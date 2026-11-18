//! Synthetic-only candidate report. No Vault, network, credentials or model use.
use nova_core::engine::{AssertionEvidence, AssertionTime, AtomicAssertion, analyze_assertions};
use nova_core::{AssertionOrigin, CaptureFidelity, MemoryStatus, MemoryType, PrivacyLevel};

fn assertion(id: &str, value: &str, time: AssertionTime) -> AtomicAssertion {
    AtomicAssertion {
        assertion_id: id.into(),
        owner_id: "owner:synthetic".into(),
        entity_id: "project:synthetic".into(),
        memory_type: MemoryType::Decision,
        predicate: "current_host".into(),
        context_key: "alpha:affirmative".into(),
        value: value.into(),
        single_valued: true,
        valid_time: time,
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
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let time = AssertionTime::Interval {
        start: "2026-10-01T00:00:00Z".into(),
        end: None,
    };
    let existing = vec![assertion("existing:1", "chatgpt", time.clone())];
    let incoming = vec![
        assertion("incoming:duplicate", "chatgpt", time.clone()),
        assertion("incoming:conflict", "other_host", time),
        assertion("incoming:unknown_time", "chatgpt", AssertionTime::Unknown),
        assertion(
            "incoming:merge",
            "chatgpt",
            AssertionTime::Interval {
                start: "2026-10-02T00:00:00Z".into(),
                end: None,
            },
        ),
    ];
    let report = analyze_assertions("owner:synthetic", &incoming, &existing)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
