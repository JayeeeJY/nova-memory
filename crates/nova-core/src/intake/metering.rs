//! Content-free provider observations, independent from source validity and semantic commit.
//! These encrypted facts are not a tariff, invoice, budget settlement, or proof of quality.
use super::{storage::*, *};
use crate::VaultKey;
use rusqlite::{Connection, TransactionBehavior, params};

pub const COMPILATION_METERING_VERSION: &str = "nova.compilation-metering.v0.1";
const INTENT: &str = "compilation_meter_intent";
const OBSERVATION: &str = "compilation_meter_observation";
const PAGE_SIZE: usize = 50;
const MAX_RECORD_BYTES: usize = 8192;
const MAX_SAFE_COUNT: u64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilationTransport {
    OpenaiCompatibleJson,
    OpenaiCompatibleLocalSse,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderObservationOutcome {
    ResponseReceived,
    ResponseRejected,
    TransportFailed,
    DispatchNotStarted,
    WorkerInterrupted,
}
/// Provider-reported token counts, never independently verified usage or money. Cache counts
/// are a subset of input tokens, never added to total tokens. Missing cache remains unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: Option<u64>,
}
impl ProviderTokenUsage {
    pub fn validate(&self) -> Result<()> {
        if self.total_tokens > MAX_SAFE_COUNT
            || self.input_tokens.checked_add(self.output_tokens) != Some(self.total_tokens)
            || self
                .cached_input_tokens
                .is_some_and(|n| n > self.input_tokens)
        {
            return Err(invalid("invalid_provider_usage"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderObservation {
    pub outcome: ProviderObservationOutcome,
    pub usage: Option<ProviderTokenUsage>,
    pub elapsed_millis: Option<u64>,
    pub http_status: Option<u16>,
    pub curl_exit_code: Option<i32>,
}
impl ProviderObservation {
    fn validate(&self) -> Result<()> {
        if self.elapsed_millis.is_some_and(|n| n > MAX_SAFE_COUNT)
            || self.http_status.is_some_and(|n| !(100..=599).contains(&n))
            || self.curl_exit_code.is_some_and(|n| !(0..=255).contains(&n))
            || (matches!(
                self.outcome,
                ProviderObservationOutcome::DispatchNotStarted
                    | ProviderObservationOutcome::WorkerInterrupted
            ) && (self.usage.is_some() || self.http_status.is_some()))
            || (self.outcome == ProviderObservationOutcome::ResponseReceived
                && (self.http_status.is_none_or(|n| !(200..=299).contains(&n))
                    || self.curl_exit_code != Some(0)))
        {
            return Err(invalid("invalid_provider_observation"));
        }
        if let Some(usage) = &self.usage {
            usage.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationMeterIntent {
    pub schema_version: String,
    pub scope: AssetScope,
    pub job_id: String,
    pub attempt: u32,
    pub configuration_revision: u64,
    pub model_id: String,
    pub model_location: String,
    /// Keyed binding distinguishes provider configurations without retaining their endpoint.
    pub configuration_binding: String,
    pub protocol: CompilationTransport,
    pub authorized_at: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundIntent {
    intent: CompilationMeterIntent,
    lease_binding: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedProviderObservation {
    pub observed_at: String,
    pub observation: ProviderObservation,
}
#[derive(Debug, Serialize)]
pub struct CompilationUsageItem {
    pub sequence: i64,
    pub intent: CompilationMeterIntent,
    pub observed: Option<RecordedProviderObservation>,
}
#[derive(Debug, Serialize)]
pub struct CompilationUsagePage {
    pub schema_version: &'static str,
    pub items: Vec<CompilationUsageItem>,
    pub next_before: Option<i64>,
    pub reporting_only: bool,
    pub monetary_cost: Option<u64>,
    pub cost_status: &'static str,
}
fn record_id(job_id: &str, attempt: u32) -> Result<String> {
    if !identifier(job_id) || !(1..=MAX_ATTEMPTS).contains(&attempt) {
        return Err(invalid("invalid_metering_attempt"));
    }
    Ok(format!("{job_id}:{attempt}"))
}
pub(super) fn reserve(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    attempt: &CompilationAttempt,
    protocol: CompilationTransport,
) -> Result<()> {
    let location = match attempt.configuration.selection {
        ModelSelection::Local { .. } => "local",
        ModelSelection::Remote { .. } => "remote",
        ModelSelection::Unconfigured => return Err(denied()),
    };
    let record = BoundIntent {
        intent: CompilationMeterIntent {
            schema_version: COMPILATION_METERING_VERSION.into(),
            scope: scope.clone(),
            job_id: attempt.job_id.clone(),
            attempt: attempt.attempt,
            configuration_revision: attempt.configuration.revision,
            model_id: attempt.compiler.model_id.clone(),
            model_location: location.into(),
            configuration_binding: key.authentication_tag(&[
                b"compilation-meter:configuration:v1",
                &serde_json::to_vec(&attempt.configuration)?,
            ]),
            protocol,
            authorized_at: attempt.dispatched_at.clone().ok_or_else(denied)?,
        },
        lease_binding: key
            .authentication_tag(&[b"compilation-meter:lease:v1", attempt.lease_id.as_bytes()]),
    };
    insert(
        c,
        key,
        INTENT,
        &record_id(&attempt.job_id, attempt.attempt)?,
        &record,
    )
}
fn bounded_load<T: serde::de::DeserializeOwned>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
) -> Result<T> {
    let size: usize = c.query_row(
        "SELECT length(ciphertext) FROM intake_objects WHERE id=?1 AND kind=?2",
        params![physical_id(kind, id), kind],
        |r| r.get(0),
    )?;
    if size > MAX_RECORD_BYTES {
        return Err(invalid("metering_record_limit"));
    }
    load(c, key, kind, id)
}
impl IntakeStore {
    /// Trusted runtime only: no HTTP capture input may write provider observations. The original
    /// lease binding remains valid for accounting after retry, source deletion or config change;
    /// it grants no permission to commit model output or read withdrawn sources.
    pub fn record_compilation_observation(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        attempt: u32,
        lease_id: &str,
        observation: ProviderObservation,
    ) -> Result<RecordedProviderObservation> {
        self.check(scope)?;
        observation.validate()?;
        let id = record_id(job_id, attempt)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        let bound: BoundIntent = bounded_load(&tx, &self.key, INTENT, &id)?;
        if bound.intent.scope != *scope
            || bound.intent.job_id != job_id
            || bound.intent.attempt != attempt
            || bound.lease_binding
                != self
                    .key
                    .authentication_tag(&[b"compilation-meter:lease:v1", lease_id.as_bytes()])
        {
            return Err(denied());
        }
        if exists(&tx, OBSERVATION, &id)? {
            let old: RecordedProviderObservation = bounded_load(&tx, &self.key, OBSERVATION, &id)?;
            if old.observation != observation {
                return Err(conflict());
            }
            return Ok(old);
        }
        let recorded = RecordedProviderObservation {
            observed_at: now()?,
            observation,
        };
        insert(&tx, &self.key, OBSERVATION, &id, &recorded)?;
        tx.commit()?;
        Ok(recorded)
    }
    /// Owner-scoped, bounded immutable history. Unobserved intents remain explicitly unknown,
    /// including process crashes. No totals across models, money conversion or inferred zeroes.
    pub fn compilation_usage(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<CompilationUsagePage> {
        self.check(scope)?;
        if before.is_some_and(|n| n <= 0) {
            return Err(invalid("invalid_metering_cursor"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut q = tx.prepare("SELECT rowid,id FROM intake_objects WHERE kind=?1 AND rowid<?2 ORDER BY rowid DESC LIMIT ?3")?;
        let rows = q
            .query_map(
                params![INTENT, before.unwrap_or(i64::MAX), PAGE_SIZE + 1],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let more = rows.len() > PAGE_SIZE;
        let mut items = vec![];
        for (sequence, physical) in rows.into_iter().take(PAGE_SIZE) {
            let prefix = physical_id(INTENT, "");
            let id = physical.strip_prefix(&prefix).ok_or_else(denied)?;
            let bound: BoundIntent = bounded_load(&tx, &self.key, INTENT, id)?;
            if bound.intent.scope != *scope
                || bound.intent.schema_version != COMPILATION_METERING_VERSION
            {
                return Err(denied());
            }
            let observed = if exists(&tx, OBSERVATION, id)? {
                Some(bounded_load(&tx, &self.key, OBSERVATION, id)?)
            } else {
                None
            };
            items.push(CompilationUsageItem {
                sequence,
                intent: bound.intent,
                observed,
            });
        }
        let next_before = more.then(|| items.last().expect("full page").sequence);
        Ok(CompilationUsagePage {
            schema_version: COMPILATION_METERING_VERSION,
            items,
            next_before,
            reporting_only: true,
            monetary_cost: None,
            cost_status: "not_measured",
        })
    }
}
