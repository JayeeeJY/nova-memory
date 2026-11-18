import XCTest
import Foundation
import Darwin
@testable import NovaDesktopCore

/// This opt-in exercises the self-contained packaged Rust runtime through the desktop client.
/// It never uses the default profile, an existing Vault, or a configured model endpoint.
final class DesktopRuntimeIntegrationTests: XCTestCase {
    func testIndependentPackagedRuntimePersistenceAndProcessingSwitch() async throws {
        guard let selectedBundle = ProcessInfo.processInfo.environment["NOVA_DESKTOP_TEST_RUNTIME"], !selectedBundle.isEmpty else {
            throw XCTSkip("Set NOVA_DESKTOP_TEST_RUNTIME to the verified local engineering bundle to run this isolated test.")
        }
        guard let selectedProfile = ProcessInfo.processInfo.environment["NOVA_DESKTOP_TEST_PROFILE"], !selectedProfile.isEmpty else {
            throw XCTSkip("Set NOVA_DESKTOP_TEST_PROFILE to a new absolute profile directory under this repository's .artifacts.")
        }
        let repository = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let expectedBundle = URL(fileURLWithPath: selectedBundle, isDirectory: true)
        guard selectedBundle.hasPrefix(repository.appendingPathComponent(".artifacts").path + "/") else { throw IntegrationFailure.wrongBundle }
        try PrivateFile.validateRawPath(selectedBundle)
        try PrivateFile.validateDirectory(expectedBundle)
        try Self.requireFreePort(3060)

        let runID = UUID().uuidString.lowercased()
        let artifacts = repository.appendingPathComponent(".artifacts", isDirectory: true)
        guard selectedProfile.hasPrefix(artifacts.path + "/") else { throw IntegrationFailure.wrongProfile }
        try PrivateFile.validateRawPath(selectedProfile)
        let profile = URL(fileURLWithPath: selectedProfile, isDirectory: true)
        let configuration = try DesktopConfiguration(runtimeBundle: expectedBundle, profileDirectory: profile, port: 3060)
        guard !FileManager.default.fileExists(atPath: profile.path) else { throw IntegrationFailure.existingProfile }
        let artifact = profile.deletingLastPathComponent()
        let evidenceFile = artifact.appendingPathComponent(profile.lastPathComponent + ".result.json")
        guard !FileManager.default.fileExists(atPath: evidenceFile.path) else { throw IntegrationFailure.existingProfile }
        try FileManager.default.createDirectory(at: artifact, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let client = RuntimeClient(configuration: configuration)
        let delegate = LocalOnlySessionDelegate()
        let sessionConfiguration = URLSessionConfiguration.ephemeral
        sessionConfiguration.connectionProxyDictionary = [:]
        sessionConfiguration.urlCache = nil
        sessionConfiguration.httpCookieStorage = nil
        sessionConfiguration.requestCachePolicy = .reloadIgnoringLocalCacheData
        sessionConfiguration.timeoutIntervalForRequest = 5
        sessionConfiguration.timeoutIntervalForResource = 10
        let session = URLSession(configuration: sessionConfiguration, delegate: delegate, delegateQueue: nil)
        defer { session.invalidateAndCancel() }

        do {
            XCTAssertFalse(FileManager.default.fileExists(atPath: profile.path))
            let launched = try await client.perform(action: .launch)
            try Self.assertReady(launched)
            let first = try await Self.awaitReady(client)
            let originalProfileID = try XCTUnwrap(first.profileID)
            XCTAssertEqual(first.modelSelection, "unconfigured")
            XCTAssertEqual(first.compilerAvailable, false)
            let initialSettings = try await client.readSettings()
            XCTAssertFalse(initialSettings.compilerEnabled)
            XCTAssertFalse(initialSettings.continuityEnabled)
            XCTAssertFalse(initialSettings.rankedRetrievalEnabled)
            XCTAssertEqual(first.rankedRetrievalAvailable, false)
            XCTAssertEqual(first.continuityAvailable, false)
            XCTAssertEqual(initialSettings.maximumCalls, 100)

            // The key stays in memory. Requests only target this test's exact loopback origin.
            let owner = try await client.readOwnerCredential()
            XCTAssertEqual(owner.utf8.count, 64)
            let sourceText = "SYNTHETIC desktop runtime integration only: preserve this isolated test material across a local restart. No personal information and no model execution."
            let capture: [String: Any] = [
                "schema_version": "nova.intake.v0.1", "request_id": "desktop-runtime-" + runID,
                "source_platform": "web-console", "capture_fidelity": "f2_visible_transcript",
                "content": sourceText, "occurred_at": NSNull()
            ]
            let received = try await Self.ownerRequest(session, path: "/console/api/owner/intake", owner: owner, body: capture)
            XCTAssertEqual(received["saved"] as? Bool, true)
            let receipt = try XCTUnwrap(received["receipt"] as? [String: Any])
            let source = try XCTUnwrap(receipt["source"] as? [String: Any])
            let sourceID = try XCTUnwrap(source["object_id"] as? String)
            guard sourceID.utf8.count < 160, sourceID.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") }) else {
                throw IntegrationFailure.invalidResponse
            }
            let beforeRestart = try await Self.ownerRequest(session, path: "/console/api/owner/intake/sources/" + sourceID, owner: owner)
            XCTAssertEqual(beforeRestart["content"] as? String, sourceText)
            let initialJobs = try await Self.ownerRequest(session, path: "/console/api/owner/intake/jobs", owner: owner)
            XCTAssertEqual(initialJobs["processing_blocker"] as? String, "model_unconfigured")

            let stoppedForConfigure = try await client.perform(action: .stop)
            XCTAssertEqual(stoppedForConfigure.status, "stopped")
            XCTAssertEqual(stoppedForConfigure.ready, false)
            _ = try await client.perform(action: .configureOn, maxCalls: 2, continuityEnabled: true, rankedRetrievalEnabled: true)
            let enabledSettings = try await client.readSettings()
            XCTAssertTrue(enabledSettings.compilerEnabled)
            XCTAssertTrue(enabledSettings.continuityEnabled)
            XCTAssertTrue(enabledSettings.rankedRetrievalEnabled)
            XCTAssertEqual(enabledSettings.maximumCalls, 2)

            let relaunched = try await client.perform(action: .launch)
            try Self.assertReady(relaunched)
            let second = try await Self.awaitReady(client)
            XCTAssertEqual(second.profileID, originalProfileID)
            XCTAssertEqual(second.compilerAvailable, true)
            XCTAssertEqual(second.continuityAvailable, true)
            XCTAssertEqual(second.rankedRetrievalAvailable, true)
            XCTAssertEqual(second.modelSelection, "unconfigured")
            let secondOwner = try await client.readOwnerCredential()
            let persisted = try await Self.ownerRequest(session, path: "/console/api/owner/intake/sources/" + sourceID, owner: secondOwner)
            XCTAssertEqual(persisted["content"] as? String, sourceText)
            let enabledJobs = try await Self.ownerRequest(session, path: "/console/api/owner/intake/jobs", owner: secondOwner)
            XCTAssertEqual(enabledJobs["processing_blocker"] as? String, "model_unconfigured")
            XCTAssertEqual((enabledJobs["items"] as? [[String: Any]])?.count, 1)

            _ = try await client.perform(action: .stop)
            _ = try await client.perform(action: .configureOff)
            let paused = try await client.readSettings()
            XCTAssertFalse(paused.compilerEnabled)
            XCTAssertTrue(paused.rankedRetrievalEnabled)
            XCTAssertTrue(paused.continuityEnabled)
            _ = try await client.perform(action: .configureOff, continuityEnabled: false, rankedRetrievalEnabled: false)
            let disabledSettings = try await client.readSettings()
            XCTAssertFalse(disabledSettings.compilerEnabled)
            XCTAssertFalse(disabledSettings.continuityEnabled)
            XCTAssertFalse(disabledSettings.rankedRetrievalEnabled)
            XCTAssertEqual(disabledSettings.maximumCalls, 2)
            _ = try await client.perform(action: .launch)
            let finalRunning = try await Self.awaitReady(client)
            XCTAssertEqual(finalRunning.compilerAvailable, false)
            XCTAssertEqual(finalRunning.continuityAvailable, false)
            XCTAssertEqual(finalRunning.rankedRetrievalAvailable, false)
            XCTAssertEqual(finalRunning.modelSelection, "unconfigured")
            let finalOwner = try await client.readOwnerCredential()
            let finalSource = try await Self.ownerRequest(session, path: "/console/api/owner/intake/sources/" + sourceID, owner: finalOwner)
            XCTAssertEqual(finalSource["content"] as? String, sourceText)
            _ = try await client.perform(action: .stop)
            let finalStatus = try await client.perform(action: .status)
            XCTAssertEqual(finalStatus.status, "stopped")
            XCTAssertEqual(finalStatus.ready, false)
            XCTAssertNil(finalStatus.workbenchURL)

            guard (testRun?.failureCount ?? 1) == 0 else { throw IntegrationFailure.failedCheck }
            // Keep the independent profile as local evidence. Do not export keys or source text.
            let evidence: [String: Any] = [
                "schema_version": "nova.desktop-runtime-integration.v0.1",
                "status": "completed", "capture_id": sourceID, "synthetic_materials": 1,
                "checks": ["port_3060_verified": true, "same_profile_reused": true,
                           "source_persisted_after_restarts": true, "processing_switch_verified": true, "continuity_switch_verified": true,
                           "ranked_retrieval_switch_verified": true, "model_remains_unconfigured": true, "runtime_finally_stopped": true,
                           "real_model_verified": false, "real_host_verified": false]
            ]
            try JSONSerialization.data(withJSONObject: evidence, options: [.prettyPrinted, .sortedKeys]).write(to: evidenceFile, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: evidenceFile.path)
        } catch {
            // The selected profile is unique to this run; no PID-based or broad service cleanup.
            _ = try? await client.perform(action: .stop)
            throw error
        }
    }

    private static func assertReady(_ snapshot: RuntimeSnapshot) throws {
        XCTAssertEqual(snapshot.status, "running")
        XCTAssertEqual(snapshot.ready, true)
        XCTAssertEqual(snapshot.port, 3060)
        XCTAssertEqual(snapshot.workbenchURL?.absoluteString, "http://127.0.0.1:3060/app/")
        _ = try XCTUnwrap(snapshot.profileID)
        _ = try XCTUnwrap(snapshot.instanceID)
        _ = try XCTUnwrap(snapshot.bundleID)
    }

    private static func awaitReady(_ client: RuntimeClient) async throws -> RuntimeSnapshot {
        for _ in 0..<20 {
            let snapshot = try await client.perform(action: .status)
            if snapshot.status == "running", snapshot.ready == true {
                try assertReady(snapshot)
                return snapshot
            }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw IntegrationFailure.readinessTimeout
    }

    private static func requireFreePort(_ port: UInt16) throws {
        let descriptor = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw XCTSkip("Cannot establish an isolated loopback port probe.") }
        defer { Darwin.close(descriptor) }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard result == 0 else { throw XCTSkip("Port 3060 is unavailable; existing services are left running.") }
    }

    private static func ownerRequest(_ session: URLSession, path: String, owner: String, body: [String: Any]? = nil) async throws -> [String: Any] {
        guard path.hasPrefix("/console/api/owner/intake"), !path.contains("?"), !path.contains("#"), !path.contains(".."),
              let url = URL(string: "http://127.0.0.1:3060" + path), url.host == "127.0.0.1", url.port == 3060 else {
            throw IntegrationFailure.invalidRequest
        }
        var request = URLRequest(url: url)
        request.httpMethod = body == nil ? "GET" : "POST"
        request.setValue("Bearer " + owner, forHTTPHeaderField: "Authorization")
        request.setValue("owner-review", forHTTPHeaderField: "X-Nova-Review")
        request.setValue("http://127.0.0.1:3060", forHTTPHeaderField: "Origin")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, http.statusCode == 200,
                  http.url == url, data.count <= 1024 * 1024,
                  let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw IntegrationFailure.invalidResponse
            }
            return value
        } catch { throw IntegrationFailure.localRequestFailed }
    }
}

private enum IntegrationFailure: String, Error {
    case wrongBundle, wrongProfile, existingProfile, invalidRequest, invalidResponse, localRequestFailed, readinessTimeout, failedCheck
}

private final class LocalOnlySessionDelegate: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
