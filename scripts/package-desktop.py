#!/usr/bin/env python3
"""Stage a private, unsigned macOS .app around the bounded local runtime bundle."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import stat
import subprocess
import sys


_LOCAL_SPEC = importlib.util.spec_from_file_location(
    "nova_desktop_local_package", Path(__file__).with_name("package-local.py")
)
LOCAL = importlib.util.module_from_spec(_LOCAL_SPEC)
_LOCAL_SPEC.loader.exec_module(LOCAL)
BundleError = LOCAL.BundleError
SCHEMA_VERSION = "nova.desktop-bundle.v0.2"
LEGACY_SCHEMA_VERSION = "nova.desktop-bundle.v0.1"
NATIVE_BINARY = "Contents/MacOS/NovaMemoryDesktop"
RUNTIME_PREFIX = "Contents/Resources/LocalRuntime"
MANIFEST_PATH = "Contents/Resources/desktop-manifest.json"
PAYLOAD_FILES = frozenset(
    [NATIVE_BINARY, "Contents/Info.plist", "Contents/Resources/NovaLogo.png"]
    + [RUNTIME_PREFIX + "/" + name for name in LOCAL.PAYLOAD_FILES | {"manifest.json"}]
)
LEGACY_PAYLOAD_FILES = frozenset(
    [NATIVE_BINARY, "Contents/Info.plist", "Contents/Resources/NovaLogo.png"]
    + [RUNTIME_PREFIX + "/" + name for name in LOCAL.LEGACY_PAYLOAD_FILES | {"manifest.json"}]
)


def payload_files(schema):
    if schema == SCHEMA_VERSION:
        return PAYLOAD_FILES
    if schema == LEGACY_SCHEMA_VERSION:
        return LEGACY_PAYLOAD_FILES
    raise BundleError("Unsupported desktop bundle version")
MANIFEST_KEYS = frozenset([
    "schema_version", "platform", "machine", "build_profile", "bundle_id", "files",
    "native_binary", "runtime_bundle_id", "integrity_scope", "signature", "publication",
])
DIRECTORIES = frozenset([
    "Contents", "Contents/MacOS", "Contents/Resources", RUNTIME_PREFIX,
    RUNTIME_PREFIX + "/bin",
])


def expected_mode(relative):
    if relative in DIRECTORIES or relative == NATIVE_BINARY:
        return 0o700
    prefix = RUNTIME_PREFIX + "/"
    if relative.startswith(prefix):
        runtime_name = relative[len(prefix):]
        if runtime_name.startswith("bin/") or runtime_name in LOCAL.COMMANDS:
            return 0o700
    return 0o600


def output_path(path, repo_root, runtime_root):
    path = Path(path)
    if not path.is_absolute() or path.name in ("", ".", "..") or path.suffix != ".app":
        raise BundleError("--output must be an absolute new .app directory")
    if path.exists() or path.is_symlink():
        raise BundleError("Output already exists; no existing .app is replaced")
    parent = path.parent.resolve(strict=True)
    if not parent.is_dir():
        raise BundleError("The output parent must be an existing directory")
    output = parent / path.name
    if LOCAL.within(repo_root, output) or LOCAL.within(runtime_root, output) or LOCAL.within(output, runtime_root):
        raise BundleError("Output must not contain or modify source directories")
    for relative in ("apps", "scripts", "script", "target", ".git", ".codex", ".agents",
                     ".artifacts/desktop-build", ".artifacts/desktop-swift-cache"):
        selected = (repo_root / relative).resolve()
        if LOCAL.within(output, selected):
            raise BundleError("Output must not be inside source or compiler cache directories")
    return output


def build_command(repo_root, profile="release"):
    return ["/usr/bin/swift", "build", "--package-path", str(repo_root / "apps/nova-desktop"),
            "--scratch-path", str(repo_root / ".artifacts/desktop-build"),
            "--cache-path", str(repo_root / ".artifacts/desktop-swift-cache"),
            "--manifest-cache", "local", "--disable-automatic-resolution",
            # The managed host sandbox cannot nest SwiftPM's manifest sandbox.
            # This dependency-free package still runs inside the host sandbox.
            "--disable-sandbox",
            "--configuration", profile, "--product", "NovaMemoryDesktop"]


def built_binary(repo_root, machine, profile="release"):
    # Swift 6.4's engine uses Products/Debug; earlier engines use a target triple.
    # Never follow scratch/debug or any symlink in either selected output path.
    candidates = [
        repo_root / ".artifacts/desktop-build/out/Products" / profile.title() / "NovaMemoryDesktop",
        repo_root / ".artifacts/desktop-build" / (machine + "-apple-macosx") / profile / "NovaMemoryDesktop",
    ]
    for candidate in candidates:
        if candidate.exists() or candidate.is_symlink():
            return LOCAL.source_path(repo_root, candidate.relative_to(repo_root).as_posix())
    # Leave an explicit missing-output path for the ordinary source error below.
    return candidates[-1]


def info_plist():
    return plistlib.dumps({
        "CFBundleDevelopmentRegion": "zh_CN",
        "CFBundleDisplayName": "Nova Memory",
        "CFBundleExecutable": "NovaMemoryDesktop",
        "CFBundleIdentifier": "chat.novamemory.desktop.engineering",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "Nova Memory",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "0.3.0",
        "CFBundleVersion": "1",
        "LSMinimumSystemVersion": "14.0",
        "NSHighResolutionCapable": True,
        "NSPrincipalClass": "NSApplication",
        "NSAppTransportSecurity": {"NSAllowsLocalNetworking": True},
    }, sort_keys=True)


def verify_bundle(directory):
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir() or stat.S_IMODE(directory.stat().st_mode) != 0o700:
        raise BundleError("Desktop bundle must be a private regular directory (0700)")
    manifest_bytes = LOCAL.regular_bytes(directory / MANIFEST_PATH)
    if len(manifest_bytes) > 64 * 1024:
        raise BundleError("Desktop manifest exceeds the engineering bundle limit")
    manifest = json.loads(manifest_bytes, object_pairs_hook=LOCAL.unique_object)
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise BundleError("Unsupported desktop manifest fields")
    selected_payload = payload_files(manifest["schema_version"])
    if (manifest["platform"] != "darwin"
            or manifest["machine"] not in ("arm64", "x86_64")
            or manifest["build_profile"] not in ("debug", "release") or manifest["native_binary"] != NATIVE_BINARY
            or manifest["integrity_scope"] != "packaging_integrity_not_developer_identity"
            or manifest["signature"] != "unsigned_not_notarized"
            or manifest["publication"] != "not_published"):
        raise BundleError("Unsupported desktop bundle metadata")
    files = manifest["files"]
    if not isinstance(files, dict) or set(files) != selected_payload:
        raise BundleError("Desktop files differ from the fixed whitelist")
    if manifest["bundle_id"] != LOCAL.digest(LOCAL.canonical_files(files)):
        raise BundleError("Desktop manifest identity mismatch")
    actual = {path.relative_to(directory).as_posix() for path in directory.rglob("*")}
    if actual != selected_payload | DIRECTORIES | {MANIFEST_PATH}:
        raise BundleError("Unexpected desktop bundle payload")
    for path in directory.rglob("*"):
        relative = path.relative_to(directory).as_posix()
        if path.is_symlink() or stat.S_IMODE(path.stat().st_mode) != expected_mode(relative):
            raise BundleError("Desktop payload type or private permissions differ from the contract")
        if relative in DIRECTORIES and not path.is_dir():
            raise BundleError("Desktop subdirectories must be regular directories")
    for relative, expected in files.items():
        if not isinstance(expected, str) or len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected):
            raise BundleError("Invalid desktop payload digest")
        if LOCAL.digest(LOCAL.regular_bytes(directory / relative)) != expected:
            raise BundleError("Desktop payload digest mismatch")
    if plistlib.loads(LOCAL.regular_bytes(directory / "Contents/Info.plist")) != plistlib.loads(info_plist()):
        raise BundleError("Desktop Info.plist differs from the app contract")
    nested_manifest = LOCAL.verify_bundle(directory / RUNTIME_PREFIX)
    expected_runtime_schema = LOCAL.SCHEMA_VERSION if manifest["schema_version"] == SCHEMA_VERSION else LOCAL.LEGACY_SCHEMA_VERSION
    if nested_manifest["schema_version"] != expected_runtime_schema:
        raise BundleError("Desktop/runtime versions differ from the fixed contract")
    if nested_manifest["bundle_id"] != manifest["runtime_bundle_id"]:
        raise BundleError("Embedded runtime identity mismatch")
    if (nested_manifest["platform"] != "darwin" or nested_manifest["machine"] != manifest["machine"]
            or nested_manifest["build_profile"] != manifest["build_profile"]):
        raise BundleError("Embedded runtime platform does not match the desktop app")
    return manifest


def build_bundle(repo_root, output, runtime_bundle, no_build=False, native_binary=None, profile="release"):
    """Read explicit sources only; injectable paths keep unit tests network/build free."""
    if profile not in ("debug", "release"):
        raise BundleError("Build profile must be debug or release")
    repo_root = Path(repo_root).resolve(strict=True)
    runtime_bundle = Path(runtime_bundle)
    if not runtime_bundle.is_absolute() or runtime_bundle.is_symlink():
        raise BundleError("--runtime-bundle must be an absolute regular directory")
    runtime_root = runtime_bundle.resolve(strict=True)
    nested_manifest = LOCAL.verify_bundle(runtime_root)
    if nested_manifest["schema_version"] != LOCAL.SCHEMA_VERSION:
        raise BundleError("New desktop bundles require the current local runtime; existing legacy apps are verified separately")
    system, machine = LOCAL.host_target()
    if system != "darwin" or nested_manifest["platform"] != "darwin" or nested_manifest["machine"] != machine:
        raise BundleError("Desktop and local runtime must target the current macOS architecture")
    if nested_manifest["build_profile"] != profile:
        raise BundleError("Native and runtime build profiles must match")
    output = output_path(output, repo_root, runtime_root)
    if not no_build:
        environment = os.environ.copy()
        cache = repo_root / ".artifacts/desktop-swift-cache"
        environment["CLANG_MODULE_CACHE_PATH"] = str(cache)
        environment["SWIFTPM_MODULECACHE_OVERRIDE"] = str(cache)
        subprocess.run(build_command(repo_root, profile), cwd=repo_root, env=environment, check=True)
    binary = Path(native_binary) if native_binary is not None else built_binary(repo_root, machine, profile)
    if not binary.is_absolute():
        raise BundleError("The prebuilt native binary must have an absolute path")
    try:
        relative_binary = binary.relative_to(repo_root).as_posix()
    except ValueError as error:
        raise BundleError("The native binary must be an explicit repository build output") from error
    binary = LOCAL.source_path(repo_root, relative_binary)
    payload = {
        NATIVE_BINARY: LOCAL.regular_bytes(binary, executable=True),
        "Contents/Info.plist": info_plist(),
        "Contents/Resources/NovaLogo.png": LOCAL.regular_bytes(
            LOCAL.source_path(repo_root, "docs/assets/nova-memory-logo-primary.png")
        ),
    }
    for name in LOCAL.PAYLOAD_FILES | {"manifest.json"}:
        payload[RUNTIME_PREFIX + "/" + name] = LOCAL.regular_bytes(LOCAL.source_path(runtime_root, name))
    files = {name: LOCAL.digest(data) for name, data in payload.items()}
    manifest = {
        "schema_version": SCHEMA_VERSION, "platform": system, "machine": machine,
        "build_profile": profile, "bundle_id": LOCAL.digest(LOCAL.canonical_files(files)),
        "files": files, "native_binary": NATIVE_BINARY,
        "runtime_bundle_id": nested_manifest["bundle_id"],
        "integrity_scope": "packaging_integrity_not_developer_identity",
        "signature": "unsigned_not_notarized", "publication": "not_published",
    }
    output.mkdir(mode=0o700)
    os.chmod(output, 0o700)
    for name in sorted(DIRECTORIES, key=lambda name: (name.count("/"), name)):
        (output / name).mkdir(mode=0o700)
        os.chmod(output / name, 0o700)
    for name, data in payload.items():
        LOCAL.write_new(output / name, data, expected_mode(name))
    LOCAL.write_new(output / MANIFEST_PATH,
                    (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8"), 0o600)
    verify_bundle(output)
    return output, manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Absolute new .app path; never overwritten")
    parser.add_argument("--runtime-bundle", type=Path, required=True, help="Verified local engineering bundle")
    parser.add_argument("--profile", choices=("debug", "release"), default="release")
    parser.add_argument("--no-build", action="store_true", help="Package explicit prebuilt outputs; does not prove build freshness")
    args = parser.parse_args(argv)
    try:
        output, manifest = build_bundle(Path(__file__).resolve().parents[1], args.output, args.runtime_bundle, args.no_build, profile=args.profile)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print("Desktop packaging failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps({"status": "desktop_unsigned_bundle_created", "directory": str(output),
                      "bundle_id": manifest["bundle_id"], "runtime_bundle_id": manifest["runtime_bundle_id"],
                      "build_performed": not args.no_build, "signature": manifest["signature"],
                      "publication": manifest["publication"], "icon": "official_logo_resource_no_icns"}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
