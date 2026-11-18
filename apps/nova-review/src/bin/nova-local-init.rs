//! Explicit, offline creation of an empty local AI Native profile. Unix permissions required.
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Create a new empty Nova Memory local profile (Unix only)")]
struct Cli {
    /// New absolute directory; its parent must exist. Existing directories are never reused.
    #[arg(long)]
    directory: PathBuf,
    #[arg(long, default_value = "owner:local")]
    owner_id: String,
    #[arg(long, default_value = "personal")]
    space_id: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    #[cfg(unix)]
    {
        unix::initialize(cli)
    }
    #[cfg(not(unix))]
    {
        let _ = cli;
        anyhow::bail!("nova-local-init requires Unix file permissions");
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::{Context, bail};
    use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::IntakeStore};
    use nova_mcp::registry::Principal;
    use serde::{Deserialize, Serialize};
    use std::{
        fs::{self, DirBuilder, OpenOptions},
        io::{ErrorKind, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        path::{Component, Path},
    };
    use uuid::Uuid;

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Profile {
        schema_version: String,
        profile_id: Uuid,
        owner_id: String,
        space_id: String,
        created_by: String,
    }

    /// Validate every existing path component without following symbolic links. A successful
    /// exclusive directory creation below is the ownership boundary; no existing profile opens.
    fn validate_destination(path: &Path) -> Result<()> {
        if !path.is_absolute()
            || path
                .components()
                .any(|p| matches!(p, Component::ParentDir | Component::CurDir))
        {
            bail!("directory must be an absolute path without traversal components");
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .context("directory must name a new child of an existing parent")?;
        let mut current = PathBuf::new();
        for component in parent.components() {
            current.push(component);
            let metadata =
                fs::symlink_metadata(&current).context("profile parent must already exist")?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("profile path components must be real directories, never symbolic links");
            }
        }
        match fs::symlink_metadata(path) {
            Ok(_) => {
                bail!("profile directory already exists; it will not be reused or overwritten")
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("cannot inspect profile destination"),
        }
    }

    pub(super) fn initialize(cli: Cli) -> Result<()> {
        Principal {
            owner_id: cli.owner_id.clone(),
            space_id: cli.space_id.clone(),
            client_id: "web-console".into(),
            capabilities: vec!["capture".into()],
        }
        .validate()
        .context("invalid local profile identity")?;
        validate_destination(&cli.directory)?;

        // Never create parents or retry an existing destination. If a later step fails the
        // incomplete directory is deliberately retained, without a completion marker.
        DirBuilder::new()
            .mode(0o700)
            .create(&cli.directory)
            .context("cannot exclusively create profile directory")?;
        fs::set_permissions(&cli.directory, fs::Permissions::from_mode(0o700))?;

        let vault_key = VaultKey::generate()?;
        let owner_key = VaultKey::generate()?;
        vault_key.write_new_file(&cli.directory.join("vault.key"))?;
        owner_key.write_new_file(&cli.directory.join("owner.key"))?;
        let legacy_path = cli.directory.join("legacy.sqlite");
        drop(Vault::create(&legacy_path, vault_key.clone())?);
        fs::set_permissions(&legacy_path, fs::Permissions::from_mode(0o600))?;

        let intake_path = cli.directory.join("intake.sqlite");
        let scope = AssetScope {
            owner_id: cli.owner_id.clone(),
            space: cli.space_id.clone(),
        };
        let mut intake = IntakeStore::create(&intake_path, vault_key, scope.clone())?;
        intake.enable_compilation(&scope)?;
        intake.enable_connections(&scope)?;
        // The core initializes ModelSelection::Unconfigured. No model, source, job,
        // connection credential, demo seed or network operation is created here.
        drop(intake);
        fs::set_permissions(&intake_path, fs::Permissions::from_mode(0o600))?;

        let profile = Profile {
            schema_version: "nova.local-profile.v0.1".into(),
            profile_id: Uuid::now_v7(),
            owner_id: cli.owner_id,
            space_id: cli.space_id,
            created_by: "nova-local-init".into(),
        };
        let metadata = serde_json::to_vec_pretty(&profile)?;
        // A durable marker is the last write; it contains only non-secret identity metadata.
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(cli.directory.join("profile.json"))?;
        marker.write_all(&metadata)?;
        marker.write_all(b"\n")?;
        marker.sync_all()?;
        fs::File::open(&cli.directory)?.sync_all()?;
        std::io::stdout().lock().write_all(&metadata)?;
        println!();
        Ok(())
    }
}
