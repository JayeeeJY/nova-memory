//! Owner context preview/release with actual Core state and synthetic records; no model/network.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::*, intake::*, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31449";
const ROOT: &str = "/console/api/owner/intake/context-packs";
struct Fixture {
    _dir: tempfile::TempDir,
    router: axum::Router,
    store: IntakeStore,
    scope: AssetScope,
    token: String,
    client: String,
    source: IntakeReceipt,
    reference: RevisionRef,
}
fn setup(native: bool) -> Fixture {
    let d = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let kp = d.path().join("vault.key");
    key.write_new_file(&kp).unwrap();
    let legacy = d.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone()).unwrap());
    let op = d.path().join("owner.key");
    VaultKey::generate().unwrap().write_new_file(&op).unwrap();
    let token = std::fs::read_to_string(&op).unwrap().trim().to_owned();
    let scope = AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "test".into(),
    };
    let db = d.path().join("intake.sqlite");
    let mut store = IntakeStore::create(&db, key, scope.clone()).unwrap();
    store.enable_compilation(&scope).unwrap();
    store.enable_connections(&scope).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-not-a-model".into(),
                endpoint: "http://127.0.0.1:9/v1".into(),
            },
        )
        .unwrap();
    let raw = "SYNTHETIC ORIGINAL never exported as source";
    let source=store.receive(&IntakeAuthority{scope:scope.clone(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"fixture","source_platform":"synthetic-chatgpt","capture_fidelity":"f2_visible_transcript","content":raw,"occurred_at":null})).unwrap()).unwrap();
    let j = store.claim_next(&scope, 60).unwrap().unwrap();
    let lease = j.lease.unwrap();
    store
        .prepare_compilation(&scope, &j.job.job_id, &lease.lease_id)
        .unwrap();
    store
        .authorize_dispatch(&scope, &j.job.job_id, &lease.lease_id, 10)
        .unwrap();
    let result=store.commit_compilation(&scope,&j.job.job_id,&lease.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"user","predicate":"work:meeting_limit","value":{"minutes":30},"polarity":"negative","modality":"asserted","conditions":{"scope:context":"工作","scope:exception":"私人聚会不受限制"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":raw,"occurrence":0}]}]})).unwrap()).unwrap();
    let reference = result.output_versions[0].clone();
    let client = store
        .create_connection(
            &scope,
            ConnectionRequest {
                request_id: "client".into(),
                label: "synthetic".into(),
                host: ConnectionHost::Cursor,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap()
        .token;
    let mut server = ReviewServer::new(legacy, kp, HOST.into(), true)
        .with_owner(scope.owner_id.clone(), scope.space.clone())
        .with_owner_token(&op)
        .unwrap()
        .with_intake_store(&db)
        .unwrap();
    if native {
        server = server.with_native_workspace().unwrap();
    }
    Fixture {
        _dir: d,
        router: server.router(),
        store,
        scope,
        token,
        client,
        source,
        reference,
    }
}
async fn raw(
    f: &Fixture,
    path: &str,
    body: Vec<u8>,
    token: &str,
    host: &str,
    origin: &str,
    review: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(format!("{ROOT}/{path}"))
        .method("POST")
        .header("Host", host)
        .header("Origin", origin)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    if review {
        req = req.header("X-Nova-Review", "owner-review");
    }
    let result = f
        .router
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = result.status();
    assert_eq!(result.headers()["cache-control"], "no-store");
    let body = to_bytes(result.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
async fn call(f: &Fixture, path: &str, body: Value) -> (StatusCode, Value) {
    raw(
        f,
        path,
        serde_json::to_vec(&body).unwrap(),
        &f.token,
        HOST,
        &format!("http://{HOST}"),
        true,
    )
    .await
}
fn selection(f: &Fixture) -> Value {
    json!({"target":"cursor","memories":[f.reference]})
}
#[tokio::test]
async fn owner_preview_export_preserves_data_but_never_contacts_target_or_creates_grant() {
    let f = setup(true);
    let before = serde_json::to_value(f.store.connections(&f.scope).unwrap()).unwrap();
    let (status, p) = call(&f, "preview", selection(&f)).await;
    assert_eq!(status, 200);
    assert_eq!(p["target_contacted"], false);
    assert_eq!(p["model_called"], false);
    assert_eq!(
        p["pack"]["payload"]["memories"][0]["content"]["conditions"]["scope:exception"],
        "私人聚会不受限制"
    );
    assert!(!p.to_string().contains("SYNTHETIC ORIGINAL"));
    let(status,out)=call(&f,"export",json!({"selection":p["pack"]["selection"],"expected_sha256":p["pack"]["sha256"],"plaintext_export":true})).await;
    assert_eq!(status, 200);
    assert_eq!(out["release"]["pack"]["text"], p["pack"]["text"]);
    assert_eq!(out["release"]["transfer_to_target_verified"], false);
    assert_eq!(out["release"]["server_recorded"], false);
    assert!(!out.to_string().contains(&f.client));
    assert_eq!(
        serde_json::to_value(f.store.connections(&f.scope).unwrap()).unwrap(),
        before
    );
}
#[tokio::test]
async fn current_owner_boundary_native_opt_in_and_closed_schema_are_required() {
    let f = setup(true);
    let bytes = serde_json::to_vec(&selection(&f)).unwrap();
    for token in ["", &f.client] {
        assert_eq!(
            raw(
                &f,
                "preview",
                bytes.clone(),
                token,
                HOST,
                &format!("http://{HOST}"),
                true
            )
            .await
            .0,
            403
        );
    }
    for (path, body) in [
        ("preview", selection(&f)),
        (
            "export",
            json!({"selection":selection(&f),"expected_sha256":"0".repeat(64),"plaintext_export":true}),
        ),
    ] {
        assert_eq!(
            raw(
                &f,
                path,
                serde_json::to_vec(&body).unwrap(),
                &f.token,
                HOST,
                "http://evil.invalid",
                true
            )
            .await
            .0,
            403
        );
        assert_eq!(
            raw(
                &f,
                path,
                serde_json::to_vec(&body).unwrap(),
                &f.token,
                "evil.invalid",
                &format!("http://{HOST}"),
                true
            )
            .await
            .0,
            403
        );
        assert_eq!(
            raw(
                &f,
                path,
                serde_json::to_vec(&body).unwrap(),
                &f.token,
                HOST,
                &format!("http://{HOST}"),
                false
            )
            .await
            .0,
            403
        );
    }
    let off = setup(false);
    assert_eq!(call(&off, "preview", selection(&off)).await.0, 403);
    let mut injected = selection(&f);
    injected["owner_id"] = json!("owner:other");
    assert_eq!(call(&f, "preview", injected).await.0, 400);
    assert_eq!(
        raw(
            &f,
            "preview",
            b"{\"target\":\"cursor\",\"target\":\"grok\",\"memories\":[]}".to_vec(),
            &f.token,
            HOST,
            &format!("http://{HOST}"),
            true
        )
        .await
        .0,
        400
    );
    assert_eq!(
        raw(
            &f,
            "preview",
            vec![b' '; 13 * 1024],
            &f.token,
            HOST,
            &format!("http://{HOST}"),
            true
        )
        .await
        .0,
        413
    );
}
#[tokio::test]
async fn copied_preview_is_revalidated_after_withdrawal_not_released_from_cache() {
    let mut f = setup(true);
    let (_, p) = call(&f, "preview", selection(&f)).await;
    f.store
        .withdraw_source(&f.scope, &f.source.source.object_id)
        .unwrap();
    let(status,out)=call(&f,"export",json!({"selection":p["pack"]["selection"],"expected_sha256":p["pack"]["sha256"],"plaintext_export":true})).await;
    assert_eq!(status, 403);
    assert!(out.get("release").is_none());
    assert!(out.get("pack").is_none());
}
#[tokio::test]
async fn export_needs_matching_target_preview_and_plaintext_intent() {
    let f = setup(true);
    let (_, p) = call(&f, "preview", selection(&f)).await;
    let mut input = json!({"selection":p["pack"]["selection"],"expected_sha256":p["pack"]["sha256"],"plaintext_export":false});
    assert_eq!(call(&f, "export", input.clone()).await.0, 400);
    input["plaintext_export"] = json!(true);
    input["selection"]["target"] = json!("grok");
    assert_eq!(call(&f, "export", input.clone()).await.0, 409);
    input["selection"]["target"] = json!("cursor");
    input["expected_sha256"] = json!("0".repeat(64));
    assert_eq!(call(&f, "export", input.clone()).await.0, 409);
}
