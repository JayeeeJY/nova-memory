//! Bounded N2a extraction and transaction executor. No network/model code lives in the core.
use super::{storage::*, *};
use crate::{CaptureFidelity, VaultKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use time::OffsetDateTime;

#[path = "compilation_capacity.rs"]
pub(super) mod storage_capacity;
pub use storage_capacity::{CompilationStorageBudget, CompilationStorageStatus, COMPILATION_STORAGE_CAPACITY};

pub const EXTRACTION_VERSION: &str = "nova.extraction.v0.1";
pub const EXTRACTOR_VERSION: &str = "n2a-quotes-v1";
pub const RULES_VERSION: &str = "n2a-source-only-v1";
pub const MAX_MODEL_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteAnchor {
    pub quote: String,
    pub occurrence: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractedUnit {
    pub origin: SemanticOrigin,
    pub content: SemanticContent,
    pub occurred_at: Option<String>,
    pub valid_time: ValidTime,
    pub quotes: Vec<QuoteAnchor>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionOutput {
    pub schema_version: String,
    pub units: Vec<ExtractedUnit>,
}
impl ExtractionOutput {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_MODEL_BYTES {
            return Err(invalid("model_output_limit"));
        }
        let output: Self = crate::compilation::wire::decode(bytes, "invalid_extraction_output")?;
        if output.schema_version != EXTRACTION_VERSION || output.units.len() > 16 {
            return Err(invalid("extraction_bounds"));
        }
        Ok(output)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionIntent {
    pub target: RevisionRef,
}
#[derive(Debug, Clone)]
pub struct CorrectionAuthority {
    pub scope: AssetScope,
    pub actor_id: String,
    pub may_correct: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilationAttempt {
    pub job_id: String,
    pub attempt: u32,
    pub lease_id: String,
    pub configuration: ModelConfiguration,
    pub context: CompilationSnapshot,
    pub correction: Option<CorrectionIntent>,
    pub compiler: CompilerVersion,
    pub dispatched_at: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedCompilation {
    pub change: ChangeSet,
    pub job_id: Option<String>,
    pub read_set: Vec<RevisionRef>,
    pub output_versions: Vec<RevisionRef>,
    pub configuration_revision: u64,
    pub committed_at: String,
    pub actor_id: Option<String>,
    pub correction: Option<CorrectionIntent>,
    pub undoes_change_id: Option<String>,
    pub semantic_support: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledUnit {
    pub unit: MemoryUnit,
    pub protected: bool,
    pub change_id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    revision: u64,
    protected: bool,
    change_id: String,
}
#[derive(Serialize, Deserialize)]
struct Feature {
    version: String,
}
#[derive(Serialize, Deserialize)]
struct DispatchBudget {
    calls: u64,
}

impl IntakeStore {
    /// Explicit opt-in creates encrypted compiler metadata only; no old Source is processed here.
    pub fn enable_compilation(&mut self, scope: &AssetScope) -> Result<()> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !exists(&tx, "compiler_feature", "compiler_feature")? {
            insert(
                &tx,
                &self.key,
                "compiler_feature",
                "compiler_feature",
                &Feature {
                    version: "n2a.v0.1".into(),
                },
            )?;
            insert(
                &tx,
                &self.key,
                "dispatch_budget",
                "dispatch_budget",
                &DispatchBudget { calls: 0 },
            )?;
        }
        super::consolidation::initialize(&tx, &self.key)?;
        tx.commit()?;
        Ok(())
    }
    pub fn compilation_enabled(&self) -> Result<bool> {
        enabled(&self.connection, &self.key)
    }
    pub fn request_correction(
        &mut self,
        authority: &CorrectionAuthority,
        target: RevisionRef,
        request_id: String,
        instruction: String,
    ) -> Result<IntakeReceipt> {
        self.check(&authority.scope)?;
        if !authority.may_correct || !identifier(&authority.actor_id) {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_enabled(&tx, &self.key)?;
        // Metadata chosen from trusted existing asset, never lowered by the correction input.
        let old: MemoryUnit = load(&tx, &self.key, "unit_revision", &revision_id(&target))?;
        let request = IntakeRequest {
            schema_version: INTAKE_VERSION.into(),
            request_id,
            source_platform: "owner-correction".into(),
            capture_fidelity: CaptureFidelity::F2VisibleTranscript,
            content: instruction,
            occurred_at: None,
        };
        request.validate()?;
        let bound = IntakeAuthority {
            scope: authority.scope.clone(),
            actor_id: authority.actor_id.clone(),
            capture_allowed: true,
            role: SourceRole::User,
            privacy_level: old.header.privacy_level,
        };
        let receipt = receive_tx(
            &tx,
            &self.key,
            &self.scope,
            &bound,
            &request,
            Some(&CorrectionIntent { target }),
        )?;
        tx.commit()?;
        Ok(receipt)
    }
    /// Persist a frozen read context and config/lease binding. A repeated call returns that context.
    pub fn prepare_compilation(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
    ) -> Result<CompilationAttempt> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        require_enabled(&tx, &self.key)?;
        let record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        let config = check_lease(&tx, &self.key, &record, lease_id)?;
        let attempt_id = attempt_id(&record);
        if exists(&tx, "compilation_attempt", &attempt_id)? {
            return load(&tx, &self.key, "compilation_attempt", &attempt_id);
        }
        let source: SourceSnapshot = load_source(&tx, &self.key, scope, &record.job.source)?;
        let mut context = snapshot(source);
        let correction: Option<CorrectionIntent> =
            optional(&tx, &self.key, "correction_intent", job_id)?;
        if let Some(intent) = &correction {
            check_correction_target(&tx, &self.key, scope, intent)?;
            let selected: MemoryUnit = load(
                &tx,
                &self.key,
                "unit_revision",
                &revision_id(&intent.target),
            )?;
            for ev in &selected.evidence {
                if !context.sources.iter().any(|s| s.source == ev.source) {
                    context
                        .sources
                        .push(load_source(&tx, &self.key, scope, &ev.source)?);
                }
            }
            context.units.push(selected);
            if read_unit(&tx, &self.key, scope, &intent.target.object_id)?.protected {
                context
                    .protected_unit_ids
                    .insert(intent.target.object_id.clone());
            }
        }
        context.validate(scope)?;
        let model_id = match &config.selection {
            ModelSelection::Local { model_id, .. } | ModelSelection::Remote { model_id, .. } => {
                model_id.clone()
            }
            ModelSelection::Unconfigured => return Err(denied()),
        };
        let attempt = CompilationAttempt {
            job_id: job_id.into(),
            attempt: record.job.attempt,
            lease_id: lease_id.into(),
            configuration: config,
            context,
            correction,
            compiler: CompilerVersion {
                model_id,
                extractor_version: EXTRACTOR_VERSION.into(),
                rules_version: RULES_VERSION.into(),
            },
            dispatched_at: None,
        };
        insert(&tx, &self.key, "compilation_attempt", &attempt_id, &attempt)?;
        tx.commit()?;
        Ok(attempt)
    }
    /// Single dispatch per attempt, bounded lifetime call budget, current config/privacy/source checks.
    /// Caller still enforces endpoint, credential, byte/token/time limits immediately before transport.
    pub fn authorize_dispatch(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
        maximum_calls: u64,
    ) -> Result<CompilationAttempt> {
        self.authorize_dispatch_inner(scope, job_id, lease_id, maximum_calls, None)
    }
    /// Normal runtime dispatch with a durable, content-free accounting intent in the same
    /// transaction as the call budget. An intent alone does not prove a provider received a call.
    pub fn authorize_metered_dispatch(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
        maximum_calls: u64,
        protocol: CompilationTransport,
    ) -> Result<CompilationAttempt> {
        self.authorize_dispatch_inner(scope, job_id, lease_id, maximum_calls, Some(protocol))
    }
    fn authorize_dispatch_inner(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
        maximum_calls: u64,
        protocol: Option<CompilationTransport>,
    ) -> Result<CompilationAttempt> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        let record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        check_lease(&tx, &self.key, &record, lease_id)?;
        let mut attempt: CompilationAttempt =
            load(&tx, &self.key, "compilation_attempt", &attempt_id(&record))?;
        recheck_context(&tx, &self.key, scope, &attempt)?;
        if attempt.dispatched_at.is_some() {
            return Err(conflict());
        }
        // N2a deliberately does not process sensitive/sealed content, even on a local endpoint.
        if attempt.context.sources.iter().any(|s| {
            matches!(
                s.privacy_level,
                PrivacyLevel::Sensitive | PrivacyLevel::Sealed
            )
        }) {
            return Err(denied());
        }
        let mut budget: DispatchBudget =
            load(&tx, &self.key, "dispatch_budget", "dispatch_budget")?;
        if maximum_calls == 0 || budget.calls >= maximum_calls {
            return Err(invalid("model_call_budget_exhausted"));
        }
        budget.calls += 1;
        attempt.dispatched_at = Some(now()?);
        update(
            &tx,
            &self.key,
            "dispatch_budget",
            "dispatch_budget",
            &budget,
        )?;
        update(
            &tx,
            &self.key,
            "compilation_attempt",
            &attempt_id(&record),
            &attempt,
        )?;
        if let Some(protocol) = protocol {
            super::metering::reserve(&tx, &self.key, scope, &attempt, protocol)?;
        }
        tx.commit()?;
        Ok(attempt)
    }
    /// Strict output is converted to a restricted ChangeSet; transaction rechecks actual saved context.
    pub fn commit_compilation(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
        bytes: &[u8],
    ) -> Result<CommittedCompilation> {
        self.check(scope)?;
        let output = ExtractionOutput::parse(bytes)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        require_enabled(&tx, &self.key)?;
        let mut record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        // An idempotent finalization only returns a receipt for the identical model response/lease.
        let attempt: CompilationAttempt =
            load(&tx, &self.key, "compilation_attempt", &attempt_id(&record))?;
        if record.job.state == JobState::Ready {
            let digest: String = load(&tx, &self.key, "model_response_hash", job_id)?;
            if attempt.lease_id != lease_id || digest != self.key.authentication_tag(&[bytes]) {
                return Err(conflict());
            }
            return load(
                &tx,
                &self.key,
                "compilation_commit",
                record
                    .job
                    .committed_change_id
                    .as_ref()
                    .ok_or_else(conflict)?,
            );
        }
        check_lease(&tx, &self.key, &record, lease_id)?;
        if attempt.dispatched_at.is_none() {
            return Err(denied());
        }
        recheck_context(&tx, &self.key, scope, &attempt)?;
        let change = build_change(&attempt, output)?;
        let storage_limit = storage_capacity::before_commit(&tx, &self.key, &record, lease_id)?;
        let impact = if let Some(intent) = &attempt.correction {
            change.validate_owner_correction(&attempt.context, scope, &intent.target)?
        } else {
            change.validate_against(&attempt.context, scope)?
        };
        let committed_at = now()?;
        let mut versions = Vec::new();
        for op in &change.operations {
            let (unit, protected) = match op {
                ChangeOperation::AddUnit { unit } => {
                    if exists(&tx, "unit_head", &unit.header.object.object_id)? {
                        return Err(conflict());
                    }
                    (unit, false)
                }
                ChangeOperation::ReviseUnit { unit, .. } => (unit, true),
                _ => return Err(invalid("n2a_unit_only")),
            };
            insert(
                &tx,
                &self.key,
                "unit_revision",
                &revision_id(&unit.header.object),
                unit,
            )?;
            let head = Head {
                revision: unit.header.object.revision,
                protected,
                change_id: change.change_id.clone(),
            };
            if protected {
                update(
                    &tx,
                    &self.key,
                    "unit_head",
                    &unit.header.object.object_id,
                    &head,
                )?;
            } else {
                insert(
                    &tx,
                    &self.key,
                    "unit_head",
                    &unit.header.object.object_id,
                    &head,
                )?;
            }
            super::consolidation::register(&tx, &self.key, unit)?;
            versions.push(unit.header.object.clone());
        }
        let commit = CommittedCompilation {
            change: change.clone(),
            job_id: Some(job_id.into()),
            read_set: impact.read_set,
            output_versions: versions,
            configuration_revision: attempt.configuration.revision,
            committed_at,
            actor_id: attempt.correction.as_ref().map(|_| record.actor_id.clone()),
            correction: attempt.correction.clone(),
            undoes_change_id: None,
            semantic_support: "model_output_unverified".into(),
        };
        insert(
            &tx,
            &self.key,
            "compilation_commit",
            &change.change_id,
            &commit,
        )?;
        insert(
            &tx,
            &self.key,
            "model_response_hash",
            job_id,
            &self.key.authentication_tag(&[bytes]),
        )?;
        let mut ready = record.job.clone();
        ready.state = JobState::Ready;
        ready.committed_change_id = Some(change.change_id);
        ready.error_code = None;
        record.job.validate_successor(&ready, &attempt.context)?;
        record.job = ready;
        record.lease = None;
        event(&mut record, "compilation_committed")?;
        update(&tx, &self.key, "job", job_id, &record)?;
        storage_capacity::committed(&tx, &self.key, &record, lease_id, storage_limit)?;
        tx.commit()?;
        Ok(commit)
    }
    pub fn compiled_unit(&self, scope: &AssetScope, unit_id: &str) -> Result<CompiledUnit> {
        self.check(scope)?;
        read_unit(&self.connection, &self.key, scope, unit_id)
    }
    pub fn compiled_units(
        &self,
        scope: &AssetScope,
        before: Option<i64>,
    ) -> Result<Vec<(i64, CompiledUnit)>> {
        self.check(scope)?;
        let sql = if super::consolidation::available(&self.connection)? {
            "SELECT rowid,substr(id,?2) FROM intake_objects WHERE kind='unit_head' AND rowid<?1 AND NOT EXISTS(SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id=intake_objects.id) ORDER BY rowid DESC LIMIT 50"
        } else {
            "SELECT rowid,substr(id,?2) FROM intake_objects WHERE kind='unit_head' AND rowid<?1 ORDER BY rowid DESC LIMIT 50"
        };
        let mut q = self.connection.prepare(sql)?;
        let ids = q
            .query_map(
                rusqlite::params![
                    before.unwrap_or(i64::MAX),
                    physical_id("unit_head", "").len() + 1
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .filter_map(|(row, id)| {
                match super::consolidation::readable(
                    &self.connection,
                    &physical_id("unit_head", &id),
                ) {
                    Ok(false) => None,
                    Ok(true) => {
                        Some(read_unit(&self.connection, &self.key, scope, &id).map(|u| (row, u)))
                    }
                    Err(e) => Some(Err(e)),
                }
            })
            .collect()
    }
    pub fn unit_history(&self, scope: &AssetScope, unit_id: &str) -> Result<Vec<MemoryUnit>> {
        self.check(scope)?;
        let current = read_unit(&self.connection, &self.key, scope, unit_id)?;
        // Bound each unit to at most 100 retained revisions in this first executor.
        (1..=current.unit.header.object.revision)
            .map(|revision| {
                load(
                    &self.connection,
                    &self.key,
                    "unit_revision",
                    &revision_id(&RevisionRef {
                        object_id: unit_id.into(),
                        revision,
                    }),
                )
            })
            .collect()
    }
    pub fn compilation_commit(
        &self,
        scope: &AssetScope,
        change_id: &str,
    ) -> Result<CommittedCompilation> {
        self.check(scope)?;
        load(&self.connection, &self.key, "compilation_commit", change_id)
    }
    /// Immediate transitive invalidation; encrypted originals remain until explicit deletion.
    pub fn withdraw_source(&mut self, scope: &AssetScope, source_id: &str) -> Result<()> {
        self.retire_source(scope, source_id, false).map(|_| ())
    }
    /// Undo is a new owner-authored revision; the previous correction and its evidence remain intact.
    pub fn undo_correction(
        &mut self,
        authority: &CorrectionAuthority,
        unit_id: &str,
        expected_revision: u64,
    ) -> Result<CompiledUnit> {
        self.check(&authority.scope)?;
        if !authority.may_correct || !identifier(&authority.actor_id) {
            return Err(denied());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_unit(&tx, &self.key, &self.scope, unit_id)?;
        if current.unit.header.object.revision != expected_revision || expected_revision >= 100 {
            return Err(conflict());
        }
        let previous_commit: CommittedCompilation =
            load(&tx, &self.key, "compilation_commit", &current.change_id)?;
        let intent = previous_commit.correction.ok_or_else(conflict)?;
        if previous_commit.undoes_change_id.is_some() {
            return Err(conflict());
        }
        let mut restored: MemoryUnit = load(
            &tx,
            &self.key,
            "unit_revision",
            &revision_id(&intent.target),
        )?;
        for e in &restored.evidence {
            load_source(&tx, &self.key, &self.scope, &e.source)?;
        }
        let previous = current.unit.header.object.clone();
        restored.header.object.revision = expected_revision + 1;
        restored.header.recorded_at = now()?;
        let change_id = id("undo");
        let change = ChangeSet {
            schema_version: COMPILATION_VERSION.into(),
            scope: self.scope.clone(),
            change_id: change_id.clone(),
            compiler: CompilerVersion {
                model_id: "owner-undo".into(),
                extractor_version: EXTRACTOR_VERSION.into(),
                rules_version: RULES_VERSION.into(),
            },
            operations: vec![ChangeOperation::ReviseUnit {
                base_revision: expected_revision,
                intent: RevisionIntent::Correct,
                unit: restored.clone(),
            }],
        };
        let mut context = snapshot(load_source(
            &tx,
            &self.key,
            &self.scope,
            &restored.evidence[0].source,
        )?);
        for ev in current.unit.evidence.iter().chain(restored.evidence.iter()) {
            if !context.sources.iter().any(|s| s.source == ev.source) {
                context
                    .sources
                    .push(load_source(&tx, &self.key, &self.scope, &ev.source)?);
            }
        }
        context.units.push(current.unit.clone());
        let impact = change.validate_owner_correction(&context, &self.scope, &previous)?;
        insert(
            &tx,
            &self.key,
            "unit_revision",
            &revision_id(&restored.header.object),
            &restored,
        )?;
        update(
            &tx,
            &self.key,
            "unit_head",
            unit_id,
            &Head {
                revision: expected_revision + 1,
                protected: true,
                change_id: change_id.clone(),
            },
        )?;
        let commit = CommittedCompilation {
            change,
            job_id: None,
            read_set: impact
                .read_set
                .into_iter()
                .chain([previous, intent.target.clone()])
                .collect(),
            output_versions: vec![restored.header.object.clone()],
            configuration_revision: 0,
            committed_at: now()?,
            actor_id: Some(authority.actor_id.clone()),
            correction: Some(intent),
            undoes_change_id: Some(current.change_id),
            semantic_support: "owner_undo_not_model_evaluation".into(),
        };
        insert(&tx, &self.key, "compilation_commit", &change_id, &commit)?;
        super::consolidation::register(&tx, &self.key, &restored)?;
        tx.commit()?;
        Ok(CompiledUnit {
            unit: restored,
            protected: true,
            change_id,
        })
    }
}

pub(super) fn check_correction_target(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    intent: &CorrectionIntent,
) -> Result<()> {
    let selected = read_unit(c, key, scope, &intent.target.object_id)?;
    if selected.unit.header.object != intent.target
        || selected.unit.header.state != UnitState::Active
        || intent.target.revision >= 100
    {
        return Err(conflict());
    }
    Ok(())
}
fn enabled(c: &Connection, key: &VaultKey) -> Result<bool> {
    let f: Option<Feature> = optional(c, key, "compiler_feature", "compiler_feature")?;
    match f {
        None => Ok(false),
        Some(f) if f.version == "n2a.v0.1" => Ok(true),
        _ => Err(invalid("compiler_feature_version")),
    }
}
fn require_enabled(c: &Connection, key: &VaultKey) -> Result<()> {
    if enabled(c, key)? {
        Ok(())
    } else {
        Err(invalid("compiler_not_enabled"))
    }
}
pub(super) fn exists(c: &Connection, kind: &str, id: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM intake_objects WHERE kind=?1 AND id=?2",
        [kind, &physical_id(kind, id)],
        |r| r.get::<_, i64>(0),
    )
    .optional()?
    .is_some())
}
pub(super) fn optional<T: serde::de::DeserializeOwned>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
) -> Result<Option<T>> {
    if exists(c, kind, id)? {
        Ok(Some(load(c, key, kind, id)?))
    } else {
        Ok(None)
    }
}
fn attempt_id(record: &IntakeJob) -> String {
    format!("{}:attempt:{}", record.job.job_id, record.job.attempt)
}
pub(super) fn revision_id(reference: &RevisionRef) -> String {
    format!("{}:revision:{}", reference.object_id, reference.revision)
}
fn check_lease(
    c: &Connection,
    key: &VaultKey,
    record: &IntakeJob,
    lease_id: &str,
) -> Result<ModelConfiguration> {
    let config: ModelConfiguration = load(c, key, "configuration", "configuration")?;
    config.validate()?;
    if record.job.state != JobState::Processing
        || record.lease.as_ref().is_none_or(|l| {
            l.lease_id != lease_id
                || l.configuration_revision != config.revision
                || parse_time(&l.expires_at).map_or(true, |at| at <= OffsetDateTime::now_utc())
        })
        || config.selection == ModelSelection::Unconfigured
    {
        return Err(conflict());
    }
    Ok(config)
}
pub(super) fn load_source(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    reference: &RevisionRef,
) -> Result<SourceSnapshot> {
    if exists(c, "source_withdrawal", &reference.object_id)? {
        return Err(denied());
    }
    let source: SourceSnapshot = load(c, key, "source", &reference.object_id)?;
    if source.scope != *scope || source.source != *reference {
        return Err(denied());
    }
    Ok(source)
}
pub(super) fn read_unit(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    unit_id: &str,
) -> Result<CompiledUnit> {
    let head: Head = load(c, key, "unit_head", unit_id)?;
    let unit: MemoryUnit = load(
        c,
        key,
        "unit_revision",
        &revision_id(&RevisionRef {
            object_id: unit_id.into(),
            revision: head.revision,
        }),
    )?;
    if unit.header.scope != *scope {
        return Err(denied());
    }
    for e in &unit.evidence {
        load_source(c, key, scope, &e.source)?;
    }
    Ok(CompiledUnit {
        unit,
        protected: head.protected,
        change_id: head.change_id,
    })
}
fn recheck_context(
    c: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    attempt: &CompilationAttempt,
) -> Result<()> {
    for source in &attempt.context.sources {
        let current = load_source(c, key, scope, &source.source)?;
        if current != *source {
            return Err(conflict());
        }
    }
    for unit in &attempt.context.units {
        let current = read_unit(c, key, scope, &unit.header.object.object_id)?;
        if current.unit != *unit {
            return Err(conflict());
        }
    }
    if let Some(intent) = &attempt.correction {
        check_correction_target(c, key, scope, intent)?;
    }
    Ok(())
}
fn locate(source: &SourceSnapshot, anchor: &QuoteAnchor) -> Result<EvidenceSpan> {
    if anchor.quote.trim().is_empty() || anchor.quote.len() > 4096 || anchor.occurrence > 64 {
        return Err(invalid("quote_bounds"));
    }
    let (byte, _) = source
        .content
        .match_indices(&anchor.quote)
        .nth(anchor.occurrence)
        .ok_or_else(|| invalid("quote_not_found"))?;
    let start_char = source.content[..byte].chars().count();
    Ok(EvidenceSpan {
        source: source.source.clone(),
        start_char,
        end_char: start_char + anchor.quote.chars().count(),
        quote: anchor.quote.clone(),
    })
}
fn build_change(attempt: &CompilationAttempt, output: ExtractionOutput) -> Result<ChangeSet> {
    let source = &attempt.context.sources[0];
    let at = now()?;
    if attempt.correction.is_some() && output.units.len() != 1 {
        return Err(invalid("correction_requires_one_unit"));
    }
    let mut operations = Vec::new();
    let mut seen = BTreeSet::new();
    for extracted in output.units {
        if extracted.quotes.is_empty()
            || extracted.quotes.len() > 16
            || extracted.origin == SemanticOrigin::UserStatement
        {
            return Err(invalid("automatic_origin_or_evidence"));
        }
        let evidence = extracted
            .quotes
            .iter()
            .map(|q| locate(source, q))
            .collect::<Result<Vec<_>>>()?;
        let fingerprint = serde_json::to_string(&(&extracted.content, &evidence))?;
        if !seen.insert(fingerprint) {
            return Err(invalid("duplicate_output_unit"));
        }
        let mut unit = MemoryUnit {
            header: AssetHeader {
                object: RevisionRef {
                    object_id: id("unit"),
                    revision: 1,
                },
                scope: source.scope.clone(),
                recorded_at: at.clone(),
                privacy_level: source.privacy_level,
                state: UnitState::Active,
            },
            origin: extracted.origin,
            content: extracted.content,
            occurred_at: extracted.occurred_at,
            valid_time: extracted.valid_time,
            evidence,
            derived_from: vec![],
            modules: vec![],
            extensions: [(
                "nova:support_status".into(),
                json!("model_output_unverified"),
            )]
            .into(),
        };
        if let Some(intent) = &attempt.correction {
            unit.header.object = RevisionRef {
                object_id: intent.target.object_id.clone(),
                revision: intent.target.revision + 1,
            };
            if !matches!(
                unit.origin,
                SemanticOrigin::Extracted | SemanticOrigin::Inferred
            ) {
                return Err(invalid("owner_instruction_origin_required"));
            }
            unit.header.privacy_level = attempt
                .context
                .sources
                .iter()
                .map(|s| s.privacy_level)
                .max_by_key(|p| privacy_rank(*p))
                .unwrap_or(source.privacy_level);
            operations.push(ChangeOperation::ReviseUnit {
                base_revision: intent.target.revision,
                intent: RevisionIntent::Correct,
                unit,
            });
        } else {
            operations.push(ChangeOperation::AddUnit { unit });
        }
    }
    Ok(ChangeSet {
        schema_version: COMPILATION_VERSION.into(),
        scope: source.scope.clone(),
        change_id: id("change"),
        compiler: attempt.compiler.clone(),
        operations,
    })
}
fn privacy_rank(p: PrivacyLevel) -> u8 {
    match p {
        PrivacyLevel::Shareable => 0,
        PrivacyLevel::Private => 1,
        PrivacyLevel::Sensitive => 2,
        PrivacyLevel::Sealed => 3,
    }
}

impl CompilationAttempt {
    /// Only selected text and an explicitly selected correction target go to the model.
    pub fn model_input(&self) -> Value {
        json!({"schema_version":EXTRACTION_VERSION,"source_text":self.context.sources[0].content,"source_role":self.context.sources[0].role,"occurred_at":self.context.sources[0].occurred_at,"correction_target":self.context.units.first().map(|u|&u.content)})
    }
}
