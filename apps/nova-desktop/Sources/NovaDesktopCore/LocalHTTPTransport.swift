import Foundation

struct LocalHTTPResponse: Sendable {
    let statusCode: Int
    let data: Data
    let url: URL
}

enum LocalHTTPError: Error, Equatable, Sendable {
    case unavailable, timedOut, redirectDenied, responseTooLarge, invalidResponse, cancelled
}

/// One ephemeral session per request; neither caller credentials nor cookies can
/// be reused by a later probe. The delegate bounds each received chunk.
struct LocalHTTPTransport: @unchecked Sendable {
    private let configuration: URLSessionConfiguration

    init(sessionConfiguration: URLSessionConfiguration? = nil) {
        configuration = sessionConfiguration?.copy() as? URLSessionConfiguration ?? .ephemeral
    }

    func request(_ request: URLRequest, maximumBytes: Int = 65536, timeout: TimeInterval = 5) async throws -> LocalHTTPResponse {
        guard let url = request.url, let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == "http", let host = parts.host,
              ["127.0.0.1", "::1", "[::1]"].contains(host),
              let port = parts.port, (1...65535).contains(port),
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              maximumBytes > 0, maximumBytes <= 1048576,
              timeout.isFinite, timeout > 0, timeout <= 35 else {
            throw LocalHTTPError.invalidResponse
        }
        let settings = configuration.copy() as! URLSessionConfiguration
        settings.urlCache = nil
        settings.requestCachePolicy = .reloadIgnoringLocalCacheData
        settings.httpCookieStorage = nil
        settings.httpShouldSetCookies = false
        settings.urlCredentialStorage = nil
        settings.connectionProxyDictionary = ["HTTPEnable": 0, "HTTPSEnable": 0, "SOCKSEnable": 0,
                                              "ProxyAutoConfigEnable": 0, "ProxyAutoDiscoveryEnable": 0]
        settings.timeoutIntervalForRequest = timeout
        settings.timeoutIntervalForResource = timeout
        var boundedRequest = request
        boundedRequest.timeoutInterval = timeout
        boundedRequest.cachePolicy = .reloadIgnoringLocalCacheData
        boundedRequest.httpShouldHandleCookies = false
        let operation = LocalHTTPRequestOperation(request: boundedRequest, configuration: settings,
                                                  maximumBytes: maximumBytes, timeout: timeout)
        return try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { operation.start($0) }
        }, onCancel: { operation.cancel() })
    }
}

private final class LocalHTTPRequestOperation: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let request: URLRequest
    private let configuration: URLSessionConfiguration
    private let maximumBytes: Int
    private let timeout: TimeInterval
    private let lock = NSLock()
    private var continuation: CheckedContinuation<LocalHTTPResponse, Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var timer: DispatchSourceTimer?
    private var finished = false
    private var cancellationRequested = false
    // Delegate callbacks run on a serial queue; cancellation only accesses locked state.
    private var response: HTTPURLResponse?
    private var data = Data()

    init(request: URLRequest, configuration: URLSessionConfiguration, maximumBytes: Int, timeout: TimeInterval) {
        self.request = request
        self.configuration = configuration
        self.maximumBytes = maximumBytes
        self.timeout = timeout
    }

    func start(_ continuation: CheckedContinuation<LocalHTTPResponse, Error>) {
        lock.lock()
        if cancellationRequested {
            finished = true
            lock.unlock()
            continuation.resume(throwing: LocalHTTPError.cancelled)
            return
        }
        self.continuation = continuation
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
        let task = session.dataTask(with: request)
        let timer = DispatchSource.makeTimerSource(queue: .global(qos: .utility))
        timer.schedule(deadline: .now() + timeout)
        timer.setEventHandler { [weak self] in self?.finish(.failure(LocalHTTPError.timedOut)) }
        self.session = session
        self.task = task
        self.timer = timer
        timer.resume()
        task.resume()
        lock.unlock()
    }

    func cancel() {
        lock.lock()
        cancellationRequested = true
        let hasStarted = continuation != nil
        lock.unlock()
        if hasStarted { finish(.failure(LocalHTTPError.cancelled)) }
    }

    private func finish(_ result: Result<LocalHTTPResponse, Error>) {
        lock.lock()
        guard !finished else { lock.unlock(); return }
        finished = true
        let continuation = self.continuation
        let session = self.session
        let timer = self.timer
        self.continuation = nil
        self.session = nil
        self.task = nil
        self.timer = nil
        lock.unlock()
        timer?.cancel()
        session?.invalidateAndCancel()
        continuation?.resume(with: result)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
        finish(.failure(LocalHTTPError.redirectDenied))
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard let response = response as? HTTPURLResponse, let url = response.url, url == request.url else {
            completionHandler(.cancel)
            finish(.failure(LocalHTTPError.invalidResponse))
            return
        }
        guard response.expectedContentLength <= Int64(maximumBytes) else {
            completionHandler(.cancel)
            finish(.failure(LocalHTTPError.responseTooLarge))
            return
        }
        guard !(300...399).contains(response.statusCode) else {
            completionHandler(.cancel)
            finish(.failure(LocalHTTPError.redirectDenied))
            return
        }
        self.response = response
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive chunk: Data) {
        guard chunk.count <= maximumBytes - data.count else {
            finish(.failure(LocalHTTPError.responseTooLarge))
            return
        }
        data.append(chunk)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error = error as? URLError {
            finish(.failure(error.code == .timedOut ? LocalHTTPError.timedOut :
                            error.code == .cancelled ? LocalHTTPError.cancelled : LocalHTTPError.unavailable))
        } else if error != nil {
            finish(.failure(LocalHTTPError.unavailable))
        } else if let response, let url = response.url {
            finish(.success(LocalHTTPResponse(statusCode: response.statusCode, data: data, url: url)))
        } else {
            finish(.failure(LocalHTTPError.invalidResponse))
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        // Keep an authentication diagnostic without consulting a system keychain
        // or retrying with credentials. The response body is intentionally unused.
        let authenticationResponse = challenge.failureResponse as? HTTPURLResponse
        completionHandler(.cancelAuthenticationChallenge, nil)
        if let response = authenticationResponse, let url = response.url,
           url == request.url, [401, 403].contains(response.statusCode) {
            finish(.success(LocalHTTPResponse(statusCode: response.statusCode, data: Data(), url: url)))
        } else {
            finish(.failure(LocalHTTPError.unavailable))
        }
    }
}
