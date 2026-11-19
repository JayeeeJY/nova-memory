# 使用 Nova Memory 本地版

当前 GitHub 发布本地版源码。建议先在 Apple 芯片 Mac 上构建并体验，正式安装包的开发者签名、公证和换机检查继续准备。

## 构建并打开

准备 macOS 14 或更新版本、带 macOS SDK 的 Xcode、Rust 1.97.1 和 Python 3.11 或更新版本。

```sh
git clone https://github.com/JayeeeJY/nova-memory.git
cd nova-memory
cargo fetch --locked
./script/build_and_run.sh
```

入口在新的 `.artifacts/` 目录中构建运行包与桌面 App，然后打开候选。依赖版本由 `Cargo.lock` 固定。已构建的 App 携带本机运行程序。

想使用独立测试库，可以指定一个新的绝对路径和空闲端口。

```sh
./script/build_and_run.sh \
  --profile-dir "/absolute/path/to/new-nova-profile" \
  --port 3060
```

构建需要下载依赖并使用 Xcode 的命令行工具。如果系统找不到编译器，先在 Xcode 设置中确认命令行工具已安装和选定。

## 第一次保存

1. 打开 App，点「开始本地使用」。
2. 在工作台点「留住一段记忆」，粘贴一小段适合保留的材料并保存。
3. 到「AI 整理」查看原文、接收记录与处理状态。
4. 准备好模型后，在运行设置中选择模型、服务地址和处理方式，再开启整理。

未配置模型时，原文会保留并等待整理。模型连接与提取质量需要分别核对，结果可以继续修正。

## 在其他 AI 中使用

工作台「连接」按所选平台提供接入步骤。

- 持续使用时，为目标 AI 选择采集或读取范围，并安装相应 MCP 配置。
- 临时分享时，在记忆库选择需要的记录，预览一次性上下文，再复制或下载到目标工具。
- 已发出的一次性文本由接收方保管。后续版本修改和连接撤销作用于后续 Nova 访问。

ChatGPT、Grok 等远程连接器需要可达的服务地址、认证和相应账号配置。具体方式以当前工作台给出的平台说明为准。

## 原件、版本与修正

在「记忆库」查看项目或主题，展开来源及适用条件。到「AI 整理」告诉 Nova 哪一处不准确或只适用于什么场景，系统保留修正的来源与历史。

模型使用的材料及处理范围由当前设置决定。连接远程服务时，相应内容会发送至所配置的服务。

## 数据与备份

默认数据目录为 `~/Library/Application Support/Nova Memory/local-workspace-v03`。停止服务后，材料和配置继续保存在该目录中。关闭 App 窗口后服务可能继续运行，需要结束时使用 App 的「停止」。

工作台的设置与数据区域可以管理备份。加密备份检查需要备份文件和配套的 Vault 密钥；从桌面「运行设置 → 加密备份」选择文件，核对并打开独立副本。

检查副本保持采集、自动整理与 AI 连接暂停。重新启用前核对备份之后的删除和撤权状态。

## 开发验证

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*package.py'
python3 -m unittest discover -s scripts/tests -p 'test_desktop_local_signing.py'
```

脚本的 `--help` 可查看各自的打包参数。

```sh
python3 scripts/package-local.py --help
python3 scripts/package-desktop.py --help
python3 scripts/sign-desktop-local.py --help
```

参与改进前阅读 [CONTRIBUTING.md](../CONTRIBUTING.md)。反馈中提供版本、系统、目标 AI 客户端、最小合成复现步骤与预期结果。
