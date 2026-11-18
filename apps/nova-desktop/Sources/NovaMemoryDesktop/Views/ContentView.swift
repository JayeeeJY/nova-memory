import AppKit
import SwiftUI
import NovaDesktopCore

struct ContentView: View {
    @ObservedObject var store: DesktopStore

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Circle().fill(store.isRunning ? Color.green : Color.secondary.opacity(0.5)).frame(width: 8, height: 8)
                Text(store.title).font(.callout.weight(.medium))
                Text(store.processingLabel).font(.caption).foregroundStyle(.secondary)
                Spacer()
                if store.isBusy { ProgressView().controlSize(.small) }
            }
            .padding(.horizontal, 20).padding(.vertical, 10)
            Divider()
            if let url = store.workbenchURL {
                WorkspaceWebView(baseURL: url, credentialProvider: { try await store.credential() }, onStatus: { store.webStatus = $0 })
                    .id((store.snapshot?.instanceID ?? url.absoluteString) + store.workspaceRevision.uuidString)
                    .accessibilityIdentifier("native-workspace")
            } else {
                welcome
            }
            Divider()
            HStack {
                Text(store.isRunning && !store.webStatus.isEmpty ? store.webStatus : "关闭应用后服务继续运行；可使用“停止”结束本机服务。")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                Spacer()
                Text("本机加密存储").font(.caption).foregroundStyle(.secondary)
            }.padding(.horizontal, 20).padding(.vertical, 8)
        }
        .toolbar {
            ToolbarItemGroup {
                if store.isRunning {
                    Button { Task { await store.stop() } } label: { Label("停止", systemImage: "stop.circle") }
                        .disabled(store.isBusy)
                        .help("停止选定记忆库的本机服务，保留数据")
                        .accessibilityIdentifier("stop-local-service")
                } else {
                    Button { Task { await store.start() } } label: { Label("启动", systemImage: "play.circle") }
                        .disabled(store.isBusy || store.configuration == nil)
                        .accessibilityIdentifier("start-local-service")
                }
                Button { Task { await store.refresh() } } label: { Label("检查状态", systemImage: "arrow.clockwise") }
                    .disabled(store.isBusy)
                    .accessibilityIdentifier("refresh-local-status")
                if store.isRunning {
                    Button { store.reopenWorkspace() } label: { Label("重新打开工作台", systemImage: "arrow.triangle.2.circlepath") }
                        .disabled(store.isBusy)
                        .accessibilityIdentifier("reopen-workspace")
                }
                if !store.isBackupInspection {
                    SettingsLink { Label("运行设置", systemImage: "gearshape") }
                        .accessibilityIdentifier("open-runtime-settings")
                } else {
                    Button { store.revealProfile() } label: { Label("显示副本目录", systemImage: "folder") }
                        .disabled(!store.profileExists || store.isBusy)
                        .accessibilityIdentifier("reveal-inspection-profile")
                }
            }
        }
    }

    private var welcome: some View {
        VStack(alignment: .leading, spacing: 20) {
            if let url = Bundle.main.url(forResource: "NovaLogo", withExtension: "png"), let image = NSImage(contentsOf: url) {
                Image(nsImage: image).resizable().scaledToFit().frame(width: 64, height: 64).accessibilityHidden(true)
            }
            Text(store.isBackupInspection ? "检查加密备份" : store.phase == .firstUse ? "让理解积累，让协作延续。" : store.title)
                .font(.largeTitle.bold())
            Text(store.isBackupInspection
                 ? "这个独立副本保留备份当时的材料，AI 连接与自动整理关闭。"
                 : "选取一段内容，一键保存。Nova 留住来源、组织记忆，让获准的 AI 接得上此前的协作。")
                .font(.title3).foregroundStyle(.secondary)
            if !store.isBackupInspection {
                VStack(alignment: .leading, spacing: 12) {
                    Label("在独立的本地库中保存，由你掌握密钥", systemImage: "externaldrive.badge.checkmark")
                    Label("模型可以稍后配置，材料先保存、等待整理", systemImage: "tray.and.arrow.down")
                    Label("在“连接”领取配置，范围与撤销由你决定", systemImage: "link")
                }.font(.body)
            }
            if !store.message.isEmpty { Text(store.message).foregroundStyle(.secondary).accessibilityIdentifier("runtime-message") }
            HStack(spacing: 12) {
                Button(store.isBackupInspection ? "查看备份副本" : store.phase == .firstUse ? "开始本地使用" : "启动工作台") {
                    Task { await store.start() }
                }
                    .buttonStyle(.borderedProminent).controlSize(.large)
                    .disabled(store.isBusy || store.configuration == nil)
                    .accessibilityIdentifier("start-workspace")
                if store.profileExists {
                    Button("检查状态") { Task { await store.refresh() } }.disabled(store.isBusy)
                }
            }
            if !store.isBackupInspection {
                Text("随包提供本机运行组件，无需安装 Python、Rust 或 Node。不会自动导入旧库、启动模型或上传材料。")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: 700, alignment: .leading)
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }
}
