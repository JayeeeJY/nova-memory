use anyhow::{Result, bail};
use clap::Parser;
use nova_core::{
    AssertionOrigin, CaptureChannel, CaptureFidelity, CaptureRequest, ConfirmRequest,
    CorrectionRequest, DisputeRequest, MemoryStatus, MemoryType, PrivacyLevel, QuerySelection,
    QuerySpec, ReviewTier, Vault, VaultKey,
};
use nova_review::ReviewServer;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(about = "Local owner-only Nova Memory Review Inbox")]
struct Cli {
    #[arg(long, default_value = ".nova/nova.sqlite")]
    vault: PathBuf,
    #[arg(long, default_value = ".nova/vault.key")]
    key_file: PathBuf,
    #[arg(long, default_value = "127.0.0.1:3031")]
    bind: SocketAddr,
    /// Use only temporary synthetic data; never opens the configured real Vault.
    #[arg(long)]
    demo: bool,
    #[arg(long)]
    owner_token_file: Option<PathBuf>,
    #[arg(long)]
    registry: Option<PathBuf>,
    /// Public asset directory of the shared website; enables /app/.
    #[arg(long)]
    site_dir: Option<PathBuf>,
    /// Existing independent automatic-intake store. Never initialized or migrated at startup.
    #[arg(long)]
    intake_store: Option<PathBuf>,
    /// Enable the N1 capture page with a temporary synthetic intake store.
    #[arg(long, requires = "demo", conflicts_with = "intake_store")]
    demo_intake: bool,
    #[arg(long, default_value = "owner:founder")]
    owner_id: String,
    /// Explicitly enable automatic processing of the selected intake store.
    #[arg(long)]
    enable_compiler: bool,
    /// Explicitly use the experimental schema-constrained streaming path for a local model.
    #[arg(
        long,
        requires = "enable_compiler",
        conflicts_with = "model_credential_file"
    )]
    structured_local_output: bool,
    /// Opt in to the v0.3 four-entry workspace and separately authenticated automatic MCP.
    #[arg(long)]
    workspace_v03: bool,
    /// Public child identity for a local supervisor; never an authorization token.
    #[arg(long, requires = "workspace_v03")]
    local_runtime_instance: Option<String>,
    /// Enable fixed-instance, capture-only ingress; Owner must still issue each scoped grant.
    #[arg(long,requires="workspace_v03")]
    enable_capture_ingress: bool,
    /// Enable owner-managed sessions and opt-in handoff/brief/recap tools.
    #[arg(long,requires="workspace_v03")]
    enable_continuity: bool,
    /// Add a separate local lexical/CJK search tool, retaining legacy substring search.
    #[arg(long,requires="workspace_v03")]
    enable_ranked_retrieval: bool,
    /// Expose temporal reads; every recipient still needs separate Owner history permission.
    #[arg(long,requires="workspace_v03")]
    enable_historical_retrieval: bool,
    #[arg(long, default_value_t = 100)]
    compiler_max_calls: u64,
    #[arg(long, requires_all = ["enable_compiler", "model_credential_origin"])]
    model_credential_file: Option<PathBuf>,
    #[arg(long, requires = "model_credential_file")]
    model_credential_origin: Option<String>,
    #[arg(long, default_value = "personal")]
    space_id: String,
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.bind.ip().is_loopback() {
        bail!("Review Inbox must bind to loopback only; do not tunnel this owner control plane");
    }
    let demo_directory = if cli.demo {
        Some(tempfile::tempdir()?)
    } else {
        None
    };
    let (vault_path, key_path) = if let Some(directory) = &demo_directory {
        let vault_path = directory.path().join("demo.sqlite");
        let key_path = directory.path().join("demo.key");
        let key = VaultKey::generate()?;
        key.write_new_file(&key_path)?;
        let mut vault = Vault::create(&vault_path, key)?;
        let mut decision_template = None;
        for (title, memory_type) in [
            ("决定与依据", MemoryType::Decision),
            ("事实与来源", MemoryType::Fact),
            ("个人偏好", MemoryType::Preference),
            ("目标与约束", MemoryType::Goal),
            ("经验与复盘", MemoryType::Lesson),
            ("经历记录", MemoryType::Event),
        ] {
            let template: nova_core::StructureTemplate = serde_json::from_value(
                serde_json::json!({
                    "template_id":"","owner_id":cli.owner_id,"revision":0,"title":title,
                    "memory_type":memory_type,"privacy_level":"private",
                    "guidance":"合成示例：从原文选出依据，不把推测写成事实。",
                    "fields":[{"id":"rationale","label":"依据","kind":"text","required":true,"source_required":true,"max_length":500}]
                }),
            )?;
            let saved = vault.put_template(&cli.owner_id, template, None)?;
            if memory_type == MemoryType::Decision {
                decision_template = Some(saved);
            }
        }
        for (index, title) in ["可信记忆闭环", "待核实的项目约束", "下次复盘", "待审核决定"]
            .into_iter()
            .enumerate()
        {
            let captured = vault.capture(CaptureRequest {
                structure: if index == 0 { Some(nova_core::StructureInput {
                    template_id: decision_template.as_ref().unwrap().template_id.clone(), template_revision: 1,
                    values: [("rationale".into(), serde_json::json!("先验证长期效用"))].into_iter().collect(),
                    evidence: [("rationale".into(), "先验证长期效用".into())].into_iter().collect(),
                }) } else { None },
                request_id: Some(format!("demo_capture_{index}")), owner_id: cli.owner_id.clone(),
                source_platform: "chatgpt".into(), capture_channel: CaptureChannel::RemoteMcp,
                capture_fidelity: CaptureFidelity::F1AiDigest,
                source_content: serde_json::json!({"schema_version":"nova.session_digest.v0.2", "host":"chatgpt",
                    "surface":"chat_text", "capture_fidelity":"f1_ai_digest", "source_ref": null,
                    "session_digest":"用户决定先验证保存、审核与复用。理由是先验证效用。暂不扩展第二 Host。",
                    "decisions":["先验证可信记忆闭环"], "rationales":["先验证长期效用"],
                    "open_questions":["后台 Voice 是否可重复"], "next_steps":["审核待确认草稿"]}).to_string(),
                title: title.into(), memory_type: MemoryType::Decision,
                summary: "先验证保存、审核与复用，暂不扩展第二 Host。".into(),
                decisions: vec!["先验证可信记忆闭环".into()], privacy_level: PrivacyLevel::Private,
                proposed_origin: if index == 2 { AssertionOrigin::ModelInferred } else { AssertionOrigin::AiExtractedProposal },
                review_tier: ReviewTier::Attention, permission_scope: vec!["personal_review".into()],
                purpose_limits: vec!["personal_memory_continuity".into()], observed_at: None,
            })?;
            if index < 2 {
                let confirmed = vault.confirm_proposal(ConfirmRequest {
                    request_id: Some(format!("demo_confirm_{index}")),
                    subject: cli.owner_id.clone(),
                    proposal_id: captured.proposal.proposal_id,
                })?;
                if index == 1 {
                    vault.dispute_memory(DisputeRequest {
                        request_id: Some("demo_dispute".into()),
                        subject: cli.owner_id.clone(),
                        memory_id: confirmed.memory.memory_id,
                        reason: "合成演示：此决定仍待核实".into(),
                    })?;
                } else {
                    vault.correct_memory(CorrectionRequest {
                        request_id: Some("demo_correction".into()),
                        subject: cli.owner_id.clone(),
                        memory_id: confirmed.memory.memory_id,
                        title: Some("可信记忆闭环 · 已复核".into()),
                        summary: "先验证保存、人工审核与获准复用。".into(),
                        reason: "合成演示：保留旧版本并明确复用需要授权".into(),
                    })?;
                }
            }
        }
        // Synthetic deletion demonstrates the recovery boundary without touching real data.
        let deleted_proposal = vault.pending_proposals()?.pop().unwrap();
        let deleted_memory = vault
            .confirm_proposal(ConfirmRequest {
                request_id: Some("demo_delete_confirm".into()),
                subject: cli.owner_id.clone(),
                proposal_id: deleted_proposal.proposal_id,
            })?
            .memory;
        vault.delete_memory(&cli.owner_id, &deleted_memory.memory_id)?;
        let memory = vault
            .memories()?
            .into_iter()
            .find(|m| m.status == MemoryStatus::Active)
            .unwrap();
        for index in 0..2 {
            let spec = QuerySpec {
                subject: cli.owner_id.clone(),
                client: "chatgpt".into(),
                space: cli.space_id.clone(),
                request_id: format!("demo_get_{index}"),
                purpose: "personal_memory_continuity".into(),
                selection: QuerySelection::Get {
                    memory_id: memory.memory_id.clone(),
                    capture_status: false,
                },
            };
            let grant = vault.issue_disclosure_grant(
                spec.clone(),
                vec![memory.memory_id.clone()],
                PrivacyLevel::Private,
                3600,
            )?;
            if index == 0 {
                vault.query_memory(spec, &grant.approval_id)?;
            }
        }
        (vault_path, key_path)
    } else {
        (cli.vault, cli.key_file)
    };
    let demo_registry = if let Some(directory) = &demo_directory {
        let path = directory.path().join("clients.json");
        std::fs::write(
            &path,
            serde_json::json!({"schema_version":"nova.clients.v0.1","clients":[]}).to_string(),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Some(path)
    } else {
        None
    };
    if vault_path == key_path {
        bail!("Vault and key paths must differ");
    }
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    let address = listener.local_addr()?;
    let principal = nova_mcp::registry::Principal {
        owner_id: cli.owner_id.clone(),
        space_id: cli.space_id.clone(),
        client_id: "web-console".into(),
        capabilities: vec!["capture".into()],
    };
    principal.validate()?;
    let synthetic = cli.demo
        || vault_path
            .parent()
            .is_some_and(|p| p.join("SYNTHETIC_M1_FIXTURE").is_file());
    let server_key_path = key_path.clone();
    let server_owner = cli.owner_id.clone();
    let server_space = cli.space_id.clone();
    let mut server = ReviewServer::new(vault_path, key_path, address.to_string(), synthetic)
        .with_owner(cli.owner_id, cli.space_id)
        .with_temporary_demo(cli.demo);
    if let Some(path) = demo_registry.or(cli.registry) {
        server = server.with_registry(path);
    }
    if let Some(path) = cli.site_dir {
        server = server.with_site_dir(&path)?;
    }
    if let Some(path) = cli.owner_token_file {
        server = server.with_owner_token(&path)?;
    } else if !cli.demo {
        bail!(
            "real Console requires --owner-token-file; use an independent owner credential, never a client token"
        );
    }
    let server_has_intake = cli.intake_store.is_some();
    if cli.demo_intake {
        let path = demo_directory
            .as_ref()
            .unwrap()
            .path()
            .join("intake.sqlite");
        nova_core::intake::IntakeStore::create(
            &path,
            VaultKey::read_file(&server_key_path)?,
            nova_core::compilation::AssetScope {
                owner_id: server_owner,
                space: server_space,
            },
        )?;
        server = server.with_intake_store(&path)?;
    } else if let Some(path) = cli.intake_store {
        server = server.with_intake_store(&path)?;
    }
    if cli.enable_compiler {
        server = server.with_model_runtime(nova_review::compiler::ModelRuntime {
            maximum_calls: cli.compiler_max_calls,
            credential_file: cli.model_credential_file,
            credential_origin: cli.model_credential_origin,
            output_mode: if cli.structured_local_output {
                nova_review::compiler::OutputMode::StructuredLocal
            } else {
                nova_review::compiler::OutputMode::JsonObject
            },
        })?;
    }
    if cli.workspace_v03 {
        server = server.with_native_workspace()?;
    }
    if let Some(instance) = cli.local_runtime_instance {
        server = server.with_local_runtime_instance(instance)?;
    }
    if cli.enable_capture_ingress {server=server.with_capture_ingress()?;}
    if cli.enable_continuity {server=server.with_continuity()?;}
    if cli.enable_ranked_retrieval {server=server.with_ranked_retrieval()?;}
    if cli.enable_historical_retrieval {server=server.with_historical_retrieval()?;}
    server.verify()?;
    let compiler_worker = server.start_compiler();
    println!(
        "Nova Memory local workspace: http://{address}/app/{}",
        if cli.demo {
            " (temporary synthetic demo)"
        } else {
            ""
        }
    );
    if cli.demo_intake || server_has_intake {
        println!("One-click capture: http://{address}/app/intake/");
    }
    let gateway_shutdown = server.clone();
    axum::serve(listener, server.router())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            gateway_shutdown.stop_local_gateways().await;
        })
        .await?;
    if let Some(worker) = compiler_worker {
        worker.abort();
    }
    drop(demo_directory);
    Ok(())
}
