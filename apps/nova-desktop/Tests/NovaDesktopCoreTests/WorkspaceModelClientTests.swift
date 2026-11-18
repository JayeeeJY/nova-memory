import XCTest
import Foundation
@testable import NovaDesktopCore

final class WorkspaceModelClientTests: XCTestCase {
    func testLoadUsesFreshOwnerAuthenticationOnlyOnExactConfigurationRoute() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state()))
        let state = try await fixture.client().load()
        XCTAssertEqual(state.configuration.revision, 1)
        XCTAssertEqual(state.configuration.location, .unconfigured)
        XCTAssertFalse(state.compilerAvailable)
        let requests = fixture.requests
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.httpMethod, "GET")
        XCTAssertEqual(requests.first?.url?.path, "/console/api/owner/intake/configuration")
        XCTAssertEqual(requests.first?.value(forHTTPHeaderField: "Authorization"), "Bearer " + fixture.key)
        XCTAssertEqual(requests.first?.value(forHTTPHeaderField: "Origin"), fixture.origin)
        XCTAssertEqual(requests.first?.value(forHTTPHeaderField: "X-Nova-Review"), "owner-review")
        XCTAssertNil(requests.first?.httpBody)
        XCTAssertEqual(fixture.authorizationCount, 1)
    }

    func testLocalSaveUsesCallerCASAndNoRemoteConsentOrCredentialFields() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        let draft = fixture.local(revision: 7)
        fixture.respond(.json(fixture.state(revision: 7)), .json(fixture.configuration(revision: 8, location: "local")))
        let saved = try await fixture.client().savePaused(selection: draft, expectedRevision: 7)
        XCTAssertEqual(saved.revision, 8)
        XCTAssertEqual(fixture.authorizationCount, 2)
        let requests = fixture.requests
        XCTAssertEqual(requests.map(\.httpMethod), ["GET", "POST"])
        let body = try XCTUnwrap(requests.last?.fixtureBody)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["expected_revision", "selection"])
        XCTAssertEqual(json["expected_revision"] as? Int, 7)
        let selection = try XCTUnwrap(json["selection"] as? [String: String])
        XCTAssertEqual(Set(selection.keys), ["location", "model_id", "endpoint"])
        XCTAssertEqual(selection["location"], "local")
        XCTAssertFalse(String(decoding: body, as: UTF8.self).contains(fixture.key))
    }

    func testClearSelectionSendsOnlyUnconfiguredLocation() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state(revision: 2)), .json(fixture.configuration(revision: 3)))
        let saved = try await fixture.client().savePaused(selection: .init(revision: 2, location: .unconfigured), expectedRevision: 2)
        XCTAssertEqual(saved.location, .unconfigured)
        let body = try XCTUnwrap(fixture.requests.last?.fixtureBody)
        let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(wire["selection"] as? [String: String], ["location": "unconfigured"])
    }

    func testProcessorMustPauseBeforeAnyConfigurationWrite() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state(compiler: true)))
        do { _ = try await fixture.client().savePaused(selection: fixture.local(), expectedRevision: 1); XCTFail("active processor accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .processorMustPause) }
        XCTAssertEqual(fixture.requests.map(\.httpMethod), ["GET"])
    }

    func testStaleRevisionIsNotReplacedByLatestRevisionOrRetried() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state(revision: 9)))
        do { _ = try await fixture.client().savePaused(selection: fixture.local(), expectedRevision: 1); XCTFail("stale draft accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .staleRevision) }
        XCTAssertEqual(fixture.requests.count, 1)
    }

    func testServerConflictNeverRetriesOrExposesServerErrorText() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state()), .json(["error": "SECRET_SERVER_DETAIL"], status: 409))
        do { _ = try await fixture.client().savePaused(selection: fixture.local(), expectedRevision: 1); XCTFail("conflict accepted") }
        catch {
            XCTAssertEqual(error as? ModelSetupError, .staleRevision)
            XCTAssertFalse(error.localizedDescription.contains("SECRET"))
        }
        XCTAssertEqual(fixture.requests.count, 2)
    }

    func testRuntimeIdentityChangeBetweenReadAndWritePreventsPOST() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state()))
        fixture.changeIdentityOnSecondAuthorization = true
        do { _ = try await fixture.client().savePaused(selection: fixture.local(), expectedRevision: 1); XCTFail("changed instance accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .runtimeNotReady) }
        XCTAssertEqual(fixture.requests.map(\.httpMethod), ["GET"])
    }

    func testUnavailableRuntimeOrInvalidCredentialCannotIssueHTTP() async throws {
        for authFailure in [true, false] {
            let fixture = OwnerHTTPFixture()
            defer { fixture.remove() }
            let client = WorkspaceModelClient(transport: fixture.transport) {
                if authFailure { throw DesktopError.runtimeNotReady }
                return WorkspaceOwnerAuthorization(baseURL: fixture.baseURL, credential: "bad-key", instanceID: "one")
            }
            do { _ = try await client.load(); XCTFail("bad readiness/auth accepted") }
            catch { XCTAssertEqual(error as? ModelSetupError, authFailure ? .runtimeNotReady : .authenticationFailed) }
            XCTAssertTrue(fixture.requests.isEmpty)
        }
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        let client = WorkspaceModelClient(transport: fixture.transport) {
            WorkspaceOwnerAuthorization(baseURL: URL(string: "http://external.example:4311/app/")!,
                                         credential: fixture.key, instanceID: "one")
        }
        do { _ = try await client.load(); XCTFail("foreign Owner URL accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .runtimeNotReady) }
        XCTAssertTrue(fixture.requests.isEmpty)
    }

    func testRemoteConfigurationCanBeReadButNeverWrittenByNativeSettings() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        var remote = fixture.configuration(revision: 4, location: "remote")
        remote["selection"] = ["location": "remote", "model_id": "remote-model", "endpoint": "https://model.example/v1",
                               "allow_selected_source": true, "api_key": "NOT_EXPOSED"]
        fixture.respond(.json(["configuration": remote, "compiler_available": false]))
        let state = try await fixture.client().load()
        XCTAssertEqual(state.configuration.location, .remote)
        XCTAssertEqual(state.configuration.modelID, "remote-model")
        do { _ = try await fixture.client().savePaused(selection: state.configuration, expectedRevision: 4); XCTFail("remote write accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .remoteConfigurationUnsupported) }
        XCTAssertEqual(fixture.requests.count, 1)
    }

    func testModelBoundsAndUnsafeEndpointsFailBeforeOwnerRequest() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        for model in ["", "  ", String(repeating: "a", count: 129), String(repeating: "界", count: 43), "bad\nname"] {
            let draft = WorkspaceModelConfiguration(revision: 1, location: .local, modelID: model, endpoint: "http://127.0.0.1:11434/v1")
            XCTAssertThrowsError(try draft.validateForWrite()) { XCTAssertEqual($0 as? ModelSetupError, .invalidModelID) }
            do { _ = try await fixture.client().savePaused(selection: draft, expectedRevision: 1); XCTFail("bad ID accepted") }
            catch { XCTAssertEqual(error as? ModelSetupError, .invalidModelID) }
        }
        for endpoint in ["https://remote.example/v1", "http://localhost:11434/v1", "http://user:pass@127.0.0.1:11434/v1", "http://127.0.0.1:11434/v1?q=secret"] {
            let draft = WorkspaceModelConfiguration(revision: 1, location: .local, modelID: "test", endpoint: endpoint)
            XCTAssertThrowsError(try draft.validateForWrite())
        }
        XCTAssertTrue(fixture.requests.isEmpty)
    }

    func testAuthenticationFailureMapsToFixedSafeMessage() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(["error": "SECRET_OWNER_TOKEN"], status: 403))
        do { _ = try await fixture.client().load(); XCTFail("forbidden accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .authenticationFailed); XCTAssertFalse(error.localizedDescription.contains("SECRET")) }
    }

    func testMalformedAndMismatchedSaveResponseAreRejected() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(.json(fixture.state()), .json(fixture.configuration(revision: 2)))
        do { _ = try await fixture.client().savePaused(selection: fixture.local(), expectedRevision: 1); XCTFail("unconfigured response matched local write") }
        catch { XCTAssertEqual(error as? ModelSetupError, .invalidResponse) }
        fixture.respond(.json(["configuration": ["revision": 0, "selection": ["location": "unconfigured"]], "compiler_available": false]))
        do { _ = try await fixture.client().load(); XCTFail("revision zero accepted") }
        catch { XCTAssertEqual(error as? ModelSetupError, .invalidResponse) }
    }

    func testTransportRejectsForeignOriginsBeforeSessionStart() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        for value in ["https://127.0.0.1:4311/v1/models", "http://localhost:4311/v1/models", "http://example.com:4311/v1/models", "http://user@127.0.0.1:4311/v1/models", "http://127.0.0.1:4311/v1/models?x=1"] {
            do { _ = try await fixture.transport.request(URLRequest(url: URL(string: value)!)); XCTFail("foreign address accepted") }
            catch { XCTAssertEqual(error as? LocalHTTPError, .invalidResponse) }
        }
        XCTAssertTrue(fixture.requests.isEmpty)
    }

    func testTransportBoundsDeclaredAndChunkedResponses() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        for plan in [OwnerResponsePlan(status: 200, headers: ["Content-Length": "9999"], chunks: [Data("x".utf8)]),
                     OwnerResponsePlan(status: 200, chunks: [Data(repeating: 120, count: 8), Data(repeating: 120, count: 9)])] {
            fixture.respond(plan)
            do { _ = try await fixture.transport.request(URLRequest(url: fixture.modelsURL), maximumBytes: 16); XCTFail("oversize response accepted") }
            catch { XCTAssertEqual(error as? LocalHTTPError, .responseTooLarge) }
        }
    }

    func testRedirectIsDeniedWithoutIssuingFollowUpRequest() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(OwnerResponsePlan(status: 302, headers: ["Location": "https://external.example/credential-sink"], chunks: []))
        do { _ = try await fixture.transport.request(URLRequest(url: fixture.modelsURL)); XCTFail("redirect accepted") }
        catch { XCTAssertEqual(error as? LocalHTTPError, .redirectDenied) }
        XCTAssertEqual(fixture.requests.count, 1)
    }

    func testTransportDeadlineAndCancellationAreBounded() async throws {
        let fixture = OwnerHTTPFixture()
        defer { fixture.remove() }
        fixture.respond(OwnerResponsePlan(status: 200, chunks: [], delay: 1))
        let began = Date()
        do { _ = try await fixture.transport.request(URLRequest(url: fixture.modelsURL), timeout: 0.05); XCTFail("deadline ignored") }
        catch { XCTAssertEqual(error as? LocalHTTPError, .timedOut) }
        XCTAssertLessThan(Date().timeIntervalSince(began), 0.8)
        fixture.respond(OwnerResponsePlan(status: 200, chunks: [], delay: 1))
        let task = Task { try await fixture.transport.request(URLRequest(url: fixture.modelsURL)) }
        try await Task.sleep(nanoseconds: 20_000_000)
        task.cancel()
        do { _ = try await task.value; XCTFail("cancellation ignored") }
        catch { XCTAssertEqual(error as? LocalHTTPError, .cancelled) }
    }
}

private struct OwnerResponsePlan {
    var status: Int
    var headers: [String: String] = [:]
    var chunks: [Data]
    var delay: TimeInterval = 0
    static func json(_ value: [String: Any], status: Int = 200) -> OwnerResponsePlan {
        OwnerResponsePlan(status: status, headers: ["Content-Type": "application/json"],
                          chunks: [try! JSONSerialization.data(withJSONObject: value)])
    }
}

private final class OwnerHTTPFixture: @unchecked Sendable {
    let port: Int
    let key = String(repeating: "b", count: 64)
    let transport: LocalHTTPTransport
    private let lock = NSLock()
    private var recordedRequests: [URLRequest] = []
    private var plans: [OwnerResponsePlan] = []
    private var authorizations = 0
    var changeIdentityOnSecondAuthorization = false
    var origin: String { "http://127.0.0.1:\(port)" }
    var baseURL: URL { URL(string: origin + "/app/")! }
    var modelsURL: URL { URL(string: origin + "/v1/models")! }
    var requests: [URLRequest] { lock.lock(); defer { lock.unlock() }; return recordedRequests }
    var authorizationCount: Int { lock.lock(); defer { lock.unlock() }; return authorizations }

    init() {
        port = OwnerFixtureURLProtocol.register()
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [OwnerFixtureURLProtocol.self]
        transport = LocalHTTPTransport(sessionConfiguration: configuration)
        OwnerFixtureURLProtocol.setHandler(port: port) { [weak self] request in
            guard let self else { return OwnerResponsePlan(status: 500, chunks: []) }
            self.lock.lock(); defer { self.lock.unlock() }
            var captured = request
            if let bytes = request.fixtureBody { captured.httpBody = bytes }
            self.recordedRequests.append(captured)
            return self.plans.isEmpty ? OwnerResponsePlan(status: 500, chunks: []) : self.plans.removeFirst()
        }
    }
    func remove() { OwnerFixtureURLProtocol.remove(port: port) }
    func respond(_ plans: OwnerResponsePlan...) { lock.lock(); self.plans.append(contentsOf: plans); lock.unlock() }
    func local(revision: UInt64 = 1) -> WorkspaceModelConfiguration {
        .init(revision: revision, location: .local, modelID: "fixture-model", endpoint: "http://127.0.0.1:11434/v1")
    }
    func configuration(revision: UInt64, location: String = "unconfigured") -> [String: Any] {
        var selection = ["location": location]
        if location != "unconfigured" { selection["model_id"] = "fixture-model"; selection["endpoint"] = "http://127.0.0.1:11434/v1" }
        return ["revision": revision, "selection": selection]
    }
    func state(revision: UInt64 = 1, compiler: Bool = false) -> [String: Any] {
        ["configuration": configuration(revision: revision), "compiler_available": compiler]
    }
    func client() -> WorkspaceModelClient {
        WorkspaceModelClient(transport: transport) { [self] in authorization() }
    }
    private func authorization() -> WorkspaceOwnerAuthorization {
        lock.lock(); defer { lock.unlock() }
        authorizations += 1
        let changed = changeIdentityOnSecondAuthorization && authorizations >= 2
        return WorkspaceOwnerAuthorization(baseURL: baseURL, credential: key, instanceID: changed ? "two" : "one")
    }
}

private final class OwnerFixtureURLProtocol: URLProtocol {
    private static let registryLock = NSLock()
    private static var nextPort = 45000
    private static var handlers: [Int: (URLRequest) -> OwnerResponsePlan] = [:]
    private let stopLock = NSLock()
    private var stopped = false

    static func register() -> Int { registryLock.lock(); defer { registryLock.unlock() }; nextPort += 1; return nextPort }
    static func setHandler(port: Int, handler: @escaping (URLRequest) -> OwnerResponsePlan) {
        registryLock.lock(); handlers[port] = handler; registryLock.unlock()
    }
    static func remove(port: Int) { registryLock.lock(); handlers[port] = nil; registryLock.unlock() }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.registryLock.lock()
        let handler = request.url?.port.flatMap { Self.handlers[$0] }
        Self.registryLock.unlock()
        guard let handler else { client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost)); return }
        let plan = handler(request)
        // URLProtocol deliberately has no Sendable conformance. A dispatch work
        // item owns this fixture callback; shared cancellation stays lock-guarded.
        let delivery = DispatchWorkItem { [self] in
            stopLock.lock(); let stopped = self.stopped; stopLock.unlock()
            guard !stopped else { return }
            let response = HTTPURLResponse(url: request.url!, statusCode: plan.status, httpVersion: "HTTP/1.1", headerFields: plan.headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            for chunk in plan.chunks { client?.urlProtocol(self, didLoad: chunk) }
            client?.urlProtocolDidFinishLoading(self)
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + plan.delay, execute: delivery)
    }
    override func stopLoading() { stopLock.lock(); stopped = true; stopLock.unlock() }
}

private extension URLRequest {
    var fixtureBody: Data? {
        if let httpBody { return httpBody }
        guard let stream = httpBodyStream else { return nil }
        stream.open(); defer { stream.close() }
        var bytes = [UInt8](repeating: 0, count: 2048)
        var result = Data()
        while stream.hasBytesAvailable {
            let count = stream.read(&bytes, maxLength: bytes.count)
            guard count > 0 else { break }
            result.append(contentsOf: bytes.prefix(count))
            if result.count > 65536 { break }
        }
        return result
    }
}
