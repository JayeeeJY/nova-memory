import SwiftUI

struct RuntimeSettingsView: View {
    @ObservedObject var store: DesktopStore
    @StateObject private var model: ModelSetupStore

    init(store: DesktopStore) {
        self.store = store
        _model = StateObject(wrappedValue: ModelSetupStore(desktop: store))
    }

    var body: some View {
        if store.isBackupInspection {
            Text("此窗口用于查看独立备份副本。AI 连接与自动整理保持关闭。")
                .padding(24)
        } else {
            settingsTabs
        }
    }

    private var settingsTabs: some View {
        TabView {
            ScrollView {
                ModelSetupView(desktop: store, model: model)
                    .padding(24).frame(maxWidth: .infinity, alignment: .leading)
            }
            .tabItem { Label("模型与检查", systemImage: "sparkles") }
            ScrollView {
                RuntimeControlsView(store: store)
                    .padding(24).frame(maxWidth: .infinity, alignment: .leading)
            }
            .tabItem { Label("运行与存储", systemImage: "externaldrive") }
            BackupInspectionView(store: store)
                .padding(24).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .tabItem { Label("加密备份", systemImage: "lock.doc") }
        }
        .task { await store.loadSettings(); await model.load() }
        .onChange(of: store.isRunning) { _, running in
            if running && !store.settingsLoaded { Task { await store.loadSettings() } }
            if running && model.state == nil { Task { await model.load() } }
        }
    }
}

private struct RuntimeControlsView: View {
    @ObservedObject var store: DesktopStore

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("本机运行设置").font(.title2.bold())
            Toggle("开启任务接续与启动简报", isOn: $store.desiredContinuity)
                .disabled(!store.settingsLoaded || store.isBusy)
                .accessibilityIdentifier("continuity-enabled")
            Text("让 AI 在新会话中读取获准的任务上下文。开启本身不调用模型、不自动采集，也不扩大已有连接范围。")
                .font(.caption).foregroundStyle(.secondary)
            Text(store.isRunning
                 ? (store.snapshot?.continuityAvailable == true ? "当前服务：任务接续已开启" : "当前服务：任务接续已关闭")
                 : "服务尚未就绪，保存并启动后核对实际状态。")
                .font(.caption).foregroundStyle(.secondary)
                .accessibilityIdentifier("continuity-runtime-status")
            Toggle("开启渐进读取（本机 MCP 预览）", isOn: $store.desiredRankedRetrieval)
                .disabled(!store.settingsLoaded || store.isBusy)
                .accessibilityIdentifier("ranked-retrieval-enabled")
            Text("复杂任务可先查索引，再展开指定版本的详情或查看同主题时间邻域。只在已有连接授权范围内读取。")
                .font(.caption).foregroundStyle(.secondary)
            Text(store.isRunning
                 ? (store.snapshot?.rankedRetrievalAvailable == true ? "当前服务：渐进读取已开启" : "当前服务：渐进读取已关闭")
                 : "服务尚未就绪，保存并启动后核对实际状态。")
                .font(.caption).foregroundStyle(.secondary)
                .accessibilityIdentifier("ranked-retrieval-runtime-status")
            Text("下方保存按钮会先停止此记忆库的服务，再保存设置并重新启动。已有记忆、授权范围和到期时间保持不变。")
                .font(.caption).foregroundStyle(.secondary)
            Divider()
            Text("模型选择保存后，整理保持暂停。准备好让 Nova 处理已保存材料时，在这里开启。")
                .foregroundStyle(.secondary)
            Toggle("开启自动整理处理", isOn: $store.desiredCompiler)
                .disabled(!store.settingsLoaded || store.isBusy)
                .accessibilityIdentifier("compiler-enabled")
            Text("开启后会按已保存的模型配置处理队列中的材料。模型未配置时，材料继续等待。")
                .font(.caption).foregroundStyle(.secondary)
            DisclosureGroup("调用限额") {
                Stepper(value: $store.desiredMaximumCalls, in: 1...10000, step: 1) {
                    Text("最多 \(store.desiredMaximumCalls) 次整理调用")
                }.disabled(!store.settingsLoaded || store.isBusy)
                Text("这是持续累计的调用上限，不是 token 或金额预算。重启不会重置已用次数。")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Button("保存并重新启动") { Task { await store.applySettings() } }
                .buttonStyle(.borderedProminent)
                .disabled(!store.settingsLoaded || store.isBusy)
                .accessibilityIdentifier("apply-runtime-settings")
            Divider()
            if let directory = store.configuration?.profileDirectory {
                Text("记忆库位置").font(.headline)
                Text(directory.path).font(.caption.monospaced()).textSelection(.enabled)
                Button("在访达中显示") { store.revealProfile() }.disabled(!store.profileExists)
            }
            DisclosureGroup("运行异常时") {
                Text("清理只处理已经停止、端口空闲的遗留运行状态，不删除材料或凭据，也不会结束其他进程。")
                    .font(.caption).foregroundStyle(.secondary)
                Button("清理遗留状态") { Task { await store.recover() } }
                    .disabled(store.isBusy || !store.profileExists || store.isRunning)
                    .accessibilityIdentifier("recover-local-state")
            }
            if !store.message.isEmpty { Text(store.message).font(.caption).foregroundStyle(.secondary) }
            Text("关闭窗口不等于停止服务；需要停止时使用主窗口工具栏的“停止”。")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}
