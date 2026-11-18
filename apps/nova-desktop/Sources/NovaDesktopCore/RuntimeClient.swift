import Foundation
import Darwin

public enum RuntimeAction: Sendable, Equatable {
    case status, launch, start, stop, doctor, recover, configureOn, configureOff

    var command: String {
        switch self {
        case .configureOn, .configureOff: return "configure"
        default: return String(describing: self)
        }
    }
}

public struct RuntimeSnapshot: Decodable, Sendable {
    public let status: String
    public let ready: Bool?
    public let profileID: String?
    public let instanceID: String?
    public let url: String?
    public let port: Int?
    public let modelSelection: String?
    public let compilerAvailable: Bool?
    public let continuityAvailable: Bool?
    public let rankedRetrievalAvailable: Bool?
    public let backupQuarantined: Bool?
    public let bundleID: String?
    /// This is a fixed local message. Runtime-provided free text is never exposed.
    public let message: String?

    enum CodingKeys: String, CodingKey {
        case status, ready, url, port, runtime
        case profileID = "profile_id", instanceID = "instance_id", modelSelection = "model_selection"
        case compilerAvailable = "compiler_available", bundleID = "bundle_id"
        case continuityAvailable = "continuity_available"
        case rankedRetrievalAvailable = "ranked_retrieval_available"
        case backupQuarantined = "backup_quarantined"
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        if values.contains(.runtime) {
            let nested = try values.decode(RuntimeSnapshot.self, forKey: .runtime)
            self.status = nested.status
            self.ready = nested.ready
            self.profileID = nested.profileID
            self.instanceID = nested.instanceID
            self.url = nested.url
            self.port = nested.port
            self.modelSelection = nested.modelSelection
            self.compilerAvailable = nested.compilerAvailable
            self.continuityAvailable = nested.continuityAvailable
            self.rankedRetrievalAvailable = nested.rankedRetrievalAvailable
            self.backupQuarantined = nested.backupQuarantined
            self.bundleID = try values.decodeIfPresent(String.self, forKey: .bundleID) ?? nested.bundleID
        } else {
            self.status = try values.decode(String.self, forKey: .status)
            self.ready = try values.decodeIfPresent(Bool.self, forKey: .ready)
            self.profileID = try values.decodeIfPresent(String.self, forKey: .profileID)
            self.instanceID = try values.decodeIfPresent(String.self, forKey: .instanceID)
            self.url = try values.decodeIfPresent(String.self, forKey: .url)
            self.port = try values.decodeIfPresent(Int.self, forKey: .port)
            self.modelSelection = try values.decodeIfPresent(String.self, forKey: .modelSelection)
            self.compilerAvailable = try values.decodeIfPresent(Bool.self, forKey: .compilerAvailable)
            self.continuityAvailable = values.contains(.continuityAvailable)
                ? try values.decode(Bool.self, forKey: .continuityAvailable) : nil
            self.rankedRetrievalAvailable = values.contains(.rankedRetrievalAvailable)
                ? try values.decode(Bool.self, forKey: .rankedRetrievalAvailable) : nil
            self.backupQuarantined = try values.decodeIfPresent(Bool.self, forKey: .backupQuarantined)
            self.bundleID = try values.decodeIfPresent(String.self, forKey: .bundleID)
        }
        guard ["running", "stopped", "starting", "unresponsive", "configured", "initialized"].contains(status),
              profileID.map({ UUID(uuidString: $0) != nil }) ?? true,
              instanceID.map({ $0.count >= 32 && $0.count <= 64 && $0.allSatisfy(\.isHexDigit) }) ?? true,
              bundleID.map({ $0.count >= 32 && $0.count <= 64 && $0.allSatisfy(\.isHexDigit) }) ?? true,
              modelSelection.map({ ["unconfigured", "local", "remote", "unknown"].contains($0) }) ?? true else {
            throw DesktopError.invalidResponse
        }
        self.message = status == "unresponsive" ? "本地运行状态无法核对，请先检查或恢复此记忆库。" : nil
    }

    public var workbenchURL: URL? {
        guard status == "running", ready == true, let value = url, let parsed = URL(string: value),
              let expectedPort = port, LocalNavigationPolicy.localPort(parsed) == expectedPort,
              URLComponents(url: parsed, resolvingAgainstBaseURL: false)?.path == "/app/",
              parsed.query == nil, parsed.fragment == nil else { return nil }
        return parsed
    }
}

public struct RuntimeClient: Sendable {
    public let configuration: DesktopConfiguration
    private let timeout: TimeInterval

    public init(configuration: DesktopConfiguration) {
        self.configuration = configuration
        // Leave the runtime enough time to finish its bounded startup probe and
        // reap its own supervisor on failure before cancelling the CLI process.
        self.timeout = 45
    }

    // A shorter timeout is useful only in process-boundary fixtures.
    init(configuration: DesktopConfiguration, timeout: TimeInterval) {
        self.configuration = configuration
        self.timeout = timeout
    }

    public func perform(action: RuntimeAction, maxCalls: Int? = nil, continuityEnabled: Bool? = nil,
                        rankedRetrievalEnabled: Bool? = nil) async throws -> RuntimeSnapshot {
        let arguments = try arguments(for: action, maxCalls: maxCalls, continuityEnabled: continuityEnabled,
                                      rankedRetrievalEnabled: rankedRetrievalEnabled)
        let data = try await run(arguments)
        let snapshot: RuntimeSnapshot
        do { snapshot = try JSONDecoder().decode(RuntimeSnapshot.self, from: data) }
        catch { throw DesktopError.invalidResponse }
        try validate(snapshot)
        return snapshot
    }

    public func prepareBackupInspection(snapshot: URL, key: URL, destination: URL) async throws -> Int {
        for url in [snapshot, key, destination] { try PrivateFile.validatePath(url) }
        return try await backupInspectionPort(command: "inspect-backup", directory: destination,
                                              extra: ["--backup-file", snapshot.path, "--backup-key-file", key.path],
                                              status: "backup_inspection_prepared")
    }

    public func checkBackupInspection(directory: URL) async throws -> Int {
        try PrivateFile.validateDirectory(directory)
        return try await backupInspectionPort(command: "check-inspection", directory: directory,
                                              extra: [], status: "backup_inspection_checked")
    }

    private func backupInspectionPort(command: String, directory: URL, extra: [String], status: String) async throws -> Int {
        struct Reply: Decodable {
            let status: String
            let port: Int
            let backup_quarantined: Bool
            let runtime_started: Bool
        }
        let data = try await run([try runtimeExecutable().path, command, "--bundle", configuration.runtimeBundle.path,
                                  "--directory", directory.path] + extra)
        guard let result = try? JSONDecoder().decode(Reply.self, from: data),
              result.status == status, (1024...65535).contains(result.port),
              result.backup_quarantined, !result.runtime_started else { throw DesktopError.invalidResponse }
        return result.port
    }

    private func run(_ arguments: [String]) async throws -> Data {
        let operation = BoundedProcess()
        let result = try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in
                DispatchQueue.global(qos: .userInitiated).async {
                    do { continuation.resume(returning: try operation.run(arguments: arguments, timeout: timeout)) }
                    catch { continuation.resume(throwing: error) }
                }
            }
        }, onCancel: { operation.cancel() })
        guard result.exitCode == 0 else { throw Self.safeRuntimeError(result.standardError) }
        return result.standardOutput
    }

    public func readOwnerCredential() async throws -> String {
        let snapshot = try await perform(action: .status)
        guard snapshot.status == "running", snapshot.ready == true, snapshot.workbenchURL != nil else { throw DesktopError.runtimeNotReady }
        return try readCredentialFile()
    }

    public func readSettings() async throws -> RuntimeSettings {
        let metadata = try profileMetadata()
        return RuntimeSettings(compilerEnabled: metadata.compilerEnabled, maximumCalls: metadata.maximumCalls,
                               continuityEnabled: metadata.continuityEnabled,
                               rankedRetrievalEnabled: metadata.rankedRetrievalEnabled)
    }

    func readCredentialFile() throws -> String {
        do {
            try PrivateFile.validateDirectory(configuration.profileDirectory)
            let data = try PrivateFile.read(configuration.profileDirectory.appendingPathComponent("owner.key"), maximum: 4096, exactMode: 0o600)
            guard let raw = String(data: data, encoding: .utf8) else { throw DesktopError.invalidCredential }
            let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard value.utf8.count == 64, value.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw DesktopError.invalidCredential
            }
            return value
        } catch { throw DesktopError.invalidCredential }
    }

    func arguments(for action: RuntimeAction, maxCalls: Int?, continuityEnabled: Bool? = nil,
                   rankedRetrievalEnabled: Bool? = nil) throws -> [String] {
        if configuration.backupInspection && [.launch, .configureOn, .configureOff].contains(action) {
            throw DesktopError.invalidBackup
        }
        try PrivateFile.validatePath(configuration.profileDirectory)
        if action != .launch && !FileManager.default.fileExists(atPath: configuration.profileDirectory.path) { throw DesktopError.profileMissing }
        let script = try runtimeExecutable()
        if let budget = maxCalls {
            guard action == .configureOn || action == .configureOff, (1...10000).contains(budget) else { throw DesktopError.invalidBudget }
        }
        if (continuityEnabled != nil || rankedRetrievalEnabled != nil) && action != .configureOn && action != .configureOff {
            throw DesktopError.operationFailed
        }
        var values = [script.path, action.command, "--bundle", configuration.runtimeBundle.path,
                      "--directory", configuration.profileDirectory.path, "--port", String(configuration.port)]
        if action == .launch { values.append("--no-open") }
        if action == .start && configuration.backupInspection { values.append("--inspection-only") }
        if action == .configureOn { values += ["--compiler", "on"] }
        if action == .configureOff { values += ["--compiler", "off"] }
        if let budget = maxCalls { values += ["--max-calls", String(budget)] }
        if let enabled = continuityEnabled { values += ["--continuity", enabled ? "on" : "off"] }
        if let enabled = rankedRetrievalEnabled { values += ["--ranked-retrieval", enabled ? "on" : "off"] }
        return values
    }

    private func runtimeExecutable() throws -> URL {
        let script = configuration.runtimeBundle.appendingPathComponent("bin/nova-local")
        try PrivateFile.validatePath(script)
        var info = stat()
        guard lstat(script.path, &info) == 0, info.st_mode & S_IFMT == S_IFREG,
              info.st_uid == getuid(), info.st_mode & 0o077 == 0,
              FileManager.default.isExecutableFile(atPath: script.path) else { throw DesktopError.runtimeUnavailable }
        return script
    }

    func validate(_ snapshot: RuntimeSnapshot) throws {
        if snapshot.status == "running" || snapshot.ready == true || snapshot.url != nil {
            guard let url = snapshot.workbenchURL, snapshot.profileID != nil else { throw DesktopError.invalidURL }
            let metadata = try profileMetadata()
            guard snapshot.port == metadata.port, snapshot.profileID == metadata.profileID,
                  url.port == metadata.port else { throw DesktopError.invalidProfile }
            guard (snapshot.continuityAvailable ?? false) == metadata.continuityEnabled,
                  (snapshot.rankedRetrievalAvailable ?? false) == metadata.rankedRetrievalEnabled else {
                throw DesktopError.runtimeNotReady
            }
        } else if let identity = snapshot.profileID {
            guard identity == (try profileMetadata()).profileID else { throw DesktopError.invalidProfile }
        }
    }

    private func profileMetadata() throws -> ProfileMetadata {
        try PrivateFile.validateDirectory(configuration.profileDirectory)
        let raw = try PrivateFile.read(configuration.profileDirectory.appendingPathComponent("runtime.json"), maximum: 65536)
        let parsed: ProfileMetadata
        do { parsed = try JSONDecoder().decode(ProfileMetadata.self, from: raw) }
        catch { throw DesktopError.invalidProfile }
        guard parsed.schemaVersion == "nova.local-runtime.v0.1", UUID(uuidString: parsed.profileID) != nil,
              (1024...65535).contains(parsed.port), (1...10000).contains(parsed.maximumCalls) else { throw DesktopError.invalidProfile }
        return parsed
    }

    private static func safeRuntimeError(_ data: Data) -> DesktopError {
        struct RuntimeFailure: Decodable { let code: String }
        // Only a final JSON error code is interpreted; runtime messages are never shown.
        guard let last = data.split(separator: 10).last,
              let code = try? JSONDecoder().decode(RuntimeFailure.self, from: Data(last)).code else { return .operationFailed }
        switch code {
        case "runtime_busy": return .runtimeBusy
        case "port_in_use": return .portInUse
        case "stop_before_configure": return .stopBeforeConfigure
        case "invalid_budget": return .invalidBudget
        case "runtime_not_ready", "runtime_unverified", "wrong_runtime_instance", "readiness_mismatch": return .runtimeNotReady
        case "invalid_profile", "invalid_configuration", "private_path_required", "synthetic_profile": return .invalidProfile
        case "symlink_path": return .symlinkPath
        case "invalid_backup_key_file": return .invalidBackupKey
        case "backup_verification_failed", "invalid_backup_file", "backup_inspection_required": return .invalidBackup
        default: return .operationFailed
        }
    }
}

public struct RuntimeSettings: Equatable, Sendable {
    public let compilerEnabled: Bool
    public let maximumCalls: Int
    public let continuityEnabled: Bool
    public let rankedRetrievalEnabled: Bool
}

private struct ProfileMetadata: Decodable {
    let schemaVersion: String
    let profileID: String
    let port: Int
    let compilerEnabled: Bool
    let maximumCalls: Int
    let continuityEnabled: Bool
    let rankedRetrievalEnabled: Bool
    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version", profileID = "profile_id", port
        case compilerEnabled = "compiler_enabled", maximumCalls = "compiler_max_calls"
        case continuityEnabled = "continuity_enabled"
        case rankedRetrievalEnabled = "ranked_retrieval_enabled"
    }
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try values.decode(String.self, forKey: .schemaVersion)
        profileID = try values.decode(String.self, forKey: .profileID)
        port = try values.decode(Int.self, forKey: .port)
        compilerEnabled = try values.decode(Bool.self, forKey: .compilerEnabled)
        maximumCalls = try values.decode(Int.self, forKey: .maximumCalls)
        continuityEnabled = values.contains(.continuityEnabled)
            ? try values.decode(Bool.self, forKey: .continuityEnabled) : false
        rankedRetrievalEnabled = values.contains(.rankedRetrievalEnabled)
            ? try values.decode(Bool.self, forKey: .rankedRetrievalEnabled) : false
    }
}

struct ProcessOutput { let exitCode: Int32; let standardOutput: Data; let standardError: Data }

/// Runs off the main actor, drains both pipes independently, and retains at most 64 KiB per stream.
private final class BoundedProcess: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var failure: DesktopError?
    private var output = Data()
    private var errors = Data()

    func cancel() { abort(.cancelled) }

    private func abort(_ reason: DesktopError) {
        lock.lock()
        if failure == nil { failure = reason }
        let child = process
        lock.unlock()
        terminate(child)
    }

    private func terminate(_ child: Process?) {
        guard let child, child.isRunning else { return }
        child.terminate()
        DispatchQueue.global().asyncAfter(deadline: .now() + 1) {
            if child.isRunning { kill(child.processIdentifier, SIGKILL) }
        }
    }

    private func collect(_ handle: FileHandle, standardError: Bool) {
        var buffer = [UInt8](repeating: 0, count: 8192)
        while true {
            let count = Darwin.read(handle.fileDescriptor, &buffer, buffer.count)
            if count == 0 { break }
            if count < 0 { if errno == EINTR { continue }; break }
            let chunk = Data(buffer.prefix(count))
            lock.lock()
            let current = standardError ? errors.count : output.count
            let allowed = max(0, 65536 - current)
            if standardError { errors.append(chunk.prefix(allowed)) } else { output.append(chunk.prefix(allowed)) }
            let overflow = chunk.count > allowed
            lock.unlock()
            if overflow { abort(.outputLimit); break }
        }
    }

    func run(arguments: [String], timeout: TimeInterval) throws -> ProcessOutput {
        let child = Process()
        guard let executable = arguments.first else { throw DesktopError.runtimeUnavailable }
        child.executableURL = URL(fileURLWithPath: executable)
        child.arguments = Array(arguments.dropFirst())
        // The bundled executable inherits no credentials, module hooks or proxy settings.
        child.environment = ["PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"]
        let stdout = Pipe(), stderr = Pipe()
        child.standardInput = FileHandle.nullDevice
        child.standardOutput = stdout
        child.standardError = stderr
        lock.lock()
        process = child
        let before = failure
        lock.unlock()
        if let before { throw before }
        do { try child.run() } catch { throw DesktopError.runtimeUnavailable }
        lock.lock(); let after = failure; lock.unlock()
        if after != nil { terminate(child) }
        let drains = DispatchGroup()
        drains.enter()
        DispatchQueue.global().async { self.collect(stdout.fileHandleForReading, standardError: false); drains.leave() }
        drains.enter()
        DispatchQueue.global().async { self.collect(stderr.fileHandleForReading, standardError: true); drains.leave() }
        let timer = DispatchWorkItem { self.abort(.timedOut) }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timer)
        child.waitUntilExit()
        timer.cancel()
        if drains.wait(timeout: .now() + 2) == .timedOut { abort(.invalidResponse) }
        try? stdout.fileHandleForReading.close()
        try? stderr.fileHandleForReading.close()
        lock.lock()
        let finalFailure = failure
        let result = ProcessOutput(exitCode: child.terminationStatus, standardOutput: output, standardError: errors)
        process = nil
        lock.unlock()
        if let finalFailure { throw finalFailure }
        return result
    }
}
