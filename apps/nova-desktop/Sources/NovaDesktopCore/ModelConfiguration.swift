import Foundation

public enum ModelLocation: String, Codable, Sendable {
    case unconfigured, local, remote
}

public enum ModelSetupError: String, Error, LocalizedError, Equatable, Sendable {
    case invalidConfiguration, invalidModelID, invalidEndpoint
    case remoteConfigurationUnsupported, staleRevision, processorMustPause
    case runtimeNotReady, authenticationFailed, responseTooLarge, timedOut
    case unavailable, invalidResponse, redirectDenied, cancelled

    public var errorDescription: String? {
        switch self {
        case .invalidConfiguration: return "模型配置无效，请重新检查所选配置。"
        case .invalidModelID: return "模型名称不能为空，最多 128 字节，且不能包含控制字符。"
        case .invalidEndpoint: return "请填写带端口的本机模型 HTTP 地址。"
        case .remoteConfigurationUnsupported: return "此设置暂只支持本机模型，已有远程配置可在工作台中查看。"
        case .staleRevision: return "模型配置已发生变化，请刷新后重新确认。"
        case .processorMustPause: return "请先暂停自动整理，再保存模型配置。"
        case .runtimeNotReady: return "本机工作台尚未通过运行状态核对。"
        case .authenticationFailed: return "本机工作台未接受 Owner 凭据，请重新检查运行状态。"
        case .responseTooLarge: return "本机服务返回内容超过限制，操作已中止。"
        case .timedOut: return "本机服务等待超时，请检查服务后重试。"
        case .unavailable: return "无法连接本机服务，请检查运行状态。"
        case .invalidResponse: return "本机服务未返回有效的模型配置。"
        case .redirectDenied: return "本机服务返回了重定向，操作已中止。"
        case .cancelled: return "模型设置操作已取消，请重新检查状态。"
        }
    }
}

/// Only the non-secret projection needed by native settings. Remote consent and
/// provider credentials are deliberately not retained or available for writes.
public struct WorkspaceModelConfiguration: Equatable, Sendable, Decodable {
    public let revision: UInt64
    public let location: ModelLocation
    public let modelID: String?
    public let endpoint: String?

    public init(revision: UInt64, location: ModelLocation, modelID: String? = nil, endpoint: String? = nil) {
        self.revision = revision
        self.location = location
        self.modelID = modelID
        self.endpoint = endpoint
    }

    private enum CodingKeys: String, CodingKey { case revision, selection }
    private enum SelectionKeys: String, CodingKey {
        case location, endpoint
        case modelID = "model_id"
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        revision = try values.decode(UInt64.self, forKey: .revision)
        let selection = try values.nestedContainer(keyedBy: SelectionKeys.self, forKey: .selection)
        location = try selection.decode(ModelLocation.self, forKey: .location)
        modelID = try selection.decodeIfPresent(String.self, forKey: .modelID)
        endpoint = try selection.decodeIfPresent(String.self, forKey: .endpoint)
        try validateReading()
    }

    func validateReading() throws {
        guard revision > 0 else { throw ModelSetupError.invalidConfiguration }
        if location == .unconfigured {
            guard modelID == nil, endpoint == nil else { throw ModelSetupError.invalidConfiguration }
            return
        }
        try validateModelID()
        guard let endpoint, endpoint.utf8.count <= 512,
              !endpoint.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              let parts = URLComponents(string: endpoint), let scheme = parts.scheme,
              ["http", "https"].contains(scheme), parts.host?.isEmpty == false,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil else {
            throw ModelSetupError.invalidEndpoint
        }
        if location == .local { _ = try localEndpoint() }
    }

    public func validateForWrite() throws {
        guard revision > 0 else { throw ModelSetupError.invalidConfiguration }
        switch location {
        case .remote: throw ModelSetupError.remoteConfigurationUnsupported
        case .unconfigured:
            guard modelID == nil, endpoint == nil else { throw ModelSetupError.invalidConfiguration }
        case .local:
            try validateModelID()
            _ = try localEndpoint()
        }
    }

    func validateWriting() throws { try validateForWrite() }

    private func validateModelID() throws {
        guard let modelID, !modelID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              modelID.utf8.count <= 128,
              !modelID.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw ModelSetupError.invalidModelID
        }
    }

    private func localEndpoint() throws -> LocalModelEndpoint {
        guard let endpoint else { throw ModelSetupError.invalidEndpoint }
        do { return try LocalModelEndpoint(endpoint) }
        catch { throw ModelSetupError.invalidEndpoint }
    }
}

public struct WorkspaceModelState: Equatable, Sendable, Decodable {
    public let configuration: WorkspaceModelConfiguration
    public let compilerAvailable: Bool

    public init(configuration: WorkspaceModelConfiguration, compilerAvailable: Bool) {
        self.configuration = configuration
        self.compilerAvailable = compilerAvailable
    }

    private enum CodingKeys: String, CodingKey {
        case configuration
        case compilerAvailable = "compiler_available"
    }
}
