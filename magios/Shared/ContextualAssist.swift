import Foundation

enum ContextualAssistAction: String, CaseIterable, Identifiable, Codable {
    case rewrite
    case summarize
    case reply
    case shorten
    case clarify
    case continueWriting

    var id: String { rawValue }

    var backendID: String {
        switch self {
        case .rewrite: return "rewrite"
        case .summarize: return "summarize"
        case .reply: return "draft_reply"
        case .shorten: return "shorten"
        case .clarify: return "clarify"
        case .continueWriting: return "continue_draft"
        }
    }

    var label: String {
        switch self {
        case .rewrite: return "Rewrite"
        case .summarize: return "Summarize"
        case .reply: return "Reply"
        case .shorten: return "Shorten"
        case .clarify: return "Clarify"
        case .continueWriting: return "Continue"
        }
    }

    var systemImage: String {
        switch self {
        case .rewrite: return "pencil.and.outline"
        case .summarize: return "text.alignleft"
        case .reply: return "arrowshape.turn.up.left"
        case .shorten: return "arrow.down.right.and.arrow.up.left"
        case .clarify: return "sparkles"
        case .continueWriting: return "text.append"
        }
    }

    var intent: String {
        switch self {
        case .rewrite: return "rewrite_selection"
        case .summarize: return "summarize_selection"
        case .reply: return "draft_reply_to_selection"
        case .shorten: return "shorten_field_selection"
        case .clarify: return "clarify_field_selection"
        case .continueWriting: return "continue_after_selection"
        }
    }
}

struct ContextualAssistRequest: Encodable, Equatable {
    static let maximumTextCharacters = 50_000

    struct ActionPayload: Encodable, Equatable {
        let id: String
        let label: String
        let intent: String
        let requiresScreenshot: Bool
        let createsTask: Bool
        let opensHUD: Bool

        enum CodingKeys: String, CodingKey {
            case id, label, intent, requiresScreenshot, createsTask
            case opensHUD = "opensHud"
        }
    }

    struct ContextPayload: Encodable, Equatable {
        let state: String
        let personality: String
        let app: String?
        let windowTitle: String?
        let url: String?
        let contextText: String?
        let hasContextText: Bool
    }

    struct RoutingPayload: Encodable, Equatable {
        let agentID: String
        let surface: String?
        let featureMode: String?
        let sourceKind: String
        let sourceKey: String
        let sessionKey: String
        let threadID: String?
        let sessionID: String?
        let sessionTitle: String?
        let rootURL: String?
        let targetTextKind: String
        let actionIntent: String
        let personality: String

        enum CodingKeys: String, CodingKey {
            case sourceKind, sourceKey, sessionKey, targetTextKind, actionIntent, personality
            case surface, featureMode
            case agentID = "agentId"
            case threadID = "threadId"
            case sessionID = "sessionId"
            case sessionTitle
            case rootURL = "rootUrl"
        }

        init(
            agentID: String,
            surface: String? = nil,
            featureMode: String? = nil,
            sourceKind: String,
            sourceKey: String,
            sessionKey: String,
            threadID: String? = nil,
            sessionID: String? = nil,
            sessionTitle: String? = nil,
            rootURL: String?,
            targetTextKind: String,
            actionIntent: String,
            personality: String
        ) {
            self.agentID = agentID
            self.surface = surface
            self.featureMode = featureMode
            self.sourceKind = sourceKind
            self.sourceKey = sourceKey
            self.sessionKey = sessionKey
            self.threadID = threadID
            self.sessionID = sessionID
            self.sessionTitle = sessionTitle
            self.rootURL = rootURL
            self.targetTextKind = targetTextKind
            self.actionIntent = actionIntent
            self.personality = personality
        }
    }

    let principal: String
    let workspace: String
    let userPrompt: String?
    let action: ActionPayload
    let context: ContextPayload
    let routing: RoutingPayload
    /// Optional client-supplied turn id (parity with chat's `chat_turn_id`). When
    /// set, the backend uses it for the underlying chat turn and echoes it, so the
    /// client can correlate/tail the turn. Omitted from the wire when nil.
    let chatTurnID: String?

    enum CodingKeys: String, CodingKey {
        case userPrompt, action, context, routing
        case chatTurnID = "chatTurnId"
    }

    static func make(
        action selectedAction: ContextualAssistAction,
        text: String,
        guidance: String?,
        sourceURL: URL? = nil,
        personality: String = "active",
        sessionKey: String,
        sourceKey: String = "app:ios",
        chatTurnID: String? = nil
    ) -> ContextualAssistRequest {
        let cappedText = String(text.prefix(maximumTextCharacters))
        let trimmedGuidance = guidance?.trimmingCharacters(in: .whitespacesAndNewlines)
        let prompt = trimmedGuidance?.isEmpty == false ? trimmedGuidance : nil
        let source = sourceURL?.absoluteString
        return ContextualAssistRequest(
            principal: MagicianAccess.principal,
            workspace: MagicianAccess.workspace,
            userPrompt: prompt,
            action: ActionPayload(
                id: selectedAction.backendID,
                label: selectedAction.label,
                intent: selectedAction.intent,
                requiresScreenshot: false,
                createsTask: false,
                opensHUD: false
            ),
            context: ContextPayload(
                state: "selection",
                personality: personality,
                app: "iOS Share Extension",
                windowTitle: nil,
                url: source,
                contextText: cappedText,
                hasContextText: !cappedText.isEmpty
            ),
            routing: RoutingPayload(
                agentID: "writing-assistant",
                sourceKind: "app",
                sourceKey: sourceKey,
                sessionKey: sessionKey,
                rootURL: source,
                targetTextKind: "selected_text",
                actionIntent: selectedAction.intent,
                personality: personality
            ),
            chatTurnID: chatTurnID
        )
    }
}

struct ContextualAssistSessionRequest: Encodable, Equatable {
    let principal: String
    let workspace: String
    let threadID: String
    let agentID: String
    let surface: String?
    let featureMode: String?
    let sourceKey: String
    let title: String

    enum CodingKeys: String, CodingKey {
        case sourceKey, title, surface, featureMode
        case threadID = "threadId"
        case agentID = "agentId"
    }

    init(
        principal: String,
        workspace: String,
        threadID: String,
        agentID: String,
        surface: String? = nil,
        featureMode: String? = nil,
        sourceKey: String,
        title: String
    ) {
        self.principal = principal
        self.workspace = workspace
        self.threadID = threadID
        self.agentID = agentID
        self.surface = surface
        self.featureMode = featureMode
        self.sourceKey = sourceKey
        self.title = title
    }
}

struct ContextualAssistSessionResponse: Decodable, Equatable {
    let status: String
    let sessionID: String
    let threadID: String
    let sessionTitle: String

    enum CodingKeys: String, CodingKey {
        case status, sessionTitle
        case sessionID = "sessionId"
        case threadID = "threadId"
    }
}

struct ContextualAssistResponse: Decodable, Equatable {
    let status: String
    let sessionID: String?
    let threadID: String?
    let chatTurnID: String?
    let draftText: String?

    enum CodingKeys: String, CodingKey {
        case status, draftText
        case sessionID = "sessionId"
        case threadID = "threadId"
        case chatTurnID = "chatTurnId"
    }
}

enum ContextualAssistClientError: LocalizedError, Equatable {
    case offline
    case timedOut
    case cancelled
    case authentication(String)
    case validation(String)
    /// The caller supplied a durable feature-session id that the server no
    /// longer owns. Feature clients may replace that exact local binding once.
    case staleSession(String)
    case server(String)
    case malformedResponse

    var errorDescription: String? {
        switch self {
        case .offline: return "You appear to be offline. Check your connection and try again."
        case .timedOut: return "Writing Help took too long. Try again or continue in Magican."
        case .cancelled: return "The request was cancelled."
        case .authentication(let message), .validation(let message), .staleSession(let message),
             .server(let message): return message
        case .malformedResponse: return "Sam returned a response that Writing Help could not read."
        }
    }
}

struct ContextualAssistClient {
    var session: URLSession
    var baseURL: URL
    var timeout: TimeInterval

    init(
        session: URLSession = .shared,
        baseURL: URL = MagicianAccess.baseURL,
        timeout: TimeInterval = 30
    ) {
        self.session = session
        self.baseURL = baseURL
        self.timeout = timeout
    }

    func run(_ body: ContextualAssistRequest) async throws -> ContextualAssistResponse {
        let url = baseURL.appendingPathComponent("api/magician/v2/contextual-writing/actions")
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try JSONEncoder().encode(body)

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch is CancellationError {
            throw ContextualAssistClientError.cancelled
        } catch let error as URLError {
            switch error.code {
            case .cancelled: throw ContextualAssistClientError.cancelled
            case .timedOut: throw ContextualAssistClientError.timedOut
            case .notConnectedToInternet, .networkConnectionLost, .cannotFindHost, .cannotConnectToHost:
                throw ContextualAssistClientError.offline
            default: throw ContextualAssistClientError.server(error.localizedDescription)
            }
        }

        guard let http = response as? HTTPURLResponse else {
            throw ContextualAssistClientError.malformedResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            let message = Self.errorMessage(in: data) ?? HTTPURLResponse.localizedString(forStatusCode: http.statusCode)
            switch http.statusCode {
            case 404, 409, 410:
                if body.routing.sessionID?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false {
                    throw ContextualAssistClientError.staleSession(message)
                }
                throw ContextualAssistClientError.validation(message)
            case 400, 422:
                throw ContextualAssistClientError.validation(message)
            case 401, 403:
                let setup = MagicianAccess.hasAccessCredentials
                    ? message
                    : "Open Magican once to finish secure access setup, then try again."
                throw ContextualAssistClientError.authentication(setup)
            default:
                throw ContextualAssistClientError.server(message)
            }
        }

        guard let decoded = try? JSONDecoder().decode(ContextualAssistResponse.self, from: data) else {
            throw ContextualAssistClientError.malformedResponse
        }
        return decoded
    }

    /// Allocate a durable feature-owned chat session before starting a long
    /// generation turn. The caller persists the id immediately and every later
    /// contextual turn addresses that exact session.
    func createSession(
        _ body: ContextualAssistSessionRequest
    ) async throws -> ContextualAssistSessionResponse {
        let url = baseURL.appendingPathComponent("api/magician/v2/contextual-writing/sessions")
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try JSONEncoder().encode(body)

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch is CancellationError {
            throw ContextualAssistClientError.cancelled
        } catch let error as URLError {
            switch error.code {
            case .cancelled: throw ContextualAssistClientError.cancelled
            case .timedOut: throw ContextualAssistClientError.timedOut
            case .notConnectedToInternet, .networkConnectionLost, .cannotFindHost, .cannotConnectToHost:
                throw ContextualAssistClientError.offline
            default: throw ContextualAssistClientError.server(error.localizedDescription)
            }
        }

        guard let http = response as? HTTPURLResponse else {
            throw ContextualAssistClientError.malformedResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            let message = Self.errorMessage(in: data)
                ?? HTTPURLResponse.localizedString(forStatusCode: http.statusCode)
            switch http.statusCode {
            case 400, 404, 409, 422:
                throw ContextualAssistClientError.validation(message)
            case 401, 403:
                throw ContextualAssistClientError.authentication(message)
            default:
                throw ContextualAssistClientError.server(message)
            }
        }
        guard let decoded = try? JSONDecoder().decode(ContextualAssistSessionResponse.self, from: data),
              !decoded.sessionID.isEmpty else {
            throw ContextualAssistClientError.malformedResponse
        }
        return decoded
    }

    static func errorMessage(in data: Data) -> String? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        if let details = object["details"] as? [String: Any],
           let reason = details["reason"] as? String,
           !reason.isEmpty { return reason }
        if let details = object["details"] as? String, !details.isEmpty { return details }
        if let error = object["error"] as? String, !error.isEmpty { return error }
        if let message = object["message"] as? String, !message.isEmpty { return message }
        return nil
    }
}

enum WebpageAssistOperation: String, Equatable {
    case summarizePage
    case askSam

    var label: String {
        switch self {
        case .summarizePage: return "Summarize Page"
        case .askSam: return "Ask Sam"
        }
    }

    var systemImage: String {
        switch self {
        case .summarizePage: return "doc.text.magnifyingglass"
        case .askSam: return "sparkles"
        }
    }
}

struct WebpageAssistExecutionRequest: Encodable, Equatable {
    let title: String
    let initialMessage: String
    let uiThreadID: String
    let skipPlanning: Bool
    /// The page summary/question is user-requested output, but its backing run
    /// is execution machinery rather than a commitment the user should manage.
    let internalTask: Bool
    let envMode: String

    enum CodingKeys: String, CodingKey {
        case title
        case initialMessage = "initial_message"
        case uiThreadID = "ui_thread_id"
        case skipPlanning = "skip_planning"
        case internalTask = "internal"
        case envMode = "env_mode"
    }

    static func make(
        operation: WebpageAssistOperation,
        url: URL,
        guidance: String?
    ) throws -> WebpageAssistExecutionRequest {
        guard let scheme = url.scheme?.lowercased(), ["http", "https"].contains(scheme) else {
            throw WebpageAssistClientError.validation("Summarize Page and Ask Sam require an HTTP or HTTPS webpage.")
        }

        let trimmedGuidance = guidance?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if operation == .askSam, trimmedGuidance.isEmpty {
            throw WebpageAssistClientError.validation("Add a question or instruction for Sam.")
        }

        let source = url.absoluteString
        let host = url.host?.replacingOccurrences(of: "www.", with: "") ?? "webpage"
        let goal: String
        let title: String
        switch operation {
        case .summarizePage:
            title = "Summarize \(host)"
            goal = """
            This is a direct webpage task; execute it now without creating or proposing a plan. Open the URL with the browser tool using connection_mode="headless" first. Only if the page cannot be accessed or rendered in headless mode, retry with connection_mode="headed". Summarize the actual page contents, not the URL string. Provide a concise overview, the key points, important caveats or uncertainties, and include the source URL in the response. If neither browser mode can access the page, say so explicitly instead of inferring its contents from the URL.

            URL: \(source)
            """
        case .askSam:
            title = "Ask Sam about \(host)"
            goal = """
            This is a direct webpage task; execute it now without creating or proposing a plan. Open the URL with the browser tool using connection_mode="headless" first. Only if the page cannot be accessed or rendered in headless mode, retry with connection_mode="headed". Answer the user's request from the actual page contents, not from the URL string alone. Include the source URL in the response, distinguish page facts from your own inference, and say explicitly if neither browser mode can access the page.

            User request: \(trimmedGuidance)

            URL: \(source)
            """
        }

        return WebpageAssistExecutionRequest(
            title: title,
            initialMessage: goal,
            uiThreadID: "general",
            skipPlanning: true,
            internalTask: true,
            envMode: "browser"
        )
    }
}

struct WebpageAssistTask: Decodable, Equatable {
    let id: String
    let executionID: String
}

private struct WebpageAssistExecutionResponse: Decodable {
    struct ExecutionPayload: Decodable {
        let id: String
        let taskID: String?

        enum CodingKeys: String, CodingKey {
            case id
            case taskID = "task_id"
        }
    }

    let executionID: String
    let execution: ExecutionPayload
    let initialMessageEnqueued: Bool
    let skipPlanning: Bool

    enum CodingKeys: String, CodingKey {
        case execution
        case executionID = "execution_id"
        case initialMessageEnqueued = "initial_message_enqueued"
        case skipPlanning = "skip_planning"
    }
}

enum WebpageAssistClientError: LocalizedError, Equatable {
    case offline
    case timedOut
    case cancelled
    case authentication(String)
    case validation(String)
    case server(String)
    case malformedResponse

    var errorDescription: String? {
        switch self {
        case .offline: return "You appear to be offline. Check your connection and try again."
        case .timedOut: return "The webpage request took too long. Try again or continue in Magican."
        case .cancelled: return "The request was cancelled."
        case .authentication(let message), .validation(let message), .server(let message): return message
        case .malformedResponse: return "Sam returned a webpage-task response that Magican Assist could not read."
        }
    }
}

struct WebpageAssistClient {
    var session: URLSession
    var baseURL: URL
    var timeout: TimeInterval

    init(
        session: URLSession = .shared,
        baseURL: URL = MagicianAccess.baseURL,
        timeout: TimeInterval = 30
    ) {
        self.session = session
        self.baseURL = baseURL
        self.timeout = timeout
    }

    /// Atomically creates a durable task-backed execution and queues the goal on
    /// the direct runtime path. The endpoint responds before the page work
    /// finishes, allowing the extension to persist the exact task and hand off.
    func start(
        operation: WebpageAssistOperation,
        url: URL,
        guidance: String? = nil
    ) async throws -> WebpageAssistTask {
        let body = try WebpageAssistExecutionRequest.make(
            operation: operation,
            url: url,
            guidance: guidance
        )
        var request = authorizedRequest(
            path: "api/magician/v2/executions",
            body: try JSONEncoder().encode(body)
        )
        request.httpMethod = "POST"
        let data = try await send(request)
        guard let response = try? JSONDecoder().decode(WebpageAssistExecutionResponse.self, from: data),
              response.skipPlanning,
              response.initialMessageEnqueued,
              let taskID = response.execution.taskID?.trimmingCharacters(in: .whitespacesAndNewlines),
              !taskID.isEmpty,
              !response.executionID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              response.execution.id == response.executionID else {
            throw WebpageAssistClientError.malformedResponse
        }
        return WebpageAssistTask(id: taskID, executionID: response.executionID)
    }

    private func authorizedRequest(path: String, body: Data) -> URLRequest {
        var request = URLRequest(url: baseURL.appendingPathComponent(path), timeoutInterval: timeout)
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = body
        return request
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch is CancellationError {
            throw WebpageAssistClientError.cancelled
        } catch let error as URLError {
            switch error.code {
            case .cancelled: throw WebpageAssistClientError.cancelled
            case .timedOut: throw WebpageAssistClientError.timedOut
            case .notConnectedToInternet, .networkConnectionLost, .cannotFindHost, .cannotConnectToHost:
                throw WebpageAssistClientError.offline
            default: throw WebpageAssistClientError.server(error.localizedDescription)
            }
        }

        guard let http = response as? HTTPURLResponse else {
            throw WebpageAssistClientError.malformedResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            let message = ContextualAssistClient.errorMessage(in: data)
                ?? HTTPURLResponse.localizedString(forStatusCode: http.statusCode)
            switch http.statusCode {
            case 400, 404, 409, 422:
                throw WebpageAssistClientError.validation(message)
            case 401, 403:
                let setup = MagicianAccess.hasAccessCredentials
                    ? message
                    : "Open Magican once to finish secure access setup, then try again."
                throw WebpageAssistClientError.authentication(setup)
            default:
                throw WebpageAssistClientError.server(message)
            }
        }
        return data
    }
}
