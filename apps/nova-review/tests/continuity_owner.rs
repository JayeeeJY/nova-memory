use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::*, intake::*, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;
const HOST: &str = "127.0.0.1:31445";
const ROOT: &str = "/console/api/owner/intake/continuity";
fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "continuity-owner".into(),
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    router: axum::Router,
    owner_token: String,
    client_token: String,
    connection_id: String,
    memory: RevisionRef,
    source: RevisionRef,
}
fn setup(enabled: bool) -> Fixture { setup_host(enabled, ConnectionHost::Codex) }
fn setup_host(enabled: bool, host: ConnectionHost) -> Fixture {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("legacy.sqlite");
    let kf = d.path().join("vault.key");
    let key = VaultKey::generate().unwrap();
    key.write_new_file(&kf).unwrap();
    drop(Vault::create(&db, key.clone()).unwrap());
    let of = d.path().join("owner.key");
    let ot = VaultKey::generate().unwrap();
    ot.write_new_file(&of).unwrap();
    let owner_token = ot.to_hex();
    let intake = d.path().join("intake.sqlite");
    let mut s = IntakeStore::create(&intake, key, scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.enable_connections(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-no-model".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
        },
    )
    .unwrap();
    let source=s.receive(&IntakeAuthority {scope:scope(),actor_id:"owner".into(),capture_allowed:true,role:SourceRole::User,privacy_level:PrivacyLevel::Private},&serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":"fixture","source_platform":"synthetic","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC owner fixture source","occurred_at":null})).unwrap()).unwrap().source;
    let j = s.claim_next(&scope(), 60).unwrap().unwrap();
    let l = j.lease.unwrap();
    s.prepare_compilation(&scope(), &j.job.job_id, &l.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &j.job.job_id, &l.lease_id, 1000)
        .unwrap();
    let result=s.commit_compilation(&scope(),&j.job.job_id,&l.lease_id,&serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":"project:synthetic","predicate":"task:scope","value":"local fixture","polarity":"positive","modality":"asserted","conditions":{}},"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":"SYNTHETIC owner fixture source","occurrence":0}]}]})).unwrap()).unwrap();
    let connection = s
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "client".into(),
                label: "fixture".into(),
                host,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: false,
            },
        )
        .unwrap();
    drop(s);
    let mut server = ReviewServer::new(db, kf, HOST.into(), true)
        .with_owner(scope().owner_id, scope().space)
        .with_owner_token(&of)
        .unwrap()
        .with_intake_store(&intake)
        .unwrap()
        .with_native_workspace()
        .unwrap();
    if enabled {
        server = server.with_continuity().unwrap();
    }
    Fixture {
        _dir: d,
        router: server.router(),
        owner_token,
        client_token: connection.token,
        connection_id: connection.connection.id,
        memory: result.output_versions[0].clone(),
        source,
    }
}
async fn raw(
    f: &Fixture,
    path: &str,
    body: Option<String>,
    token: &str,
    origin: &str,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("Host", HOST)
        .header("X-Nova-Review", "owner-review")
        .header("Authorization", format!("Bearer {token}"));
    if body.is_some() {
        request = request
            .header("Origin", origin)
            .header("Content-Type", "application/json");
    }
    let response = f
        .router
        .clone()
        .oneshot(request.body(Body::from(body.unwrap_or_default())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn call(f: &Fixture, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    raw(
        f,
        path,
        body.map(|v| v.to_string()),
        &f.owner_token,
        &format!("http://{HOST}"),
    )
    .await
}
async fn register(f: &Fixture, id: &str) -> Value {
    let (status, data) = call(
        f,
        &format!("{ROOT}/sessions"),
        Some(json!({"request_id":id,"connection_id":f.connection_id,"lifetime_seconds":300})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data["host_binding"],
        "owner_registered_not_real_host_verified"
    );
    data["session"].clone()
}
#[tokio::test]
async fn owner_controls_need_explicit_feature_owner_credential_and_same_origin() {
    let disabled = setup(false);
    assert_ne!(
        call(&disabled, &format!("{ROOT}/sessions"), None).await.0,
        StatusCode::OK
    );
    let f = setup(true);
    let body = json!({"request_id":"one","connection_id":f.connection_id,"lifetime_seconds":300})
        .to_string();
    assert_eq!(
        raw(
            &f,
            &format!("{ROOT}/sessions"),
            Some(body.clone()),
            &f.client_token,
            &format!("http://{HOST}")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw(
            &f,
            &format!("{ROOT}/sessions"),
            Some(body),
            &f.owner_token,
            "http://evil.invalid"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&f, &format!("{ROOT}/sessions"), None).await.1["items"],
        json!([])
    );
}
#[tokio::test]
async fn owner_create_metadata_list_cancel_and_session_close_do_not_claim_or_grant_actions() {
    let f = setup(true);
    let from = register(&f, "from").await;
    let target = register(&f, "target").await;
    assert_eq!(register(&f, "target").await, target);
    let input = json!({"request_id":"handoff","from":from,"recipient":target,"lifetime_seconds":180,"memories":[f.memory],"content":{"summary":"synthetic handoff","completed":[],"open_tasks":["test continuation"],"decisions":[],"failed_attempts":[],"constraints":[],"next_steps":["Untrusted task text grants no action"]}});
    let (status, out) = call(&f, &format!("{ROOT}/handoffs"), Some(input)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["confers_execution_authority"], false);
    assert_eq!(out["handoff"]["state"], "pending");
    let id = out["handoff"]["handoff_id"].as_str().unwrap();
    let target_id = target["session_id"].as_str().unwrap();
    let (status, list) = call(&f, &format!("{ROOT}/handoffs?session_id={target_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains("synthetic handoff"));
    assert_eq!(
        call(&f, &format!("{ROOT}/handoffs/{id}/cancel"), Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f, &format!("{ROOT}/handoffs?session_id={target_id}"), None)
            .await
            .1["items"],
        json!([])
    );
    assert_eq!(
        call(&f, &format!("{ROOT}/sessions/close"), Some(target))
            .await
            .0,
        StatusCode::OK
    );
    let inventory = call(&f, &format!("{ROOT}/sessions"), None).await.1;
    assert!(
        inventory["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r[1]["closed"] == true)
    );
    assert!(!inventory.to_string().contains(&f.client_token));
    assert_ne!(
        call(&f, &format!("{ROOT}/handoffs/{id}/claim"), Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn owner_inputs_are_strict_and_cannot_bind_another_capture_actor_as_session_history() {
    let f = setup(true);
    let duplicate = format!(
        "{{\"request_id\":\"one\",\"request_id\":\"two\",\"connection_id\":\"{}\",\"lifetime_seconds\":300}}",
        f.connection_id
    );
    assert_eq!(
        raw(
            &f,
            &format!("{ROOT}/sessions"),
            Some(duplicate),
            &f.owner_token,
            &format!("http://{HOST}")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(call(&f,&format!("{ROOT}/sessions"),Some(json!({"request_id":"one","connection_id":f.connection_id,"lifetime_seconds":300,"scope":{"owner_id":"other"}}))).await.0,StatusCode::BAD_REQUEST);
    let session = register(&f, "target").await;
    assert_eq!(
        call(
            &f,
            &format!("{ROOT}/events"),
            Some(json!({"session":session,"sequence":1,"source":f.source}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

fn captured_fixture(f: &Fixture) -> String {
    let key = VaultKey::read_file(&f._dir.path().join("vault.key")).unwrap();
    let mut store = IntakeStore::open(&f._dir.path().join("intake.sqlite"), key, scope()).unwrap();
    store.enable_capture(&scope()).unwrap();
    let grant = store
        .create_capture_grant(
            &scope(),
            CaptureGrantRequest {
                request_id: "capture".into(),
                source_namespace: "synthetic-codex".into(),
                host: "codex".into(),
                surface: "cli_hooks".into(),
                host_version: "synthetic".into(),
                events: vec![CaptureEventKind::UserPrompt],
                lifetime_seconds: 600,
                event_budget: 10,
                byte_budget: 4096,
            },
        )
        .unwrap();
    let access = store.authenticate_capture(&scope(), &grant.token).unwrap();
    let session = store
        .register_capture_session(&scope(), &access, "fixture-session")
        .unwrap();
    let ack = store
        .capture_event(
            &scope(),
            &access,
            &serde_json::to_vec(&CaptureEnvelope {
                schema_version: CAPTURE_VERSION.into(),
                session_id: session.session_id.clone(),
                event_id: "one".into(),
                sequence: 1,
                kind: CaptureEventKind::UserPrompt,
                content: "PRIVATE_CAPTURE_BODY_NOT_IN_METADATA".into(),
                occurred_at: None,
                loss: CaptureLoss {
                    redacted: false,
                    truncated: false,
                    partial_history: true,
                },
            })
            .unwrap(),
        )
        .unwrap();
    store
        .record_capture_provenance(
            &scope(),
            &access,
            CaptureProvenanceRequest {
                session_id: session.session_id,
                event_id: "one".into(),
                kind: CaptureEventKind::UserPrompt,
                origin: CaptureDeliveryOrigin::UserProvidedExport,
            },
        )
        .unwrap();
    ack.event_key
}

#[tokio::test]
async fn captured_sources_owner_link_and_remove_preserve_authority_and_reject_stale_scope() {
    let disabled = setup(false);
    assert_ne!(
        call(&disabled, &format!("{ROOT}/captured-sources"), None)
            .await
            .0,
        StatusCode::OK
    );
    let f = setup(true);
    let event_key = captured_fixture(&f);
    let session = register(&f, "recipient").await;
    let path = format!("{ROOT}/source-links");
    let input = json!({"event_key":event_key,"recipient_session":session});
    for (token, origin) in [
        (&f.client_token, format!("http://{HOST}")),
        (&f.owner_token, "http://other.invalid".into()),
    ] {
        assert_eq!(
            raw(&f, &path, Some(input.to_string()), token, &origin)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        raw(
            &f,
            &format!("{ROOT}/captured-sources"),
            None,
            &f.client_token,
            ""
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, inventory) = call(&f, &format!("{ROOT}/captured-sources"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inventory["items"].as_array().unwrap().len(), 1);
    assert!(!inventory.to_string().contains("PRIVATE_CAPTURE_BODY"));
    assert_eq!(inventory["full_history"], false);
    assert_ne!(
        call(&f, &format!("{ROOT}/captured-sources?before=-1"), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, link) = call(&f, &path, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(link["confers_read_authority"], false);
    assert_eq!(link["recap_completed"], false);
    assert_eq!(
        call(&f, &path, Some(input.clone())).await.1["association_id"],
        link["association_id"]
    );
    let listing = format!(
        "{path}?session_id={}",
        session["session_id"].as_str().unwrap()
    );
    assert_eq!(
        call(&f, &listing, None).await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut forged = input.clone();
    forged["recipient_session"]["connection_id"] = json!("other");
    assert_ne!(call(&f, &path, Some(forged)).await.0, StatusCode::OK);
    let remove = format!("{path}/{}/remove", link["association_id"].as_str().unwrap());
    assert_ne!(
        call(&f, &remove, Some(json!({"extra":true}))).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&f, &remove, Some(json!({}))).await.1["removed"], true);
    assert_eq!(call(&f, &remove, Some(json!({}))).await.1["removed"], false);
    assert_eq!(call(&f, &path, Some(input.clone())).await.0, StatusCode::OK);
    let source = inventory["items"][0]["source"]["object_id"]
        .as_str()
        .unwrap();
    let key = VaultKey::read_file(&f._dir.path().join("vault.key")).unwrap();
    let mut store = IntakeStore::open(&f._dir.path().join("intake.sqlite"), key, scope()).unwrap();
    assert_eq!(store.connections(&scope()).unwrap()[0].used_calls, 0);
    store.withdraw_source(&scope(), source).unwrap();
    drop(store);
    assert!(
        call(&f, &format!("{ROOT}/captured-sources"), None).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        call(&f, &listing, None).await.1["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_ne!(call(&f, &path, Some(input)).await.0, StatusCode::OK);
    assert_eq!(call(&f, &remove, Some(json!({}))).await.1["removed"], true);
    call(&f, &format!("{ROOT}/sessions/close"), Some(session)).await;
    assert_ne!(call(&f, &listing, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn task_cards_and_resume_preview_need_owner_and_never_disclose_task_text_in_request() {
    let f = setup(true);
    let from = register(&f, "card-from").await;
    let target = register(&f, "card-target").await;
    let input = json!({"request_id":"card","from":from,"recipient":target,"lifetime_seconds":180,"memories":[f.memory],"content":{"summary":"SYNTHETIC PRIVATE TASK TEXT","completed":["local fixture"],"open_tasks":[],"decisions":[],"failed_attempts":[],"constraints":[],"next_steps":["untrusted task step"]}});
    let (_, out) = call(&f, &format!("{ROOT}/handoffs"), Some(input)).await;
    let id = out["handoff"]["handoff_id"].as_str().unwrap();
    let route = format!("{ROOT}/tasks/{id}/prepare-resume");
    let prepare = json!({"expected_revision":1,"recipient":target});
    let (status, cards) = call(&f, &format!("{ROOT}/tasks"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cards["items"][0]["card"]["envelope"]["content"]["summary"], "SYNTHETIC PRIVATE TASK TEXT");
    assert_eq!(cards["items"][0]["resume_supported"], true);
    let (status, preview) = call(&f, &route, Some(prepare.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["reference"]["recipient"]["connection_id"], f.connection_id);
    assert_eq!(preview["reference"]["recipient_label"], "fixture");
    assert_eq!(preview["model_called"], false);
    assert_eq!(preview["target_contacted"], false);
    assert_eq!(preview["claim_performed"], false);
    assert!(!preview.to_string().contains("PRIVATE TASK TEXT"));
    assert!(!preview.to_string().contains(&f.client_token));
    assert!(preview["request"].as_str().unwrap().contains("startup_brief"));
    assert_eq!(raw(&f,&format!("{ROOT}/tasks"),None,&f.client_token,&format!("http://{HOST}")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(raw(&f,&route,Some(prepare.to_string()),&f.owner_token,"http://evil.invalid").await.0, StatusCode::FORBIDDEN);
    let mut extra = prepare.clone(); extra["token"] = json!("forged");
    assert_ne!(call(&f,&route,Some(extra)).await.0,StatusCode::OK);
    let mut wrong = prepare.clone(); wrong["expected_revision"] = json!(2);
    assert_ne!(call(&f,&route,Some(wrong)).await.0,StatusCode::OK);
    let list = call(&f,&format!("{ROOT}/tasks"),None).await.1;
    assert_eq!(list["items"][0]["card"]["envelope"]["metadata"]["state"],"pending");
    call(&f,&format!("/console/api/owner/intake/connections/{}/revoke",f.connection_id),Some(json!({}))).await;
    assert_eq!(call(&f,&format!("{ROOT}/tasks"),None).await.1["items"],json!([]));
    assert_ne!(call(&f,&route,Some(prepare)).await.0,StatusCode::OK);
    assert_ne!(call(&setup(false),&format!("{ROOT}/tasks"),None).await.0,StatusCode::OK);
}

#[tokio::test]
async fn remote_gateway_is_not_presented_as_a_working_continuity_adapter() {
    for host in [ConnectionHost::Chatgpt, ConnectionHost::Grok, ConnectionHost::Other] {
        let f = setup_host(true, host);
        let from = register(&f,"from").await;
        let target = register(&f,"target").await;
        let input=json!({"request_id":"remote-task","from":from,"recipient":target,"lifetime_seconds":120,"memories":[f.memory],"content":{"summary":"synthetic remote task","completed":[],"open_tasks":[],"decisions":[],"failed_attempts":[],"constraints":[],"next_steps":[]}});
        let (_, out)=call(&f,&format!("{ROOT}/handoffs"),Some(input)).await;
        let id=out["handoff"]["handoff_id"].as_str().unwrap();
        let list=call(&f,&format!("{ROOT}/tasks"),None).await.1;
        assert_eq!(list["items"][0]["resume_supported"],false);
        assert_ne!(call(&f,&format!("{ROOT}/tasks/{id}/prepare-resume"),Some(json!({"expected_revision":1,"recipient":target}))).await.0,StatusCode::OK);
    }
}

#[tokio::test]
async fn owner_task_goal_and_clarification_work_without_session_forms_or_grant_changes() {
    let f=setup(true);
    let options=call(&f,&format!("{ROOT}/task-options?memory_id={}&revision={}",f.memory.object_id,f.memory.revision),None).await;
    assert_eq!(options.0,StatusCode::OK);assert_eq!(options.1["items"][0]["id"],f.connection_id);assert!(!options.1.to_string().contains(&f.client_token));
    assert_eq!(call(&f,"/console/api/owner/intake/units",None).await.1["continuity_available"],true);
    let create=json!({"request_id":"natural-goal","memory":f.memory,"connection_id":f.connection_id,"instruction":"合成任务：请先核对测试时区"});
    let (status,out)=call(&f,&format!("{ROOT}/tasks"),Some(create.clone())).await;assert_eq!(status,StatusCode::OK);assert_eq!(out["authority_changed"],false);assert_eq!(out["model_called"],false);assert_eq!(out["host_binding"],"owner_workspace_task_not_real_host_verified");
    let old=out["handoff"].clone();assert_eq!(call(&f,&format!("{ROOT}/tasks"),Some(create)).await.1["handoff"],old);
    let amend=json!({"request_id":"natural-clarification","expected_revision":old["revision"],"recipient":old["recipient"],"instruction":"只用于测试环境，<b>时区未定</b>"});
    let route=format!("{ROOT}/tasks/{}/amend",old["handoff_id"].as_str().unwrap());
    let (status,updated)=call(&f,&route,Some(amend.clone())).await;assert_eq!(status,StatusCode::OK);assert_eq!(updated["handoff"]["expires_at"],old["expires_at"]);assert_eq!(updated["previous_request_invalidated"],true);
    assert_eq!(call(&f,&route,Some(amend)).await.1["handoff"],updated["handoff"]);
    let cards=call(&f,&format!("{ROOT}/tasks"),None).await.1;assert_eq!(cards["items"].as_array().unwrap().len(),1);assert_eq!(cards["items"][0]["card"]["envelope"]["owner_notes"][1]["text"],"只用于测试环境，<b>时区未定</b>");
    assert_eq!(call(&f,&format!("{ROOT}/tasks/{}/prepare-resume",old["handoff_id"].as_str().unwrap()),Some(json!({"expected_revision":1,"recipient":old["recipient"]}))).await.0,StatusCode::FORBIDDEN);
    let state_route=format!("{ROOT}/tasks/{}/edit-state",updated["handoff"]["handoff_id"].as_str().unwrap());
    let edit=call(&f,&state_route,None).await;assert_eq!(edit.0,StatusCode::OK);assert!(!edit.1.to_string().contains("<b>时区未定</b>"));
    assert_eq!(raw(&f,&state_route,None,&f.client_token,&format!("http://{HOST}")).await.0,StatusCode::FORBIDDEN);
    let usage=call(&f,"/console/api/owner/intake/connections",None).await.1;assert_eq!(usage["items"].as_array().unwrap().len(),1);assert_eq!(usage["items"][0]["used_calls"],0);
}
#[tokio::test]
async fn natural_task_routes_preserve_owner_feature_origin_and_strict_input_boundaries() {
    let f=setup(true);let body=json!({"request_id":"goal","memory":f.memory,"connection_id":f.connection_id,"instruction":"synthetic"});let route=format!("{ROOT}/tasks");
    assert_eq!(raw(&f,&route,Some(body.to_string()),&f.client_token,&format!("http://{HOST}")).await.0,StatusCode::FORBIDDEN);
    assert_eq!(raw(&f,&route,Some(body.to_string()),&f.owner_token,"http://evil.invalid").await.0,StatusCode::FORBIDDEN);
    let mut injected=body.clone();injected["lifetime_seconds"]=json!(86400);assert_ne!(call(&f,&route,Some(injected)).await.0,StatusCode::OK);
    let mut empty=body.clone();empty["instruction"]=json!(" ");assert_ne!(call(&f,&route,Some(empty)).await.0,StatusCode::OK);
    let mut stale=body.clone();stale["memory"]["revision"]=json!(999);assert_ne!(call(&f,&route,Some(stale)).await.0,StatusCode::OK);
    let disabled=setup(false);let input=json!({"request_id":"goal","memory":disabled.memory,"connection_id":disabled.connection_id,"instruction":"synthetic"});assert_ne!(call(&disabled,&route,Some(input)).await.0,StatusCode::OK);
    assert_eq!(call(&disabled,"/console/api/owner/intake/units",None).await.1["continuity_available"],false);
    let remote=setup_host(true,ConnectionHost::Grok);let input=json!({"request_id":"remote-goal","memory":remote.memory,"connection_id":remote.connection_id,"instruction":"synthetic"});assert_ne!(call(&remote,&route,Some(input)).await.0,StatusCode::OK);
    assert_eq!(call(&f,&format!("{ROOT}/sessions"),None).await.1["items"],json!([]));
}
