//! Offline fixed-response controller. There is deliberately no execute/model mode.
use nova_core::{NovaError, compilation::decode_untrusted_json};
use nova_review::structured_suite::{
    self, FixtureProfile, FixtureResponse, FixtureSuite, StepPermit, TargetMapping,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    sync::mpsc,
    time::Duration,
};

fn error() -> NovaError {
    NovaError::Validation("structured_suite_fixture: protocol_stopped".into())
}
fn emit(value: Value) -> Result<(), NovaError> {
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() > 1024 * 1024 {
        return Err(error());
    }
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
fn commands() -> Result<mpsc::Receiver<Result<Vec<u8>, ()>>, NovaError> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("suite-fixture-input".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            loop {
                let mut line = Vec::new();
                let result = loop {
                    let data = match input.fill_buf() {
                        Ok(data) => data,
                        Err(_) => break Err(()),
                    };
                    if data.is_empty() {
                        if line.is_empty() {
                            return;
                        }
                        break Err(());
                    }
                    let count = data
                        .iter()
                        .position(|b| *b == b'\n')
                        .map_or(data.len(), |n| n + 1);
                    if line.len() + count > 512 * 1024 {
                        break Err(());
                    }
                    line.extend_from_slice(&data[..count]);
                    input.consume(count);
                    if line.last() == Some(&b'\n') {
                        break Ok(line);
                    }
                };
                let stop = result.is_err();
                if tx.send(result).is_err() || stop {
                    return;
                }
            }
        })?;
    Ok(rx)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    command: String,
    schema_version: String,
    fixture_only: bool,
    plan_id: String,
}
#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Prepare {
        target_mapping: Option<TargetMapping>,
    },
    CompleteFixture {
        permit: Box<StepPermit>,
        response: Option<FixtureResponse>,
    },
    Finish,
}
fn read<T: serde::de::DeserializeOwned>(
    rx: &mpsc::Receiver<Result<Vec<u8>, ()>>,
) -> Result<T, NovaError> {
    let bytes = rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| error())?
        .map_err(|_| error())?;
    decode_untrusted_json(&bytes)
}
fn serve(profile: FixtureProfile) -> Result<(), NovaError> {
    emit(json!({"event":"hello", "metadata":structured_suite::metadata_for(profile)}))?;
    let rx = commands()?;
    let begin: Begin = read(&rx)?;
    if begin.command != "begin_fixture"
        || begin.schema_version != structured_suite::PROTOCOL
        || !begin.fixture_only
    {
        return Err(error());
    }
    let mut suite = FixtureSuite::new_with_profile(&begin.plan_id, profile)?;
    emit(suite.ready())?;
    let outcome = (|| -> Result<(), NovaError> {
        for _ in 0..25 {
            match read(&rx)? {
                Command::Prepare { target_mapping } => emit(
                    json!({"event":"prepared", "prepared":suite.prepare_next(target_mapping)?}),
                )?,
                Command::CompleteFixture { permit, response } => emit(
                    json!({"event":"completed", "result":suite.complete_fixture(&permit, response)?}),
                )?,
                Command::Finish => {
                    if suite.summary()["mechanical_fixture_complete"] != true {
                        return Err(error());
                    }
                    emit(json!({"event":"finished", "summary":suite.summary()}))?;
                    return Ok(());
                }
            }
        }
        Err(error())
    })();
    if outcome.is_err() {
        let mut summary = suite.summary();
        summary["aborted"] = json!(true);
        summary["mechanical_fixture_complete"] = json!(false);
        emit(json!({"event":"aborted", "summary":summary, "error":"fixture_protocol_stopped"}))?;
    }
    outcome
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let outcome = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--describe"] => emit(structured_suite::metadata()),
        ["--fixture-stdio"] => serve(FixtureProfile::Legacy),
        ["--describe", "--condition-list"] => {
            emit(structured_suite::metadata_for(FixtureProfile::ConditionList))
        }
        ["--fixture-stdio", "--condition-list"] => serve(FixtureProfile::ConditionList),
        _ => Err(error()),
    };
    if outcome.is_err() {
        eprintln!("offline suite fixture stopped");
        std::process::exit(1);
    }
}
