use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ConfirmRequest,
    ContextRequest, ContextSurface, CorrectionRequest, DisputeRequest, MemoryType, NovaError,
    PrivacyLevel, ReviewTier, Vault, VaultKey,
};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "nova",
    version,
    about = "Nova Memory Phase 0 local trust-core CLI"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".nova/nova.sqlite")]
    vault: PathBuf,
    #[arg(long, global = true, default_value = ".nova/vault.key")]
    key_file: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new encrypted local Vault and user-held key file.
    Init,
    /// Save source evidence and create a reviewable proposal; never writes canonical memory.
    #[command(group(
        ArgGroup::new("source")
            .required(true)
            .args(["source_text", "source_file"])
    ))]
    Capture {
        /// Stable idempotency key supplied by the connector.
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value = "owner:founder")]
        owner: String,
        #[arg(long, default_value = "chatgpt")]
        source_platform: String,
        #[arg(long, value_enum, default_value_t = ChannelArg::RemoteMcp)]
        channel: ChannelArg,
        #[arg(long, value_enum, default_value_t = FidelityArg::F1AiDigest)]
        fidelity: FidelityArg,
        #[arg(long)]
        source_text: Option<String>,
        #[arg(long)]
        source_file: Option<PathBuf>,
        #[arg(long)]
        title: String,
        #[arg(long, value_enum, default_value_t = MemoryTypeArg::Decision)]
        memory_type: MemoryTypeArg,
        #[arg(long)]
        summary: String,
        /// Confirmed decisions extracted into this proposal. Repeat for multiple decisions.
        #[arg(long = "decision")]
        decisions: Vec<String>,
        #[arg(long, value_enum, default_value_t = PrivacyArg::Private)]
        privacy: PrivacyArg,
        #[arg(long, value_enum, default_value_t = OriginArg::AiExtractedProposal)]
        origin: OriginArg,
        #[arg(long, value_enum, default_value_t = ReviewArg::Routine)]
        review_tier: ReviewArg,
        #[arg(long = "permission", default_value = "personal_review")]
        permission_scope: Vec<String>,
        #[arg(long = "purpose-limit")]
        purpose_limits: Vec<String>,
        #[arg(long)]
        observed_at: Option<String>,
    },
    /// List proposals that still require explicit user confirmation.
    Inbox,
    /// Confirm one proposal and create canonical memory.
    Confirm {
        proposal_id: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value = "owner:founder")]
        subject: String,
    },
    /// Correct memory by creating a new version and superseding the old version.
    Correct {
        memory_id: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value = "owner:founder")]
        subject: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        reason: String,
    },
    /// Mark active memory as disputed without rewriting epistemic provenance.
    Dispute {
        memory_id: String,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value = "owner:founder")]
        subject: String,
        #[arg(long)]
        reason: String,
    },
    /// List canonical history, including disputed and superseded versions.
    List,
    /// Build and deliver a minimum-summary Context Pack.
    Context {
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value = "owner:founder")]
        subject: String,
        #[arg(long, default_value = "chatgpt")]
        recipient: String,
        #[arg(long)]
        purpose: String,
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, value_enum, default_value_t = ContextSurfaceArg::ChatText)]
        surface: ContextSurfaceArg,
        /// Restrict disclosure to these memory types. Repeat to allow multiple types.
        #[arg(long = "allowed-type")]
        allowed_memory_types: Vec<MemoryTypeArg>,
        #[arg(long, value_enum, default_value_t = PrivacyArg::Private)]
        maximum_privacy: PrivacyArg,
        #[arg(long, default_value_t = 5)]
        max_items: usize,
        #[arg(long, default_value_t = 600)]
        expires_in_seconds: i64,
        /// Explicitly authorize this one delivery. Without it the request is denied.
        #[arg(long)]
        approve: bool,
    },
    /// Delete canonical memory, its proposal key, and dependent Context Pack keys.
    Delete {
        memory_id: String,
        #[arg(long, default_value = "owner:founder")]
        subject: String,
    },
    /// Verify the append-only encrypted audit hash chain.
    Verify,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ChannelArg {
    RemoteMcp,
    ShareSheet,
    WebAdapter,
    QuickCapture,
    OfficialExport,
}

impl From<ChannelArg> for CaptureChannel {
    fn from(value: ChannelArg) -> Self {
        match value {
            ChannelArg::RemoteMcp => Self::RemoteMcp,
            ChannelArg::ShareSheet => Self::ShareSheet,
            ChannelArg::WebAdapter => Self::WebAdapter,
            ChannelArg::QuickCapture => Self::QuickCapture,
            ChannelArg::OfficialExport => Self::OfficialExport,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FidelityArg {
    F1AiDigest,
    F2VisibleTranscript,
    F3RawAudio,
}

impl From<FidelityArg> for CaptureFidelity {
    fn from(value: FidelityArg) -> Self {
        match value {
            FidelityArg::F1AiDigest => Self::F1AiDigest,
            FidelityArg::F2VisibleTranscript => Self::F2VisibleTranscript,
            FidelityArg::F3RawAudio => Self::F3RawAudio,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ContextSurfaceArg {
    ChatText,
    VoiceAudio,
    DesktopReview,
}

impl From<ContextSurfaceArg> for ContextSurface {
    fn from(value: ContextSurfaceArg) -> Self {
        match value {
            ContextSurfaceArg::ChatText => Self::ChatText,
            ContextSurfaceArg::VoiceAudio => Self::VoiceAudio,
            ContextSurfaceArg::DesktopReview => Self::DesktopReview,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MemoryTypeArg {
    Fact,
    Preference,
    Decision,
    Goal,
    Commitment,
    Event,
    Lesson,
    Reflection,
}

impl From<MemoryTypeArg> for MemoryType {
    fn from(value: MemoryTypeArg) -> Self {
        match value {
            MemoryTypeArg::Fact => Self::Fact,
            MemoryTypeArg::Preference => Self::Preference,
            MemoryTypeArg::Decision => Self::Decision,
            MemoryTypeArg::Goal => Self::Goal,
            MemoryTypeArg::Commitment => Self::Commitment,
            MemoryTypeArg::Event => Self::Event,
            MemoryTypeArg::Lesson => Self::Lesson,
            MemoryTypeArg::Reflection => Self::Reflection,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PrivacyArg {
    Shareable,
    Private,
    Sensitive,
    Sealed,
}

impl From<PrivacyArg> for PrivacyLevel {
    fn from(value: PrivacyArg) -> Self {
        match value {
            PrivacyArg::Shareable => Self::Shareable,
            PrivacyArg::Private => Self::Private,
            PrivacyArg::Sensitive => Self::Sensitive,
            PrivacyArg::Sealed => Self::Sealed,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OriginArg {
    UserSelfReport,
    AiExtractedProposal,
    ModelInferred,
}

impl From<OriginArg> for AssertionOrigin {
    fn from(value: OriginArg) -> Self {
        match value {
            OriginArg::UserSelfReport => Self::UserSelfReport,
            OriginArg::AiExtractedProposal => Self::AiExtractedProposal,
            OriginArg::ModelInferred => Self::ModelInferred,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReviewArg {
    Routine,
    Attention,
    Restricted,
}

impl From<ReviewArg> for ReviewTier {
    fn from(value: ReviewArg) -> Self {
        match value {
            ReviewArg::Routine => Self::Routine,
            ReviewArg::Attention => Self::Attention,
            ReviewArg::Restricted => Self::Restricted,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        let (code, retryable) = error
            .downcast_ref::<NovaError>()
            .map(|error| (error.code(), error.retryable()))
            .unwrap_or(("NOVA_CLI_FAILED", false));
        eprintln!(
            "{}",
            json!({
                "error": {
                    "code": code,
                    "message": error.to_string(),
                    "retryable": retryable
                }
            })
        );
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => initialize(&cli.vault, &cli.key_file),
        Command::Capture {
            request_id,
            owner,
            source_platform,
            channel,
            fidelity,
            source_text,
            source_file,
            title,
            memory_type,
            summary,
            decisions,
            privacy,
            origin,
            review_tier,
            permission_scope,
            purpose_limits,
            observed_at,
        } => {
            let source_content = match (source_text, source_file) {
                (Some(text), None) => text,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
                _ => bail!("provide exactly one of --source-text or --source-file"),
            };
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            let outcome = vault.capture(CaptureRequest {
                structure: None,
                request_id,
                owner_id: owner,
                source_platform,
                capture_channel: channel.into(),
                capture_fidelity: fidelity.into(),
                source_content,
                title,
                memory_type: memory_type.into(),
                summary,
                decisions,
                privacy_level: privacy.into(),
                proposed_origin: origin.into(),
                review_tier: review_tier.into(),
                permission_scope,
                purpose_limits,
                observed_at,
            })?;
            print_json(&json!({
                "source": {
                    "source_id": outcome.source.source_id,
                    "source_platform": outcome.source.source_platform,
                    "capture_channel": outcome.source.capture_channel,
                    "capture_fidelity": outcome.source.capture_fidelity,
                    "observed_at": outcome.source.observed_at,
                    "content_hash": outcome.source.content_hash
                },
                "proposal": outcome.proposal,
                "receipt": outcome.receipt
            }))
        }
        Command::Inbox => {
            let vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.pending_proposals()?)
        }
        Command::Confirm {
            proposal_id,
            request_id,
            subject,
        } => {
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.confirm_proposal(ConfirmRequest {
                request_id,
                subject,
                proposal_id,
            })?)
        }
        Command::Correct {
            memory_id,
            request_id,
            subject,
            title,
            summary,
            reason,
        } => {
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            let outcome = vault.correct_memory(CorrectionRequest {
                request_id,
                subject,
                memory_id,
                title,
                summary,
                reason,
            })?;
            print_json(&json!({
                "previous_memory": outcome.previous_memory,
                "memory": outcome.memory,
                "source": {
                    "source_id": outcome.source.source_id,
                    "source_platform": outcome.source.source_platform,
                    "capture_channel": outcome.source.capture_channel,
                    "capture_fidelity": outcome.source.capture_fidelity,
                    "observed_at": outcome.source.observed_at,
                    "content_hash": outcome.source.content_hash
                },
                "receipt": outcome.receipt
            }))
        }
        Command::Dispute {
            memory_id,
            request_id,
            subject,
            reason,
        } => {
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            let outcome = vault.dispute_memory(DisputeRequest {
                request_id,
                subject,
                memory_id,
                reason,
            })?;
            print_json(&json!({
                "memory": outcome.memory,
                "source": {
                    "source_id": outcome.source.source_id,
                    "source_platform": outcome.source.source_platform,
                    "capture_channel": outcome.source.capture_channel,
                    "capture_fidelity": outcome.source.capture_fidelity,
                    "observed_at": outcome.source.observed_at,
                    "content_hash": outcome.source.content_hash
                },
                "receipt": outcome.receipt
            }))
        }
        Command::List => {
            let vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.memories()?)
        }
        Command::Context {
            request_id,
            subject,
            recipient,
            purpose,
            query,
            surface,
            allowed_memory_types,
            maximum_privacy,
            max_items,
            expires_in_seconds,
            approve,
        } => {
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.prepare_context(ContextRequest {
                request_id,
                subject,
                recipient,
                purpose,
                query,
                surface: surface.into(),
                allowed_memory_types: allowed_memory_types.into_iter().map(Into::into).collect(),
                maximum_privacy_level: maximum_privacy.into(),
                max_items,
                expires_in_seconds,
                user_approved: approve,
            })?)
        }
        Command::Delete { memory_id, subject } => {
            let mut vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.delete_memory(&subject, &memory_id)?)
        }
        Command::Verify => {
            let vault = open_vault(&cli.vault, &cli.key_file)?;
            print_json(&vault.verify_audit_chain()?)
        }
    }
}

fn initialize(vault_path: &Path, key_path: &Path) -> Result<()> {
    if vault_path.exists() {
        return Err(NovaError::VaultAlreadyExists(vault_path.display().to_string()).into());
    }
    if key_path.exists() {
        return Err(NovaError::Validation(format!(
            "key file already exists: {}",
            key_path.display()
        ))
        .into());
    }
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = VaultKey::generate()?;
    key.write_new_file(key_path)?;
    Vault::create(vault_path, key)?;
    print_json(&json!({
        "status": "initialized",
        "vault": vault_path,
        "key_file": key_path,
        "warning": "Keep the key file private and backed up separately. Phase 0 has no account-based key recovery."
    }))
}

fn open_vault(vault_path: &Path, key_path: &Path) -> Result<Vault> {
    let key = VaultKey::read_file(key_path)
        .with_context(|| format!("failed to load key file {}", key_path.display()))?;
    Vault::open(vault_path, key)
        .with_context(|| format!("failed to open vault {}", vault_path.display()))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
