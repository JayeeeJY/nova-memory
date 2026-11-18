//! Local owner control plane, intentionally absent from MCP tool registration.
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use nova_core::{PrivacyLevel, QuerySelection, Vault, VaultKey};
use serde::Deserialize;
use std::path::PathBuf;
#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = ".nova/nova.sqlite")]
    vault: PathBuf,
    #[arg(long, default_value = ".nova/vault.key")]
    key_file: PathBuf,
    #[command(subcommand)]
    command: Command,
    #[arg(long, default_value = "codex")]
    client: String,
    #[arg(long, default_value = "owner:founder")]
    owner_id: String,
    #[arg(long, default_value = "personal")]
    space_id: String,
}
#[derive(Subcommand)]
enum Command {
    /// Generate a distinct owner-only client credential; never print its value.
    CreateClientToken {
        #[arg(long)]
        file: PathBuf,
    },
    /// Review this JSON request locally, then issue one exact-request, fixed-scope disclosure grant.
    Issue {
        #[arg(long)]
        request_file: PathBuf,
        #[arg(long)]
        acknowledge: bool,
    },
    Revoke {
        #[arg(long)]
        approval_id: String,
        #[arg(long)]
        acknowledge: bool,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalInput {
    request_id: String,
    purpose: String,
    selection: QuerySelection,
    allowed_ids: Vec<String>,
    maximum_privacy: PrivacyLevel,
    ttl_seconds: i64,
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.vault == cli.key_file {
        bail!("Vault and key paths must differ");
    }
    match cli.command {
        Command::CreateClientToken { file } => {
            if file == cli.vault || file == cli.key_file {
                bail!("credential must be a separate new file");
            }
            VaultKey::generate()?.write_new_file(&file)?;
            println!("Client credential created (owner-only; value omitted).");
        }
        Command::Issue {
            request_file,
            acknowledge,
        } => {
            if !acknowledge {
                bail!("review the local request and pass --acknowledge to approve this disclosure");
            }
            if std::fs::metadata(&request_file)?.len() > 32 * 1024 {
                bail!("approval request too large");
            }
            let input: ApprovalInput = serde_json::from_slice(&std::fs::read(&request_file)?)
                .context("invalid approval request")?;
            if input.request_id.trim().is_empty() || input.request_id.len() > 96 {
                bail!("invalid request_id");
            }
            let spec = nova_mcp::generic::query_spec_bound(
                &nova_mcp::registry::Principal {
                    owner_id: cli.owner_id.clone(),
                    space_id: cli.space_id.clone(),
                    client_id: cli.client.clone(),
                    capabilities: vec!["query".into()],
                }
                .binding()?,
                &input.request_id,
                &input.purpose,
                input.selection,
            );
            let mut vault = Vault::open(&cli.vault, VaultKey::read_file(&cli.key_file)?)?;
            let grant = vault.issue_disclosure_grant(
                spec,
                input.allowed_ids,
                input.maximum_privacy,
                input.ttl_seconds,
            )?;
            println!("{}", serde_json::to_string_pretty(&grant)?);
        }
        Command::Revoke {
            approval_id,
            acknowledge,
        } => {
            if !acknowledge {
                bail!("pass --acknowledge to revoke this approval");
            }
            let mut vault = Vault::open(&cli.vault, VaultKey::read_file(&cli.key_file)?)?;
            vault.revoke_disclosure_grant(&cli.owner_id, &approval_id)?;
            println!("Disclosure approval revoked.");
        }
    }
    Ok(())
}
