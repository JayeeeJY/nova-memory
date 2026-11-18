//! Four ordinary worker calls to a disposable HTTP stub; no real model or existing user data.
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use nova_core::{
    PrivacyLevel, Vault, VaultKey,
    compilation::{AssetScope, SourceRole},
    intake::*,
};
use nova_review::{
    ReviewServer,
    compiler::{ModelRuntime, run_once},
};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

async fn mock(
    State(count): State<Arc<AtomicUsize>>,
    Json(_body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let n = count.fetch_add(1, Ordering::SeqCst);
    let content = if n == 1 {
        "synthetic malformed output".to_string()
    } else {
        json!({"schema_version":EXTRACTION_VERSION,"units":[]}).to_string()
    };
    let mut response = json!({"choices":[{"finish_reason":"stop","message":{"content":content}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":3}}});
    if n == 2 {
        response.as_object_mut().unwrap().remove("usage");
    }
    (
        if n == 3 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        Json(response),
    )
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("ready path required"))?;
    let temp = tempfile::tempdir()?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    let key = VaultKey::generate()?;
    let key_path = temp.path().join("vault.key");
    key.write_new_file(&key_path)?;
    let owner_file = temp.path().join("owner.key");
    VaultKey::generate()?.write_new_file(&owner_file)?;
    let legacy = temp.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone())?);
    let intake = temp.path().join("intake.sqlite");
    let scope = AssetScope {
        owner_id: "owner:synthetic-meter".into(),
        space: "synthetic".into(),
    };
    let mut store = IntakeStore::create(&intake, key.clone(), scope.clone())?;
    store.enable_compilation(&scope)?;
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", model_listener.local_addr()?);
    let count = Arc::new(AtomicUsize::new(0));
    let model = Router::new()
        .route("/v1/chat/completions", post(mock))
        .with_state(count.clone());
    let model_task = tokio::spawn(async move {
        axum::serve(model_listener, model).await.unwrap();
    });
    store.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-meter-ui".into(),
            endpoint,
        },
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = ReviewServer::new(legacy, key_path, address.to_string(), true)
        .with_owner(scope.owner_id.clone(), scope.space.clone())
        .with_owner_token(&owner_file)?
        .with_intake_store(&intake)?
        .with_native_workspace()?
        .with_model_runtime(ModelRuntime::local(10))?;
    for n in 0..5 {
        let authority = IntakeAuthority {
            scope: scope.clone(),
            actor_id: "owner:synthetic".into(),
            capture_allowed: true,
            role: SourceRole::User,
            privacy_level: PrivacyLevel::Private,
        };
        store.receive(&authority,&serde_json::to_vec(&json!({"schema_version":"nova.intake.v0.1","request_id":format!("fixture-{n}"),"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC_PRIVATE_SOURCE_MUST_NOT_BE_IN_USAGE","occurred_at":null}))?)?;
        if n < 4 {
            anyhow::ensure!(
                run_once(&server).await.is_ok() == matches!(n, 0 | 2),
                "unexpected synthetic worker result"
            );
        } else {
            let job = store.claim_next(&scope, 120)?.unwrap();
            let lease = job.lease.unwrap();
            store.prepare_compilation(&scope, &job.job.job_id, &lease.lease_id)?;
            store.authorize_metered_dispatch(
                &scope,
                &job.job.job_id,
                &lease.lease_id,
                10,
                CompilationTransport::OpenaiCompatibleJson,
            )?;
        }
    }
    anyhow::ensure!(
        count.load(Ordering::SeqCst) == 4,
        "unexpected dispatch count"
    );
    drop(store);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    file.write_all(serde_json::to_string(&json!({"origin":format!("http://{address}"),"owner_file":owner_file,"fixture_dir":temp.path(),"synthetic_http_calls":4,"real_model_calls":0}))?.as_bytes())?;
    file.sync_all()?;
    // Deliberately no background compiler: the fifth authorization simulates an interrupted call.
    axum::serve(listener, server.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    model_task.abort();
    let _ = model_task.await;
    drop(temp);
    Ok(())
}
