"""No Cargo/network tests for the bounded local engineering packager."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("nova_package_local", Path(__file__).resolve().parents[1] / "package-local.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class LocalPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nova package fixture ")
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo with spaces"
        (self.repo / "target/debug").mkdir(parents=True)
        (self.repo / "target/release").mkdir()
        (self.repo / "scripts").mkdir()
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        for profile in ("debug", "release"):
            for binary in ("nova-review", "nova-local-init", "nova-local", "nova-capture-hook", "nova-memory-connect"):
                path = self.repo / "target" / profile / binary
                path.write_text("#!/bin/sh\nprintf 'fixture-controller %s\\n' \"$1\"\n" if binary == "nova-local" else "#!/bin/sh\nexit 0\n# " + profile + " " + binary + "\n")
                path.chmod(0o700)
        self.output = self.root / "bundle with spaces"

    def tearDown(self):
        self.temporary.cleanup()

    def build(self, **options):
        return PACKAGE.build_bundle(self.repo, self.output, no_build=True, **options)

    def test_fixed_payload_excludes_source_data_credentials_and_cache(self):
        for name in ("vault.key", "owner.key", "intake.sqlite", "private-source.txt"):
            (self.repo / name).write_text("EXCLUDED_PRIVATE_SENTINEL")
        (self.repo / "target/debug/not-a-payload").write_text("EXCLUDED_PRIVATE_SENTINEL")
        directory, manifest = self.build(profile="debug")
        self.assertEqual(set(manifest), PACKAGE.MANIFEST_KEYS)
        self.assertEqual(set(manifest["files"]), PACKAGE.PAYLOAD_FILES)
        self.assertEqual(manifest["build_profile"], "debug")
        self.assertIn("bin/nova-local", manifest["files"])
        self.assertIn("bin/nova-memory-connect", manifest["files"])
        self.assertNotIn("runtime/nova-local.py", manifest["files"])
        self.assertNotIn(b"python3", PACKAGE.command_source("launch"))
        self.assertEqual(PACKAGE.verify_bundle(directory), manifest)
        all_bytes = b"".join(p.read_bytes() for p in directory.rglob("*") if p.is_file())
        self.assertNotIn(b"EXCLUDED_PRIVATE_SENTINEL", all_bytes)
        self.assertEqual(manifest["bundle_id"], hashlib.sha256(json.dumps(manifest["files"], sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest())

    def test_modes_are_private_and_only_selected_launchers_executable(self):
        directory, _ = self.build()
        for path in [directory] + list(directory.rglob("*")):
            relative = path.relative_to(directory).as_posix()
            expected = 0o700 if path.is_dir() or relative.startswith("bin/") or relative in PACKAGE.COMMANDS else 0o600
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), expected, relative)

    def test_existing_directory_file_and_symlink_are_never_replaced(self):
        for kind in ("directory", "file", "symlink"):
            with self.subTest(kind=kind):
                target = self.root / ("existing-" + kind)
                if kind == "directory":
                    target.mkdir()
                    (target / "keep").write_text("unchanged")
                elif kind == "file":
                    target.write_text("unchanged")
                else:
                    target.symlink_to(self.root / "missing-target")
                with self.assertRaises(PACKAGE.BundleError):
                    PACKAGE.build_bundle(self.repo, target, no_build=True)
                self.assertTrue(target.exists() or target.is_symlink())

    def test_relative_output_and_source_symlink_rejected_before_output_creation(self):
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.build_bundle(self.repo, Path("relative-new"), no_build=True)
        binary = self.repo / "target/release/nova-review"
        binary.unlink()
        binary.symlink_to(self.repo / "target/debug/nova-review")
        with self.assertRaises(PACKAGE.BundleError):
            self.build()
        self.assertFalse(self.output.exists())

    def test_source_directory_alias_rejected(self):
        alias = self.root / "repo-alias"
        alias.symlink_to(self.repo, target_is_directory=True)
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.build_bundle(self.repo, alias, no_build=True)
        self.assertTrue(alias.is_symlink())

    def test_manifest_and_payload_tampering_rejected(self):
        directory, original = self.build()
        manifest_path = directory / "manifest.json"
        changed = dict(original)
        changed["bundle_id"] = "0" * 64
        manifest_path.write_text(json.dumps(changed))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        manifest_path.write_text(json.dumps(original))
        (directory / "bin/nova-review").write_text("tampered")
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)

    def test_extra_payload_unknown_manifest_and_duplicate_keys_rejected(self):
        directory, original = self.build()
        extra = directory / "unexpected.key"
        extra.write_text("unexpected")
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        extra.unlink()
        manifest_path = directory / "manifest.json"
        changed = dict(original, trusted=True)
        manifest_path.write_text(json.dumps(changed))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        manifest_path.write_text('{"schema_version":"wrong",' + json.dumps(original)[1:])
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)

    def test_commands_are_valid_quoted_and_noninteractive_execution_returns(self):
        directory, _ = self.build()
        for name, action in PACKAGE.COMMANDS.items():
            with self.subTest(action=action):
                command = directory / name
                subprocess.run(["bash", "-n", str(command)], check=True, capture_output=True)
                environment = os.environ.copy()
                environment["NOVA_MEMORY_DATA_DIR"] = str(self.root / "isolated data directory")
                result = subprocess.run(["bash", str(command)], stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=5, env=environment, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("fixture-controller " + action, result.stdout)
                self.assertFalse(Path(environment["NOVA_MEMORY_DATA_DIR"]).exists())

    def test_default_build_is_locked_offline_and_selects_five_bins(self):
        with mock.patch.object(PACKAGE.subprocess, "run") as run:
            PACKAGE.build_bundle(self.repo, self.output, profile="release")
        args, kwargs = run.call_args
        self.assertEqual(args[0], ["cargo", "build", "--manifest-path", str(self.repo.resolve() / "Cargo.toml"), "--locked", "--offline", "-p", "nova-review", "--bin", "nova-review", "--bin", "nova-local-init", "--bin", "nova-local", "--bin", "nova-capture-hook", "--bin", "nova-memory-connect", "--release"])
        self.assertEqual(kwargs["env"]["CARGO_INCREMENTAL"], "0")
        self.assertEqual(kwargs["env"]["CARGO_TARGET_DIR"], str(self.repo.resolve() / "target"))
        self.assertTrue(kwargs["check"])


    def test_bridge_is_required_for_current_bundle_without_fallback(self):
        (self.repo / "target/release/nova-memory-connect").unlink()
        with self.assertRaises((OSError, PACKAGE.BundleError)):
            self.build()
        self.assertFalse(self.output.exists())

    def test_legacy_version_has_its_own_exact_payload_and_archive(self):
        import tarfile
        directory, current = self.build()
        declared_legacy = dict(current, schema_version=PACKAGE.LEGACY_SCHEMA_VERSION)
        (directory / "manifest.json").write_text(json.dumps(declared_legacy))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        (directory / "bin/nova-memory-connect").unlink()
        declared_legacy["files"] = {k: v for k, v in current["files"].items() if k != "bin/nova-memory-connect"}
        declared_legacy["bundle_id"] = PACKAGE.digest(PACKAGE.canonical_files(declared_legacy["files"]))
        (directory / "manifest.json").write_text(json.dumps(declared_legacy))
        self.assertEqual(PACKAGE.verify_bundle(directory), declared_legacy)
        archive = self.root.resolve() / "legacy.tar"
        PACKAGE.archive_bundle(directory, archive)
        with tarfile.open(archive) as tar:
            self.assertEqual(set(tar.getnames()), PACKAGE.LEGACY_PAYLOAD_FILES | {"manifest.json"})
        falsely_current = dict(declared_legacy, schema_version=PACKAGE.SCHEMA_VERSION)
        (directory / "manifest.json").write_text(json.dumps(falsely_current))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)


if __name__ == "__main__":
    unittest.main()
