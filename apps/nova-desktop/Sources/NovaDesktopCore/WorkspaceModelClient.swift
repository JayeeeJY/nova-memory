import Foundation

/// This client talks exclusively to the authenticated local Owner API. Model
/// probes use a separate client and never receive this authorization context.
public struct WorkspaceModelClient: Sendable {
    private let authorize: @Sendable () async throws -> WorkspaceOwnerAuthorization
    private let transport: LocalHTTPTransport

    public init(runtime: RuntimeClient) {
        self.transport = LocalHTTPTransport()
        self.authorize = {
            do {
                let before = try await runtime.perform(action: .status)
                guard let baseURL = before.workbenchURL, before.ready == true,
                      let instanceID = before.instanceID else {
                    throw ModelSetupError.runtimeNotReady
                }
                let credential = try await runtime.readOwnerCredential()
                let after = try await runtime.perform(action: .status)
                guard after.ready == true, after.workbenchURL == baseURL,
                      before.profileID == after.profileID, instanceID == after.instanceID else {
                    throw ModelSetupError.runtimeNotReady
                }
                return WorkspaceOwnerAuthorization(baseURL: baseURL, credential: credential,
                                                   instanceID: instanceID)
            } catch is CancellationError { throw ModelSetupError.cancelled }
            catch let error as ModelSetupError { throw error }
            catch let error as DesktopError where error == .cancelled { throw ModelSetupError.cancelled }
            catch { throw ModelSetupError.runtimeNotReady }
        }
    }

    // Fixtures inject the readiness/key boundary, never a real user Vault.
    init(transport: LocalHTTPTransport, authorize: @escaping @Sendable () async throws -> WorkspaceOwnerAuthorization) {
        self.transport = transport
        self.authorize = authorize
    }

    public func load() async throws -> WorkspaceModelState {
        let authorization = try await authorizeSafely()
        return try await load(authorization: authorization)
    }

    public func savePaused(selection: WorkspaceModelConfiguration, expectedRevision: UInt64) async throws -> WorkspaceModelConfiguration {
        try selection.validateWriting()
        guard expectedRevision > 0, expectedRevision < UInt64.max else { throw ModelSetupError.invalidConfiguration }
        let before = try await authorizeSafely()
        let current = try await load(authorization: before)
        guard !current.compilerAvailable else { throw ModelSetupError.processorMustPause }
        guard current.configuration.revision == expectedRevision else { throw ModelSetupError.staleRevision }
        let fresh = try await authorizeSafely()
        guard fresh.baseURL == before.baseURL, fresh.instanceID == before.instanceID else {
            throw ModelSetupError.runtimeNotReady
        }
        var request = try ownerRequest(authorization: fresh, method: "POST")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(ConfigurationWrite(expectedRevision: expectedRevision, selection: selection))
        let response = try await send(request)
        let saved: WorkspaceModelConfiguration = try decode(response.data)
        guard saved.revision == expectedRevision + 1,
              saved.location == selection.location,
              saved.modelID == selection.modelID, saved.endpoint == selection.endpoint else {
            throw ModelSetupError.invalidResponse
        }
        return saved
    }

    private func authorizeSafely() async throws -> WorkspaceOwnerAuthorization {
        do {
            let value = try await authorize()
            _ = try LocalNavigationPolicy(baseURL: value.baseURL)
            guard value.credential.utf8.count == 64,
                  value.credential.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw ModelSetupError.authenticationFailed
            }
            return value
        } catch is CancellationError { throw ModelSetupError.cancelled }
        catch let error as ModelSetupError { throw error }
        catch { throw ModelSetupError.runtimeNotReady }
    }

    private func load(authorization: WorkspaceOwnerAuthorization) async throws -> WorkspaceModelState {
        let request = try ownerRequest(authorization: authorization, method: "GET")
        let response = try await send(request)
        return try decode(response.data)
    }

    private func ownerRequest(authorization: WorkspaceOwnerAuthorization, method: String) throws -> URLRequest {
        guard var parts = URLComponents(url: authorization.baseURL, resolvingAgainstBaseURL: false) else {
            throw ModelSetupError.runtimeNotReady
        }
        parts.path = ""
        guard let origin = parts.url?.absoluteString else { throw ModelSetupError.runtimeNotReady }
        parts.path = "/console/api/owner/intake/configuration"
        guard let url = parts.url else { throw ModelSetupError.runtimeNotReady }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("Bearer " + authorization.credential, forHTTPHeaderField: "Authorization")
        request.setValue("owner-review", forHTTPHeaderField: "X-Nova-Review")
        request.setValue(origin, forHTTPHeaderField: "Origin")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }

    private func send(_ request: URLRequest) async throws -> LocalHTTPResponse {
        let response: LocalHTTPResponse
        do { response = try await transport.request(request) }
        catch let error as LocalHTTPError {
            switch error {
            case .unavailable: throw ModelSetupError.unavailable
            case .timedOut: throw ModelSetupError.timedOut
            case .redirectDenied: throw ModelSetupError.redirectDenied
            case .responseTooLarge: throw ModelSetupError.responseTooLarge
            case .invalidResponse: throw ModelSetupError.invalidResponse
            case .cancelled: throw ModelSetupError.cancelled
            }
        } catch is CancellationError { throw ModelSetupError.cancelled }
        catch { throw ModelSetupError.unavailable }
        switch response.statusCode {
        case 200...299: return response
        case 300...399: throw ModelSetupError.redirectDenied
        case 401, 403: throw ModelSetupError.authenticationFailed
        case 409: throw ModelSetupError.staleRevision
        case 400, 422: throw ModelSetupError.invalidConfiguration
        case 500...599: throw ModelSetupError.unavailable
        default: throw ModelSetupError.invalidResponse
        }
    }

    private func decode<T: Decodable>(_ data: Data) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { throw ModelSetupError.invalidResponse }
    }
}

struct WorkspaceOwnerAuthorization: Sendable {
    let baseURL: URL
    let credential: String
    let instanceID: String?
}

private struct ConfigurationWrite: Encodable {
    let expectedRevision: UInt64
    let selection: WorkspaceModelConfiguration

    private enum CodingKeys: String, CodingKey {
        case expectedRevision = "expected_revision", selection
    }
    private enum SelectionKeys: String, CodingKey {
        case location, endpoint
        case modelID = "model_id"
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(expectedRevision, forKey: .expectedRevision)
        var wire = values.nestedContainer(keyedBy: SelectionKeys.self, forKey: .selection)
        try wire.encode(selection.location, forKey: .location)
        if selection.location == .local {
            try wire.encode(selection.modelID, forKey: .modelID)
            try wire.encode(selection.endpoint, forKey: .endpoint)
        }
    }
}
