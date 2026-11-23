# Contributing to Nova Memory

Nova Memory's current delivery focuses on a complete local experience. Useful
contributions improve capture, source preservation, memory editing and retrieval,
or continuity between AI tools.

## Before changing code

Describe the user-visible problem, the smallest reproducible example and the
expected behavior in an issue. Use synthetic material to reproduce memory-related
problems. Preserve existing originals, provenance, corrections and connection
permissions when changing storage or retrieval behavior.

For UI changes, follow the existing workbench's visual design and test the flow
on desktop and a narrow viewport. For platform integration changes, identify the
client, version and connection mode used.

## Local development

The source preview README describes prerequisites and the normal build entry
point. Language editions and build guides start from [README.md](README.md). Rust packages live under `apps/` and `crates/`; the native macOS shell is
in `apps/nova-desktop/`.

Use a new data directory for development. Run tests relevant to the affected
code. In a pull request, explain the user-visible behavior, verification and any
remaining limits. State whether checks used a synthetic client, an actual AI
client or an actual model.

## Reporting a problem

Include the Nova version or download filename, operating system, AI platform
when relevant, reproduction steps and expected/observed behavior.

Remove private memory, access tokens, keys and personal data from examples.
Share a minimal synthetic example and an error message with sensitive values
removed. Public issues are visible to other people.

## Contributions and licensing

Original contributions to the local-product paths listed in
[LICENSE_SCOPE.md](LICENSE_SCOPE.md) are submitted under `AGPL-3.0-only`.
Keep existing author and third-party notices. When incorporating external code,
identify its original source, version and license in the pull request.

Contributors retain their copyrights. Submitting a patch does not assign
copyright or separately grant a proprietary relicensing right.

## Versioning

Product versions come from `VERSION`. Read [the version policy](docs/VERSIONING.md), add user-visible changes to `CHANGELOG.md`, and run `python3 scripts/version.py check` before preparing a release. Version metadata checks do not replace feature-specific tests or client acceptance.
