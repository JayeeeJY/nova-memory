import XCTest
import Foundation
import Darwin
@testable import NovaDesktopCore

/// Explicitly opted-in packaged-runtime exercise against an empty, independent
/// profile and a metadata-only synthetic model catalog. Never uses a real model.
final class ModelSetupIntegrationTests: XCTestCase {
    func testPausedModelConfigurationAndSyntheticCatalog() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let bundlePath = environment["NOVA_DESKTOP_TEST_RUNTIME"], !bundlePath.isEmpty,
              let profilePath = environment["NOVA_MODEL_SETUP_TEST_PROFILE"], !profilePath.isEmpty,
              let fixtureEndpoint = environment["NOVA_MODEL_SETUP_TEST_ENDPOINT"], !fixtureEndpoint.isEmpty else {
            throw XCTSkip("Set the runtime, a new model-setup profile, and an explicit synthetic catalog endpoint to opt in.")
        }
        let repository = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let bundle = repository.appendingPathComponent(".artifacts/Nova Memory Local", isDirectory: true)
        let artifacts = repository.appendingPathComponent(".artifacts", isDirectory: true)
        guard bundlePath == bundle.path else { throw ModelIntegrationFailure.wrongBundle }
        try PrivateFile.validateRawPath(profilePath)
        guard profilePath.hasPrefix(artifacts.path + "/") else { throw ModelIntegrationFailure.wrongProfile }
        let profile = URL(fileURLWithPath: profilePath, isDirectory: true)
        let evidence = profile.deletingLastPathComponent().appendingPathComponent(profile.lastPathComponent + ".result.json")
        guard !FileManager.default.fileExists(atPath: profile.path),
              !FileManager.default.fileExists(atPath: evidence.path) else { throw ModelIntegrationFailure.existingProfile }

        // A literal, dedicated fixture origin only. No DNS, user-supplied remote
        // endpoints, existing model daemons, query strings, or path normalization.
        guard let parts = URLComponents(string: fixtureEndpoint), let fixturePort = parts.port,
              (31300...31399).contains(fixturePort),
              fixtureEndpoint == "http://127.0.0.1:\(fixturePort)/v1" else {
            throw ModelIntegrationFailure.wrongFixture
        }
        let endpoint = try LocalModelEndpoint(fixtureEndpoint)
        try Self.requireFreePort()
        let runtime = RuntimeClient(configuration: try DesktopConfiguration(runtimeBundle: bundle, profileDirectory: profile, port: 3061))
        let client = WorkspaceModelClient(runtime: runtime)
        try FileManager.default.createDirectory(at: profile.deletingLastPathComponent(), withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])

        do {
            let launched = try await runtime.perform(action: .launch)
            try Self.requireReady(launched, compilerEnabled: false)
            let first = try await client.load()
            XCTAssertEqual(first.configuration.revision, 1)
            XCTAssertEqual(first.configuration.location, .unconfigured)
            XCTAssertFalse(first.compilerAvailable)
            try await Self.requireEmptyJobs(runtime)

            let selected = WorkspaceModelConfiguration(revision: 1, location: .local,
                                                        modelID: "synthetic-setup-fixture", endpoint: fixtureEndpoint)
            _ = try await client.savePaused(selection: selected, expectedRevision: 1)
            let configured = try await client.load()
            XCTAssertEqual(configured.configuration.revision, 2)
            XCTAssertEqual(configured.configuration.location, .local)
            XCTAssertEqual(configured.configuration.modelID, "synthetic-setup-fixture")
            XCTAssertEqual(configured.configuration.endpoint, fixtureEndpoint)
            XCTAssertFalse(configured.compilerAvailable)
            do {
                _ = try await client.savePaused(selection: selected, expectedRevision: 1)
                XCTFail("A stale model configuration revision was accepted.")
            } catch {
                XCTAssertEqual(error as? ModelSetupError, .staleRevision)
            }
            let afterStale = try await client.load()
            XCTAssertEqual(afterStale.configuration, configured.configuration)

            let probe = await LocalModelProbe().check(endpoint: endpoint, selectedModelID: "synthetic-setup-fixture")
            XCTAssertEqual(probe.status, .available)
            XCTAssertEqual(probe.modelIDs, ["synthetic-setup-fixture"])
            XCTAssertEqual(probe.selectedModelFound, true)

            // This profile has never received a capture. Refuse to enable the
            // processor if any earlier assertion or the empty-queue check failed.
            try await Self.requireEmptyJobs(runtime)
            guard (testRun?.failureCount ?? 1) == 0 else { throw ModelIntegrationFailure.failedCheck }
            _ = try await runtime.perform(action: .stop)
            _ = try await runtime.perform(action: .configureOn, maxCalls: 1)
            let enabled = try await runtime.perform(action: .launch)
            try Self.requireReady(enabled, compilerEnabled: true)
            try await Self.requireEmptyJobs(runtime)
            let enabledState = try await client.load()
            XCTAssertTrue(enabledState.compilerAvailable)
            XCTAssertEqual(enabledState.configuration, configured.configuration)
            let unconfigured = WorkspaceModelConfiguration(revision: 2, location: .unconfigured)
            do {
                _ = try await client.savePaused(selection: unconfigured, expectedRevision: 2)
                XCTFail("Native model save was accepted while processing was enabled.")
            } catch {
                XCTAssertEqual(error as? ModelSetupError, .processorMustPause)
            }
            let afterRejectedSave = try await client.load()
            XCTAssertEqual(afterRejectedSave.configuration, configured.configuration)

            _ = try await runtime.perform(action: .stop)
            _ = try await runtime.perform(action: .configureOff)
            let paused = try await runtime.perform(action: .launch)
            try Self.requireReady(paused, compilerEnabled: false)
            _ = try await client.savePaused(selection: unconfigured, expectedRevision: 2)
            let final = try await client.load()
            XCTAssertEqual(final.configuration.revision, 3)
            XCTAssertEqual(final.configuration.location, .unconfigured)
            XCTAssertNil(final.configuration.modelID)
            XCTAssertNil(final.configuration.endpoint)
            XCTAssertFalse(final.compilerAvailable)
            try await Self.requireEmptyJobs(runtime)
            _ = try await runtime.perform(action: .stop)
            let stopped = try await runtime.perform(action: .status)
            XCTAssertEqual(stopped.status, "stopped")
            XCTAssertEqual(stopped.ready, false)
            let stoppedSettings = try await runtime.readSettings()
            XCTAssertFalse(stoppedSettings.compilerEnabled)
            guard (testRun?.failureCount ?? 1) == 0 else { throw ModelIntegrationFailure.failedCheck }

            // No credentials, configuration payloads, source bodies, local paths,
            // or model response bodies are included in this evidence artifact.
            let result: [String: Any] = [
                "schema_version": "nova.desktop-model-setup-integration.v0.1",
                "status": "completed", "model_unconfigured": true, "compiler_enabled": false,
                "checks": ["fresh_profile_only": true, "port_3061_verified": true,
                           "empty_queue_throughout": true, "paused_save_and_reload": true,
                           "stale_revision_rejected": true, "synthetic_catalog_available": true,
                           "save_while_processing_rejected": true, "clear_configuration_verified": true,
                           "runtime_finally_stopped": true, "real_model_verified": false]
            ]
            try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys])
                .write(to: evidence, options: .withoutOverwriting)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: evidence.path)
        } catch {
            // The identity-bound runtime controller can only stop this selected
            // test profile; no PID-based or broad service cleanup is used.
            _ = try? await runtime.perform(action: .stop)
            throw error
        }
    }

    private static func requireReady(_ snapshot: RuntimeSnapshot, compilerEnabled: Bool) throws {
        guard snapshot.status == "running", snapshot.ready == true, snapshot.port == 3061,
              snapshot.workbenchURL?.absoluteString == "http://127.0.0.1:3061/app/",
              snapshot.compilerAvailable == compilerEnabled else { throw ModelIntegrationFailure.invalidRuntime }
    }

    private static func requireEmptyJobs(_ runtime: RuntimeClient) async throws {
        let owner = try await runtime.readOwnerCredential()
        let url = URL(string: "http://127.0.0.1:3061/console/api/owner/intake/jobs")!
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer " + owner, forHTTPHeaderField: "Authorization")
        request.setValue("owner-review", forHTTPHeaderField: "X-Nova-Review")
        request.setValue("http://127.0.0.1:3061", forHTTPHeaderField: "Origin")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let response = try await LocalHTTPTransport().request(request, maximumBytes: 65536, timeout: 5)
        guard response.statusCode == 200, response.url == url,
              let value = try JSONSerialization.jsonObject(with: response.data) as? [String: Any],
              let items = value["items"] as? [[String: Any]], items.isEmpty else {
            throw ModelIntegrationFailure.nonemptyProfile
        }
    }

    private static func requireFreePort() throws {
        let descriptor = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw XCTSkip("Cannot check an independent loopback port.") }
        defer { Darwin.close(descriptor) }
        var reuse: Int32 = 1
        guard setsockopt(descriptor, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size)) == 0 else {
            throw XCTSkip("Cannot establish an independent loopback port probe.")
        }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = UInt16(3061).bigEndian
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0 else { throw XCTSkip("Port 3061 is unavailable; existing services are left running.") }
    }
}

private enum ModelIntegrationFailure: Error {
    case wrongBundle, wrongProfile, wrongFixture, existingProfile, invalidRuntime, nonemptyProfile, failedCheck
}
