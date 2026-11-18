//! Hook configuration planning and isolated fixture installation only. No live Host configuration I/O.
use super::{
    storage::{conflict, parse_time},
    *,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

pub const HOOK_INSTALL_VERSION: &str = "nova.hook-install.v0.1";
const MAX_CONFIG: usize = 256 * 1024;
const FIXTURE_MARKER: &[u8] = b"nova-hook-install-fixture-only-v1\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureHookHost {
    CodexCli,
    ClaudeCodeCli,
}
impl CaptureHookHost {
    fn slug(self) -> &'static str {
        match self {
            Self::CodexCli => "codex",
            Self::ClaudeCodeCli => "claude",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::CodexCli => ".codex/hooks.json",
            Self::ClaudeCodeCli => ".claude/settings.json",
        }
    }
    fn events(self) -> &'static [&'static str] {
        match self {
            Self::CodexCli => &["UserPromptSubmit", "Stop"],
            Self::ClaudeCodeCli => &["Stop"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureHookInstallSpec {
    pub installation_id: String,
    pub host: CaptureHookHost,
    /// Fixed native consumer path. Merely rendering this path does not attest its binary.
    pub executable: PathBuf,
    pub binding_file: PathBuf,
    pub target: CaptureRelayTarget,
    pub observed_host_version: Option<String>,
}
impl CaptureHookInstallSpec {
    fn validate(&self) -> Result<()> {
        let t = &self.target;
        if !identifier(&self.installation_id)
            || [
                &t.instance_id,
                &t.target_id,
                &t.grant_id,
                &t.source_namespace,
                &t.session_id,
            ]
            .iter()
            .any(|s| !identifier(s))
            || self
                .observed_host_version
                .as_ref()
                .is_some_and(|s| !identifier(s))
        {
            return Err(invalid("hook_spec_bounds"));
        }
        parse_time(&t.expires_at)?;
        absolute_path(&self.executable)?;
        absolute_path(&self.binding_file)?;
        if self.executable.file_name().and_then(|s| s.to_str()) != Some("nova-capture-hook") {
            return Err(invalid("hook_native_consumer_required"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureHookInstallPhase {
    Prepared,
    FixtureApplied,
    FixtureRemoved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureHookOwnedEntry {
    pub event: String,
    pub group: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureHookManifest {
    pub schema_version: String,
    pub spec: CaptureHookInstallSpec,
    pub phase: CaptureHookInstallPhase,
    pub operation: String,
    pub before_sha256: String,
    pub after_sha256: String,
    pub prior_manifest_sha256: Option<String>,
    pub backup_file: String,
    pub owned_entries: Vec<CaptureHookOwnedEntry>,
    pub real_host_verified: bool,
}

pub struct CaptureHookInstallPlan {
    manifest: CaptureHookManifest,
    rendered: Vec<u8>,
    before: Option<Vec<u8>>,
    expected_manifest: Option<Vec<u8>>,
}
impl CaptureHookInstallPlan {
    pub fn rendered(&self) -> &[u8] {
        &self.rendered
    }
    pub fn manifest(&self) -> &CaptureHookManifest {
        &self.manifest
    }
    pub fn required_live_checks(&self) -> &'static [&'static str] {
        &[
            "owner_selected_config_and_scope",
            "native_consumer_hash_verified",
            "capture_grant_and_session_registered",
            "target_instance_and_transport_verified",
            "single_owner_and_cross_path_dedup_verified",
            "installed_host_version_probed",
            "exact_hook_definition_trusted",
            "real_event_trigger_and_source_ack_observed",
        ]
    }
}

fn absolute_path(path: &Path) -> Result<()> {
    let text = path.to_str().ok_or_else(|| invalid("hook_path_encoding"))?;
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || text.contains("${")
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("hook_absolute_path_required"));
    }
    Ok(())
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn current_sha(bytes: Option<&[u8]>) -> String {
    bytes.map(sha).unwrap_or_else(|| "absent".into())
}
fn encoded<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}
fn parse_config(bytes: Option<&[u8]>) -> Result<Value> {
    let value: Value = match bytes {
        None => json!({}),
        Some(b) => {
            if b.len() > MAX_CONFIG {
                return Err(invalid("hook_configuration_bounds"));
            }
            crate::compilation::decode_untrusted_json(b)?
        }
    };
    if !value.is_object() || value.get("hooks").is_some_and(|h| !h.is_object()) {
        return Err(invalid("hook_configuration_shape"));
    }
    if let Some(hooks) = value.get("hooks").and_then(Value::as_object) {
        for groups in hooks.values() {
            if !groups.is_array() || groups.as_array().is_some_and(|g| g.len() > 256) {
                return Err(invalid("hook_configuration_shape"));
            }
        }
    }
    Ok(value)
}
/// POSIX shell literal quoting, including apostrophes. No event data is ever interpolated.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
fn entries(spec: &CaptureHookInstallSpec) -> Result<Vec<CaptureHookOwnedEntry>> {
    let executable = spec
        .executable
        .to_str()
        .ok_or_else(|| invalid("hook_path_encoding"))?;
    let binding = spec
        .binding_file
        .to_str()
        .ok_or_else(|| invalid("hook_path_encoding"))?;
    Ok(spec
        .host
        .events()
        .iter()
        .map(|event| {
            let args = [
                "enqueue-visible",
                "--binding-file",
                binding,
                "--host",
                spec.host.slug(),
                "--installation",
                &spec.installation_id,
                "--event",
                event,
            ];
            let handler = match spec.host {
                CaptureHookHost::CodexCli => {
                    let command = std::iter::once(executable)
                        .chain(args)
                        .map(quote)
                        .collect::<Vec<_>>()
                        .join(" ");
                    json!({"type":"command","command":command,"timeout":5})
                }
                CaptureHookHost::ClaudeCodeCli => {
                    json!({"type":"command","command":executable,"args":args,"timeout":5})
                }
            };
            CaptureHookOwnedEntry {
                event: (*event).into(),
                group: json!({"hooks":[handler]}),
            }
        })
        .collect())
}
fn validate_manifest(m: &CaptureHookManifest, spec: &CaptureHookInstallSpec) -> Result<()> {
    if m.schema_version != HOOK_INSTALL_VERSION
        || m.spec != *spec
        || m.real_host_verified
        || !matches!(
            m.phase,
            CaptureHookInstallPhase::FixtureApplied | CaptureHookInstallPhase::FixtureRemoved
        )
        || serde_json::to_value(&m.owned_entries)? != serde_json::to_value(entries(spec)?)?
    {
        return Err(invalid("hook_manifest_ownership"));
    }
    Ok(())
}
fn prior_manifest(
    bytes: Option<&[u8]>,
    spec: &CaptureHookInstallSpec,
) -> Result<Option<CaptureHookManifest>> {
    let Some(b) = bytes else { return Ok(None) };
    if b.len() > 64 * 1024 {
        return Err(invalid("hook_manifest_bounds"));
    }
    let m: CaptureHookManifest = crate::compilation::decode_untrusted_json(b)?;
    validate_manifest(&m, spec)?;
    Ok(Some(m))
}
fn prepare_plan(
    spec: CaptureHookInstallSpec,
    current: Option<&[u8]>,
    prior: Option<&[u8]>,
    uninstall: bool,
) -> Result<CaptureHookInstallPlan> {
    spec.validate()?;
    let previous = prior_manifest(prior, &spec)?;
    if uninstall && previous.is_none() {
        return Err(invalid("hook_missing_owned_manifest"));
    }
    let owned = entries(&spec)?;
    let mut value = parse_config(current)?;
    if !value.as_object().unwrap().contains_key("hooks") {
        value
            .as_object_mut()
            .unwrap()
            .insert("hooks".into(), json!({}));
    }
    let hooks = value.get_mut("hooks").unwrap().as_object_mut().unwrap();
    let mut changed = false;
    for entry in &owned {
        let groups = hooks
            .entry(entry.event.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| invalid("hook_event_shape"))?;
        let handler = &entry.group["hooks"][0];
        if groups.iter().any(|g| {
            *g != entry.group
                && g.get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hs| hs.contains(handler))
        }) {
            return Err(invalid("hook_matching_handler_outside_owned_group"));
        }
        let matches = groups.iter().filter(|g| **g == entry.group).count();
        if matches > 1 {
            return Err(invalid("hook_duplicate_owned_entry"));
        }
        if uninstall {
            if matches == 1 {
                groups.retain(|g| *g != entry.group);
                changed = true;
            } else if previous
                .as_ref()
                .is_some_and(|m| m.phase == CaptureHookInstallPhase::FixtureApplied)
            {
                // Edited or missing owned definitions are not silently treated as ours.
                return Err(invalid("hook_owned_definition_changed"));
            }
        } else if matches == 1 {
            if !previous
                .as_ref()
                .is_some_and(|m| m.phase == CaptureHookInstallPhase::FixtureApplied)
            {
                return Err(invalid("hook_matching_entry_without_ownership"));
            }
        } else {
            if previous
                .as_ref()
                .is_some_and(|m| m.phase == CaptureHookInstallPhase::FixtureApplied)
            {
                return Err(invalid("hook_owned_definition_changed"));
            }
            groups.push(entry.group.clone());
            changed = true;
        }
    }
    // Empty lists are valid and preserve any original container; no unrelated key is removed.
    let rendered = if changed {
        encoded(&value)?
    } else {
        current.unwrap_or(b"{}\n").to_vec()
    };
    if rendered.len() > MAX_CONFIG {
        return Err(invalid("hook_configuration_bounds"));
    }
    let before_sha256 = current_sha(current);
    let after_sha256 = sha(&rendered);
    let operation = if uninstall { "remove" } else { "install" };
    let backup_file = format!(
        "{}.{}.{}.backup",
        spec.installation_id,
        operation,
        &sha(before_sha256.as_bytes())[..16]
    );
    Ok(CaptureHookInstallPlan {
        manifest: CaptureHookManifest {
            schema_version: HOOK_INSTALL_VERSION.into(),
            spec,
            phase: CaptureHookInstallPhase::Prepared,
            operation: operation.into(),
            before_sha256,
            after_sha256,
            prior_manifest_sha256: prior.map(sha),
            backup_file,
            owned_entries: owned,
            real_host_verified: false,
        },
        rendered,
        before: current.map(<[u8]>::to_vec),
        expected_manifest: prior.map(<[u8]>::to_vec),
    })
}
pub fn prepare_capture_hook_install(
    spec: CaptureHookInstallSpec,
    current: Option<&[u8]>,
    prior_manifest: Option<&[u8]>,
) -> Result<CaptureHookInstallPlan> {
    prepare_plan(spec, current, prior_manifest, false)
}
pub fn prepare_capture_hook_uninstall(
    spec: CaptureHookInstallSpec,
    current: Option<&[u8]>,
    prior_manifest: Option<&[u8]>,
) -> Result<CaptureHookInstallPlan> {
    prepare_plan(spec, current, prior_manifest, true)
}

/// Only a newly created isolated fixture root can be represented by this type.
/// There is intentionally no `apply_live`, existing-directory constructor, HOME discovery or Host launcher.
pub struct CaptureHookFixture {
    root: PathBuf,
}
fn read_optional(path: &Path, max: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > max as u64 {
        return Err(invalid("hook_fixture_file_type_or_size"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid("hook_fixture_hardlink"));
        }
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > max {
        return Err(invalid("hook_fixture_file_size"));
    }
    Ok(Some(bytes))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("hook_fixture_parent"))?;
    let tmp = parent.join(format!(".nova-hook-{}.tmp", uuid::Uuid::now_v7()));
    write_new(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}
impl CaptureHookFixture {
    pub fn create(root: &Path) -> Result<Self> {
        absolute_path(root)?;
        let parent = root
            .parent()
            .ok_or_else(|| invalid("hook_fixture_parent"))?
            .canonicalize()?;
        let root = parent.join(
            root.file_name()
                .ok_or_else(|| invalid("hook_fixture_root"))?,
        );
        std::fs::create_dir(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        for name in [".codex", ".claude", "manifests", "backups"] {
            std::fs::create_dir(root.join(name))?;
        }
        write_new(&root.join("fixture.marker"), FIXTURE_MARKER)?;
        write_new(&root.join("fixture.lock"), b"")?;
        Ok(Self { root })
    }
    pub fn config_path(&self, host: CaptureHookHost) -> PathBuf {
        self.root.join(host.path())
    }
    fn manifest_path(&self, id: &str) -> Result<PathBuf> {
        if !identifier(id) {
            return Err(invalid("hook_installation_id"));
        }
        Ok(self.root.join("manifests").join(format!("{id}.json")))
    }
    fn lock(&self) -> Result<File> {
        if read_optional(&self.root.join("fixture.marker"), 128)?.as_deref() != Some(FIXTURE_MARKER)
        {
            return Err(invalid("hook_fixture_marker"));
        }
        for directory in [".codex", ".claude", "manifests", "backups"] {
            let p = self.root.join(directory);
            if std::fs::symlink_metadata(&p)?.file_type().is_symlink()
                || p.canonicalize()?.parent() != Some(self.root.as_path())
            {
                return Err(invalid("hook_fixture_path_escape"));
            }
        }
        read_optional(&self.root.join("fixture.lock"), 0)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.join("fixture.lock"))?;
        file.lock()?;
        Ok(file)
    }
    pub fn seed(&self, host: CaptureHookHost, bytes: &[u8]) -> Result<()> {
        let _lock = self.lock()?;
        parse_config(Some(bytes))?;
        write_new(&self.config_path(host), bytes)
    }
    pub fn config(&self, host: CaptureHookHost) -> Result<Option<Vec<u8>>> {
        let _lock = self.lock()?;
        read_optional(&self.config_path(host), MAX_CONFIG)
    }
    pub fn manifest(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let _lock = self.lock()?;
        read_optional(&self.manifest_path(id)?, 64 * 1024)
    }
    /// Optimistic hash CAS under a fixture-wide lock, same-directory atomic config replacement,
    /// durable exact backup and prepared intent before replacement. Does not edit real Host paths.
    pub fn apply(&self, plan: &CaptureHookInstallPlan) -> Result<CaptureHookManifest> {
        let _lock = self.lock()?;
        let path = self.config_path(plan.manifest.spec.host);
        let manifest_path = self.manifest_path(&plan.manifest.spec.installation_id)?;
        let current = read_optional(&path, MAX_CONFIG)?;
        let prior = read_optional(&manifest_path, 64 * 1024)?;
        if current_sha(current.as_deref()) != plan.manifest.before_sha256
            || current != plan.before
            || prior != plan.expected_manifest
        {
            return Err(conflict());
        }
        if plan.manifest.after_sha256 != sha(&plan.rendered) {
            return Err(invalid("hook_plan_hash"));
        }
        let backup = self.root.join("backups").join(&plan.manifest.backup_file);
        let exact_before = current.as_deref().unwrap_or(b"");
        match read_optional(&backup, MAX_CONFIG)? {
            Some(b) if b != exact_before => return Err(conflict()),
            Some(_) => {}
            None => write_new(&backup, exact_before)?,
        }
        File::open(self.root.join("backups"))?.sync_all()?;
        atomic_replace(&manifest_path, &encoded(&plan.manifest)?)?;
        // Recheck immediately before rename. Non-cooperating editors must be quiesced for live use.
        if read_optional(&path, MAX_CONFIG)? != current {
            return Err(conflict());
        }
        atomic_replace(&path, &plan.rendered)?;
        let mut completed = plan.manifest.clone();
        completed.phase = if completed.operation == "remove" {
            CaptureHookInstallPhase::FixtureRemoved
        } else {
            CaptureHookInstallPhase::FixtureApplied
        };
        atomic_replace(&manifest_path, &encoded(&completed)?)?;
        Ok(completed)
    }
    /// Repairs only the manifest after a completed config rename. Never applies an uncommitted plan.
    pub fn recover(&self, id: &str) -> Result<CaptureHookManifest> {
        let _lock = self.lock()?;
        let path = self.manifest_path(id)?;
        let bytes = read_optional(&path, 64 * 1024)?
            .ok_or_else(|| invalid("hook_missing_owned_manifest"))?;
        let mut m: CaptureHookManifest = crate::compilation::decode_untrusted_json(&bytes)?;
        m.spec.validate()?;
        if m.schema_version != HOOK_INSTALL_VERSION
            || m.spec.installation_id != id
            || m.real_host_verified
        {
            return Err(invalid("hook_manifest_ownership"));
        }
        if m.phase != CaptureHookInstallPhase::Prepared {
            validate_manifest(&m, &m.spec)?;
            return Ok(m);
        }
        let current = read_optional(&self.config_path(m.spec.host), MAX_CONFIG)?;
        if current_sha(current.as_deref()) != m.after_sha256 {
            return Err(invalid("hook_prepared_intent_needs_review"));
        }
        m.phase = if m.operation == "remove" {
            CaptureHookInstallPhase::FixtureRemoved
        } else if m.operation == "install" {
            CaptureHookInstallPhase::FixtureApplied
        } else {
            return Err(invalid("hook_operation"));
        };
        validate_manifest(&m, &m.spec)?;
        atomic_replace(&path, &encoded(&m)?)?;
        Ok(m)
    }
}

/// Pure adapter projection. It never opens transcript_path or retains other event fields.
/// `sequence` is supplied by the trusted durable session ledger, not by the Hook JSON.
/// Consumers must reject missing stable host identity rather than deduplicating by text.
pub fn decode_visible_capture_hook(
    host: CaptureHookHost,
    expected_origin_session: &str,
    sequence: u64,
    bytes: &[u8],
) -> Result<CaptureRelayEvent> {
    if bytes.len() > 1024 * 1024
        || !identifier(expected_origin_session)
        || !(1..=1_000_000).contains(&sequence)
    {
        return Err(invalid("hook_event_bounds"));
    }
    let v: Value = crate::compilation::decode_untrusted_json(bytes)?;
    let o = v.as_object().ok_or_else(|| invalid("hook_event_shape"))?;
    let text = |key: &str| {
        o.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("hook_missing_visible_field"))
    };
    if text("session_id")? != expected_origin_session
        || o.get("agent_id").is_some_and(|v| !v.is_null())
    {
        return Err(invalid("hook_session_or_agent_mismatch"));
    }
    let name = text("hook_event_name")?;
    if !host.events().contains(&name) {
        return Err(invalid("hook_event_not_enabled"));
    }
    if name == "Stop" && o.get("stop_hook_active").and_then(Value::as_bool) != Some(false) {
        return Err(invalid("hook_continuation_unverified"));
    }
    let turn = text(match host {
        CaptureHookHost::CodexCli => "turn_id",
        CaptureHookHost::ClaudeCodeCli => "prompt_id",
    })?;
    if !identifier(turn) {
        return Err(invalid("hook_stable_event_identity_required"));
    }
    let content = text(if name == "UserPromptSubmit" {
        "prompt"
    } else {
        "last_assistant_message"
    })?;
    if content.trim().is_empty() || content.len() > 128 * 1024 {
        return Err(invalid("hook_visible_text_bounds"));
    }
    let event_id = format!(
        "hook-{}",
        sha(&serde_json::to_vec(&(
            host,
            expected_origin_session,
            turn,
            name
        ))?)
    );
    Ok(CaptureRelayEvent {
        event_id,
        sequence,
        kind: if name == "UserPromptSubmit" {
            CaptureEventKind::UserPrompt
        } else {
            CaptureEventKind::AssistantResponse
        },
        content: content.into(),
        occurred_at: None,
        loss: CaptureLoss {
            truncated: false,
            redacted: false,
            partial_history: true,
        },
    })
}
