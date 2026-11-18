import Foundation

/// A local OpenAI-compatible API base. The base always ends in `/`;
/// `/models` and `/chat/completions` are appended to this same base.
/// Only literal loopback addresses are accepted, so validation never resolves DNS.
public struct LocalModelEndpoint: Equatable, Sendable {
    public let endpoint: URL
    public let modelsURL: URL

    public init(_ raw: String) throws {
        guard !raw.isEmpty, raw.utf8.count <= 512,
              raw.utf8.allSatisfy({ (33...126).contains($0) }),
              !raw.contains("%"), !raw.contains("\\"),
              !raw.contains("?"), !raw.contains("#"),
              raw.hasPrefix("http://") else { throw LocalModelEndpointError.invalidEndpoint }

        let remainder = String(raw.dropFirst("http://".count))
        let split = remainder.firstIndex(of: "/")
        let authority = split.map { String(remainder[..<$0]) } ?? remainder
        let path = split.map { String(remainder[$0...]) } ?? ""
        let host: String
        let portText: String
        if authority.hasPrefix("127.0.0.1:") {
            host = "127.0.0.1"
            portText = String(authority.dropFirst("127.0.0.1:".count))
        } else if authority.hasPrefix("[::1]:") {
            host = "[::1]"
            portText = String(authority.dropFirst("[::1]:".count))
        } else {
            throw LocalModelEndpointError.invalidEndpoint
        }
        guard !portText.isEmpty, portText.utf8.allSatisfy({ (48...57).contains($0) }),
              let port = Int(portText), (1...65535).contains(port) else {
            throw LocalModelEndpointError.invalidEndpoint
        }

        var segments = path.split(separator: "/", omittingEmptySubsequences: false)
        if !segments.isEmpty { segments.removeFirst() }
        if segments.last?.isEmpty == true { segments.removeLast() }
        guard segments.allSatisfy({ segment in
            !segment.isEmpty && segment != "." && segment != ".." &&
            segment.utf8.allSatisfy(Self.safePathByte)
        }) else { throw LocalModelEndpointError.invalidEndpoint }
        let basePath = segments.isEmpty ? "/" : "/" + segments.joined(separator: "/") + "/"
        guard let base = URL(string: "http://\(host):\(port)\(basePath)"),
              let models = URL(string: "models", relativeTo: base)?.absoluteURL else {
            throw LocalModelEndpointError.invalidEndpoint
        }
        endpoint = base
        modelsURL = models
    }

    private static func safePathByte(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) ||
        (97...122).contains(byte) || [45, 46, 95, 126].contains(byte)
    }
}

public enum LocalModelEndpointError: Error, Equatable, LocalizedError, Sendable {
    case invalidEndpoint

    public var errorDescription: String? {
        "请输入带明确端口的本机 HTTP 地址，例如 http://127.0.0.1:1234/v1。"
    }
}

public enum ModelProbeStatus: String, Equatable, Sendable {
    case available
    case modelNotListed
    case emptyCatalog
    case authenticationRequired
    case notFound
    case serviceError
    case unreachable
    case timedOut
    case redirectDenied
    case invalidResponse
    case responseTooLarge
}

public struct ModelProbeResult: Equatable, Sendable {
    public let status: ModelProbeStatus
    public let modelIDs: [String]
    public let selectedModelFound: Bool?
    public let checkedAt: Date
    public let title: String
    public let detail: String

    init(status: ModelProbeStatus, modelIDs: [String] = [], selectedModelFound: Bool? = nil, checkedAt: Date = Date()) {
        self.status = status
        self.modelIDs = modelIDs
        self.selectedModelFound = selectedModelFound
        self.checkedAt = checkedAt
        switch status {
        case .available:
            title = "已获取本机模型目录"
            detail = "只检查了模型目录元数据，尚未发送材料或执行模型推理。"
        case .modelNotListed:
            title = "目录中未列出所填模型"
            detail = "本机服务可返回模型目录；部分服务不会完整列出可用模型。这不代表所填模型无法使用，尚未执行推理。"
        case .emptyCatalog:
            title = "模型目录为空"
            detail = "请在本机模型服务中加载模型；本次只读取目录元数据，尚未执行推理。"
        case .authenticationRequired:
            title = "本机服务要求认证"
            detail = "本次目录检查不发送认证信息。请检查本机服务的访问设置，或使用支持无认证本机接口的服务。"
        case .notFound:
            title = "未找到模型目录接口"
            detail = "请核对兼容 API 的基础地址；检查会在基础地址后附加 /models。未执行推理。"
        case .serviceError:
            title = "本机服务返回错误"
            detail = "请检查模型服务的运行状态与日志，再重试目录检查。服务响应内容未被展示或保存。"
        case .unreachable:
            title = "无法连接本机模型服务"
            detail = "请先启动模型服务，并核对本机地址和端口。未发送记忆材料。"
        case .timedOut:
            title = "模型目录检查超时"
            detail = "本机服务未在 5 秒内完成目录响应。请检查服务状态后重试。未执行推理。"
        case .redirectDenied:
            title = "已阻止模型接口跳转"
            detail = "目录检查仅访问指定本机接口，不跟随重定向。请填写服务的直接基础地址。"
        case .invalidResponse:
            title = "无法识别模型目录"
            detail = "服务需返回受支持的模型目录格式。请核对兼容 API 地址；本次结果不代表模型的推理能力。"
        case .responseTooLarge:
            title = "模型目录响应过大"
            detail = "目录检查最多接收 64 KiB、128 条模型记录。请缩小本机服务返回的目录后重试。"
        }
    }
}

/// An explicit, bounded metadata-only check. This type never sends a credential,
/// memory material, or a completion request to the model service.
public struct LocalModelProbe: Sendable {
    private let transport: LocalHTTPTransport

    public init() {
        transport = LocalHTTPTransport()
    }

    init(transport: LocalHTTPTransport) {
        self.transport = transport
    }

    public func check(endpoint: LocalModelEndpoint, selectedModelID: String? = nil) async -> ModelProbeResult {
        var request = URLRequest(url: endpoint.modelsURL)
        request.httpMethod = "GET"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.cachePolicy = .reloadIgnoringLocalCacheData
        do {
            let response = try await transport.request(request, maximumBytes: 65_536, timeout: 5)
            switch response.statusCode {
            case 200:
                return parseCatalog(response.data, selectedModelID: selectedModelID)
            case 300...399:
                return ModelProbeResult(status: .redirectDenied)
            case 401, 403:
                return ModelProbeResult(status: .authenticationRequired)
            case 404:
                return ModelProbeResult(status: .notFound)
            default:
                return ModelProbeResult(status: .serviceError)
            }
        } catch let error as LocalHTTPError {
            let status: ModelProbeStatus
            switch error {
            case .unavailable, .cancelled: status = .unreachable
            case .timedOut: status = .timedOut
            case .redirectDenied: status = .redirectDenied
            case .responseTooLarge: status = .responseTooLarge
            case .invalidResponse: status = .invalidResponse
            }
            return ModelProbeResult(status: status)
        } catch {
            return ModelProbeResult(status: .unreachable)
        }
    }

    private func parseCatalog(_ data: Data, selectedModelID: String?) -> ModelProbeResult {
        guard data.count <= 65_536 else { return ModelProbeResult(status: .responseTooLarge) }
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let entries = root["data"] as? [[String: Any]] else {
            return ModelProbeResult(status: .invalidResponse)
        }
        guard entries.count <= 128 else { return ModelProbeResult(status: .responseTooLarge) }
        var ids = Set<String>()
        for entry in entries {
            guard let id = entry["id"] as? String, Self.validModelID(id) else {
                return ModelProbeResult(status: .invalidResponse)
            }
            ids.insert(id)
        }
        let modelIDs = ids.sorted()
        let selected = selectedModelID.flatMap { $0.isEmpty ? nil : $0 }
        let selectedFound = selected.map { ids.contains($0) }
        let status: ModelProbeStatus = modelIDs.isEmpty ? .emptyCatalog :
            (selectedFound == false ? .modelNotListed : .available)
        return ModelProbeResult(status: status, modelIDs: modelIDs, selectedModelFound: selectedFound)
    }

    private static func validModelID(_ id: String) -> Bool {
        guard !id.isEmpty, id.utf8.count <= 128 else { return false }
        return id.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) ||
            (97...122).contains(byte) || [43, 45, 46, 47, 58, 64, 95, 126].contains(byte)
        }
    }
}
