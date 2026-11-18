import AppKit
import SwiftUI
import NovaDesktopCore

@main
struct NovaMemoryDesktopApp: App {
    @NSApplicationDelegateAdaptor(DesktopAppDelegate.self) private var appDelegate
    @StateObject private var store = DesktopStore()

    var body: some Scene {
        WindowGroup("Nova Memory", id: "main") {
            ContentView(store: store)
                .frame(minWidth: 920, minHeight: 620)
                .onAppear { store.startPolling() }
        }
        .defaultSize(width: 1240, height: 840)
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandMenu("本机服务") {
                Button("启动工作台") { Task { await store.start() } }
                    .disabled(store.isBusy || store.isRunning)
                Button("停止本机服务") { Task { await store.stop() } }
                    .disabled(store.isBusy || !store.profileExists)
                Divider()
                Button("检查状态") { Task { await store.refresh() } }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(store.isBusy)
            }
        }
        Settings {
            RuntimeSettingsView(store: store)
                .frame(width: 640, height: 720)
        }
    }
}

final class DesktopAppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        if let url = Bundle.main.url(forResource: "NovaLogo", withExtension: "png"),
           let icon = NSImage(contentsOf: url) {
            NSApp.applicationIconImage = icon
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        // The persistent local runtime has its own explicit Stop action, as in the local bundle.
        true
    }
}
