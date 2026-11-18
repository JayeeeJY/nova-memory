import NovaDesktopCore
import SwiftUI
import UniformTypeIdentifiers

struct BackupInspectionView: View {
    @ObservedObject var store: DesktopStore
    @State private var snapshot: URL?
    @State private var key: URL?
    @State private var choosingSnapshot = false
    @State private var choosingKey = false
    @State private var choosingExisting = false
    @State private var busy = false
    @State private var status = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("检查加密备份").font(.title2.bold())
            Text("选择 Nova 导出的加密备份和配套的 Vault 密钥。核对通过后，在新窗口点「查看备份副本」。")
            Text("副本反映备份当时的状态。请留意备份之后的删除和撤权。副本的 AI 连接与自动整理保持关闭。")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("选择加密备份…") { choosingSnapshot = true }
                    .accessibilityIdentifier("choose-backup-snapshot")
                    .fileImporter(isPresented: $choosingSnapshot, allowedContentTypes: [.data]) {
                        select($0, into: $snapshot)
                    }
                Text(snapshot?.lastPathComponent ?? "尚未选择").lineLimit(1)
            }
            HStack {
                Button("选择 Vault 密钥文件…") { choosingKey = true }
                    .accessibilityIdentifier("choose-backup-key")
                    .fileImporter(isPresented: $choosingKey, allowedContentTypes: [.data]) {
                        select($0, into: $key)
                    }
                Text(key?.lastPathComponent ?? "尚未选择").lineLimit(1)
            }
            Button("核对并打开副本") { Task { await open() } }
                .buttonStyle(.borderedProminent)
                .disabled(snapshot == nil || key == nil || store.configuration == nil)
                .accessibilityIdentifier("open-backup-inspection")
            Divider()
            Button("打开已有检查副本…") { choosingExisting = true }
                .disabled(store.configuration == nil)
                .accessibilityIdentifier("reopen-backup-inspection")
                .fileImporter(isPresented: $choosingExisting, allowedContentTypes: [.folder]) { result in
                    if case .success(let directory) = result {
                        Task { await reopen(directory) }
                    }
                }
            if busy { ProgressView().controlSize(.small) }
            if !status.isEmpty { Text(status).font(.caption).accessibilityIdentifier("backup-inspection-status") }
        }
        .disabled(busy)
    }

    private func select(_ result: Result<URL, Error>, into selection: Binding<URL?>) {
        if case .success(let url) = result {
            selection.wrappedValue = url
            status = ""
        }
    }

    private func open() async {
        guard let snapshot, let key else { return }
        let accessedSnapshot = snapshot.startAccessingSecurityScopedResource()
        let accessedKey = key.startAccessingSecurityScopedResource()
        defer {
            if accessedSnapshot { snapshot.stopAccessingSecurityScopedResource() }
            if accessedKey { key.stopAccessingSecurityScopedResource() }
        }
        await runInspection("正在本机核对加密备份…") {
            try await store.openBackupInspection(snapshot: snapshot, key: key)
            self.snapshot = nil
            self.key = nil
        }
    }

    private func reopen(_ directory: URL) async {
        let accessed = directory.startAccessingSecurityScopedResource()
        defer { if accessed { directory.stopAccessingSecurityScopedResource() } }
        await runInspection("正在核对检查副本…") {
            try await store.reopenBackupInspection(directory: directory)
        }
    }

    private func runInspection(_ pendingMessage: String, operation: () async throws -> Void) async {
        guard !busy else { return }
        busy = true
        status = pendingMessage
        defer { busy = false }
        do {
            try await operation()
            status = "核对通过，已打开独立备份窗口。"
        } catch let error as DesktopError {
            status = error.localizedDescription
        } catch {
            status = "无法打开备份窗口，请检查本机文件访问权限后重试。"
        }
    }
}
