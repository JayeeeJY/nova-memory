import Foundation

public struct LocalNavigationPolicy: Sendable {
    public let baseURL: URL
    private let port: Int
    private static let resources = Set(["/intake.js", "/intake.css", "/intake-logo.png", "/native.js", "/native.css", "/graph.js", "/graph.css", "/report.js", "/report.css"])

    public init(baseURL: URL) throws {
        guard let port = Self.localPort(baseURL), Self.safePath(baseURL) == "/app/",
              baseURL.query == nil, baseURL.fragment == nil else { throw DesktopError.invalidURL }
        self.baseURL = baseURL
        self.port = port
    }

    public func allows(_ url: URL) -> Bool {
        guard Self.localPort(url) == port, url.query == nil, let path = Self.safePath(url) else { return false }
        return path == "/app/" || path.hasPrefix("/app/") || path == "/install" || Self.resources.contains(path)
    }

    public func credentialEligibleURL(_ url: URL) -> Bool {
        Self.localPort(url) == port && url.query == nil && Self.safePath(url) == "/app/"
    }

    static func localPort(_ url: URL) -> Int? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.scheme == "http", components.host == "127.0.0.1",
              components.user == nil, components.password == nil,
              let port = components.port, (1024...65535).contains(port) else { return nil }
        return port
    }

    private static func safePath(_ url: URL) -> String? {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              !components.percentEncodedPath.contains("%"), !components.path.contains("\\"),
              !components.path.split(separator: "/", omittingEmptySubsequences: false).contains(where: { $0 == "." || $0 == ".." }) else { return nil }
        return components.path
    }
}
