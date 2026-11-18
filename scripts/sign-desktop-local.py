#!/usr/bin/env python3
"""Create a new, locally ad-hoc sealed .app from a verified engineering app.

No identity lookup, keychain, timestamp server, notarization, publication or launch.
The integrity sidecar is outside the sealed app to avoid a signature/hash cycle.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

_SPEC = importlib.util.spec_from_file_location(
    "nova_local_signed_desktop", Path(__file__).with_name("package-desktop.py"))
DESKTOP = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(DESKTOP)
LOCAL = DESKTOP.LOCAL
BundleError = LOCAL.BundleError
SCHEMA = "nova.desktop-local-signature.v0.1"
SEAL = "Contents/_CodeSignature/CodeResources"
DIRECTORIES = DESKTOP.DIRECTORIES | {"Contents/_CodeSignature"}
PAYLOAD = DESKTOP.PAYLOAD_FILES | {SEAL}
KEYS = frozenset(("schema_version", "platform", "machine", "build_profile", "bundle_id",
                  "source_bundle_id", "runtime_bundle_id", "files", "signature",
                  "developer_identity_verified", "notarized", "publication"))


def sidecar(app):
    return app.with_name(app.name + ".integrity.json")


def codesign(app, *, sign=False):
    args = (["--force", "--sign", "-", "--timestamp=none"] if sign else
            ["--verify", "--deep", "--strict", "--all-architectures", "--verbose=2"])
    try:
        subprocess.run(["/usr/bin/codesign", *args, str(app)], check=True, timeout=30,
                       stdin=subprocess.DEVNULL, capture_output=True,
                       env={"PATH":"/usr/bin:/bin", "LANG":"en_US.UTF-8"})
    except (OSError, subprocess.SubprocessError) as error:
        raise BundleError("Local ad-hoc signing failed" if sign else
                          "App resource signature verification failed") from error


def private_file(path, *, limit=None):
    info = path.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o600
            or (limit is not None and info.st_size > limit)):
        raise BundleError("Integrity metadata must be a bounded private regular file")
    return LOCAL.regular_bytes(path)


def is_digest(value):
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def verify(app):
    app = Path(app)
    if (not app.is_absolute() or app.is_symlink() or not app.is_dir()
            or app.suffix != ".app" or app.parent != app.parent.resolve(strict=True)
            or stat.S_IMODE(app.stat().st_mode) != 0o700):
        raise BundleError("Expected an absolute private regular .app directory")
    manifest = json.loads(private_file(sidecar(app), limit=64 * 1024),
                          object_pairs_hook=LOCAL.unique_object)
    if not isinstance(manifest, dict) or set(manifest) != KEYS:
        raise BundleError("Unsupported signed app integrity fields")
    if (manifest["schema_version"] != SCHEMA or manifest["platform"] != "darwin"
            or manifest["machine"] not in ("arm64", "x86_64")
            or manifest["build_profile"] not in ("debug", "release")
            or manifest["signature"] != "local_adhoc_sealed_not_notarized"
            or manifest["developer_identity_verified"] is not False
            or manifest["notarized"] is not False or manifest["publication"] != "not_published"
            or not is_digest(manifest["source_bundle_id"]) or not is_digest(manifest["runtime_bundle_id"])):
        raise BundleError("Unsupported local signing metadata")
    files = manifest["files"]
    if not isinstance(files, dict) or set(files) != PAYLOAD or not all(map(is_digest, files.values())):
        raise BundleError("Signed app payload differs from the fixed whitelist")
    if manifest["bundle_id"] != LOCAL.digest(LOCAL.canonical_files(files)):
        raise BundleError("Signed app identity mismatch")
    actual = {p.relative_to(app).as_posix() for p in app.rglob("*")}
    if actual != PAYLOAD | DIRECTORIES:
        raise BundleError("Unexpected signed app content")
    for relative in actual:
        path = app / relative
        entry = path.lstat()
        mode = 0o700 if relative in DIRECTORIES else DESKTOP.expected_mode(relative)
        if (stat.S_ISLNK(entry.st_mode) or entry.st_uid != os.geteuid()
                or stat.S_IMODE(entry.st_mode) != mode):
            raise BundleError("Signed app permissions or owner mismatch")
        if relative in DIRECTORIES:
            if not stat.S_ISDIR(entry.st_mode):
                raise BundleError("Signed app directory is not regular")
        elif (not stat.S_ISREG(entry.st_mode) or entry.st_nlink != 1
              or LOCAL.digest(LOCAL.regular_bytes(path)) != files[relative]):
            raise BundleError("Signed app content digest mismatch")
    if LOCAL.regular_bytes(app / "Contents/Info.plist") != DESKTOP.info_plist():
        raise BundleError("Signed app Info.plist differs from the fixed contract")
    nested = LOCAL.verify_bundle(app / DESKTOP.RUNTIME_PREFIX)
    if (nested["schema_version"] != LOCAL.SCHEMA_VERSION
            or nested["bundle_id"] != manifest["runtime_bundle_id"]
            or any(nested[k] != manifest[k] for k in ("platform", "machine", "build_profile"))):
        raise BundleError("Signed app runtime binding mismatch")
    codesign(app)
    return manifest


def seal(source, output):
    source = Path(source)
    if (not source.is_absolute() or source.parent != source.parent.resolve(strict=True)):
        raise BundleError("Source app must be an absolute canonical engineering path")
    original = DESKTOP.verify_bundle(source)
    if original["schema_version"] != DESKTOP.SCHEMA_VERSION:
        raise BundleError("Only the current engineering app format can be sealed")
    if LOCAL.host_target() != ("darwin", original["machine"]):
        raise BundleError("Local signing requires the matching macOS architecture")
    repo = Path(__file__).resolve().parents[1]
    output = DESKTOP.output_path(output, repo, source)
    metadata = sidecar(output)
    if metadata.exists() or metadata.is_symlink():
        raise BundleError("Integrity sidecar exists; no existing file is replaced")
    output.mkdir(mode=0o700)
    output.chmod(0o700)
    for relative in sorted(DESKTOP.DIRECTORIES, key=lambda p:(p.count('/'), p)):
        (output / relative).mkdir(mode=0o700)
        (output / relative).chmod(0o700)
    # Read only the verified fixed whitelist, not arbitrary source trees/xattrs.
    for relative in sorted(DESKTOP.PAYLOAD_FILES | {DESKTOP.MANIFEST_PATH}):
        raw = LOCAL.regular_bytes(LOCAL.source_path(source, relative))
        LOCAL.write_new(output / relative, raw, DESKTOP.expected_mode(relative))
    if DESKTOP.verify_bundle(output) != original:
        raise BundleError("Source app changed during staging")
    # This manifest hashes the unsigned executable; sealing changes that executable.
    # Keep final integrity metadata outside the app, after codesign has sealed it.
    (output / DESKTOP.MANIFEST_PATH).unlink()
    codesign(output, sign=True)
    actual = {p.relative_to(output).as_posix() for p in output.rglob('*')}
    if actual != PAYLOAD | DIRECTORIES:
        raise BundleError("Signing generated unexpected bundle content")
    for relative in actual:
        path = output / relative
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
            raise BundleError("Unexpected link or special file after signing")
        path.chmod(0o700 if relative in DIRECTORIES else DESKTOP.expected_mode(relative))
    manifest = {
        "schema_version":SCHEMA, "platform":"darwin", "machine":original["machine"],
        "build_profile":original["build_profile"], "source_bundle_id":original["bundle_id"],
        "runtime_bundle_id":original["runtime_bundle_id"],
        "files":{p:LOCAL.digest(LOCAL.regular_bytes(output / p)) for p in sorted(PAYLOAD)},
        "signature":"local_adhoc_sealed_not_notarized", "developer_identity_verified":False,
        "notarized":False, "publication":"not_published",
    }
    manifest["bundle_id"] = LOCAL.digest(LOCAL.canonical_files(manifest["files"]))
    LOCAL.write_new(metadata, (json.dumps(manifest,ensure_ascii=False,indent=2)+'\n').encode(),0o600)
    verify(output)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, help="Verified explicit engineering .app; remains unchanged")
    parser.add_argument("--output", type=Path, help="New absolute signed copy; never overwrite")
    parser.add_argument("--verify", type=Path, help="Verify an existing signed copy and its adjacent sidecar")
    args = parser.parse_args()
    if bool(args.verify) == bool(args.source or args.output) or (not args.verify and not (args.source and args.output)):
        parser.error("choose --verify APP or --source APP --output NEW_APP")
    try:
        manifest = verify(args.verify) if args.verify else seal(args.source, args.output)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print("Local app sealing failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps({"status":"local_adhoc_verified", "bundle_id":manifest["bundle_id"],
                      "signature":manifest["signature"], "notarized":False,
                      "developer_identity_verified":False, "publication":"not_published"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
