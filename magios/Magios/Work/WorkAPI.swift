import Foundation

enum WorkSpecialist: String, CaseIterable, Identifiable {
    case analytics = "/llm", history = "/history", memory = "/memory", budget = "/budget", evals = "/evals", apps = "/apps", skills = "/skills", runtime = "/runtime", events = "/events", observe = "/observe"
    var id: String { rawValue }
    var title: String {
        switch self {
        case .analytics: return "Models and usage"
        case .history: return "Execution history"
        case .memory: return "Memory observability"
        case .budget: return "Budget and tokens"
        case .evals: return "Evaluations"
        case .apps: return "Apps"
        case .skills: return "Skills"
        case .runtime: return "Runtime diagnostics"
        case .events: return "Events"
        case .observe: return "Performance"
        }
    }
    var scoped: Bool { [.analytics, .history, .memory, .events].contains(self) }
}

/// Mirrors the web specialist adapter, including only filters the page consumes.
struct WorkWebContext {
    var section: WorkSection = .all
    var agent = "", task = "", execution = ""
    var internalWork = false
    var program = "", run = ""
    var appInstallation = ""
    var returnPath: String {
        var components = URLComponents(); components.path = "/work"
        var query: [URLQueryItem] = []
        switch section {
        case .crew: query.append(.init(name: "view", value: "crew")); if !agent.isEmpty { query.append(.init(name: "agent", value: agent)) }
        case .programs: query.append(.init(name: "view", value: "programs")); if !program.isEmpty { query.append(.init(name: "program", value: program)) }
        case .quality:
            query.append(.init(name: "view", value: "quality"))
            if !run.isEmpty { query.append(.init(name: "run", value: run)) }
            if !agent.isEmpty { query.append(.init(name: "agent", value: agent)) }
        case .operations: query.append(.init(name: "view", value: "operations"))
        case .goals: query.append(.init(name: "view", value: "goals"))
        case .needsYou: query.append(.init(name: "view", value: "needs-you"))
        case .all:
            if !task.isEmpty { query.append(.init(name: "selected", value: task)) }
            if internalWork { query.append(.init(name: "type", value: "internal")) }
            if !execution.isEmpty { query.append(.init(name: "execution_id", value: execution)) }
        }
        components.queryItems = query.isEmpty ? nil : query
        components.percentEncodedQuery = components.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return components.string ?? "/work"
    }
    func url(base: URL, destination: WorkSpecialist) -> URL? {
        guard var components = URLComponents(url: base, resolvingAgainstBaseURL: false),
              ["http", "https"].contains(components.scheme?.lowercased() ?? ""), components.host?.isEmpty == false,
              components.user == nil, components.password == nil, components.query == nil, components.fragment == nil else { return nil }
        components.path = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")).isEmpty ? destination.rawValue : components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + destination.rawValue
        if destination == .apps, appInstallation.range(of: "^[A-Za-z0-9_.-]{1,128}$", options: .regularExpression) != nil { components.path += "/" + appInstallation }
        if !components.path.hasPrefix("/") { components.path = "/" + components.path }
        var query: [URLQueryItem] = []
        if destination.scoped && !agent.isEmpty { query.append(.init(name: "agent_id", value: agent)) }
        if [.analytics, .events].contains(destination) && !task.isEmpty { query.append(.init(name: "task_id", value: task)) }
        if destination == .events && !execution.isEmpty { query.append(.init(name: "execution_id", value: execution)) }
        query.append(.init(name: "from", value: returnPath)); components.queryItems = query
        components.percentEncodedQuery = components.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        if destination == .analytics && (!agent.isEmpty || !task.isEmpty) { components.fragment = "usage" }
        return components.url
    }
    func workURL(base: URL) -> URL? {
        guard let checked = url(base: base, destination: .events),
              var components = URLComponents(url: checked, resolvingAgainstBaseURL: false),
              let target = URLComponents(string: returnPath) else { return nil }
        components.path = String(components.path.dropLast(WorkSpecialist.events.rawValue.count)) + "/work"
        components.percentEncodedQuery = target.percentEncodedQuery; components.fragment = nil
        return components.url
    }
}

/// Navigation over existing records, rather than another task or execution model.
enum WorkSection: String, CaseIterable, Identifiable {
    case all, needsYou, crew, goals, programs, quality, operations
    var id: String { rawValue }
    var title: String {
        switch self {
        case .all: return "All work"
        case .needsYou: return "Needs you"
        case .crew: return "Crew"
        case .goals: return "Goals"
        case .programs: return "Playbooks"
        case .quality: return "Quality checks"
        case .operations: return "Team routines"
        }
    }
}


/// Plain-language Work concepts, shared by navigation and contextual help.
enum WorkHelpTopic: CaseIterable {
    case allWork, needsYou, crew, goals, playbooks, quality, operations, tasks, monitors, systemWork, autopilot, cycles, focus, delegation
    var title: String {
        switch self {
        case .allWork: return "All work"
        case .needsYou: return "Needs you"
        case .crew: return "Crew"
        case .goals: return "Goals"
        case .playbooks: return "Playbooks"
        case .quality: return "Quality checks"
        case .operations: return "Team routines"
        case .tasks: return "Tasks"
        case .monitors: return "Monitors"
        case .systemWork: return "System work"
        case .autopilot: return "Autopilot"
        case .cycles: return "Work cycles"
        case .focus: return "Focus areas"
        case .delegation: return "Delegation"
        }
    }
    var explanation: String {
        switch self {
        case .allWork: return "Track tasks, scheduled checks and system activity in one place."
        case .needsYou: return "Answer questions, review plans and unblock work waiting for your decision."
        case .crew: return "Your AI team. Give each member a role, tools and responsibilities, then see what they are working on."
        case .goals: return "Your goal workspaces: goals, the sub-goals under them and who owns each. Set one up from a template in chat."
        case .playbooks: return "Reusable goals, instructions and success checks for your crew. Use them to guide ongoing work and track progress."
        case .quality: return "Have agents review and improve an output against your criteria, then inspect the result before accepting it."
        case .operations: return "Control the shared schedule for company roles, start their sequence manually and inspect problems. Individual crew schedules have separate controls."
        case .tasks: return "Give an agent a specific outcome to deliver, once or on a schedule."
        case .monitors: return "Watch for changes on a schedule and notify you when a condition needs attention."
        case .systemWork: return "Background activity created by Magican, including crew cycles and supporting jobs."
        case .autopilot: return "Let a crew member use its playbook to choose the next useful work. Permissions, budgets and approval rules still apply."
        case .cycles: return "Scheduled check-ins where a crew member reviews its responsibilities and starts work within its task and step limits."
        case .focus: return "A crew member’s ongoing responsibilities. Give each area a priority, instructions and an optional schedule."
        case .delegation: return "Choose which teammates this member can ask for help, and how far work can be passed along."
        }
    }
    static func forLabel(_ label: String) -> Self? {
        let text = label.lowercased()
        if text.contains("autopilot") { return .autopilot }
        if text.contains("team routine") { return .operations }
        if text.contains("playbook") { return .playbooks }
        if text.contains("focus area") { return .focus }
        if text.contains("cycle") || text.contains("scheduled check-in") { return .cycles }
        if text.contains("delegation") { return .delegation }
        return allCases.first { $0.title.lowercased() == text }
    }
}

extension WorkSection {
    var help: WorkHelpTopic {
        switch self {
        case .all: return .allWork
        case .needsYou: return .needsYou
        case .crew: return .crew
        case .goals: return .goals
        case .programs: return .playbooks
        case .quality: return .quality
        case .operations: return .operations
        }
    }
}

/// Unknown definition fields survive a native editor round trip.
indirect enum WorkJSON: Codable, Equatable, Sendable {
    case object([String: WorkJSON]), array([WorkJSON]), string(String), integer(Int64), unsigned(UInt64), number(Double), bool(Bool), null
    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let bool = try? value.decode(Bool.self) { self = .bool(bool) }
        else if let integer = try? value.decode(Int64.self) { self = .integer(integer) }
        else if let integer = try? value.decode(UInt64.self) { self = .unsigned(integer) }
        else if let number = try? value.decode(Double.self) { self = .number(number) }
        else if let text = try? value.decode(String.self) { self = .string(text) }
        else if let list = try? value.decode([WorkJSON].self) { self = .array(list) }
        else { self = .object(try value.decode([String: WorkJSON].self)) }
    }
    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .object(let object): try value.encode(object)
        case .array(let array): try value.encode(array)
        case .string(let string): try value.encode(string)
        case .integer(let integer): try value.encode(integer)
        case .unsigned(let integer): try value.encode(integer)
        case .number(let number): try value.encode(number)
        case .bool(let bool): try value.encode(bool)
        case .null: try value.encodeNil()
        }
    }
    var object: [String: WorkJSON]? { if case .object(let value) = self { return value }; return nil }
    var text: String? { if case .string(let value) = self { return value }; return nil }
    var flag: Bool? { if case .bool(let value) = self { return value }; return nil }
    var array: [WorkJSON]? { if case .array(let value) = self { return value }; return nil }
    var formatted: String {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? encoder.encode(self)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
}

struct WorkCrewMember: Identifiable, Equatable, Sendable {
    let id: String
    let definition: [String: WorkJSON]
    let etag: String
    let status: String
    let cycleID: String?
    let executionID: String?
    var cycleTaskID: String? {
        guard let cycleID, !cycleID.isEmpty, let executionID, !executionID.isEmpty else { return nil }
        return "agent-cycle:\(id):\(cycleID)"
    }
    var name: String { definition["name"]?.text ?? id }
    var kind: String { definition["kind"]?.text ?? "" }
    var disabled: Bool { definition["disabled"]?.flag ?? false }
    var isPrimary: Bool { definition["is_primary"]?.flag ?? false }
    var description: String { definition["description"]?.text ?? "" }

    init(_ value: WorkJSON, etagHeader: String = "") throws {
        guard let record = value.object else { throw WorkAPIError.invalidResponse }
        let definition = record["definition"]?.object ?? record
        guard let id = definition["agent_id"]?.text ?? definition["id"]?.text, !id.isEmpty else {
            throw WorkAPIError.invalidResponse
        }
        self.id = id; self.definition = definition
        etag = record["etag"]?.text ?? etagHeader
        status = record["status"]?.text ?? "unknown"
        cycleID = record["current_cycle_id"]?.text?.trimmingCharacters(in: .whitespacesAndNewlines)
        executionID = record["current_execution_id"]?.text?.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

func workDelegationCycle(origin: String, target: String, members: [WorkCrewMember]) -> [String]? {
    let graph = Dictionary(uniqueKeysWithValues: members.map { ($0.id, $0.definition["delegation_targets"]?.array?.compactMap(\.text) ?? []) })
    guard graph[target] != nil else { return nil }
    var parents: [String: String] = [:], visited: Set<String> = [target]
    var queue = [target], index = 0
    while index < queue.count {
        let node = queue[index]; index += 1
        if node == origin {
            var path = [node], step = node
            while let parent = parents[step] { path.append(parent); step = parent }
            return [origin] + path.reversed()
        }
        for next in graph[node] ?? [] where graph[next] != nil && !visited.contains(next) {
            visited.insert(next); parents[next] = node; queue.append(next)
        }
    }
    return nil
}

struct WorkProgramSummary: Codable, Identifiable, Equatable, Sendable {
    let name: String
    let title: String
    var id: String { name }
}

struct WorkProgramDocument: Decodable, Equatable, Sendable {
    struct Version: Decodable, Identifiable, Equatable, Sendable {
        let snapshot: String
        let modified: String
        var id: String { snapshot }
    }
    let name: String
    let title: String
    let content: String
    let history: [Version]
    /// Hex SHA-256 of `content`, sent back as the base of an owner save.
    /// Empty from a runtime that predates owner edits.
    let sha256: String
    private enum CodingKeys: String, CodingKey { case name, title, content, history, sha256 }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        title = (try? c.decodeIfPresent(String.self, forKey: .title)) ?? name
        content = try c.decode(String.self, forKey: .content)
        history = (try? c.decodeIfPresent([Version].self, forKey: .history)) ?? []
        sha256 = (try? c.decodeIfPresent(String.self, forKey: .sha256)) ?? ""
    }
}

/// `PUT /programs/{name}` success: the stored document's new hash.
struct WorkProgramSaveResult: Equatable, Sendable {
    let program: String
    let created: Bool
    let unchanged: Bool
    let historySnapshot: String?
    let sha256: String
    init(_ value: WorkJSON) {
        program = value["program"].text ?? ""
        created = value["created"].flag ?? false
        unchanged = value["unchanged"].flag ?? false
        historySnapshot = value["history_snapshot"].text
        sha256 = value["sha256"].text ?? ""
    }
}

/// A rejected program save. `changed` is the 409 `program_changed` answer:
/// the document changed, appeared or vanished after it was opened.
enum WorkProgramSaveError: LocalizedError, Equatable {
    case changed(message: String, currentSHA256: String?)
    case missingBase
    case rejected(String)
    var errorDescription: String? {
        switch self {
        case .changed(let message, _): return message
        case .missingBase: return "Reload this document before editing it."
        case .rejected(let message): return message
        }
    }
}

enum WorkAPIError: LocalizedError {
    case unconfigured, invalidResponse, missingVersion, http(Int, String)
    var errorDescription: String? {
        switch self {
        case .unconfigured: return "Connect to your Magician in Settings."
        case .invalidResponse: return "This work response could not be read. Refresh to try again."
        case .missingVersion: return "Refresh the crew member before saving changes."
        case .http(let status, let message):
            if status == 412 { return "This crew member changed elsewhere. Refresh before saving again." }
            if status == 401 || status == 403 { return "Your connection cannot access this work. Check Settings." }
            return message.isEmpty ? "Work request failed (\(status))." : message
        }
    }
}

/// An immutable connection snapshot. A machine switch cannot mix a new bearer
/// with an old URL; the host model also discards responses from a previous scope.
struct WorkAPIClient {
    let baseURL: URL
    private let headers: [String: String]
    private let session: URLSession
    private let currentCheck: (() -> Bool)?
    init(baseURL: URL, headers: [String: String], session: URLSession = .shared, currentCheck: (() -> Bool)? = nil) {
        self.baseURL = baseURL; self.headers = headers; self.session = session; self.currentCheck = currentCheck
    }
    private static let runtimeSession = URLSession(configuration: .ephemeral, delegate: WorkHTTPRedirectDelegate(), delegateQueue: nil)
    static func connected(session: URLSession? = nil) throws -> WorkAPIClient {
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--work-ui-test-fixture") { return WorkFixtureProtocol.client() }
#endif
        guard let profile = MagicianAccess.connectionProfile else { throw WorkAPIError.unconfigured }
        return WorkAPIClient(baseURL: profile.publicOrigin, headers: MagicianAccess.headers(profile: profile), session: session ?? runtimeSession)
    }
    var isCurrentConnection: Bool {
        if let currentCheck { return currentCheck() }
        guard let profile = MagicianAccess.connectionProfile else { return false }
        return profile.publicOrigin == baseURL && MagicianAccess.headers(profile: profile) == headers
    }
    static func segment(_ value: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#%")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? ""
    }
    static func query(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? ""
    }
    /// One round trip with any status; the caller interprets the answer.
    private func send(_ path: String, method: String, body: WorkJSON?, etag: String?) async throws -> (WorkJSON?, HTTPURLResponse) {
        let bootstrapRead = method == "GET" && path.range(of: "^/api/magician/v3/tasks/bootstrap__[^/?]+$", options: .regularExpression) != nil
        guard path.hasPrefix("/api/magician/v2/") || bootstrapRead else { throw WorkAPIError.invalidResponse }
        guard let url = URL(string: baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + path) else {
            throw WorkAPIError.invalidResponse
        }
        var request = URLRequest(url: url); request.httpMethod = method; request.timeoutInterval = 30
        if path == "/api/magician/v2/analytics/llm_calls/query",
           body?["sql"].text?.contains("llm_cost_daily") == true {
            request.timeoutInterval = 150
        }
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        if let body { request.httpBody = try JSONEncoder().encode(body); request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        if let etag { request.setValue(etag, forHTTPHeaderField: "If-Match") }
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse else { throw WorkAPIError.invalidResponse }
        return (try? JSONDecoder().decode(WorkJSON.self, from: data), response)
    }
    func request(_ path: String, method: String = "GET", body: WorkJSON? = nil, etag: String? = nil) async throws -> (WorkJSON, HTTPURLResponse) {
        let (value, response) = try await send(path, method: method, body: body, etag: etag)
        guard (200..<300).contains(response.statusCode) else {
            throw WorkAPIError.http(response.statusCode, value?["error"].text ?? value?["error"]["message"].text ?? value?["message"].text ?? "")
        }
        guard let value else { throw WorkAPIError.invalidResponse }
        return (value, response)
    }
    /// One request (a GET unless named) with any status and its JSON body, for callers
    /// that read a 404 as "absent" or a 409 as "changed" and word failures themselves
    /// (the Goals notes reads and their front-matter edits).
    func status(_ path: String, method: String = "GET", body: WorkJSON? = nil) async throws -> (status: Int, value: WorkJSON?) {
        let (value, response) = try await send(path, method: method, body: body, etag: nil)
        return (response.statusCode, value)
    }
    func file(_ path: String, maximumBytes: Int = 128 * 1024 * 1024) async throws -> Data {
        guard (1...(128 * 1024 * 1024)).contains(maximumBytes) else { throw WorkAPIError.invalidResponse }
        guard path.hasPrefix("/api/magician/v2/") || path.range(of: "^/api/magician/v3/tasks/[^/?]+/outputs/[^?]+$", options: .regularExpression) != nil, let url = URL(string: baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + path) else { throw WorkAPIError.invalidResponse }
        var request = URLRequest(url: url); request.timeoutInterval = 60
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        let (bytes, response) = try await session.bytes(for: request)
        defer { bytes.task.cancel() }
        guard let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else { throw WorkAPIError.invalidResponse }
        guard response.expectedContentLength <= maximumBytes else { throw WorkAPIError.http(0, "This artifact is too large to preview on this device.") }
        var data = Data()
        data.reserveCapacity(response.expectedContentLength > 0 ? Int(response.expectedContentLength) : min(65536, maximumBytes))
        var chunk = [UInt8](); chunk.reserveCapacity(min(8192, maximumBytes))
        for try await byte in bytes {
            // Count delivered bytes too: a size header may be absent or wrong.
            guard data.count + chunk.count < maximumBytes else { throw WorkAPIError.http(0, "This artifact is too large to preview on this device.") }
            chunk.append(byte)
            if chunk.count == 8192 {
                try Task.checkCancellation()
                data.append(contentsOf: chunk); chunk.removeAll(keepingCapacity: true)
            }
        }
        data.append(contentsOf: chunk)
        return data
    }
    func crew() async throws -> [WorkCrewMember] {
        let (value, _) = try await request("/api/magician/v2/agents")
        guard let rows = value.object?["agents"]?.array ?? value.array else { throw WorkAPIError.invalidResponse }
        return try rows.map { try WorkCrewMember($0) }
    }
    func member(_ id: String) async throws -> WorkCrewMember {
        let (value, response) = try await request("/api/magician/v2/agents/\(Self.segment(id))")
        return try WorkCrewMember(value, etagHeader: response.value(forHTTPHeaderField: "ETag") ?? "")
    }
    func patchMember(_ id: String, patch: [String: WorkJSON], etag: String) async throws -> WorkCrewMember {
        guard !etag.isEmpty else { throw WorkAPIError.missingVersion }
        let (value, response) = try await request("/api/magician/v2/agents/\(Self.segment(id))", method: "PATCH", body: .object(patch), etag: etag)
        return try WorkCrewMember(value, etagHeader: response.value(forHTTPHeaderField: "ETag") ?? "")
    }
    enum AgentAction: String { case pause, resume, trigger; case setPrimary = "set-primary" }
    func actOnMember(_ id: String, action: AgentAction) async throws {
        _ = try await request("/api/magician/v2/agents/\(Self.segment(id))/\(action.rawValue)", method: "POST", body: action == .trigger ? .object([:]) : nil)
    }
    func programs() async throws -> [WorkProgramSummary] {
        let (value, _) = try await request("/api/magician/v2/programs")
        guard let rows = value.object?["programs"] else { throw WorkAPIError.invalidResponse }
        return try JSONDecoder().decode([WorkProgramSummary].self, from: JSONEncoder().encode(rows))
    }
    func program(_ name: String) async throws -> WorkProgramDocument {
        let (value, _) = try await request("/api/magician/v2/programs/\(Self.segment(name))")
        return try JSONDecoder().decode(WorkProgramDocument.self, from: JSONEncoder().encode(value))
    }
    func restoreProgram(_ name: String, snapshot: String) async throws {
        _ = try await request("/api/magician/v2/programs/\(Self.segment(name))/revert", method: "POST", body: .object(["snapshot": .string(snapshot)]))
    }
    /// The owner's whole-document save. `baseSHA256` is the `sha256` of the
    /// version opened for edit; a document that changed since answers 409
    /// and is never overwritten. Only a signed-in owner session may save, so
    /// the server's own refusal text is surfaced as is.
    func saveProgram(name: String, content: String, baseSHA256: String) async throws -> WorkProgramSaveResult {
        guard !baseSHA256.isEmpty else { throw WorkProgramSaveError.missingBase }
        let (value, response) = try await send("/api/magician/v2/programs/\(Self.segment(name))", method: "PUT",
                                               body: .object(["content": .string(content), "base_sha256": .string(baseSHA256)]), etag: nil)
        if response.statusCode == 409, value?["error"].text == "program_changed" {
            throw WorkProgramSaveError.changed(message: value?["message"].text ?? "This playbook changed after you opened it.",
                                               currentSHA256: value?["current_sha256"].text)
        }
        guard (200..<300).contains(response.statusCode) else {
            let message = value?["message"].text ?? value?["error"].text ?? value?["error"]["message"].text ?? ""
            throw WorkProgramSaveError.rejected(message.isEmpty ? "Playbook save failed (\(response.statusCode))." : message)
        }
        guard let value, value.object != nil else { throw WorkAPIError.invalidResponse }
        return WorkProgramSaveResult(value)
    }
}

/// Runtime credentials must never follow a redirect to another host.
private final class WorkHTTPRedirectDelegate: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Guided changes update only their own field; provider fields and integer
/// budgets survive every round trip, including fields nested in focus areas.
extension WorkJSON {
    func at(_ path: [String]) -> WorkJSON {
        path.reduce(self) { value, key in value.object?[key] ?? .null }
    }
    func setting(_ path: [String], to value: WorkJSON?) -> WorkJSON {
        guard let key = path.first else { return value ?? .null }
        var fields = object ?? [:]
        if path.count == 1 { fields[key] = value }
        else { fields[key] = (fields[key] ?? .object([:])).setting(Array(path.dropFirst()), to: value) }
        return .object(fields)
    }
    static var newCrewDefinition: WorkJSON {
        .object(["agent_id": .string(""), "name": .string(""), "description": .string(""),
                 "persona": .string("You are a practical autonomous assistant."), "trust_level": .string("local"),
                 "kind": .string("worker"), "tools": .array([]), "excluded_tools": .array([]),
                 "delegation_targets": .array([]), "constraints": .object(["coordination": .object(["max_delegation_depth": .integer(3)])])])
    }
    /// A crew definition's `kind` is `personal` on the server (snake_case);
    /// older fixtures and drafts may carry `Personal`.
    var isPersonalCrew: Bool { self["kind"].text?.lowercased() == "personal" }
    func crewValidation(editingID: String? = nil) -> String? {
        if let editingID, self["agent_id"].text != editingID { return "Keep the crew member identity unchanged." }
        let id = self["agent_id"].text ?? ""
        if !id.isEmpty && id.range(of: "^[a-z0-9]+(?:-[a-z0-9]+)*$", options: .regularExpression) == nil { return "Use lowercase letters, numbers and hyphens for the member ID." }
        for key in ["name", "persona", "trust_level"] where (self[key].text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "\(key.replacingOccurrences(of: "_", with: " ").capitalized) is required." }
        guard ["personal", "worker"].contains((self["kind"].text ?? "").lowercased()) else { return "Choose Personal or Worker." }
        let depth = Int(self.at(["constraints", "coordination", "max_delegation_depth"]).display) ?? 3
        if !(1...10).contains(depth) { return "Delegation depth must be between 1 and 10." }
        if self["autonomous_config"].object != nil {
            guard isPersonalCrew else { return "Autonomous cycles require a Personal member." }
            let auto = self["autonomous_config"]
            if (auto["schedule"].text ?? "").split(whereSeparator: { $0.isWhitespace }).count != 5 { return "Enter a five-field cron schedule for autonomous cycles." }
            guard let areas = auto["focus_areas"].array, !areas.isEmpty else { return "Add at least one focus area." }
            for area in areas {
                if (area["name"].text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || (area["description"].text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "Each focus area needs a name and description." }
                let cron = area["schedule"].text ?? ""
                if !cron.isEmpty && cron.split(whereSeparator: { $0.isWhitespace }).count != 5 { return "Focus schedules need five cron fields." }
            }
            if !(1...10).contains(Int(auto["max_tasks_per_cycle"].display) ?? 3) { return "Tasks per cycle must be between 1 and 10." }
            if !(1...30).contains(Int(auto["max_steps_per_plan"].display) ?? 10) { return "Steps per plan must be between 1 and 30." }
        }
        return nil
    }
}


/// A focus area's `shared_programs` has three distinct states that must
/// survive a round trip: absent (or null) follows the global
/// `harness.shared_program_docs` list, `[]` means no shared documents, and a
/// non-empty list replaces the global list for that area.
enum WorkSharedProgramsMode: String, CaseIterable, Identifiable {
    case global, none, choose
    var id: String { rawValue }
    var title: String {
        switch self {
        case .global: return "Use global list"
        case .none: return "None"
        case .choose: return "Choose documents"
        }
    }
    var hint: String {
        switch self {
        case .global: return "Follows the shared documents in autopilot settings."
        case .none: return "This area gets no shared documents."
        case .choose: return "These documents replace the global list for this area."
        }
    }
}

extension WorkJSON {
    static let sharedProgramsKey = "shared_programs"
    /// The stored names, trimmed and de-duplicated in order; nil when the key
    /// is absent or null (the global list).
    var sharedPrograms: [String]? {
        guard let list = self[Self.sharedProgramsKey].array else { return nil }
        var names: [String] = []
        for case .string(let raw) in list {
            let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !name.isEmpty && !names.contains(name) { names.append(name) }
        }
        return names
    }
    /// The stored state. `[]` reads as `.none` unless the editor is in
    /// "Choose documents" with nothing checked yet, which stores `[]` too.
    func sharedProgramsMode(choosing: Bool = false) -> WorkSharedProgramsMode {
        guard let names = sharedPrograms else { return .global }
        return names.isEmpty && !choosing ? .none : .choose
    }
    /// The area with `mode` applied: `.global` removes the key, `.none` writes
    /// `[]`, `.choose` writes `selected` (`[]` when nothing is checked).
    /// Every other field of the area is left as it was.
    func settingSharedPrograms(_ mode: WorkSharedProgramsMode, selected: [String] = []) -> WorkJSON {
        switch mode {
        case .global: return setting([Self.sharedProgramsKey], to: nil)
        case .none: return setting([Self.sharedProgramsKey], to: .array([]))
        case .choose:
            var names: [String] = []
            for name in selected.map({ $0.trimmingCharacters(in: .whitespacesAndNewlines) }) where !name.isEmpty && !names.contains(name) { names.append(name) }
            return setting([Self.sharedProgramsKey], to: .array(names.map(WorkJSON.string)))
        }
    }
    /// Checks or unchecks one document while choosing.
    func togglingSharedProgram(_ name: String, on: Bool) -> WorkJSON {
        var names = (sharedPrograms ?? []).filter { $0 != name }
        if on { names.append(name) }
        return settingSharedPrograms(.choose, selected: names)
    }
}

extension WorkJSON {
    /// RFC 7386: absent draft fields require explicit nulls in the patch.
    func mergePatch(from original: WorkJSON) -> WorkJSON {
        guard let next = object, let previous = original.object else { return self }
        var patch: [String: WorkJSON] = [:]
        for key in Set(next.keys).union(previous.keys) where next[key] != previous[key] {
            patch[key] = next[key].map { $0.mergePatch(from: previous[key] ?? .null) } ?? .null
        }
        return .object(patch)
    }
}

/// Canonical Work and legacy URLs resolve to existing native records.
struct WorkNavigationTarget: Hashable, Identifiable {
    enum Kind: String { case all, needsYou, task, monitor, crew, member, editMember, addMember, rules, memory, goals, programs, program, quality, qualityRun, qualityProposal, qualityCampaign, operations, thread }
    let kind: Kind
    var recordID = ""
    var executionID: String? = nil
    var goalID: String? = nil
    var tier = ""
    var plan = false
    var internalWork = false
    var threadID: String? = nil
    var agentID: String? = nil
    var updateID: String? = nil
    /// `/work?view=goals&goals=closed`: open Goals on its Closed list.
    var closedGoals = false
    var id: String { "\(kind.rawValue)|\(recordID)|\(executionID ?? "")|\(goalID ?? "")|\(tier)|\(threadID ?? "")|\(plan)|\(closedGoals)" }
    var section: WorkSection {
        switch kind {
        case .crew, .member, .editMember, .addMember, .rules, .memory: return .crew
        case .goals: return .goals
        case .programs, .program: return .programs
        case .quality, .qualityRun, .qualityProposal, .qualityCampaign: return .quality
        case .operations: return .operations
        case .needsYou: return .needsYou
        default: return .all
        }
    }
    static func parse(_ raw: String) -> Self? {
        guard let parts = URLComponents(string: raw) else { return nil }
        if let scheme = parts.scheme, !["http", "https"].contains(scheme.lowercased()) && !MagicanAppURL.isScheme(scheme) { return nil }
        var path = parts.percentEncodedPath
        if MagicanAppURL.isScheme(parts.scheme) {
            if parts.host == "task" || parts.host == "monitor" {
                let id = path.split(separator: "/").first.flatMap { String($0).removingPercentEncoding } ?? ""
                guard !id.isEmpty else { return nil }
                let query = parts.queryItems ?? []
                return Self(kind: parts.host == "task" ? .task : .monitor, recordID: id,
                            executionID: query.first { $0.name == "execution_id" }?.value,
                            updateID: query.first { $0.name == "update" }?.value)
            }
            guard parts.host == "work" else { return nil }
            path = "/work" + path
        }
        path = path.count > 1 && path.hasSuffix("/") ? String(path.dropLast()) : path
        let query = parts.queryItems ?? []
        func q(_ name: String) -> String? {
            guard let value = query.first(where: { $0.name == name })?.value?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty else { return nil }; return value
        }
        let segments = path.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
        func taskTarget(_ id: String, thread: String? = nil, threadScoped: Bool = false) -> Self {
            Self(kind: !threadScoped && q("type") == "monitors" ? .monitor : .task, recordID: id,
                 executionID: q("execution_id") ?? q("execution"), plan: q("plan") == "open",
                 internalWork: !threadScoped && q("type") == "internal", threadID: thread ?? q("thread"), updateID: q("update"))
        }
        // Retired path-style links still exist in saved chat and feed content.
        // Decode each identity once and prefer a path's task over query selection.
        if segments.count == 2 && segments[0] == "tasks" {
            return taskTarget(segments[1])
        }
        if (segments.count == 3 || segments.count == 4) && segments[0] == "t" && segments[2] == "tasks" {
            let thread = segments[1].trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if let id = segments.count == 4 ? segments[3] : q("selected") {
                return taskTarget(id, thread: thread, threadScoped: true)
            }
            return Self(kind: .thread, recordID: thread)
        }
        if segments.first == "crew" || (segments.first == "work" && segments.dropFirst().first == "crew") {
            let crew = segments.first == "crew" ? Array(segments.dropFirst()) : Array(segments.dropFirst(2))
            if crew.isEmpty { return Self(kind: .crew) }
            if crew[0] == "new" && crew.count == 1 { return q("edit").map { Self(kind: .editMember, recordID: $0) } ?? Self(kind: .addMember) }
            if crew.count == 1 { return Self(kind: .member, recordID: crew[0], agentID: crew[0]) }
            if crew.count == 2 && crew[1] == "edit" { return Self(kind: .editMember, recordID: crew[0]) }
            if crew.count == 2 && crew[1] == "rules" { return Self(kind: .rules, recordID: crew[0]) }
            if crew.count == 3 && crew[1] == "memory" { return Self(kind: .memory, recordID: crew[0], goalID: q("goal_id"), tier: crew[2]) }
            return nil
        }
        if path == "/harness" { return Self(kind: .operations) }
        guard ["/work", "/tasks", "/relentless"].contains(path) else { return nil }
        let view = path == "/relentless" ? "quality" : q("view")
        if view == "crew" { return q("agent").map { Self(kind: .member, recordID: $0, agentID: $0) } ?? Self(kind: .crew) }
        if view == "programs" { return q("program").map { Self(kind: .program, recordID: $0) } ?? Self(kind: .programs) }
        if view == "quality" {
            if let run = q("run") { return Self(kind: .qualityRun, recordID: run) }
            if let proposal = q("proposal") { return Self(kind: .qualityProposal, recordID: proposal) }
            if let campaign = q("campaign") { return Self(kind: .qualityCampaign, recordID: campaign) }
            return Self(kind: .quality, recordID: q("task") ?? "", agentID: q("agent"))
        }
        if view == "operations" { return Self(kind: .operations) }
        if view == "goals" { return Self(kind: .goals, closedGoals: q("goals") == "closed") }
        if view == "needs-you" { return Self(kind: .needsYou) }
        guard view == nil || ["all", "inbox", "today", "overdue", "running", "completed"].contains(view!) else { return nil }
        if let selected = q("selected") {
            return taskTarget(selected)
        }
        if let thread = q("thread") { return Self(kind: .thread, recordID: thread) }
        // `magican://work?task=…`: a Work task's link on the "Magician at work"
        // and followed-item Live Activities.
        if view == nil, let task = q("task") { return Self(kind: .task, recordID: task) }
        return Self(kind: .all)
    }
}
