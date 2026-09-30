import Foundation

// MARK: - Core Execution Panel Types

public struct ExecutionPanelOverview: Codable, Equatable {
    public let taskId: String
    public let executionId: String?
    public let principal: String
    public let workspace: String
    public let uiThreadId: String
    public let title: String
    public let description: String
    /// Raw backend task status, deliberately NOT an enum. The backend owns this
    /// vocabulary and has grown it before; a closed enum here made an unmodelled
    /// value throw, and since this type only ever decodes as a child of
    /// `ExecutionPanelDeltaEventData`, one unknown status discarded the whole
    /// delta — silently stalling the chat task card and the Live Activity.
    /// Mirrors `TaskV3.status`, which is a plain `String` for the same reason.
    public let status: String
    public let assignedAgentId: String
    public let activeAgentId: String?
    public let hasPlan: Bool
    public let progress: Double?
    public let currentStep: Int?
    public let createdAt: Int
    public let updatedAt: Int

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case executionId = "execution_id"
        case principal, workspace
        case uiThreadId = "ui_thread_id"
        case title, description, status
        case assignedAgentId = "assigned_agent_id"
        case activeAgentId = "active_agent_id"
        case hasPlan = "has_plan"
        case progress
        case currentStep = "current_step"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    /// Whether the run has reached a state it will never leave. Mirrors the
    /// backend's `TaskStatus::is_terminal`. An unknown status is treated as
    /// non-terminal: a run we don't understand is better left showing progress
    /// than declared finished.
    public var isTerminal: Bool {
        ["completed", "failed", "cancelled"].contains(status)
    }
}

public struct FeedItem: Codable, Equatable, Identifiable {
    public let id: String
    public let kind: String
    public let timestamp: Int
    public let title: String?
    public let content: String?
    public let agentId: String?
    
    enum CodingKeys: String, CodingKey {
        case id, kind, timestamp, title, content
        case agentId = "agent_id"
    }
}

public struct ExecutionPanelRunState: Codable, Equatable {
    public let summary: String?
    public let recentActivity: [FeedItem]
    public let activityLog: [FeedItem]?

    enum CodingKeys: String, CodingKey {
        case summary
        case recentActivity = "recent_activity"
        case activityLog = "activity_log"
    }
}

public struct ExecutionPanelOutputState: Codable, Equatable {
    public let deliveries: [FeedItem]
}

public struct ExecutionPanelDebugState: Codable, Equatable {
    public let latestErrorMessage: String?
    public let historyCount: Int

    enum CodingKeys: String, CodingKey {
        case latestErrorMessage = "latest_error_message"
        case historyCount = "history_count"
    }
}

public struct ExecutionPanelState: Codable, Equatable {
    public let defaultTab: String
    public let overview: ExecutionPanelOverview
    public let run: ExecutionPanelRunState
    public let output: ExecutionPanelOutputState
    public let debug: ExecutionPanelDebugState

    enum CodingKeys: String, CodingKey {
        case defaultTab = "default_tab"
        case overview, run, output, debug
    }
}

// MARK: - WebSocket Events

public struct ExecutionPanelDeltaEventData: Codable, Equatable {
    public let principal: String
    public let workspace: String
    public let taskId: String?
    public let executionId: String?
    public let state: ExecutionPanelState

    enum CodingKeys: String, CodingKey {
        case principal, workspace, state
        case taskId = "task_id"
        case executionId = "execution_id"
    }
}

/// Decode an `ExecutionPanelDelta` payload, reporting why on failure.
///
/// Both callers previously used a bare `try?`, so a rejected delta changed
/// nothing and said nothing — which is how a decoding mismatch survived
/// unnoticed. Failures are rare and always a client/backend contract drift, so
/// they are worth a log line rather than silence.
public func decodeExecutionPanelDelta(from data: Data) -> ExecutionPanelDeltaEventData? {
    do {
        return try JSONDecoder().decode(ExecutionPanelDeltaEventData.self, from: data)
    } catch {
        debugLog("Failed to decode ExecutionPanelDeltaEventData: \(error)")
        return nil
    }
}

public struct MessageCompletedEventData: Codable, Equatable {
    public let executionId: String
    public let turnId: String
    public let correlationId: String
    public let response: String

    enum CodingKeys: String, CodingKey {
        case executionId = "execution_id"
        case turnId = "turn_id"
        case correlationId = "correlation_id"
        case response
    }
}

public struct ExecutionPanelShellLine: Codable, Equatable {
    public let text: String
    public let stream: String
    public let timestamp: Int
}

/// Streaming shell output off the realtime bus, in the shape the wire
/// actually carries: a `data` string of newline-joined output. This type
/// decoded a `lines` array for as long as it existed, so it never decoded —
/// every chunk died inside a `try?` and the transcript's terminal stayed
/// empty. Found by the Android parity sweep, which had to read the Rust
/// event to deliver the chunks at all.
public struct ShellOutputChunkEventData: Codable, Equatable {
    public let executionId: String
    public let stepId: String
    /// The shell command; populated on the first chunk only, empty after.
    public let command: String
    /// "stdout" or "stderr".
    public let stream: String
    /// Batched output, possibly several newline-separated lines.
    public let data: String
    public let sequence: Int
    public let isFinal: Bool
    /// Present only on the final chunk.
    public let exitCode: Int?

    enum CodingKeys: String, CodingKey {
        case executionId = "execution_id"
        case stepId = "step_id"
        case command, stream, data, sequence
        case isFinal = "is_final"
        case exitCode = "exit_code"
    }

    /// The batch split the way the backend's own `parse_shell_lines` splits
    /// it: interior blank lines are real output, the empty artifact after a
    /// trailing newline is not.
    public var lines: [String] {
        guard !data.isEmpty else { return [] }
        let raw = data.components(separatedBy: "\n")
        return raw.enumerated().compactMap { index, value in
            value.isEmpty && index == raw.count - 1 ? nil : value
        }
    }
}

public struct V3PlanningStartedEventData: Codable, Equatable {
    public let principal: String
    public let workspace: String
    public let taskId: String
    public let taskTitle: String
    public let planId: String
    public let uiThreadId: String

    enum CodingKeys: String, CodingKey {
        case principal, workspace
        case taskId = "task_id"
        case taskTitle = "task_title"
        case planId = "plan_id"
        case uiThreadId = "ui_thread_id"
    }
}

public struct EscalationOptionData: Codable, Equatable {
    public let id: String
    public let label: String
    public let requiresInput: Bool?
    public let action: EscalationOptionActionData?

    public init(
        id: String,
        label: String,
        requiresInput: Bool?,
        action: EscalationOptionActionData? = nil
    ) {
        self.id = id
        self.label = label
        self.requiresInput = requiresInput
        self.action = action
    }
    
    enum CodingKeys: String, CodingKey {
        case id, label, action
        case requiresInput = "requires_input"
    }
}

/// Server-authored operation behind a chat escalation option. The backend owns
/// these semantics so clients never infer behavior from localized labels or IDs.
public struct EscalationOptionActionData: Codable, Equatable {
    public let type: String
    public let confirmed: Bool?

    public init(type: String, confirmed: Bool? = nil) {
        self.type = type
        self.confirmed = confirmed
    }
}

/// Canonical HITL schema retained on chat messages. This mirrors the metadata
/// already used by Attention so chat cards preserve the same affordances.
/// The backend's value-free classification of an ask that collects a secret
/// (Rust `SensitiveInputSpec`, published as `input_schema.sensitive` on every
/// `hitl.requested` and pending listing since P3). A client masks by this —
/// never by the request-type name or the wording. `kind` describes a
/// single-value ask; `fields` lists a form's flagged ids and a field absent
/// from it is ordinary; `oneTime` material has a short collection deadline.
public struct ChatSensitiveSpecData: Codable, Equatable {
    public struct Field: Codable, Equatable {
        public let id: String
        public let kind: String
        public init(id: String, kind: String) {
            self.id = id
            self.kind = kind
        }
    }

    public let kind: String?
    public let fields: [Field]?
    public let provenance: String?
    public let oneTime: Bool?
    public let collectionDeadlineMs: Int64?
    public let challengeId: String?

    enum CodingKeys: String, CodingKey {
        case kind, fields, provenance
        case oneTime = "one_time"
        case collectionDeadlineMs = "collection_deadline_ms"
        case challengeId = "challenge_id"
    }

    public init(
        kind: String?,
        fields: [Field]? = nil,
        provenance: String? = nil,
        oneTime: Bool? = nil,
        collectionDeadlineMs: Int64? = nil,
        challengeId: String? = nil
    ) {
        self.kind = kind
        self.fields = fields
        self.provenance = provenance
        self.oneTime = oneTime
        self.collectionDeadlineMs = collectionDeadlineMs
        self.challengeId = challengeId
    }

    /// The flagged kind of one form field, or nil for an ordinary field.
    public func fieldKind(_ id: String) -> String? {
        fields?.first(where: { $0.id == id })?.kind
    }

    /// Whether the window for this material has closed.
    public func isExpired(now: Date = Date()) -> Bool {
        guard let deadline = collectionDeadlineMs, deadline > 0 else { return false }
        return Int64(now.timeIntervalSince1970 * 1000) >= deadline
    }
}

/// How an ask with the given widget type and classification is rendered.
///
/// The typed widget wins; a `text`/`guidance` ask the backend classified as a
/// secret is masked by its kind — a code as `otp`, anything else as `password`.
/// An identifier stays readable; only its handling changes. **The value posted
/// still follows the widget type** (a masked `text` ask posts `text`), which is
/// the type the pause expects; the backend routes it to custody by the spec.
public func hitlRenderKind(inputType: String, sensitiveKind: String?) -> String {
    guard let kind = sensitiveKind else { return inputType }
    guard inputType == "text" || inputType == "guidance" else { return inputType }
    switch kind {
    case "otp": return "otp"
    case "password", "other": return "password"
    default: return inputType
    }
}

/// Whether a field of this flagged kind renders masked.
public func hitlFieldIsMasked(sensitiveKind: String?) -> Bool {
    guard let kind = sensitiveKind else { return false }
    return kind == "password" || kind == "otp" || kind == "other"
}

public struct ChatEscalationInputSchemaData: Codable, Equatable {
    public let type: String?
    // Deliberately do not decode raw `input_schema.options` here. Canonical chat
    // messages carry normalized, actionable options at `content.options`; raw
    // schemas legitimately use either `id` or `value` and may grow provider-
    // specific fields. Treating that redundant copy as EscalationOptionData made
    // one unfamiliar raw option reject the entire chat message/history response.
    public let placeholder: String?
    public let allowOther: Bool?
    public let confirmLabel: String?
    public let denyLabel: String?
    public let suggestions: [String]?
    public let multiline: Bool?
    public let minSelections: Int?
    public let maxSelections: Int?
    public let destructive: Bool?
    public let instructions: String?
    public let doneLabel: String?
    public let multiple: Bool?
    public let filter: String?
    public let toolName: String?
    public let paramsSummary: String?
    public let command: String?
    public let violation: String?
    public let allowedRoots: [String]?
    /// Present when the backend classified the ask as collecting a secret.
    public let sensitive: ChatSensitiveSpecData?

    enum CodingKeys: String, CodingKey {
        case type, placeholder, suggestions, multiline, destructive, instructions, multiple, filter
        case command, violation, sensitive
        case allowOther = "allow_other"
        case confirmLabel = "confirm_label"
        case denyLabel = "deny_label"
        case doneLabel = "done_label"
        case minSelections = "min_selections"
        case maxSelections = "max_selections"
        case toolName = "tool_name"
        case paramsSummary = "params_summary"
        case allowedRoots = "allowed_roots"
    }
}

public struct ContentBlockSource: Codable, Equatable {
    public let type: String
    public let taskId: String?

    enum CodingKeys: String, CodingKey {
        case type
        case taskId = "task_id"
    }
}

public struct ContentBlock: Codable, Equatable {
    public let type: String?
    public let text: String?
    public let filename: String?
    public let url: String?
    public let mimeType: String?
    public let source: ContentBlockSource?
    public let relativePath: String?
    public let displayName: String?
    public let absolutePath: String?
    public let label: String?
    public let size: Int?

    enum CodingKeys: String, CodingKey {
        case type, text, filename, url, source, label, size
        case mimeType = "mime_type"
        case relativePath = "relative_path"
        case displayName = "display_name"
        case absolutePath = "absolute_path"
    }
}

private struct DynamicCodingKey: CodingKey, Hashable {
    var stringValue: String
    var intValue: Int?

    init?(stringValue: String) {
        self.stringValue = stringValue
        self.intValue = nil
    }

    init?(intValue: Int) {
        self.stringValue = "\(intValue)"
        self.intValue = intValue
    }
}

private func decodeStringMap(_ container: inout KeyedDecodingContainer<DynamicCodingKey>, forKey key: DynamicCodingKey) -> [String: String] {
    guard let nested = try? container.nestedContainer(keyedBy: DynamicCodingKey.self, forKey: key) else {
        return [:]
    }
    var result: [String: String] = [:]
    for child in nested.allKeys {
        if let value = try? nested.decode(String.self, forKey: child) {
            result[child.stringValue] = value
            continue
        }
        if let value = try? nested.decode(Bool.self, forKey: child) {
            result[child.stringValue] = value ? "true" : "false"
            continue
        }
        if let value = try? nested.decode(Int.self, forKey: child) {
            result[child.stringValue] = "\(value)"
            continue
        }
        if let value = try? nested.decode(Double.self, forKey: child) {
            result[child.stringValue] = "\(value)"
            continue
        }
    }
    return result
}

public indirect enum ChatStructuredValue: Codable, Equatable {
    case string(String)
    case bool(Bool)
    case integer(Int)
    case unsignedInteger(UInt64)
    case number(Double)
    case null
    case array([ChatStructuredValue])
    case object([String: ChatStructuredValue])

    public var stringValue: String? {
        guard case let .string(value) = self else { return nil }
        return value
    }

    public var displayValue: String? {
        switch self {
        case let .string(value): return value
        case let .bool(value): return value ? "true" : "false"
        case let .integer(value): return "\(value)"
        case let .unsignedInteger(value): return "\(value)"
        case let .number(value): return "\(value)"
        case .null, .array, .object: return nil
        }
    }

    public var isNonNegativeInteger: Bool {
        switch self {
        case let .integer(value): return value >= 0
        case .unsignedInteger: return true
        default: return false
        }
    }

    public init(from decoder: Decoder) throws {
        if let container = try? decoder.singleValueContainer() {
            if container.decodeNil() {
                self = .null
                return
            }
            if let value = try? container.decode(String.self) {
                self = .string(value)
                return
            }
            if let value = try? container.decode(Bool.self) {
                self = .bool(value)
                return
            }
            if let value = try? container.decode(Int.self) {
                self = .integer(value)
                return
            }
            if let value = try? container.decode(UInt64.self) {
                self = .unsignedInteger(value)
                return
            }
            if let value = try? container.decode(Double.self) {
                self = .number(value)
                return
            }
        }
        if var container = try? decoder.unkeyedContainer() {
            var values: [ChatStructuredValue] = []
            while !container.isAtEnd {
                values.append(try container.decode(ChatStructuredValue.self))
            }
            self = .array(values)
            return
        }
        if let container = try? decoder.container(keyedBy: DynamicCodingKey.self) {
            var values: [String: ChatStructuredValue] = [:]
            for key in container.allKeys {
                values[key.stringValue] = try container.decode(ChatStructuredValue.self, forKey: key)
            }
            self = .object(values)
            return
        }
        throw DecodingError.typeMismatch(
            ChatStructuredValue.self,
            DecodingError.Context(codingPath: decoder.codingPath, debugDescription: "Unsupported structured response value")
        )
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case let .string(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .bool(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .integer(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .unsignedInteger(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .number(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case .null:
            var container = encoder.singleValueContainer()
            try container.encodeNil()
        case let .array(values):
            var container = encoder.unkeyedContainer()
            for value in values {
                try container.encode(value)
            }
        case let .object(values):
            var container = encoder.container(keyedBy: DynamicCodingKey.self)
            for (key, value) in values {
                guard let codingKey = DynamicCodingKey(stringValue: key) else { continue }
                try container.encode(value, forKey: codingKey)
            }
        }
    }
}

public struct ChatStructuredStringMap: Codable, Equatable {
    public let values: [String: ChatStructuredValue]

    public init(_ values: [String: String]) {
        self.values = values.mapValues(ChatStructuredValue.string)
    }

    public subscript(_ key: String) -> String? {
        values[key]?.displayValue
    }

    public var displayValues: [String: String] {
        values.compactMapValues(\.displayValue)
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DynamicCodingKey.self)
        var valueMap: [String: ChatStructuredValue] = [:]
        for key in container.allKeys {
            valueMap[key.stringValue] = try container.decode(ChatStructuredValue.self, forKey: key)
        }
        values = valueMap
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicCodingKey.self)
        for (key, value) in values {
            guard let codingKey = DynamicCodingKey(stringValue: key) else { continue }
            try container.encode(value, forKey: codingKey)
        }
    }
}

public struct ChatStructuredTableColumn: Codable, Equatable {
    public let key: String
    public let label: String
    public let alignment: String?

    enum CodingKeys: String, CodingKey {
        case key
        case label
        case alignment
    }
}

public struct ChatStructuredAction: Codable, Equatable {
    public let kind: String
    public let label: String
    public let text: String?
    public let url: String?
    public let taskId: String?
    public let artifactId: String?
    public let prompt: String?
    public let actionRef: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case label
        case text
        case url
        case taskId = "task_id"
        case artifactId = "artifact_id"
        case prompt
        case actionRef = "action_ref"
    }
}

public struct ChatStructuredModelContext: Codable, Equatable {
    public let summary: String?
    public let visibleFacts: [String]?
    public let selectedItem: String?
    public let privacy: String?

    enum CodingKeys: String, CodingKey {
        case summary
        case visibleFacts = "visible_facts"
        case selectedItem = "selected_item"
        case privacy
    }
}

public struct ChatStructuredProvenanceRef: Codable, Equatable {
    public let id: String
    public let label: String?
    public let ref: String?
}

public struct ChatStructuredResponseCost: Codable, Equatable {
    public let inputTokens: Int?
    public let outputTokens: Int?
    public let totalTokens: Int?
    public let costUsd: Double?
    public let model: String?

    enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case totalTokens = "total_tokens"
        case costUsd = "cost_usd"
        case model
    }
}

public struct ChatStructuredMeta: Codable, Equatable {
    public let responseId: String?
    public let sourceSurface: String?
    public let taskId: String?
    public let executionId: String?
    public let chatTurnId: String?
    public let provenance: [ChatStructuredProvenanceRef]?
    public let cost: ChatStructuredResponseCost?
    public let confidence: Double?
    public let createdAt: String?

    enum CodingKeys: String, CodingKey {
        case responseId = "response_id"
        case sourceSurface = "source_surface"
        case taskId = "task_id"
        case executionId = "execution_id"
        case chatTurnId = "chat_turn_id"
        case provenance
        case cost
        case confidence
        case createdAt = "created_at"
    }
}

public struct ChatStructuredBlock: Codable, Equatable {
    public let kind: String
    public let title: String?
    public let text: String?
    public let tone: String?
    public let style: String?
    public let items: [ChatStructuredStringMap]?
    public let columns: [ChatStructuredTableColumn]?
    public let rows: [ChatStructuredStringMap]?

    enum CodingKeys: String, CodingKey {
        case kind
        case title
        case text
        case tone
        case style
        case items
        case columns
        case rows
    }
}

public struct PlanReplyMessageContextData: Codable, Equatable {
    public let taskId: String
    public let taskTitle: String
    public let questionId: String
    public let questionText: String

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case taskTitle = "task_title"
        case questionId = "question_id"
        case questionText = "question_text"
    }
}

public struct ChatMessagePresentationData: Codable, Equatable {
    public let schema: String
    public let version: Int
    public let plainText: String
    public let title: String?
    public let summary: String?
    public let tone: String?
    public let blocks: [ChatStructuredBlock]
    public let actions: [ChatStructuredAction]?
    public let modelContext: ChatStructuredModelContext?
    public let meta: ChatStructuredMeta?

    enum CodingKeys: String, CodingKey {
        case schema, version
        case plainText = "plain_text"
        case title
        case summary
        case tone
        case blocks
        case actions
        case modelContext = "model_context"
        case meta
    }

    public init(
        schema: String = "magician.structured_response",
        version: Int = 1,
        plainText: String = "",
        title: String? = nil,
        summary: String? = nil,
        tone: String? = nil,
        blocks: [ChatStructuredBlock] = [],
        actions: [ChatStructuredAction]? = nil,
        modelContext: ChatStructuredModelContext? = nil,
        meta: ChatStructuredMeta? = nil
    ) {
        self.schema = schema
        self.version = version
        self.plainText = plainText
        self.title = title
        self.summary = summary
        self.tone = tone
        self.blocks = blocks
        self.actions = actions
        self.modelContext = modelContext
        self.meta = meta
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schema = try container.decode(String.self, forKey: .schema)
        version = try container.decode(Int.self, forKey: .version)
        plainText = try container.decodeIfPresent(String.self, forKey: .plainText) ?? ""
        title = try container.decodeIfPresent(String.self, forKey: .title)
        summary = try container.decodeIfPresent(String.self, forKey: .summary)
        tone = try container.decodeIfPresent(String.self, forKey: .tone)
        blocks = try container.decodeIfPresent([ChatStructuredBlock].self, forKey: .blocks) ?? []
        actions = try container.decodeIfPresent([ChatStructuredAction].self, forKey: .actions)
        modelContext = try container.decodeIfPresent(ChatStructuredModelContext.self, forKey: .modelContext)
        meta = try container.decodeIfPresent(ChatStructuredMeta.self, forKey: .meta)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(schema, forKey: .schema)
        try container.encode(version, forKey: .version)
        try container.encode(plainText, forKey: .plainText)
        try container.encodeIfPresent(title, forKey: .title)
        try container.encodeIfPresent(summary, forKey: .summary)
        try container.encodeIfPresent(tone, forKey: .tone)
        try container.encode(blocks, forKey: .blocks)
        try container.encodeIfPresent(actions, forKey: .actions)
        try container.encodeIfPresent(modelContext, forKey: .modelContext)
        try container.encodeIfPresent(meta, forKey: .meta)
    }
}

public struct ChatMessageContentData: Codable, Equatable {
    public let type: String
    public let text: String?
    public let planReply: PlanReplyMessageContextData?
    public let executionId: String?
    public let requestId: String?
    public let escalationType: String?
    public let inputType: String?
    public let inputSchema: ChatEscalationInputSchemaData?
    public var question: String?
    /// Optional supporting or validation text for the current question.
    public var hint: String?
    /// The answer rejected by the backend during a validation re-ask. It is
    /// retained locally so the form can be corrected instead of retyped.
    public var previousAnswer: String?
    public let options: [EscalationOptionData]?
    /// var so an incoming `escalation_resolved` can flip the matching open card.
    public var resolved: Bool?
    public let filename: String?
    public let mimeType: String?
    public let absolutePath: String?
    public let label: String?
    public let size: Int?
    /// Correlation id used to answer an escalation via POST /hitl/{id}/respond.
    public var pauseStateId: String?
    public var correlationId: String?
    /// tool_call_executed / rich_tool_result.
    public let toolName: String?
    public let toolCallId: String?
    public let summary: String?
    public let contentBlocks: [ContentBlock]?
    /// task_status_update fields.
    public let taskId: String?
    public let status: String?
    public let displayLabel: String?
    public let uiThreadId: String?
    public let outputFiles: [ContentBlock]?
    public let synthesisPending: Bool?
    public let speechTTS: String?
    public let stale: Bool?
    public let inactiveReason: String?

    /// Reconstruct the canonical response target persisted by the backend's chat
    /// escalation projection. Planning clarifications deliberately store two
    /// different identities: `pause_state_id` is the question/correlation id,
    /// while `request_id` carries `clarification_responder:<task-id>` so the
    /// responder can route the answer back to the owning plan.
    public var hitlTarget: ChatHitlTarget? {
        ChatHitlTarget(content: self)
    }

    /// The canonical id used by response and resolution matching. Never prefer
    /// the synthetic clarification responder id over the question id.
    public var hitlCorrelationId: String? { hitlTarget?.correlationId }

    /// The input contract used by the chat card. Older persisted cards may not
    /// contain `input_type`, so the same conservative compatibility mapping as
    /// the web chat is used.
    public var hitlInputType: String {
        ChatHitlTarget.compatibilityInputType(for: self)
    }

    enum CodingKeys: String, CodingKey {
        case type, text, question, hint, options, resolved, filename, size, summary, label, stale
        case previousAnswer = "previous_answer"
        case planReply = "plan_reply"
        case executionId = "execution_id"
        case requestId = "request_id"
        case escalationType = "escalation_type"
        case inputType = "input_type"
        case inputSchema = "input_schema"
        case pauseStateId = "pause_state_id"
        case correlationId = "correlation_id"
        case toolName = "tool_name"
        case toolCallId = "tool_call_id"
        case contentBlocks = "content_blocks"
        case mimeType = "mime_type"
        case absolutePath = "absolute_path"
        case taskId = "task_id"
        case status
        case displayLabel = "display_label"
        case uiThreadId = "ui_thread_id"
        case outputFiles = "output_files"
        case synthesisPending = "synthesis_pending"
        case speechTTS = "speech_tts"
        case inactiveReason = "inactive_reason"
    }
}

/// Canonical `/hitl/{correlation_id}/respond` identity reconstructed from a
/// persisted chat escalation card.
public struct ChatHitlTarget: Equatable {
    public static let clarificationResponderPrefix = "clarification_responder:"

    public let correlationId: String
    public let source: String
    public let inputType: String
    public let taskId: String?
    public let executionId: String?

    fileprivate init?(content: ChatMessageContentData) {
        let correlation = Self.nonEmpty(content.correlationId)
        let pause = Self.nonEmpty(content.pauseStateId)
        let request = Self.nonEmpty(content.requestId)
        let execution = Self.nonEmpty(content.executionId)
        let isClarification = content.escalationType == "clarification"
        let responder = isClarification
            ? Self.clarificationResponderId(from: request, fallback: execution)
            : nil

        let resolvedSource: String
        let resolvedCorrelation: String?
        if isClarification {
            resolvedSource = "clarification"
            resolvedCorrelation = correlation ?? pause
        } else if request != nil {
            resolvedSource = "user_request"
            resolvedCorrelation = correlation ?? request
        } else {
            resolvedSource = "escalation"
            resolvedCorrelation = correlation ?? pause
        }

        guard let resolvedCorrelation else { return nil }
        if resolvedSource == "clarification", responder == nil { return nil }
        if resolvedSource == "escalation", execution == nil { return nil }

        correlationId = resolvedCorrelation
        source = resolvedSource
        inputType = Self.compatibilityInputType(for: content)
        taskId = responder
        executionId = execution
    }

    fileprivate static func compatibilityInputType(for content: ChatMessageContentData) -> String {
        if let explicit = nonEmpty(content.inputType) { return explicit }
        switch content.escalationType {
        case "clarification": return "text"
        case "cannot_proceed", "loop_detected": return "external_action"
        case "tool_authorization": return "tool_authorization"
        case "sandbox_override": return "sandbox_override"
        default:
            return content.options?.contains(where: { $0.requiresInput == true }) == true
                ? "guidance"
                : "confirmation"
        }
    }

    private static func clarificationResponderId(from requestId: String?, fallback: String?) -> String? {
        guard let requestId else { return fallback }
        if requestId.hasPrefix(clarificationResponderPrefix) {
            let value = String(requestId.dropFirst(clarificationResponderPrefix.count))
            return nonEmpty(value) ?? fallback
        }
        // Compatibility with cards persisted before the prefix was introduced.
        return requestId
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty else { return nil }
        return trimmed
    }
}

/// Canonical replacement state returned when a response fails validation. The
/// pause key may rotate, so callers must replace both the visible prompt and the
/// response identity atomically before allowing another submission.
public struct ChatHitlReaskData: Equatable {
    public let pauseStateId: String
    public let question: String
    public let hint: String?
    public let previousAnswer: String?

    public init(
        pauseStateId: String,
        question: String,
        hint: String?,
        previousAnswer: String?
    ) {
        self.pauseStateId = pauseStateId
        self.question = question
        self.hint = hint
        self.previousAnswer = previousAnswer
    }
}

public extension ChatMessageContentData {
    mutating func applyCanonicalReask(_ reask: ChatHitlReaskData) {
        pauseStateId = reask.pauseStateId
        correlationId = reask.pauseStateId
        question = reask.question
        hint = reask.hint
        previousAnswer = reask.previousAnswer
        resolved = false
    }
}

/// Typed values accepted by the canonical HITL response endpoint.
public enum ChatHitlResponseValue: Equatable {
    case text(String)
    case password(String)
    case choice(selectedId: String, otherValue: String?)
    case multiChoice([String])
    case confirmation(Bool)
    case externalActionCompleted(guidance: String?)
    case filePath([String])
    case guidance(String)

    var wireValue: [String: Any] {
        switch self {
        case .text(let value):
            return ["type": "text", "value": value]
        case .password(let value):
            return ["type": "password", "value": value]
        case .choice(let selectedId, let otherValue):
            var value: [String: Any] = ["type": "choice", "selected_id": selectedId]
            if let otherValue, !otherValue.isEmpty { value["other_value"] = otherValue }
            return value
        case .multiChoice(let selectedIds):
            return ["type": "multi_choice", "selected_ids": selectedIds]
        case .confirmation(let confirmed):
            return ["type": "confirmation", "confirmed": confirmed]
        case .externalActionCompleted(let guidance):
            var value: [String: Any] = ["type": "external_action_completed"]
            if let guidance, !guidance.isEmpty { value["guidance"] = guidance }
            return value
        case .filePath(let paths):
            return ["type": "file_path", "paths": paths]
        case .guidance(let advice):
            return ["type": "guidance", "advice": advice]
        }
    }
}

/// A card submission is either a canonical HITL response or the distinct
/// max-iteration continuation operation that grants a fresh execution budget.
public enum ChatHitlSubmission: Equatable {
    case response(ChatHitlResponseValue)
    case continueExecution
}

/// Pure response composition shared by the SwiftUI card and contract tests.
/// Returning nil keeps incomplete text/choice forms from reaching the network.
public enum ChatHitlResponseComposer {
    /// Old persisted option-driven cards predate server-authored actions. They
    /// remain safely actionable by opening their canonical Attention record;
    /// clients must not reconstruct authority from localized labels or IDs.
    public static func needsCanonicalAttentionFallback(
        inputType: String,
        options: [EscalationOptionData],
        allowsOther: Bool = false,
        inputTypeIsAuthoritative: Bool = true
    ) -> Bool {
        guard inputTypeIsAuthoritative else { return true }
        switch inputType {
        case "text", "password", "otp", "guidance", "file_path", "multi_choice":
            return false
        case "form":
            return true
        default:
            if options.isEmpty { return !allowsOther }
            return options.contains(where: { $0.action == nil })
        }
    }

    public static func compose(
        inputType: String,
        option: EscalationOptionData? = nil,
        text: String = "",
        selectedIds: [String] = [],
        allowsMultipleFiles: Bool = false,
        sensitive: Bool = false
    ) -> ChatHitlSubmission? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        switch inputType {
        case "text":
            // A secret is the exact string typed — a code's leading zero is
            // part of the code, and a password's trailing space is part of
            // the password. The type posted is still the pause's.
            if sensitive { return text.isEmpty ? nil : .response(.text(text)) }
            return trimmed.isEmpty ? nil : .response(.text(trimmed))
        case "password", "otp":
            // A one-time code rides the password value shape: masked
            // everywhere, exact, never coerced through a number.
            return text.isEmpty ? nil : .response(.password(text))
        case "guidance":
            if sensitive { return text.isEmpty ? nil : .response(.guidance(text)) }
            return trimmed.isEmpty ? nil : .response(.guidance(trimmed))
        case "file_path":
            let paths = allowsMultipleFiles
                ? text
                    .components(separatedBy: CharacterSet(charactersIn: ",\n"))
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                : (trimmed.isEmpty ? [] : [trimmed])
            return paths.isEmpty ? nil : .response(.filePath(paths))
        case "multi_choice":
            return selectedIds.isEmpty ? nil : .response(.multiChoice(selectedIds))
        case "confirmation":
            guard let action = option?.action else { return nil }
            if action.type == "continue_execution" { return .continueExecution }
            guard action.type == "respond_confirmation", let confirmed = action.confirmed else {
                return nil
            }
            return .response(.confirmation(confirmed))
        case "external_action":
            guard let option, option.action?.type == "respond_external_action" else { return nil }
            if option.requiresInput == true, trimmed.isEmpty { return nil }
            return .response(.externalActionCompleted(guidance: trimmed.isEmpty ? nil : trimmed))
        default:
            guard let option, option.action?.type == "respond_choice" else { return nil }
            if option.requiresInput == true, trimmed.isEmpty { return nil }
            return .response(
                .choice(
                    selectedId: option.id,
                    otherValue: trimmed.isEmpty ? nil : trimmed
                )
            )
        }
    }
}

public struct ChatMessageOrigin: Codable, Equatable {
    public let uiThreadId: String
    public let sessionId: String
    public let requestId: String
    public let messageId: String?
    enum CodingKeys: String, CodingKey {
        case uiThreadId = "ui_thread_id", sessionId = "session_id"
        case requestId = "request_id", messageId = "message_id"
    }
}

public struct OriginalAnswerLink: Equatable {
    public let origin: ChatMessageOrigin
    public let turnId: String?
    public let createdAt: Int64?
    public func matches(_ message: ChatMessageRawData) -> Bool {
        if let id = origin.messageId { return message.id == id }
        return turnId != nil && createdAt != nil && message.chatTurnId == turnId &&
            message.createdAt == createdAt && message.direction != "user"
    }
}

public struct ChatMessageRawData: Codable, Equatable {
    public let contextOrigin: ChatMessageOrigin?
    public let createdAt: Int64?
    public let id: String
    public let sessionId: String
    public let direction: String
    public let content: ChatMessageContentData
    public let presentation: ChatMessagePresentationData?
    /// Stable request correlation shared by the user message, assistant reply,
    /// and the durable per-turn activity event projection.
    public let chatTurnId: String?
    /// Message-level flag stamped by the backend when the turn came from voice.
    public let voiceOrigin: Bool?
    /// Realtime voice transcripts carry the registered call session. Unlike a
    /// normal typed/dictated send, they have no optimistic iOS chat echo.
    public let presenceSessionId: String?
    public let sourceSurface: String?

    enum CodingKeys: String, CodingKey {
        case contextOrigin = "context_origin", createdAt = "created_at"
        case id, direction, content
        case presentation
        case sessionId = "session_id"
        case chatTurnId = "chat_turn_id"
        case voiceOrigin = "voice_origin"
        case presenceSessionId = "presence_session_id"
        case sourceSurface = "source_surface"
    }
}

public struct ChatMessageReceivedEventData: Codable, Equatable {
    public let sessionId: String
    public let message: ChatMessageRawData
    
    enum CodingKeys: String, CodingKey {
        case message
        case sessionId = "session_id"
    }
}
