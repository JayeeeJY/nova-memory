import Foundation
import Darwin

public enum DesktopError: String, Error, LocalizedError, Sendable {
    case invalidArguments = "invalid_arguments"
    case invalidPath = "invalid_path"
    case symlinkPath = "symlink_path"
    case invalidPort = "invalid_port"
    case profileMissing = "profile_missing"
    case invalidProfile = "invalid_profile"
    case invalidCredential = "invalid_credential"
    case runtimeUnavailable = "runtime_unavailable"
    case runtimeNotReady = "runtime_not_ready"
    case runtimeBusy = "runtime_busy"
    case portInUse = "port_in_use"
    case stopBeforeConfigure = "stop_before_configure"
    case invalidBudget = "invalid_budget"
    case outputLimit = "output_limit"
    case timedOut = "timed_out"
    case cancelled = "cancelled"
    case invalidResponse = "invalid_response"
    case invalidURL = "invalid_url"
    case invalidBackup = "invalid_backup"
    case invalidBackupKey = "invalid_backup_key"
    case operationFailed = "operation_failed"

    public var code: String { rawValue }
    public var errorDescription: String? {
        switch self {
        case .invalidArguments: return "启动参数无效，请检查本地应用配置。"
        case .invalidPath: return "请选择不含路径跳转的绝对路径。"
        case .symlinkPath: return "运行程序和数据目录不能使用符号链接。"
        case .invalidPort: return "本地端口应为 1024 至 65535。"
        case .profileMissing: return "尚未创建此本地记忆库，可以启动并创建独立数据目录。"
        case .invalidProfile: return "本地记忆库配置未通过核对，现有数据保持不变。"
        case .invalidCredential: return "Owner 凭据的格式、所有权或文件权限未通过核对。"
        case .runtimeUnavailable: return "随包本地运行组件不可用，请检查应用包。"
        case .runtimeNotReady: return "工作台尚未通过本地运行状态核对。"
        case .runtimeBusy: return "此记忆库已有运行操作，请稍后重试。"
        case .portInUse: return "本地端口已被占用，未停止其他服务。"
        case .stopBeforeConfigure: return "请先停止此记忆库，再修改处理设置。"
        case .invalidBudget: return "处理次数上限应为 1 至 10000。"
        case .outputLimit: return "运行组件返回内容超过限制，操作已中止。"
        case .timedOut: return "本地操作等待超时，请重新检查运行状态。"
        case .cancelled: return "本地操作已取消，请重新检查运行状态。"
        case .invalidResponse: return "运行组件未返回有效的状态信息。"
        case .invalidURL: return "工作台地址未通过本地来源核对。"
        case .invalidBackup: return "无法核对备份。请选择 Nova 导出的加密备份及与它配套的 Vault 密钥。"
        case .invalidBackupKey: return "无法读取 Vault 密钥文件，请核对文件格式、所有者及私有权限。"
        case .operationFailed: return "本地操作未完成，请检查状态或本地运行日志。"
        }
    }
}

public struct DesktopConfiguration: Equatable, Sendable {
    public let runtimeBundle: URL
    public let profileDirectory: URL
    public let port: Int
    public let backupInspection: Bool

    public init(runtimeBundle: URL, profileDirectory: URL, port: Int = 3053, backupInspection: Bool = false) throws {
        guard (1024...65535).contains(port) else { throw DesktopError.invalidPort }
        try PrivateFile.validatePath(runtimeBundle)
        try PrivateFile.validatePath(profileDirectory)
        self.runtimeBundle = runtimeBundle
        self.profileDirectory = profileDirectory
        self.port = port
        self.backupInspection = backupInspection
    }

    public static func parse(arguments: [String], resourceURL: URL?, homeDirectory: URL) throws -> DesktopConfiguration {
        var bundle: URL?
        var profile: URL?
        var port = 3053
        var seen = Set<String>()
        var index = 0
        while index < arguments.count {
            let flag = arguments[index]
            if flag == "--backup-inspection" {
                guard seen.insert(flag).inserted else { throw DesktopError.invalidArguments }
                index += 1
                continue
            }
            guard ["--runtime-bundle", "--profile-dir", "--port"].contains(flag),
                  seen.insert(flag).inserted, index + 1 < arguments.count else { throw DesktopError.invalidArguments }
            let value = arguments[index + 1]
            if flag == "--port" {
                guard value.allSatisfy({ $0.isASCII && $0.isNumber }), let parsed = Int(value) else { throw DesktopError.invalidPort }
                port = parsed
            } else {
                try PrivateFile.validateRawPath(value)
                let path = URL(fileURLWithPath: value, isDirectory: true)
                if flag == "--runtime-bundle" { bundle = path } else { profile = path }
            }
            index += 2
        }
        guard let selectedBundle = bundle ?? resourceURL?.appendingPathComponent("LocalRuntime", isDirectory: true) else {
            throw DesktopError.runtimeUnavailable
        }
        let selectedProfile = profile ?? homeDirectory.appendingPathComponent("Library/Application Support/Nova Memory/local-workspace-v03", isDirectory: true)
        let inspection = seen.contains("--backup-inspection")
        guard !inspection || (profile != nil && seen.contains("--port")) else { throw DesktopError.invalidArguments }
        return try DesktopConfiguration(runtimeBundle: selectedBundle, profileDirectory: selectedProfile, port: port,
                                        backupInspection: inspection)
    }
}

enum PrivateFile {
    static func validateRawPath(_ path: String) throws {
        guard path.hasPrefix("/"), !path.utf8.contains(0),
              !path.split(separator: "/", omittingEmptySubsequences: false).contains(where: { $0 == "." || $0 == ".." }) else {
            throw DesktopError.invalidPath
        }
    }

    static func validatePath(_ url: URL) throws {
        guard url.isFileURL, url.host == nil || url.host == "localhost" else { throw DesktopError.invalidPath }
        try validateRawPath(url.path)
        var cursor = ""
        for component in url.path.split(separator: "/") {
            cursor += "/" + component
            var info = stat()
            if lstat(cursor, &info) == 0 {
                if info.st_mode & S_IFMT == S_IFLNK { throw DesktopError.symlinkPath }
            } else if errno != ENOENT { throw DesktopError.invalidPath }
        }
    }

    static func read(_ url: URL, maximum: Int, exactMode: mode_t? = nil) throws -> Data {
        try validatePath(url)
        let descriptor = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard descriptor >= 0 else { throw DesktopError.invalidProfile }
        defer { close(descriptor) }
        var info = stat()
        guard fstat(descriptor, &info) == 0,
              info.st_mode & S_IFMT == S_IFREG, info.st_uid == getuid(),
              info.st_mode & 0o077 == 0, info.st_size >= 0, info.st_size <= maximum,
              exactMode == nil || info.st_mode & 0o777 == exactMode else { throw DesktopError.invalidProfile }
        var data = Data()
        var bytes = [UInt8](repeating: 0, count: min(maximum + 1, 8192))
        while true {
            let count = Darwin.read(descriptor, &bytes, bytes.count)
            if count == 0 { break }
            if count < 0 { if errno == EINTR { continue }; throw DesktopError.invalidProfile }
            data.append(contentsOf: bytes.prefix(count))
            if data.count > maximum { throw DesktopError.invalidProfile }
        }
        return data
    }

    static func validateDirectory(_ url: URL) throws {
        try validatePath(url)
        var info = stat()
        guard lstat(url.path, &info) == 0, info.st_mode & S_IFMT == S_IFDIR,
              info.st_uid == getuid(), info.st_mode & 0o077 == 0 else { throw DesktopError.invalidProfile }
    }
}
