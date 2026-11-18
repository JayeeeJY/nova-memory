import SwiftUI
import NovaDesktopCore

struct ModelSetupView: View {
    @ObservedObject var desktop: DesktopStore
    @ObservedObject var model: ModelSetupStore

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("让 Nova 开始整理").font(.title2.bold())
            Text("材料可以先保存。准备好模型服务后，保存模型选择，再开启整理。")
                .foregroundStyle(.secondary)
            Label(model.processingSummary, systemImage: desktop.isRunning ? "checkmark.circle" : "pause.circle")
                .font(.callout).accessibilityIdentifier("model-processing-summary")
            if !desktop.isRunning {
                Button("启动本机工作台") { Task { await desktop.start(); await model.load() } }
                    .disabled(desktop.isBusy || desktop.configuration == nil)
                    .accessibilityIdentifier("start-for-model-setup")
            }
            HStack {
                Text("1 · 选择整理模型").font(.headline)
                Spacer()
                Button("重新读取配置") { Task { await model.load() } }
                    .help("替换本页尚未保存的模型输入")
                    .disabled(!desktop.isRunning || desktop.isBusy || model.isWorking)
                    .accessibilityIdentifier("reload-model-configuration")
            }
            Picker("整理方式", selection: $model.location) {
                Text("稍后配置").tag(ModelLocation.unconfigured)
                Text("本机模型").tag(ModelLocation.local)
                if model.state?.configuration.location == .remote {
                    Text("已有远程模型").tag(ModelLocation.remote)
                }
            }
            .disabled(model.state == nil || model.isWorking || desktop.isBusy)
            .accessibilityIdentifier("model-location")

            if model.location == .local {
                TextField("服务地址", text: $model.endpointText, prompt: Text("http://127.0.0.1:11434/v1"))
                    .textFieldStyle(.roundedBorder).disabled(model.isWorking || desktop.isBusy)
                    .accessibilityIdentifier("model-endpoint")
                TextField("模型名称", text: $model.modelID, prompt: Text("模型服务中的模型 ID"))
                    .textFieldStyle(.roundedBorder).disabled(model.isWorking || desktop.isBusy)
                    .accessibilityIdentifier("model-id")
                HStack {
                    Button(model.isChecking ? "正在检查…" : "检查模型服务") { Task { await model.checkService() } }
                        .disabled(model.isWorking || desktop.isBusy)
                        .accessibilityIdentifier("check-model-service")
                    if model.isChecking { ProgressView().controlSize(.small) }
                    if let ids = model.probe?.modelIDs, !ids.isEmpty {
                        Menu("从检查结果选择") {
                            ForEach(ids, id: \.self) { id in Button(id) { model.modelID = id } }
                        }.accessibilityIdentifier("choose-discovered-model")
                    }
                }
                Text("只接受本机回环地址。检查仅读取模型目录，不发送记忆或调用生成。部分服务没有目录接口，仍可手工填写。")
                    .font(.caption).foregroundStyle(.secondary)
                if let result = model.probe {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(result.title).font(.headline)
                        Text(result.detail).font(.callout).foregroundStyle(.secondary)
                        Text("检查时间：\(result.checkedAt.formatted(date: .omitted, time: .standard))")
                            .font(.caption).foregroundStyle(.secondary)
                    }.accessibilityIdentifier("model-probe-result")
                }
            } else if model.location == .remote {
                Text("已有远程模型配置继续保留。远程地址、发送范围与独立凭据在工作台和运行环境中管理；这里不检查远程服务。")
                    .font(.callout).foregroundStyle(.secondary)
            } else {
                Text("保持稍后配置时，原文仍可加密保存，等待你准备好模型。")
                    .font(.callout).foregroundStyle(.secondary)
            }

            Divider()
            Text("2 · 保存选择，整理先暂停").font(.headline)
            Text("保存会暂停当前整理并重新打开本机服务。材料保留，不会因保存模型选择而开始处理。")
                .font(.callout).foregroundStyle(.secondary)
            Button(model.isSaving ? "正在保存…" : "保存模型并暂停整理") { Task { await model.save() } }
                .buttonStyle(.borderedProminent).disabled(!model.canSave)
                .accessibilityIdentifier("save-model-paused")
            if !model.message.isEmpty {
                Text(model.message).font(.callout).textSelection(.enabled)
                    .accessibilityIdentifier("model-setup-message")
            }
            Divider()
            Text("3 · 准备好后开启处理").font(.headline)
            Text("在“运行与存储”中开启自动整理并保存。目录可访问，只说明服务连接情况；实际整理、语义质量与 AI 使用效果仍需分别验证。")
                .font(.callout).foregroundStyle(.secondary)
        }
    }
}
