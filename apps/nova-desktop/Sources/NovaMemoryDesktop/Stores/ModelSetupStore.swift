import Foundation
import SwiftUI
import NovaDesktopCore

@MainActor
final class ModelSetupStore: ObservableObject {
    let desktop: DesktopStore
    @Published private(set) var state: WorkspaceModelState?
    @Published var location: ModelLocation = .unconfigured {
        didSet { probe = nil }
    }
    @Published var endpointText = "http://127.0.0.1:11434/v1" {
        didSet { probe = nil }
    }
    @Published var modelID = "" {
        didSet { probe = nil }
    }
    @Published private(set) var probe: ModelProbeResult?
    @Published private(set) var isLoading = false
    @Published private(set) var isSaving = false
    @Published private(set) var isChecking = false
    @Published private(set) var message = ""

    init(desktop: DesktopStore) { self.desktop = desktop }
    var isWorking: Bool { isLoading || isSaving || isChecking }
    var canSave: Bool { state != nil && location != .remote && desktop.isRunning && !desktop.isBusy && !isWorking }

    var processingSummary: String {
        guard desktop.isRunning else { return "本机服务未就绪。材料保留在数据目录中。" }
        guard let state else { return "尚未读取模型配置。" }
        if state.configuration.location == .unconfigured { return "模型尚未配置，材料可以先保存、等待整理。" }
        if desktop.snapshot?.compilerAvailable != true { return "模型选择已保存，整理处理已暂停。需要时在“运行与存储”中开启。" }
        return "整理处理已开启。服务目录检查与实际整理结果分别验证。"
    }

    func load() async {
        guard !isWorking else { return }
        isLoading = true
        defer { isLoading = false }
        while desktop.isBusy {
            do { try await Task.sleep(nanoseconds: 100_000_000) }
            catch { return }
        }
        guard !Task.isCancelled, desktop.isRunning, let runtime = desktop.runtimeClient else {
            state = nil; message = "请先启动本机工作台，再读取模型配置。"; return
        }
        do {
            let loaded = try await WorkspaceModelClient(runtime: runtime).load()
            accept(loaded)
            message = ""
        } catch { state = nil; message = Self.safeMessage(error) }
    }

    func checkService() async {
        guard !isWorking, location == .local else { return }
        let enteredEndpoint = endpointText, enteredModel = modelID
        let endpoint: LocalModelEndpoint
        do { endpoint = try LocalModelEndpoint(enteredEndpoint) }
        catch { message = Self.safeMessage(error); probe = nil; return }
        isChecking = true; message = ""
        defer { isChecking = false }
        let result = await LocalModelProbe().check(endpoint: endpoint, selectedModelID: enteredModel.isEmpty ? nil : enteredModel)
        guard endpointText == enteredEndpoint, modelID == enteredModel, location == .local else { return }
        if modelID.isEmpty, let first = result.modelIDs.first { modelID = first }
        probe = result
    }

    func save() async {
        guard canSave, let expected = state?.configuration.revision else { return }
        let selection = WorkspaceModelConfiguration(revision: expected, location: location,
            modelID: location == .local ? modelID : nil,
            endpoint: location == .local ? endpointText : nil)
        do { try selection.validateForWrite() }
        catch { message = Self.safeMessage(error); return }
        isSaving = true; message = "正在暂停整理并保存模型选择…"
        defer { isSaving = false }
        do {
            let saved = try await desktop.saveModelPaused(selection, expectedRevision: expected)
            state = WorkspaceModelState(configuration: saved, compilerAvailable: false)
            location = saved.location
            message = "模型选择已保存。本次操作未开启整理；需要时在“运行与存储”中开启。"
        } catch { message = Self.safeMessage(error) }
    }

    private func accept(_ loaded: WorkspaceModelState) {
        state = loaded
        location = loaded.configuration.location
        if loaded.configuration.location == .local {
            endpointText = loaded.configuration.endpoint ?? "http://127.0.0.1:11434/v1"
            modelID = loaded.configuration.modelID ?? ""
        } else {
            endpointText = "http://127.0.0.1:11434/v1"
            modelID = ""
        }
        probe = nil
    }

    private static func safeMessage(_ error: Error) -> String {
        if let error = error as? ModelSetupError { return error.localizedDescription }
        if let error = error as? LocalModelEndpointError { return error.localizedDescription }
        if let error = error as? DesktopError { return error.localizedDescription }
        return "本次检查或保存未完成。请重新读取配置，材料仍保留。"
    }
}
