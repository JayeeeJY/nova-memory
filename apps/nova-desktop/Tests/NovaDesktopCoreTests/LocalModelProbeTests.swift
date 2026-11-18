import XCTest
import Foundation
@testable import NovaDesktopCore

final class LocalModelProbeTests: XCTestCase {
    func testEndpointNormalizesOnlyLiteralLoopbackBases() throws {
        for (raw, base, models) in [
            ("http://127.0.0.1:1234/v1", "http://127.0.0.1:1234/v1/", "http://127.0.0.1:1234/v1/models"),
            ("http://127.0.0.1:80", "http://127.0.0.1:80/", "http://127.0.0.1:80/models"),
            ("http://[::1]:11434/v1/", "http://[::1]:11434/v1/", "http://[::1]:11434/v1/models"),
            ("http://127.0.0.1:65535/api-v1/model_2.0~stable/", "http://127.0.0.1:65535/api-v1/model_2.0~stable/", "http://127.0.0.1:65535/api-v1/model_2.0~stable/models")
        ] {
            let endpoint = try LocalModelEndpoint(raw)
            XCTAssertEqual(endpoint.endpoint.absoluteString, base)
            XCTAssertEqual(endpoint.modelsURL.absoluteString, models)
        }
    }

    func testEndpointRejectsAliasesAmbiguityAndUnsafePaths() {
        let rejected = [
            "", "http://localhost:1234/v1", "http://127.1:1234/v1", "http://127.0.0.2:1234/v1",
            "http://2130706433:1234/v1", "http://127.0.0.1.example:1234/v1", "http://[::ffff:127.0.0.1]:1234/v1",
            "https://127.0.0.1:1234/v1", "HTTP://127.0.0.1:1234/v1", "//127.0.0.1:1234/v1",
            "http://127.0.0.1/v1", "http://127.0.0.1:0/v1", "http://127.0.0.1:65536/v1",
            "http://127.0.0.1:-1/v1", "http://127.0.0.1:+1234/v1", "http://[::1]/v1",
            "http://user@127.0.0.1:1234/v1", "http://user:secret@127.0.0.1:1234/v1",
            "http://127.0.0.1:1234/v1?api_key=secret", "http://127.0.0.1:1234/v1#token",
            "http://127.0.0.1:1234/v1/../v2", "http://127.0.0.1:1234/./v1", "http://127.0.0.1:1234/v1//models",
            "http://127.0.0.1:1234/v1/%2e%2e", "http://127.0.0.1:1234/v1\\models",
            " http://127.0.0.1:1234/v1", "http://127.0.0.1:1234/v1\n", "http://127.0.0.1:1234/v1\0models",
            "http://127.0.0.1:1234/模型", "http://127.0.0.1:1234/model name", "http://127.0.0.1:1234/v1;models",
            "http://127.0.0.1:1234/" + String(repeating: "x", count: 512)
        ]
        for raw in rejected {
            XCTAssertThrowsError(try LocalModelEndpoint(raw), raw) {
                XCTAssertEqual($0 as? LocalModelEndpointError, .invalidEndpoint)
                XCTAssertFalse($0.localizedDescription.contains("secret"))
            }
        }
    }

    func testExplicitProbeSendsOnlyMetadataGETWithoutCredentialsOrBody() async throws {
        let observed = ProbeRequestRecorder()
        let fixture = try ProbeFixture { request, protocolInstance in
            observed.record(request)
            protocolInstance.respond(status: 200, data: Data(#"{"data":[{"id":"synthetic-setup-fixture"}]}"#.utf8))
        }
        defer { fixture.remove() }
        XCTAssertNil(observed.request)
        let result = await fixture.probe.check(endpoint: fixture.endpoint, selectedModelID: "synthetic-setup-fixture")
        let request = try XCTUnwrap(observed.request)
        XCTAssertEqual(request.url, fixture.endpoint.modelsURL)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertNil(request.httpBody)
        XCTAssertNil(request.httpBodyStream)
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        XCTAssertNil(request.value(forHTTPHeaderField: "Cookie"))
        XCTAssertEqual(request.value(forHTTPHeaderField: "Accept"), "application/json")
        XCTAssertEqual(result.status, .available)
        XCTAssertEqual(result.selectedModelFound, true)
        XCTAssertTrue(result.detail.contains("尚未"))
        XCTAssertLessThan(abs(result.checkedAt.timeIntervalSinceNow), 5)
    }

    func testCatalogIsSortedDeduplicatedAndAllowsCommonLocalModelIDs() async throws {
        let fixture = try ProbeFixture.catalog(["qwen/qwen3:8b", "model-z", "model-a", "model-z", "org/model@q4_k+test~1"])
        defer { fixture.remove() }
        let result = await fixture.probe.check(endpoint: fixture.endpoint)
        XCTAssertEqual(result.status, .available)
        XCTAssertEqual(result.modelIDs, ["model-a", "model-z", "org/model@q4_k+test~1", "qwen/qwen3:8b"])
        XCTAssertNil(result.selectedModelFound)
    }

    func testMissingSelectedModelDoesNotClaimInferenceFailure() async throws {
        let fixture = try ProbeFixture.catalog(["model-a"])
        defer { fixture.remove() }
        let result = await fixture.probe.check(endpoint: fixture.endpoint, selectedModelID: "model-b")
        XCTAssertEqual(result.status, .modelNotListed)
        XCTAssertEqual(result.selectedModelFound, false)
        XCTAssertEqual(result.modelIDs, ["model-a"])
        XCTAssertTrue(result.detail.contains("不代表"))
        XCTAssertTrue(result.detail.contains("尚未执行推理"))
        XCTAssertFalse(result.detail.contains("model-b"))
    }

    func testEmptyCatalogHasIndependentStatus() async throws {
        let fixture = try ProbeFixture.catalog([])
        defer { fixture.remove() }
        let result = await fixture.probe.check(endpoint: fixture.endpoint, selectedModelID: "model-a")
        XCTAssertEqual(result.status, .emptyCatalog)
        XCTAssertEqual(result.modelIDs, [])
        XCTAssertEqual(result.selectedModelFound, false)
    }

    func testMalformedSchemasAndUnsafeModelIDsAreRejectedAsWholeCatalog() async throws {
        let rawResponses = [
            #"{}"#, #"[]"#, #"{"data":null}"#, #"{"data":["model"]}"#,
            #"{"data":[{}]}"#, #"{"data":[{"id":1}]}"#, #"{"data":[{"id":null}]}"#,
            #"{"data":[{"id":"valid"},{"id":"ignore\nuser"}]}"#,
            #"{"data":[{"id":"<script>alert(1)</script>"}]}"#,
            #"{"data":[{"id":"model name"}]}"#, #"{"data":[{"id":"模型"}]}"#,
            #"{"data":[{"id":""}]}"#, #"{"data":[{"id":"model\\name"}]}"#,
            "{\"data\":[{\"id\":\"" + String(repeating: "a", count: 129) + "\"}]}"
        ]
        for raw in rawResponses {
            let fixture = try ProbeFixture { _, stub in stub.respond(status: 200, data: Data(raw.utf8)) }
            let result = await fixture.probe.check(endpoint: fixture.endpoint)
            fixture.remove()
            XCTAssertEqual(result.status, .invalidResponse, raw)
            XCTAssertTrue(result.modelIDs.isEmpty)
            XCTAssertNil(result.selectedModelFound)
            XCTAssertFalse(result.detail.contains("script"))
        }
    }

    func testCatalogEntryAndModelIDLimitsAreEnforced() async throws {
        let boundary = try ProbeFixture.catalog((0..<128).map { "model-\($0)" })
        defer { boundary.remove() }
        let valid = await boundary.probe.check(endpoint: boundary.endpoint)
        XCTAssertEqual(valid.status, .available)
        XCTAssertEqual(valid.modelIDs.count, 128)
        let tooMany = try ProbeFixture.catalog(Array(repeating: "model-a", count: 129))
        defer { tooMany.remove() }
        let over = await tooMany.probe.check(endpoint: tooMany.endpoint)
        XCTAssertEqual(over.status, .responseTooLarge)
        XCTAssertTrue(over.modelIDs.isEmpty)
        let longID = String(repeating: "a", count: 128)
        let lastValid = try ProbeFixture.catalog([longID])
        defer { lastValid.remove() }
        let exact = await lastValid.probe.check(endpoint: lastValid.endpoint)
        XCTAssertEqual(exact.modelIDs, [longID])
    }

    func testOversizeResponseIsStoppedForDeclaredAndStreamingBodies() async throws {
        for declared in [false, true] {
            let fixture = try ProbeFixture { _, stub in
                stub.respond(status: 200, data: Data(repeating: 65, count: 65_537),
                             headers: declared ? ["Content-Length": "65537"] : [:])
            }
            let result = await fixture.probe.check(endpoint: fixture.endpoint)
            fixture.remove()
            XCTAssertEqual(result.status, .responseTooLarge)
            XCTAssertTrue(result.modelIDs.isEmpty)
        }
    }

    func testHTTPFailuresUseFixedMessagesWithoutServiceText() async throws {
        let cases: [(Int, ModelProbeStatus)] = [
            (401, .authenticationRequired), (403, .authenticationRequired), (404, .notFound),
            (302, .redirectDenied), (307, .redirectDenied), (429, .serviceError), (500, .serviceError), (201, .serviceError)
        ]
        for (status, expected) in cases {
            let fixture = try ProbeFixture { _, stub in
                stub.respond(status: status, data: Data("SECRET_IGNORE_PREVIOUS_INSTRUCTIONS".utf8),
                             headers: ["Location": "https://external.invalid/models"])
            }
            let result = await fixture.probe.check(endpoint: fixture.endpoint)
            fixture.remove()
            XCTAssertEqual(result.status, expected)
            XCTAssertFalse(result.title.contains("SECRET"))
            XCTAssertFalse(result.detail.contains("SECRET"))
            XCTAssertTrue(result.modelIDs.isEmpty)
            XCTAssertNil(result.selectedModelFound)
        }
    }

    func testTransportFailuresMapToSafeStatuses() async throws {
        let cases: [(URLError.Code, ModelProbeStatus)] = [(.timedOut, .timedOut), (.cannotConnectToHost, .unreachable)]
        for (code, expected) in cases {
            let fixture = try ProbeFixture { _, stub in stub.fail(URLError(code)) }
            let result = await fixture.probe.check(endpoint: fixture.endpoint)
            fixture.remove()
            XCTAssertEqual(result.status, expected)
            XCTAssertTrue(result.modelIDs.isEmpty)
        }
    }

    func testNonHTTPAndSubstitutedResponseURLsAreRejected() async throws {
        let nonHTTP = try ProbeFixture { request, stub in
            stub.client?.urlProtocol(stub, didReceive: URLResponse(url: request.url!, mimeType: "application/json", expectedContentLength: 0, textEncodingName: nil), cacheStoragePolicy: .notAllowed)
            stub.client?.urlProtocolDidFinishLoading(stub)
        }
        defer { nonHTTP.remove() }
        let first = await nonHTTP.probe.check(endpoint: nonHTTP.endpoint)
        XCTAssertEqual(first.status, .invalidResponse)
        let substituted = try ProbeFixture { _, stub in
            stub.respond(status: 200, data: Data(#"{"data":[{"id":"model"}]}"#.utf8),
                         responseURL: URL(string: "http://127.0.0.1:12345/different/models")!)
        }
        defer { substituted.remove() }
        let second = await substituted.probe.check(endpoint: substituted.endpoint)
        XCTAssertEqual(second.status, .invalidResponse)
        XCTAssertTrue(second.modelIDs.isEmpty)
    }
}

private final class ProbeRequestRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: URLRequest?
    var request: URLRequest? {
        lock.lock(); defer { lock.unlock() }
        return stored
    }
    func record(_ request: URLRequest) {
        lock.lock(); defer { lock.unlock() }
        stored = request
    }
}

private struct ProbeFixture {
    let endpoint: LocalModelEndpoint
    let probe: LocalModelProbe

    init(handler: @escaping (URLRequest, ProbeURLProtocol) -> Void) throws {
        endpoint = try LocalModelEndpoint("http://127.0.0.1:61381/" + UUID().uuidString + "/v1")
        ProbeURLProtocol.register(url: endpoint.modelsURL, handler: handler)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ProbeURLProtocol.self]
        probe = LocalModelProbe(transport: LocalHTTPTransport(sessionConfiguration: configuration))
    }

    static func catalog(_ ids: [String]) throws -> ProbeFixture {
        let data = try JSONSerialization.data(withJSONObject: ["data": ids.map { ["id": $0] }])
        return try ProbeFixture { _, stub in stub.respond(status: 200, data: data) }
    }

    func remove() { ProbeURLProtocol.unregister(url: endpoint.modelsURL) }
}

private final class ProbeURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var handlers: [URL: (URLRequest, ProbeURLProtocol) -> Void] = [:]

    static func register(url: URL, handler: @escaping (URLRequest, ProbeURLProtocol) -> Void) {
        lock.lock(); defer { lock.unlock() }
        handlers[url] = handler
    }

    static func unregister(url: URL) {
        lock.lock(); defer { lock.unlock() }
        handlers.removeValue(forKey: url)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lock.lock()
        let handler = request.url.flatMap { Self.handlers[$0] }
        Self.lock.unlock()
        if let handler { handler(request, self) }
        else { fail(URLError(.unsupportedURL)) }
    }
    override func stopLoading() {}

    func respond(status: Int, data: Data, headers: [String: String] = [:], responseURL: URL? = nil) {
        let response = HTTPURLResponse(url: responseURL ?? request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }

    func fail(_ error: Error) { client?.urlProtocol(self, didFailWithError: error) }
}
