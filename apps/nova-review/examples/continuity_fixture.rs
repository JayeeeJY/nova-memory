//! New disposable synthetic Vault only. No model, transcript import or external Host.
use nova_core::{Vault, VaultKey, compilation::AssetScope, intake::*};
use nova_review::ReviewServer;
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("ready path required"))?,
    );
    let temp = tempfile::tempdir()?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    let key_file = temp.path().join("vault.key");
    let owner_file = temp.path().join("owner.key");
    let key = VaultKey::generate()?;
    key.write_new_file(&key_file)?;
    VaultKey::generate()?.write_new_file(&owner_file)?;
    let legacy = temp.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone())?);
    let intake = temp.path().join("intake.sqlite");
    let scope = AssetScope {
        owner_id: "owner:synthetic-continuity".into(),
        space: "synthetic".into(),
    };
    let mut store = IntakeStore::create(&intake, key, scope.clone())?;
    store.enable_compilation(&scope)?;
    store.enable_capture(&scope)?;
    store.enable_connections(&scope)?;
    let grant = store.create_capture_grant(
        &scope,
        CaptureGrantRequest {
            request_id: "fixture".into(),
            source_namespace: "synthetic-codex".into(),
            host: "codex".into(),
            surface: "cli_hooks".into(),
            host_version: "synthetic".into(),
            events: vec![CaptureEventKind::UserPrompt],
            lifetime_seconds: 3600,
            event_budget: 30,
            byte_budget: 8192,
        },
    )?;
    let access = store.authenticate_capture(&scope, &grant.token)?;
    let session = store.register_capture_session(&scope, &access, "synthetic-origin")?;
    for n in 1..=22 {
        store.capture_event(
            &scope,
            &access,
            &serde_json::to_vec(&CaptureEnvelope {
                schema_version: CAPTURE_VERSION.into(),
                session_id: session.session_id.clone(),
                event_id: format!("event-{n}"),
                sequence: n,
                kind: CaptureEventKind::UserPrompt,
                content: format!("SYNTHETIC_CAPTURE_RAW_PRIVATE_{n}"),
                occurred_at: None,
                loss: CaptureLoss {
                    redacted: false,
                    truncated: false,
                    partial_history: true,
                },
            })?,
        )?;
        store.record_capture_provenance(
            &scope,
            &access,
            CaptureProvenanceRequest {
                session_id: session.session_id.clone(),
                event_id: format!("event-{n}"),
                kind: CaptureEventKind::UserPrompt,
                origin: CaptureDeliveryOrigin::UserProvidedExport,
            },
        )?;
    }
    store.create_connection(
        &scope,
        ConnectionRequest {
            request_id: "reader".into(),
            label: "合成 Claude 接续".into(),
            host: ConnectionHost::Claude,
            preset: ConnectionPreset::ReadOnly,
            topic_id: None,
            include_future: true,
        },
    )?;
    drop(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = ReviewServer::new(legacy, key_file, address.to_string(), true)
        .with_owner(scope.owner_id, scope.space)
        .with_owner_token(&owner_file)?
        .with_intake_store(&intake)?
        .with_native_workspace()?
        .with_capture_ingress()?
        .with_continuity()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    file.write_all(serde_json::to_string(&json!({"origin":format!("http://{address}"),"owner_file":owner_file,"synthetic":true,"real_model_calls":0}))?.as_bytes())?;
    file.sync_all()?;
    axum::serve(listener, server.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    drop(temp);
    Ok(())
}
