use super::*;
use crate::crypto::{DataKey, EncryptedBlob};
use crate::{NovaError, VaultKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::path::Path;
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub struct IntakeStore {
    pub(super) connection: Connection,
    pub(super) key: VaultKey,
    pub(super) scope: AssetScope,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: String,
    scope: AssetScope,
}

impl IntakeStore {
    /// Explicit opt-in. Fails if any file exists; never creates tables in a legacy Vault.
    pub fn create(path: &Path, key: VaultKey, scope: AssetScope) -> Result<Self> {
        check_scope(&scope)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        file.sync_all()?;
        let connection = Connection::open(path)?;
        configure(&connection)?;
        let mut store = Self {
            connection,
            key,
            scope,
        };
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE intake_objects (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, key_nonce BLOB NOT NULL,
            wrapped_key BLOB NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL
        ); CREATE TABLE intake_requests (
            binding TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
            source_id TEXT NOT NULL REFERENCES intake_objects(id),
            job_id TEXT NOT NULL UNIQUE REFERENCES intake_objects(id)
        );",
        )?;
        insert(
            &tx,
            &store.key,
            "identity",
            "identity",
            &Identity {
                version: STORE_VERSION.into(),
                scope: store.scope.clone(),
            },
        )?;
        insert(
            &tx,
            &store.key,
            "configuration",
            "configuration",
            &ModelConfiguration {
                revision: 1,
                selection: ModelSelection::Unconfigured,
            },
        )?;
        tx.commit()?;
        Ok(store)
    }
    /// Existing format only. Wrong keys/scope/legacy stores fail before any write or PRAGMA change.
    pub fn open(path: &Path, key: VaultKey, scope: AssetScope) -> Result<Self> {
        Self::open_existing(path,key,scope,false)
    }
    /// Inspect identity/configuration with SQLite read-only flags; no migrations or PRAGMA writes.
    pub fn open_read_only(path: &Path, key: VaultKey, scope: AssetScope) -> Result<Self> {
        Self::open_existing(path,key,scope,true)
    }
    fn open_existing(path:&Path,key:VaultKey,scope:AssetScope,read_only:bool)->Result<Self>{
        check_scope(&scope)?;
        let connection =
            Connection::open_with_flags(path, if read_only {rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY}else{rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE})?;
        let identity: Identity = load(&connection, &key, "identity", "identity")
            .map_err(|_| NovaError::InvalidVaultKey)?;
        if identity.version != STORE_VERSION {
            return Err(invalid("unsupported_store_version"));
        }
        if identity.scope != scope {
            return Err(denied());
        }
        if !read_only {configure(&connection)?;}
        let store = Self {
            connection,
            key,
            scope,
        };
        store.configuration()?;
        Ok(store)
    }
    pub fn configuration(&self) -> Result<ModelConfiguration> {
        let config: ModelConfiguration = load(
            &self.connection,
            &self.key,
            "configuration",
            "configuration",
        )?;
        config.validate()?;
        Ok(config)
    }
    pub fn configure_model(
        &mut self,
        expected_revision: u64,
        selection: ModelSelection,
    ) -> Result<ModelConfiguration> {
        let next = ModelConfiguration {
            revision: expected_revision
                .checked_add(1)
                .ok_or_else(|| invalid("configuration_revision"))?,
            selection,
        };
        next.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: ModelConfiguration = load(&tx, &self.key, "configuration", "configuration")?;
        if current.revision != expected_revision {
            return Err(conflict());
        }
        update(&tx, &self.key, "configuration", "configuration", &next)?;
        tx.commit()?;
        Ok(next)
    }
    /// All untrusted capture bytes pass through the N0 strict parser.
    pub fn receive(&mut self, authority: &IntakeAuthority, bytes: &[u8]) -> Result<IntakeReceipt> {
        self.authorize(authority)?;
        let request = IntakeRequest::parse(bytes)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::recovery::require_automation(&tx, &self.key)?;
        let receipt = receive_tx(&tx, &self.key, &self.scope, authority, &request, None)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn source(&self, scope: &AssetScope, source_id: &str) -> Result<SourceSnapshot> {
        self.check(scope)?;
        let source: SourceSnapshot = load(&self.connection, &self.key, "source", source_id)?;
        snapshot(source.clone()).validate(scope)?;
        Ok(source)
    }
    pub fn job(&self, scope: &AssetScope, job_id: &str) -> Result<IntakeJob> {
        self.check(scope)?;
        let job: IntakeJob = load(&self.connection, &self.key, "job", job_id)?;
        let source = self.source(scope, &job.job.source.object_id)?;
        job.job.validate(&snapshot(source))?;
        Ok(job)
    }
    /// Clients may inspect only their own receipts; no source/disclosure permission is implied.
    pub fn actor_job(&self, authority: &IntakeAuthority, job_id: &str) -> Result<IntakeJob> {
        self.authorize(authority)?;
        let job = self.job(&authority.scope, job_id)?;
        if job.actor_id != authority.actor_id {
            return Err(denied());
        }
        Ok(job)
    }
    /// Bounded latest-first page; original text is fetched separately, not duplicated in the queue.
    pub fn jobs(&self, scope: &AssetScope, before: Option<i64>) -> Result<Vec<(i64, IntakeJob)>> {
        self.check(scope)?;
        let sql = if super::consolidation::available(&self.connection)? {
            "SELECT rowid,job_id FROM intake_requests WHERE rowid<?1 AND NOT EXISTS(SELECT 1 FROM intake_dependencies d JOIN intake_revocations r ON d.dependency=r.dependency WHERE d.record_id=intake_requests.job_id) ORDER BY rowid DESC LIMIT 50"
        } else {
            "SELECT rowid,job_id FROM intake_requests WHERE rowid<?1 ORDER BY rowid DESC LIMIT 50"
        };
        let mut query = self.connection.prepare(sql)?;
        let ids = query
            .query_map([before.unwrap_or(i64::MAX)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .filter_map(
                |(seq, id)| match super::consolidation::readable(&self.connection, &id) {
                    Ok(false) => None,
                    Ok(true) => Some(self.job(scope, &id).map(|j| (seq, j))),
                    Err(e) => Some(Err(e)),
                },
            )
            .collect()
    }
    pub fn retry(&mut self, scope: &AssetScope, job_id: &str) -> Result<IntakeJob> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        if record.job.state != JobState::Failed || record.job.attempt >= MAX_ATTEMPTS {
            return Err(conflict());
        }
        if !record.retry_requested {
            record.retry_requested = true;
            event(&mut record, "retry_requested")?;
            update(&tx, &self.key, "job", job_id, &record)?;
        }
        tx.commit()?;
        Ok(record)
    }
    /// Compatibility whole-store worker step; not a bounded scheduler primitive.
    /// New periodic workers must retain the cursor returned by claim_next_batch.
    /// Returns a fencing lease and never executes a model itself.
    pub fn claim_next(
        &mut self,
        scope: &AssetScope,
        lease_seconds: u32,
    ) -> Result<Option<IntakeJob>> {
        self.check(scope)?;
        super::recovery::require_automation(&self.connection, &self.key)?;
        if !(1..=900).contains(&lease_seconds) {
            return Err(invalid("lease_limit"));
        }
        self.recover_expired(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let config: ModelConfiguration = load(&tx, &self.key, "configuration", "configuration")?;
        super::recovery::require_automation(&tx, &self.key)?;
        config.validate()?;
        if config.selection == ModelSelection::Unconfigured {
            return Ok(None);
        }
        let ids = job_ids(&tx)?;
        for job_id in ids {
            let mut record: IntakeJob = load(&tx, &self.key, "job", &job_id)?;
            if !record.retry_requested
                || record.job.attempt >= MAX_ATTEMPTS
                || !matches!(record.job.state, JobState::Received | JobState::Failed)
            {
                continue;
            }
            let source: SourceSnapshot =
                load(&tx, &self.key, "source", &record.job.source.object_id)?;
            let mut next = record.job.clone();
            next.state = JobState::Processing;
            next.attempt += 1;
            next.error_code = None;
            record.job.validate_successor(&next, &snapshot(source))?;
            record.job = next;
            record.retry_requested = false;
            record.lease = Some(WorkerLease {
                lease_id: id("lease"),
                expires_at: format_time(
                    OffsetDateTime::now_utc() + Duration::seconds(i64::from(lease_seconds)),
                )?,
                configuration_revision: config.revision,
            });
            event(&mut record, "worker_claimed")?;
            update(&tx, &self.key, "job", &job_id, &record)?;
            tx.commit()?;
            return Ok(Some(record));
        }
        Ok(None)
    }
    pub fn fail_attempt(
        &mut self,
        scope: &AssetScope,
        job_id: &str,
        lease_id: &str,
        failure: AttemptFailure,
    ) -> Result<IntakeJob> {
        self.check(scope)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record: IntakeJob = load(&tx, &self.key, "job", job_id)?;
        if record.job.state != JobState::Processing
            || record.lease.as_ref().is_none_or(|l| {
                l.lease_id != lease_id
                    || parse_time(&l.expires_at).map_or(true, |at| at <= OffsetDateTime::now_utc())
            })
        {
            return Err(conflict());
        }
        fail(&tx, &self.key, &mut record, failure.code())?;
        tx.commit()?;
        Ok(record)
    }
    /// Compatibility whole-store recovery. Periodic cleanup uses recover_expired_batch.
    /// Expired attempts become failed and remain visible for explicit retry.
    pub fn recover_expired(&mut self, scope: &AssetScope) -> Result<usize> {
        self.check(scope)?;
        if self.backup_recovery_manifest(scope)?.is_some() { return Ok(0); }
        self.recover_at(OffsetDateTime::now_utc())
    }
    fn recover_at(&mut self, at: OffsetDateTime) -> Result<usize> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut recovered = 0;
        for job_id in job_ids(&tx)? {
            let mut record: IntakeJob = load(&tx, &self.key, "job", &job_id)?;
            if record.job.state == JobState::Processing
                && record
                    .lease
                    .as_ref()
                    .is_some_and(|l| parse_time(&l.expires_at).is_ok_and(|expiry| expiry <= at))
            {
                fail(&tx, &self.key, &mut record, "worker_interrupted")?;
                recovered += 1;
            }
        }
        tx.commit()?;
        Ok(recovered)
    }
    /// Consistent encrypted SQLite snapshot, including configuration, jobs and idempotency.
    /// Requires the same separately held VaultKey. It is NOT a legacy Portable package.
    pub fn backup_to(&self, scope: &AssetScope, path: &Path) -> Result<()> {
        self.check(scope)?;
        if self.backup_recovery_manifest(scope)?.is_some() {
            // Repackaging must not bless a modified or damaged snapshot with a new fingerprint.
            self.verify_backup_copy(scope)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let result = (|| {
        self.connection.execute(
            "VACUUM INTO ?1",
            [path.to_str().ok_or_else(|| invalid("backup_path"))?],
        )?;
        super::connections::strip_backup_connections(path, &self.key)?;
        super::capture::strip_backup_capture(path, &self.key)?;
        let manifest=super::recovery::quarantine_backup(path, &self.key, scope)?;
        file.sync_all()?;
        super::recovery::record_generation(&self.connection, &self.key, &manifest)?;
        Ok(())
        })();
        if result.is_err() { let _ = std::fs::remove_file(path); }
        result
    }
    pub(super) fn check(&self, scope: &AssetScope) -> Result<()> {
        if scope == &self.scope {
            Ok(())
        } else {
            Err(denied())
        }
    }
    pub(super) fn authorize(&self, authority: &IntakeAuthority) -> Result<()> {
        self.check(&authority.scope)?;
        if !authority.capture_allowed || !identifier(&authority.actor_id) {
            return Err(denied());
        }
        Ok(())
    }
}

pub(super) fn configure(c: &Connection) -> Result<()> {
    c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;")?;
    Ok(())
}
pub(super) fn check_scope(scope: &AssetScope) -> Result<()> {
    if !identifier(&scope.owner_id) || !identifier(&scope.space) {
        return Err(invalid("invalid_scope"));
    }
    Ok(())
}
pub(super) fn denied() -> NovaError {
    NovaError::PolicyDenied("intake authority denied".into())
}
pub(super) fn conflict() -> NovaError {
    NovaError::InvalidState("intake revision or lease conflict".into())
}
pub(super) fn id(kind: &str) -> String {
    format!("{kind}_{}", Uuid::now_v7())
}
pub(super) fn format_time(at: OffsetDateTime) -> Result<String> {
    at.format(&Rfc3339).map_err(|_| invalid("invalid_clock"))
}
pub(super) fn parse_time(s: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| invalid("invalid_clock"))
}
pub(super) fn now() -> Result<String> {
    format_time(OffsetDateTime::now_utc())
}
pub(super) fn snapshot(source: SourceSnapshot) -> CompilationSnapshot {
    CompilationSnapshot {
        schema_version: COMPILATION_VERSION.into(),
        scope: source.scope.clone(),
        sources: vec![source],
        units: vec![],
        relations: vec![],
        modules: vec![],
        tombstones: vec![],
        protected_unit_ids: Default::default(),
    }
}
pub(super) fn event(record: &mut IntakeJob, reason: &str) -> Result<()> {
    record.updated_at = now()?;
    record.history.push(JobEvent {
        sequence: record.history.len() as u32 + 1,
        at: record.updated_at.clone(),
        state: record.job.state,
        attempt: record.job.attempt,
        reason: reason.into(),
    });
    Ok(())
}
pub(super) fn fail(
    tx: &Connection,
    key: &VaultKey,
    record: &mut IntakeJob,
    reason: &str,
) -> Result<()> {
    let source: SourceSnapshot = load(tx, key, "source", &record.job.source.object_id)?;
    let mut next = record.job.clone();
    next.state = JobState::Failed;
    next.error_code = Some(reason.into());
    record.job.validate_successor(&next, &snapshot(source))?;
    record.job = next;
    record.lease = None;
    record.retry_requested = false;
    event(record, reason)?;
    update(tx, key, "job", &record.job.job_id, record)?;
    super::compiler::storage_capacity::failed(tx, key, record)
}
pub(super) fn job_ids(c: &Connection) -> Result<Vec<String>> {
    let mut q = c.prepare("SELECT job_id FROM intake_requests ORDER BY rowid")?;
    let ids = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ids.into_iter()
        .filter_map(|id| match super::consolidation::readable(c, &id) {
            Ok(true) => Some(Ok(id)),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        })
        .collect()
}
pub(super) fn aad(kind: &str, id: &str) -> Vec<u8> {
    format!("{STORE_VERSION}:{kind}:{id}").into_bytes()
}
pub(super) fn insert<T: Serialize>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    super::consolidation::track(c, key, kind, id, value)?;
    let dek = DataKey::generate()?;
    let context = aad(kind, id);
    let wrapped = key.encrypt(dek.as_bytes(), &context)?;
    let encrypted = dek.encrypt(&serde_json::to_vec(value)?, &context)?;
    c.execute("INSERT INTO intake_objects(id,kind,key_nonce,wrapped_key,nonce,ciphertext) VALUES (?1,?2,?3,?4,?5,?6)", params![physical_id(kind,id),kind,wrapped.nonce,wrapped.ciphertext,encrypted.nonce,encrypted.ciphertext])?;
    Ok(())
}
pub(super) fn data(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
) -> Result<(DataKey, EncryptedBlob)> {
    let (kn,kc,n,cipher) = c.query_row("SELECT key_nonce,wrapped_key,nonce,ciphertext FROM intake_objects WHERE id=?1 AND kind=?2", params![physical_id(kind,id),kind], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or_else(|| NovaError::NotFound { kind: "intake_object", id: "[redacted]".into() })?;
    let dek = DataKey::from_bytes(key.decrypt(
        &EncryptedBlob {
            nonce: kn,
            ciphertext: kc,
        },
        &aad(kind, id),
    )?)?;
    Ok((
        dek,
        EncryptedBlob {
            nonce: n,
            ciphertext: cipher,
        },
    ))
}
pub(super) fn load<T: DeserializeOwned>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
) -> Result<T> {
    if !super::consolidation::readable(c, &physical_id(kind, id))? {
        return Err(denied());
    }
    let (dek, blob) = data(c, key, kind, id)?;
    Ok(serde_json::from_slice(
        &dek.decrypt(&blob, &aad(kind, id))?,
    )?)
}
pub(super) fn update<T: Serialize>(
    c: &Connection,
    key: &VaultKey,
    kind: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    super::consolidation::track(c, key, kind, id, value)?;
    let (dek, _) = data(c, key, kind, id)?;
    let blob = dek.encrypt(&serde_json::to_vec(value)?, &aad(kind, id))?;
    if c.execute(
        "UPDATE intake_objects SET nonce=?1,ciphertext=?2 WHERE id=?3 AND kind=?4",
        params![blob.nonce, blob.ciphertext, physical_id(kind, id), kind],
    )? != 1
    {
        return Err(conflict());
    }
    Ok(())
}

pub(super) fn receive_tx(
    tx: &Connection,
    key: &VaultKey,
    scope: &AssetScope,
    authority: &IntakeAuthority,
    request: &IntakeRequest,
    correction: Option<&super::compiler::CorrectionIntent>,
) -> Result<IntakeReceipt> {
    let binding = key.authentication_tag(&[
        b"intake:request",
        authority.scope.owner_id.as_bytes(),
        authority.scope.space.as_bytes(),
        authority.actor_id.as_bytes(),
        request.request_id.as_bytes(),
    ]);
    if super::consolidation::available(tx)?
        && tx
            .query_row(
                "SELECT 1 FROM intake_retired_requests WHERE binding=?1",
                [&binding],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
    {
        return Err(denied());
    }
    let fingerprint = key.authentication_tag(&[
        b"intake:input",
        &serde_json::to_vec(&request)?,
        &if let Some(c) = correction {
            serde_json::to_vec(&(authority.role, authority.privacy_level, c))?
        } else {
            serde_json::to_vec(&(authority.role, authority.privacy_level))?
        },
    ]);
    if let Some((previous, job_id)) = tx
        .query_row(
            "SELECT fingerprint, job_id FROM intake_requests WHERE binding=?1",
            [&binding],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if previous != fingerprint {
            return Err(NovaError::IdempotencyConflict {
                request_id: "[redacted]".into(),
            });
        }
        let job: IntakeJob = load(tx, key, "job", &job_id)?;
        // Only the original immutable receipt is replayed, regardless of processing state.
        return Ok(job.receipt);
    }
    if let Some(correction) = correction {
        super::compiler::check_correction_target(tx, key, scope, correction)?;
    }
    let at = now()?;
    let source = SourceSnapshot {
        source: RevisionRef {
            object_id: id("source"),
            revision: 1,
        },
        scope: scope.clone(),
        recorded_at: at.clone(),
        source_platform: request.source_platform.clone(),
        occurred_at: request.occurred_at.clone(),
        role: authority.role,
        capture_fidelity: request.capture_fidelity,
        privacy_level: authority.privacy_level,
        content: request.content.clone(),
    };
    let job = CompilationJob {
        job_id: id("job"),
        scope: scope.clone(),
        source: source.source.clone(),
        state: JobState::Received,
        attempt: 0,
        error_code: None,
        committed_change_id: None,
    };
    let receipt = IntakeReceipt {
        schema_version: INTAKE_VERSION.into(),
        request_id: request.request_id.clone(),
        source: source.source.clone(),
        job_id: job.job_id.clone(),
        saved_at: at.clone(),
        content_sha256: hex::encode(Sha256::digest(source.content.as_bytes())),
        status: IntakeAcceptance::Received,
    };
    receipt.validate(request, &job, &snapshot(source.clone()))?;
    let stored = IntakeJob {
        job,
        receipt: receipt.clone(),
        actor_id: authority.actor_id.clone(),
        updated_at: at.clone(),
        lease: None,
        retry_requested: true,
        history: vec![JobEvent {
            sequence: 1,
            at,
            state: JobState::Received,
            attempt: 0,
            reason: "source_persisted".into(),
        }],
    };
    insert(tx, key, "source", &source.source.object_id, &source)?;
    insert(tx, key, "job", &stored.job.job_id, &stored)?;
    tx.execute(
        "INSERT INTO intake_requests(binding,fingerprint,source_id,job_id) VALUES (?1,?2,?3,?4)",
        params![
            binding,
            fingerprint,
            source.source.object_id,
            stored.job.job_id
        ],
    )?;
    if let Some(correction) = correction {
        insert(tx, key, "correction_intent", &stored.job.job_id, correction)?;
    }
    Ok(receipt)
}

pub(super) fn physical_id(kind: &str, id: &str) -> String {
    match kind {
        "identity" | "configuration" | "source" | "job" => id.to_string(),
        _ => format!("n2a:{kind}:{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_worker_recovers_once_and_is_fenced() {
        let d = tempfile::tempdir().unwrap();
        let scope = AssetScope {
            owner_id: "owner:test".into(),
            space: "test".into(),
        };
        let mut store = IntakeStore::create(
            &d.path().join("test.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        let authority = IntakeAuthority {
            scope: scope.clone(),
            actor_id: "test".into(),
            capture_allowed: true,
            role: SourceRole::User,
            privacy_level: PrivacyLevel::Private,
        };
        let bytes = br#"{"schema_version":"nova.intake.v0.1","request_id":"test","source_platform":"test","capture_fidelity":"f2_visible_transcript","content":"synthetic","occurred_at":null}"#;
        let receipt = store.receive(&authority, bytes).unwrap();
        assert!(store.claim_next(&scope, 1).unwrap().is_none());
        store
            .configure_model(
                1,
                ModelSelection::Local {
                    model_id: "synthetic".into(),
                    endpoint: "http://127.0.0.1:11434/v1".into(),
                },
            )
            .unwrap();
        let claimed = store.claim_next(&scope, 1).unwrap().unwrap();
        let lease = claimed.lease.unwrap();
        assert!(store.claim_next(&scope, 1).unwrap().is_none());
        let future = OffsetDateTime::now_utc() + Duration::seconds(2);
        assert_eq!(store.recover_at(future).unwrap(), 1);
        assert_eq!(store.recover_at(future).unwrap(), 0);
        assert!(
            store
                .fail_attempt(
                    &scope,
                    &receipt.job_id,
                    &lease.lease_id,
                    AttemptFailure::InvalidOutput
                )
                .is_err()
        );
        store.retry(&scope, &receipt.job_id).unwrap();
        let next = store.claim_next(&scope, 60).unwrap().unwrap();
        assert_eq!(next.job.attempt, 2);
        assert_ne!(next.lease.unwrap().lease_id, lease.lease_id);
        assert!(
            store
                .fail_attempt(
                    &scope,
                    &receipt.job_id,
                    &lease.lease_id,
                    AttemptFailure::InvalidOutput
                )
                .is_err()
        );
    }
}
