//! Prepare a fresh, quarantined profile from an authenticated encrypted snapshot.
use super::*;
use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::IntakeStore};

const MAX_SNAPSHOT: u64 = 256 * 1024 * 1024;

pub(super) fn require_profile(
    directory: &Path,
    profile: &Profile,
    runtime: &Runtime,
) -> Result<()> {
    if runtime.compiler_enabled || runtime.continuity_enabled || runtime.ranked_retrieval_enabled {
        return reject("backup_inspection_required");
    }
    let scope = AssetScope {
        owner_id: profile.owner_id.clone(),
        space: profile.space_id.clone(),
    };
    let key = VaultKey::read_file(&directory.join("vault.key"))
        .map_err(|_| Failure("backup_inspection_required"))?;
    let store = IntakeStore::open_read_only(&directory.join("intake.sqlite"), key, scope.clone())
        .map_err(|_| Failure("backup_inspection_required"))?;
    if store
        .backup_recovery_manifest(&scope)
        .map_err(|_| Failure("backup_inspection_required"))?
        .is_none()
    {
        return reject("backup_inspection_required");
    }
    Ok(())
}

pub(super) fn prepare(args: &Cli) -> Result<Value> {
    if exists(&args.directory) {
        return reject("profile_exists");
    }
    let snapshot = args
        .backup_file
        .as_deref()
        .ok_or(Failure("backup_file_required"))?;
    let key_file = args
        .backup_key_file
        .as_deref()
        .ok_or(Failure("backup_key_file_required"))?;
    let key_bytes =
        Zeroizing::new(read_bytes(key_file, 4096).map_err(|_| Failure("invalid_backup_key_file"))?);
    let key = VaultKey::from_hex(
        std::str::from_utf8(&key_bytes).map_err(|_| Failure("invalid_backup_key_file"))?,
    )
    .map_err(|_| Failure("invalid_backup_key_file"))?;

    let parent = args.directory.parent().ok_or(Failure("invalid_path"))?;
    let stage = tempfile::tempdir_in(parent)?;
    let intake = stage.path().join("intake.sqlite");
    stage_snapshot(snapshot, &intake)?;
    let checked = IntakeStore::verify_backup_file(&intake, key.clone())
        .map_err(|_| Failure("backup_verification_failed"))?;
    let scope = checked.manifest.scope;
    let profile = Profile {
        schema_version: "nova.local-profile.v0.1".into(),
        profile_id: uuid::Uuid::now_v7().to_string(),
        owner_id: scope.owner_id,
        space_id: scope.space,
        created_by: "nova-local-init".into(),
    };
    let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let legacy = stage.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone()).map_err(|_| Failure("initialization_failed"))?);
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o600))?;
    key.write_new_file(&stage.path().join("vault.key"))
        .map_err(|_| Failure("initialization_failed"))?;
    VaultKey::generate()
        .and_then(|owner| owner.write_new_file(&stage.path().join("owner.key")))
        .map_err(|_| Failure("initialization_failed"))?;
    write_initial_runtime(stage.path(), &profile, port)?;
    write_json(&stage.path().join("profile.json"), &profile, true)?;
    profile_info(stage.path())?;

    // Exclusive destination creation. Publish the profile marker last, as the
    // normal initializer does. A concurrent invocation cannot reuse this path.
    fs::DirBuilder::new().mode(0o700).create(&args.directory)?;
    for name in [
        "vault.key",
        "owner.key",
        "legacy.sqlite",
        "intake.sqlite",
        "runtime.json",
        "profile.json",
    ] {
        fs::rename(stage.path().join(name), args.directory.join(name))?;
    }
    File::open(&args.directory)?.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(json!({
        "status": "backup_inspection_prepared", "port": port, "profile_id": profile.profile_id,
        "backup_quarantined": true, "runtime_started": false, "off_device_restore_verified": false,
    }))
}

fn stage_snapshot(source: &Path, destination: &Path) -> Result<()> {
    absolute(source)?;
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)?;
    let metadata = source.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid()
        || metadata.nlink() != 1
        || metadata.len() > MAX_SNAPSHOT
    {
        return reject("invalid_backup_file");
    }
    let mut staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    if std::io::copy(&mut source.take(MAX_SNAPSHOT + 1), &mut staged)? > MAX_SNAPSHOT {
        return reject("invalid_backup_file");
    }
    staged.sync_all()?;
    Ok(())
}
