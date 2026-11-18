"""Bounded .app packaging checks; no compiler, network, real Vault, or GUI launch."""

import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import stat
import subprocess
import tempfile
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("nova_desktop_package", SCRIPTS / "package-desktop.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class DesktopPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nova desktop fixture ")
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo with spaces"
        (self.repo / "target/release").mkdir(parents=True)
        (self.repo / "scripts").mkdir()
        (self.repo / "docs/assets").mkdir(parents=True)
        (self.repo / "docs/assets/nova-memory-logo-primary.png").write_bytes(b"fixture-logo")
        (self.repo / "scripts/nova-local.py").write_text("print('synthetic-controller')\n")
        for binary in ("nova-review", "nova-local-init", "nova-local", "nova-capture-hook", "nova-memory-connect"):
            path = self.repo / "target/release" / binary
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o700)
        self.native = PACKAGE.built_binary(self.repo, "arm64")
        self.native.parent.mkdir(parents=True)
        self.native.write_bytes(b"synthetic-native-executable")
        self.native.chmod(0o700)
        self.runtime = self.root / "Local Runtime with spaces"
        self.host = mock.patch.object(PACKAGE.LOCAL, "host_target", return_value=("darwin", "arm64"))
        self.host.start()
        PACKAGE.LOCAL.build_bundle(self.repo, self.runtime, no_build=True)
        self.output = self.root / "Nova Memory engineering.app"

    def tearDown(self):
        self.host.stop()
        self.temporary.cleanup()

    def build(self, **options):
        return PACKAGE.build_bundle(self.repo, self.output, self.runtime, no_build=True, **options)

    def test_whitelist_embeds_verified_runtime_logo_and_native_only(self):
        for name in ("vault.key", "owner.key", "intake.sqlite", "private-source.txt"):
            (self.repo / name).write_text("EXCLUDED_PRIVATE_SENTINEL")
        (self.repo / ".artifacts/desktop-build/compiler-cache").write_text("EXCLUDED_PRIVATE_SENTINEL")
        directory, manifest = self.build()
        self.assertEqual(len(manifest["files"]), 14)
        self.assertEqual(set(manifest["files"]), PACKAGE.PAYLOAD_FILES)
        self.assertEqual(set(manifest), PACKAGE.MANIFEST_KEYS)
        self.assertEqual(PACKAGE.verify_bundle(directory), manifest)
        self.assertEqual(manifest["runtime_bundle_id"], PACKAGE.LOCAL.verify_bundle(self.runtime)["bundle_id"])
        payload = b"".join(path.read_bytes() for path in directory.rglob("*") if path.is_file())
        self.assertNotIn(b"EXCLUDED_PRIVATE_SENTINEL", payload)
        self.assertIn(b"fixture-logo", payload)
        self.assertEqual(manifest["signature"], "unsigned_not_notarized")

    def test_mixed_debug_release_payload_is_rejected(self):
        with self.assertRaises(PACKAGE.BundleError):
            self.build(profile="debug")
        self.assertFalse(self.output.exists())

    def test_info_plist_is_gui_app_with_fixed_identifier_and_minimum_version(self):
        directory, _ = self.build()
        info = plistlib.loads((directory / "Contents/Info.plist").read_bytes())
        self.assertEqual(info["CFBundlePackageType"], "APPL")
        self.assertEqual(info["CFBundleExecutable"], "NovaMemoryDesktop")
        self.assertEqual(info["CFBundleIdentifier"], "chat.novamemory.desktop.engineering")
        self.assertEqual(info["LSMinimumSystemVersion"], "14.0")
        self.assertEqual(info["NSPrincipalClass"], "NSApplication")
        self.assertEqual(info["NSAppTransportSecurity"], {"NSAllowsLocalNetworking": True})

    def test_all_directories_and_executables_private_and_metadata_not_executable(self):
        directory, _ = self.build()
        self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
        for path in directory.rglob("*"):
            relative = path.relative_to(directory).as_posix()
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), PACKAGE.expected_mode(relative), relative)
        self.assertEqual(stat.S_IMODE((directory / PACKAGE.MANIFEST_PATH).stat().st_mode), 0o600)

    def test_bundle_moves_without_referencing_repository_or_original_runtime(self):
        directory, original = self.build()
        moved = self.root / "a moved application.app"
        directory.rename(moved)
        shutil.rmtree(self.repo)
        shutil.rmtree(self.runtime)
        self.assertEqual(PACKAGE.verify_bundle(moved), original)
        manifest_text = (moved / PACKAGE.MANIFEST_PATH).read_text()
        self.assertNotIn(str(self.repo), manifest_text)
        self.assertNotIn(str(self.runtime), manifest_text)

    def test_existing_output_directory_file_and_dangling_symlink_never_replaced(self):
        for kind in ("directory", "file", "symlink"):
            with self.subTest(kind=kind):
                output = self.root / (kind + ".app")
                if kind == "directory":
                    output.mkdir()
                    (output / "keep").write_text("unchanged")
                elif kind == "file":
                    output.write_text("unchanged")
                else:
                    output.symlink_to(self.root / "missing")
                with self.assertRaises(PACKAGE.BundleError):
                    PACKAGE.build_bundle(self.repo, output, self.runtime, no_build=True)
                self.assertTrue(output.exists() or output.is_symlink())

    def test_output_must_be_absolute_app_and_outside_runtime_sources_and_cache(self):
        for output in (Path("new.app"), self.root / "new-folder", self.repo / "scripts/Nova.app",
                       self.repo / "apps/Nova.app", self.repo / ".artifacts/desktop-build/Nova.app",
                       self.runtime / "Nova.app"):
            with self.subTest(output=output), self.assertRaises((PACKAGE.BundleError, FileNotFoundError)):
                PACKAGE.build_bundle(self.repo, output, self.runtime, no_build=True)
        self.assertFalse(self.output.exists())

    def test_unverified_runtime_tamper_and_extra_keys_fail_before_build(self):
        (self.runtime / "owner.key").write_text("UNEXPECTED_KEY")
        with mock.patch.object(PACKAGE.subprocess, "run") as run:
            with self.assertRaises(PACKAGE.BundleError):
                PACKAGE.build_bundle(self.repo, self.output, self.runtime)
        run.assert_not_called()
        self.assertFalse(self.output.exists())
        (self.runtime / "owner.key").unlink()
        (self.runtime / "bin/nova-review").write_text("tampered")
        with self.assertRaises(PACKAGE.BundleError):
            self.build()

    def test_source_binary_runtime_and_logo_symlinks_rejected(self):
        for kind in ("binary", "runtime", "logo"):
            with self.subTest(kind=kind):
                selected = {"binary": self.native, "runtime": self.runtime,
                            "logo": self.repo / "docs/assets/nova-memory-logo-primary.png"}[kind]
                moved = selected.with_name(selected.name + ".original")
                selected.rename(moved)
                selected.symlink_to(moved, target_is_directory=kind == "runtime")
                try:
                    with self.assertRaises(PACKAGE.BundleError):
                        self.build()
                    self.assertFalse(self.output.exists())
                finally:
                    selected.unlink()
                    moved.rename(selected)

    def test_native_source_must_be_executable_regular_repository_build_output(self):
        self.native.chmod(0o600)
        with self.assertRaises(PACKAGE.BundleError):
            self.build()
        self.native.chmod(0o700)
        external = self.root / "foreign-binary"
        external.write_bytes(b"foreign")
        external.chmod(0o700)
        with self.assertRaises(PACKAGE.BundleError):
            self.build(native_binary=external)
        self.assertEqual(PACKAGE.built_binary(self.repo, "arm64"), self.native)
        modern = self.repo / ".artifacts/desktop-build/out/Products/Release/NovaMemoryDesktop"
        modern.parent.mkdir(parents=True)
        modern.write_bytes(b"swift-6.4-new-engine-native")
        modern.chmod(0o700)
        self.assertEqual(PACKAGE.built_binary(self.repo, "arm64"), modern)
        directory, _ = self.build()
        self.assertEqual((directory / PACKAGE.NATIVE_BINARY).read_bytes(), modern.read_bytes())
        modern.unlink()
        modern.symlink_to(self.native)
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.built_binary(self.repo, "arm64")

    def test_manifest_unknown_duplicate_fields_and_payload_tamper_rejected(self):
        directory, original = self.build()
        path = directory / PACKAGE.MANIFEST_PATH
        path.write_text(json.dumps(dict(original, trusted=True)))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        path.write_text('{"schema_version":"wrong",' + json.dumps(original)[1:])
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        path.write_text(json.dumps(original))
        (directory / "Contents/Resources/NovaLogo.png").write_bytes(b"changed")
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)

    def test_nested_extra_file_and_permission_change_rejected(self):
        directory, _ = self.build()
        extra = directory / "Contents/Resources/owner.key"
        extra.write_text("EXCLUDED_PRIVATE_SENTINEL")
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)
        extra.unlink()
        (directory / PACKAGE.NATIVE_BINARY).chmod(0o755)
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)

    def test_build_is_release_locked_resolution_and_all_cache_paths_are_writable_artifacts(self):
        with mock.patch.object(PACKAGE.subprocess, "run") as run:
            PACKAGE.build_bundle(self.repo, self.output, self.runtime)
        args, kwargs = run.call_args
        self.assertEqual(args[0], PACKAGE.build_command(self.repo))
        self.assertIn("--disable-automatic-resolution", args[0])
        self.assertIn("--disable-sandbox", args[0])
        self.assertEqual(args[0][-4:], ["--configuration", "release", "--product", "NovaMemoryDesktop"])
        cache = str(self.repo / ".artifacts/desktop-swift-cache")
        self.assertEqual(kwargs["env"]["CLANG_MODULE_CACHE_PATH"], cache)
        self.assertEqual(kwargs["env"]["SWIFTPM_MODULECACHE_OVERRIDE"], cache)
        self.assertTrue(kwargs["check"])
        self.assertNotIn(".codex", " ".join(args[0]))

    def test_payload_paths_with_literal_shell_substitutions_are_data(self):
        self.output = self.root / "literal $(touch NEVER_CREATED) 'quoted'.app"
        directory, _ = self.build()
        self.assertEqual(PACKAGE.verify_bundle(directory)["schema_version"], PACKAGE.SCHEMA_VERSION)
        self.assertFalse((self.root / "NEVER_CREATED").exists())

    def test_run_entrypoint_validates_flags_uses_launchservices_and_has_no_broad_kill(self):
        script = SCRIPTS.parent / "script/build_and_run.sh"
        subprocess.run(["bash", "-n", str(script)], check=True, capture_output=True)
        text = script.read_text()
        self.assertIn('/usr/bin/open -n "$app" --args "${app_args[@]}"', text)
        self.assertIn('"${package_args[@]}"', text)
        self.assertNotIn("pkill", text)
        self.assertNotIn("killall", text)
        self.assertNotIn("eval ", text)
        self.assertNotIn("/bin/ps", text)
        self.assertIn('sign-desktop-local.py" --source "$unsigned_app" --output "$app"', text)
        self.assertIn('sign-desktop-local.py" --verify "$app"', text)
        for arguments in (["--profile-dir", "relative"], ["--port", "22"],
                          ["--port", "3060;touch NEVER_CREATED"], ["--unknown"]):
            result = subprocess.run(["bash", str(script)] + arguments,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2)

    @unittest.skipUnless(platform.system() == "Darwin", "macOS script preflight")
    def test_run_entrypoint_preserves_spaced_literal_arguments_without_launching(self):
        scripts = self.repo / "script"
        scripts.mkdir()
        source = SCRIPTS.parent / "script/build_and_run.sh"
        shutil.copyfile(source, scripts / "build_and_run.sh")
        (self.repo / "scripts/package-desktop.py").write_text(
            "import json,sys\nprint(json.dumps(sys.argv[1:]))\nsys.exit(1)\n"
        )
        runtime = self.root / "literal $(touch NEVER_CREATED) runtime"
        result = subprocess.run(["bash", str(scripts / "build_and_run.sh"), "--no-build",
                                 "--profile-dir", str(self.root / "a profile with spaces"),
                                 "--runtime-bundle", str(runtime)], capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 1)
        arguments = json.loads(result.stdout)
        self.assertEqual(arguments[arguments.index("--runtime-bundle") + 1], str(runtime))
        self.assertIn("--no-build", arguments)
        self.assertFalse((self.repo / "NEVER_CREATED").exists())


    def test_legacy_app_is_verified_with_legacy_runtime_exactly(self):
        directory, manifest = self.build()
        nested_root = directory / PACKAGE.RUNTIME_PREFIX
        nested = PACKAGE.LOCAL.verify_bundle(nested_root)
        (nested_root / "bin/nova-memory-connect").unlink()
        nested["schema_version"] = PACKAGE.LOCAL.LEGACY_SCHEMA_VERSION
        nested["files"].pop("bin/nova-memory-connect")
        nested["bundle_id"] = PACKAGE.LOCAL.digest(PACKAGE.LOCAL.canonical_files(nested["files"]))
        (nested_root / "manifest.json").write_text(json.dumps(nested))
        manifest["schema_version"] = PACKAGE.LEGACY_SCHEMA_VERSION
        manifest["files"].pop(PACKAGE.RUNTIME_PREFIX + "/bin/nova-memory-connect")
        manifest["files"][PACKAGE.RUNTIME_PREFIX + "/manifest.json"] = PACKAGE.LOCAL.digest((nested_root / "manifest.json").read_bytes())
        manifest["runtime_bundle_id"] = nested["bundle_id"]
        manifest["bundle_id"] = PACKAGE.LOCAL.digest(PACKAGE.LOCAL.canonical_files(manifest["files"]))
        (directory / PACKAGE.MANIFEST_PATH).write_text(json.dumps(manifest))
        self.assertEqual(PACKAGE.verify_bundle(directory), manifest)
        self.assertEqual(len(manifest["files"]), 13)
        manifest["schema_version"] = PACKAGE.SCHEMA_VERSION
        (directory / PACKAGE.MANIFEST_PATH).write_text(json.dumps(manifest))
        with self.assertRaises(PACKAGE.BundleError):
            PACKAGE.verify_bundle(directory)

    def test_new_app_requires_current_runtime_before_creating_output(self):
        nested = PACKAGE.LOCAL.verify_bundle(self.runtime)
        (self.runtime / "bin/nova-memory-connect").unlink()
        nested["schema_version"] = PACKAGE.LOCAL.LEGACY_SCHEMA_VERSION
        nested["files"].pop("bin/nova-memory-connect")
        nested["bundle_id"] = PACKAGE.LOCAL.digest(PACKAGE.LOCAL.canonical_files(nested["files"]))
        (self.runtime / "manifest.json").write_text(json.dumps(nested))
        with self.assertRaises(PACKAGE.BundleError):
            self.build()
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
