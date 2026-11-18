import AppKit
import WebKit

/// A WebKit download is permitted to write only after the owner selects a path.
/// The page never supplies a native filesystem destination.
@MainActor
final class WebDownloadSupport: NSObject, WKDownloadDelegate {
    private let download: WKDownload
    private weak var window: NSWindow?
    private let isCurrent: @MainActor () -> Bool
    private let onStatus: @MainActor (String) -> Void
    private let onFinish: @MainActor () -> Void
    private var panel: NSSavePanel?
    private var pendingDestination: (@MainActor @Sendable (URL?) -> Void)?
    private var finished = false

    init(download: WKDownload, window: NSWindow,
         isCurrent: @escaping @MainActor () -> Bool,
         onStatus: @escaping @MainActor (String) -> Void,
         onFinish: @escaping @MainActor () -> Void) {
        self.download = download
        self.window = window
        self.isCurrent = isCurrent
        self.onStatus = onStatus
        self.onFinish = onFinish
    }

    func invalidate() {
        guard !finished else { return }
        completeDestination(nil)
        panel?.cancel(nil)
        panel = nil
        download.cancel { _ in }
        finish()
    }

    func download(_ download: WKDownload, decideDestinationUsing response: URLResponse,
                  suggestedFilename: String, completionHandler: @escaping @MainActor @Sendable (URL?) -> Void) {
        guard !finished, isCurrent(), let window else {
            completionHandler(nil)
            finish()
            return
        }
        let panel = NSSavePanel()
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        panel.nameFieldStringValue = Self.safeFilename(suggestedFilename)
        panel.title = "保存 Nova Memory 文件"
        self.panel = panel
        pendingDestination = completionHandler
        panel.beginSheetModal(for: window) { [weak self] result in
            guard let self else { return }
            let destination = result == .OK && self.isCurrent() ? panel.url : nil
            self.panel = nil
            self.completeDestination(destination)
            if destination == nil {
                if self.isCurrent() { self.onStatus("已取消本次文件保存。") }
                self.finish()
            }
        }
    }

    func downloadDidFinish(_ download: WKDownload) {
        if isCurrent() { onStatus("文件已保存到你选择的位置。") }
        finish()
    }

    func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
        if !finished && isCurrent() { onStatus("文件保存未完成。") }
        // Never retain a resumable request or write a partial file independently.
        finish()
    }

    private func completeDestination(_ url: URL?) {
        let completion = pendingDestination
        pendingDestination = nil
        completion?(url)
    }

    private func finish() {
        guard !finished else { return }
        finished = true
        completeDestination(nil)
        download.delegate = nil
        onFinish()
    }

    private static func safeFilename(_ suggestion: String) -> String {
        let leaf = (suggestion.replacingOccurrences(of: "\\", with: "/") as NSString).lastPathComponent
        let cleaned = String(leaf.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }.prefix(128))
        return cleaned.isEmpty || cleaned == "." || cleaned == ".." ? "nova-memory-download" : cleaned
    }
}
