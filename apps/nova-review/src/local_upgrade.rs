//! Explicit, stopped-profile bundle selection. No downloads, candidate execution or data migration.
use super::*;
const VERSION: &str = "nova.local-upgrade.v0.1";
const SELECTION: &str = "selected-runtime.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleRef {
    path: PathBuf,
    id: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema_version: String,
    profile_id: String,
    anchor: BundleRef,
    current: BundleRef,
    previous: Option<BundleRef>,
    before_data: String,
    applied_plan: String,
    known_bundles: BTreeSet<String>,
}
#[derive(Serialize)]
struct Plan {
    schema_version: &'static str,
    profile_id: String,
    from: BundleRef,
    to: BundleRef,
    profile_fingerprint: String,
    configuration_compatible: bool,
    data_migration: bool,
    automatic_start: bool,
    signature_verified: bool,
}
fn digest_ok(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn read_selection(dir: &Path) -> Result<Option<Selection>> {
    let path = dir.join(SELECTION);
    if !exists(&path) {
        return Ok(None);
    }
    let s: Selection = read_json(&path)?;
    let (p, _) = profile_info(dir)?;
    if s.schema_version != VERSION
        || s.profile_id != p.profile_id
        || !digest_ok(&s.before_data)
        || !digest_ok(&s.applied_plan)
        || s.known_bundles.len() > 32
        || !s.known_bundles.contains(&s.current.id)
        || !s.known_bundles.iter().all(|id| digest_ok(id))
    {
        return reject("invalid_upgrade_selection");
    }
    for r in [&s.anchor, &s.current]
        .into_iter()
        .chain(s.previous.as_ref())
    {
        absolute(&r.path)?;
        if !digest_ok(&r.id) {
            return reject("invalid_upgrade_selection");
        }
    }
    Ok(Some(s))
}
fn matches_requested(s: &Selection, requested: &Path) -> bool {
    requested == s.anchor.path
        || requested == s.current.path
        || s.previous.as_ref().is_some_and(|p| p.path == requested)
}
/// Existing launcher/desktop may keep invoking the original anchor. Only its explicitly
/// selected profile replacement is followed; an unrelated bundle cannot silently take over.
pub(super) fn selected(dir: &Path, requested: &Path) -> Result<PathBuf> {
    let Some(s) = read_selection(dir)? else {
        return Ok(requested.into());
    };
    if !matches_requested(&s, requested) {
        return reject("bundle_not_in_upgrade_lineage");
    }
    if bundle_info(&s.current.path)?.bundle_id != s.current.id {
        return reject("selected_bundle_changed");
    }
    Ok(s.current.path)
}
fn reference(path: &Path) -> Result<BundleRef> {
    Ok(reference_with_schema(path)?.0)
}
fn reference_with_schema(path: &Path) -> Result<(BundleRef, String)> {
    let manifest = bundle_info(path)?;
    Ok((
        BundleRef {
            path: path.into(),
            id: manifest.bundle_id,
        },
        manifest.schema_version,
    ))
}
fn inspect_format(dir: &Path, p: &Profile) -> Result<()> {
    let hex = Zeroizing::new(read_bytes(&dir.join("vault.key"), 256)?);
    let hex = std::str::from_utf8(&hex).map_err(|_| Failure("profile_format_unverified"))?;
    let key = nova_core::VaultKey::from_hex(hex.trim())
        .map_err(|_| Failure("profile_format_unverified"))?;
    nova_core::Vault::open_readonly(&dir.join("legacy.sqlite"), key.clone())
        .map_err(|_| Failure("profile_format_unverified"))?;
    let scope = nova_core::compilation::AssetScope {
        owner_id: p.owner_id.clone(),
        space: p.space_id.clone(),
    };
    nova_core::intake::IntakeStore::open_read_only(&dir.join("intake.sqlite"), key, scope)
        .map_err(|_| Failure("profile_format_unverified"))?;
    Ok(())
}
fn data_fingerprint(dir: &Path) -> Result<String> {
    let names = [
        "profile.json",
        "runtime.json",
        "vault.key",
        "owner.key",
        "legacy.sqlite",
        "legacy.sqlite-wal",
        "legacy.sqlite-shm",
        "intake.sqlite",
        "intake.sqlite-wal",
        "intake.sqlite-shm",
    ];
    let mut h = Sha256::new();
    let mut total = 0u64;
    for name in names {
        h.update((name.len() as u64).to_be_bytes());
        h.update(name);
        let path = dir.join(name);
        if !exists(&path) {
            h.update(b"absent");
            continue;
        }
        let mut file = open_private(&path, false, false)?;
        let size = file.metadata()?.len();
        total = total
            .checked_add(size)
            .ok_or(Failure("upgrade_inspection_limit"))?;
        if total > 1024 * 1024 * 1024 {
            return reject("upgrade_inspection_limit");
        }
        h.update(b"present");
        h.update(size.to_be_bytes());
        let mut buf = [0u8; 32768];
        let mut read = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > size {
                return reject("profile_changed_during_upgrade");
            }
            h.update(&buf[..n]);
        }
        if read != size {
            return reject("profile_changed_during_upgrade");
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
fn stopped_profile(dir: &Path) -> Result<Profile> {
    let (p, _) = profile_info(dir)?;
    if status(dir, &p)?["status"] != "stopped" {
        return reject("stop_before_upgrade");
    }
    inspect_format(dir, &p)?;
    Ok(p)
}
fn plan(a: &Cli, requested: &Path, p: &Profile) -> Result<Plan> {
    let candidate = a
        .candidate
        .as_deref()
        .ok_or(Failure("upgrade_candidate_required"))?;
    absolute(candidate)?;
    if candidate.starts_with(&a.directory) || a.directory.starts_with(candidate) {
        return reject("bundle_data_overlap");
    }
    let (from, from_schema) = reference_with_schema(&selected(&a.directory, requested)?)?;
    let (to, to_schema) = reference_with_schema(candidate)?;
    // Recognizing an old package is not evidence that old software can read new data.
    // A recorded rollback retains its existing, stricter unchanged-data fence below.
    if from_schema == BUNDLE && to_schema == LEGACY_BUNDLE {
        return reject("legacy_bundle_requires_separate_data_compatibility_review");
    }
    if from.id == to.id {
        return reject("same_bundle");
    }
    let fingerprint = data_fingerprint(&a.directory)?;
    if let Some(s) = read_selection(&a.directory)? {
        if s.known_bundles.contains(&to.id) && s.before_data != fingerprint {
            return reject("rollback_requires_separate_data_compatibility_review");
        }
        if s.known_bundles.len() >= 32 && !s.known_bundles.contains(&to.id) {
            return reject("upgrade_history_limit");
        }
    }
    Ok(Plan {
        schema_version: VERSION,
        profile_id: p.profile_id.clone(),
        from,
        to,
        profile_fingerprint: fingerprint,
        configuration_compatible: true,
        data_migration: false,
        automatic_start: false,
        signature_verified: false,
    })
}
fn plan_id(p: &Plan) -> Result<String> {
    Ok(hash(
        &serde_json::to_vec(p).map_err(|_| Failure("invalid_upgrade_plan"))?,
    ))
}
fn persist(dir: &Path, s: &Selection) -> Result<()> {
    write_json(&dir.join(SELECTION), s, false)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
pub(super) fn execute(a: &Cli, requested: &Path) -> Result<Value> {
    owned(&a.directory, true)?;
    let _guard = lock(&a.directory, "operation.lock", true)?;
    let p = stopped_profile(&a.directory)?;
    let _service = lock(&a.directory, "service.lock", false)?;
    if state_info(&a.directory, &p)?.is_some() {
        return reject("stop_before_upgrade");
    }
    if a.command == Action::UpgradeRollback {
        if a.candidate.is_some() || a.plan_id.is_some() {
            return reject("rollback_arguments_invalid");
        }
        let mut s = read_selection(&a.directory)?.ok_or(Failure("no_upgrade_to_rollback"))?;
        if !matches_requested(&s, requested) {
            return reject("bundle_not_in_upgrade_lineage");
        }
        let previous = s
            .previous
            .as_ref()
            .ok_or(Failure("no_upgrade_to_rollback"))?;
        if reference(&previous.path)?.id != previous.id {
            return reject("rollback_bundle_changed");
        }
        if data_fingerprint(&a.directory)? != s.before_data {
            return reject("rollback_requires_separate_data_compatibility_review");
        }
        s.current = previous.clone();
        s.previous = None;
        persist(&a.directory, &s)?;
        return Ok(
            json!({"status":"rolled_back","selected_bundle_id":s.current.id,"selected_bundle":s.current.path,"data_restored":false,"runtime_started":false,"signature_verified":false}),
        );
    }
    let plan = plan(a, requested, &p)?;
    let id = plan_id(&plan)?;
    if a.command == Action::UpgradePreview {
        if a.plan_id.is_some() {
            return reject("preview_does_not_accept_plan_id");
        }
        return Ok(
            json!({"status":"previewed","plan_id":id,"plan":plan,"downloaded":false,"compatibility":"same_supported_bundle_profile_runtime_and_store_formats_only"}),
        );
    }
    if a.plan_id.as_deref() != Some(id.as_str()) {
        return reject("upgrade_preview_changed");
    }
    let previous = read_selection(&a.directory)?;
    let mut known_bundles = previous
        .as_ref()
        .map(|s| s.known_bundles.clone())
        .unwrap_or_default();
    known_bundles.insert(plan.from.id.clone());
    known_bundles.insert(plan.to.id.clone());
    let anchor = previous
        .map(|s| s.anchor)
        .unwrap_or_else(|| plan.from.clone());
    let selection = Selection {
        schema_version: VERSION.into(),
        profile_id: p.profile_id,
        anchor,
        current: plan.to,
        previous: Some(plan.from),
        before_data: plan.profile_fingerprint,
        applied_plan: id,
        known_bundles,
    };
    persist(&a.directory, &selection)?;
    Ok(
        json!({"status":"selected","selected_bundle_id":selection.current.id,"selected_bundle":selection.current.path,"data_migrated":false,"runtime_started":false,"signature_verified":false,"rollback":"only_while_profile_data_and_configuration_match_pre_upgrade_snapshot"}),
    )
}
