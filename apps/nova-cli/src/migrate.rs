use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use nova_core::{MigrationPackage, Vault, VaultKey};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Parser)]
#[command(
    name = "nova-migrate",
    about = "Owner-only encrypted same-owner recovery; no client credentials are carried"
)]
struct Cli {
    #[arg(long, default_value = "owner:founder")]
    owner: String,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    AccessSnapshot {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        memory_id: String,
    },
    AuthorizeClient {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        memory_id: String,
        #[arg(long)]
        client: String,
        #[arg(long)]
        base_snapshot: String,
        #[arg(long)]
        acknowledge: bool,
    },
    Keygen {
        #[arg(long)]
        package_key: PathBuf,
    },
    Export {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        package_key: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        acknowledge: bool,
    },
    Inspect {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        package_key: PathBuf,
    },
    /// Validate the package and current empty/replayed target; do not import records.
    Preflight {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        package_key: PathBuf,
    },
    Import {
        #[arg(long)]
        vault: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        package_key: PathBuf,
        #[arg(long)]
        acknowledge: bool,
    },
}
fn key(path: &Path) -> Result<VaultKey> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 128 {
        bail!("key must be a small regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("key must be owner-only");
        }
    }
    Ok(VaultKey::read_file(path)?)
}
fn package(path: &Path) -> Result<Vec<u8>> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        bail!("package must be a regular file");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        bail!("package exceeds size limit");
    }
    Ok(bytes)
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Preflight {
            vault,
            key_file,
            package: input,
            package_key,
        } => {
            let report = Vault::open_readonly(&vault, key(&key_file)?)?
                .preflight_import_encrypted(&package(&input)?, &key(&package_key)?, &cli.owner)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "preflight_passed", "import_executed": false, "report": report
                }))?
            );
        }
        Command::AccessSnapshot {
            vault,
            key_file,
            memory_id,
        } => {
            println!(
                "{}",
                Vault::open(&vault, key(&key_file)?)?
                    .memory_access_snapshot(&cli.owner, &memory_id)?
            );
        }
        Command::AuthorizeClient {
            vault,
            key_file,
            memory_id,
            client,
            base_snapshot,
            acknowledge,
        } => {
            if !acknowledge {
                bail!("client scope approval requires --acknowledge");
            }
            Vault::open(&vault, key(&key_file)?)?.authorize_memory_client(
                &cli.owner,
                &memory_id,
                &client,
                &base_snapshot,
            )?;
            println!(
                "Client scope approved; every read still requires an exact-request disclosure grant."
            );
        }
        Command::Keygen { package_key } => {
            VaultKey::generate()?.write_new_file(&package_key)?;
            println!("Independent package key created; value omitted.");
        }
        Command::Export {
            vault,
            key_file,
            package_key,
            output,
            acknowledge,
        } => {
            if !acknowledge {
                bail!("export requires --acknowledge");
            }
            let bytes = Vault::open(&vault, key(&key_file)?)?
                .export_encrypted(&cli.owner, &key(&package_key)?)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(output)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            println!(
                "Encrypted package written; sources and history included, credentials excluded."
            );
        }
        Command::Inspect {
            package: input,
            package_key,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&MigrationPackage::inspect_encrypted(
                    &package(&input)?,
                    &key(&package_key)?,
                    &cli.owner
                )?)?
            );
        }
        Command::Import {
            vault,
            key_file,
            package: input,
            package_key,
            acknowledge,
        } => {
            if !acknowledge {
                bail!("import requires --acknowledge");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Vault::open(&vault, key(&key_file)?)?.import_encrypted(
                        &package(&input)?,
                        &key(&package_key)?,
                        &cli.owner
                    )?
                )?
            );
        }
    }
    Ok(())
}
