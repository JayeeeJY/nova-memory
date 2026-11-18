use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    routing::post,
};
use nova_core::{
    compilation::{AssetScope, SourceRole, INTAKE_VERSION},
    intake::*,
    *,
};
use nova_review::{
    ReviewServer,
    compiler::{ModelRuntime, run_once},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31412";
#[derive(Clone)]
struct Model {
    mode: &'static str,
    seen: Arc<Mutex<Vec<Value>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn mock(State(m): State<Model>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    m.seen.lock().unwrap().push(body.clone());
    m.entered.notify_one();
    if m.mode == "blocked" {
        m.release.notified().await;
    }
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let source = input["source_text"].as_str().unwrap();
    let out = json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"user","predicate":"work:meeting_preference","value":if input["correction_target"].is_null(){"30 minutes"}else{"60 minutes"},"polarity":"negative","modality":"asserted","conditions":{"scope:context":"work"}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":source,"occurrence":0}]}]});
    let content = if m.mode == "invalid" {
        "not JSON".to_string()
    } else {
        out.to_string()
    };
    let mut response = json!({"choices":[{"finish_reason":if m.mode=="truncated"{"length"}else{"stop"},"message":{"content":content}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":3}}});
    if m.mode == "missing_usage" { response.as_object_mut().unwrap().remove("usage"); }
    if m.mode == "invalid_usage" { response["usage"]["total_tokens"] = json!(999); }
    if m.mode == "tools" {
        response["choices"][0]["message"]["tool_calls"] =
            json!([{"type":"function","function":{"name":"read_all"}}]);
    }
    (
        if m.mode == "redirect" {
            StatusCode::FOUND
        } else {
            StatusCode::OK
        },
        Json(response),
    )
}
async fn setup(
    mode: &'static str,
    maximum_calls: u64,
) -> (
    tempfile::TempDir,
    ReviewServer,
    String,
    Model,
    tokio::task::JoinHandle<()>,
) {
    let model = Model {
        mode,
        seen: Arc::new(Mutex::new(vec![])),
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(mock))
        .with_state(model.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let d = tempfile::tempdir().unwrap();
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&d.path().join("vault.key")).unwrap();
    drop(Vault::create(&d.path().join("legacy.sqlite"), k.clone()).unwrap());
    VaultKey::generate()
        .unwrap()
        .write_new_file(&d.path().join("owner.key"))
        .unwrap();
    let token = std::fs::read_to_string(d.path().join("owner.key"))
        .unwrap()
        .trim()
        .into();
    let mut store = IntakeStore::create(&d.path().join("intake.sqlite"), k, scope()).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-fixture-not-a-real-llm".into(),
                endpoint,
            },
        )
        .unwrap();
    drop(store);
    let server = ReviewServer::new(
        d.path().join("legacy.sqlite"),
        d.path().join("vault.key"),
        HOST.into(),
        true,
    )
    .with_owner("owner:test".into(), "private".into())
    .with_owner_token(&d.path().join("owner.key"))
    .unwrap()
    .with_intake_store(&d.path().join("intake.sqlite"))
    .unwrap()
    .with_model_runtime(ModelRuntime::local(maximum_calls))
    .unwrap();
    (d, server, token, model, task)
}
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:test".into(),
        space: "private".into(),
    }
}
fn store(d: &tempfile::TempDir) -> IntakeStore {
    IntakeStore::open(
        &d.path().join("intake.sqlite"),
        VaultKey::read_file(&d.path().join("vault.key")).unwrap(),
        scope(),
    )
    .unwrap()
}
async fn call(r: &Router, path: &str, body: Option<Value>, token: &str) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("host", HOST)
        .header("x-nova-review", "owner-review")
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        req = req
            .header("origin", format!("http://{HOST}"))
            .header("content-type", "application/json");
    }
    let out = r
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status=out.status();
    let bytes=to_bytes(out.into_body(),1_000_000).await.unwrap();
    let data=serde_json::from_slice(&bytes).unwrap_or_else(|_|{assert!(!status.is_success());Value::Null});
    (status,data)
}
const ROOT: &str = "/console/api/owner/intake";
async fn receive(r: &Router, token: &str, id: &str) -> Value {
    let(s,data)=call(r,ROOT,Some(json!({"schema_version":"nova.intake.v0.1","request_id":id,"source_platform":"web-console","capture_fidelity":"f2_visible_transcript","content":"在工作时我不参加超过30分钟的会议。","occurred_at":null})),token).await;
    assert_eq!(s, StatusCode::OK);
    data["receipt"].clone()
}
#[tokio::test]
async fn real_http_transport_to_mock_compiles_corrects_and_undoes_with_owner_only_routes() {
    let (d, s, t, m, task) = setup("good", 100).await;
    let r = s.clone().router();
    let receipt = receive(&r, &t, "first").await;
    assert!(run_once(&s).await.unwrap().is_some());
    let data = call(&r, &format!("{ROOT}/units"), None, &t).await.1;
    let u = &data["items"][0]["memory"]["unit"];
    assert_eq!(u["content"]["value"], "30 minutes");
    assert_eq!(u["origin"], "extracted");
    let usage=store(&d).compilation_usage(&scope(),None).unwrap();
    assert_eq!(usage.items.len(),1);
    assert_eq!(usage.items[0].observed.as_ref().unwrap().observation.usage.as_ref().unwrap().cached_input_tokens,Some(3));
    assert_eq!(
        store(&d)
            .job(&scope(), receipt["job_id"].as_str().unwrap())
            .unwrap()
            .job
            .state,
        nova_core::compilation::JobState::Ready
    );
    let id = u["header"]["object"]["object_id"].as_str().unwrap();
    let correction = format!("{ROOT}/units/{id}/correct");
    let body = json!({"request_id":"correct","expected_revision":1,"instruction":"更正为60分钟，仅工作场景。"});
    assert_eq!(
        call(&r, &correction, Some(body.clone()), "client").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, &correction, Some(body), &t).await.0,
        StatusCode::OK
    );
    run_once(&s).await.unwrap();
    assert_eq!(
        store(&d)
            .compiled_unit(&scope(), id)
            .unwrap()
            .unit
            .header
            .object
            .revision,
        2
    );
    assert_eq!(
        call(
            &r,
            &format!("{ROOT}/units/{id}/undo"),
            Some(json!({"expected_revision":2})),
            &t
        )
        .await
        .0,
        StatusCode::OK
    );
    let history = call(&r, &format!("{ROOT}/units/{id}/history"), None, &t)
        .await
        .1;
    assert_eq!(history["versions"].as_array().unwrap().len(), 3);
    let seen = m.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["max_tokens"], 2048);
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[0]["messages"].as_array().unwrap().len(), 2);
    let input: Value =
        serde_json::from_str(seen[0]["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert!(input.get("owner_id").is_none());
    assert!(input.get("approval_id").is_none());
    assert!(input["correction_target"].is_null());
    task.abort();
}
#[tokio::test]
async fn malformed_truncated_tools_and_redirect_outputs_fail_and_keep_original() {
    for mode in ["invalid", "truncated", "tools", "redirect"] {
        let (d, s, t, _m, task) = setup(mode, 100).await;
        let r = s.clone().router();
        let receipt = receive(&r, &t, mode).await;
        assert!(run_once(&s).await.is_err(), "{mode}");
        let st = store(&d);
        let job = st
            .job(&scope(), receipt["job_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(job.job.state, nova_core::compilation::JobState::Failed);
        assert!(st.compiled_units(&scope(), None).unwrap().is_empty());
        let usage=st.compilation_usage(&scope(),None).unwrap();
        assert_eq!(usage.items.len(),1);
        let observed=&usage.items[0].observed.as_ref().unwrap().observation;
        if mode=="redirect" { assert!(observed.usage.is_none()); }
        else { assert_eq!(observed.usage.as_ref().unwrap().total_tokens,15); }
        assert!(
            !st.source(&scope(), receipt["source"]["object_id"].as_str().unwrap())
                .unwrap()
                .content
                .is_empty()
        );
        task.abort();
    }
}
#[tokio::test]
async fn model_call_does_not_lock_owner_config_and_late_response_cannot_commit() {
    let (d, s, t, m, task) = setup("blocked", 100).await;
    let r = s.clone().router();
    let receipt = receive(&r, &t, "config-revoke").await;
    let copy = s.clone();
    let run = tokio::spawn(async move { run_once(&copy).await });
    m.entered.notified().await;
    let config_path = format!("{ROOT}/configuration");
    let change = call(
        &r,
        &config_path,
        Some(json!({"expected_revision":2,"selection":{"location":"unconfigured"}})),
        &t,
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), change)
            .await
            .unwrap()
            .0,
        StatusCode::OK
    );
    m.release.notify_one();
    assert!(run.await.unwrap().is_err());
    assert!(store(&d).compiled_units(&scope(), None).unwrap().is_empty());
    let usage=store(&d).compilation_usage(&scope(),None).unwrap();
    assert_eq!(usage.items[0].intent.configuration_revision,2);
    assert_eq!(usage.items[0].observed.as_ref().unwrap().observation.usage.as_ref().unwrap().total_tokens,15);
    assert_ne!(
        store(&d)
            .job(&scope(), receipt["job_id"].as_str().unwrap())
            .unwrap()
            .job
            .state,
        nova_core::compilation::JobState::Ready
    );
    task.abort();
}
#[tokio::test]
async fn durable_call_budget_prevents_second_http_call() {
    let (d, s, t, m, task) = setup("good", 1).await;
    let r = s.clone().router();
    receive(&r, &t, "one").await;
    run_once(&s).await.unwrap();
    let receipt = receive(&r, &t, "two").await;
    assert!(run_once(&s).await.is_err());
    assert_eq!(m.seen.lock().unwrap().len(), 1);
    assert_eq!(store(&d).compilation_usage(&scope(),None).unwrap().items.len(),1);
    assert_eq!(
        store(&d)
            .job(&scope(), receipt["job_id"].as_str().unwrap())
            .unwrap()
            .job
            .error_code
            .as_deref(),
        Some("budget_exceeded")
    );
    task.abort();
}
#[tokio::test]
async fn remote_credential_origin_mismatch_denies_before_any_http_call() {
    let (d, s, t, m, task) = setup("good", 100).await;
    let key = d.path().join("model.key");
    VaultKey::generate().unwrap().write_new_file(&key).unwrap();
    let s = s
        .with_model_runtime(ModelRuntime {
            maximum_calls: 100,
            credential_file: Some(key),
            credential_origin: Some("https://allowed.example.test".into()),
            ..ModelRuntime::local(100)
        })
        .unwrap();
    let mut st = store(&d);
    st.configure_model(
        2,
        ModelSelection::Remote {
            model_id: "synthetic".into(),
            endpoint: "https://other.example.test/v1".into(),
            allow_selected_source: true,
        },
    )
    .unwrap();
    let r = s.clone().router();
    receive(&r, &t, "wrong-origin").await;
    assert!(run_once(&s).await.is_err());
    assert!(m.seen.lock().unwrap().is_empty());
    assert!(store(&d).compilation_usage(&scope(),None).unwrap().items.is_empty());
    task.abort();
}

#[tokio::test]
async fn metering_owner_read_is_bounded_private_and_never_dispatches() {
    for mode in ["good","missing_usage","invalid_usage"] {
        let(d,s,t,m,task)=setup(mode,100).await;let r=s.clone().router();
        receive(&r,&t,mode).await;run_once(&s).await.unwrap();
        let path=format!("{ROOT}/model-usage");let(code,data)=call(&r,&path,None,&t).await;
        assert_eq!(code,StatusCode::OK);assert_eq!(data["reporting_only"],true);assert!(data["monetary_cost"].is_null());
        let usage=&data["items"][0]["observed"]["observation"]["usage"];
        if mode=="good"{assert_eq!(usage["total_tokens"],15);}else{assert!(usage.is_null());}
        for private in [&t,"http://127.0.0.1","在工作时","lease_id"] {assert!(!data.to_string().contains(private));}
        assert_eq!(call(&r,&path,None,"wrong-owner").await.0,StatusCode::FORBIDDEN);
        assert_eq!(call(&r,&path,Some(json!({"usage":{"total_tokens":0}})),&t).await.0,StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(call(&r,&format!("{path}?before=0"),None,&t).await.0,StatusCode::BAD_REQUEST);
        assert_ne!(call(&r,&format!("{path}?owner_id=other"),None,&t).await.0,StatusCode::OK);
        assert_eq!(m.seen.lock().unwrap().len(),1);assert_eq!(store(&d).compilation_usage(&scope(),None).unwrap().items.len(),1);
        task.abort();
    }
}

#[tokio::test]
async fn withdrawn_source_late_usage_is_preserved_but_content_does_not_commit() {
    let(d,s,t,m,task)=setup("blocked",100).await;let r=s.clone().router();
    let receipt=receive(&r,&t,"withdraw-during-call").await;
    let copy=s.clone();let run=tokio::spawn(async move{run_once(&copy).await});m.entered.notified().await;
    store(&d).delete_source(&scope(),receipt["source"]["object_id"].as_str().unwrap()).unwrap();
    m.release.notify_one();assert!(run.await.unwrap().is_err());
    let st=store(&d);assert!(st.compiled_units(&scope(),None).unwrap().is_empty());
    assert_eq!(st.compilation_usage(&scope(),None).unwrap().items[0].observed.as_ref().unwrap().observation.usage.as_ref().unwrap().total_tokens,15);
    assert!(st.source(&scope(),receipt["source"]["object_id"].as_str().unwrap()).is_err());
    task.abort();
}

#[tokio::test]
async fn cancelling_worker_leaves_unknown_intent_and_never_automatically_replays() {
    let(d,s,t,m,task)=setup("blocked",100).await;let r=s.clone().router();receive(&r,&t,"interrupted").await;
    let copy=s.clone();let run=tokio::spawn(async move{run_once(&copy).await});m.entered.notified().await;
    run.abort();assert!(run.await.unwrap_err().is_cancelled());m.release.notify_one();
    assert!(run_once(&s).await.unwrap().is_none());
    let st=store(&d);let p=st.compilation_usage(&scope(),None).unwrap();assert_eq!(p.items.len(),1);assert!(p.items[0].observed.is_none());
    assert!(st.compiled_units(&scope(),None).unwrap().is_empty());assert_eq!(m.seen.lock().unwrap().len(),1);
    task.abort();
}

#[tokio::test]
async fn model_credential_cannot_reuse_owner_copied_vault_or_connection_key() {
    let (d, s, _t, _m, task) = setup("good", 100).await;
    for source in ["vault.key", "owner.key"] {
        let copy = d.path().join("wrong-model.key");
        std::fs::copy(d.path().join(source), &copy).unwrap();
        let result = s.clone().with_model_runtime(ModelRuntime {
            maximum_calls: 100,
            credential_file: Some(copy),
            credential_origin: Some("https://api.example.test".into()),
            ..ModelRuntime::local(100)
        });
        assert!(matches!(result, Err(NovaError::PolicyDenied(_))));
    }
    let uppercase = d.path().join("uppercase-vault-model.key");
    std::fs::write(
        &uppercase,
        std::fs::read_to_string(d.path().join("vault.key"))
            .unwrap()
            .to_uppercase(),
    )
    .unwrap();
    assert!(matches!(
        s.clone().with_model_runtime(ModelRuntime {
            maximum_calls: 100,
            credential_file: Some(uppercase),
            credential_origin: Some("https://api.example.test".into()),
            ..ModelRuntime::local(100)
        }),
        Err(NovaError::PolicyDenied(_))
    ));
    let mut st = store(&d);
    st.enable_connections(&scope()).unwrap();
    let client = st
        .create_connection(
            &scope(),
            nova_core::intake::ConnectionRequest {
                request_id: "credential-separation".into(),
                label: "synthetic".into(),
                host: nova_core::intake::ConnectionHost::Codex,
                preset: nova_core::intake::ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap();
    let copy = d.path().join("copied-client-model.key");
    VaultKey::from_hex(&client.token)
        .unwrap()
        .write_new_file(&copy)
        .unwrap();
    for revoke in [false, true] {
        if revoke {
            st.revoke_connection(&scope(), &client.connection.id)
                .unwrap();
        }
        assert!(matches!(
            s.clone().with_model_runtime(ModelRuntime {
                maximum_calls: 100,
                credential_file: Some(copy.clone()),
                credential_origin: Some("https://api.example.test".into()),
                ..ModelRuntime::local(100)
            }),
            Err(NovaError::PolicyDenied(_))
        ));
    }
    task.abort();
}

#[tokio::test]
async fn worker_retains_page_progress_across_server_clones_and_does_not_retry_history() {
    let (d, s, token, model, task) = setup("good", 1000).await;
    {
        let mut history = store(&d);
        let authority = IntakeAuthority {
            scope: scope(),
            actor_id: "synthetic-history".into(),
            capture_allowed: true,
            role: SourceRole::User,
            privacy_level: PrivacyLevel::Private,
        };
        for n in 0..MAX_WORKER_QUEUE_ROWS + 1 {
            history.receive(&authority,&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":format!("old-{n}"),"source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"historical synthetic input","occurred_at":null})).unwrap()).unwrap();
            let j = history.claim_next(&scope(), 120).unwrap().unwrap();
            history
                .fail_attempt(
                    &scope(),
                    &j.job.job_id,
                    &j.lease.unwrap().lease_id,
                    AttemptFailure::ModelUnavailable,
                )
                .unwrap();
        }
    }
    let receipt = receive(&s.clone().router(), &token, "after-long-history").await;
    assert!(run_once(&s).await.unwrap().is_none());
    assert!(model.seen.lock().unwrap().is_empty());
    let done = run_once(&s.clone()).await.unwrap();
    assert_eq!(done.as_deref(), receipt["job_id"].as_str());
    assert_eq!(model.seen.lock().unwrap().len(), 1);
    assert_eq!(store(&d).compiled_units(&scope(), None).unwrap().len(), 1);
    task.abort();
}
