import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "version.py"
spec = importlib.util.spec_from_file_location("product_version", SCRIPT)
VERSION = importlib.util.module_from_spec(spec)
spec.loader.exec_module(VERSION)


class ProductVersionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "apps/core").mkdir(parents=True)
        (self.root / "VERSION").write_text("0.1.0\n")
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["apps/core"]\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n')
        (self.root / "apps/core/Cargo.toml").write_text('[package]\nname = "nova-core"\nversion.workspace = true\n')
        (self.root / "Cargo.lock").write_text('version = 4\n\n[[package]]\nname = "nova-core"\nversion = "0.1.0"\n\n[[package]]\nname = "external-dependency"\nversion = "0.1.0"\nsource = "registry+https://example.invalid"\nchecksum = "unchanged"\n')
        (self.root / "README.md").write_text('<!-- nova-product-version --> **v0.1.0 · Local source preview**\n')
        (self.root / "CHANGELOG.md").write_text("# Changelog\n\n## [0.1.0] - 2026-11-22\n")

    def run_cli(self, *args):
        return subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.root), *args], capture_output=True, text=True)

    def test_check_accepts_matching_tag(self):
        self.assertEqual(VERSION.check(self.root, "v0.1.0"), "0.1.0")

    def test_wrong_tag_is_rejected(self):
        with self.assertRaises(ValueError): VERSION.check(self.root, "v0.2.0")

    def test_detects_cargo_lock_drift(self):
        p = self.root / "Cargo.lock"
        p.write_text(p.read_text().replace('name = "nova-core"\nversion = "0.1.0"', 'name = "nova-core"\nversion = "0.2.0"'))
        with self.assertRaises(ValueError): VERSION.check(self.root)

    def test_update_preserves_third_party_versions_and_metadata(self):
        result = self.run_cli("set", "0.2.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('name = "nova-core"\nversion = "0.2.0"', (self.root / "Cargo.lock").read_text())
        self.assertIn('name = "external-dependency"\nversion = "0.1.0"', (self.root / "Cargo.lock").read_text())
        self.assertIn('edition = "2024"', (self.root / "Cargo.toml").read_text())
        self.assertIn('v0.2.0', (self.root / "README.md").read_text())

    def test_dry_run_leaves_files_unchanged(self):
        before = {p: p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        self.assertEqual(self.run_cli("set", "0.2.0", "--dry-run").returncode, 0)
        self.assertEqual(before, {p: p.read_bytes() for p in before})

    def test_rejects_downgrade_and_invalid_versions(self):
        for version in ["0.0.9", "01.1.0", "0.2", "0.2.0-rc.0"]:
            self.assertNotEqual(self.run_cli("set", version).returncode, 0, version)

    def test_stable_to_same_base_prerelease_is_downgrade(self):
        self.assertNotEqual(self.run_cli("set", "0.1.0-rc.1").returncode, 0)

    def test_missing_release_notes_is_rejected(self):
        (self.root / "CHANGELOG.md").write_text("# Changelog\n")
        with self.assertRaises(ValueError): VERSION.check(self.root)


if __name__ == "__main__":
    unittest.main()
