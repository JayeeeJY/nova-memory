import AppKit
import NovaDesktopCore
import SwiftUI
import UniformTypeIdentifiers
import WebKit

/// The workspace remains a web application. This wrapper only confines navigation
/// and supplies the verified local Owner credential to its existing unlock form.
@MainActor
struct WorkspaceWebView: NSViewRepresentable {
    let baseURL: URL
    let credentialProvider: @MainActor () async throws -> String
    let onStatus: @MainActor (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.userContentController.addUserScript(WKUserScript(
            source: Self.documentIdentityScript,
            injectionTime: .atDocumentStart,
            forMainFrameOnly: true,
            in: Coordinator.bridgeWorld
        ))
        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.allowsBackForwardNavigationGestures = false
        webView.allowsLinkPreview = false
        webView.navigationDelegate = context.coordinator
        webView.uiDelegate = context.coordinator
        context.coordinator.configure(webView, baseURL: baseURL,
                                      credentialProvider: credentialProvider, onStatus: onStatus)
        return webView
    }

    func updateNSView(_ webView: WKWebView, context: Context) {
        context.coordinator.configure(webView, baseURL: baseURL,
                                      credentialProvider: credentialProvider, onStatus: onStatus)
    }

    static func dismantleNSView(_ webView: WKWebView, coordinator: Coordinator) {
        coordinator.invalidate(webView)
    }

    private static let documentIdentityScript = """
    (() => {
      const bytes = new Uint32Array(8);
      crypto.getRandomValues(bytes);
      Object.defineProperty(globalThis, '__novaDesktopDocumentIdentity', {
        value: Array.from(bytes, x => x.toString(16).padStart(8, '0')).join(''),
        writable: false, configurable: false
      });
    })();
    """

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate {
        static let bridgeWorld = WKContentWorld.world(name: "NovaMemoryDesktopUnlock")
        private weak var webView: WKWebView?
        private var origin: TrustedWorkspaceOrigin?
        private var configuredURL: URL?
        private var credentialProvider: (@MainActor () async throws -> String)?
        private var onStatus: (@MainActor (String) -> Void)?
        private var generation = UUID()
        private var navigationGeneration = UUID()
        private var unlockTask: Task<Void, Never>?
        private var rulesReady = false
        private var clearingPage = false
        private var downloads: [ObjectIdentifier: WebDownloadSupport] = [:]
        private var openPanel: NSOpenPanel?
        private var alerts: [NSAlert] = []

        func configure(_ webView: WKWebView, baseURL: URL,
                       credentialProvider: @escaping @MainActor () async throws -> String,
                       onStatus: @escaping @MainActor (String) -> Void) {
            self.credentialProvider = credentialProvider
            self.onStatus = onStatus
            guard configuredURL != baseURL else { return }
            clearPage(webView)
            self.webView = webView
            configuredURL = baseURL
            guard let trusted = TrustedWorkspaceOrigin(baseURL: baseURL) else {
                onStatus("本地工作台地址无效。")
                return
            }
            origin = trusted
            let currentGeneration = generation
            let identifier = "NovaMemory-local-origin-" + UUID().uuidString
            let rules = trusted.contentRules
            onStatus("正在准备本地工作台…")
            WKContentRuleListStore.default().compileContentRuleList(
                forIdentifier: identifier, encodedContentRuleList: rules
            ) { [weak self, weak webView] list, error in
                // The compiled list is retained by the view, not as an accumulating cache.
                WKContentRuleListStore.default().removeContentRuleList(forIdentifier: identifier) { _ in }
                Task { @MainActor in
                    guard let self, let webView, self.generation == currentGeneration,
                          self.webView === webView else { return }
                    guard error == nil, let list else {
                        self.onStatus?("无法建立工作台网络边界，请重新打开。")
                        return
                    }
                    // Keep the previous confinement until replacing it synchronously.
                    webView.configuration.userContentController.removeAllContentRuleLists()
                    webView.configuration.userContentController.add(list)
                    self.rulesReady = true
                    self.clearingPage = false
                    webView.load(URLRequest(url: trusted.workspaceURL,
                                            cachePolicy: .reloadIgnoringLocalCacheData))
                }
            }
        }

        func invalidate(_ webView: WKWebView) {
            clearPage(webView)
            webView.navigationDelegate = nil
            webView.uiDelegate = nil
            webView.configuration.userContentController.removeAllUserScripts()
            credentialProvider = nil
            onStatus = nil
            configuredURL = nil
            self.webView = nil
        }

        private func clearPage(_ webView: WKWebView) {
            generation = UUID()
            navigationGeneration = UUID()
            unlockTask?.cancel()
            unlockTask = nil
            for support in Array(downloads.values) { support.invalidate() }
            downloads.removeAll()
            openPanel?.cancel(nil)
            openPanel = nil
            for alert in alerts {
                alert.window.sheetParent?.endSheet(alert.window, returnCode: .abort)
            }
            alerts.removeAll()
            rulesReady = false
            origin = nil
            webView.stopLoading()
            clearingPage = true
            // No credential or page token survives in a retained DOM on removal.
            webView.loadHTMLString("", baseURL: nil)
        }

        func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                     decisionHandler: @escaping @MainActor @Sendable (WKNavigationActionPolicy) -> Void) {
            if clearingPage, navigationAction.request.url?.absoluteString == "about:blank" {
                decisionHandler(.allow)
                return
            }
            if rulesReady, let origin, let url = navigationAction.request.url,
               navigationAction.shouldPerformDownload,
               isTrusted(navigationAction.sourceFrame, in: webView), origin.allowsDownloadURL(url) {
                decisionHandler(.download)
                return
            }
            guard rulesReady, let origin, let url = navigationAction.request.url,
                  navigationAction.targetFrame?.isMainFrame != false,
                  origin.allowsPage(url), !navigationAction.shouldPerformDownload else {
                decisionHandler(.cancel)
                return
            }
            if navigationAction.targetFrame == nil {
                // Trusted target=_blank links remain in this confined view.
                decisionHandler(.cancel)
                webView.load(navigationAction.request)
                return
            }
            decisionHandler(.allow)
        }

        func webView(_ webView: WKWebView, navigationAction: WKNavigationAction,
                     didBecome download: WKDownload) {
            guard let origin, let url = navigationAction.request.url,
                  navigationAction.shouldPerformDownload,
                  isTrusted(navigationAction.sourceFrame, in: webView), origin.allowsDownloadURL(url),
                  let window = webView.window else {
                download.cancel { _ in }
                return
            }
            if #available(macOS 15.2, *) {
                guard download.isUserInitiated, isTrusted(download.originatingFrame, in: webView) else {
                    download.cancel { _ in }
                    return
                }
            }
            let viewGeneration = generation
            let documentGeneration = navigationGeneration
            let key = ObjectIdentifier(download)
            let support = WebDownloadSupport(download: download, window: window, isCurrent: { [weak self, weak webView] in
                guard let self, let webView, self.webView === webView else { return false }
                return self.generation == viewGeneration && self.navigationGeneration == documentGeneration
                    && self.rulesReady && webView.url.map(origin.allowsPage) == true
            }, onStatus: { [weak self] message in self?.onStatus?(message) }, onFinish: { [weak self] in
                self?.downloads.removeValue(forKey: key)
            })
            downloads[key] = support
            download.delegate = support
        }

        func webView(_ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
                     decisionHandler: @escaping @MainActor @Sendable (WKNavigationResponsePolicy) -> Void) {
            if clearingPage, navigationResponse.response.url?.absoluteString == "about:blank" {
                decisionHandler(.allow)
                return
            }
            guard navigationResponse.isForMainFrame, let origin,
                  let url = navigationResponse.response.url, origin.allowsPage(url),
                  navigationResponse.canShowMIMEType else {
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }

        func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
            navigationGeneration = UUID()
            unlockTask?.cancel()
            unlockTask = nil
            for support in Array(downloads.values) { support.invalidate() }
            downloads.removeAll()
            openPanel?.cancel(nil)
            openPanel = nil
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            guard rulesReady else { return }
            guard let origin, let url = webView.url, origin.isUnlockPage(url) else {
                onStatus?("本地运行指南")
                return
            }
            let viewGeneration = generation
            let documentGeneration = navigationGeneration
            unlockTask?.cancel()
            unlockTask = Task { @MainActor [weak self, weak webView] in
                guard let self, let webView else { return }
                await self.unlock(webView, origin: origin, viewGeneration: viewGeneration,
                                  documentGeneration: documentGeneration)
            }
        }

        private func unlock(_ webView: WKWebView, origin: TrustedWorkspaceOrigin,
                            viewGeneration: UUID, documentGeneration: UUID) async {
            func isCurrent() -> Bool {
                !Task.isCancelled && self.generation == viewGeneration
                    && self.navigationGeneration == documentGeneration
                    && self.webView === webView && webView.url.map(origin.isUnlockPage) == true
            }
            do {
                let identityResult = try await webView.callAsyncJavaScript(
                    Self.prepareUnlockScript, arguments: [:], in: nil, contentWorld: Self.bridgeWorld)
                guard isCurrent(), let state = identityResult as? [String: Any],
                      let identity = state["identity"] as? String, !identity.isEmpty else { return }
                if state["ready"] as? Bool == true {
                    onStatus?("工作台已连接")
                    return
                }
                if state["manualUnlock"] as? Bool == true {
                    onStatus?("凭据解锁已开启，请在工作台输入解锁凭据。")
                    return
                }
                guard state["locked"] as? Bool == true, let provider = credentialProvider else {
                    onStatus?("工作台尚未准备好，请重新打开。")
                    return
                }
                // The provider revalidates runtime ownership immediately before reading a key.
                let credential = try await provider()
                guard isCurrent(), !credential.isEmpty else { return }
                let submitted = try await webView.callAsyncJavaScript(
                    Self.submitUnlockScript,
                    arguments: ["credential": credential, "identity": identity],
                    in: nil, contentWorld: Self.bridgeWorld)
                guard isCurrent(), submitted as? Bool == true else { return }
                let opened = try await webView.callAsyncJavaScript(
                    Self.checkUnlockScript, arguments: ["identity": identity],
                    in: nil, contentWorld: Self.bridgeWorld)
                guard isCurrent() else { return }
                onStatus?(opened as? Bool == true ? "工作台已连接" : "工作台未能解锁，请检查本地运行状态。")
            } catch {
                guard isCurrent() else { return }
                // WebKit errors may include page-provided text; keep native status fixed.
                onStatus?("工作台连接未完成，请检查本地运行状态。")
            }
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            reportNavigationFailure(error)
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                     withError error: Error) {
            reportNavigationFailure(error)
        }

        private func reportNavigationFailure(_ error: Error) {
            guard (error as NSError).code != NSURLErrorCancelled else { return }
            onStatus?("本地工作台暂不可用，请检查运行状态。")
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            navigationGeneration = UUID()
            unlockTask?.cancel()
            unlockTask = nil
            for support in Array(downloads.values) { support.invalidate() }
            downloads.removeAll()
            openPanel?.cancel(nil)
            openPanel = nil
            onStatus?("工作台页面已退出，请重新打开。")
        }

        func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                     for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
            nil
        }

        func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable () -> Void) {
            guard isTrusted(frame, in: webView) else { completionHandler(); return }
            let alert = NSAlert()
            alert.messageText = "Nova Memory"
            alert.informativeText = String(message.prefix(1024))
            alert.addButton(withTitle: "好")
            if let window = webView.window {
                alerts.append(alert)
                alert.beginSheetModal(for: window) { [weak self, weak alert] _ in
                    self?.alerts.removeAll { $0 === alert }
                    completionHandler()
                }
            } else { completionHandler() }
        }

        func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                     initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping @MainActor @Sendable (Bool) -> Void) {
            guard isTrusted(frame, in: webView), let window = webView.window else {
                completionHandler(false)
                return
            }
            let alert = NSAlert()
            alert.messageText = "Nova Memory"
            alert.informativeText = String(message.prefix(1024))
            alert.addButton(withTitle: "继续")
            alert.addButton(withTitle: "取消")
            let viewGeneration = generation
            let documentGeneration = navigationGeneration
            alerts.append(alert)
            alert.beginSheetModal(for: window) { [weak self, weak webView, weak alert] response in
                self?.alerts.removeAll { $0 === alert }
                guard let self, let webView, self.generation == viewGeneration,
                      self.navigationGeneration == documentGeneration,
                      self.isTrusted(frame, in: webView) else { completionHandler(false); return }
                completionHandler(response == .alertFirstButtonReturn)
            }
        }

        func webView(_ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
                     initiatedByFrame frame: WKFrameInfo,
                     completionHandler: @escaping @MainActor @Sendable ([URL]?) -> Void) {
            guard isTrusted(frame, in: webView), let window = webView.window, openPanel == nil else {
                completionHandler(nil)
                return
            }
            let panel = NSOpenPanel()
            panel.canChooseFiles = true
            panel.canChooseDirectories = false
            panel.allowsMultipleSelection = false
            panel.allowedContentTypes = [.json]
            panel.title = "导入评测记录"
            let viewGeneration = generation
            let documentGeneration = navigationGeneration
            openPanel = panel
            panel.beginSheetModal(for: window) { [weak self, weak webView] response in
                defer { if self?.openPanel === panel { self?.openPanel = nil } }
                guard response == .OK, let self, let webView, self.generation == viewGeneration,
                      self.navigationGeneration == documentGeneration,
                      self.isTrusted(frame, in: webView) else { completionHandler(nil); return }
                completionHandler(panel.urls.count == 1 ? panel.urls : nil)
            }
        }

        private func isTrusted(_ frame: WKFrameInfo, in webView: WKWebView) -> Bool {
            guard self.webView === webView, rulesReady, frame.isMainFrame, let origin,
                  let frameURL = frame.request.url, let pageURL = webView.url else { return false }
            return origin.allowsPage(frameURL) && origin.allowsPage(pageURL)
        }

        private static let prepareUnlockScript = """
        for (let attempt = 0; attempt < 100; attempt++) {
          const workspace = document.getElementById('workspace');
          const unlock = document.getElementById('unlock');
          if (workspace && !workspace.hidden) return {identity: globalThis.__novaDesktopDocumentIdentity, ready: true};
          if (unlock && !unlock.hidden && document.getElementById('owner-token') && document.getElementById('unlock-form')) {
            return {identity: globalThis.__novaDesktopDocumentIdentity, locked: true,
                    manualUnlock: unlock.dataset.credentialUnlock === 'required'};
          }
          await new Promise(resolve => setTimeout(resolve, 50));
        }
        return null;
        """

        private static let submitUnlockScript = """
        if (globalThis.__novaDesktopDocumentIdentity !== identity || location.pathname !== '/app/' || location.search) return false;
        const input = document.getElementById('owner-token');
        const form = document.getElementById('unlock-form');
        const unlock = document.getElementById('unlock');
        if (!input || !form || !unlock || unlock.hidden || unlock.dataset.credentialUnlock === 'required') return false;
        try { input.value = credential; form.requestSubmit(); }
        finally { input.value = ''; }
        return true;
        """

        private static let checkUnlockScript = """
        for (let attempt = 0; attempt < 100; attempt++) {
          if (globalThis.__novaDesktopDocumentIdentity !== identity) return false;
          const workspace = document.getElementById('workspace');
          if (workspace && !workspace.hidden) return true;
          if (document.getElementById('unlock-status')?.textContent) return false;
          await new Promise(resolve => setTimeout(resolve, 50));
        }
        return false;
        """
    }
}

/// Deliberately narrower than a browser: resources may use this one origin;
/// documents are only the workspace and its bundled installation guide.
private struct TrustedWorkspaceOrigin {
    let port: Int
    let baseURL: URL
    let navigationPolicy: LocalNavigationPolicy

    init?(baseURL: URL) {
        guard let policy = try? LocalNavigationPolicy(baseURL: baseURL),
              let components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false),
              components.scheme == "http", components.host == "127.0.0.1",
              let port = components.port, (1...65535).contains(port),
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path == "/app/",
              let normalized = URL(string: "http://127.0.0.1:\(port)/") else { return nil }
        self.port = port
        self.baseURL = normalized
        navigationPolicy = policy
    }

    var workspaceURL: URL { baseURL.appendingPathComponent("app", isDirectory: true) }

    func allowsPage(_ url: URL) -> Bool {
        guard navigationPolicy.allows(url),
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme == "http", components.host == "127.0.0.1", components.port == port,
              components.user == nil, components.password == nil, components.query == nil else { return false }
        return components.percentEncodedPath == "/app/" || components.percentEncodedPath == "/install"
    }

    func isUnlockPage(_ url: URL) -> Bool {
        allowsPage(url) && navigationPolicy.credentialEligibleURL(url)
    }

    func allowsDownloadURL(_ url: URL) -> Bool {
        let prefix = "blob:http://127.0.0.1:\(port)/"
        let address = url.absoluteString
        guard address.hasPrefix(prefix) else { return false }
        // WebKit creates one opaque UUID after the originating local URL.
        return UUID(uuidString: String(address.dropFirst(prefix.count))) != nil
    }

    var contentRules: String {
        // JSON encoding avoids altering the regular expression through Swift/JSON escaping.
        let allowed = "^http://127\\.0\\.0\\.1:\(port)/"
        let localBlob = "^blob:http://127\\.0\\.0\\.1:\(port)/[0-9a-fA-F-]+$"
        let rules: [[String: [String: String]]] = [
            ["trigger": ["url-filter": ".*"], "action": ["type": "block"]],
            ["trigger": ["url-filter": allowed], "action": ["type": "ignore-previous-rules"]],
            ["trigger": ["url-filter": localBlob], "action": ["type": "ignore-previous-rules"]]
        ]
        return String(data: try! JSONSerialization.data(withJSONObject: rules), encoding: .utf8)!
    }
}
