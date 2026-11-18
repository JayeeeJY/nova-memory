use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};

use clap::Parser;
use nova_mcp::NovaMcpServer;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "nova-mcp",
    version,
    about = "Local-only Streamable HTTP MCP adapter for Nova Memory"
)]
struct Cli {
    /// Registered client ID (codex, chatgpt-generic, claude-code, cursor, or another ID). Legacy chatgpt requires --enable-legacy.
    #[arg(long, default_value = "codex")]
    client: String,
    #[arg(long)]
    token_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "token_file")]
    registry: Option<PathBuf>,
    #[arg(long)]
    enable_legacy: bool,
    #[arg(long, default_value = "owner:founder")]
    owner_id: String,
    #[arg(long, default_value = "personal")]
    space_id: String,
    #[arg(long, default_value = ".nova/nova.sqlite")]
    vault: PathBuf,
    #[arg(long, default_value = ".nova/vault.key")]
    key_file: PathBuf,
    #[arg(long, default_value = "127.0.0.1:3030")]
    bind: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    if !cli.bind.ip().is_loopback() {
        bail!(
            "nova-mcp P0 may bind only to loopback; use an authenticated secure tunnel for ChatGPT developer-mode testing"
        );
    }
    if cli.vault == cli.key_file {
        bail!("vault and key file paths must be different");
    }

    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    let address = listener.local_addr()?;
    let cancellation = CancellationToken::new();
    let legacy = cli.client == "chatgpt";
    if legacy && !cli.enable_legacy {
        bail!(
            "legacy chatgpt requires --enable-legacy and protected credentials; use chatgpt-generic for standard tools"
        );
    }
    if cli.enable_legacy && !legacy {
        bail!("--enable-legacy only applies to --client chatgpt");
    }
    let selected = if cli.client == "chatgpt-generic" {
        "chatgpt"
    } else {
        &cli.client
    };
    let (policy, binding) = if let Some(path) = cli.registry.as_deref() {
        let (credential, principal) =
            nova_mcp::registry::RegistryCredential::load(path, selected, &cli.key_file)
                .context("registry unavailable")?;
        (
            nova_mcp::transport::CredentialPolicy::Registry(credential),
            principal.binding()?,
        )
    } else {
        let token_path = cli
            .token_file
            .as_deref()
            .context("all modes require --token-file or --registry")?;
        if token_path == cli.key_file || token_path == cli.vault {
            bail!("credential must be separate from Vault and master key");
        }
        let credential =
            nova_mcp::transport::ClientCredential::read_for_vault(token_path, &cli.key_file)
                .context("client credential is not ready")?;
        let principal = nova_mcp::registry::Principal {
            owner_id: cli.owner_id.clone(),
            space_id: cli.space_id.clone(),
            client_id: selected.into(),
            capabilities: vec!["capture".into(), "query".into()],
        };
        (
            nova_mcp::transport::CredentialPolicy::Static(credential),
            principal.binding()?,
        )
    };
    let router = if legacy {
        if binding.owner() != "owner:founder"
            || binding.space() != "personal"
            || !binding.allows("capture")
            || !binding.allows("query")
        {
            bail!("legacy compatibility is restricted to original owner/space");
        }
        let server = NovaMcpServer::new(&cli.vault, &cli.key_file);
        server.verify_configuration().map_err(anyhow::Error::msg)?;
        nova_mcp::transport::protected_router(
            server,
            policy,
            address.to_string(),
            cancellation.child_token(),
        )
    } else {
        let server =
            nova_mcp::generic::GenericMcpServer::with_binding(&cli.vault, &cli.key_file, binding);
        server.verify_configuration().map_err(anyhow::Error::msg)?;
        nova_mcp::transport::protected_router(
            server,
            policy,
            address.to_string(),
            cancellation.child_token(),
        )
    };
    info!(address = %address, "Nova Memory MCP listening on loopback /mcp");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            cancellation.cancel();
        })
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
