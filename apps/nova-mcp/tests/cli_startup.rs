use nova_core::{Vault, VaultKey};
#[test]
fn binary_configuration_requires_auth_and_explicit_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let key = dir.path().join("test.key");
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&key).unwrap();
    drop(Vault::create(&db, k).unwrap());
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nova-mcp"))
            .args([
                "--vault",
                db.to_str().unwrap(),
                "--key-file",
                key.to_str().unwrap(),
            ])
            .args(args)
            .output()
            .unwrap()
    };
    let help = run(&["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("chatgpt"));
    assert!(help.contains("codex"));
    let missing = run(&["--client", "codex", "--bind", "127.0.0.1:0"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("require --token-file"));
    let public = run(&["--client", "codex", "--bind", "0.0.0.0:0"]);
    assert!(!public.status.success());
    assert!(String::from_utf8_lossy(&public.stderr).contains("loopback"));
    let bad = run(&[
        "--client",
        "codex",
        "--bind",
        "127.0.0.1:0",
        "--token-file",
        key.to_str().unwrap(),
    ]);
    assert!(!bad.status.success());
    let legacy = run(&[
        "--bind",
        "127.0.0.1:0",
        "--token-file",
        key.to_str().unwrap(),
    ]);
    assert!(!legacy.status.success());
    assert!(!legacy.status.success());
    let legacy = run(&["--client", "chatgpt", "--bind", "127.0.0.1:0"]);
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("requires --enable-legacy"));
    assert_eq!(
        Vault::open(&db, VaultKey::read_file(&key).unwrap())
            .unwrap()
            .verify_audit_chain()
            .unwrap()
            .event_count,
        0
    );
}
