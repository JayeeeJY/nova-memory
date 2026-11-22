# Nova Memory licensing

The Nova Memory local-product code is licensed under the GNU Affero General Public
License, version 3 only. The SPDX identifier is `AGPL-3.0-only`. The complete
license text is in [LICENSE](LICENSE).

## Covered local-product files

This license applies to original Nova Memory code, tests and build material in
the following paths, subject to any retained third-party notices.

- `apps/nova-cli/`
- `apps/nova-mcp/`
- `apps/nova-review/`
- `apps/nova-desktop/Package.swift`, `Sources/` and `Tests/`
- `crates/nova-core/`
- `crates/nova-model-runtime/`
- `script/build_and_run.sh`
- `scripts/package-local.py`, `package-desktop.py`, `sign-desktop-local.py`
- `scripts/model-acceptance.py`, `n2a-real-model-acceptance.py`
- `scripts/tests/test_local_package.py`, `test_desktop_package.py`,
  `test_desktop_local_signing.py`
- `eval/model-acceptance/cases.json`, `eval/n5/fixture-submission.json`,
  `eval/n5/negative-submission.json`
- The local-product source preview README, Cargo manifests, lockfile and build
  configuration supplied with that source preview.
- `CONTRIBUTING.md` and `.github/ISSUE_TEMPLATE/bug_report.yml`.
- `VERSION`, `CHANGELOG.md`, `docs/VERSIONING.md`, `scripts/version.py`,
  `scripts/tests/test_product_version.py` and `.github/workflows/version.yml`.

Existing author and copyright notices remain in effect. Original project
contributions belong to their respective copyright holders. This declaration
does not assign those copyrights.

The local-product source preview packages these paths as a standalone workspace.
The internal development checkout also contains hosted-service implementations,
historical plans, private evidence and other tools. Their location in this
checkout does not extend this local-product licensing declaration to them.

## Third-party software

Dependencies retain their own licenses and copyright notices. The release
companion `Third-Party Notices/` directory records the pinned dependency versions
and their upstream license and notice files. Dependency source locations and
versions are also specified by `Cargo.lock`.

## Name and logo

Nova Memory and Atom Awareness branding identifies the project and its official
products. This code license grants no trademark rights or permission to imply
official endorsement. The founder-supplied logo at
`docs/assets/nova-memory-logo-primary.png` is a separately maintained brand asset.
Its provenance is recorded in [docs/BRAND_ASSETS.md](docs/BRAND_ASSETS.md).
The AGPL grant above applies to the listed software and build material.

## Source access

Every distributed local-product binary must be accompanied by clear directions
to its corresponding source and the license. The internal preview distribution
includes `Nova-Memory-local-source-AGPL.tar.gz` beside the App and the source
build instructions.

When operating a modified version over a network, provide the corresponding
source to its remote users as required by section 13. A public release must
identify a working download location for the matching version. Keep that source
available with the binary downloads.

The covered software is provided without warranty, to the extent permitted by
applicable law. Users may redistribute and modify it under the included AGPL v3.
