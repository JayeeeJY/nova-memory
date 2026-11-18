import XCTest
import Foundation
import Darwin
@testable import NovaDesktopCore

final class DesktopCoreTests: XCTestCase {
    func testDefaultsKeepDedicatedLocalProfile() throws {
        let selected = try DesktopConfiguration.parse(arguments: [], resourceURL: URL(fileURLWithPath: "/Applications/Nova Memory.app/Contents/Resources"), homeDirectory: URL(fileURLWithPath: "/Users/example"))
        XCTAssertEqual(selected.port, 3053)
        XCTAssertEqual(selected.runtimeBundle.path, "/Applications/Nova Memory.app/Contents/Resources/LocalRuntime")
        XCTAssertEqual(selected.profileDirectory.path, "/Users/example/Library/Application Support/Nova Memory/local-workspace-v03")
    }

    func testExplicitCLIPathsAndPort() throws {
        let selected = try DesktopConfiguration.parse(arguments: ["--runtime-bundle", "/private/tmp/runtime with spaces", "--profile-dir", "/private/tmp/profile", "--port", "4321"], resourceURL: nil, homeDirectory: URL(fileURLWithPath: "/Users/example"))
        XCTAssertEqual(selected.port, 4321)
        XCTAssertEqual(selected.runtimeBundle.lastPathComponent, "runtime with spaces")
    }

    func testBackupInspectionRequiresExplicitProfileAndRestrictedStartup() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let home = URL(fileURLWithPath: "/Users/example")
        let args = ["--runtime-bundle", fixture.bundle.path, "--backup-inspection"]
        XCTAssertThrowsError(try DesktopConfiguration.parse(arguments: args, resourceURL: nil, homeDirectory: home))
        let configuration = try DesktopConfiguration.parse(
            arguments: args + ["--profile-dir", fixture.profile.path, "--port", "4321"],
            resourceURL: nil, homeDirectory: home)
        let client = RuntimeClient(configuration: configuration)
        XCTAssertTrue(try client.arguments(for: .start, maxCalls: nil).contains("--inspection-only"))
        XCTAssertThrowsError(try client.arguments(for: .launch, maxCalls: nil))
        XCTAssertThrowsError(try client.arguments(for: .configureOn, maxCalls: nil))
    }

    func testCheckingExistingBackupRequiresQuarantineAndDoesNotAcceptLaunch() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let client = RuntimeClient(configuration: try fixture.configuration())
        try fixture.script("""
        import json, sys
        assert sys.argv[1] == 'check-inspection' and '--backup-key-file' not in sys.argv
        print(json.dumps(dict(status='backup_inspection_checked', port=4321, backup_quarantined=True, runtime_started=False)))
        """)
        let port = try await client.checkBackupInspection(directory: fixture.profile)
        XCTAssertEqual(port, 4321)

        let base: [String: Any] = ["status": "backup_inspection_checked", "port": 4321,
                                   "backup_quarantined": true, "runtime_started": false]
        for (field, invalid) in [("backup_quarantined", false), ("runtime_started", true), ("port", 80)] as [(String, Any)] {
            var changed = base
            changed[field] = invalid
            try fixture.respond(changed)
            do {
                _ = try await client.checkBackupInspection(directory: fixture.profile)
                XCTFail("accepted invalid inspection response: \(field)")
            } catch { XCTAssertEqual(error as? DesktopError, .invalidResponse) }
        }
    }

    func testUnknownDuplicateAndMissingCLIValuesRejected() {
        for args in [["--unknown"], ["--port"], ["--port", "3053", "--port", "3054"], ["--port", "x"], ["--port", "-1"], ["--port", "1023"], ["--port", "65536"]] {
            XCTAssertThrowsError(try DesktopConfiguration.parse(arguments: args, resourceURL: URL(fileURLWithPath: "/Applications/Example"), homeDirectory: URL(fileURLWithPath: "/Users/example")))
        }
    }

    func testRelativeAndTraversalPathsRejectedBeforeFoundationNormalizes() {
        for path in ["relative", "/private/tmp/../profile", "/private/./profile", "/private/tmp/a\0b"] {
            XCTAssertThrowsError(try DesktopConfiguration.parse(arguments: ["--runtime-bundle", path], resourceURL: nil, homeDirectory: URL(fileURLWithPath: "/Users/example")))
        }
    }

    func testSymlinkAncestorRejected() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let alias = fixture.root.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: fixture.profile)
        XCTAssertThrowsError(try DesktopConfiguration(runtimeBundle: fixture.bundle, profileDirectory: alias.appendingPathComponent("new"))) { XCTAssertEqual($0 as? DesktopError, .symlinkPath) }
    }

    func testNavigationRemainsWithinExactLoopbackOrigin() throws {
        let policy = try LocalNavigationPolicy(baseURL: URL(string: "http://127.0.0.1:3053/app/")!)
        for suffix in ["/app/", "/app/#space", "/app/settings", "/install", "/native.js", "/graph.css", "/intake-logo.png"] {
            XCTAssertTrue(policy.allows(URL(string: "http://127.0.0.1:3053" + suffix)!), suffix)
        }
        for value in ["https://127.0.0.1:3053/app/", "http://localhost:3053/app/", "http://127.0.0.1:3054/app/", "http://user@127.0.0.1:3053/app/", "http://127.0.0.1:3053/app/?token=secret", "file:///private/tmp/a", "javascript:alert(1)", "http://127.0.0.1:3053/console/api/owner/intake/sources", "http://127.0.0.1:3053/app/%2e%2e/install", "http://127.0.0.1:3053/app/../install"] {
            XCTAssertFalse(policy.allows(URL(string: value)!), value)
        }
    }

    func testCredentialEligibilityOnlyRootPage() throws {
        let policy = try LocalNavigationPolicy(baseURL: URL(string: "http://127.0.0.1:3053/app/")!)
        XCTAssertTrue(policy.credentialEligibleURL(URL(string: "http://127.0.0.1:3053/app/#space")!))
        for path in ["/app/subpage", "/install", "/native.js", "/app/?x=1"] {
            XCTAssertFalse(policy.credentialEligibleURL(URL(string: "http://127.0.0.1:3053" + path)!))
        }
    }

    func testMalformedBaseURLsRejected() {
        for url in ["http://user:pass@127.0.0.1:3053/app/", "http://127.0.0.1/app/", "http://127.0.0.1:80/app/", "http://127.0.0.1:3053/install", "http://127.0.0.1:3053/app/#x", "http://127.0.0.1:3053/app/?x=1"] {
            XCTAssertThrowsError(try LocalNavigationPolicy(baseURL: URL(string: url)!))
        }
    }

    func testSnapshotDoctorFlattensAndNeverExposesRuntimeMessage() throws {
        let raw = #"{"status":"checked","bundle_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","runtime":{"status":"unresponsive","ready":false,"message":"DO_NOT_EXPOSE"}}"#
        let snapshot = try JSONDecoder().decode(RuntimeSnapshot.self, from: Data(raw.utf8))
        XCTAssertEqual(snapshot.status, "unresponsive")
        XCTAssertNotNil(snapshot.bundleID)
        XCTAssertFalse(snapshot.message?.contains("DO_NOT_EXPOSE") ?? true)
        XCTAssertNil(snapshot.workbenchURL)
    }

    func testSnapshotURLRejectsMismatchUserInfoAndQuery() throws {
        for url in ["http://127.0.0.1:3054/app/", "http://user@127.0.0.1:3053/app/", "http://127.0.0.1:3053/app/?secret=x", "http://127.0.0.1:3053/app/#x", "http://127.0.0.1:3053/install"] {
            let data = try JSONSerialization.data(withJSONObject: ["status": "running", "ready": true, "port": 3053, "url": url])
            XCTAssertNil(try JSONDecoder().decode(RuntimeSnapshot.self, from: data).workbenchURL)
        }
    }

    func testLaunchAndConfigureArgumentsAreNotShellStrings() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let client = RuntimeClient(configuration: try fixture.configuration())
        let launch = try client.arguments(for: .launch, maxCalls: nil)
        XCTAssertTrue(launch.contains("--no-open"))
        XCTAssertTrue(launch.contains(fixture.bundle.path))
        let configure = try client.arguments(for: .configureOn, maxCalls: 42)
        XCTAssertEqual(configure.suffix(4), ["--compiler", "on", "--max-calls", "42"])
        XCTAssertThrowsError(try client.arguments(for: .status, maxCalls: 3))
        XCTAssertThrowsError(try client.arguments(for: .configureOff, maxCalls: 0))
    }

    func testMissingProfileStatusDoesNotCreateDirectory() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try FileManager.default.removeItem(at: fixture.profile)
        let client = RuntimeClient(configuration: try fixture.configuration())
        XCTAssertThrowsError(try client.arguments(for: .status, maxCalls: nil)) { XCTAssertEqual($0 as? DesktopError, .profileMissing) }
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.profile.path))
        XCTAssertNoThrow(try client.arguments(for: .launch, maxCalls: nil))
    }

    func testRunningSnapshotUsesProfilePortInsteadOfNewProfileDefault() async throws {
        let fixture = try Fixture(port: 4317); defer { fixture.remove() }
        try fixture.respond(fixture.running())
        let client = RuntimeClient(configuration: try fixture.configuration())
        let result = try await client.perform(action: .status)
        XCTAssertEqual(result.workbenchURL?.port, 4317)
        let credential = try await client.readOwnerCredential()
        XCTAssertEqual(credential, String(repeating: "a", count: 64))
    }

    func testRunningSnapshotCannotSubstitutePortOrProfile() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        var wrong = fixture.running(); wrong["port"] = 4053; wrong["url"] = "http://127.0.0.1:4053/app/"
        try fixture.respond(wrong)
        do { _ = try await RuntimeClient(configuration: fixture.configuration()).perform(action: .status); XCTFail("substituted port accepted") }
        catch { XCTAssertEqual(error as? DesktopError, .invalidProfile) }
    }

    func testReadSettingsReflectsPersistedBudget() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let result = try await RuntimeClient(configuration: fixture.configuration()).readSettings()
        XCTAssertFalse(result.compilerEnabled)
        XCTAssertEqual(result.maximumCalls, 37)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: fixture.profile.appendingPathComponent("runtime.json").path)
        do { _ = try await RuntimeClient(configuration: fixture.configuration()).readSettings(); XCTFail("public settings accepted") }
        catch { XCTAssertEqual(error as? DesktopError, .invalidProfile) }
    }

    func testCredentialRequiresPrivateRegularFileAndExactHex() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let client = RuntimeClient(configuration: try fixture.configuration())
        XCTAssertEqual(try client.readCredentialFile(), String(repeating: "a", count: 64))
        let key = fixture.profile.appendingPathComponent("owner.key")
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: key.path)
        XCTAssertThrowsError(try client.readCredentialFile())
        try fixture.privateWrite(Data("sk-secret".utf8), to: key)
        XCTAssertThrowsError(try client.readCredentialFile())
        try fixture.privateWrite(Data(repeating: 97, count: 4097), to: key)
        XCTAssertThrowsError(try client.readCredentialFile())
        try FileManager.default.removeItem(at: key)
        try FileManager.default.createSymbolicLink(at: key, withDestinationURL: fixture.profile.appendingPathComponent("runtime.json"))
        XCTAssertThrowsError(try client.readCredentialFile())
    }

    func testCredentialNotReturnedBeforeVerifiedReadiness() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.respond(["status": "stopped", "ready": false, "profile_id": fixture.profileID])
        do { _ = try await RuntimeClient(configuration: fixture.configuration()).readOwnerCredential(); XCTFail("stopped runtime released credential") }
        catch { XCTAssertEqual(error as? DesktopError, .runtimeNotReady) }
    }

    func testCredentialRejectsNonregularFileWithoutBlocking() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let key = fixture.profile.appendingPathComponent("owner.key")
        try FileManager.default.removeItem(at: key)
        XCTAssertEqual(mkfifo(key.path, 0o600), 0)
        let began = Date()
        XCTAssertThrowsError(try RuntimeClient(configuration: fixture.configuration()).readCredentialFile())
        XCTAssertLessThan(Date().timeIntervalSince(began), 1)
    }

    func testCredentialRejectsPublicProfileDirectory() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: fixture.profile.path)
        XCTAssertThrowsError(try RuntimeClient(configuration: fixture.configuration()).readCredentialFile())
    }

    func testProcessErrorOnlyReturnsSafeCode() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        try fixture.script("import sys\nsys.stderr.write('{\"code\":\"port_in_use\",\"message\":\"SECRET_MUST_NOT_APPEAR\"}')\nsys.exit(1)")
        do { _ = try await RuntimeClient(configuration: fixture.configuration()).perform(action: .status); XCTFail("error was accepted") }
        catch { XCTAssertEqual(error as? DesktopError, .portInUse); XCTAssertFalse(error.localizedDescription.contains("SECRET")) }
    }

    func testBothOutputStreamsAreBounded() async throws {
        for descriptor in [1, 2] {
            let fixture = try Fixture(); defer { fixture.remove() }
            try fixture.script("import os, time\nos.write(\(descriptor), b'x'*70000)\ntime.sleep(10)")
            do { _ = try await RuntimeClient(configuration: fixture.configuration()).perform(action: .status); XCTFail("oversize output accepted") }
            catch { XCTAssertEqual(error as? DesktopError, .outputLimit) }
        }
    }

    func testTimeoutAndCancellationTerminateOnlyOwnedHelper() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let pidFile = fixture.root.appendingPathComponent("helper.pid")
        let quotedPath = "'" + pidFile.path.replacingOccurrences(of: "'", with: "'\\''") + "'"
        // Avoid timing the startup of a development interpreter in a process-
        // cancellation test. exec keeps the recorded PID on the owned child.
        try fixture.shell("printf '%s' \"$$\" > \(quotedPath)\nexec /bin/sleep 10")
        let client = RuntimeClient(configuration: try fixture.configuration(), timeout: 1.0)
        do { _ = try await client.perform(action: .status); XCTFail("timeout ignored") }
        catch { XCTAssertEqual(error as? DesktopError, .timedOut) }
        let pid = Int32(try String(contentsOf: pidFile).trimmingCharacters(in: .whitespacesAndNewlines))!
        XCTAssertEqual(kill(pid, 0), -1)
        try FileManager.default.removeItem(at: pidFile)
        let task = Task { try await RuntimeClient(configuration: fixture.configuration()).perform(action: .status) }
        defer { task.cancel() }
        var cancelledPID: Int32?
        for _ in 0..<100 {
            if let contents = try? String(contentsOf: pidFile),
               let pid = Int32(contents.trimmingCharacters(in: .whitespacesAndNewlines)) {
                cancelledPID = pid
                break
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let runningPID = try XCTUnwrap(cancelledPID)
        task.cancel()
        do { _ = try await task.value; XCTFail("cancellation ignored") }
        catch { XCTAssertEqual(error as? DesktopError, .cancelled) }
        XCTAssertEqual(kill(runningPID, 0), -1)
    }
}

private final class Fixture {
    let root: URL
    let bundle: URL
    let profile: URL
    let port: Int
    let profileID = "7e0a447b-a76f-4e57-8ab7-c1503c159ac0"

    init(port: Int = 3053) throws {
        root = URL(fileURLWithPath: "/private/tmp/nova-desktop-core-" + UUID().uuidString, isDirectory: true)
        bundle = root.appendingPathComponent("runtime bundle ; untouched", isDirectory: true)
        profile = root.appendingPathComponent("profile", isDirectory: true)
        self.port = port
        for directory in [root, bundle, bundle.appendingPathComponent("bin"), profile] {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        }
        let metadata: [String: Any] = ["schema_version": "nova.local-runtime.v0.1", "profile_id": profileID, "port": port, "compiler_enabled": false, "compiler_max_calls": 37]
        try privateWrite(JSONSerialization.data(withJSONObject: metadata), to: profile.appendingPathComponent("runtime.json"))
        try privateWrite(Data((String(repeating: "a", count: 64) + "\n").utf8), to: profile.appendingPathComponent("owner.key"))
        try script("print('{\"status\":\"stopped\",\"ready\":false}')")
    }

    func configuration() throws -> DesktopConfiguration { try DesktopConfiguration(runtimeBundle: bundle, profileDirectory: profile) }
    func privateWrite(_ data: Data, to url: URL) throws {
        try data.write(to: url)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }
    func script(_ value: String) throws {
        let path = bundle.appendingPathComponent("bin/nova-local")
        try privateWrite(Data(("#!/usr/bin/python3\n" + value).utf8), to: path)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: path.path)
    }
    func shell(_ value: String) throws {
        let path = bundle.appendingPathComponent("bin/nova-local")
        try privateWrite(Data(("#!/bin/sh\n" + value).utf8), to: path)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: path.path)
    }
    func respond(_ value: [String: Any]) throws {
        let data = try JSONSerialization.data(withJSONObject: value, options: .withoutEscapingSlashes)
        let literal = String(data: try JSONSerialization.data(withJSONObject: String(data: data, encoding: .utf8)!, options: [.fragmentsAllowed, .withoutEscapingSlashes]), encoding: .utf8)!
        try script("print(\(literal))")
    }
    func running() -> [String: Any] {
        ["status": "running", "ready": true, "profile_id": profileID, "instance_id": String(repeating: "b", count: 32), "port": port,
         "url": "http://127.0.0.1:\(port)/app/", "model_selection": "unconfigured", "compiler_available": false]
    }
    func remove() { try? FileManager.default.removeItem(at: root) }
}

final class RuntimeFeatureSettingsTests: XCTestCase {
    func testLegacySettingsDefaultOffAndExplicitBooleanIsStrict() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let client = RuntimeClient(configuration: try fixture.configuration())
        let old = try await client.readSettings()
        XCTAssertFalse(old.continuityEnabled)
        XCTAssertFalse(old.rankedRetrievalEnabled)
        let path = fixture.profile.appendingPathComponent("runtime.json")
        var metadata = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: path)) as? [String: Any])
        metadata["continuity_enabled"] = true
        metadata["ranked_retrieval_enabled"] = true
        try fixture.privateWrite(JSONSerialization.data(withJSONObject: metadata), to: path)
        let enabled = try await client.readSettings()
        XCTAssertTrue(enabled.continuityEnabled)
        XCTAssertTrue(enabled.rankedRetrievalEnabled)
        XCTAssertFalse(enabled.compilerEnabled)
        XCTAssertEqual(enabled.maximumCalls, 37)
        for key in ["continuity_enabled", "ranked_retrieval_enabled"] {
            for invalid: Any in [NSNull(), "on", 1] {
                metadata[key] = invalid
                try fixture.privateWrite(JSONSerialization.data(withJSONObject: metadata), to: path)
                do { _ = try await client.readSettings(); XCTFail("non-boolean runtime feature accepted") }
                catch { XCTAssertEqual(error as? DesktopError, .invalidProfile) }
            }
            metadata[key] = true
        }
    }

    func testFeatureArgumentsOnlyConfigureAndDoNotStartCompiler() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let client = RuntimeClient(configuration: try fixture.configuration())
        let on = try client.arguments(for: .configureOff, maxCalls: 37, continuityEnabled: true)
        XCTAssertEqual(Array(on.suffix(6)), ["--compiler", "off", "--max-calls", "37", "--continuity", "on"])
        let off = try client.arguments(for: .configureOff, maxCalls: nil, continuityEnabled: false)
        XCTAssertEqual(Array(off.suffix(2)), ["--continuity", "off"])
        XCTAssertFalse(try client.arguments(for: .configureOff, maxCalls: nil).contains("--continuity"))
        let ranked = try client.arguments(for: .configureOff, maxCalls: nil, rankedRetrievalEnabled: true)
        XCTAssertEqual(Array(ranked.suffix(4)), ["--compiler", "off", "--ranked-retrieval", "on"])
        XCTAssertFalse(try client.arguments(for: .configureOff, maxCalls: nil).contains("--ranked-retrieval"))
        for action in [RuntimeAction.status, .stop, .launch, .doctor, .recover] {
            XCTAssertThrowsError(try client.arguments(for: action, maxCalls: nil, continuityEnabled: true))
            XCTAssertThrowsError(try client.arguments(for: action, maxCalls: nil, rankedRetrievalEnabled: true))
        }
    }

    func testRequestedCapabilityMustMatchVerifiedRunningProfile() async throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        let path = fixture.profile.appendingPathComponent("runtime.json")
        var metadata = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: path)) as? [String: Any])
        let client = RuntimeClient(configuration: try fixture.configuration())
        for (savedKey, runningKey) in [("continuity_enabled", "continuity_available"),
                                       ("ranked_retrieval_enabled", "ranked_retrieval_available")] {
            metadata[savedKey] = true
            try fixture.privateWrite(JSONSerialization.data(withJSONObject: metadata), to: path)
            var running = fixture.running()
            for reported: Any? in [nil, false] {
                running[runningKey] = reported
                try fixture.respond(running)
                do { _ = try await client.readOwnerCredential(); XCTFail("credential released before requested capability matched") }
                catch { XCTAssertEqual(error as? DesktopError, .runtimeNotReady) }
            }
            running[runningKey] = true
            try fixture.respond(running)
            let ready = try await client.perform(action: .status)
            XCTAssertEqual(ready.status, "running")
            metadata[savedKey] = false
            try fixture.privateWrite(JSONSerialization.data(withJSONObject: metadata), to: path)
            XCTAssertThrowsError(try client.validate(ready))
        }
    }

    func testSnapshotBooleanAndNestedDoctorResponseAreStrict() throws {
        let fixture = try Fixture(); defer { fixture.remove() }
        var running = fixture.running()
        for key in ["continuity_available", "ranked_retrieval_available"] {
            for invalid: Any in [NSNull(), "true", 1] {
                running[key] = invalid
                XCTAssertThrowsError(try JSONDecoder().decode(RuntimeSnapshot.self, from: JSONSerialization.data(withJSONObject: running)))
            }
            running[key] = true
        }
        let nested = try JSONDecoder().decode(RuntimeSnapshot.self, from: JSONSerialization.data(withJSONObject: ["runtime": running]))
        XCTAssertEqual(nested.continuityAvailable, true)
        XCTAssertEqual(nested.rankedRetrievalAvailable, true)
    }
}
