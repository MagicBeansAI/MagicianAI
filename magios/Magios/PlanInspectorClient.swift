import Foundation

/// The Plan Inspector's API: the task plan endpoints the web inspector uses
/// (`routes/(app)/ExecutionPlanInspector.svelte`, `taskStore.ts`
/// approve/reject/replan). Requests are built here and sent through a
/// `PlanInspectorTransport` — the app's authorised session in the app, a stub
/// in tests and DEBUG fixtures.

// MARK: - Wire records

enum PlanStatus: String, Equatable {
    case planning, draft, eliciting, approved, rejected, failed

    /// Editing is open while the plan is not running or settled. Approved
    /// plans stay editable (web: "approve, then notice a step needs a tweak");
    /// saving writes a new version.
    var canEdit: Bool { self == .draft || self == .eliciting || self == .approved }

    var label: String { rawValue.prefix(1).uppercased() + rawValue.dropFirst() }
}

struct PlanQuestionOption: Equatable, Identifiable {
    let value: String
    let label: String
    let description: String?
    var id: String { value }
}

struct PlanQuestion: Equatable, Identifiable {
    let id: String
    let text: String
    let options: [PlanQuestionOption]
    let sourceSlotId: String?
    let urgency: Double?
}

/// The `/plan` envelope (`TaskPlanRecord`).
struct PlanRecord: Equatable {
    let planId: String?
    let status: PlanStatus?
    let rawStatus: String?
    let graph: PlanGraph?
    let pendingQuestions: [PlanQuestion]
    let error: String?

    init(json: PlanJSON?) {
        let object = json?.objectValue ?? [:]
        planId = object["plan_id"]?.stringValue.flatMap { $0.isEmpty ? nil : $0 }
        rawStatus = object["status"]?.stringValue
        status = rawStatus.flatMap { PlanStatus(rawValue: $0.lowercased()) }
        graph = PlanGraph(json: object["plan_graph"])
        pendingQuestions = (object["pending_questions"]?.arrayValue ?? []).compactMap { value in
            guard let id = value["id"]?.stringValue else { return nil }
            return PlanQuestion(
                id: id,
                text: value["question_text"]?.stringValue ?? "",
                options: (value["options"]?.arrayValue ?? []).compactMap { option in
                    guard let optionValue = option["value"]?.stringValue else { return nil }
                    return PlanQuestionOption(value: optionValue,
                                              label: option["label"]?.stringValue ?? optionValue,
                                              description: option["description"]?.stringValue)
                },
                sourceSlotId: value["source_slot_id"]?.stringValue,
                urgency: value["urgency"]?.doubleValue)
        }
        error = object["error"]?.stringValue
    }
}

struct PlanVersionSummary: Equatable, Identifiable {
    let epochMs: Int64
    let label: String?
    let stepCount: Int
    let confidence: Double?
    var id: Int64 { epochMs }

    var title: String {
        let trimmed = label?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? "Saved version" : trimmed
    }

    var date: Date { Date(timeIntervalSince1970: Double(epochMs) / 1000) }
}

struct PlanVersionsPage: Equatable {
    let versions: [PlanVersionSummary]
    let hasMore: Bool
}

struct PlanSlotRecord: Equatable, Identifiable {
    let id: String
    let slotType: String
    let value: PlanJSON
    let confidence: Double
    let sources: [String]

    /// The id after the last `::` (slot ids are namespaced).
    var shortId: String { id.components(separatedBy: "::").last ?? id }
}

struct PlanSlotGroup: Equatable, Identifiable {
    let type: String
    let slots: [PlanSlotRecord]
    var id: String { type }
    var averageConfidence: Double {
        slots.isEmpty ? 0 : slots.reduce(0) { $0 + $1.confidence } / Double(slots.count)
    }
}

struct PlanSlotSnapshot: Equatable {
    let executionId: String?
    let slots: [PlanSlotRecord]
    let overallConfidence: Double?
    let unresolvedSlots: [String]
    let clarifiedTask: PlanJSON?
    let triggerMappings: PlanJSON?
    let boostResults: PlanJSON?
    let enrichmentSummary: PlanJSON?

    init(json: PlanJSON) {
        executionId = json["execution_id"]?.stringValue
        slots = (json["slots"]?.arrayValue ?? []).compactMap { value in
            guard let id = value["id"]?.stringValue else { return nil }
            return PlanSlotRecord(
                id: id,
                slotType: Self.slotType(value["slot_type"]),
                value: value["value"] ?? .null,
                confidence: value["confidence"]?.doubleValue ?? 0,
                sources: (value["provenance"]?.arrayValue ?? []).compactMap { $0["source"]?.stringValue })
        }
        let summary = json["confidence_summary"]
        overallConfidence = summary?["overall"]?.doubleValue
        unresolvedSlots = (summary?["unresolved_slots"]?.arrayValue ?? []).compactMap(\.stringValue)
        clarifiedTask = Self.nonEmpty(json["clarified_task"])
        triggerMappings = Self.nonEmpty(json["slot_trigger_mappings"])
        boostResults = Self.nonEmpty(json["confidence_boost_results"])
        enrichmentSummary = Self.nonEmpty(json["enrichment_summary"])
    }

    /// Slots grouped by type, the largest group first (web `computeSlotGroups`).
    var groups: [PlanSlotGroup] {
        var order: [String] = []
        var byType: [String: [PlanSlotRecord]] = [:]
        for slot in slots {
            if byType[slot.slotType] == nil { order.append(slot.slotType) }
            byType[slot.slotType, default: []].append(slot)
        }
        let groups: [PlanSlotGroup] = order.map { PlanSlotGroup(type: $0, slots: byType[$0] ?? []) }
        let indexed: [(offset: Int, group: PlanSlotGroup)] = groups.enumerated().map { ($0.offset, $0.element) }
        let sorted = indexed.sorted { lhs, rhs in
            let left = lhs.group.slots.count
            let right = rhs.group.slots.count
            return left == right ? lhs.offset < rhs.offset : left > right
        }
        return sorted.map(\.group)
    }

    /// A slot type arrives as a string or an externally tagged enum.
    private static func slotType(_ value: PlanJSON?) -> String {
        if let text = value?.stringValue { return text.lowercased() }
        if let key = value?.objectValue?.keys.sorted().first { return key.lowercased() }
        return "other"
    }

    private static func nonEmpty(_ value: PlanJSON?) -> PlanJSON? {
        switch value {
        case nil, .null?: return nil
        case .array(let items)? where items.isEmpty: return nil
        case .object(let items)? where items.isEmpty: return nil
        default: return value
        }
    }
}

struct PlanStrategyAttempt: Equatable, Identifiable {
    let index: Int
    let strategyType: String?
    let attemptNumber: Int?
    let failureReason: String?
    let attemptedAt: Date?
    let explorationResult: PlanJSON?
    let resourcesUsed: PlanJSON?
    var id: Int { index }

    var title: String { "\(strategyType ?? "Strategy attempt") #\(attemptNumber ?? index + 1)" }
    var confidence: Double? { explorationResult?["confidence"]?.doubleValue }
}

struct PlanClarificationEntry: Equatable, Identifiable {
    let questionId: String
    let questionText: String
    let sourceSlotId: String?
    let stage: String
    let answeredAt: Date?
    let slotUpdates: [PlanJSON]
    var id: String { questionId }

    /// `waiting_user` → `Waiting User` (web `stageLabel`).
    var stageLabel: String {
        let words = stage.split(whereSeparator: { $0 == "_" || $0 == " " })
        guard !words.isEmpty else { return "Unknown" }
        return words.map { $0.prefix(1).uppercased() + $0.dropFirst().lowercased() }.joined(separator: " ")
    }
}

/// The Advanced → manual resume form, validated (web `handleManualResume`).
struct PlanManualResume: Equatable {
    var handledQuestion: String = ""
    var updatedConfidence: String = ""
    var slotsJSON: String = ""

    enum Invalid: Error, Equatable, LocalizedError {
        case confidence, slotsNotArray, slotMissingFields(Int), slotsNotJSON
        var errorDescription: String? {
            switch self {
            case .confidence: return "Updated confidence must be a number between 0 and 1."
            case .slotsNotArray: return "Manual resume slots must be a JSON array."
            case .slotMissingFields(let index): return "Manual resume slot \(index) must include slot_id and slot_type."
            case .slotsNotJSON: return "The slots are not valid JSON."
            }
        }
    }

    /// The POST body, or the first validation error.
    func payload() throws -> [String: PlanJSON] {
        var body: [String: PlanJSON] = [:]
        let question = handledQuestion.trimmingCharacters(in: .whitespacesAndNewlines)
        if !question.isEmpty { body["handled_question"] = .string(question) }
        let confidenceText = updatedConfidence.trimmingCharacters(in: .whitespacesAndNewlines)
        if !confidenceText.isEmpty {
            guard let value = Double(confidenceText), value.isFinite, (0...1).contains(value) else { throw Invalid.confidence }
            body["updated_confidence"] = .number(value)
        }
        let slotsText = slotsJSON.trimmingCharacters(in: .whitespacesAndNewlines)
        if !slotsText.isEmpty {
            guard let parsed = PlanJSON.parse(slotsText) else { throw Invalid.slotsNotJSON }
            guard let items = parsed.arrayValue else { throw Invalid.slotsNotArray }
            let slots: [PlanJSON] = try items.enumerated().map { index, item in
                let slotId = item["slot_id"]?.stringValue?.trimmingCharacters(in: .whitespaces) ?? ""
                let slotType = item["slot_type"]?.stringValue?.trimmingCharacters(in: .whitespaces) ?? ""
                guard !slotId.isEmpty, !slotType.isEmpty else { throw Invalid.slotMissingFields(index + 1) }
                var slot: [String: PlanJSON] = [
                    "slot_id": .string(slotId), "slot_type": .string(slotType), "value": item["value"] ?? .null
                ]
                if let confidence = item["confidence"]?.doubleValue, confidence.isFinite { slot["confidence"] = .number(confidence) }
                return .object(slot)
            }
            if !slots.isEmpty { body["slots"] = .array(slots) }
        }
        return body
    }
}

// MARK: - Transport

struct PlanInspectorRequest: Equatable {
    var method: String
    /// From `/api/…`.
    var path: String
    var query: [URLQueryItem] = []
    var body: PlanJSON? = nil
}

struct PlanInspectorResponse {
    let status: Int
    let body: PlanJSON?
}

protocol PlanInspectorTransport {
    func send(_ request: PlanInspectorRequest) async throws -> PlanInspectorResponse
}

struct PlanInspectorError: LocalizedError, Equatable {
    let status: Int
    let message: String
    var errorDescription: String? { message }
}

/// The app's authorised session (`MagicianAccess`).
struct LivePlanInspectorTransport: PlanInspectorTransport {
    var session: URLSession = .shared

    func send(_ request: PlanInspectorRequest) async throws -> PlanInspectorResponse {
        guard var components = URLComponents(string: MagicianAccess.baseURL.absoluteString + request.path) else {
            throw URLError(.badURL)
        }
        if !request.query.isEmpty { components.queryItems = request.query }
        guard let url = components.url else { throw URLError(.badURL) }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = request.method
        urlRequest.timeoutInterval = 20
        if let body = request.body {
            urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
            urlRequest.httpBody = try JSONSerialization.data(withJSONObject: body.foundation, options: [.fragmentsAllowed])
        }
        MagicianAccess.authorize(&urlRequest)
        let (data, response) = try await session.data(for: urlRequest)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        return PlanInspectorResponse(status: status, body: data.isEmpty ? nil : PlanJSON.parse(data: data))
    }
}

// MARK: - Client

struct PlanInspectorClient {
    let taskId: String
    var transport: PlanInspectorTransport = LivePlanInspectorTransport()
    var channel = "ios"

    private static let segment = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
    private static func encode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: segment) ?? value
    }

    var planPath: String { "/api/magician/v3/tasks/\(Self.encode(taskId))/plan" }

    // MARK: Request building (pure; covered by tests)

    func loadPlanRequest() -> PlanInspectorRequest { .init(method: "GET", path: planPath) }

    func savePlanRequest(_ graph: PlanGraph, label: String?) -> PlanInspectorRequest {
        let trimmed = label?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return .init(method: "PUT", path: planPath, body: .object([
            "plan": .object(graph.savePayload()),
            "label": trimmed.isEmpty ? .null : .string(trimmed)
        ]))
    }

    func versionsRequest(limit: Int = 20) -> PlanInspectorRequest {
        .init(method: "GET", path: "\(planPath)/versions", query: [URLQueryItem(name: "limit", value: String(limit))])
    }

    func versionRequest(_ epochMs: Int64) -> PlanInspectorRequest {
        .init(method: "GET", path: "\(planPath)/versions/\(epochMs)")
    }

    func restoreRequest(_ epochMs: Int64) -> PlanInspectorRequest {
        .init(method: "POST", path: "\(planPath)/versions/\(epochMs)/restore")
    }

    func artifactRequest(_ name: String) -> PlanInspectorRequest { .init(method: "GET", path: "\(planPath)/\(name)") }

    func resumeRequest(_ form: PlanManualResume) throws -> PlanInspectorRequest {
        .init(method: "POST", path: "\(planPath)/clarifications/resume", body: .object(try form.payload()))
    }

    func answerRequest(questionId: String, text: String) -> PlanInspectorRequest {
        .init(method: "POST", path: "/api/magician/v2/hitl/\(Self.encode(questionId))/respond", body: .object([
            "source": .string("clarification"),
            "value": .object(["type": .string("text"), "value": .string(text)]),
            "channel": .string(channel),
            "task_id": .string(taskId)
        ]))
    }

    func decisionRequest(_ action: String, planId: String) -> PlanInspectorRequest {
        .init(method: "POST", path: "\(planPath)/\(action)", query: [URLQueryItem(name: "plan_id", value: planId)])
    }

    func replanRequest() -> PlanInspectorRequest { .init(method: "POST", path: "\(planPath)/replan") }

    func executeRequest() -> PlanInspectorRequest {
        .init(method: "POST", path: "/api/magician/v3/tasks/\(Self.encode(taskId))/execute", body: .object([:]))
    }

    func executionPanelRequest() -> PlanInspectorRequest {
        .init(method: "GET", path: "/api/magician/v3/tasks/\(Self.encode(taskId))/execution-panel")
    }

    // MARK: Calls

    /// The task's plan; nil when the task has none (404).
    func loadPlan() async throws -> PlanRecord? {
        let response = try await transport.send(loadPlanRequest())
        if response.status == 404 { return nil }
        return PlanRecord(json: try Self.checked(response)["plan"])
    }

    func savePlan(_ graph: PlanGraph, label: String?) async throws -> PlanRecord {
        PlanRecord(json: try Self.checked(try await transport.send(savePlanRequest(graph, label: label)))["plan"])
    }

    func versions(limit: Int = 20) async throws -> PlanVersionsPage {
        let body = try Self.checked(try await transport.send(versionsRequest(limit: limit)))
        let versions = (body["versions"]?.arrayValue ?? []).compactMap { value -> PlanVersionSummary? in
            guard let epoch = value["epoch_ms"]?.doubleValue else { return nil }
            return PlanVersionSummary(epochMs: Int64(epoch), label: value["label"]?.stringValue,
                                      stepCount: Int(value["step_count"]?.doubleValue ?? 0),
                                      confidence: value["confidence"]?.doubleValue)
        }
        return PlanVersionsPage(versions: versions, hasMore: body["has_more"] == .bool(true))
    }

    func version(_ epochMs: Int64) async throws -> PlanGraph? {
        PlanGraph(json: try Self.checked(try await transport.send(versionRequest(epochMs)))["version"]?["plan_graph"])
    }

    func restore(_ epochMs: Int64) async throws -> PlanRecord {
        PlanRecord(json: try Self.checked(try await transport.send(restoreRequest(epochMs)))["plan"])
    }

    func analysis() async throws -> PlanJSON? {
        let value = try Self.checked(try await transport.send(artifactRequest("analysis")))["query_analysis"]
        return value?.isNull == false ? value : nil
    }

    func slots() async throws -> PlanSlotSnapshot? {
        guard let value = try Self.checked(try await transport.send(artifactRequest("slots")))["slot_graph_snapshot"],
              value.objectValue != nil else { return nil }
        return PlanSlotSnapshot(json: value)
    }

    func attempts() async throws -> [PlanStrategyAttempt] {
        let items = try Self.checked(try await transport.send(artifactRequest("attempts")))["strategy_attempts"]?.arrayValue ?? []
        return items.enumerated().map { index, value in
            PlanStrategyAttempt(
                index: index,
                strategyType: value["strategy_type"]?.stringValue,
                attemptNumber: value["attempt_number"]?.doubleValue.map { Int($0) },
                failureReason: value["failure_reason"]?.stringValue,
                attemptedAt: value["attempted_at"]?.doubleValue.map { Date(timeIntervalSince1970: $0 / 1000) },
                explorationResult: value["exploration_result"].flatMap { $0.isNull ? nil : $0 },
                resourcesUsed: value["resources_used"].flatMap { $0.isNull ? nil : $0 })
        }
    }

    func clarifications() async throws -> [PlanClarificationEntry] {
        let items = try Self.checked(try await transport.send(artifactRequest("clarifications")))["clarification_history"]?.arrayValue ?? []
        return items.compactMap { value in
            guard let id = value["question_id"]?.stringValue else { return nil }
            return PlanClarificationEntry(
                questionId: id,
                questionText: value["question_text"]?.stringValue ?? "",
                sourceSlotId: value["source_slot_id"]?.stringValue,
                stage: value["stage"]?.stringValue ?? "",
                answeredAt: value["answered_at"]?.doubleValue.map { Date(timeIntervalSince1970: $0 / 1000) },
                slotUpdates: value["slot_updates"]?.arrayValue ?? [])
        }
    }

    /// Returns whether the planning workflow resumed.
    func resume(_ form: PlanManualResume) async throws -> Bool {
        try Self.checked(try await transport.send(try resumeRequest(form)))["workflow_resumed"] == .bool(true)
    }

    /// Answer a pending planning question. 409 means it was already answered,
    /// which counts as done.
    func answer(questionId: String, text: String) async throws {
        let response = try await transport.send(answerRequest(questionId: questionId, text: text))
        if response.status == 409 { return }
        _ = try Self.checked(response)
    }

    func approve(planId: String) async throws { _ = try Self.checked(try await transport.send(decisionRequest("approve", planId: planId))) }
    func reject(planId: String) async throws { _ = try Self.checked(try await transport.send(decisionRequest("reject", planId: planId))) }
    func replan() async throws { _ = try Self.checked(try await transport.send(replanRequest())) }
    func startRun() async throws { _ = try Self.checked(try await transport.send(executeRequest())) }

    /// The `/execution-panel` payload, as Foundation objects for `PlanSteps`.
    func executionPanel() async throws -> [String: Any]? {
        try Self.checked(try await transport.send(executionPanelRequest())).foundation as? [String: Any]
    }

    /// The body of a 2xx, else an error carrying the server's message.
    static func checked(_ response: PlanInspectorResponse) throws -> PlanJSON {
        guard (200..<300).contains(response.status) else {
            let message = response.body?["message"]?.stringValue
                ?? response.body?["error"]?.stringValue
                ?? "The request failed (HTTP \(response.status))."
            throw PlanInspectorError(status: response.status, message: message)
        }
        return response.body ?? .object([:])
    }
}
