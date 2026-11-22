#!/usr/bin/env python3
"""Create a bounded, unsigned local engineering bundle from explicit build outputs."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import stat
import subprocess
import sys
import tarfile


SCHEMA_VERSION = "nova.local-bundle.v0.3"
LEGACY_SCHEMA_VERSION = "nova.local-bundle.v0.2"
CARGO_VERSION = (Path(__file__).resolve().parents[1] / "VERSION").read_text(encoding="utf-8").strip()
COMMANDS = {
    "启动 Nova Memory.command": "launch",
    "停止 Nova Memory.command": "stop",
    "检查 Nova Memory.command": "status",
    "复制 Owner 凭据.command": "copy-owner",
}
LEGACY_PAYLOAD_FILES = frozenset(
    ["bin/nova-review", "bin/nova-local-init", "bin/nova-local", "bin/nova-capture-hook", "README.md"]
    + list(COMMANDS)
)
PAYLOAD_FILES = LEGACY_PAYLOAD_FILES | {"bin/nova-memory-connect"}


def payload_files(schema):
    if schema == SCHEMA_VERSION:
        return PAYLOAD_FILES
    if schema == LEGACY_SCHEMA_VERSION:
        return LEGACY_PAYLOAD_FILES
    raise BundleError("Unsupported local bundle version")
MANIFEST_KEYS = frozenset(
    ["schema_version", "platform", "machine", "build_profile", "cargo_version", "bundle_id", "files"]
)


class BundleError(ValueError):
    pass


def canonical_files(files):
    return json.dumps(files, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def host_target():
    system = platform.system().lower()
    machine = platform.machine().lower()
    machine = {"aarch64": "arm64", "amd64": "x86_64"}.get(machine, machine)
    if system not in ("darwin", "linux") or machine not in ("arm64", "x86_64"):
        raise BundleError("Only darwin/linux arm64/x86_64 engineering bundles are supported")
    return system, machine


def regular_bytes(path, executable=False):
    """Read one selected regular source without following a final symlink."""
    # NONBLOCK prevents an unexpected FIFO/device source from hanging before fstat.
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    fd = os.open(path, flags)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise BundleError("A selected package source is not a regular file")
        if executable and not info.st_mode & 0o111:
            raise BundleError("A selected prebuilt binary is not executable")
        with os.fdopen(fd, "rb") as source:
            fd = -1
            data = source.read()
        if not data:
            raise BundleError("A selected package source is empty")
        return data
    finally:
        if fd >= 0:
            os.close(fd)


def source_path(repo_root, relative):
    selected = repo_root / relative
    current = selected
    while current != repo_root:
        if current.is_symlink():
            raise BundleError("Package sources must not use symlinks")
        current = current.parent
    if selected.resolve(strict=True).relative_to(repo_root).as_posix() != relative:
        raise BundleError("Package source aliases are not supported")
    return selected


def within(path, parent):
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def new_output(path, repo_root):
    path = Path(path)
    if not path.is_absolute() or path.name in ("", ".", ".."):
        raise BundleError("--output must be an absolute new directory")
    if path.exists() or path.is_symlink():
        raise BundleError("Output already exists; no existing directory is replaced")
    parent = path.parent.resolve(strict=True)
    if not parent.is_dir():
        raise BundleError("The output parent must be an existing directory")
    output = parent / path.name
    if output == repo_root or within(repo_root, output):
        raise BundleError("Output must not alias or contain the source repository")
    for relative in ("target/debug", "target/release", "scripts"):
        source = (repo_root / relative).resolve()
        if output == source or within(source, output):
            raise BundleError("Output must not alias a package source directory")
    return output


def command_source(action):
    return ('''#!/bin/bash
set -euo pipefail
bundle="$(cd -- "$(dirname -- "$0")" && pwd -P)"
directory="${NOVA_MEMORY_DATA_DIR:-$HOME/Library/Application Support/Nova Memory/local-workspace-v03}"
result=0
"$bundle/bin/nova-local" ''' + action + ''' --bundle "$bundle" --directory "$directory" || result=$?
if [[ -t 0 ]]; then
  read -r -p '按回车关闭此窗口。' _ || true
fi
exit "$result"
''').encode("utf-8")


def readme_source(system, machine, profile):
    return ("""# Nova Memory 本地工程包

当前平台：{system} / {machine}；构建模式：{profile}；版本：{version}。

本包仅用于本地工程验证，未签名、未公证，尚未公开发布。macOS Gatekeeper、真实设备分发、其他架构及系统版本需要另行验收。Linux 可运行命令行控制器；Finder 双击入口只适用于 macOS。

消费运行时由随包 Rust 二进制提供，不需要 Python、Rust、Cargo、Node 或源码仓库。本包不自动安装环境、账户、模型或网络服务。开发侧构建/打包工具仍可使用 Python。

## 使用

- macOS 双击 `启动 Nova Memory.command`。控制器检查完整性后启动；只有目标目录不存在时才初始化全新的独立本地库。
- 默认数据目录为 `~/Library/Application Support/Nova Memory/local-workspace-v03`。命令行可用 `NOVA_MEMORY_DATA_DIR` 指定另一个绝对路径。已有数据不会因停止、移动本包或重复启动而删除或覆盖。
- 双击 `检查 Nova Memory.command` 查看实际状态与本地地址；双击 `停止 Nova Memory.command` 仅停止匹配的本包服务。
- 工作台默认直接进入。可在“设置与数据 → 打开工作台的方式”按需开启凭据解锁；开启后双击 `复制 Owner 凭据.command` 并在解锁框粘贴。凭据不打印到终端；不要把 Owner 凭据或 Vault 主钥填进 Agent 的 MCP 客户端配置。
- 终端入口为 `bin/nova-local {{launch|stop|status|copy-owner}} --bundle /absolute/bundle --directory /absolute/data`。所有含空格的路径需要引用。
- 本包包含 `bin/nova-memory-connect` 本机 MCP 连接桥。在工作台为本机客户端创建连接时，可下载不含明文令牌的启动配置；私有凭据保留在资料目录。请保持 Nova 服务运行。移动程序包后，在工作台恢复连接配置可获得当前程序路径；不会改写已安装客户端的配置。
- 工作台提供受控的 MCP 网关及远端通路控制。ChatGPT 通路需要另行安装官方 `tunnel-client`、提供对应 Tunnel ID 和运行凭据；Grok 实验性临时 HTTPS 需要另行安装 `cloudflared`。本包不包含或自动安装这两项工具，不读取它们原有的配置或凭据。远端认证兼容和实际平台调用仍需分别验收。
- “在其他 AI 中使用”也可生成一次性上下文包：选择记忆、预览后复制或下载。该明文副本保留条件、来源和版本信息；交付后不能撤回或自动同步，不等于目标平台原生记忆或 MCP 接通。

新库没有示例记忆，默认模型未配置。可保存材料，结构化处理等待明确的模型配置；本包不代表真实模型、独立 Gold 或新 AI Host 已验收。旧验收库与合成预览不被自动迁移或接管。

## 完整性与构建

`manifest.json` 记录固定文件白名单和 SHA-256。它可检查传输损坏及打包后文件变更，不是开发者签名或可信发布证明。包中不包含数据库、来源、个人配置、凭据、仓库或编译缓存。

## 显式升级与回退

仅在停止实例后操作。`bin/nova-local upgrade-preview --bundle /原包 --directory /资料目录 --candidate /新包` 返回预览及 plan_id；核对后用 `upgrade-apply` 的相同参数加 `--plan-id` 选择新包。下一次原有入口启动会使用所选包，切换本身不启动程序、不迁移数据。程序包需保留原位置。

`upgrade-rollback` 只选择前一个完整程序包；数据和配置发生变化后会拒绝自动回退，需要另行核对数据兼容。它不会恢复旧数据。当前仅支持已知的相同包/配置/数据库格式，不是通用数据库降级器。

对明确取得的未压缩 USTAR 包，可用 `unpack --archive /已取得的包.tar --directory /全新程序目录` 校验并解包。没有自动下载和执行，没有签名认证；仅允许固定文件清单，拒绝链接、路径穿越和重复内容。

当前程序包格式为 v0.3；新控制器仍可检查 v0.2 的固定文件清单。旧 v0.2 控制器不认识新增桥程序，不能承诺其原入口自动理解新包。跨格式切换请停止服务后明确使用新包入口；不改写旧包，也不自动迁移资料或真实客户端配置。新包到旧格式不是默认升级路径；未经单独数据兼容核对会被拒绝，已记录的回退仍要求资料未变化。

默认打包使用锁定依赖的离线构建。`--no-build` 仅供显式预构建产物的工程演练；文件 hash 不能证明它与当前源码一致，不能据此宣称当前版本完成发行。正式对外安装、签名、公证、升级、恢复和兼容验收另行进行。
""".format(system=system, machine=machine, profile=profile, version=CARGO_VERSION)).encode("utf-8")


def write_new(path, data, mode):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, mode)
    with os.fdopen(fd, "wb") as destination:
        destination.write(data)
        destination.flush()
        os.fsync(destination.fileno())
        os.fchmod(destination.fileno(), mode)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise BundleError("Duplicate manifest keys are not allowed")
        result[key] = value
    return result


def verify_bundle(directory):
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise BundleError("Bundle must be a regular directory")
    if stat.S_IMODE(directory.stat().st_mode) != 0o700:
        raise BundleError("Bundle directory must be private (0700)")
    manifest_data = regular_bytes(directory / "manifest.json")
    if len(manifest_data) > 64 * 1024:
        raise BundleError("Manifest exceeds the engineering bundle limit")
    manifest = json.loads(manifest_data, object_pairs_hook=unique_object)
    if not isinstance(manifest, dict) or set(manifest) != MANIFEST_KEYS:
        raise BundleError("Unsupported manifest fields")
    selected_payload = payload_files(manifest["schema_version"])
    if (manifest["platform"] not in ("darwin", "linux")
            or manifest["machine"] not in ("arm64", "x86_64")
            or manifest["build_profile"] not in ("debug", "release")
            or manifest["cargo_version"] != CARGO_VERSION):
        raise BundleError("Unsupported bundle manifest")
    files = manifest["files"]
    if not isinstance(files, dict) or set(files) != selected_payload:
        raise BundleError("Bundle files differ from the fixed whitelist")
    if manifest["bundle_id"] != digest(canonical_files(files)):
        raise BundleError("Bundle manifest identity mismatch")
    expected_tree = selected_payload | {"manifest.json", "bin"}
    actual_tree = {p.relative_to(directory).as_posix() for p in directory.rglob("*")}
    if actual_tree != expected_tree:
        raise BundleError("Unexpected bundle payload")
    for path in directory.rglob("*"):
        relative = path.relative_to(directory).as_posix()
        expected_mode = 0o700 if relative in ("bin",) or relative.startswith("bin/") or relative in COMMANDS else 0o600
        if path.is_symlink() or stat.S_IMODE(path.stat().st_mode) != expected_mode:
            raise BundleError("Bundle file type or private permissions differ from the contract")
    for relative in ("bin",):
        path = directory / relative
        if path.is_symlink() or not path.is_dir():
            raise BundleError("Bundle subdirectories must be regular directories")
    for relative, expected in files.items():
        if not isinstance(expected, str) or len(expected) != 64 or any(c not in "0123456789abcdef" for c in expected):
            raise BundleError("Invalid payload digest")
        if digest(regular_bytes(directory / relative)) != expected:
            raise BundleError("Bundle payload digest mismatch")
    return manifest


def build_bundle(repo_root, output, profile="release", no_build=False):
    """Package only explicit files; repo_root is injectable for offline tests."""
    repo_root = Path(repo_root).resolve(strict=True)
    if profile not in ("debug", "release"):
        raise BundleError("Build profile must be debug or release")
    output = new_output(output, repo_root)
    system, machine = host_target()
    if not no_build:
        environment = os.environ.copy()
        environment["CARGO_INCREMENTAL"] = "0"
        environment["CARGO_TARGET_DIR"] = str(repo_root / "target")
        environment.pop("CARGO_BUILD_TARGET", None)
        command = ["cargo", "build", "--manifest-path", str(repo_root / "Cargo.toml"),
                   "--locked", "--offline", "-p", "nova-review", "--bin", "nova-review",
                   "--bin", "nova-local-init", "--bin", "nova-local", "--bin", "nova-capture-hook",
                   "--bin", "nova-memory-connect"]
        if profile == "release":
            command.append("--release")
        subprocess.run(command, cwd=repo_root, env=environment, check=True)
    payload = {
        "bin/nova-review": regular_bytes(source_path(repo_root, "target/" + profile + "/nova-review"), True),
        "bin/nova-local-init": regular_bytes(source_path(repo_root, "target/" + profile + "/nova-local-init"), True),
        "bin/nova-local": regular_bytes(source_path(repo_root, "target/" + profile + "/nova-local"), True),
        "bin/nova-capture-hook": regular_bytes(source_path(repo_root, "target/" + profile + "/nova-capture-hook"), True),
        "bin/nova-memory-connect": regular_bytes(source_path(repo_root, "target/" + profile + "/nova-memory-connect"), True),
        "README.md": readme_source(system, machine, profile),
    }
    payload.update({name: command_source(action) for name, action in COMMANDS.items()})
    if set(payload) != PAYLOAD_FILES:
        raise BundleError("Internal package whitelist mismatch")
    files = {name: digest(data) for name, data in payload.items()}
    manifest = {"schema_version": SCHEMA_VERSION, "platform": system, "machine": machine,
                "build_profile": profile, "cargo_version": CARGO_VERSION,
                "bundle_id": digest(canonical_files(files)), "files": files}
    # mkdir is the final no-overwrite check, including a destination raced into place.
    output.mkdir(mode=0o700)
    os.chmod(output, 0o700)
    for name in ("bin",):
        (output / name).mkdir(mode=0o700)
        os.chmod(output / name, 0o700)
    for name, data in payload.items():
        mode = 0o700 if name.startswith("bin/") or name in COMMANDS else 0o600
        write_new(output / name, data, mode)
    write_new(output / "manifest.json", (json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8"), 0o600)
    verify_bundle(output)
    return output, manifest


def archive_bundle(directory, archive):
    """Fixed files only, uncompressed USTAR for the dependency-free Rust consumer."""
    directory=Path(directory).resolve(strict=True)
    manifest = verify_bundle(directory)
    archive=Path(archive)
    if not archive.is_absolute() or any(part in ('.','..') for part in archive.parts):
        raise BundleError('Archive must be an absolute new file')
    parent=archive.parent.resolve(strict=True)
    if archive.parent!=parent or within(archive,directory):
        raise BundleError('Archive parent must be canonical and outside the bundle')
    fd=os.open(archive,os.O_WRONLY|os.O_CREAT|os.O_EXCL|getattr(os,'O_NOFOLLOW',0),0o600)
    try:
        with os.fdopen(fd,'wb') as target:
            with tarfile.open(fileobj=target,mode='w',format=tarfile.USTAR_FORMAT) as tar:
                for relative in sorted(payload_files(manifest['schema_version'])|{'manifest.json'}):
                    source=directory/relative
                    info=tarfile.TarInfo(relative);info.size=source.stat().st_size
                    info.mode=0o700 if relative.startswith('bin/') or relative in COMMANDS else 0o600
                    with source.open('rb') as content:tar.addfile(info,content)
            target.flush();os.fsync(target.fileno())
    except Exception:
        archive.unlink(missing_ok=True)
        raise
    return archive


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Absolute new bundle directory; never overwritten")
    parser.add_argument("--profile", choices=("debug", "release"), default="release")
    parser.add_argument("--no-build", action="store_true", help="Engineering only: package explicit prebuilt outputs; freshness is not established")
    parser.add_argument("--archive",type=Path,help="Optional absolute new uncompressed USTAR file; no publication")
    args = parser.parse_args(argv)
    try:
        output, manifest = build_bundle(Path(__file__).resolve().parents[1], args.output, args.profile, args.no_build)
        archive = archive_bundle(output,args.archive) if args.archive else None
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print("Local bundle packaging failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps({"status": "local_unsigned_bundle_created", "directory": str(output),
                      "bundle_id": manifest["bundle_id"], "build_performed": not args.no_build,
                      "archive":str(archive) if archive else None,"signature": "unsigned_not_notarized", "publication": "not_published"}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
