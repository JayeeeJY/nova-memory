//! Normal-worker transport observations. Never retains prompts, bodies, endpoints or credentials.
use super::*;
use nova_core::intake::{
    ProviderObservation, ProviderObservationOutcome as Outcome, ProviderTokenUsage,
};

pub struct ObservedGeneration {
    pub result: Result<Vec<u8>, NovaError>,
    pub observation: ProviderObservation,
}
impl ObservedGeneration {
    pub fn interrupted() -> Self {
        Self {
            result: Err(error("transport_worker_interrupted")),
            observation: ProviderObservation {
                outcome: Outcome::WorkerInterrupted,
                usage: None,
                elapsed_millis: None,
                http_status: None,
                curl_exit_code: None,
            },
        }
    }
}

// Parse accounting independently from output validity. Known duplicates, malformed counters,
// floats, signed values, arithmetic overflow and inconsistent cached subsets remain unknown.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "choices")]
    _choices: Option<serde::de::IgnoredAny>,
    usage: Option<WireUsage>,
}
#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_tokens_details: Option<PromptDetails>,
}
#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}
fn parse_usage(bytes: &[u8]) -> Option<ProviderTokenUsage> {
    let raw = serde_json::from_slice::<Envelope>(bytes).ok()?.usage?;
    let usage = ProviderTokenUsage {
        input_tokens: raw.prompt_tokens,
        output_tokens: raw.completion_tokens,
        total_tokens: raw.total_tokens,
        cached_input_tokens: raw.prompt_tokens_details.and_then(|d| d.cached_tokens),
    };
    usage.validate().ok()?;
    Some(usage)
}

pub(super) fn send(transport: Transport) -> ObservedGeneration {
    if transport.output_mode == OutputMode::StructuredLocal {
        let (result, report) = streaming::observe(transport);
        let outcome = if result.is_ok() {
            Outcome::ResponseReceived
        } else if !report.dispatch_started {
            Outcome::DispatchNotStarted
        } else if result.as_ref().err().is_some_and(|e| {
            let code = streaming::code(e);
            code.contains("transport")
                || code.contains("deadline")
                || code.contains("http_status")
                || code.contains("owned_child")
        }) {
            Outcome::TransportFailed
        } else {
            Outcome::ResponseRejected
        };
        let usage = report
            .usage
            .map(|u| ProviderTokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                // The existing SSE guard does not observe cache detail. Never infer a zero.
                cached_input_tokens: None,
            })
            .filter(|u| u.validate().is_ok());
        return ObservedGeneration {
            result: result.map(|g| g.content),
            observation: ProviderObservation {
                outcome,
                usage,
                elapsed_millis: Some(report.elapsed_ms),
                http_status: report.http_status,
                curl_exit_code: report.curl_exit_code,
            },
        };
    }
    match send_raw_observed(transport) {
        Ok(response) => {
            let usage = parse_usage(&response.bytes);
            let result = decode_response(&response.bytes);
            ObservedGeneration {
                observation: ProviderObservation {
                    outcome: if result.is_ok() {
                        Outcome::ResponseReceived
                    } else {
                        Outcome::ResponseRejected
                    },
                    usage,
                    elapsed_millis: Some(response.elapsed_millis),
                    http_status: Some(response.http_status),
                    curl_exit_code: Some(0),
                },
                result,
            }
        }
        Err(failure) => ObservedGeneration {
            observation: ProviderObservation {
                outcome: if matches!(failure.diagnostic.phase, "preflight" | "start") {
                    Outcome::DispatchNotStarted
                } else {
                    Outcome::TransportFailed
                },
                usage: None,
                elapsed_millis: Some(failure.diagnostic.elapsed_millis),
                http_status: failure.diagnostic.http_status,
                curl_exit_code: failure.diagnostic.curl_exit_code,
            },
            result: Err(failure.error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_are_reported_only_when_unambiguous_and_consistent() {
        for usage in [
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}"#,
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":3}}"#,
            r#"{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"prompt_tokens_details":{"cached_tokens":0}}"#,
        ] {
            assert!(
                parse_usage(format!(r#"{{"choices":[],"usage":{usage}}}"#).as_bytes()).is_some()
            );
        }
        for usage in [
            "null",
            "{}",
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":16}"#,
            r#"{"prompt_tokens":-1,"completion_tokens":5,"total_tokens":4}"#,
            r#"{"prompt_tokens":1.0,"completion_tokens":5,"total_tokens":6}"#,
            r#"{"prompt_tokens":1,"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}"#,
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":11}}"#,
            r#"{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":1,"cached_tokens":1}}"#,
            r#"{"prompt_tokens":18446744073709551615,"completion_tokens":1,"total_tokens":0}"#,
            r#"{"prompt_tokens":9007199254740992,"completion_tokens":0,"total_tokens":9007199254740992}"#,
        ] {
            assert!(
                parse_usage(format!(r#"{{"choices":[],"usage":{usage}}}"#).as_bytes()).is_none(),
                "{usage}"
            );
        }
        assert!(parse_usage(br#"{"usage":null,"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#).is_none());
        assert!(parse_usage(br#"{"choices":[],"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#).is_none());
    }
    #[test]
    fn usage_ignores_private_provider_fields_without_accepting_output() {
        let bytes = br#"{"choices":[{"finish_reason":"length","message":{"content":"NEVER_RETAIN_OUTPUT"}}],"secret":"NEVER_RETAIN_PROVIDER_METADATA","usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#;
        assert!(decode_response(bytes).is_err());
        let usage = parse_usage(bytes).unwrap();
        assert_eq!(usage.total_tokens, 15);
        assert_eq!(usage.cached_input_tokens, None);
        assert!(
            !serde_json::to_string(&usage)
                .unwrap()
                .contains("NEVER_RETAIN")
        );
    }
}
