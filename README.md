# Nova Memory · 本地版源码预览

Nova Memory 是一个本地记忆工作台，支持保存原文、整理和查找记忆，以及通过受限的 AI 连接或手动分享在不同工具里续接任务。配置整理模型后，工作台可把已保存的材料整理成结构化记忆。

## 从源码构建

桌面版构建需要 Apple 芯片 Mac、带 macOS SDK 的 Xcode、Rust 1.97.1 和 Python 3.11 或更新版本。命令会先下载 Cargo.lock 指定的依赖，再从本机缓存离线编译。应用运行无需另外安装 Rust 或 Python。

```sh
cargo fetch --locked
./script/build_and_run.sh
```

默认入口构建并打开桌面 App。构建产物保存在 `.artifacts/`。首次点击「开始本地使用」后，可在工作台快速采集一条材料并查看原文。默认 Vault 位于 `~/Library/Application Support/Nova Memory/local-workspace-v03`。开发时可用 `--profile-dir` 指定一个新的绝对路径，并用 `--port` 指定一个空闲端口。

仅构建时，先用 `scripts/package-local.py` 生成运行包，再用 `scripts/package-desktop.py` 生成 App。两个脚本的 `--help` 列出参数。这套脚本产出本机工程候选，公开下载需要相应的 Developer ID 签名与 Apple 公证。

## 验证

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*package.py'
python3 -m unittest discover -s scripts/tests -p 'test_desktop_local_signing.py'
```

## 许可与源码

本地版代码采用 `AGPL-3.0-only`。完整许可与适用路径见随附的 `LICENSE` 和 `LICENSE_SCOPE.md`，第三方软件保持各自许可及归属声明。Nova Memory、Atom Awareness 名称和 Logo 的品牌使用范围单独管理。

本地体验包随附 `Nova-Memory-local-source-AGPL.tar.gz`，其中包含本地产品代码、构建脚本和锁定依赖版本。运行修改后的网络服务时，应按 AGPL v3 第 13 条向远程用户提供对应版本源码入口。公开 Release 将同时提供安装包与匹配版本源码。

工作台底部的「许可与源码」可以查看许可全文与随包源码的取得方式。开发与反馈说明见 `CONTRIBUTING.md`，GitHub 问题表单位于 `.github/ISSUE_TEMPLATE/`。
