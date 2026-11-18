//! Disposable task-card QA only. Fixed synthetic extraction, no model or external AI.
use nova_core::{Vault, VaultKey, compilation::*, intake::*, *};
use nova_review::ReviewServer;
use serde_json::json;
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("new ready path required"))?,
    );
    let temp = tempfile::tempdir()?;
    let key = VaultKey::generate()?;
    let key_file = temp.path().join("vault.key");
    key.write_new_file(&key_file)?;
    let owner_file = temp.path().join("owner.key");
    VaultKey::generate()?.write_new_file(&owner_file)?;
    let legacy = temp.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone())?);
    let intake = temp.path().join("intake.sqlite");
    let scope = AssetScope {
        owner_id: "owner:synthetic-task-ui".into(),
        space: "synthetic".into(),
    };
    let mut store = IntakeStore::create(&intake, key, scope.clone())?;
    store.enable_compilation(&scope)?;
    store.enable_connections(&scope)?;
    store.enable_continuity(&scope)?;
    store.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-no-model-call".into(),
            endpoint: "http://127.0.0.1:1/v1".into(),
        },
    )?;
    let source = store.receive(&IntakeAuthority { scope: scope.clone(), actor_id: "owner".into(), capture_allowed: true, role: SourceRole::User, privacy_level: PrivacyLevel::Private }, &serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"fixture","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC task fixture, not a real project.","occurred_at":null}))?)?.source;
    let job = store.claim_next(&scope, 60)?.unwrap();
    let lease = job.lease.unwrap();
    store.prepare_compilation(&scope, &job.job.job_id, &lease.lease_id)?;
    store.authorize_dispatch(&scope, &job.job.job_id, &lease.lease_id, 1000)?;
    let compiled = store.commit_compilation(&scope, &job.job.job_id, &lease.lease_id, &serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:synthetic","predicate":"task:scope","value":"合成星桥接续演练","polarity":"positive","modality":"planned","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC task fixture, not a real project.","occurrence":0}]}]}))?)?;
    let connection = store.create_connection(
        &scope,
        ConnectionRequest {
            request_id: "reader".into(),
            label: "独立合成接续库".into(),
            host: ConnectionHost::Cursor,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: false,
        },
    )?;
    let from = store.register_continuity_session(
        &scope,
        SessionRegistration {
            request_id: "from".into(),
            connection_id: connection.connection.id.clone(),
            lifetime_seconds: 1800,
        },
    )?;
    let recipient = store.register_continuity_session(
        &scope,
        SessionRegistration {
            request_id: "to".into(),
            connection_id: connection.connection.id.clone(),
            lifetime_seconds: 1800,
        },
    )?;
    let mut handoffs = vec![];
    for n in 1..=5 {
        let h = store.create_handoff(
            &scope,
            CreateHandoff {
                request_id: format!("task-{n}"),
                from: from.clone(),
                recipient: recipient.clone(),
                lifetime_seconds: 1200,
                memories: compiled.output_versions.clone(),
                content: HandoffContent {
                    summary: format!("星桥接续演练 {n} · 合成测试"),
                    completed: vec!["保存了一份独立测试材料".into()],
                    open_tasks: vec!["核对新会话能否恢复下一步".into()],
                    decisions: vec!["先验证本机接续，再做真实 AI 验收".into()],
                    failed_attempts: vec!["未确定时区，不能假设为北京时间".into()],
                    constraints: vec!["只使用合成资料；尚未调用模型".into()],
                    next_steps: vec!["检查接续请求和撤权后的失效状态".into()],
                },
            },
        )?;
        if n == 4 {
            let access = store.authenticate_connection(&scope, &connection.token)?;
            store.claim_handoff(
                &scope,
                &access,
                &recipient,
                &h.handoff_id,
                "fixture-claimed",
            )?;
        }
        handoffs.push(h.handoff_id);
    }
    drop(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = ReviewServer::new(legacy, key_file, address.to_string(), true)
        .with_owner(scope.owner_id, scope.space)
        .with_owner_token(&owner_file)?
        .with_intake_store(&intake)?
        .with_native_workspace()?
        .with_continuity()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    file.write_all(serde_json::to_string(&json!({"origin":format!("http://{address}"),"owner_file":owner_file,"source":source,"connection_id":connection.connection.id,"handoffs":handoffs,"real_model_calls":0,"public_tunnels":0}))?.as_bytes())?;
    file.sync_all()?;
    axum::serve(listener, server.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    std::fs::remove_file(output)?;
    Ok(())
}
