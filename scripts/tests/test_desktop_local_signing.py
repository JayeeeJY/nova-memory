"""Local app sealing checks; synthetic unit fixtures plus explicit real-artifact opt-in."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock
import test_desktop_package as fixtures

SPEC = importlib.util.spec_from_file_location(
    "nova_desktop_local_signing", Path(__file__).resolve().parents[1] / "sign-desktop-local.py")
SIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGN)


class LocalSigningTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.DesktopPackageTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        self.source, self.original = self.fixture.build()
        self.output = self.fixture.root / "local sealed $(touch NEVER_CREATED) 'copy'.app"
        self.events = []
        def fake_codesign(app, *, sign=False):
            self.events.append(sign)
            if sign:
                self.assertFalse((app / SIGN.DESKTOP.MANIFEST_PATH).exists())
                (app / "Contents/_CodeSignature").mkdir()
                (app / SIGN.SEAL).write_bytes(b"synthetic-seal-not-a-signature")
                with (app / SIGN.DESKTOP.NATIVE_BINARY).open('ab') as output:
                    output.write(b"synthetic-code-signature")
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(SIGN, "codesign", side_effect=fake_codesign).start()
        mock.patch.object(SIGN.LOCAL, "host_target", return_value=("darwin", "arm64")).start()

    def build(self):
        return SIGN.seal(self.source, self.output)

    def test_seal_precedes_sidecar_hashes_and_source_runtime_remain_unchanged(self):
        original_bytes = (self.source / SIGN.DESKTOP.NATIVE_BINARY).read_bytes()
        result = self.build()
        self.assertEqual(self.events, [True, False])
        self.assertEqual(SIGN.verify(self.output), result)
        self.assertEqual(set(result['files']), SIGN.PAYLOAD)
        self.assertEqual(len(result['files']), 15)
        self.assertEqual((self.source / SIGN.DESKTOP.NATIVE_BINARY).read_bytes(), original_bytes)
        self.assertEqual(SIGN.DESKTOP.verify_bundle(self.source), self.original)
        self.assertEqual(result['source_bundle_id'], self.original['bundle_id'])
        self.assertEqual(result['runtime_bundle_id'], self.original['runtime_bundle_id'])
        self.assertNotIn(str(self.fixture.repo), SIGN.sidecar(self.output).read_text())
        self.assertFalse((self.fixture.repo / 'NEVER_CREATED').exists())
        self.assertFalse(result['developer_identity_verified'])
        self.assertFalse(result['notarized'])

    def test_existing_output_or_sidecar_never_overwritten(self):
        metadata = SIGN.sidecar(self.output)
        metadata.write_text('KEEP')
        with self.assertRaises(SIGN.BundleError): self.build()
        self.assertEqual(metadata.read_text(), 'KEEP')
        self.assertFalse(self.output.exists())
        metadata.unlink()
        self.output.mkdir()
        with self.assertRaises(SIGN.BundleError): self.build()
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(self.events, [])

    def test_invalid_source_refused_before_copy_or_signing(self):
        (self.source / 'Contents/Resources/owner.key').write_text('EXCLUDED')
        with self.assertRaises(SIGN.BundleError): self.build()
        self.assertFalse(self.output.exists())
        self.assertEqual(self.events, [])

    def test_tampered_payload_symlink_hardlink_and_permissions_rejected(self):
        self.build()
        path = self.output / 'Contents/Resources/NovaLogo.png'
        original = path.read_bytes()
        path.write_bytes(b'changed')
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)
        path.write_bytes(original)
        path.chmod(0o644)
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)
        path.chmod(0o600)
        outside = self.fixture.root / 'outside-logo'
        outside.write_bytes(original)
        path.unlink(); path.symlink_to(outside)
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)
        path.unlink(); os.link(outside, path)
        outside.chmod(0o600)
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)

    def test_duplicate_unknown_or_trust_promoting_metadata_rejected(self):
        result = self.build()
        metadata = SIGN.sidecar(self.output)
        for changes in ({'notarized':True}, {'developer_identity_verified':1},
                        {'extra':True}, {'signature':'Developer ID'}):
            metadata.write_text(json.dumps({**result, **changes}))
            with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)
        metadata.write_text('{"notarized":true,' + json.dumps(result)[1:])
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)
        metadata.write_text(json.dumps(result) + ' ' * 65536)
        with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)

    def test_missing_sidecar_and_apple_verification_failure_are_not_success(self):
        self.build()
        metadata = SIGN.sidecar(self.output)
        contents = metadata.read_bytes()
        metadata.unlink()
        with self.assertRaises(OSError): SIGN.verify(self.output)
        metadata.write_bytes(contents);metadata.chmod(0o600)
        with mock.patch.object(SIGN, 'codesign', side_effect=SIGN.BundleError('verification failed')):
            with self.assertRaises(SIGN.BundleError): SIGN.verify(self.output)

    def test_signing_failure_does_not_emit_success_sidecar_or_change_source(self):
        with mock.patch.object(SIGN, 'codesign', side_effect=SIGN.BundleError('signing failed')):
            with self.assertRaises(SIGN.BundleError): self.build()
        self.assertFalse(SIGN.sidecar(self.output).exists())
        self.assertEqual(SIGN.DESKTOP.verify_bundle(self.source), self.original)

    def test_signed_pair_relocates_without_old_paths_or_resealing(self):
        result = self.build()
        moved_dir = self.fixture.root / 'moved with spaces'
        moved_dir.mkdir()
        moved = moved_dir / self.output.name
        SIGN.sidecar(self.output).rename(SIGN.sidecar(moved));self.output.rename(moved)
        shutil.rmtree(self.source)
        self.assertEqual(SIGN.verify(moved), result)
        self.assertNotIn(str(self.source), SIGN.sidecar(moved).read_text())


@unittest.skipUnless(os.environ.get('NOVA_SIGNED_APP'), 'explicit locally signed app required')
class ActualSignedAppTests(unittest.TestCase):
    def test_actual_apple_resource_seal_relocation_and_rehashed_tamper(self):
        source = Path(os.environ['NOVA_SIGNED_APP'])
        original = SIGN.verify(source)
        with tempfile.TemporaryDirectory(prefix='nova signed relocation ') as temporary:
            moved = Path(temporary).resolve() / 'relocated Nova Memory.app'
            shutil.copytree(source, moved)
            shutil.copyfile(SIGN.sidecar(source), SIGN.sidecar(moved))
            SIGN.sidecar(moved).chmod(0o600)
            self.assertEqual(SIGN.verify(moved), original)
            logo = moved / 'Contents/Resources/NovaLogo.png'
            raw = logo.read_bytes()
            logo.write_bytes(raw + b'SYNTHETIC_RESOURCE_TAMPER')
            with self.assertRaises(SIGN.BundleError): SIGN.verify(moved)
            # Even a recomputed untrusted sidecar must not bypass Apple's resource seal.
            altered = json.loads(json.dumps(original))
            altered['files']['Contents/Resources/NovaLogo.png'] = hashlib.sha256(logo.read_bytes()).hexdigest()
            altered['bundle_id'] = SIGN.LOCAL.digest(SIGN.LOCAL.canonical_files(altered['files']))
            SIGN.sidecar(moved).write_text(json.dumps(altered))
            with self.assertRaisesRegex(SIGN.BundleError, 'signature verification failed'):
                SIGN.verify(moved)
            logo.write_bytes(raw)
            SIGN.sidecar(moved).write_text(json.dumps(original))
            self.assertEqual(SIGN.verify(moved), original)


if __name__ == '__main__': unittest.main()
