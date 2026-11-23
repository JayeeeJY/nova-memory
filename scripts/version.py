#!/usr/bin/env python3
"""Inspect or update product metadata without building, tagging or publishing."""
import argparse
from pathlib import Path
import re
import sys
import tomllib

SEMVER = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(alpha|beta|rc)\.([1-9]\d*))?\Z")


def version_files(root, version):
    if not SEMVER.fullmatch(version):
        raise ValueError("Use X.Y.Z or X.Y.Z-alpha.N / beta.N / rc.N")
    manifest_path = root / "Cargo.toml"
    manifest = manifest_path.read_text(encoding="utf-8")
    data = tomllib.loads(manifest)
    old = data["workspace"]["package"]["version"]
    section = re.search(r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", manifest)
    if not section:
        raise ValueError("Missing workspace package metadata")
    body, count = re.subn(r'(?m)^version\s*=\s*"[^"]+"', 'version = "' + version + '"', section[1])
    if count != 1:
        raise ValueError("Expected one workspace version")
    changed = manifest[:section.start(1)] + body + manifest[section.end(1):]
    names = set()
    for member in data["workspace"]["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
        if package.get("version") != {"workspace": True}:
            raise ValueError("Workspace member does not inherit its version: " + member)
        names.add(package["name"])
    lock_path = root / "Cargo.lock"
    lock = lock_path.read_text(encoding="utf-8")
    found = set()
    def update(block):
        name = re.search(r'^name = "([^"]+)"', block[0], re.M)
        if not name or name[1] not in names or re.search(r"^source\s*=", block[0], re.M):
            return block[0]
        found.add(name[1])
        return re.sub(r'(?m)^version = "[^"]+"', 'version = "' + version + '"', block[0], count=1)
    updated_lock = re.sub(r"(?ms)^\[\[package\]\].*?(?=^\[\[package\]\]|\Z)", update, lock)
    if found != names:
        raise ValueError("Workspace members missing from Cargo.lock: " + str(sorted(names - found)))
    files = {root / "VERSION": version + "\n", manifest_path: changed, lock_path: updated_lock}
    for name in ["README.md", "README.en.md"]:
        path = root / name
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        if "<!-- nova-product-version -->" in text:
            files[path] = re.sub(r"(?m)^(<!-- nova-product-version -->).*", r"\g<1> **v" + version + " · Local source preview**", text)
    return old, files


def check(root, tag=None):
    version = (root / "VERSION").read_text(encoding="utf-8").strip()
    _, files = version_files(root, version)
    mismatches = [str(p.relative_to(root)) for p, text in files.items() if p.read_text(encoding="utf-8") != text]
    if mismatches:
        raise ValueError("Version mismatch: " + ", ".join(mismatches))
    if tag and tag != "v" + version:
        raise ValueError("Tag must be v" + version)
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    if not re.search(r"(?m)^## \[" + re.escape(version) + r"\] - \d{4}-\d{2}-\d{2}$", changelog):
        raise ValueError("Add a dated CHANGELOG entry for " + version)
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("show")
    verify = commands.add_parser("check")
    verify.add_argument("--tag")
    setter = commands.add_parser("set")
    setter.add_argument("version")
    setter.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.command == "show":
        print((root / "VERSION").read_text(encoding="utf-8").strip())
    elif args.command == "check":
        print("Version metadata verified: " + check(root, args.tag))
    else:
        old, files = version_files(root, args.version)
        before = (root / "VERSION").read_text(encoding="utf-8").strip()
        if not SEMVER.fullmatch(before):
            raise ValueError("Invalid existing product version")
        def order(value):
            match = SEMVER.fullmatch(value)
            return tuple(map(int, match.groups()[:3])) + ({"alpha": 0, "beta": 1, "rc": 2, None: 3}[match[4]], int(match[5] or 0))
        if order(args.version) < order(before):
            raise ValueError("Refusing a product version downgrade")
        if not args.dry_run:
            for path, text in files.items():
                path.write_text(text, encoding="utf-8")
        print(("Would update " if args.dry_run else "Updated ") + old + " -> " + args.version)
        print("Add the CHANGELOG entry, validate compatibility and run checks before release. No tag or release was created.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
