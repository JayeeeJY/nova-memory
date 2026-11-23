# Use the Nova Memory local preview

English · [简体中文](GETTING_STARTED.md) · [繁體中文](GETTING_STARTED.zh-Hant.md) · [日本語](GETTING_STARTED.ja.md)

[Product introduction](../README.md)

What GitHub publishes now is the **v0.1.0** local source preview. Build it yourself on an Apple Silicon Mac. Developer ID signing, notarization and cross-machine installer checks are still being prepared. A version number, changelog entry or GitHub Release names this source preview. It does not mean a signed installer is available, and it does not mean real-client or device acceptance is complete. The current desktop controls are in Chinese. The steps below are instructions to follow, not a record of a completed Mac run.

## Build and open

Prepare macOS 14 or later, Xcode with the macOS SDK, Rust 1.97.1, and Python 3.11 or later.

```sh
git clone https://github.com/JayeeeJY/nova-memory.git
cd nova-memory
cargo fetch --locked
./script/build_and_run.sh
```

The entry builds a runtime bundle and the desktop app in a new `.artifacts/` directory, then opens the candidate. Dependency versions come from `Cargo.lock`. The built app carries the local runtime.

For a separate test library, pass a new absolute path and a free port.

```sh
./script/build_and_run.sh \
  --profile-dir "/absolute/path/to/new-nova-profile" \
  --port 3060
```

The build downloads dependencies and uses Xcode command-line tools. If the system cannot find a compiler, confirm in Xcode settings that the command-line tools are installed and selected.

## First save

1. Open the app and choose 「开始本地使用」.
2. In the workbench, choose 「留住一段记忆」, paste a short piece of material worth keeping, and save it.
3. Open 「AI 整理」 to see the original text, the capture record and the processing state.
4. When a model is ready, choose the model, service address and processing mode in the runtime settings, then turn organization on.

If no model is configured, the original stays and waits. Check the model connection and extraction quality separately. Results can still be corrected.

## Use it from another AI

「连接」 in the workbench gives the steps for the platform you select.

- For ongoing use, choose a capture or read scope for the target AI and install the corresponding MCP configuration.
- For a temporary share, select the records in 「记忆库」, preview the one-time context, then copy or download it into the other tool.
- Text already sent is kept by the recipient. Later revisions and revocation apply to later Nova access.

Remote connectors such as ChatGPT and Grok need a reachable address, authentication and the matching account setup. Follow the platform notes the workbench shows.

## Originals, versions and corrections

In 「记忆库」, open a project or topic and expand sources and conditions. In 「AI 整理」, tell Nova what is inaccurate or which situation a statement applies to. The correction keeps its source and history.

What the model sees, and how far processing goes, follows the current settings. When you connect a remote service, the relevant content is sent to that configured service. Local encrypted storage does not prevent that send.

## Data and backups

The default data directory is `~/Library/Application Support/Nova Memory/local-workspace-v03`. After the service stops, materials and configuration remain there. Closing the app window may leave the service running. Use 「停止」 in the app when you want it to stop.

Backup controls are in the workbench settings and data area. Inspecting an encrypted backup needs the backup file and its Vault key. From 「运行设置 → 加密备份」, choose the file, check it, and open a separate copy.

An inspected copy keeps capture, automatic organization and AI connections paused. Before turning them back on, check deletions and revocations made after the backup.

## Developer checks

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*package.py'
python3 -m unittest discover -s scripts/tests -p 'test_desktop_local_signing.py'
```

`--help` shows each packaging script's arguments.

```sh
python3 scripts/package-local.py --help
python3 scripts/package-desktop.py --help
python3 scripts/sign-desktop-local.py --help
```

Read [CONTRIBUTING.md](../CONTRIBUTING.md) before contributing. In a report, include the version, system, target AI client, the smallest synthetic reproduction and the expected result.
