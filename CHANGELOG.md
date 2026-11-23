# Changelog

Product releases use the version in `VERSION`. Documentation revisions and protocol/schema versions have separate lifecycles. Dates use Asia/Shanghai.

## [Unreleased]

Changes after the first public-source baseline belong here until assigned to a release. `v0.2.0` is the next feature-iteration target, not a completed or promised delivery.

## [0.1.0] - 2026-11-22

### First public-source baseline

- Published the local Nova Memory source under AGPL-3.0-only, with build and contribution instructions.
- Established the local workbench for capture, original/source preservation, memory management, correction, Wiki/graph views and scoped context reuse.
- Included platform-specific MCP connection guidance and supported capture paths; actual compatibility remains client-specific as described in the README.
- Published the bilingual product story, English website visuals, horizontal capture-to-Vault animation and English memory sphere.
- Added one product version source, packaging-version integration, a version-check/update command and a version-metadata workflow.

### Delivery and compatibility

- This is a **local source preview**. This release does not include a newly signed or notarized Mac installer or declare new Host/model/device acceptance.
- Storage schemas, protocol versions and existing user data are unchanged by the version-management work. Core Cargo packages remain at `0.1.0`.
- New desktop packages take their product version from `VERSION`; the earlier hard-coded `0.3.0` was an internal engineering label, not a previous public product release. Existing app files and private profiles are not rewritten.
- The product story/media baseline before version metadata is commit `dda6b50c47ba88e76cf96cddf63409586fc91690`. The `v0.1.0` tag identifies the full baseline including version management.

### Known limits

- Developer ID signing, notarization and cross-machine installer validation remain separate delivery work.
- ChatGPT and Grok remote access depends on reachable endpoints and account/authentication compatibility; this release does not promote pending checks to complete.
- Extraction quality depends on the configured model. Even Hub and other wearable integrations remain future development.
