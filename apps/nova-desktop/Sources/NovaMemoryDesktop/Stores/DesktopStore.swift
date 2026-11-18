import AppKit
import Foundation
import SwiftUI
import NovaDesktopCore

@MainActor
final class DesktopStore: ObservableObject {
    enum Phase { case firstUse, stopped, busy, running, unverified, failed }
    let configuration: DesktopConfiguration?
    private let client: RuntimeClient?
    @Published private(set) var phase: Phase = .firstUse
    @Published private(set) var snapshot: RuntimeSnapshot?
    @Published private(set) var isBusy = false
    @Published private(set) var message = ""
    @Published var webStatus = ""
    @Published private(set) var workspaceRevision = UUID()
    @Published var desiredCompiler = false
    @Published var desiredContinuity = false
    @Published var desiredRankedRetrieval = false
    @Published var desiredMaximumCalls = 100
    @Published private(set) var settingsLoaded = false
    private var polling: Task<Void, Never>?
    private var isRefreshing = false
    private var operationGeneration: UInt64 = 0

    init() {
        do {
            let config = try DesktopConfiguration.parse(
                arguments: Array(CommandLine.arguments.dropFirst()),
                resourceURL: Bundle.main.resourceURL,
                homeDirectory: FileManager.default.homeDirectoryForCurrentUser)
            configuration = config
            client = RuntimeClient(configuration: config)
            phase = FileManager.default.fileExists(atPath: config.profileDirectory.path) ? .stopped : .firstUse
        } catch {
            configuration = nil
            client = nil
            phase = .failed
            message = Self.safeMessage(error)
        }
    }

    var profileExists: Bool {
        configuration.map { FileManager.default.fileExists(atPath: $0.profileDirectory.path) } ?? false
    }
    var runtimeClient: RuntimeClient? { client }
    var isBackupInspection: Bool { configuration?.backupInspection == true }
    var isRunning: Bool { phase == .running && snapshot?.ready == true }
    var workbenchURL: URL? { isRunning ? snapshot?.workbenchURL : nil }
    var title: String {
        switch phase {
        case .firstUse: return "开始你的本地记忆库"
        case .stopped: return "本机服务已停止"
        case .busy: return "正在处理本机服务…"
        case .running: return "本机工作台已就绪"
        case .unverified: return "运行状态需要检查"
        case .failed: return "本次操作未完成"
        }
    }
    var processingLabel: String {
        if isBackupInspection { return "加密备份检查 · AI 连接与整理关闭" }
        guard isRunning else { return "材料保存在本机" }
        if snapshot?.modelSelection == "unconfigured" { return "模型未配置 · 材料可先保存" }
        return snapshot?.compilerAvailable == true ? "整理处理已开启" : "整理处理已暂停"
    }

    func startPolling() {
        guard polling == nil, client != nil else { return }
        polling = Task { [weak self] in
            await self?.refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                if Task.isCancelled { break }
                await self?.refresh()
            }
        }
    }

    func refresh() async {
        guard !isBusy, !isRefreshing, let client else { return }
        guard profileExists else {
            snapshot = nil; phase = .firstUse; message = ""; return
        }
        let generation = operationGeneration
        isRefreshing = true
        defer { isRefreshing = false }
        do {
            let current = try await client.perform(action: .status)
            guard !isBusy, generation == operationGeneration else { return }
            apply(current)
        } catch {
            guard !isBusy, generation == operationGeneration else { return }
            fail(error)
        }
    }

    func start() async {
        guard !isBusy, let client else { return }
        operationGeneration &+= 1
        isBusy = true; phase = .busy; snapshot = nil; webStatus = ""; message = ""
        defer { isBusy = false }
        do { apply(try await client.perform(action: isBackupInspection ? .start : .launch)) }
        catch { fail(error) }
    }

    func stop() async {
        guard !isBusy, let client, profileExists else { return }
        operationGeneration &+= 1
        isBusy = true; phase = .busy; snapshot = nil; webStatus = ""; message = ""
        defer { isBusy = false }
        do { apply(try await client.perform(action: .stop)) }
        catch { fail(error) }
    }

    func loadSettings() async {
        // A settings window may open while the initial status probe is running.
        // Wait for it instead of leaving the editor permanently disabled.
        while isBusy {
            do { try await Task.sleep(nanoseconds: 100_000_000) }
            catch { return }
        }
        guard !Task.isCancelled, let client, profileExists else { settingsLoaded = false; return }
        do {
            let settings = try await client.readSettings()
            desiredCompiler = settings.compilerEnabled
            desiredContinuity = settings.continuityEnabled
            desiredRankedRetrieval = settings.rankedRetrievalEnabled
            desiredMaximumCalls = settings.maximumCalls
            settingsLoaded = true
        } catch { settingsLoaded = false; message = Self.safeMessage(error) }
    }

    func applySettings() async {
        guard !isBusy, let client, settingsLoaded, (1...10000).contains(desiredMaximumCalls) else { return }
        operationGeneration &+= 1
        let enabled = desiredCompiler, limit = desiredMaximumCalls, continuity = desiredContinuity
        let rankedRetrieval = desiredRankedRetrieval
        isBusy = true; phase = .busy; snapshot = nil; webStatus = ""; message = ""
        defer { isBusy = false }
        do {
            _ = try await client.perform(action: .stop)
            _ = try await client.perform(action: enabled ? .configureOn : .configureOff, maxCalls: limit,
                                         continuityEnabled: continuity, rankedRetrievalEnabled: rankedRetrieval)
            let saved = try await client.readSettings()
            guard saved.compilerEnabled == enabled, saved.maximumCalls == limit,
                  saved.continuityEnabled == continuity, saved.rankedRetrievalEnabled == rankedRetrieval else { throw DesktopError.runtimeNotReady }
            let running = try await client.perform(action: .launch)
            guard (running.continuityAvailable ?? false) == continuity,
                  (running.rankedRetrievalAvailable ?? false) == rankedRetrieval else { throw DesktopError.runtimeNotReady }
            apply(running)
        } catch { fail(error) }
    }

    func recover() async {
        guard !isBusy, let client, profileExists else { return }
        operationGeneration &+= 1
        isBusy = true; phase = .busy; snapshot = nil; webStatus = ""
        defer { isBusy = false }
        do { apply(try await client.perform(action: .recover)) }
        catch { fail(error) }
    }

    /// An explicit model save always pauses processing before changing selection.
    /// Keep the old page detached until the compare-and-swap has completed.
    func saveModelPaused(_ selection: WorkspaceModelConfiguration, expectedRevision: UInt64) async throws -> WorkspaceModelConfiguration {
        try selection.validateForWrite()
        guard !isBusy, let client, profileExists else { throw DesktopError.runtimeNotReady }
        operationGeneration &+= 1
        isBusy = true; phase = .busy; snapshot = nil; webStatus = ""; message = ""
        defer { isBusy = false }
        do {
            _ = try await client.perform(action: .stop)
            _ = try await client.perform(action: .configureOff)
            desiredCompiler = false
            _ = try await client.perform(action: .launch)
            let saved = try await WorkspaceModelClient(runtime: client).savePaused(selection: selection, expectedRevision: expectedRevision)
            apply(try await client.perform(action: .status))
            return saved
        } catch {
            // A stale model revision is not a failed runtime. Recheck it without
            // retrying the write or restarting processing.
            if let current = try? await client.perform(action: .status) { apply(current) }
            else { fail(error) }
            throw error
        }
    }

    func credential() async throws -> String {
        guard let client, isRunning else { throw DesktopError.runtimeNotReady }
        return try await client.readOwnerCredential()
    }

    func revealProfile() {
        guard let url = configuration?.profileDirectory, profileExists else { return }
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: url.path)
    }

    func openBackupInspection(snapshot: URL, key: URL) async throws {
        guard let configuration, let client, !isBackupInspection else { throw DesktopError.runtimeUnavailable }
        let parent = configuration.profileDirectory.deletingLastPathComponent()
        let destination = parent.appendingPathComponent("backup-inspection-" + UUID().uuidString, isDirectory: true)
        if !FileManager.default.fileExists(atPath: parent.path) {
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
        }
        let port = try await client.prepareBackupInspection(snapshot: snapshot, key: key, destination: destination)
        try await launchBackupWindow(directory: destination, port: port, bundle: configuration.runtimeBundle)
    }

    func reopenBackupInspection(directory: URL) async throws {
        guard let configuration, let client, !isBackupInspection else { throw DesktopError.runtimeUnavailable }
        let port = try await client.checkBackupInspection(directory: directory)
        try await launchBackupWindow(directory: directory, port: port, bundle: configuration.runtimeBundle)
    }

    private func launchBackupWindow(directory: URL, port: Int, bundle: URL) async throws {
        let launch = NSWorkspace.OpenConfiguration()
        launch.createsNewApplicationInstance = true
        launch.arguments = ["--runtime-bundle", bundle.path, "--profile-dir", directory.path,
                            "--port", String(port), "--backup-inspection"]
        _ = try await NSWorkspace.shared.openApplication(at: Bundle.main.bundleURL, configuration: launch)
    }

    func reopenWorkspace() {
        guard isRunning, !isBusy else { return }
        webStatus = ""
        workspaceRevision = UUID()
    }

    private func apply(_ result: RuntimeSnapshot) {
        if isBackupInspection, result.status == "running", result.backupQuarantined != true {
            fail(DesktopError.invalidBackup)
            return
        }
        if result.instanceID != snapshot?.instanceID { webStatus = "" }
        snapshot = result
        switch result.status {
        case "running" where result.ready == true && result.workbenchURL != nil:
            phase = .running; message = ""
        case "stopped": phase = .stopped; message = "记忆和配置仍保存在本机。"
        default: phase = .unverified; message = "暂时无法核对本机服务。可以检查状态；遗留状态仅在确认服务停止后清理。"
        }
    }

    private func fail(_ error: Error) {
        snapshot = nil; phase = .failed; webStatus = ""; message = Self.safeMessage(error)
    }

    private static func safeMessage(_ error: Error) -> String {
        (error as? DesktopError)?.localizedDescription ?? "未能完成本机操作，请检查运行状态。记忆不会因本次失败而删除。"
    }
}
