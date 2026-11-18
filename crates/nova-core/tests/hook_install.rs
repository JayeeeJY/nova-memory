use nova_core::intake::*;
use serde_json::{Value, json};
use std::path::PathBuf;

fn spec(host: CaptureHookHost) -> CaptureHookInstallSpec {
    CaptureHookInstallSpec {
        installation_id: "fixture-nova-v1".into(),
        host,
        executable: PathBuf::from("/opt/Nova Memory/nova-capture-hook"),
        binding_file: PathBuf::from("/opt/Nova Memory/capture-binding.json"),
        target: CaptureRelayTarget {
            instance_id: "instance-fixture".into(),
            target_id: "fixed-loopback".into(),
            grant_id: "grant-fixture".into(),
            source_namespace: "codex:fixture".into(),
            session_id: "capture-session-fixture".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        },
        observed_host_version: None,
    }
}
fn fixture() -> (tempfile::TempDir, CaptureHookFixture) {
    let d = tempfile::tempdir().unwrap();
    let f = CaptureHookFixture::create(&d.path().join("owned-fixture")).unwrap();
    (d, f)
}
fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}
fn original() -> Vec<u8> {
    br#"{ "theme":"dark", "custom":{"anything":true}, "hooks":{"Stop":[{"hooks":[{"type":"command","command":"/other/owner-hook"}]}]}}"#.to_vec()
}

#[test]
fn dry_plan_is_reviewable_and_never_claims_live_host_verified() {
    for host in [CaptureHookHost::CodexCli, CaptureHookHost::ClaudeCodeCli] {
        let s = spec(host);
        let before = original();
        let p = prepare_capture_hook_install(s, Some(&before), None).unwrap();
        assert_eq!(p.manifest().phase, CaptureHookInstallPhase::Prepared);
        assert!(!p.manifest().real_host_verified);
        assert!(
            p.required_live_checks()
                .contains(&"real_event_trigger_and_source_ack_observed")
        );
        let v = parse(p.rendered());
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["custom"]["anything"], true);
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/other/owner-hook"
        );
        if host == CaptureHookHost::ClaudeCodeCli {
            assert!(v["hooks"].get("UserPromptSubmit").is_none());
            assert!(v["hooks"]["Stop"][1]["hooks"][0]["args"].is_array());
        } else {
            assert!(v["hooks"]["UserPromptSubmit"].is_array());
        }
    }
}

#[test]
fn fixture_install_exact_backup_repeat_and_owned_only_uninstall() {
    let (d, f) = fixture();
    let s = spec(CaptureHookHost::CodexCli);
    let before = original();
    f.seed(s.host, &before).unwrap();
    let p = prepare_capture_hook_install(s.clone(), Some(&before), None).unwrap();
    let m = f.apply(&p).unwrap();
    assert_eq!(m.phase, CaptureHookInstallPhase::FixtureApplied);
    assert_eq!(
        std::fs::read(d.path().join("owned-fixture/backups").join(&m.backup_file)).unwrap(),
        before
    );
    let current = f.config(s.host).unwrap().unwrap();
    let manifest = f.manifest(&s.installation_id).unwrap().unwrap();
    let repeat = prepare_capture_hook_install(s.clone(), Some(&current), Some(&manifest)).unwrap();
    assert_eq!(repeat.rendered(), current);
    f.apply(&repeat).unwrap();
    let mut edit = parse(&current);
    edit["user_added_after_install"] = json!(123);
    edit["hooks"]["Stop"]
        .as_array_mut()
        .unwrap()
        .push(json!({"hooks":[{"type":"command","command":"/other/later-hook"}]}));
    std::fs::write(f.config_path(s.host), serde_json::to_vec(&edit).unwrap()).unwrap();
    let current = f.config(s.host).unwrap().unwrap();
    let manifest = f.manifest(&s.installation_id).unwrap().unwrap();
    let remove =
        prepare_capture_hook_uninstall(s.clone(), Some(&current), Some(&manifest)).unwrap();
    f.apply(&remove).unwrap();
    let clean = f.config(s.host).unwrap().unwrap();
    let v = parse(&clean);
    assert_eq!(v["user_added_after_install"], 123);
    assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
    assert!(
        v["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let manifest = f.manifest(&s.installation_id).unwrap().unwrap();
    let again = prepare_capture_hook_uninstall(s.clone(), Some(&clean), Some(&manifest)).unwrap();
    assert_eq!(again.rendered(), clean);
    f.apply(&again).unwrap();
}

#[test]
fn concurrent_edit_or_manifest_change_fails_compare_and_swap_without_overwrite() {
    let (_d, f) = fixture();
    let s = spec(CaptureHookHost::ClaudeCodeCli);
    let before = original();
    f.seed(s.host, &before).unwrap();
    let p = prepare_capture_hook_install(s.clone(), Some(&before), None).unwrap();
    std::fs::write(f.config_path(s.host), b"{\"new-owner-value\":true}").unwrap();
    assert!(f.apply(&p).is_err());
    assert_eq!(
        parse(&f.config(s.host).unwrap().unwrap())["new-owner-value"],
        true
    );
    assert!(f.manifest(&s.installation_id).unwrap().is_none());
    let before = f.config(s.host).unwrap().unwrap();
    let first = prepare_capture_hook_install(s.clone(), Some(&before), None).unwrap();
    let stale = prepare_capture_hook_install(s.clone(), Some(&before), None).unwrap();
    f.apply(&first).unwrap();
    assert!(f.apply(&stale).is_err());
}

#[test]
fn edited_owned_entry_unowned_match_and_duplicate_refuse_ownership_takeover() {
    let s = spec(CaptureHookHost::CodexCli);
    let p = prepare_capture_hook_install(s.clone(), None, None).unwrap();
    assert!(prepare_capture_hook_install(s.clone(), Some(p.rendered()), None).is_err());
    let (_d, f) = fixture();
    f.apply(&p).unwrap();
    let manifest = f.manifest(&s.installation_id).unwrap().unwrap();
    let mut v = parse(p.rendered());
    v["hooks"]["Stop"][0]["hooks"][0]["timeout"] = json!(55);
    assert!(
        prepare_capture_hook_uninstall(
            s.clone(),
            Some(&serde_json::to_vec(&v).unwrap()),
            Some(&manifest)
        )
        .is_err()
    );
    let mut v = parse(p.rendered());
    let duplicate = v["hooks"]["Stop"][0].clone();
    v["hooks"]["Stop"].as_array_mut().unwrap().push(duplicate);
    assert!(
        prepare_capture_hook_uninstall(
            s.clone(),
            Some(&serde_json::to_vec(&v).unwrap()),
            Some(&manifest)
        )
        .is_err()
    );
    let mut v = parse(p.rendered());
    v["hooks"]["Stop"][0]["matcher"] = json!("");
    assert!(prepare_capture_hook_install(s, Some(&serde_json::to_vec(&v).unwrap()), None).is_err());
}

#[test]
fn existing_directories_symlinks_and_hardlinks_cannot_be_fixture_targets() {
    let (d, f) = fixture();
    assert!(CaptureHookFixture::create(d.path()).is_err());
    let external = d.path().join("other-file");
    std::fs::write(&external, b"{}").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&external, f.config_path(CaptureHookHost::CodexCli)).unwrap();
        let p = prepare_capture_hook_install(spec(CaptureHookHost::CodexCli), None, None).unwrap();
        assert!(f.apply(&p).is_err());
        assert_eq!(std::fs::read(&external).unwrap(), b"{}");
        std::fs::remove_file(f.config_path(CaptureHookHost::CodexCli)).unwrap();
        std::fs::hard_link(&external, f.config_path(CaptureHookHost::CodexCli)).unwrap();
        assert!(f.config(CaptureHookHost::CodexCli).is_err());
    }
}

#[test]
fn rendered_command_cannot_execute_shell_fragments_in_fixed_paths() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let weird = d.path().join("quote' $(touch PWNED) `touch PWNED2` space");
        std::fs::create_dir(&weird).unwrap();
        let executable = weird.join("nova-capture-hook");
        std::fs::write(&executable, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = spec(CaptureHookHost::CodexCli);
        s.executable = executable;
        s.binding_file = weird.join("binding ' $.json");
        let p = prepare_capture_hook_install(s.clone(), None, None).unwrap();
        let v = parse(p.rendered());
        let command = v["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(d.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        let lines = String::from_utf8(out.stdout).unwrap();
        assert!(lines.contains(s.binding_file.to_str().unwrap()));
        assert!(!d.path().join("PWNED").exists());
        assert!(!d.path().join("PWNED2").exists());
    }
}

#[test]
fn claude_exec_form_has_no_shell_and_policy_flags_are_preserved() {
    let s = spec(CaptureHookHost::ClaudeCodeCli);
    let p = prepare_capture_hook_install(
        s.clone(),
        Some(br#"{"disableAllHooks":true,"hooks":{}}"#),
        None,
    )
    .unwrap();
    let v = parse(p.rendered());
    assert_eq!(v["disableAllHooks"], true);
    let h = &v["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(h["command"], s.executable.to_str().unwrap());
    assert_eq!(h["args"][0], "enqueue-visible");
    assert!(h.get("onFailure").is_none());
    assert!(h.get("asyncRewake").is_none());
}

#[test]
fn malformed_configuration_duplicate_keys_and_invalid_paths_fail_closed() {
    let s = spec(CaptureHookHost::CodexCli);
    for bytes in [
        b"[]".as_slice(),
        b"{\"hooks\":[]}",
        b"{\"hooks\":{\"Stop\":{}}}",
        b"{\"hooks\":{},\"hooks\":{}}",
    ] {
        assert!(prepare_capture_hook_install(s.clone(), Some(bytes), None).is_err());
    }
    for path in [
        "relative/nova-capture-hook",
        "/opt/../nova-capture-hook",
        "/opt/\n/nova-capture-hook",
        "/opt/${CLAUDE_PLUGIN_ROOT}/nova-capture-hook",
        "/usr/bin/python3",
    ] {
        let mut bad = s.clone();
        bad.executable = path.into();
        assert!(prepare_capture_hook_install(bad, None, None).is_err());
    }
    let mut s = s;
    s.target.instance_id.clear();
    assert!(prepare_capture_hook_install(s, None, None).is_err());
}

#[test]
fn recover_prepared_manifest_only_when_config_rename_already_completed() {
    let (d, f) = fixture();
    let s = spec(CaptureHookHost::CodexCli);
    let p = prepare_capture_hook_install(s.clone(), None, None).unwrap();
    f.apply(&p).unwrap();
    let manifest_path = d
        .path()
        .join("owned-fixture/manifests/fixture-nova-v1.json");
    std::fs::write(&manifest_path, serde_json::to_vec(p.manifest()).unwrap()).unwrap();
    assert_eq!(
        f.recover(&s.installation_id).unwrap().phase,
        CaptureHookInstallPhase::FixtureApplied
    );
    std::fs::write(&manifest_path, serde_json::to_vec(p.manifest()).unwrap()).unwrap();
    std::fs::write(f.config_path(s.host), b"{}").unwrap();
    assert!(f.recover(&s.installation_id).is_err());
    assert_eq!(std::fs::read(f.config_path(s.host)).unwrap(), b"{}");
}

fn hook(host: CaptureHookHost, event: &str, content: &str) -> Value {
    let mut v = json!({"session_id":"host-session","hook_event_name":event,"stop_hook_active":false,"transcript_path":"/DO-NOT-OPEN/secrets.jsonl","cwd":"/DO-NOT-READ","thinking":"NEVER-CAPTURE-THIS","tool_input":{"secret":"NEVER-CAPTURE-THIS"}});
    v[if host == CaptureHookHost::CodexCli {
        "turn_id"
    } else {
        "prompt_id"
    }] = json!("stable-turn-1");
    v[if event == "UserPromptSubmit" {
        "prompt"
    } else {
        "last_assistant_message"
    }] = json!(content);
    v
}
#[test]
fn adapter_projects_only_visible_fields_and_preserves_stable_identity() {
    for host in [CaptureHookHost::CodexCli, CaptureHookHost::ClaudeCodeCli] {
        let v = hook(host, "Stop", "visible synthetic reply");
        let b = serde_json::to_vec(&v).unwrap();
        let first = decode_visible_capture_hook(host, "host-session", 7, &b).unwrap();
        let second = decode_visible_capture_hook(host, "host-session", 7, &b).unwrap();
        assert_eq!(first.content, "visible synthetic reply");
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.sequence, 7);
        assert!(first.loss.partial_history);
        let bytes = serde_json::to_string(&first).unwrap();
        assert!(!bytes.contains("NEVER-CAPTURE"));
        assert!(!bytes.contains("DO-NOT-OPEN"));
        let mut changed = v;
        changed["last_assistant_message"] = json!("changed visible reply");
        assert_eq!(
            decode_visible_capture_hook(
                host,
                "host-session",
                7,
                &serde_json::to_vec(&changed).unwrap()
            )
            .unwrap()
            .event_id,
            first.event_id
        );
    }
}
#[test]
fn adapter_missing_identity_wrong_session_subagents_continuations_and_hidden_events_refused() {
    for host in [CaptureHookHost::CodexCli, CaptureHookHost::ClaudeCodeCli] {
        let v = hook(host, "Stop", "visible");
        for field in [
            "session_id",
            "turn_id",
            "prompt_id",
            "agent_id",
            "stop_hook_active",
            "last_assistant_message",
        ] {
            if (host == CaptureHookHost::CodexCli && field == "prompt_id")
                || (host == CaptureHookHost::ClaudeCodeCli && field == "turn_id")
            {
                continue;
            }
            let mut bad = v.clone();
            match field {
                "agent_id" => bad[field] = json!("subagent-1"),
                "stop_hook_active" => bad[field] = json!(true),
                _ => {
                    bad.as_object_mut().unwrap().remove(field);
                }
            }
            assert!(
                decode_visible_capture_hook(
                    host,
                    "host-session",
                    1,
                    &serde_json::to_vec(&bad).unwrap()
                )
                .is_err(),
                "{host:?} {field}"
            );
        }
        for event in [
            "PreToolUse",
            "PostToolUse",
            "InstructionsLoaded",
            "MessageDisplay",
            "SubagentStop",
            "StopFailure",
        ] {
            let bad = hook(host, event, "not enabled");
            assert!(
                decode_visible_capture_hook(
                    host,
                    "host-session",
                    1,
                    &serde_json::to_vec(&bad).unwrap()
                )
                .is_err()
            );
        }
    }
    let v = hook(
        CaptureHookHost::ClaudeCodeCli,
        "UserPromptSubmit",
        "could be an automatic message",
    );
    assert!(
        decode_visible_capture_hook(
            CaptureHookHost::ClaudeCodeCli,
            "host-session",
            1,
            &serde_json::to_vec(&v).unwrap()
        )
        .is_err()
    );
    let v = hook(
        CaptureHookHost::CodexCli,
        "UserPromptSubmit",
        "synthetic prompt",
    );
    assert_eq!(
        decode_visible_capture_hook(
            CaptureHookHost::CodexCli,
            "host-session",
            1,
            &serde_json::to_vec(&v).unwrap()
        )
        .unwrap()
        .kind,
        CaptureEventKind::UserPrompt
    );
}
