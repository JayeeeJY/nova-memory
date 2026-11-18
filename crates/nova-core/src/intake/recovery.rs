//! New backup copies are quarantined until a separately trusted lifecycle reconciliation exists.
//! Envelope integrity and SQLite consistency cannot prove the absence of later deletions.
use super::{storage::*, *};
use crate::{NovaError, VaultKey};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

const MANIFEST_KIND: &str = "backup_recovery";
const MANIFEST_ID: &str = "current";
const MAX_RECORDS: usize = 100_000;
const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRecoveryManifest {
    pub schema_version: String,
    pub backup_id: String,
    pub created_at: String,
    pub origin_backup_created_at: String,
    pub store_version: String,
    pub scope: AssetScope,
    pub envelope_fingerprint: String,
    pub encrypted_bytes: usize,
    pub envelope_counts: BTreeMap<String, usize>,
    pub control_fingerprint: String,
    pub control_bytes: usize,
    pub control_rows: BTreeMap<String, usize>,
    pub external_authority_removed: bool,
    pub quarantined: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupVerification {
    pub manifest: BackupRecoveryManifest,
    pub sqlite_consistent: bool,
    pub envelopes_authenticated: bool,
    pub control_state_matches: bool,
    pub authority_freshness: String,
    pub off_device_restore_verified: bool,
    pub may_automate: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupGenerationReceipt {
    pub backup_id: String,
    pub generated_at: String,
    pub encrypted_bytes: usize,
    pub external_copy_verified: bool,
    pub off_device_restore_verified: bool,
}
fn optional_manifest(c: &Connection, key: &VaultKey) -> Result<Option<BackupRecoveryManifest>> {
    let present = c
        .query_row(
            "SELECT 1 FROM intake_objects WHERE id=?1",
            [physical_id(MANIFEST_KIND, MANIFEST_ID)],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !present {
        return Ok(None);
    }
    let m: BackupRecoveryManifest = load(c, key, MANIFEST_KIND, MANIFEST_ID)?;
    if m.schema_version != "nova.backup-recovery.v0.1"
        || m.store_version != STORE_VERSION
        || !m.quarantined
        || !m.external_authority_removed
    {
        return Err(invalid("invalid_backup_recovery_marker"));
    }
    parse_time(&m.created_at)?;
    parse_time(&m.origin_backup_created_at)?;
    Ok(Some(m))
}
pub(super) fn require_automation(c: &Connection, key: &VaultKey) -> Result<()> {
    if optional_manifest(c, key)?.is_some() {
        return Err(NovaError::PolicyDenied(
            "backup requires trusted lifecycle reconciliation".into(),
        ));
    }
    Ok(())
}
fn inventory(c: &Connection, key: &VaultKey) -> Result<(String, usize, BTreeMap<String, usize>)> {
    if c.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))? != "ok" {
        return Err(invalid("backup_sqlite_integrity"));
    }
    let malformed: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM intake_objects WHERE length(id)>512 OR length(kind)>64 OR length(key_nonce)>64 OR length(wrapped_key)>1024 OR length(nonce)>64)", [], |r|r.get(0))?;
    if malformed {
        return Err(invalid("backup_metadata_budget"));
    }
    let mut query = c.prepare(
        "SELECT id,kind,length(ciphertext) FROM intake_objects WHERE id<>?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = query
        .query_map(
            rusqlite::params![physical_id(MANIFEST_KIND, MANIFEST_ID), MAX_RECORDS + 1],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, usize>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_RECORDS {
        return Err(invalid("backup_verification_row_budget"));
    }
    let mut total = 0usize;
    for (_, _, size) in &rows {
        total = total
            .checked_add(*size)
            .ok_or_else(|| invalid("backup_verification_byte_budget"))?;
        if total > MAX_BYTES {
            return Err(invalid("backup_verification_byte_budget"));
        }
    }
    let mut hash = Sha256::new();
    let mut counts = BTreeMap::new();
    for (physical, kind, _) in rows {
        let prefix = physical_id(&kind, "");
        let id = physical
            .strip_prefix(&prefix)
            .ok_or_else(|| invalid("backup_record_kind"))?;
        // Authenticate even revoked ciphertext, but never return its contents. Lifecycle access
        // is checked separately; this verification cannot promote a record into current truth.
        let (dek, blob) = data(c, key, &kind, id)?;
        let plaintext = zeroize::Zeroizing::new(dek.decrypt(&blob, &aad(&kind, id))?);
        let _: serde_json::Value = serde_json::from_slice(&plaintext)?;
        for bytes in [
            physical.as_bytes(),
            kind.as_bytes(),
            blob.nonce.as_slice(),
            blob.ciphertext.as_slice(),
        ] {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        *counts.entry(kind).or_insert(0) += 1;
    }
    let digest = hash.finalize();
    Ok((
        key.authentication_tag(&[b"backup-envelopes:v1", digest.as_slice()]),
        total,
        counts,
    ))
}
/// Bind the plain indices and lifecycle fences as well as the encrypted payloads. The
/// inventory is format-specific: unknown objects and budgets require an explicit upgrade.
fn controls(c: &Connection, key: &VaultKey) -> Result<(String, usize, BTreeMap<String, usize>)> {
    const LIMIT: usize = 16 * 1024 * 1024;
    let mut statement=c.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL ORDER BY type,name LIMIT 65")?;
    let mut rows = statement.query([])?;
    let mut schema = vec![];
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let table: String = row.get(2)?;
        let sql = row.get_ref(3)?.as_str().map_err(|_| invalid("backup_control_schema"))?;
        if schema.len() >= 64
            || sql.len() > 65536
            || ![&name, &table].iter().all(|v| {
                v.starts_with("intake_")
                    && v.len() <= 128
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            || !matches!(kind.as_str(), "table" | "index")
        {
            return Err(invalid("backup_control_schema"));
        }
        schema.push((kind, name, table, sql.to_owned()));
    }
    let mut hash = Sha256::new();
    let mut bytes = 0usize;
    let mut total_rows = 0usize;
    let mut counts = BTreeMap::new();
    fn part(hash: &mut Sha256, bytes: &mut usize, value: &[u8]) -> Result<()> {
        *bytes = bytes
            .checked_add(value.len() + 8)
            .ok_or_else(|| invalid("backup_control_budget"))?;
        if *bytes > LIMIT {
            return Err(invalid("backup_control_budget"));
        }
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
        Ok(())
    }
    for (kind, name, table, sql) in schema {
        for value in [&kind, &name, &table, &sql] {
            part(&mut hash, &mut bytes, value.as_bytes())?;
        }
        if kind != "table" || name == "intake_objects" {
            continue;
        }
        let query = format!(
            "SELECT rowid,* FROM \"{name}\" ORDER BY rowid LIMIT {}",
            MAX_RECORDS + 1
        );
        let mut statement = c.prepare(&query)?;
        let columns = statement.column_count();
        if columns > 32 {
            return Err(invalid("backup_control_columns"));
        }
        let mut rows = statement.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            total_rows += 1;
            count += 1;
            if total_rows > MAX_RECORDS {
                return Err(invalid("backup_control_rows"));
            }
            part(&mut hash, &mut bytes, b"row")?;
            for i in 0..columns {
                use rusqlite::types::ValueRef;
                match row.get_ref(i)? {
                    ValueRef::Null => part(&mut hash, &mut bytes, b"null")?,
                    ValueRef::Integer(v) => {
                        part(&mut hash, &mut bytes, b"int")?;
                        part(&mut hash, &mut bytes, &v.to_be_bytes())?;
                    }
                    ValueRef::Real(v) => {
                        part(&mut hash, &mut bytes, b"real")?;
                        part(&mut hash, &mut bytes, &v.to_bits().to_be_bytes())?;
                    }
                    ValueRef::Text(v) => {
                        part(&mut hash, &mut bytes, b"text")?;
                        part(&mut hash, &mut bytes, v)?;
                    }
                    ValueRef::Blob(v) => {
                        part(&mut hash, &mut bytes, b"blob")?;
                        part(&mut hash, &mut bytes, v)?;
                    }
                }
            }
        }
        counts.insert(name, count);
    }
    let digest = hash.finalize();
    Ok((
        key.authentication_tag(&[b"backup-controls:v1", digest.as_slice()]),
        bytes,
        counts,
    ))
}
pub(super) fn quarantine_backup(
    path: &Path,
    key: &VaultKey,
    scope: &AssetScope,
) -> Result<BackupRecoveryManifest> {
    let c = Connection::open(path)?;
    let tx = c.unchecked_transaction()?;
    let existing = optional_manifest(&tx, key)?;
    let (fingerprint, bytes, counts) = inventory(&tx, key)?;
    let (control_fingerprint, control_bytes, control_rows) = controls(&tx, key)?;
    let manifest = BackupRecoveryManifest {
        schema_version: "nova.backup-recovery.v0.1".into(),
        backup_id: format!("backup_{}", uuid::Uuid::now_v7()),
        created_at: now()?,
        origin_backup_created_at: existing
            .as_ref()
            .map(|m| m.origin_backup_created_at.clone())
            .unwrap_or(now()?),
        store_version: STORE_VERSION.into(),
        scope: scope.clone(),
        envelope_fingerprint: fingerprint,
        encrypted_bytes: bytes,
        envelope_counts: counts,
        control_fingerprint,
        control_bytes,
        control_rows,
        external_authority_removed: true,
        quarantined: true,
    };
    if existing.is_some() {
        update(&tx, key, MANIFEST_KIND, MANIFEST_ID, &manifest)?;
    } else {
        insert(&tx, key, MANIFEST_KIND, MANIFEST_ID, &manifest)?;
    }
    tx.commit()?;
    Ok(manifest)
}
pub(super) fn record_generation(
    c: &Connection,
    key: &VaultKey,
    m: &BackupRecoveryManifest,
) -> Result<()> {
    // Re-copying a quarantined snapshot must not mutate its integrity inventory or make it fresh.
    if optional_manifest(c, key)?.is_some() {
        return Ok(());
    }
    let tx = c.unchecked_transaction()?;
    let r = BackupGenerationReceipt {
        backup_id: m.backup_id.clone(),
        generated_at: m.created_at.clone(),
        encrypted_bytes: m.encrypted_bytes,
        external_copy_verified: false,
        off_device_restore_verified: false,
    };
    if super::compiler::exists(&tx, "backup_generation", "latest")? {
        update(&tx, key, "backup_generation", "latest", &r)?;
    } else {
        insert(&tx, key, "backup_generation", "latest", &r)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
impl IntakeStore {
    /// Authenticate a marked backup and derive its scope without opening it for writes.
    /// Callers that accept an external file should verify a private staged copy.
    pub fn verify_backup_file(path: &Path, key: VaultKey) -> Result<BackupVerification> {
        let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let manifest = optional_manifest(&connection, &key)?
            .ok_or_else(|| invalid("unmarked_backup_freshness_unknown"))?;
        drop(connection);
        Self::open_read_only(path, key, manifest.scope.clone())?.verify_backup_copy(&manifest.scope)
    }

    pub fn last_backup_generation(
        &self,
        scope: &AssetScope,
    ) -> Result<Option<BackupGenerationReceipt>> {
        self.check(scope)?;
        if !super::compiler::exists(&self.connection, "backup_generation", "latest")? {
            return Ok(None);
        }
        Ok(Some(load(
            &self.connection,
            &self.key,
            "backup_generation",
            "latest",
        )?))
    }
    /// Metadata only. A timestamp is creation evidence, never proof of a fresh external backup.
    pub fn backup_recovery_manifest(
        &self,
        scope: &AssetScope,
    ) -> Result<Option<BackupRecoveryManifest>> {
        self.check(scope)?;
        let m = optional_manifest(&self.connection, &self.key)?;
        if m.as_ref().is_some_and(|m| m.scope != *scope) {
            return Err(denied());
        }
        Ok(m)
    }
    /// Explicit bounded integrity check, without releasing quarantine or granting access.
    pub fn verify_backup_copy(&self, scope: &AssetScope) -> Result<BackupVerification> {
        self.check(scope)?;
        let tx = self.connection.unchecked_transaction()?;
        let manifest = optional_manifest(&tx, &self.key)?
            .ok_or_else(|| invalid("unmarked_backup_freshness_unknown"))?;
        if manifest.scope != *scope {
            return Err(denied());
        }
        let (fingerprint, bytes, counts) = inventory(&tx, &self.key)?;
        let (control_fingerprint, control_bytes, control_rows) = controls(&tx, &self.key)?;
        if manifest.envelope_fingerprint != fingerprint
            || manifest.encrypted_bytes != bytes
            || manifest.envelope_counts != counts
            || manifest.control_fingerprint != control_fingerprint
            || manifest.control_bytes != control_bytes
            || manifest.control_rows != control_rows
        {
            return Err(invalid("backup_envelope_mismatch"));
        }
        tx.commit()?;
        Ok(BackupVerification {
            manifest,
            sqlite_consistent: true,
            envelopes_authenticated: true,
            control_state_matches: true,
            authority_freshness:
                "unknown_later_deletions_and_revocations_require_trusted_checkpoint".into(),
            off_device_restore_verified: false,
            may_automate: false,
        })
    }
}
