//! Owner-held, encrypted transfer of a quarantined IntakeStore snapshot.
//! The original data key is inside the outer AEAD, never in the public header or report.
use super::{storage::check_scope, *};
use crate::{VaultKey, crypto::EncryptedBlob};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8] = b"NOVA-INTAKE-PORTABLE-V1\n";
const VERSION: &str = "nova.intake-portable.v0.1";
const META_LIMIT: usize = 8192;
/// Format implementation limit, not a commercial storage allowance.
pub const MAX_INTAKE_PORTABLE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    schema_version: String,
    scope: AssetScope,
    vault_key_hex: String,
    snapshot_sha256: String,
}
impl Drop for Metadata {
    fn drop(&mut self) {
        self.vault_key_hex.zeroize();
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortableIntakeRestore {
    pub schema_version: String,
    pub scope: AssetScope,
    pub snapshot_sha256: String,
    pub verification: BackupVerification,
    pub restored_locally: bool,
}

fn private_dir(path: &Path) -> Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)?;
    Ok(())
}
fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    if !f.metadata()?.is_file() || f.metadata()?.len() > limit as u64 {
        return Err(invalid("portable_size_limit"));
    }
    let mut bytes = vec![];
    f.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid("portable_size_limit"));
    }
    Ok(bytes)
}

impl IntakeStore {
    /// Explicit Owner export. Snapshot strips external credentials and stays quarantined.
    /// Use a fresh, separately held export key, not a password or the online VMK.
    pub fn export_portable(&self, scope: &AssetScope, export_key: &VaultKey) -> Result<Vec<u8>> {
        self.export_portable_with_scratch(scope, export_key, None)
    }
    /// Trusted host-selected private scratch directory. Never accept a model or HTTP path.
    /// Keeps temporary snapshots on the host's monitored filesystem; the directory is not retained.
    pub fn export_portable_in(
        &self,
        scope: &AssetScope,
        export_key: &VaultKey,
        parent: &Path,
    ) -> Result<Vec<u8>> {
        let metadata = std::fs::symlink_metadata(parent)?;
        if !parent.is_absolute()
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || parent.canonicalize()? != parent
        {
            return Err(invalid("portable_scratch_directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(invalid("portable_scratch_permissions"));
            }
        }
        self.export_portable_with_scratch(scope, export_key, Some(parent))
    }
    fn export_portable_with_scratch(
        &self,
        scope: &AssetScope,
        export_key: &VaultKey,
        parent: Option<&Path>,
    ) -> Result<Vec<u8>> {
        self.check(scope)?;
        let pages: u64 = self
            .connection
            .query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let page_size: u64 = self
            .connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))?;
        if pages
            .checked_mul(page_size)
            .is_none_or(|n| n > (MAX_INTAKE_PORTABLE_BYTES - META_LIMIT - 128) as u64)
        {
            return Err(invalid("portable_size_limit"));
        }
        let mut temporary = tempfile::Builder::new();
        temporary.prefix(if parent.is_some() {
            ".nova-intake-export-"
        } else {
            "nova-intake-export-"
        });
        let tmp = if let Some(parent) = parent {
            temporary.tempdir_in(parent)?
        } else {
            temporary.tempdir()?
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let path = tmp.path().join("intake.sqlite");
        self.backup_to(scope, &path)?;
        let snapshot = Self::open_read_only(&path, self.key.clone(), scope.clone())?;
        snapshot.verify_backup_copy(scope)?;
        drop(snapshot);
        let data = bounded_read(&path, MAX_INTAKE_PORTABLE_BYTES - META_LIMIT - 128)?;
        let metadata = Metadata {
            schema_version: VERSION.into(),
            scope: scope.clone(),
            vault_key_hex: self.key.to_hex(),
            snapshot_sha256: hex::encode(Sha256::digest(&data)),
        };
        let encoded = Zeroizing::new(serde_json::to_vec(&metadata)?);
        if encoded.len() > META_LIMIT {
            return Err(invalid("portable_metadata_limit"));
        }
        let mut plaintext = Zeroizing::new(Vec::with_capacity(4 + encoded.len() + data.len()));
        plaintext.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        plaintext.extend_from_slice(&encoded);
        plaintext.extend_from_slice(&data);
        let sealed = export_key.encrypt(&plaintext, MAGIC)?;
        let mut package = Vec::with_capacity(MAGIC.len() + 24 + sealed.ciphertext.len());
        package.extend_from_slice(MAGIC);
        package.extend_from_slice(&sealed.nonce);
        package.extend_from_slice(&sealed.ciphertext);
        Ok(package)
    }
}

struct DecodedPortable {
    metadata: Metadata,
    plain: Zeroizing<Vec<u8>>,
    snapshot_start: usize,
}
fn decode_portable(bytes: &[u8], export_key: &VaultKey) -> Result<DecodedPortable> {
    if bytes.len() > MAX_INTAKE_PORTABLE_BYTES
        || bytes.len() < MAGIC.len() + 24 + 16 + 4
        || !bytes.starts_with(MAGIC)
    {
        return Err(invalid("invalid_portable_envelope"));
    }
    let plain = Zeroizing::new(export_key.decrypt(
        &EncryptedBlob {
            nonce: bytes[MAGIC.len()..MAGIC.len() + 24].to_vec(),
            ciphertext: bytes[MAGIC.len() + 24..].to_vec(),
        },
        MAGIC,
    )?);
    let prefix: [u8; 4] = plain
        .get(..4)
        .ok_or_else(|| invalid("portable_metadata"))?
        .try_into()
        .map_err(|_| invalid("portable_metadata"))?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > META_LIMIT || len + 4 >= plain.len() {
        return Err(invalid("portable_metadata"));
    }
    let metadata: Metadata = serde_json::from_slice(&plain[4..4 + len])?;
    check_scope(&metadata.scope)?;
    if metadata.schema_version != VERSION || metadata.vault_key_hex.len() != 64 {
        return Err(invalid("portable_version_or_key"));
    }
    let data = &plain[4 + len..];
    if metadata.snapshot_sha256 != hex::encode(Sha256::digest(data)) {
        return Err(invalid("portable_snapshot_digest"));
    }
    VaultKey::from_hex(&metadata.vault_key_hex)?;
    Ok(DecodedPortable {
        metadata,
        plain,
        snapshot_start: 4 + len,
    })
}

/// Authenticate the portable envelope and embedded snapshot digest without restoring files.
/// This does not verify SQLite contents, prove current authority, or activate any connection.
pub fn verify_intake_portable_envelope(bytes: &[u8], export_key: &VaultKey) -> Result<AssetScope> {
    Ok(decode_portable(bytes, export_key)?.metadata.scope.clone())
}

/// Re-encrypt an authenticated snapshot for a separately held Owner export key.
/// Preserves quarantine and the original embedded key/scope; does not activate or refresh it.
pub fn rekey_intake_portable(
    bytes: &[u8],
    current_key: &VaultKey,
    export_key: &VaultKey,
) -> Result<(AssetScope, Vec<u8>)> {
    let decoded = decode_portable(bytes, current_key)?;
    let sealed = export_key.encrypt(&decoded.plain, MAGIC)?;
    let package = [MAGIC, sealed.nonce.as_slice(), sealed.ciphertext.as_slice()].concat();
    Ok((decoded.metadata.scope.clone(), package))
}

/// Restore only into a new private directory. Verifies the complete snapshot before retaining it.
/// Does not activate automation, remap identity, import into a live Vault, or prove freshness.
pub fn restore_intake_portable(
    bytes: &[u8],
    export_key: &VaultKey,
    destination: &Path,
) -> Result<PortableIntakeRestore> {
    let decoded = decode_portable(bytes, export_key)?;
    let metadata = &decoded.metadata;
    let data = &decoded.plain[decoded.snapshot_start..];
    let key = VaultKey::from_hex(&metadata.vault_key_hex)?;
    private_dir(destination)?; // Existing files, directories and symlinks are never replaced.
    let result = (|| {
        let db = destination.join("intake.sqlite");
        write_new(&db, data)?;
        let store = IntakeStore::open_read_only(&db, key.clone(), metadata.scope.clone())?;
        let verification = store.verify_backup_copy(&metadata.scope)?;
        drop(store);
        key.write_new_file(&destination.join("vault.key"))?;
        let report = PortableIntakeRestore {
            schema_version: VERSION.into(),
            scope: metadata.scope.clone(),
            snapshot_sha256: metadata.snapshot_sha256.clone(),
            verification,
            restored_locally: true,
        };
        write_new(
            &destination.join("inspection.json"),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        std::fs::File::open(destination)?.sync_all()?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(destination);
    }
    result
}

/// File entry point keeps hostile/accidental large inputs bounded before loading them.
pub fn restore_intake_portable_file(
    path: &Path,
    export_key: &VaultKey,
    destination: &Path,
) -> Result<PortableIntakeRestore> {
    restore_intake_portable(
        &bounded_read(path, MAX_INTAKE_PORTABLE_BYTES)?,
        export_key,
        destination,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seal(key: &VaultKey, metadata: &Metadata, data: &[u8]) -> Vec<u8> {
        let m = serde_json::to_vec(metadata).unwrap();
        let mut p = (m.len() as u32).to_be_bytes().to_vec();
        p.extend(m);
        p.extend(data);
        let b = key.encrypt(&p, MAGIC).unwrap();
        [MAGIC, b.nonce.as_slice(), b.ciphertext.as_slice()].concat()
    }
    #[test]
    fn authenticated_invalid_database_is_removed_without_retaining_inner_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let data = b"not a sqlite database";
        let m = Metadata {
            schema_version: VERSION.into(),
            scope: AssetScope {
                owner_id: "owner:test".into(),
                space: "personal".into(),
            },
            vault_key_hex: VaultKey::generate().unwrap().to_hex(),
            snapshot_sha256: hex::encode(Sha256::digest(data)),
        };
        let out = dir.path().join("restore");
        assert!(restore_intake_portable(&seal(&key, &m, data), &key, &out).is_err());
        assert!(!out.exists());
    }
    #[test]
    fn valid_aead_does_not_bypass_digest_or_metadata_limits() {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let data = b"wrong digest";
        let m = Metadata {
            schema_version: VERSION.into(),
            scope: AssetScope {
                owner_id: "owner:test".into(),
                space: "personal".into(),
            },
            vault_key_hex: VaultKey::generate().unwrap().to_hex(),
            snapshot_sha256: "00".repeat(32),
        };
        let out = dir.path().join("restore");
        assert!(restore_intake_portable(&seal(&key, &m, data), &key, &out).is_err());
        assert!(!out.exists());
        let raw = key.encrypt(&u32::MAX.to_be_bytes(), MAGIC).unwrap();
        let bytes = [MAGIC, raw.nonce.as_slice(), raw.ciphertext.as_slice()].concat();
        assert!(restore_intake_portable(&bytes, &key, &out).is_err());
        assert!(!out.exists());
    }
}
