//! Owner graph HTTP acceptance uses synthetic records and direct router calls only.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::*, intake::*, *};
use nova_review::ReviewServer;
use serde_json::{Value, json};
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:31445";
const ROOT: &str = "/console/api/owner/intake";
const GRAPH: &str = "/console/api/owner/intake/graph";

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:synthetic".into(),
        space: "graph-http".into(),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    router: axum::Router,
    token: String,
    store: IntakeStore,
}

fn setup(native: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = VaultKey::generate().unwrap();
    let key_path = dir.path().join("vault.key");
    key.write_new_file(&key_path).unwrap();
    let legacy = dir.path().join("legacy.sqlite");
    drop(Vault::create(&legacy, key.clone()).unwrap());
    let owner_path = dir.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner_path)
        .unwrap();
    let token = std::fs::read_to_string(&owner_path)
        .unwrap()
        .trim()
        .to_owned();
    let intake_path = dir.path().join("intake.sqlite");
    let mut store = IntakeStore::create(&intake_path, key, scope()).unwrap();
    store.enable_compilation(&scope()).unwrap();
    store
        .configure_model(
            1,
            ModelSelection::Local {
                model_id: "synthetic-fixture-not-a-real-llm".into(),
                endpoint: "http://127.0.0.1:31446/v1".into(),
            },
        )
        .unwrap();
    let mut server = ReviewServer::new(legacy, key_path, HOST.into(), true)
        .with_owner(scope().owner_id, scope().space)
        .with_owner_token(&owner_path)
        .unwrap()
        .with_intake_store(&intake_path)
        .unwrap();
    if native {
        server = server.with_native_workspace().unwrap();
    }
    Fixture {
        _dir: dir,
        router: server.router(),
        token,
        store,
    }
}

async fn request(
    router: &axum::Router,
    uri: &str,
    token: Option<&str>,
    host: &str,
    origin: Option<&str>,
    review_header: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().uri(uri).header("Host", host);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.header("Origin", origin);
    }
    if review_header {
        builder = builder.header("X-Nova-Review", "owner-review");
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    // Query rejection can be Axum's plain text; successful projections must be JSON.
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8(bytes.to_vec()).unwrap()));
    (status, body)
}

async fn graph(fixture: &Fixture, query: &str) -> (StatusCode, Value) {
    request(
        &fixture.router,
        &format!("{GRAPH}{query}"),
        Some(&fixture.token),
        HOST,
        None,
        true,
    )
    .await
}

fn compile(
    store: &mut IntakeStore,
    request_id: &str,
    subject: &str,
    value: &str,
    privacy_level: PrivacyLevel,
) -> (IntakeReceipt, CommittedCompilation) {
    compile_semantics(
        store,
        request_id,
        subject,
        value,
        privacy_level,
        "positive",
        "asserted",
    )
}

fn compile_semantics(
    store: &mut IntakeStore,
    request_id: &str,
    subject: &str,
    value: &str,
    privacy_level: PrivacyLevel,
    polarity: &str,
    modality: &str,
) -> (IntakeReceipt, CommittedCompilation) {
    let input = json!({
        "schema_version": INTAKE_VERSION,
        "request_id": request_id,
        "source_platform": "synthetic-graph-fixture",
        "capture_fidelity": "f2_visible_transcript",
        "content": "SYNTHETIC graph HTTP evidence",
        "occurred_at": null
    });
    let receipt = store
        .receive(
            &IntakeAuthority {
                scope: scope(),
                actor_id: "synthetic-owner".into(),
                capture_allowed: true,
                role: SourceRole::User,
                privacy_level,
            },
            &serde_json::to_vec(&input).unwrap(),
        )
        .unwrap();
    let claimed = store.claim_next(&scope(), 60).unwrap().unwrap();
    let lease_id = claimed.lease.unwrap().lease_id;
    store
        .prepare_compilation(&scope(), &claimed.job.job_id, &lease_id)
        .unwrap();
    let attempt = store
        .authorize_dispatch(&scope(), &claimed.job.job_id, &lease_id, 1000)
        .unwrap();
    let output = json!({
        "schema_version": EXTRACTION_VERSION,
        "units": [{
            "origin": "extracted",
            "content": {
                "subject": subject,
                "predicate": "project:deployment",
                "value": value,
                "polarity": polarity,
                "modality": modality,
                "conditions": {}
            },
            "occurred_at": null,
            "valid_time": {"kind": "unknown"},
            "quotes": [{"quote": "SYNTHETIC graph HTTP evidence", "occurrence": 0}]
        }]
    });
    let committed = store
        .commit_compilation(
            &scope(),
            &attempt.job_id,
            &attempt.lease_id,
            &serde_json::to_vec(&output).unwrap(),
        )
        .unwrap();
    (receipt, committed)
}

#[tokio::test]
async fn graph_requires_opt_in_and_owner_boundary_on_every_get() {
    let legacy = setup(false);
    assert_ne!(graph(&legacy, "").await.0, StatusCode::OK);

    let mut fixture = setup(true);
    let issued = fixture
        .store
        .create_connection(
            &scope(),
            ConnectionRequest {
                request_id: "graph-client-boundary".into(),
                label: "Synthetic graph client".into(),
                host: ConnectionHost::Codex,
                preset: ConnectionPreset::ReadOnly,
                topic_id: None,
                include_future: true,
            },
        )
        .unwrap();
    for (token, host, origin, review_header) in [
        (None, HOST, None, true),
        (Some(issued.token.as_str()), HOST, None, true),
        (Some(fixture.token.as_str()), "evil.invalid", None, true),
        (
            Some(fixture.token.as_str()),
            HOST,
            Some("http://evil.invalid"),
            true,
        ),
        (Some(fixture.token.as_str()), HOST, None, false),
    ] {
        let (status, body) =
            request(&fixture.router, GRAPH, token, host, origin, review_header).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "NOVA_REVIEW_BOUNDARY_DENIED");
        assert!(body.get("nodes").is_none());
    }
    let (status, body) = graph(&fixture, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["nodes"].as_array().unwrap().is_empty());
    assert!(body["edges"].as_array().unwrap().is_empty());
    assert_eq!(body["partial"], false);
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn graph_query_cannot_override_scope_or_escape_bounded_projection() {
    let fixture = setup(true);
    for query in [
        "?owner_id=owner:other",
        "?space=personal",
        "?include_raw=true",
        "?limit=0",
        "?limit=5",
        "?limit=not-a-number",
        "?limit=1&limit=2",
        "?cursor=forged",
        "?query=%0A",
        "?query=%09local",
    ] {
        let (status, body) = graph(&fixture, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert!(body.get("nodes").is_none());
    }
    for query in [
        format!("?query={}", "a".repeat(129)),
        format!("?cursor={}", "a".repeat(513)),
    ] {
        assert_eq!(graph(&fixture, &query).await.0, StatusCode::BAD_REQUEST);
    }
}

fn nodes_of<'a>(projection: &'a Value, kind: &str) -> Vec<&'a Value> {
    projection["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["kind"] == kind)
        .collect()
}

fn assert_graph_integrity(projection: &Value) {
    let nodes = projection["nodes"].as_array().unwrap();
    let ids = nodes
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), nodes.len());
    for edge in projection["edges"].as_array().unwrap() {
        assert!(ids.contains(edge["from"].as_str().unwrap()));
        assert!(ids.contains(edge["to"].as_str().unwrap()));
        assert!(!edge["explanation"].as_str().unwrap().is_empty());
    }
    assert_eq!(projection["stats"]["count_scope"], "page");
    // A graph includes compiled semantic claims; excluding original bodies is not metadata only.
    assert_eq!(projection["stats"]["metadata_only"], false);
    assert_eq!(projection["stats"]["source_text_included"], false);
    assert_eq!(
        projection["stats"]["relationships"].as_u64().unwrap(),
        projection["edges"].as_array().unwrap().len() as u64
    );
    assert!(
        !projection
            .to_string()
            .contains("SYNTHETIC graph HTTP evidence")
    );
}

#[tokio::test]
async fn graph_projects_real_deduplicated_members_and_invalidates_immediately_without_rebuild() {
    let mut fixture = setup(true);
    let (first, first_commit) = compile_semantics(
        &mut fixture.store,
        "source-one",
        "project:nova",
        "local",
        PrivacyLevel::Private,
        "negative",
        "planned",
    );
    let (second, second_commit) = compile_semantics(
        &mut fixture.store,
        "source-two",
        "project:nova",
        "local",
        PrivacyLevel::Private,
        "negative",
        "planned",
    );
    let (third, third_commit) = compile(
        &mut fixture.store,
        "source-three",
        "project:nova",
        "cloud",
        PrivacyLevel::Private,
    );
    fixture.store.consolidate_pending(&scope(), 4).unwrap();
    let (status, projected) = graph(&fixture, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_graph_integrity(&projected);
    assert_eq!(nodes_of(&projected, "topic").len(), 1);
    assert_eq!(nodes_of(&projected, "memory").len(), 2);
    assert_eq!(nodes_of(&projected, "source").len(), 3);
    assert_eq!(projected["stats"]["extraction_records"], 3);
    let duplicated_claim = nodes_of(&projected, "memory")
        .into_iter()
        .find(|node| node["member_count"] == 2)
        .unwrap();
    assert_eq!(duplicated_claim["polarity"], "negative");
    assert_eq!(duplicated_claim["modality"], "planned");
    let member_refs = nodes_of(&projected, "memory")
        .iter()
        .flat_map(|node| node["members"].as_array().unwrap())
        .map(|member| member["object_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        member_refs,
        [
            first_commit.output_versions[0].object_id.as_str(),
            second_commit.output_versions[0].object_id.as_str(),
            third_commit.output_versions[0].object_id.as_str(),
        ]
        .into_iter()
        .collect()
    );
    let old_generation = projected["generation"].clone();

    // The production Owner endpoint withdraws the first support. No Wiki rebuild follows.
    let withdraw = format!("{ROOT}/sources/{}/withdraw", first.source.object_id);
    let status = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(withdraw)
                .header("Host", HOST)
                .header("Origin", format!("http://{HOST}"))
                .header("X-Nova-Review", "owner-review")
                .header("Authorization", format!("Bearer {}", fixture.token))
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);
    let (status, after_withdrawal) = graph(&fixture, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_graph_integrity(&after_withdrawal);
    assert_ne!(after_withdrawal["generation"], old_generation);
    assert_eq!(nodes_of(&after_withdrawal, "memory").len(), 2);
    assert_eq!(nodes_of(&after_withdrawal, "source").len(), 2);
    assert_eq!(after_withdrawal["stats"]["extraction_records"], 2);
    assert!(
        !after_withdrawal
            .to_string()
            .contains(&first.source.object_id)
    );
    assert!(
        after_withdrawal
            .to_string()
            .contains(&second.source.object_id)
    );

    fixture
        .store
        .delete_source(&scope(), &third.source.object_id)
        .unwrap();
    let (status, after_delete) = graph(&fixture, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_graph_integrity(&after_delete);
    assert_eq!(nodes_of(&after_delete, "memory").len(), 1);
    assert_eq!(nodes_of(&after_delete, "source").len(), 1);
    assert!(!after_delete.to_string().contains(&third.source.object_id));
    assert!(!after_delete.to_string().contains("cloud"));

    fixture
        .store
        .delete_source(&scope(), &second.source.object_id)
        .unwrap();
    let (_, empty) = graph(&fixture, "").await;
    assert!(empty["nodes"].as_array().unwrap().is_empty());
    assert!(empty["edges"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn graph_pages_are_bounded_and_cursor_cannot_survive_library_change() {
    let mut fixture = setup(true);
    for (request_id, subject) in [
        ("topic-one", "project:one"),
        ("topic-two", "project:two"),
        ("topic-three", "project:three"),
    ] {
        compile(
            &mut fixture.store,
            request_id,
            subject,
            "local",
            PrivacyLevel::Private,
        );
    }
    let (status, first_page) = graph(&fixture, "?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_graph_integrity(&first_page);
    assert_eq!(nodes_of(&first_page, "topic").len(), 1);
    assert_eq!(first_page["stats"]["topics"], 1);
    assert_eq!(first_page["partial"], true);
    let first_cursor = first_page["next_cursor"].as_str().unwrap().to_owned();
    assert!(
        first_cursor
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
    );
    for changed_request in [
        format!("?limit=2&cursor={first_cursor}"),
        format!("?limit=1&query=local&cursor={first_cursor}"),
    ] {
        let (status, rejected) = graph(&fixture, &changed_request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(rejected.get("nodes").is_none());
    }
    let mut all_topics = nodes_of(&first_page, "topic")
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut cursor = Some(first_cursor.clone());
    for _ in 0..3 {
        let Some(next) = cursor.take() else { break };
        let (status, page) = graph(&fixture, &format!("?limit=1&cursor={next}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_graph_integrity(&page);
        assert_eq!(nodes_of(&page, "topic").len(), 1);
        for node in nodes_of(&page, "topic") {
            assert!(all_topics.insert(node["id"].as_str().unwrap().to_owned()));
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(all_topics.len(), 3);
    assert!(cursor.is_none());

    compile(
        &mut fixture.store,
        "topic-four",
        "project:four",
        "local",
        PrivacyLevel::Private,
    );
    let (status, stale) = graph(&fixture, &format!("?limit=1&cursor={first_cursor}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(stale.get("nodes").is_none());
}

#[tokio::test]
async fn uncompiled_sources_and_sensitive_source_metadata_do_not_create_graph_memories() {
    let mut fixture = setup(true);
    let mut source_ids = vec![];
    for (request_id, privacy_level) in [
        ("pending-private", PrivacyLevel::Private),
        ("pending-sensitive", PrivacyLevel::Sensitive),
        ("pending-sealed", PrivacyLevel::Sealed),
    ] {
        let receipt = fixture
            .store
            .receive(
                &IntakeAuthority {
                    scope: scope(),
                    actor_id: "synthetic-owner".into(),
                    capture_allowed: true,
                    role: SourceRole::User,
                    privacy_level,
                },
                &serde_json::to_vec(&json!({
                    "schema_version": INTAKE_VERSION,
                    "request_id": request_id,
                    "source_platform": "synthetic-graph-fixture",
                    "capture_fidelity": "f2_visible_transcript",
                    "content": "UNCOMPILED-SENSITIVE-RAW-BODY",
                    "occurred_at": null
                }))
                .unwrap(),
            )
            .unwrap();
        source_ids.push(receipt.source.object_id);
    }
    let (status, projection) = graph(&fixture, "").await;
    assert_eq!(status, StatusCode::OK);
    assert_graph_integrity(&projection);
    assert!(projection["nodes"].as_array().unwrap().is_empty());
    for field in [
        "topics",
        "memories",
        "sources",
        "extraction_records",
        "relationships",
    ] {
        assert_eq!(projection["stats"][field], 0, "{field}");
    }
    let serialized = projection.to_string();
    assert!(!serialized.contains("UNCOMPILED-SENSITIVE-RAW-BODY"));
    for id in source_ids {
        assert!(!serialized.contains(&id));
    }
}
