use nova_core::{VaultKey, compilation::*, intake::*};
use serde_json::json;

fn line(turn: &str) -> CaptureVisibleHistoryLine {
    CaptureVisibleHistoryLine {
        schema_version: CAPTURE_HISTORY_VERSION.into(),
        host: "codex".into(),
        origin_session_id: "source-session".into(),
        host_turn_id: turn.into(),
        kind: CaptureEventKind::UserPrompt,
        content: "VISIBLE SECRET history marker".into(),
        occurred_at: None,
        loss: CaptureLoss {
            redacted: false,
            truncated: false,
            partial_history: true,
        },
        origin: CaptureDeliveryOrigin::UserProvidedExport,
    }
}
fn jsonl(lines: &[CaptureVisibleHistoryLine]) -> Vec<u8> {
    lines
        .iter()
        .map(|l| serde_json::to_string(l).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}
struct Fixture {
    dir: tempfile::TempDir,
    key: VaultKey,
    cfg: CaptureBootstrapConfiguration,
    store: IntakeStore,
    scope: AssetScope,
    access: CaptureAccess,
    session: CaptureSession,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = VaultKey::generate().unwrap();
        let scope = AssetScope {
            owner_id: "owner:history-fixture".into(),
            space: "synthetic".into(),
        };
        let mut store = IntakeStore::create(
            &dir.path().join("source.sqlite"),
            VaultKey::generate().unwrap(),
            scope.clone(),
        )
        .unwrap();
        store.enable_capture(&scope).unwrap();
        let grant = store
            .create_capture_grant(
                &scope,
                CaptureGrantRequest {
                    request_id: "fixture".into(),
                    source_namespace: "history-fixture".into(),
                    host: "codex".into(),
                    surface: "cli_hooks".into(),
                    host_version: "unverified".into(),
                    events: vec![CaptureEventKind::UserPrompt],
                    lifetime_seconds: 3600,
                    event_budget: 100,
                    byte_budget: 1024 * 1024,
                },
            )
            .unwrap();
        let access = store.authenticate_capture(&scope, &grant.token).unwrap();
        let session = store
            .register_capture_session(&scope, &access, "source-session")
            .unwrap();
        let cfg = CaptureBootstrapConfiguration {
            instance_id: grant.grant.instance_id,
            target_id: "loopback".into(),
            grant_id: grant.grant.grant_id,
            source_namespace: grant.grant.source_namespace,
            expires_at: grant.grant.expires_at,
            policy: CaptureRelayPolicy {
                version: "fixture".into(),
                allowed_events: vec![CaptureEventKind::UserPrompt],
                redact_literals: vec!["SECRET".into()],
                max_content_bytes: 128,
            },
            max_pending_events: 10,
            max_pending_bytes: 4096,
            max_total_events: 100,
            ttl_seconds: 300,
            lease_seconds: 10,
            max_attempts: 3,
            retry_base_seconds: 2,
            retry_max_seconds: 60,
        };
        Self {
            dir,
            key,
            cfg,
            store,
            scope,
            access,
            session,
        }
    }
    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("journal.sqlite")
    }
    fn journal(&self) -> CaptureHistoryJournal {
        CaptureHistoryJournal::create(&self.path(), self.key.clone(), self.cfg.clone()).unwrap()
    }
    fn proof(
        &mut self,
        l: &CaptureVisibleHistoryLine,
        seq: u64,
    ) -> (IntakeReceipt, CaptureProvenanceRecord) {
        let e = l.project().unwrap();
        let envelope = CaptureEnvelope {
            schema_version: CAPTURE_VERSION.into(),
            session_id: self.session.session_id.clone(),
            event_id: e.event_id.clone(),
            sequence: seq,
            kind: e.kind,
            content: e.content.replace("SECRET", "[REDACTED]"),
            occurred_at: e.occurred_at,
            loss: CaptureLoss {
                redacted: true,
                truncated: false,
                partial_history: true,
            },
        };
        let ack = self
            .store
            .capture_event(
                &self.scope,
                &self.access,
                &serde_json::to_vec(&envelope).unwrap(),
            )
            .unwrap();
        let p = self
            .store
            .record_capture_provenance(
                &self.scope,
                &self.access,
                CaptureProvenanceRequest {
                    session_id: self.session.session_id.clone(),
                    event_id: e.event_id,
                    kind: e.kind,
                    origin: CaptureDeliveryOrigin::UserProvidedExport,
                },
            )
            .unwrap();
        (ack.receipt, p)
    }
}
fn file() -> CaptureHistoryFile {
    CaptureHistoryFile {
        import_id: "selected-file".into(),
        file_sha256: "a".repeat(64),
        file_bytes: 512,
        line_count: 2,
    }
}

#[test]
fn normalized_identity_matches_live_adapter_and_rejects_hidden_or_forged_metadata() {
    let l = line("turn-1");
    let p = l.project().unwrap();
    let live=decode_visible_capture_hook(CaptureHookHost::CodexCli,"source-session",1,&serde_json::to_vec(&json!({"session_id":"source-session","turn_id":"turn-1","hook_event_name":"UserPromptSubmit","prompt":l.content})).unwrap()).unwrap();
    assert_eq!(p.event_id, live.event_id);
    assert_eq!(p.content, live.content);
    assert_eq!(
        parse_visible_history_jsonl(&jsonl(&[l.clone(), l.clone()]), "codex")
            .unwrap()
            .len(),
        2
    );
    let mut changed = l.clone();
    changed.content.push('!');
    assert!(parse_visible_history_jsonl(&jsonl(&[l.clone(), changed]), "codex").is_err());
    for field in [
        "thinking",
        "system",
        "transcript_path",
        "source_id",
        "sequence",
    ] {
        let mut v = serde_json::to_value(&l).unwrap();
        v[field] = json!("untrusted");
        assert!(parse_visible_history_jsonl(&serde_json::to_vec(&v).unwrap(), "codex").is_err());
    }
    let mut v = serde_json::to_value(&l).unwrap();
    v["loss"]["partial_history"] = json!(false);
    assert!(parse_visible_history_jsonl(&serde_json::to_vec(&v).unwrap(), "codex").is_err());
    assert!(parse_visible_history_jsonl(&jsonl(&[l]), "claude").is_err());
}

#[test]
fn strict_whole_file_limits_and_claude_visible_stop_only() {
    let l = line("turn-1");
    let text = String::from_utf8(jsonl(std::slice::from_ref(&l))).unwrap();
    assert!(parse_visible_history_jsonl(format!("{text}\n\n{text}").as_bytes(), "codex").is_err());
    assert!(
        parse_visible_history_jsonl(
            text.replacen("{", "{\"host\":\"codex\",", 1).as_bytes(),
            "codex"
        )
        .is_err()
    );
    assert!(
        parse_visible_history_jsonl(&vec![b' '; MAX_CAPTURE_HISTORY_BYTES + 1], "codex").is_err()
    );
    assert!(parse_visible_history_jsonl(&jsonl(&vec![l.clone(); 1001]), "codex").is_err());
    let mut claude = l;
    claude.host = "claude".into();
    assert!(claude.project().is_err());
    claude.kind = CaptureEventKind::AssistantResponse;
    assert!(claude.project().is_ok());
    claude.occurred_at = Some("not-a-time".into());
    assert!(claude.project().is_err());
}

#[test]
fn journal_restart_double_receipt_cursor_and_file_or_policy_changes() {
    let mut f = Fixture::new();
    let mut j = f.journal();
    let file = file();
    assert_eq!(j.begin(file.clone()).unwrap().next_line, 0);
    let first = line("turn-1");
    let (r, p) = f.proof(&first, 1);
    j.pending(&file, 0, CaptureHistoryPhase::PendingProvenance)
        .unwrap();
    assert!(j.confirmed_receipts(&file).unwrap().is_empty());
    drop(j);
    let bytes = std::fs::read(f.path()).unwrap();
    for marker in ["VISIBLE", "selected-file", "source-session"] {
        assert!(!bytes.windows(marker.len()).any(|b| b == marker.as_bytes()));
    }
    assert!(
        CaptureHistoryJournal::open(&f.path(), VaultKey::generate().unwrap(), f.cfg.clone())
            .is_err()
    );
    let mut bad = f.cfg.clone();
    bad.policy.version = "other".into();
    assert!(CaptureHistoryJournal::open(&f.path(), f.key.clone(), bad).is_err());
    let mut j = CaptureHistoryJournal::open(&f.path(), f.key.clone(), f.cfg.clone()).unwrap();
    assert_eq!(
        j.begin(file.clone()).unwrap().phase,
        CaptureHistoryPhase::PendingProvenance
    );
    let mut changed = file.clone();
    changed.file_sha256 = "b".repeat(64);
    assert!(j.begin(changed).is_err());
    let progress = j.confirm(&file, 0, &first, r.clone(), p.clone()).unwrap();
    assert_eq!(progress.next_line, 1);
    assert!(j.confirm(&file, 0, &first, r, p).is_err());
    let second = line("turn-2");
    let (r, p) = f.proof(&second, 2);
    assert_eq!(
        j.confirm(&file, 1, &second, r, p).unwrap().phase,
        CaptureHistoryPhase::Complete
    );
    assert_eq!(j.confirmed_receipts(&file).unwrap().len(), 2);
    assert_eq!(j.begin(file).unwrap().phase, CaptureHistoryPhase::Complete);
}

#[test]
fn journal_rejects_wrong_line_instance_source_hash_and_nonexport_proof() {
    let mut f = Fixture::new();
    let mut j = f.journal();
    let file = file();
    j.begin(file.clone()).unwrap();
    let first = line("turn-1");
    let (r, p) = f.proof(&first, 1);
    assert!(
        j.confirm(&file, 0, &line("other-turn"), r.clone(), p.clone())
            .is_err()
    );
    let mut bad = p.clone();
    bad.instance_id = "other".into();
    assert!(j.confirm(&file, 0, &first, r.clone(), bad).is_err());
    let mut bad = p.clone();
    bad.origins = vec![CaptureDeliveryOrigin::HookUnverified];
    assert!(j.confirm(&file, 0, &first, r.clone(), bad).is_err());
    let mut bad = r.clone();
    bad.content_sha256 = "b".repeat(64);
    assert!(j.confirm(&file, 0, &first, bad, p.clone()).is_err());
    let mut bad = p.clone();
    bad.source.object_id = "other".into();
    assert!(j.confirm(&file, 0, &first, r.clone(), bad).is_err());
    assert_eq!(j.begin(file.clone()).unwrap().next_line, 0);
    assert!(j.confirm(&file, 0, &first, r, p).is_ok());
}
