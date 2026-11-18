use nova_core::{compilation::*, intake::*, *};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn scope() -> AssetScope {
    AssetScope {
        owner_id: "owner:graph-synthetic".into(),
        space: "test".into(),
    }
}
fn authority(privacy_level: PrivacyLevel) -> IntakeAuthority {
    IntakeAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        capture_allowed: true,
        role: SourceRole::User,
        privacy_level,
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, VaultKey, IntakeStore) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("graph.sqlite");
    let key = VaultKey::generate().unwrap();
    let mut s = IntakeStore::create(&path, key.clone(), scope()).unwrap();
    s.enable_compilation(&scope()).unwrap();
    s.configure_model(
        1,
        ModelSelection::Local {
            model_id: "synthetic-not-an-llm".into(),
            endpoint: "http://127.0.0.1:11434/v1".into(),
        },
    )
    .unwrap();
    (d, path, key, s)
}
fn receive(s: &mut IntakeStore, id: &str, privacy: PrivacyLevel) -> IntakeReceipt {
    s.receive(&authority(privacy), &serde_json::to_vec(&json!({"schema_version":INTAKE_VERSION,"request_id":id,"source_platform":"graph-fixture","capture_fidelity":"f2_visible_transcript","content":"SYNTHETIC original evidence. Never copy this full body into graph.","occurred_at":null})).unwrap()).unwrap()
}
fn output(topic: &str, value: Value, conditions: Value, valid_time: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":EXTRACTION_VERSION,"units":[{"origin":"extracted","content":{"subject":topic,"predicate":"project:deployment","value":value,"polarity":"positive","modality":"asserted","conditions":conditions},"occurred_at":null,"valid_time":valid_time,"quotes":[{"quote":"SYNTHETIC original evidence","occurrence":0}]}]})).unwrap()
}
fn prepared(s: &mut IntakeStore) -> CompilationAttempt {
    let job = s.claim_next(&scope(), 60).unwrap().unwrap();
    let lease = job.lease.unwrap();
    s.prepare_compilation(&scope(), &job.job.job_id, &lease.lease_id)
        .unwrap();
    s.authorize_dispatch(&scope(), &job.job.job_id, &lease.lease_id, 10_000)
        .unwrap()
}
fn ingest(
    s: &mut IntakeStore,
    id: &str,
    topic: &str,
    value: Value,
) -> (IntakeReceipt, CommittedCompilation) {
    let r = receive(s, id, PrivacyLevel::Private);
    let a = prepared(s);
    let c = s
        .commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            &output(topic, value, json!({}), json!({"kind":"unknown"})),
        )
        .unwrap();
    (r, c)
}
fn graph(s: &IntakeStore) -> GraphProjection {
    s.owner_graph(&scope(), &GraphRequest::default()).unwrap()
}
fn consolidate_all(s: &mut IntakeStore) {
    while s.consolidation_queue(&scope()).unwrap().pending > 0 {
        s.consolidate_pending(&scope(), 4).unwrap();
    }
}
fn no_dangling(g: &GraphProjection) {
    let ids = g
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), g.nodes.len());
    assert!(
        g.edges
            .iter()
            .all(|e| ids.contains(e.from.as_str()) && ids.contains(e.to.as_str()))
    );
    assert!(g.nodes.len() <= MAX_GRAPH_NODES && g.edges.len() <= MAX_GRAPH_EDGES);
    assert!(serde_json::to_vec(g).unwrap().len() <= MAX_GRAPH_BYTES);
}

#[test]
fn empty_and_uncompiled_sources_are_honest_and_do_not_expose_sensitive_originals() {
    let (_d, _p, _k, mut s) = setup();
    let empty = graph(&s);
    assert!(empty.nodes.is_empty() && empty.edges.is_empty());
    assert_eq!(empty.stats.memories, 0);
    let sensitive = receive(&mut s, "sensitive", PrivacyLevel::Sensitive);
    let pending = graph(&s);
    assert_ne!(empty.generation, pending.generation);
    assert!(pending.nodes.is_empty() && !pending.partial);
    assert!(pending.notices.iter().any(|n| n.contains("uncompiled")));
    let encoded = serde_json::to_string(&pending).unwrap();
    assert!(!encoded.contains("SYNTHETIC") && !encoded.contains(&sensitive.source.object_id));
    assert!(!pending.stats.metadata_only && !pending.stats.source_text_included);
    assert_eq!(pending.stats.count_scope, "page");
}

#[test]
fn exact_duplicate_records_enrich_one_memory_without_knowledge_inflation() {
    let (_d, _p, _k, mut s) = setup();
    let (first, _) = ingest(&mut s, "first", "project:nova", json!("local"));
    let (second, _) = ingest(&mut s, "second", "project:nova", json!("local"));
    consolidate_all(&mut s);
    let g = graph(&s);
    no_dangling(&g);
    assert_eq!(
        (
            g.stats.memories,
            g.stats.extraction_records,
            g.stats.sources,
            g.stats.topics
        ),
        (1, 2, 2, 1)
    );
    let memory = g.nodes.iter().find(|n| n.kind == "memory").unwrap();
    assert_eq!(memory.member_count, 2);
    assert_eq!(memory.source_count, 2);
    assert!(
        memory.source_refs.contains(&first.source) && memory.source_refs.contains(&second.source)
    );
    assert!(memory.semantic_support.contains("unverified"));
    assert!(
        g.edges
            .iter()
            .all(|e| matches!(e.kind.as_str(), "in_topic" | "supported_by"))
    );
    assert!(
        !serde_json::to_string(&g)
            .unwrap()
            .contains("Never copy this full body")
    );
    assert_eq!(g.generation, graph(&s).generation);
}

#[test]
fn conditions_time_origin_and_existing_rule_associations_remain_explainable() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "local", "project:nova", json!("local"));
    receive(&mut s, "cloud", PrivacyLevel::Private);
    let a = prepared(&mut s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output(
            "project:nova",
            json!("cloud"),
            json!({"scope:context":"work"}),
            json!({"kind":"interval","start":"2026-01-01T00:00:00Z","end":null}),
        ),
    )
    .unwrap();
    let pending = graph(&s);
    assert!(pending.partial);
    assert!(
        !pending
            .edges
            .iter()
            .any(|e| e.kind == "conditional_variant")
    );
    consolidate_all(&mut s);
    let g = graph(&s);
    no_dangling(&g);
    assert_ne!(pending.generation, g.generation);
    assert_eq!(
        g.edges
            .iter()
            .filter(|e| e.kind == "conditional_variant")
            .count(),
        1
    );
    assert!(
        g.nodes
            .iter()
            .filter(|n| n.kind == "memory")
            .all(|n| n.origin == Some(SemanticOrigin::Extracted) && n.valid_time.is_some())
    );
    assert!(g.nodes.iter().any(|n| {
        n.conditions
            .as_ref()
            .is_some_and(|c| c.contains_key("scope:context"))
    }));
    assert!(
        g.edges
            .iter()
            .filter(|e| e.kind == "conditional_variant")
            .all(|e| e.explanation.contains("Conditions differ"))
    );
}

#[test]
fn scope_unknown_parameters_and_request_budgets_fail_closed() {
    let (_d, _p, _k, s) = setup();
    let mut other = scope();
    other.space = "elsewhere".into();
    assert!(s.owner_graph(&other, &GraphRequest::default()).is_err());
    for limit in [0, 5, usize::MAX] {
        assert!(
            s.owner_graph(
                &scope(),
                &GraphRequest {
                    limit,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    for query in ["a".repeat(129), "bad\nquery".into(), "\n".into()] {
        assert!(
            s.owner_graph(
                &scope(),
                &GraphRequest {
                    query: Some(query),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    assert!(serde_json::from_value::<GraphRequest>(json!({"owner_id":"elsewhere"})).is_err());
    assert!(
        s.owner_graph(
            &scope(),
            &GraphRequest {
                cursor: Some("x".repeat(513)),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn keyword_filter_uses_structured_content_not_raw_source_and_exposes_no_dangling_links() {
    let (_d, _p, _k, mut s) = setup();
    ingest(&mut s, "a", "project:nova", json!("searchable-local"));
    ingest(&mut s, "b", "project:nova", json!("cloud"));
    consolidate_all(&mut s);
    let g = s
        .owner_graph(
            &scope(),
            &GraphRequest {
                query: Some("SEARCHABLE".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(g.stats.memories, 1);
    assert_eq!(g.stats.sources, 1);
    no_dangling(&g);
    assert!(!g.edges.iter().any(|e| e.kind == "potential_conflict"));
    let absent = s
        .owner_graph(
            &scope(),
            &GraphRequest {
                query: Some("Never copy this full body".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(absent.nodes.is_empty());
}

#[test]
fn pagination_is_signed_request_bound_and_stale_after_new_source_or_current_version_changes() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..3 {
        ingest(
            &mut s,
            &format!("r{i}"),
            &format!("project:p{i}"),
            json!("local"),
        );
    }
    consolidate_all(&mut s);
    let first = s
        .owner_graph(
            &scope(),
            &GraphRequest {
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let cursor = first.next_cursor.clone().unwrap();
    let second = s
        .owner_graph(
            &scope(),
            &GraphRequest {
                limit: 1,
                cursor: Some(cursor.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(first.generation, second.generation);
    assert_ne!(first.nodes[0].id, second.nodes[0].id);
    for request in [
        GraphRequest {
            limit: 2,
            cursor: Some(cursor.clone()),
            ..Default::default()
        },
        GraphRequest {
            limit: 1,
            query: Some("other".into()),
            cursor: Some(cursor.clone()),
        },
        GraphRequest {
            limit: 1,
            cursor: Some(format!("{cursor}x")),
            ..Default::default()
        },
    ] {
        assert!(s.owner_graph(&scope(), &request).is_err());
    }
    receive(&mut s, "new-uncompiled", PrivacyLevel::Private);
    assert!(
        s.owner_graph(
            &scope(),
            &GraphRequest {
                limit: 1,
                cursor: Some(cursor),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_ne!(first.generation, graph(&s).generation);
}

#[test]
fn corrections_undo_withdrawal_deletion_and_reopen_never_retain_obsolete_graph_content() {
    let (_d, p, k, mut s) = setup();
    let (source, commit) = ingest(&mut s, "a", "project:nova", json!("old-location"));
    consolidate_all(&mut s);
    let initial = graph(&s);
    let target = commit.output_versions[0].clone();
    let owner = CorrectionAuthority {
        scope: scope(),
        actor_id: "owner".into(),
        may_correct: true,
    };
    s.request_correction(
        &owner,
        target.clone(),
        "correction".into(),
        "SYNTHETIC original evidence".into(),
    )
    .unwrap();
    let a = prepared(&mut s);
    s.commit_compilation(
        &scope(),
        &a.job_id,
        &a.lease_id,
        &output(
            "project:nova",
            json!("new-location"),
            json!({}),
            json!({"kind":"unknown"}),
        ),
    )
    .unwrap();
    let corrected = graph(&s);
    assert_ne!(initial.generation, corrected.generation);
    assert!(
        !serde_json::to_string(&corrected)
            .unwrap()
            .contains("old-location")
    );
    let memory = corrected.nodes.iter().find(|n| n.kind == "memory").unwrap();
    assert_eq!(memory.members[0].revision, 2);
    assert_eq!(memory.protected_count, 1);
    s.undo_correction(&owner, &target.object_id, 2).unwrap();
    let undone = graph(&s);
    assert_ne!(corrected.generation, undone.generation);
    assert!(
        !serde_json::to_string(&undone)
            .unwrap()
            .contains("new-location")
    );
    assert_eq!(
        undone
            .nodes
            .iter()
            .find(|n| n.kind == "memory")
            .unwrap()
            .members[0]
            .revision,
        3
    );
    s.withdraw_source(&scope(), &source.source.object_id)
        .unwrap();
    let withdrawn = graph(&s);
    assert!(withdrawn.nodes.is_empty());
    assert_ne!(undone.generation, withdrawn.generation);
    s.delete_source(&scope(), &source.source.object_id).unwrap();
    let deleted = graph(&s);
    assert_ne!(withdrawn.generation, deleted.generation);
    assert!(deleted.nodes.is_empty());
    drop(s);
    let reopened = IntakeStore::open(&p, k, scope()).unwrap();
    assert_eq!(graph(&reopened).generation, deleted.generation);
    assert!(graph(&reopened).nodes.is_empty());
}

#[test]
fn two_hundred_real_synthetic_records_traverse_ten_pages_without_duplicate_or_missing_memories() {
    let (_d, _p, _k, mut s) = setup();
    let started = std::time::Instant::now();
    for i in 0..200 {
        ingest(
            &mut s,
            &format!("r{i}"),
            &format!("project:p{}", i / 10),
            json!(format!("value{i}")),
        );
    }
    consolidate_all(&mut s);
    let mut cursor = None;
    let mut memories = BTreeSet::new();
    let mut pages = 0;
    let mut total_records = 0;
    let scan_started = std::time::Instant::now();
    loop {
        let g = s
            .owner_graph(
                &scope(),
                &GraphRequest {
                    cursor,
                    limit: 2,
                    query: None,
                },
            )
            .unwrap();
        no_dangling(&g);
        pages += 1;
        total_records += g.stats.extraction_records;
        for n in g.nodes.iter().filter(|n| n.kind == "memory") {
            assert!(memories.insert(n.id.clone()));
        }
        cursor = g.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!((memories.len(), total_records, pages), (200, 200, 10));
    eprintln!(
        "N4 synthetic 200 records/20 topics/10 pages: projection {:?}; setup+projection {:?}; not a production benchmark",
        scan_started.elapsed(),
        started.elapsed()
    );
}

#[test]
fn oversized_partition_and_dense_relationships_are_explicitly_partial_with_safe_bounds() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..65 {
        ingest(
            &mut s,
            &format!("oversize{i}"),
            "project:oversize",
            json!(format!("value{i}")),
        );
    }
    s.consolidate_pending(&scope(), 4).unwrap();
    let omitted = graph(&s);
    assert!(omitted.partial && omitted.nodes.is_empty());
    assert!(
        omitted
            .notices
            .iter()
            .any(|n| n.contains("partition_over_64"))
    );
    let (_d, _p, _k, mut dense) = setup();
    for i in 0..46 {
        ingest(
            &mut dense,
            &format!("dense{i}"),
            "project:dense",
            json!(format!("value{i}")),
        );
    }
    consolidate_all(&mut dense);
    let g = graph(&dense);
    no_dangling(&g);
    assert_eq!(g.stats.memories, 46);
    assert_eq!(g.edges.len(), MAX_GRAPH_EDGES);
    assert!(g.partial && g.notices.iter().any(|n| n == "relationship_budget_partial"));
}

#[test]
fn oversized_structured_conditions_fail_output_budget_without_returning_partial_as_complete() {
    let (_d, _p, _k, mut s) = setup();
    let conditions = (0..16)
        .map(|i| (format!("scope:condition{i}"), json!("x".repeat(7000))))
        .collect::<serde_json::Map<String, Value>>();
    for i in 0..10 {
        receive(&mut s, &format!("large{i}"), PrivacyLevel::Private);
        let a = prepared(&mut s);
        s.commit_compilation(
            &scope(),
            &a.job_id,
            &a.lease_id,
            &output(
                "project:large",
                json!(format!("value{i}")),
                Value::Object(conditions.clone()),
                json!({"kind":"unknown"}),
            ),
        )
        .unwrap();
    }
    let error = s
        .owner_graph(&scope(), &GraphRequest::default())
        .unwrap_err();
    assert!(error.to_string().contains("graph_output_budget"));
}

#[test]
fn metadata_scan_hard_cap_fails_closed_before_loading_unbounded_or_invalid_source_bodies() {
    let (_d, p, _k, mut s) = setup();
    let original = receive(&mut s, "original", PrivacyLevel::Private);
    // Deliberately malformed encrypted records exercise the resource boundary, not legitimate
    // source intake. The metadata budget must fire before any such body can be decrypted/read.
    let mut db = rusqlite::Connection::open(p).unwrap();
    let tx = db.transaction().unwrap();
    {
        let mut insert = tx.prepare("INSERT INTO intake_objects SELECT ?1,kind,key_nonce,wrapped_key,nonce,ciphertext FROM intake_objects WHERE id=?2").unwrap();
        for i in 0..MAX_GRAPH_METADATA_ROWS {
            insert
                .execute(rusqlite::params![
                    format!("n2a:source:budget{i}"),
                    original.source.object_id
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    assert!(
        s.owner_graph(&scope(), &GraphRequest::default())
            .unwrap_err()
            .to_string()
            .contains("graph_metadata_budget")
    );
}

#[test]
fn deleted_newer_partitions_do_not_bury_live_older_memories_behind_empty_pages() {
    let (_d, _p, _k, mut s) = setup();
    for i in 0..3 {
        ingest(
            &mut s,
            &format!("live{i}"),
            &format!("project:live{i}"),
            json!(format!("retained{i}")),
        );
    }
    consolidate_all(&mut s);
    for i in 0..24 {
        let (source, _) = ingest(
            &mut s,
            &format!("deleted{i}"),
            &format!("project:deleted{i}"),
            json!("deleted-content"),
        );
        s.delete_source(&scope(), &source.source.object_id).unwrap();
    }
    let mut cursor = None;
    let mut memories = BTreeSet::new();
    let mut pages = 0;
    loop {
        let projection = s
            .owner_graph(
                &scope(),
                &GraphRequest {
                    cursor,
                    query: None,
                    limit: 1,
                },
            )
            .unwrap();
        no_dangling(&projection);
        assert_eq!(projection.stats.memories, 1);
        assert_eq!(projection.stats.topics, 1);
        assert!(
            !serde_json::to_string(&projection)
                .unwrap()
                .contains("deleted-content")
        );
        for memory in projection.nodes.iter().filter(|n| n.kind == "memory") {
            assert!(memories.insert(memory.id.clone()));
        }
        pages += 1;
        cursor = projection.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!((pages, memories.len()), (3, 3));
}
