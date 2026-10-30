import Foundation

/// Every VibeDev call the phone makes. View models talk to this protocol so
/// tests can stub the server.
protocol VibeDevServing {
    func liveRuns() async throws -> VibeDevLiveRunsResponse
    func runHistory(limit: Int, cursor: String?) async throws -> VibeDevRunHistory
    func projects() async throws -> VibeDevProjectsResponse
    func runsPage(projectID: String, limit: Int, offset: Int, query: String) async throws -> VibeDevRunsPage
    func crews(projectID: String) async throws -> [VibeDevCrew]
    func runTurns(runID: String) async throws -> VibeDevRunTurns
    func timeline(turnID: String, limit: Int) async throws -> VibeDevTimeline
    func changes(runID: String, turn: String?, path: String?) async throws -> VibeDevRunChanges
    func repoSuggestions() async throws -> [VibeDevRepoSuggestion]
    func codingProfiles() async throws -> [VibeDevCodingProfile]

    func startRun(_ request: VibeDevStartRunRequest) async throws -> VibeDevStartRunResponse
    func createProject(name: String, repoPath: String) async throws -> VibeDevProject
    func updateProject(id: String, trust: VibeDevTrust?, baseBranch: String?) async throws -> VibeDevProject
    /// `git init` + a first commit in the project folder (health action
    /// `git_init`). Errors carry the server's `message`.
    func gitInit(projectID: String) async throws -> VibeDevGitInitResponse
    /// Steer or stop a live turn. Throws `.http(409, …)` between turns.
    func control(liveTaskID: String, action: VibeDevControlAction, message: String?) async throws
    func createCrew(projectID: String, request: VibeDevCreateCrewRequest) async throws -> VibeDevCrew
    func crewAction(projectID: String, crewID: String, action: VibeDevCrewAction) async throws -> VibeDevCrew
    func chooseCrew(projectID: String, crewID: String, laneIndex: Int?) async throws -> VibeDevCrew
    /// Answer a crew question (a user request) with one of its option ids,
    /// the same way Attention answers it.
    func answerQuestion(requestID: String, optionID: String) async throws
    /// Apply or reject a diff approval by its id, as Attention does.
    func answerDiffApproval(approvalID: String, apply: Bool) async throws
    func merge(rootID: String) async throws
    func openPullRequest(rootID: String, title: String, body: String) async throws -> String?
    func discard(rootID: String) async throws
}

enum VibeDevControlAction: String {
    case steer, stop
}

enum VibeDevAPIError: LocalizedError, Equatable {
    case unconfigured
    case invalidResponse
    case http(Int, String)

    var status: Int? {
        if case .http(let status, _) = self { return status }
        return nil
    }

    var errorDescription: String? {
        switch self {
        case .unconfigured: return "Connect to your Magician in Settings."
        case .invalidResponse: return "The server's answer could not be read."
        case .http(let status, let message):
            if !message.isEmpty { return message }
            switch status {
            case 401: return "Your connection cannot reach VibeDev. Check Settings."
            case 403: return "Only the owner can do this, from their own machine."
            case 404: return "That is no longer on the server."
            case 409: return "That changed on the server. Refresh and try again."
            default: return "VibeDev request failed (\(status))."
            }
        }
    }
}

/// The live client: the active connection profile, authorized per request.
/// Scope comes from the device bearer (`MagicianAccess.authorize`); the server
/// ignores any scope query, so none is sent.
struct VibeDevAPI: VibeDevServing {
    var session: URLSession = .shared
    private static let prefix = "/api/magician/v2"

    // MARK: Reads

    func liveRuns() async throws -> VibeDevLiveRunsResponse {
        try await get("/vibedev/runs/live")
    }

    func runHistory(limit: Int, cursor: String?) async throws -> VibeDevRunHistory {
        var items = [URLQueryItem(name: "limit", value: String(limit))]
        if let cursor { items.append(URLQueryItem(name: "cursor", value: cursor)) }
        return try await get("/vibedev/runs", query: items)
    }

    func projects() async throws -> VibeDevProjectsResponse {
        try await get("/vibedev/projects")
    }

    func runsPage(projectID: String, limit: Int, offset: Int, query: String) async throws -> VibeDevRunsPage {
        var items = [URLQueryItem(name: "limit", value: String(limit)),
                     URLQueryItem(name: "offset", value: String(offset))]
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { items.append(URLQueryItem(name: "query", value: trimmed)) }
        return try await get("/vibedev/projects/\(Self.segment(projectID))/runs", query: items)
    }

    func crews(projectID: String) async throws -> [VibeDevCrew] {
        let response: VibeDevCrewsResponse = try await get("/vibedev/projects/\(Self.segment(projectID))/crews")
        return response.crews
    }

    func runTurns(runID: String) async throws -> VibeDevRunTurns {
        try await get("/vibedev/runs/\(Self.segment(runID))/turns")
    }

    func timeline(turnID: String, limit: Int) async throws -> VibeDevTimeline {
        try await get("/vibedev/runs/\(Self.segment(turnID))/timeline",
                      query: [URLQueryItem(name: "limit", value: String(limit))])
    }

    func changes(runID: String, turn: String?, path: String?) async throws -> VibeDevRunChanges {
        var items: [URLQueryItem] = []
        if let turn { items.append(URLQueryItem(name: "turn", value: turn)) }
        if let path { items.append(URLQueryItem(name: "path", value: path)) }
        return try await get("/vibedev/runs/\(Self.segment(runID))/changes", query: items)
    }

    func repoSuggestions() async throws -> [VibeDevRepoSuggestion] {
        let response: VibeDevRepoSuggestions = try await get("/vibedev/repos/suggest")
        return response.repos
    }

    func codingProfiles() async throws -> [VibeDevCodingProfile] {
        let response: VibeDevModelCatalog = try await get("/coding/models")
        return response.rows
    }

    // MARK: Writes

    func startRun(_ request: VibeDevStartRunRequest) async throws -> VibeDevStartRunResponse {
        try await send("POST", "/vibedev/runs", body: request.body)
    }

    func createProject(name: String, repoPath: String) async throws -> VibeDevProject {
        let response: VibeDevProjectMutation = try await send(
            "POST", "/vibedev/projects", body: ["name": name, "repo_path": repoPath])
        return response.project
    }

    func gitInit(projectID: String) async throws -> VibeDevGitInitResponse {
        try await send("POST", "/vibedev/projects/\(Self.segment(projectID))/git-init", body: [:])
    }

    func updateProject(id: String, trust: VibeDevTrust?, baseBranch: String?) async throws -> VibeDevProject {
        var body: [String: Any] = [:]
        if let trust { body["trust"] = trust.rawValue }
        if let baseBranch { body["base_branch"] = baseBranch }
        let response: VibeDevProjectMutation = try await send(
            "PATCH", "/vibedev/projects/\(Self.segment(id))", body: body)
        return response.project
    }

    func control(liveTaskID: String, action: VibeDevControlAction, message: String?) async throws {
        var body: [String: Any] = ["action": action.rawValue]
        if let message { body["message"] = message }
        let (data, status) = try await raw("POST", "/vibedev/runs/\(Self.segment(liveTaskID))/control", body: body)
        try Self.check(data, status)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        if object?["delivered"] as? Bool == false {
            throw VibeDevAPIError.http(409, (object?["reason"] as? String) ?? "")
        }
    }

    func createCrew(projectID: String, request: VibeDevCreateCrewRequest) async throws -> VibeDevCrew {
        let response: VibeDevCrewEnvelope = try await send(
            "POST", "/vibedev/projects/\(Self.segment(projectID))/crews", body: request.body)
        return response.crew
    }

    func crewAction(projectID: String, crewID: String, action: VibeDevCrewAction) async throws -> VibeDevCrew {
        let response: VibeDevCrewEnvelope = try await send(
            "POST", "/vibedev/projects/\(Self.segment(projectID))/crews/\(Self.segment(crewID))/\(action.rawValue)",
            body: [:])
        return response.crew
    }

    func chooseCrew(projectID: String, crewID: String, laneIndex: Int?) async throws -> VibeDevCrew {
        let index: Any = laneIndex.map { $0 as Any } ?? NSNull()
        let response: VibeDevCrewEnvelope = try await send(
            "POST", "/vibedev/projects/\(Self.segment(projectID))/crews/\(Self.segment(crewID))/choose",
            body: ["lane_index": index])
        return response.crew
    }

    func answerQuestion(requestID: String, optionID: String) async throws {
        let body: [String: Any] = [
            "source": "user_request",
            "channel": "ios",
            "value": ["type": "choice", "selected_id": optionID],
            "input_type": "choice",
            "correlation_id": requestID,
            "pause_state_id": requestID,
        ]
        let (data, status) = try await raw("POST", "/hitl/\(Self.segment(requestID))/respond", body: body)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        // Already answered elsewhere is the outcome the owner wanted.
        if status == 409, object?["reason"] as? String == "already_resolved" { return }
        try Self.check(data, status)
    }

    func answerDiffApproval(approvalID: String, apply: Bool) async throws {
        let body: [String: Any] = [
            "source": "diff_approval",
            "channel": "ios",
            "value": ["type": "choice", "selected_id": apply ? "apply" : "reject"],
            "input_type": "diff_approval",
            "correlation_id": approvalID,
            "pause_state_id": approvalID,
        ]
        let (data, status) = try await raw("POST", "/hitl/\(Self.segment(approvalID))/respond", body: body)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        if status == 409, object?["reason"] as? String == "already_resolved" { return }
        try Self.check(data, status)
        if object?["accepted"] as? Bool == false {
            throw VibeDevAPIError.http(status, Self.message(from: data))
        }
    }

    func merge(rootID: String) async throws {
        let (data, status) = try await raw("POST", "/vibedev/runs/\(Self.segment(rootID))/merge", body: [:])
        try Self.check(data, status)
    }

    func openPullRequest(rootID: String, title: String, body: String) async throws -> String? {
        let (data, status) = try await raw(
            "POST", "/vibedev/runs/\(Self.segment(rootID))/pull-request", body: ["title": title, "body": body])
        try Self.check(data, status)
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        return object?["pull_request_url"] as? String
    }

    func discard(rootID: String) async throws {
        let (data, status) = try await raw("POST", "/vibedev/runs/\(Self.segment(rootID))/discard", body: [:])
        try Self.check(data, status)
    }

    // MARK: Transport

    static func segment(_ value: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#%")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }

    static func url(base: URL, path: String, query: [URLQueryItem] = []) -> URL? {
        let root = base.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard var components = URLComponents(string: root + prefix + path) else { return nil }
        if !query.isEmpty { components.queryItems = query }
        return components.url
    }

    /// The server's own words for a failure: `message`, then `reason`, then `error`.
    static func message(from data: Data) -> String {
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return "" }
        for key in ["message", "reason", "detail"] {
            if let text = object[key] as? String, !text.isEmpty { return text }
        }
        if let error = object["error"] as? [String: Any], let text = error["message"] as? String { return text }
        // `vibedev_project_unhealthy` without a message: the issues say why.
        if let health = object["health"] as? [String: Any], let issues = health["issues"] as? [[String: Any]] {
            let messages = issues.compactMap { $0["message"] as? String }.filter { !$0.isEmpty }
            if !messages.isEmpty { return "Project needs attention: runs cannot start. " + messages.joined(separator: " ") }
        }
        return ""
    }

    static func check(_ data: Data, _ status: Int) throws {
        guard (200..<300).contains(status) else {
            throw VibeDevAPIError.http(status, message(from: data))
        }
    }

    private func raw(_ method: String, _ path: String, query: [URLQueryItem] = [],
                     body: [String: Any]? = nil) async throws -> (Data, Int) {
        guard MagicianAccess.isConfigured else { throw VibeDevAPIError.unconfigured }
        guard let url = Self.url(base: MagicianAccess.baseURL, path: path, query: query) else {
            throw VibeDevAPIError.invalidResponse
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 30
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        MagicianAccess.authorize(&request)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw VibeDevAPIError.invalidResponse }
        return (data, http.statusCode)
    }

    private func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        let (data, status) = try await raw("GET", path, query: query)
        try Self.check(data, status)
        return try Self.decode(data)
    }

    private func send<T: Decodable>(_ method: String, _ path: String, body: [String: Any]) async throws -> T {
        let (data, status) = try await raw(method, path, body: body)
        try Self.check(data, status)
        return try Self.decode(data)
    }

    static func decode<T: Decodable>(_ data: Data) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: data) } catch {
            throw VibeDevAPIError.invalidResponse
        }
    }
}
