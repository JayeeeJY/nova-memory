//! Only new temporary profiles are initialized. No personal Vault or credential is opened.
#![cfg(unix)]

use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::*};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn command(path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nova-local-init"));
    command.arg("--directory").arg(path);
    command
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    // macOS /var and /tmp may themselves be symbolic links. The initializer intentionally
    // requires their real absolute path, rather than silently following either alias.
    let real = directory.path().canonicalize().unwrap();
    (directory, real)
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn first_profile_is_empty_unconfigured_private_and_uses_independent_credentials() {
    let (_directory, root) = root();
    let profile = root.join("new-profile");
    let output = command(&profile).output().unwrap();
    let metadata = success(output);
    let marker: Value =
        serde_json::from_slice(&fs::read(profile.join("profile.json")).unwrap()).unwrap();
    assert_eq!(metadata, marker);
    assert_eq!(metadata.as_object().unwrap().len(), 5);
    assert_eq!(metadata["schema_version"], "nova.local-profile.v0.1");
    assert_eq!(metadata["created_by"], "nova-local-init");
    assert_eq!(metadata["owner_id"], "owner:local");
    assert_eq!(metadata["space_id"], "personal");
    let id = metadata["profile_id"].as_str().unwrap();
    assert_eq!(uuid::Uuid::parse_str(id).unwrap().get_version_num(), 7);
    assert!(!metadata.to_string().contains(profile.to_str().unwrap()));

    assert_eq!(
        fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let mut files = fs::read_dir(&profile)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(
        files,
        [
            "intake.sqlite",
            "legacy.sqlite",
            "owner.key",
            "profile.json",
            "vault.key"
        ]
    );
    for file in &files {
        assert_eq!(
            fs::metadata(profile.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let vault_secret = fs::read_to_string(profile.join("vault.key")).unwrap();
    let owner_secret = fs::read_to_string(profile.join("owner.key")).unwrap();
    assert_ne!(vault_secret, owner_secret);
    assert!(!metadata.to_string().contains(vault_secret.trim()));
    assert!(!metadata.to_string().contains(owner_secret.trim()));
    let key = VaultKey::from_hex(&vault_secret).unwrap();
    let owner_key = VaultKey::from_hex(&owner_secret).unwrap();
    let legacy = Vault::open_readonly(&profile.join("legacy.sqlite"), key.clone()).unwrap();
    assert!(legacy.memories().unwrap().is_empty());
    assert!(legacy.pending_proposals().unwrap().is_empty());
    assert!(Vault::open_readonly(&profile.join("legacy.sqlite"), owner_key.clone()).is_err());
    let scope = AssetScope {
        owner_id: "owner:local".into(),
        space: "personal".into(),
    };
    assert!(IntakeStore::open(&profile.join("intake.sqlite"), owner_key, scope.clone()).is_err());
    let store = IntakeStore::open(&profile.join("intake.sqlite"), key, scope.clone()).unwrap();
    assert!(store.compilation_enabled().unwrap());
    assert_eq!(store.configuration().unwrap().revision, 1);
    assert_eq!(
        store.configuration().unwrap().selection,
        ModelSelection::Unconfigured
    );
    assert!(store.jobs(&scope, None).unwrap().is_empty());
    assert!(store.compiled_units(&scope, None).unwrap().is_empty());
    assert!(store.connections(&scope).unwrap().is_empty());
    let report = store.construction_report(&scope).unwrap();
    assert_eq!(report.construction.sources, 0);
    assert_eq!(report.construction.extraction_records, 0);
    assert_eq!(report.construction.exact_memories, 0);
    assert_eq!(report.usage.configured_connections, 0);
    assert!(report.unmeasured.capability_level.is_none());
    assert!(!profile.join("SYNTHETIC_M1_FIXTURE").exists());
    let raw = fs::read(profile.join("intake.sqlite")).unwrap();
    assert!(
        !raw.windows(b"owner:local".len())
            .any(|b| b == b"owner:local")
    );
}

#[test]
fn custom_identity_is_bound_and_connection_feature_is_ready_without_a_preissued_token() {
    let (_directory, root) = root();
    let path = root.join("custom");
    let marker = success(
        command(&path)
            .args(["--owner-id", "owner:test-42", "--space-id", "work.test"])
            .output()
            .unwrap(),
    );
    assert_eq!(marker["owner_id"], "owner:test-42");
    assert_eq!(marker["space_id"], "work.test");
    let scope = AssetScope {
        owner_id: "owner:test-42".into(),
        space: "work.test".into(),
    };
    let key = VaultKey::read_file(&path.join("vault.key")).unwrap();
    assert!(
        IntakeStore::open(
            &path.join("intake.sqlite"),
            key.clone(),
            AssetScope {
                owner_id: "owner:local".into(),
                space: "work.test".into()
            }
        )
        .is_err()
    );
    let mut intake = IntakeStore::open(&path.join("intake.sqlite"), key, scope.clone()).unwrap();
    assert!(intake.connections(&scope).unwrap().is_empty());
    // This test's explicit subsequent request verifies the feature's schema. The initializer
    // itself never issues a connection; current-only reads fail honestly for an empty library.
    let error = intake
        .create_connection(
            &scope,
            ConnectionRequest {
                request_id: "empty-check".into(),
                label: "Empty current library".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("connection_empty_snapshot"));
    assert!(!error.contains("connections_not_enabled"));
    assert!(intake.connections(&scope).unwrap().is_empty());
    let other = success(command(&root.join("other")).output().unwrap());
    assert_ne!(other["profile_id"], marker["profile_id"]);
}

#[test]
fn existing_empty_populated_and_partial_directories_are_never_overwritten() {
    let (_directory, root) = root();
    for name in ["empty", "populated", "partial"] {
        let path = root.join(name);
        fs::create_dir(&path).unwrap();
        if name != "empty" {
            fs::write(
                path.join("vault.key"),
                "preexisting sentinel, not a credential",
            )
            .unwrap();
        }
        let before = fs::read_dir(&path).unwrap().count();
        failure(command(&path).output().unwrap());
        assert_eq!(fs::read_dir(&path).unwrap().count(), before);
        assert!(!path.join("profile.json").exists());
        if name != "empty" {
            assert_eq!(
                fs::read_to_string(path.join("vault.key")).unwrap(),
                "preexisting sentinel, not a credential"
            );
        }
    }
    let completed = root.join("completed");
    success(command(&completed).output().unwrap());
    let before = [
        "vault.key",
        "owner.key",
        "profile.json",
        "legacy.sqlite",
        "intake.sqlite",
    ]
    .map(|name| (name, fs::read(completed.join(name)).unwrap()));
    failure(command(&completed).output().unwrap());
    for (name, bytes) in before {
        assert_eq!(fs::read(completed.join(name)).unwrap(), bytes);
    }
}

#[test]
fn interrupted_file_creation_retains_a_partial_directory_and_refuses_retry() {
    let (_directory, root) = root();
    let path = root.join("interrupted");
    // A child-only OS file-size limit interrupts the first key write after exclusive mkdir.
    // Arguments remain positional (including spaces), without shell interpolation or a
    // product-only fault injection switch. Disable child core dumps before provoking it.
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "ulimit -c 0; ulimit -f 0; exec \"$@\"",
            "nova-init-interruption-test",
        ])
        .arg(env!("CARGO_BIN_EXE_nova-local-init"))
        .arg("--directory")
        .arg(&path)
        .output()
        .unwrap();
    failure(output);
    assert!(path.is_dir());
    assert!(!path.join("profile.json").exists());
    let entries = fs::read_dir(&path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        })
        .collect::<Vec<_>>();
    assert!(!entries.is_empty());
    failure(command(&path).output().unwrap());
    assert_eq!(fs::read_dir(&path).unwrap().count(), entries.len());
    for (name, bytes) in entries {
        assert_eq!(fs::read(path.join(name)).unwrap(), bytes);
    }
}

#[test]
fn relative_traversal_and_missing_or_file_parents_fail_before_creation() {
    let (_directory, root) = root();
    failure(
        command(Path::new("relative-profile"))
            .current_dir(&root)
            .output()
            .unwrap(),
    );
    assert!(!root.join("relative-profile").exists());
    let traversal = root.join("child/../escaped");
    failure(command(&traversal).output().unwrap());
    assert!(!root.join("escaped").exists());
    failure(
        command(&root.join("missing-parent/profile"))
            .output()
            .unwrap(),
    );
    assert!(!root.join("missing-parent").exists());
    fs::write(root.join("file-parent"), "sentinel").unwrap();
    failure(command(&root.join("file-parent/profile")).output().unwrap());
    assert_eq!(fs::read(root.join("file-parent")).unwrap(), b"sentinel");
}

#[test]
fn symlink_components_and_final_or_dangling_symlinks_are_rejected() {
    let (_directory, root) = root();
    fs::create_dir(root.join("real-parent")).unwrap();
    symlink(root.join("real-parent"), root.join("linked-parent")).unwrap();
    failure(
        command(&root.join("linked-parent/profile"))
            .output()
            .unwrap(),
    );
    assert!(!root.join("real-parent/profile").exists());
    fs::create_dir(root.join("real-profile")).unwrap();
    fs::write(root.join("real-profile/keep"), "keep").unwrap();
    symlink(root.join("real-profile"), root.join("final-link")).unwrap();
    failure(command(&root.join("final-link")).output().unwrap());
    assert_eq!(fs::read(root.join("real-profile/keep")).unwrap(), b"keep");
    symlink(root.join("absent"), root.join("dangling-link")).unwrap();
    failure(command(&root.join("dangling-link")).output().unwrap());
    assert!(!root.join("absent").exists());
}

#[test]
fn invalid_owner_or_space_is_rejected_without_creating_a_directory() {
    let (_directory, root) = root();
    for (index, (flag, value)) in [
        ("--owner-id", "owner with spaces"),
        ("--owner-id", ""),
        ("--owner-id", "owner/escape"),
        ("--owner-id", "owner\nline"),
        ("--space-id", "../space"),
        ("--space-id", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("invalid-{index}"));
        failure(command(&path).args([flag, value]).output().unwrap());
        assert!(!path.exists());
    }
    let path = root.join("too-long");
    failure(
        command(&path)
            .args(["--owner-id", &"a".repeat(129)])
            .output()
            .unwrap(),
    );
    assert!(!path.exists());
}

#[test]
fn racing_initializers_have_one_winner_and_leave_one_complete_profile() {
    let (_directory, root) = root();
    let path = root.join("race");
    let first = command(&path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = command(&path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let one = first.wait_with_output().unwrap();
    let two = second.wait_with_output().unwrap();
    assert_ne!(one.status.success(), two.status.success());
    let metadata = if one.status.success() {
        failure(two);
        success(one)
    } else {
        failure(one);
        success(two)
    };
    let marker: Value =
        serde_json::from_slice(&fs::read(path.join("profile.json")).unwrap()).unwrap();
    assert_eq!(metadata, marker);
    assert_eq!(fs::read_dir(path).unwrap().count(), 5);
}
