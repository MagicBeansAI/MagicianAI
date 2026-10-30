import Foundation
import Combine
import UIKit

enum MessageType: Equatable {
    case text
    case taskStatus(TaskStatusModel)
    case escalation(ChatMessageContentData)
    case system(String)
    case attachment(filename: String, size: String?)
}

struct TaskStatusModel: Equatable {
    let taskId: String
    var title: String
    var status: String
    var steps: [String]
    var terminalLines: [String] = []
    var summary: String? = nil
    var executionId: String? = nil
    var uiThreadId: String? = nil
    var outputFiles: [ContentBlock] = []
    var synthesisPending = false
    /// Backend-authoritative active root. Execution Panel's `execution_id` is
    /// inspection-oriented and may point at the latest completed execution.
    var activeRootExecutionId: String? = nil
    /// `full` / `partial` on a completed status update (a partial success).
    var completionKind: String? = nil
    /// The task's status in the task record (`GET /v3/tasks/{id}`), when
    /// fetched. The live card shows it over a message stuck at planning/draft.
    var storeStatus: String? = nil
    /// Epoch ms of the first status update this card has seen (kept across
    /// coalesced updates) and of the latest one: the card's elapsed time.
    var startedAtMs: Double? = nil
    var updatedAtMs: Double? = nil

    var normalizedStatus: String { status.lowercased() }

    var isTerminal: Bool {
        ["complete", "completed", "done", "failed", "error", "cancelled", "canceled", "archived"]
            .contains(normalizedStatus)
    }

    var statusVerb: String {
        if synthesisPending { return "Preparing final result" }
        switch normalizedStatus {
        case "created": return "Created"
        case "planning", "ready": return "Planning"
        case "queued": return "Queued"
        case "running", "in_progress", "executing": return "Running"
        case "paused": return "Paused"
        case "complete", "completed", "done": return "Completed"
        case "failed", "error": return "Failed"
        case "cancelled", "canceled": return "Cancelled"
        case "archived": return "Archived"
        default: return status.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    var showsProgress: Bool {
        synthesisPending || ["planning", "ready", "queued", "running", "in_progress", "executing"]
            .contains(normalizedStatus)
    }

    var activeExecutionIdForControls: String? {
        Self.resolveActiveExecutionId(status: status, activeRootExecutionId: activeRootExecutionId)
    }

    static func resolveActiveExecutionId(status: String, activeRootExecutionId: String?) -> String? {
        guard ["queued", "running", "planning", "paused", "executing"]
            .contains(status.lowercased()),
              let executionId = activeRootExecutionId?
                .trimmingCharacters(in: .whitespacesAndNewlines),
              !executionId.isEmpty else { return nil }
        return executionId
    }

    var executionControlRefreshToken: String {
        "\(status.lowercased())|\(activeRootExecutionId ?? "")"
    }
}

struct ChatMessage: Identifiable, Equatable {
    let id: String // Use actual message ID instead of UUID
    let isUser: Bool
    var text: String
    var type: MessageType
    /// Optional backend-authored structured response payload. When valid, the UI
    /// renders this in the structured card format introduced in the web parity
    /// rollout instead of the legacy markdown-only bubble path.
    var structuredResponse: ChatMessagePresentationData? = nil
    /// Original paragraph boundaries for the display-only verification note.
    /// The structured plain-text projection may flatten the caveat and answer.
    var verificationSourceText: String? = nil
    /// Access-gated image artifact URLs to render inline under the bubble
    /// (content-block images). Loaded with auth headers via AuthAsyncImage.
    var imageURLs: [String] = []
    /// Coalesced activity rows for this assistant turn — rendered as a
    /// collapsible "What happened" section under the bubble (web parity).
    var activityRows: [ActivityRow] = []
    /// True while this exact turn is still producing inline or delegated
    /// activity. Kept separate from `activityRows` so iOS can render the
    /// loading disclosure before the first durable event arrives.
    var activityIsLive = false
    /// Stable request correlation for canonical echo matching. Only assistant
    /// messages can own a per-turn activity disclosure.
    var chatTurnId: String? = nil
    /// When this user message answered a plan question, the plan/task title it
    /// replied to — renders a "↩ Replying to Planner" caption above the bubble.
    var planReplyContext: String? = nil
    /// Non-image file/url content blocks — rendered as per-type "open" cards.
    var richBlocks: [ContentBlock] = []
    /// This turn originated from voice (dictation) — renders a small speaker badge
    /// on the bubble (web parity: `voice_origin`).
    var voiceOrigin: Bool = false
    var originalAnswer: OriginalAnswerLink? = nil
    var linkedAnswerTarget = false
    /// When the message was written (the server's `created_at`; now for a
    /// local echo). Drives the transcript's date dividers.
    var createdAt: Date? = nil
    /// The status line of an assistant turn that settled without a reply of
    /// its own ("Stopped", "Interrupted before it finished", …), from the
    /// server's turn state (`ChatTurnState.notice`).
    var turnStatusNotice: String? = nil

    static func == (lhs: ChatMessage, rhs: ChatMessage) -> Bool {
        lhs.originalAnswer == rhs.originalAnswer && lhs.linkedAnswerTarget == rhs.linkedAnswerTarget && lhs.id == rhs.id && lhs.text == rhs.text && lhs.imageURLs == rhs.imageURLs
            && lhs.activityRows == rhs.activityRows && lhs.activityIsLive == rhs.activityIsLive
            && lhs.turnStatusNotice == rhs.turnStatusNotice
            && lhs.chatTurnId == rhs.chatTurnId
            && lhs.structuredResponse == rhs.structuredResponse
            && lhs.verificationSourceText == rhs.verificationSourceText
            && lhs.richBlocks == rhs.richBlocks
            // A task card's status lives in `type`; without it a status change
            // compares equal and SwiftUI can skip re-rendering the card.
            && lhs.type == rhs.type
            && lhs.createdAt == rhs.createdAt
    }
}

struct ChatModelSelection: Codable, Equatable {
    let modelId: String
    let thinking: String
    enum CodingKeys: String, CodingKey {
        case modelId = "model_id"
        case thinking
    }
    var wireValue: [String: String] { ["model_id": modelId, "thinking": thinking] }
}

struct ChatThinkingControl: Codable, Equatable {
    let mode: String
    let levels: [String]
    let defaultLevel: String?
    enum CodingKeys: String, CodingKey {
        case mode, levels
        case defaultLevel = "default"
    }
    /// Web `thinkingLabel` (`modelCatalog.ts`): known levels get a display name, anything else stays raw.
    static func label(_ level: String) -> String {
        ["auto": "Auto", "none": "Off", "on": "On", "minimal": "Minimal", "low": "Low", "medium": "Medium", "high": "High", "xhigh": "Extra high", "max": "Max", "ultra": "Ultra"][level] ?? level
    }

    /// The thinking tag on a composer model chip (web `ComposerModelChip` `thinking`):
    /// only for a catalog model whose thinking mode is not `none`, level defaulting
    /// to `auto`. VibeDev's coding chip uses this directly.
    static func chipLabel(model: ChatModelChoice?, selection: ChatModelSelection?) -> String? {
        guard let model, model.thinking.mode != "none" else { return nil }
        return label(selection?.thinking ?? "auto")
    }

    /// Chat composer rule (web `ChatPanel` `profileThinkingLabel`): only the catalog engines `magician` and `pi`.
    static func chatChipLabel(engine: String, model: ChatModelChoice?, selection: ChatModelSelection?) -> String? {
        engine == "magician" || engine == "pi" ? chipLabel(model: model, selection: selection) : nil
    }

    /// The chip's spoken label: `<label>, thinking: High` when a thinking tag is shown (web aria-label).
    static func chipAccessibilityLabel(_ label: String, thinking: String?) -> String {
        thinking.map { "\(label), thinking: \($0)" } ?? label
    }
}

struct ChatModelChoice: Codable, Identifiable, Equatable {
    struct Capabilities: Codable, Equatable { let vision: Bool? }
    struct Limits: Codable, Equatable {
        let contextWindowTokens: Int
        enum CodingKeys: String, CodingKey { case contextWindowTokens = "context_window_tokens" }
    }
    let id: String
    let provider: String
    let connection: String
    let model: String
    let label: String
    let thinking: ChatThinkingControl
    let choices: [String: String]
    var operationChoices: [String: String]? = nil
    var installed: Bool? = nil
    var capabilities: Capabilities? = nil
    enum CodingKeys: String, CodingKey {
        case id, provider, connection, model, label, thinking, choices, limits, installed, capabilities
        case operationChoices = "operation_choices"
    }
    let limits: Limits?
    var providerDisplayName: String {
        ["openai": "OpenAI", "anthropic": "Anthropic", "gemini": "Gemini", "deepseek": "DeepSeek", "minimax": "MiniMax", "xai": "xAI", "sarvam": "Sarvam", "ollama": "Ollama", "yutori": "Yutori", "openrouter": "OpenRouter", "harness-claude_code": "Claude Code", "harness-codex": "Codex", "harness-grok": "Grok", "harness-agy": "Antigravity", "harness-pi": "Pi"][provider] ?? provider
    }
}

struct ChatProfile: Codable, Identifiable, Equatable {
    let name: String
    let provider: String?
    let model: String?
    let isDefault: Bool?
    let supportsUserImageInputs: Bool?
    var modelSelection: ChatModelSelection? = nil
    var modelChoice: ChatModelChoice? = nil
    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, provider, model
        case isDefault = "is_default"
        case supportsUserImageInputs = "supports_user_image_inputs"
        case modelSelection = "model_selection"
        case modelChoice = "model_choice"
    }
}

struct ChatModelCatalog: Codable {
    let models: [ChatModelChoice]
    let defaultSelection: ChatModelSelection?
    enum CodingKeys: String, CodingKey {
        case models
        case defaultSelection = "default_selection"
    }
    var rows: [ChatProfile] {
        models.flatMap { model in model.choices.sorted(by: { $0.key < $1.key }).map { thinking, route in
            let selection = ChatModelSelection(modelId: model.id, thinking: thinking)
            return ChatProfile(name: route, provider: model.provider, model: model.model,
                isDefault: selection == defaultSelection, supportsUserImageInputs: model.capabilities?.vision == true,
                modelSelection: selection, modelChoice: thinking == "auto" ? model : nil)
        } }
    }
}

/// Typed preferences survive removal of the old selector rows. Unknown choices
/// stay unresolved until the owner explicitly chooses a replacement.
enum ChatModelPreference {
    static let storageKey = "magios.chat.modelSelection"
    static let unavailableRoute = "__unavailable_model_selection__"
    static let unavailableMessage = "Your saved model choice is unavailable. Choose a model to continue."

    static func resolve(profiles: [ChatProfile], legacyID: String, saved: ChatModelSelection?)
        -> (route: String, selection: ChatModelSelection?, issue: String?) {
        let previous = profiles.first { $0.name == legacyID }
        if let selection = saved ?? previous?.modelSelection ?? LegacyLlmSelections.chatSelection(for: legacyID) {
            let route = profiles.compactMap(\.modelChoice)
                .first { $0.id == selection.modelId }?.choices[selection.thinking]
            if let route, profiles.contains(where: { $0.name == route }) {
                return (route, selection, nil)
            }
            return (unavailableRoute, selection, unavailableMessage)
        }
        if !legacyID.isEmpty {
            return (legacyID, nil, previous == nil ? unavailableMessage : nil)
        }
        guard let initial = profiles.first(where: { $0.isDefault == true }) else {
            return ("", nil, "Choose a model to continue.")
        }
        return resolve(profiles: profiles, legacyID: initial.name, saved: initial.modelSelection)
    }

    static func requestFields(selection: ChatModelSelection?, engine: String, nativeModel: String) -> [String: Any]? {
        if ["magician", "pi"].contains(engine) {
            guard let selection, !selection.modelId.isEmpty, !selection.thinking.isEmpty else { return nil }
            return ["harness_engine": engine, "model_selection": selection.wireValue]
        }
        return ["harness_engine": engine, "harness_model": nativeModel]
    }
}

struct ChatHarnessOption: Codable, Identifiable, Equatable {
    let name: String
    let installed: Bool
    let models: [String]?
    /// Whose account the CLI runs on: `login`, `magician_key` or `none`
    /// (absent for `magician`); see `HarnessCredentialsLabel`.
    var credentials: String? = nil
    var id: String { name }
    var availableModels: [String] { models ?? ["default"] }
}

extension ChatHarnessOption {
    private enum CodingKeys: String, CodingKey { case name, installed, models, credentials }

    /// `credentials` is read leniently: an unexpected value means no label.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(name: try c.decode(String.self, forKey: .name),
                  installed: try c.decode(Bool.self, forKey: .installed),
                  models: try c.decodeIfPresent([String].self, forKey: .models),
                  credentials: try? c.decodeIfPresent(String.self, forKey: .credentials))
    }
}

private struct ChatHarnessRoster: Codable {
    let engines: [ChatHarnessOption]
    /// The run engine (`execution.harness_engine`) and its model.
    var current: String? = nil
    var runModel: String? = nil
    /// Pi's run model (`execution.pi_profile` as a catalog selection).
    var runModelSelection: ChatModelSelection? = nil
    /// Agentic flows follow the chat engine; `current` is what that resolves to.
    var followsChat: Bool? = nil
    /// The server's chat engine and model (clients with no pick of their own).
    var chatCurrent: String? = nil
    var chatModel: String? = nil

    enum CodingKeys: String, CodingKey {
        case engines, current
        case runModel = "run_model"
        case runModelSelection = "run_model_selection"
        case followsChat = "follows_chat"
        case chatCurrent = "chat_current"
        case chatModel = "chat_model"
    }
}

extension ComposerModelOption {
    /// A catalog model as the `/` picker shows it; `operation` also reads a
    /// routing operation's `operation_choices`.
    init(_ model: ChatModelChoice, operation: Bool = false) {
        var levels = Set(model.choices.keys)
        if operation { levels.formUnion((model.operationChoices ?? [:]).keys) }
        self.init(id: model.id, label: model.label, detail: model.providerDisplayName,
                  thinkingLevels: levels.sorted(), defaultThinking: model.thinking.defaultLevel,
                  available: model.installed != false,
                  thinkingMode: model.thinking.mode, controlLevels: model.thinking.levels)
    }
}

extension ComposerModelSelection {
    init(_ selection: ChatModelSelection) { self.init(modelId: selection.modelId, thinking: selection.thinking) }
}

extension ChatModelSelection {
    init(_ selection: ComposerModelSelection) { self.init(modelId: selection.modelId, thinking: selection.thinking) }
}

/// `GET /api/magician/v2/llm/routing`, the part `/run-engine magician` reads:
/// the catalog and the model the `agentic_decision` operation resolves to
/// (web `EnginesPanel` loop model).
struct LoopModelRouting {
    static let operation = "agentic_decision"
    let models: [ChatModelChoice]
    let selection: ChatModelSelection?

    /// The routing, or why it cannot be read.
    static func decode(_ data: Data) -> Result<LoopModelRouting, ChatViewModel.CommandFailure> {
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            return .failure(.init(text: "Model routing could not be read."))
        }
        guard let loop = (object["operations"] as? [[String: Any]])?
            .first(where: { $0["operation"] as? String == operation }) else {
            return .failure(.init(text: "The agentic decision mapping is not in the model catalog."))
        }
        // One malformed model must not hide the rest.
        let models = (object["models"] as? [Any] ?? []).compactMap { item -> ChatModelChoice? in
            guard let raw = try? JSONSerialization.data(withJSONObject: item) else { return nil }
            return try? JSONDecoder().decode(ChatModelChoice.self, from: raw)
        }
        let profiles = object["profiles"] as? [[String: Any]] ?? []
        return .success(LoopModelRouting(
            models: models,
            selection: selectionForRoute(loop["effective_profile"] as? String, models: models, profiles: profiles)))
    }

    /// Web `selectionForRoute`: the profile's own selection when its model is
    /// in the catalog, else the model and level whose choice is the route.
    static func selectionForRoute(_ route: String?, models: [ChatModelChoice], profiles: [[String: Any]]) -> ChatModelSelection? {
        guard let route else { return nil }
        if let saved = profiles.first(where: { $0["name"] as? String == route })?["model_selection"] as? [String: Any],
           let modelId = saved["model_id"] as? String, let thinking = saved["thinking"] as? String,
           models.contains(where: { $0.id == modelId }) {
            return ChatModelSelection(modelId: modelId, thinking: thinking)
        }
        for model in models {
            let choices = Array(model.choices) + Array(model.operationChoices ?? [:])
            if let thinking = choices.first(where: { $0.value == route })?.key {
                return ChatModelSelection(modelId: model.id, thinking: thinking)
            }
        }
        return nil
    }
}

/// The short confirmation (or failure) a slash command posts over the composer.
struct ComposerCommandNotice: Equatable {
    let id = UUID()
    let text: String
    let isError: Bool
}

struct QueuedMessage: Codable, Identifiable, Equatable {
    let id: String
    let text: String?
    let queuedAt: Double?
    var attachmentIds: [String]? = nil

    init(id: String, text: String?, queuedAt: Double? = nil) {
        self.id = id
        self.text = text
        self.queuedAt = queuedAt
    }

    enum CodingKeys: String, CodingKey {
        case id, text
        case queuedAt = "queued_at"
        case attachmentIds = "attachment_ids"
    }
}

/// A file staged for the next send. Added optimistically the moment the user
/// picks it (so a marker shows immediately), then flips `uploading→false` with a
/// `remoteId` when the upload lands, or `failed` on error.
struct StagedAttachment: Identifiable, Equatable {
    let id: UUID
    var remoteId: String?
    var filename: String
    var mime: String
    /// Local bytes for an inline thumbnail (images only).
    var thumbnail: Data?
    var uploading: Bool
    var failed: Bool = false
    var isImage: Bool { mime.hasPrefix("image/") }
}

private struct QueueResponse: Codable {
    let queued: [QueuedMessage]
    let active: Bool?
}

/// Server-side state of a chat turn (`chat::turn_state` in magician). The
/// session detail, `/chat/active` and `/queue` responses carry the session's
/// latest turn as `turn_state`; `chat.turn.state_changed` agent events carry
/// every change. A reopened chat reads it to show a turn that is still
/// running, stopped or failed, instead of guessing from the transcript.
struct ChatTurnState: Equatable {
    enum Status: String { case running, completed, stopped, failed }

    static let eventType = "chat.turn.state_changed"
    static let userStopReason = "user_stop"
    static let interruptedReason = "interrupted"

    static let stoppedNotice = "Stopped"
    static let interruptedNotice = "Interrupted before it finished"
    static let failedNotice = "This turn failed before it could answer"

    let chatTurnId: String
    let sessionId: String
    let status: Status
    /// Unix ms.
    let startedAt: Double
    var endedAt: Double? = nil
    /// `user_stop`, a watchdog reason (`turn_stalled`,
    /// `turn_wall_clock_exceeded`), `error` or `interrupted`.
    var reason: String? = nil

    var isRunning: Bool { status == .running }
    var isRealtimeVoice: Bool { Self.isRealtimeVoiceTurn(chatTurnId) }

    /// A watchdog stop persists an assistant notice under the turn's id;
    /// that notice is the turn's reply, so it gets no status line.
    var isWatchdogStop: Bool {
        status == .stopped && (reason ?? Self.userStopReason) != Self.userStopReason
    }

    /// The status line for a settled turn with no reply of its own. A
    /// completed turn has its reply and a watchdog stop has its notice, so
    /// neither gets a line.
    var notice: String? {
        switch status {
        case .stopped:
            return isWatchdogStop ? nil : Self.stoppedNotice
        case .failed:
            return reason == Self.interruptedReason ? Self.interruptedNotice : Self.failedNotice
        case .running, .completed:
            return nil
        }
    }

    /// Realtime voice turns render through the call surface, never as a
    /// chat bubble (web `turnState.ts` skips the same ids).
    static func isRealtimeVoiceTurn(_ turnId: String) -> Bool {
        turnId.hasPrefix("voice-turn-") || turnId.hasPrefix("local-voice-turn-")
            || turnId.hasPrefix("voice-request-") || turnId.hasPrefix("voice-task-result-")
    }

    /// Parse a wire `turn_state` (or a state event payload); `sessionId`
    /// fills a payload that omits `chat_session_id`.
    static func parse(_ raw: Any?, sessionId: String?) -> ChatTurnState? {
        guard let value = raw as? [String: Any],
              let turnId = (value["chat_turn_id"] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines), !turnId.isEmpty,
              let status = (value["status"] as? String).flatMap(Status.init(rawValue:))
        else { return nil }
        let explicitSession = (value["chat_session_id"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard let session = explicitSession.flatMap({ $0.isEmpty ? nil : $0 }) ?? sessionId,
              !session.isEmpty else { return nil }
        let reason = (value["reason"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        return ChatTurnState(
            chatTurnId: turnId,
            sessionId: session,
            status: status,
            startedAt: (value["started_at"] as? NSNumber)?.doubleValue ?? 0,
            endedAt: (value["ended_at"] as? NSNumber)?.doubleValue,
            reason: reason
        )
    }

    /// The state a `chat.turn.state_changed` frame carries, else nil. The
    /// hub delivers it as an `AgentEvent` whose inner payload is the state.
    static func fromEvent(_ frame: [String: Any]) -> ChatTurnState? {
        let outer = frame["event_type"] as? String
        let data = frame["data"] as? [String: Any]
        let inner = data?["event"] as? [String: Any]
        let payload: [String: Any]?
        if outer == "AgentEvent", inner?["event_type"] as? String == eventType {
            payload = (inner?["payload"] as? [String: Any]) ?? (data?["payload"] as? [String: Any])
        } else if outer == eventType {
            payload = (data?["payload"] as? [String: Any]) ?? data
        } else {
            return nil
        }
        return parse(payload, sessionId: nil)
    }

    /// The recording rule: a settled state is never replaced by a stale
    /// `running` snapshot of the same turn (a poll that raced the end event).
    static func shouldRecord(_ incoming: ChatTurnState, over previous: ChatTurnState?) -> Bool {
        guard let previous, previous.chatTurnId == incoming.chatTurnId else { return true }
        return previous.isRunning || !incoming.isRunning
    }
}

/// What a session or queue response says about the session's latest turn.
enum ChatTurnStateSnapshot: Equatable {
    /// The response has no `turn_state` field (a server that predates it).
    case unsupported
    /// `turn_state: null`: no turn yet, or the server restarted.
    case noTurn
    case latest(ChatTurnState)

    static func parse(object: [String: Any]?, sessionId: String) -> ChatTurnStateSnapshot {
        guard let object, object.keys.contains("turn_state") else { return .unsupported }
        return ChatTurnState.parse(object["turn_state"], sessionId: sessionId)
            .map { ChatTurnStateSnapshot.latest($0) } ?? .noTurn
    }

    static func parse(responseData: Data?, sessionId: String) -> ChatTurnStateSnapshot {
        let object = responseData.flatMap {
            try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
        }
        return parse(object: object, sessionId: sessionId)
    }

    var state: ChatTurnState? {
        if case .latest(let state) = self { return state }
        return nil
    }
}

/// How one SSE reply stream ended (`postMessageStreaming`).
struct ChatStreamEnd: Equatable {
    /// Answer text arrived (tokens or the `done` frame's persisted reply).
    var gotContent = false
    /// The final `done` frame arrived.
    var sawDone = false
    /// The `done` frame said the turn was cancelled (Stop or the watchdog).
    var cancelled = false
    /// An `event: error` frame's message.
    var errorMessage: String? = nil
    /// The connection failed (radio loss, suspension) before the stream ended.
    var transportFailed = false
    /// The server answered the POST with a non-2xx status.
    var rejected = false
}

/// Remembers the conversation selected on this device without making that
/// choice global to every client. A server origin is part of the key: two
/// Magician installations can legitimately use the same principal/workspace
/// and must never inherit one another's session id.
struct ChatSessionSelectionStore {
    static let shared = ChatSessionSelectionStore(defaults: .standard)
    private static let storageKey = "magios.chat.last-open-session.v1"

    let defaults: UserDefaults

    private func scopeKey() -> String {
        [
            MagicianAccess.baseURL.absoluteString,
            MagicianAccess.principal,
            MagicianAccess.workspace,
        ].map { "\($0.utf8.count):\($0)" }.joined(separator: "|")
    }

    func rememberedSessionId() -> String? {
        let value = (defaults.dictionary(forKey: Self.storageKey) as? [String: String])?[scopeKey()]?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return value.flatMap { $0.isEmpty ? nil : $0 }
    }

    func remember(_ sessionId: String) {
        let clean = sessionId.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty else { return }
        var selections = defaults.dictionary(forKey: Self.storageKey) as? [String: String] ?? [:]
        selections[scopeKey()] = clean
        defaults.set(selections, forKey: Self.storageKey)
    }

    func forget(_ sessionId: String? = nil) {
        var selections = defaults.dictionary(forKey: Self.storageKey) as? [String: String] ?? [:]
        let key = scopeKey()
        if let sessionId,
           selections[key] != sessionId.trimmingCharacters(in: .whitespacesAndNewlines) {
            return
        }
        selections.removeValue(forKey: key)
        defaults.set(selections, forKey: Self.storageKey)
    }
}

/// Scroll-to-top paging of a transcript's older messages.
struct OlderHistoryState: Equatable {
    var hasMore = false
    var isLoading = false
    /// The last page failed; the top row offers a retry.
    var failed = false
}

/// When the transcript's top row may load one older page: it came back into
/// view after leaving it (`armed`), the reader is not at the bottom, more
/// exists, and no page is in flight or waiting on a retry.
enum ChatOlderHistoryTrigger {
    static func shouldLoad(armed: Bool, isAtBottom: Bool, state: OlderHistoryState) -> Bool {
        armed && !isAtBottom && state.hasMore && !state.isLoading && !state.failed
    }
}

/// One prepend of older messages: the row that was first before it.
struct OlderHistoryPrepend: Equatable {
    let token: UInt64
    let anchorMessageId: String
}

class ChatViewModel: NSObject, ObservableObject {
    /// Empty until the session has messages: the view shows `ChatWelcomeView`
    /// (suggestions and recent chats) for an empty transcript.
    @Published var messages: [ChatMessage] = []
    @Published var composerMode: ComposerMode = .ask {
        didSet {
            guard let permission = ComposerDoPermission(mode: composerMode),
                  composerDoPermission != permission else { return }
            composerDoPermission = permission
        }
    }
    /// Do remembers Ask/Accept while Plan is active, matching the Web composer.
    @Published var composerDoPermission: ComposerDoPermission = .ask
    /// The plan question the composer answers (Planner dock → Reply). A
    /// Plan-mode send carries it as `plan_task_id` / `plan_question_id`.
    @Published var planReply: PlanReplyTarget?
    @Published var focusedMessageId: String?
    @Published var originalAnswerError: String?
    private var sessionLoadTask: Task<Void, Never>?
    /// Older-history paging for the open transcript: one page per scroll to
    /// the top, never during the initial open.
    @Published private(set) var olderHistory = OlderHistoryState()
    /// Set after a page lands on top; the transcript keeps `anchorMessageId`
    /// (the row that was first) where the reader left it.
    @Published private(set) var olderHistoryPrepend: OlderHistoryPrepend?
    private var oldestLoadedRawMessageId: String?
    private var olderHistoryGeneration: UInt64 = 0
    private var olderHistoryTask: Task<Void, Never>?
    @Published var isConnected = false
    @Published var isThinking = false
    @Published var cancelRunErrorMessage: String?
    @Published var escalationResponseErrorMessage: String?
    @Published var structuredActionErrorMessage: String?
    @Published private(set) var respondingEscalationID: String?
    @Published private(set) var currentRunExecutionId: String?

    private var realtimeSubscription: RealtimeHubSubscription?
    /// Session the composer posts to. Set when a thread is opened (loadSession);
    /// created on demand if a message is sent before a session exists. Whenever it
    /// changes, refresh the composer @-mention catalog for the new session's agent.
    private var currentSessionId: String? {
        didSet {
            guard currentSessionId != oldValue else { return }
            activateChatPreferences()
            queuedMessages = []; serverRunning = false; currentSessionOrigin = nil
            turnStates = [:]; runningServerTurnId = nil; queueReportsActive = false
            realtimeVoiceTurnIds = []
            suppressedCanonicalByTurn = [:]
            if let reply = planReply { planReply = nil; composerMode = reply.returnMode }
            sessionLoadTask?.cancel()
            resetOlderHistory()
            focusedMessageId = nil
            originalAnswerError = nil
            referenceCatalogGeneration &+= 1
            // A catalog is authorization scoped to one exact session owner.
            // Clear it synchronously before fetching so a slow response from
            // the prior session cannot leave stale agents/skills selectable.
            mentionItems = buildComposerMentionItems(agents: [], skills: [])
            if let sid = currentSessionId { fetchReferenceCatalog(sid) }
        }
    }
    /// Non-nil while an SSE-streamed reply is in progress — suppresses the WS
    /// echo of the same turn's final message to avoid a duplicate.
    private var streamingMessageId: String?
    private var transcriptGeneration: UInt64 = 0
    private let principal = MagicianAccess.principal
    private let workspace = MagicianAccess.workspace
    private let networkSession: URLSession
    private let executionControlCoordinator: ExecutionControlCoordinator?
    private let sessionSelectionStore: ChatSessionSelectionStore
    /// Monotonic authorization epoch for the session-scoped composer catalog.
    /// Network completions must match both this epoch and the session id before
    /// publishing results, so switching away and back cannot revive an older
    /// request for the same session.
    private var referenceCatalogGeneration: UInt64 = 0
    private var activeExecutionLookupGeneration: [String: UInt64] = [:]
    private var activeExecutionLookupCounter: UInt64 = 0
    private static let structuredResponseSchema = "magician.structured_response"
    private static let structuredResponseVersion = 1
    private static let structuredResponseMaxBytes = 32_768
    private static let structuredResponsePresentationMaxBytes = 65_536
    private static let structuredResponseValueMaxBytes = 2_048

    /// Read-only view of the session the composer is bound to.
    var currentSessionIdValue: String? { currentSessionId }
    var rememberedSessionId: String? { sessionSelectionStore.rememberedSessionId() }

    @Published var profiles: [ChatProfile] = []
    @Published var selectedProfile: String = UserDefaults.standard.string(forKey: "magios.chat.profile") ?? "" {
        didSet {
            guard profiles.contains(where: { $0.name == selectedProfile }) else { return }
            let resolved = ChatModelPreference.resolve(profiles: profiles, legacyID: selectedProfile, saved: nil)
            modelSelectionIssue = resolved.issue
            if resolved.issue == nil { persistModelPreference(resolved.route, selection: resolved.selection) }
        }
    }
    @Published private(set) var modelSelectionIssue: String?
    private var modelChoicesLoading = true
    private var modelCatalogGeneration: UInt64 = 0
    private var currentModelSelection: ChatModelSelection? {
        profiles.first(where: { $0.name == selectedProfile })?.modelSelection
    }

    private var hydratingChatPreferences = false
    var chatLocalStorageKey: String {
        Self.localStorageKey(origin: MagicianAccess.baseURL.absoluteString, principal: MagicianAccess.principal,
                             workspace: MagicianAccess.workspace, session: currentSessionId)
    }
    static func localStorageKey(origin: String, principal: String, workspace: String, session: String?) -> String {
        "magios.chat.v2|" + [origin, principal, workspace, session ?? "pending"].map { "\($0.utf8.count):\($0)" }.joined(separator: "|")
    }
    private func preferenceKey(_ suffix: String) -> String { chatLocalStorageKey + "|" + suffix }
    private func activateChatPreferences() {
        hydratingChatPreferences = true
        modelCatalogGeneration &+= 1
        profiles = []
        selectedProfile = UserDefaults.standard.string(forKey: "magios.chat.profile") ?? ""
        selectedHarnessEngine = UserDefaults.standard.string(forKey: preferenceKey("engine")) ?? UserDefaults.standard.string(forKey: "magios.chat.harnessEngine") ?? "magician"
        selectedHarnessModel = UserDefaults.standard.string(forKey: preferenceKey("model")) ?? UserDefaults.standard.string(forKey: "magios.chat.harnessModel") ?? "default"
        hydratingChatPreferences = false
        fetchProfiles()
    }

    private func persistModelPreference(_ route: String, selection: ChatModelSelection?) {
        guard !hydratingChatPreferences, let selection, let encoded = try? JSONEncoder().encode(selection) else { return }
        UserDefaults.standard.set(encoded, forKey: preferenceKey(ChatModelPreference.storageKey))
    }

    private func modelSelectionCanSend() -> Bool {
        guard ["magician", "pi"].contains(selectedHarnessEngine), !isUITestLaunch else { return true }
        if let issue = modelSelectionIssue {
            commandNotice = ComposerCommandNotice(text: issue, isError: true)
            return false
        }
        if modelChoicesLoading {
            commandNotice = ComposerCommandNotice(text: "Model choices are still loading. Try again in a moment.", isError: true)
            return false
        }
        if currentModelSelection == nil {
            commandNotice = ComposerCommandNotice(text: ChatModelPreference.unavailableMessage, isError: true)
            return false
        }
        return true
    }
    @Published var chatHarnesses: [ChatHarnessOption] = [ChatHarnessOption(name: "magician", installed: true, models: ["default"])]
    @Published var selectedHarnessEngine: String = UserDefaults.standard.string(forKey: "magios.chat.harnessEngine") ?? "magician" {
        didSet {
            guard !hydratingChatPreferences else { return }
            UserDefaults.standard.set(selectedHarnessEngine, forKey: preferenceKey("engine"))
            if oldValue != selectedHarnessEngine {
                selectedHarnessModel = "default"
                fetchProfiles()
            }
        }
    }
    @Published var selectedHarnessModel: String = UserDefaults.standard.string(forKey: "magios.chat.harnessModel") ?? "default" {
        didSet { if !hydratingChatPreferences { UserDefaults.standard.set(selectedHarnessModel, forKey: preferenceKey("model")) } }
    }
    /// The run engine from the roster (`/run-engine` marks it).
    @Published private(set) var runHarnessEngine: String?
    @Published private(set) var runHarnessModel: String?
    /// Pi's run model, and whether agentic flows follow the chat engine.
    @Published private(set) var runModelSelection: ChatModelSelection?
    @Published private(set) var runFollowsChat = false
    /// The server's chat engine (`/engine` "everywhere").
    @Published private(set) var serverChatEngine: String?
    /// `/engine <magician|pi>` and `/run-engine pi`: the catalog per engine,
    /// loaded without touching the chat's own engine or model list.
    @Published private(set) var commandCatalogs: [String: [ChatProfile]] = [:]
    @Published private(set) var commandCatalogErrors: [String: String] = [:]
    /// `/run-engine magician`: the `agentic_decision` models and choice.
    @Published private(set) var loopModels: [ChatModelChoice]?
    @Published private(set) var loopSelection: ChatModelSelection?
    @Published private(set) var loopModelsError: String?
    /// `/budget default`: the caps read from the server, or why they could not be.
    @Published private(set) var budgetDefaults: BudgetDefaults?
    @Published private(set) var budgetDefaultsError: String?
    /// The last slash command's confirmation, shown briefly over the composer.
    @Published var commandNotice: ComposerCommandNotice?
    /// Files staged for the next send — shown as preview chips in the composer.
    @Published var stagedAttachments: [StagedAttachment] = []
    /// Messages queued behind the in-flight turn (surfaced by the QueueInspector).
    @Published var queuedMessages: [QueuedMessage] = []
    @Published private(set) var serverRunning = false
    /// The session's chat turns by id, as the server last reported them
    /// (`turn_state` snapshots and `chat.turn.state_changed` events).
    private var turnStates: [String: ChatTurnState] = [:]
    /// The non-voice turn the server says is running in this session. Its
    /// bubble stays live (steps, Stop) whether or not this device still
    /// reads the turn's reply stream: after a stream drop or a relaunch the
    /// server keeps the turn running (`continue_on_disconnect`).
    @Published private(set) var runningServerTurnId: String?
    /// The queue endpoint's `active` (a run or a task tail holds the session).
    private var queueReportsActive = false
    /// Canonical replies the realtime path held back because this device's
    /// SSE stream owned the turn; applied if that stream ends without one.
    private var suppressedCanonicalByTurn: [String: ChatMessage] = [:]
    private var transcriptReloadScheduled = false
    private var transcriptReloadTurns: Set<String> = []
    /// Turns of a realtime voice call (their rows carry the call's
    /// `presence_session_id`): the call surface shows them, never a bubble.
    private var realtimeVoiceTurnIds: Set<String> = []
    /// Turns whose stream dropped after partial text; their canonical reply
    /// still has to replace that text.
    private var partialReplyTurns: Set<String> = []
    @Published private(set) var currentSessionOrigin: ConcurrentSessionOrigin?
    private var queueReadInFlight = false
    @Published private(set) var queueMutationInFlight = false
    @Published var queueErrorMessage: String?
    @Published var failedConcurrentInput: String?
    /// Mirrors whether the owner picked a voice topic to continue; set by
    /// `ChatView` from the main-actor coordinator.
    var concurrentVoiceTopicSelected = false
    @Published private(set) var queueNoticeMessage: String?
    /// Composer @-mention catalog (agents / tools / personalities / features)
    /// scoped to the active session's agent. Fetched on session change.
    @Published var mentionItems: [ComposerMentionItem] = buildComposerMentionItems(agents: [], skills: [])
    /// Accumulates this turn's granular events into activity rows. Reset when a
    /// send starts; snapshot onto the assistant message when the turn finalizes.
    private let activity = ChatTurnActivityAccumulator()
    /// Canonical per-turn activity state. The general realtime socket remains a
    /// fallback, while these rows come from the same durable turn projection as web.
    private var activityEventsByTurn: [String: [[String: Any]]] = [:]
    private var activityRowsByTurn: [String: [ActivityRow]] = [:]
    private var hydratedActivityTurns: Set<String> = []
    private var activityLoadsInFlight: Set<String> = []
    private var forcedActivityRefreshPending: [String: (sessionId: String, messageId: String)] = [:]
    /// The turn whose live steps this chat follows, and its hub subscription.
    private var activityStreamTurnId: String?
    private var turnActivitySubscription: RealtimeHubSubscription?
    /// Request-body streaming and delegated-task liveness are separate. The
    /// former ends with the assistant response; the latter can continue for
    /// minutes and is derived from correlated task status cards.
    private var activeSendingChatTurnId: String?

    /// Load the session-scoped reference catalog and rebuild the mention list.
    /// Tutor feature mentions are always present on iOS.
    /// True while the in-flight turn originated from voice dictation.
    private var currentTurnViaVoice = false

    /// Small seams keep the secure-screen gate and native handoff testable
    /// without constructing UIKit presentation or audio objects.
    var guidedFlowScreenIsLocked: () -> Bool = { DeviceScreenLock.isLocked }
    var guidedFlowSpeaker: (String) -> Void = { message in
        SpeechSynthesizer.shared.speak(message, messageId: "guided-flow-screen-gate")
    }
    var tutorPresenter: @MainActor (String, UIImage?, Bool) -> Void = { question, image, autoStart in
        TutorOverlayRouter.shared.present(
            question: question,
            image: image,
            autoStart: autoStart
        )
    }

    /// Speak an assistant reply aloud on finalize — when auto-speak is on, or when
    /// this turn started via voice (voice in → voice out).
    private func speakReplyIfEnabled(_ text: String, messageId: String) {
        // A realtime Live call speaks its own reply and owns the shared audio
        // session, so never add the on-device voice on top of it: that would
        // double the audio AND (via SpeechSynthesizer's session takeover) kill
        // the still-running capture engine, leaving the next turn's mic dead.
        if !TutorAudioFocus.shared.isActive,
           !VoiceCallAudioFocus.shared.isActive,
           AudioSettings.shared.speakReplies || currentTurnViaVoice {
            SpeechSynthesizer.shared.speak(text, messageId: messageId, chatSessionId: currentSessionId)
        }
    }

    func fetchReferenceCatalog(_ sessionId: String) {
        let generation = referenceCatalogGeneration
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/reference-catalog") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, _, _ in
            guard let data,
                  let catalog = try? JSONDecoder().decode(ReferenceCatalogResponse.self, from: data) else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self,
                      self.currentSessionId == sessionId,
                      self.referenceCatalogGeneration == generation else { return }
                // Completed tasks become @-mentions too (web parity) — fetch,
                // then publish only if the exact authorization epoch remains current.
                self.fetchMentionTasks { tasks in
                    let items = buildComposerMentionItems(
                        agents: catalog.agents ?? [], skills: catalog.skills ?? [], tasks: tasks)
                    DispatchQueue.main.async { [weak self] in
                        guard let self,
                              self.currentSessionId == sessionId,
                              self.referenceCatalogGeneration == generation else { return }
                        self.mentionItems = items
                    }
                }
            }
        }.resume()
    }

    /// Fetch completed tasks (GET /v3/tasks) to offer as `@`-mentions. Mirrors web,
    /// which mentions `taskStore.tasks.filter(status === 'completed')`.
    private func fetchMentionTasks(_ completion: @escaping ([(id: String?, title: String?)]) -> Void) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3/tasks") else {
            completion([]); return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { data, _, _ in
            let tasks = (data.flatMap { try? JSONDecoder().decode(TaskListResponse.self, from: $0) }?.tasks ?? [])
                .filter { $0.status == "completed" }
                .map { (id: $0.id as String?, title: $0.title as String?) }
            completion(tasks)
        }.resume()
    }

    /// An incoming `escalation_resolved` flips the matching open escalation card to
    /// its resolved state (web renders it as the resolved card); if no open card
    /// matches and the payload is self-renderable, append it resolved.
    private func markEscalationResolved(_ resolved: ChatMessageContentData) {
        if let idx = messages.lastIndex(where: {
            if case .escalation(let c) = $0.type { return Self.escalationMatches(c, resolved) }
            return false
        }), case .escalation(var c) = messages[idx].type {
            c.resolved = true
            messages[idx].type = .escalation(c)
        } else if resolved.question != nil, resolved.options != nil {
            var c = resolved; c.resolved = true
            messages.append(ChatMessage(id: resolved.correlationId ?? UUID().uuidString,
                                        isUser: false, text: "", type: .escalation(c)))
        } else {
            let summary = resolved.summary ?? resolved.inactiveReason ?? "Request resolved"
            messages.append(ChatMessage(
                id: resolved.correlationId ?? resolved.requestId ?? UUID().uuidString,
                isUser: false,
                text: summary,
                type: .system(summary)
            ))
        }
    }

    /// Two escalation payloads refer to the same request (by execution or HITL id).
    static func escalationMatches(_ a: ChatMessageContentData, _ b: ChatMessageContentData) -> Bool {
        let aCorrelation = a.hitlCorrelationId
        let bCorrelation = b.hitlCorrelationId
        if let aCorrelation, !aCorrelation.isEmpty,
           let bCorrelation, !bCorrelation.isEmpty {
            // One planning execution may own several pending clarification
            // questions. Once both cards carry a canonical id, execution-level
            // fallback would resolve the wrong sibling question.
            return aCorrelation == bCorrelation
        }
        if let execution = a.executionId, !execution.isEmpty {
            return execution == b.executionId
        }
        return false
    }

    init(
        networkSession: URLSession = .shared,
        connectsOnInit: Bool = true,
        executionControlCoordinator: ExecutionControlCoordinator? = nil,
        sessionSelectionStore: ChatSessionSelectionStore = .shared
    ) {
        self.networkSession = networkSession
        self.executionControlCoordinator = executionControlCoordinator
        self.sessionSelectionStore = sessionSelectionStore
        super.init()
        if connectsOnInit {
            connect()
            fetchProfiles()
            fetchChatHarnesses()
        }
    }

    // Subscribe to the app's realtime hub — the one socket every surface shares
    // (`RealtimeHub`, `/api/magician/v2/realtime/ws` with structured
    // presentation). Chat messages are SENT over REST (POST .../messages); the
    // assistant reply + execution-panel updates arrive here as broadcast
    // events. The hub reconnects and resumes on its own; when events may have
    // been missed it asks for a resync, answered by re-reading the
    // conversation over REST.
    func connect() {
        guard !isRunningUnderTests else { return }   // no realtime in unit tests
        guard realtimeSubscription == nil else { return }
        realtimeSubscription = RealtimeHub.shared.subscribe(
            onEvent: { [weak self] event in self?.handleIncomingObject(event.object) },
            onResync: { [weak self] in self?.resyncFromServer() }
        )
        isConnected = true
    }

    func disconnect() {
        realtimeSubscription?.cancel()
        realtimeSubscription = nil
        stopActivityStream()
        isConnected = false
    }

    /// Events may have been missed (a reconnect the hub could not resume).
    /// Re-read what this chat shows: the transcript when no reply is
    /// streaming (a reload mid-stream would drop the streaming bubble), the
    /// followed turn's steps, and the active task cards' run targets.
    private func resyncFromServer() {
        guard let sessionId = currentSessionId, !sessionId.isEmpty else { return }
        if streamingMessageId == nil, activeSendingChatTurnId == nil {
            loadSession(sessionId)
            return
        }
        if let turnId = activityStreamTurnId {
            refreshLiveTurnActivity(sessionId: sessionId, chatTurnId: turnId)
        }
        let activeTaskIds = Set(messages.compactMap { message -> String? in
            guard case .taskStatus(let task) = message.type, !task.isTerminal else { return nil }
            return task.taskId
        })
        activeTaskIds.forEach { refreshAuthoritativeExecutionTarget(for: $0) }
    }

    // MARK: - Session actions
    /// Adopt a freshly-created (or empty) session — reset the transcript.
    func startNewSession(_ id: String?) {
        resetTurnActivityState()
        if let id {
            sessionSelectionStore.remember(id)
        } else if let currentSessionId {
            sessionSelectionStore.forget(currentSessionId)
        }
        currentSessionId = id
        resetOlderHistory()
        currentRunExecutionId = nil
        activeExecutionLookupGeneration.removeAll()
        messages = []
    }

    func forgetRememberedSession(_ id: String?) {
        sessionSelectionStore.forget(id)
    }

    /// Clear the current session's transcript locally and on the server.
    func clearMessages() {
        resetTurnActivityState()
        resetOlderHistory()
        messages = []
        if let sessionId = currentSessionId { deleteRemote("chat/sessions/\(sessionId)/messages") }
    }

    /// Delete a single message locally and on the server.
    func deleteMessage(_ id: String) {
        messages.removeAll { $0.id == id }
        // The server answers an unknown `before` cursor with the NEWEST page,
        // so paging past a deleted oldest row would re-read the loaded history.
        if oldestLoadedRawMessageId == id { oldestLoadedRawMessageId = messages.first?.id }
        if let sessionId = currentSessionId { deleteRemote("chat/sessions/\(sessionId)/messages/\(id)") }
    }

    private func resetTurnActivityState() {
        transcriptGeneration &+= 1
        streamingMessageId = nil
        isThinking = false
        currentTurnViaVoice = false
        stopActivityStream()
        activeSendingChatTurnId = nil
        activityEventsByTurn.removeAll()
        activityRowsByTurn.removeAll()
        hydratedActivityTurns.removeAll()
        activityLoadsInFlight.removeAll()
        forcedActivityRefreshPending.removeAll()
        suppressedCanonicalByTurn.removeAll()
        partialReplyTurns.removeAll()
        activity.reset()
    }

    private func deleteRemote(_ path: String) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/\(path)") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { _, _, _ in }.resume()
    }

    // MARK: - Queue
    func fetchQueue() {
        guard !queueReadInFlight, let sessionId = currentSessionId else { return }
        queueReadInFlight = true
        readQueue(sessionId: sessionId) { [weak self] _ in self?.queueReadInFlight = false }
    }

    /// Read the session queue, which also carries the latest turn's state,
    /// and apply both. `completion` (main queue) gets the turn-state
    /// snapshot, or nil when the read failed or the session changed.
    private func readQueue(sessionId: String, completion: @escaping (ChatTurnStateSnapshot?) -> Void) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/queue") else {
            completion(nil)
            return
        }
        let base = MagicianAccess.baseURL.absoluteString
        var request = URLRequest(url: url); request.timeoutInterval = 10
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            let queue = data.flatMap { try? JSONDecoder().decode(QueueResponse.self, from: $0) }
            let snapshot = ChatTurnStateSnapshot.parse(responseData: data, sessionId: sessionId)
            DispatchQueue.main.async {
                guard let self else { return }
                guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base,
                      (response as? HTTPURLResponse)?.statusCode == 200 else {
                    completion(nil)
                    return
                }
                if let queue {
                    self.queuedMessages = queue.queued
                    self.queueReportsActive = queue.active ?? false
                }
                self.applyTurnStateSnapshot(snapshot, sessionId: sessionId, fromTranscriptLoad: false)
                self.refreshServerRunning()
                completion(snapshot)
            }
        }.resume()
    }

    private func refreshServerRunning() {
        let running = queueReportsActive || runningServerTurnId != nil
        if serverRunning != running { serverRunning = running }
    }

    func actOnQueue(_ id: String, action: String) {
        guard !queueMutationInFlight, let sessionId = currentSessionId else { return }
        mutateQueue(path: "chat/sessions/\(sessionId)/queue/\(id)/action",
            successNotice: action == "parallel" ? "Running in parallel. Current work continues." : "This message will run next.",
            method: "POST", body: ["action": action]) { [weak self] in self?.fetchQueue() }
    }

    private func enqueueText(_ text: String, stopAndSend: Bool) {
        guard !queueMutationInFlight, let sessionId = currentSessionId,
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/queue") else { return }
        let attachments = stagedAttachments
        var body: [String: Any] = ["text": text, "chat_turn_id": "ios-chat-\(UUID().uuidString)",
            "attachment_ids": attachments.compactMap(\.remoteId), "source_surface": "mobile"]
        body["mode"] = composerMode.wire
        guard let fields = ChatModelPreference.requestFields(selection: currentModelSelection, engine: selectedHarnessEngine, nativeModel: selectedHarnessModel) else { return }
        body.merge(fields) { _, new in new }
        queueMutationInFlight = true; stagedAttachments = []
        var request = URLRequest(url: url)
        request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            let object = data.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
            DispatchQueue.main.async {
                guard let self else { return }
                self.queueMutationInFlight = false
                guard self.currentSessionId == sessionId else { return }
                guard error == nil, let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status),
                      let receipt = object?["queued"] as? [String: Any], let id = receipt["id"] as? String else {
                    self.failedConcurrentInput = text; self.stagedAttachments = attachments
                    self.queueErrorMessage = object?["error"] as? String ?? error?.localizedDescription ?? "Could not queue message."
                    return
                }
                if stopAndSend { self.actOnQueue(id, action: "stop_and_send") }
                self.fetchQueue()
            }
        }.resume()
    }

    func deleteQueued(_ id: String) {
        guard !queueMutationInFlight, let sessionId = currentSessionId else { return }
        mutateQueue(
            path: "chat/sessions/\(sessionId)/queue/\(id)",
            successNotice: "Queued message removed."
        ) { [weak self] in
            self?.queuedMessages.removeAll { $0.id == id }
        }
    }

    func clearQueued() {
        guard !queueMutationInFlight, !queuedMessages.isEmpty, let sessionId = currentSessionId else { return }
        let count = queuedMessages.count
        mutateQueue(
            path: "chat/sessions/\(sessionId)/queue",
            successNotice: "Cleared \(count) queued message\(count == 1 ? "" : "s")."
        ) { [weak self] in
            self?.queuedMessages = []
        }
    }

    private func mutateQueue(
        path: String,
        successNotice: String,
        method: String = "DELETE", body: [String: String]? = nil,
        apply: @escaping () -> Void
    ) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/\(path)") else { return }
        queueMutationInFlight = true
        queueErrorMessage = nil
        queueNoticeMessage = nil
        var request = URLRequest(url: url)
        request.httpMethod = method
        if let body { request.httpBody = try? JSONSerialization.data(withJSONObject: body); request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        let sessionId = currentSessionId
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { return }
                self.queueMutationInFlight = false
                guard self.currentSessionId == sessionId else { return }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard error == nil, (200..<300).contains(status) else {
                    let object = data.flatMap {
                        try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                    }
                    self.queueErrorMessage = (object?[
                        "error"
                    ] as? String) ?? (object?["message"] as? String)
                        ?? error?.localizedDescription
                        ?? "Could not update the queued messages (HTTP \(status))."
                    self.fetchQueue()
                    return
                }
                apply()
                self.queueNoticeMessage = successNotice
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.4) { [weak self] in
                    if self?.queueNoticeMessage == successNotice { self?.queueNoticeMessage = nil }
                }
            }
        }.resume()
    }

    // MARK: - Sending (REST; the reply streams back over the realtime WS above)
    private func rejectGuidedFlow(_ message: String, speak: Bool) {
        queueNoticeMessage = message
        if speak { guidedFlowSpeaker(message) }
    }

    /// When `viaVoice` is true (the message came from dictation), the reply is
    /// spoken even if auto-speak is off — voice in, voice out.
    func sendMessage(_ text: String, viaVoice: Bool = false, background: Bool = false, stopAndSend: Bool = false) {
        var admittedConcurrently = false
        defer {
            if viaVoice && !admittedConcurrently {
                Task { @MainActor in ConcurrentVoiceCoordinator.shared.inputSettled() }
            }
        }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        // Native feature lane: @brainstorm opens a Thinking Map seeded with the
        // text after the invoke word. It is intentionally not posted as an
        // ordinary chat turn; the map owns its dedicated facilitator session.
        if BrainstormInvoke.isBrainstormInvoke(trimmed) {
            let seed = BrainstormInvoke.strip(trimmed)
            Task { @MainActor in
                ThinkingMapRouter.shared.present(initialThought: seed)
            }
            return
        }

        // Voice parity: source-free Tutor/Tutor Quick opens the native blackboard
        // and starts immediately. Screen Tutor and App Copilot are recognized but
        // rejected on iOS, so they can never fall through as ordinary chat turns.
        if viaVoice, let invocation = TutorInvoke.parseVoiceGuidedFlow(trimmed) {
            if guidedFlowScreenIsLocked() {
                rejectGuidedFlow(
                    DeviceScreenLock.message(for: invocation.feature),
                    speak: true
                )
                return
            }
            guard invocation.feature == .tutor else {
                rejectGuidedFlow("App Copilot isn't available on this device yet.", speak: true)
                return
            }
            guard !invocation.requiresScreenCapture else {
                rejectGuidedFlow(
                    "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead.",
                    speak: true
                )
                return
            }
            let concept = TutorInvoke.strip(invocation.normalizedText)
            Task { @MainActor in
                guard !self.guidedFlowScreenIsLocked() else {
                    self.rejectGuidedFlow(
                        DeviceScreenLock.message(for: .tutor),
                        speak: true
                    )
                    return
                }
                self.tutorPresenter(concept, nil, true)
            }
            return
        }

        // Keyboard parity: a @tutor turn opens the Tutor overlay instead of
        // posting a chat message. A staged image selects screen overlay; none
        // selects blackboard. Actual device lock is checked before either starts.
        if TutorInvoke.isTutorInvoke(trimmed) {
            if guidedFlowScreenIsLocked() {
                rejectGuidedFlow(
                    DeviceScreenLock.message(for: .tutor),
                    speak: viaVoice
                )
                return
            }
            let imageBytes = stagedAttachments.first(where: { $0.isImage })?.thumbnail
            let concept = TutorInvoke.strip(trimmed)
            Task { @MainActor in
                guard !self.guidedFlowScreenIsLocked() else {
                    self.rejectGuidedFlow(
                        DeviceScreenLock.message(for: .tutor),
                        speak: viaVoice
                    )
                    return
                }
                self.stagedAttachments = []   // consumed only by an admitted hand-off
                self.tutorPresenter(
                    concept,
                    imageBytes.flatMap(UIImage.init(data:)),
                    viaVoice
                )
            }
            return
        }

        guard modelSelectionCanSend() else { return }

        // A spoken turn only branches when it would otherwise wait behind a
        // running turn, or when the owner picked a voice topic to continue.
        // An idle session takes it as an ordinary chat turn.
        let voiceBranches = viaVoice && (isThinking || serverRunning || concurrentVoiceTopicSelected)
        if (background || voiceBranches), composerMode != .plan, stagedAttachments.isEmpty {
            admittedConcurrently = true
            let parent = currentSessionId
            guard let fields = ChatModelPreference.requestFields(selection: currentModelSelection, engine: selectedHarnessEngine, nativeModel: selectedHarnessModel) else { return }
            var options: [String: Any] = ["source_surface": "ios"]
            options.merge(fields) { _, new in new }
            options["mode"] = composerMode.wire
            Task { @MainActor in
                let coordinator = ConcurrentVoiceCoordinator.shared
                coordinator.voiceInteracted = coordinator.voiceInteracted || viaVoice
                do {
                    let session: String?
                    if let parent { session = parent }
                    else { session = await withCheckedContinuation { continuation in
                        self.createSession { continuation.resume(returning: $0) }
                    } }
                    guard let session else { throw NSError(domain: "VoiceRequests", code: 0, userInfo: [NSLocalizedDescriptionKey: "Could not open a session."]) }
                    if parent == nil { self.currentSessionId = session; self.sessionSelectionStore.remember(session) }
                    try await coordinator.submit(session, text: trimmed, options: options, voiceInput: viaVoice)
                } catch {
                    if viaVoice { coordinator.inputSettled() }; coordinator.error = error.localizedDescription
                    self.failedConcurrentInput = trimmed
                }
            }
            return
        }

        if composerMode != .plan && (isThinking || serverRunning || stopAndSend) {
            enqueueText(trimmed, stopAndSend: stopAndSend)
            return
        }
        currentTurnViaVoice = viaVoice
        let chatTurnId = "ios-chat-\(UUID().uuidString)"
        activeSendingChatTurnId = chatTurnId
        let postedMode = composerMode.wire
        // A Plan-mode send while replying answers that question; the
        // composer then returns to the mode it came from.
        let postedPlanReply = composerMode == .plan ? planReply : nil
        if let reply = postedPlanReply {
            planReply = nil
            composerMode = reply.returnMode
        }
        let postedHarnessEngine = selectedHarnessEngine
        let postedHarnessModel = selectedHarnessModel
        let postedModelSelection = currentModelSelection

        // Optimistic local echo of the user's message.
        var echo = ChatMessage(id: UUID().uuidString, isUser: true, text: trimmed, type: .text)
        echo.voiceOrigin = viaVoice
        echo.chatTurnId = chatTurnId
        echo.createdAt = Date()
        echo.planReplyContext = postedPlanReply?.taskTitle
        messages.append(echo)
        isThinking = true
        activity.reset()  // fresh activity timeline for this turn
        SpeechSynthesizer.shared.stop()  // hush any prior spoken reply

        if let sessionId = currentSessionId {
            postMessageStreaming(
                trimmed, to: sessionId, chatTurnId: chatTurnId, mode: postedMode,
                harnessEngine: postedHarnessEngine, harnessModel: postedHarnessModel,
                modelSelection: postedModelSelection, planReply: postedPlanReply
            )
        } else {
            createSession { [weak self] sessionId in
                guard let self = self else { return }
                guard let sessionId = sessionId else {
                    self.isThinking = false
                    self.activeSendingChatTurnId = nil
                    self.synchronizeActivityLivenessAndStream()
                    debugLog("Chat send failed: could not open a session")
                    return
                }
                self.currentSessionId = sessionId
                self.sessionSelectionStore.remember(sessionId)
                self.postMessageStreaming(
                    trimmed,
                    to: sessionId,
                    chatTurnId: chatTurnId,
                    mode: postedMode,
                    harnessEngine: postedHarnessEngine,
                    harnessModel: postedHarnessModel,
                    modelSelection: postedModelSelection,
                    planReply: postedPlanReply
                )
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in self?.fetchQueue() }
    }

    // Send + stream the reply token-by-token over SSE (POST .../messages/stream).
    // While streaming, the WS echo of the same turn is suppressed (streamingMessageId).
    private func postMessageStreaming(
        _ text: String,
        to sessionId: String,
        chatTurnId: String,
        mode: String? = nil,
        harnessEngine: String,
        harnessModel: String,
        modelSelection: ChatModelSelection?,
        planReply: PlanReplyTarget? = nil
    ) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/messages/stream") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)
        let viaVoice = currentTurnViaVoice
        var payload: [String: Any] = [
            "text": text,
            "chat_turn_id": chatTurnId,
            "source_surface": "ios",
            // The server owns the accepted turn after this point. If iOS is
            // suspended or killed, history/realtime reconciles the persisted
            // result instead of replaying tools under a second request.
            "continue_on_disconnect": true
        ]
        guard let fields = ChatModelPreference.requestFields(selection: modelSelection, engine: harnessEngine, nativeModel: harnessModel) else {
            isThinking = false
            commandNotice = ComposerCommandNotice(text: ChatModelPreference.unavailableMessage, isError: true)
            return
        }
        payload.merge(fields) { _, new in new }
        let uploadedIds = stagedAttachments.compactMap { $0.remoteId }
        if !uploadedIds.isEmpty { payload["attachment_ids"] = uploadedIds }
        if viaVoice { payload["voice_origin"] = true }   // web parity: stamp the turn's origin
        if let mode { payload["mode"] = mode }
        // Web parity (`chatStore.sendMessage` planTaskId / planQuestionId).
        if let planReply {
            payload["plan_task_id"] = planReply.taskId
            payload["plan_question_id"] = planReply.questionId
        }
        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)
        stagedAttachments = []  // consumed by this send

        let streamId = "streaming-\(UUID().uuidString)"
        let generation = transcriptGeneration
        streamingMessageId = streamId

        // The response owns the activity disclosure from the moment the turn
        // starts. Tool-heavy turns may not emit answer text until all work is
        // finished; waiting for that first token left the live activity stream
        // with no bubble to update.
        var placeholder = ChatMessage(id: streamId, isUser: false, text: "", type: .text)
        placeholder.voiceOrigin = viaVoice
        placeholder.chatTurnId = chatTurnId
        placeholder.createdAt = Date()
        placeholder.activityRows = activityRowsByTurn[chatTurnId] ?? activity.snapshot()
        placeholder.activityIsLive = isActivityTurnLive(chatTurnId)
        messages.append(placeholder)
        startActivityStream(sessionId: sessionId, chatTurnId: chatTurnId)
        Task { [weak self] in
            guard let self = self else { return }
            var end = ChatStreamEnd()
            do {
                let (bytes, response) = try await networkSession.bytes(for: request)
                if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
                    end.rejected = true
                    await self.finishStreaming(streamId, chatTurnId: chatTurnId, generation: generation, end: end)
                    return
                }
                var accumulated = ""
                // SSE frames are `event: <name>` / `data: {…}`. Tokens carry
                // `text`; `error` carries `error`; the final `done` frame is
                // the turn's `ChatResponse` (`assistant_message`, `cancelled`).
                var frameEvent = ""
                for try await line in bytes.lines {
                    if line.hasPrefix("event:") {
                        frameEvent = line.dropFirst(6).trimmingCharacters(in: .whitespaces)
                        continue
                    }
                    guard line.hasPrefix("data:") else {
                        if line.isEmpty { frameEvent = "" }
                        continue
                    }
                    let json = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                    guard let d = json.data(using: .utf8),
                          let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any] else { continue }
                    let event = frameEvent
                    frameEvent = ""
                    if event == "error" {
                        // The turn may still recover (a retried provider
                        // call), so the bubble stays; the turn's state
                        // decides what it shows when the stream ends.
                        let message = (obj["error"] as? String) ?? (obj["message"] as? String) ?? ""
                        end.errorMessage = message
                        debugLog("Chat stream error frame: \(message)")
                        continue
                    }
                    if event == "done" || obj.keys.contains("assistant_message") || obj["cancelled"] != nil {
                        end.sawDone = true
                        if (obj["cancelled"] as? Bool) == true { end.cancelled = true }
                    }
                    // The final `done` frame carries the persisted reply, which
                    // the server checks against the turn's tool results; it
                    // replaces the streamed draft (and is what gets spoken).
                    if let assistant = obj["assistant_message"] as? [String: Any],
                       let content = assistant["content"] as? [String: Any],
                       let finalText = content["text"] as? String,
                       !finalText.isEmpty {
                        accumulated = finalText
                        await MainActor.run {
                            guard self.transcriptGeneration == generation,
                                  self.currentSessionId == sessionId,
                                  let idx = self.messages.firstIndex(where: { $0.id == streamId })
                            else { return }
                            self.messages[idx].text = finalText
                        }
                        continue
                    }
                    guard event.isEmpty || event == "token" || event == "message",
                          let token = obj["text"] as? String, !token.isEmpty else { continue }
                    accumulated += token
                    let snapshot = accumulated
                    await MainActor.run {
                        guard self.transcriptGeneration == generation,
                              self.currentSessionId == sessionId else { return }
                        self.isThinking = false
                        if let idx = self.messages.firstIndex(where: { $0.id == streamId }) {
                            self.messages[idx].text = snapshot
                        } else {
                            // A realtime/session refresh may have replaced the
                            // optimistic placeholder. Recreate it defensively
                            // so streamed text still has an owner.
                            var m = ChatMessage(id: streamId, isUser: false, text: snapshot, type: .text)
                            m.voiceOrigin = viaVoice   // reply to a voice turn carries the badge
                            m.chatTurnId = chatTurnId
                            m.createdAt = Date()
                            m.activityRows = self.activityRowsByTurn[chatTurnId]
                                ?? self.activity.snapshot()
                            m.activityIsLive = self.isActivityTurnLive(chatTurnId)
                            self.messages.append(m)
                        }
                    }
                }
                end.gotContent = !accumulated.isEmpty
                await self.finishStreaming(streamId, chatTurnId: chatTurnId, generation: generation, end: end)
            } catch {
                end.transportFailed = true
                end.gotContent = await MainActor.run {
                    self.messages.first(where: { $0.id == streamId })?.text.isEmpty == false
                }
                await self.finishStreaming(streamId, chatTurnId: chatTurnId, generation: generation, end: end)
                debugLog("Chat stream failed: \(error.localizedDescription)")
            }
        }
    }

    /// The stream for `chatTurnId` ended. A complete reply finalizes as
    /// before. Anything else (no reply, a Stop, an error frame, a dropped
    /// connection) does not end the turn on the server, which keeps running
    /// it (`continue_on_disconnect`): the turn's state decides whether the
    /// bubble stays live, becomes the reply, or shows a status line.
    @MainActor
    private func finishStreaming(_ streamId: String, chatTurnId: String, generation: UInt64, end: ChatStreamEnd) {
        guard transcriptGeneration == generation else { return }
        if activeSendingChatTurnId == chatTurnId {
            activeSendingChatTurnId = nil
        }
        if end.gotContent && !end.transportFailed {
            isThinking = false
            if end.sawDone, !end.cancelled {
                recordLocalCompletion(chatTurnId)
            }
            finalizeStreamedReply(streamId, chatTurnId: chatTurnId)
            synchronizeActivityLivenessAndStream()
            return
        }

        // From here the realtime path owns the turn's canonical reply.
        if streamingMessageId == streamId { streamingMessageId = nil }
        if let stashed = suppressedCanonicalByTurn.removeValue(forKey: chatTurnId) {
            isThinking = false
            adoptCanonicalReply(stashed)
            synchronizeActivityLivenessAndStream()
            return
        }
        if let state = turnStates[chatTurnId], !(state.isRunning && end.sawDone) {
            isThinking = false
            if state.isRunning {
                markTurnBubbleLive(chatTurnId)
            } else {
                settleTurnBubble(state)
            }
            synchronizeActivityLivenessAndStream()
            return
        }
        guard let sessionId = currentSessionId, !sessionId.isEmpty else {
            isThinking = false
            settleStreamWithoutServerState(streamId, chatTurnId: chatTurnId, end: end, serverForgotTurn: false)
            synchronizeActivityLivenessAndStream()
            return
        }
        readQueue(sessionId: sessionId) { [weak self] snapshot in
            guard let self, self.transcriptGeneration == generation else { return }
            if self.activeSendingChatTurnId == nil { self.isThinking = false }
            switch snapshot {
            case .latest(let state) where state.chatTurnId == chatTurnId:
                // Applied by `readQueue`: live, the reply, or a status line.
                break
            case nil where end.transportFailed:
                // Offline: the server most likely still runs the turn. Keep
                // the bubble live; the queue poll or a resync settles it.
                self.recordTurnState(ChatTurnState(
                    chatTurnId: chatTurnId, sessionId: sessionId, status: .running,
                    startedAt: Date().timeIntervalSince1970 * 1000))
                self.markTurnBubbleLive(chatTurnId)
            case .noTurn:
                self.settleStreamWithoutServerState(
                    streamId, chatTurnId: chatTurnId, end: end, serverForgotTurn: true)
            default:
                self.settleStreamWithoutServerState(
                    streamId, chatTurnId: chatTurnId, end: end, serverForgotTurn: false)
            }
            self.synchronizeActivityLivenessAndStream()
        }
    }

    /// Attach the turn's activity to a fully streamed reply, speak it, and
    /// hold off the realtime duplicate of the same reply for a short grace.
    private func finalizeStreamedReply(_ streamId: String, chatTurnId: String) {
        if let idx = messages.firstIndex(where: { $0.id == streamId }) {
            messages[idx].chatTurnId = chatTurnId
            messages[idx].activityRows = activityRowsByTurn[chatTurnId] ?? activity.snapshot()
            messages[idx].activityIsLive = isActivityTurnLive(chatTurnId)
            speakReplyIfEnabled(messages[idx].text, messageId: streamId)
        }
        refreshActivityRows(
            sessionId: currentSessionId,
            messageId: streamId,
            chatTurnId: chatTurnId,
            force: true
        )
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            guard let self, self.streamingMessageId == streamId else { return }
            self.streamingMessageId = nil
            self.suppressedCanonicalByTurn.removeValue(forKey: chatTurnId)
        }
    }

    /// The stream ended without a reply and the server has no state for the
    /// turn (an older server, a rejected send, or a restart). Show what the
    /// stream itself said; with nothing to show, drop the empty bubble and
    /// let the realtime feed deliver whatever the turn produced.
    private func settleStreamWithoutServerState(
        _ streamId: String,
        chatTurnId: String,
        end: ChatStreamEnd,
        serverForgotTurn: Bool
    ) {
        let notice: String?
        if end.cancelled {
            notice = ChatTurnState.stoppedNotice
        } else if end.errorMessage != nil {
            notice = ChatTurnState.failedNotice
        } else if serverForgotTurn && end.transportFailed {
            notice = ChatTurnState.interruptedNotice
        } else {
            notice = nil
        }
        guard let idx = messages.firstIndex(where: { $0.id == streamId }) else { return }
        if let notice {
            messages[idx].turnStatusNotice = notice
            messages[idx].activityIsLive = false
        } else if messages[idx].text.isEmpty, messages[idx].turnStatusNotice == nil {
            messages.remove(at: idx)
        } else {
            finalizeStreamedReply(streamId, chatTurnId: chatTurnId)
        }
    }

    // MARK: - Server turn state (`turn_state`, `chat.turn.state_changed`)

    /// Record one state of a turn in the open session (the recording rule
    /// applies) and refresh which turn runs. False when it was ignored.
    @discardableResult
    private func recordTurnState(_ state: ChatTurnState) -> Bool {
        guard state.sessionId == currentSessionId,
              ChatTurnState.shouldRecord(state, over: turnStates[state.chatTurnId]) else { return false }
        turnStates[state.chatTurnId] = state
        recomputeRunningServerTurn()
        return true
    }

    /// This device read the turn's whole reply: it is complete, whatever a
    /// later poll that raced the end event says.
    private func recordLocalCompletion(_ chatTurnId: String) {
        guard let sessionId = currentSessionId else { return }
        recordTurnState(ChatTurnState(
            chatTurnId: chatTurnId,
            sessionId: sessionId,
            status: .completed,
            startedAt: turnStates[chatTurnId]?.startedAt ?? 0,
            endedAt: Date().timeIntervalSince1970 * 1000))
    }

    private func recomputeRunningServerTurn() {
        let running = turnStates.values
            .filter { $0.isRunning && !isRealtimeVoiceTurn($0.chatTurnId) && $0.sessionId == currentSessionId }
            .max { $0.startedAt < $1.startedAt }?
            .chatTurnId
        if runningServerTurnId != running { runningServerTurnId = running }
        refreshServerRunning()
    }

    /// One state change: a running turn gets its live bubble, a settled one
    /// its reply or status line. Realtime voice turns stay on the call surface.
    /// `force` re-applies a state already recorded (a freshly built transcript).
    private func applyTurnState(_ state: ChatTurnState, force: Bool = false) {
        let previous = turnStates[state.chatTurnId]
        guard recordTurnState(state), !isRealtimeVoiceTurn(state.chatTurnId),
              force || previous != state else { return }
        if state.isRunning {
            ensureLiveTurnBubble(state.chatTurnId)
        } else {
            settleTurnBubble(state)
        }
    }

    /// A session or queue response's `turn_state`. The server names its
    /// latest turn (or none, after a restart), so any other turn still marked
    /// running here is not running. A transcript load with no turn state
    /// shows a trailing unanswered question as interrupted.
    private func applyTurnStateSnapshot(
        _ snapshot: ChatTurnStateSnapshot,
        sessionId: String,
        fromTranscriptLoad: Bool
    ) {
        guard sessionId == currentSessionId, snapshot != .unsupported else { return }
        let latest = snapshot.state
        let orphaned = turnStates.values.filter { $0.isRunning && $0.chatTurnId != latest?.chatTurnId }
        // This device's own stream settles its turn itself.
        for state in orphaned where !turnOwnedByLocalStream(state.chatTurnId) {
            turnStates.removeValue(forKey: state.chatTurnId)
            guard !isRealtimeVoiceTurn(state.chatTurnId) else { continue }
            if latest == nil {
                // The server no longer knows the turn: it restarted mid-turn.
                settleTurnBubble(Self.interruptedState(state.chatTurnId, sessionId: sessionId))
            } else {
                // A later turn started: this one ended while we were not
                // listening; settle as completed, which re-reads a missed reply.
                settleTurnBubble(ChatTurnState(
                    chatTurnId: state.chatTurnId, sessionId: sessionId,
                    status: .completed, startedAt: state.startedAt))
            }
        }
        recomputeRunningServerTurn()
        if let latest {
            applyTurnState(latest, force: fromTranscriptLoad)
        } else if fromTranscriptLoad,
                  let turnId = Self.unansweredTrailingUserTurnId(messages),
                  !isRealtimeVoiceTurn(turnId) {
            // Not recorded: if this load raced the turn's start, its own
            // `running` event still turns the line back into a live bubble.
            settleTurnBubble(Self.interruptedState(turnId, sessionId: sessionId))
        }
    }

    private func isRealtimeVoiceTurn(_ turnId: String) -> Bool {
        ChatTurnState.isRealtimeVoiceTurn(turnId) || realtimeVoiceTurnIds.contains(turnId)
    }

    /// Note the turns of realtime voice rows; a bubble one of them got from
    /// its running state (before its row arrived) leaves the transcript.
    private func noteRealtimeVoiceTurns(_ rows: [ChatMessageRawData]) {
        let turns = rows.compactMap { raw -> String? in
            guard raw.presenceSessionId?.isEmpty == false else { return nil }
            return Self.nonEmptyTurnId(raw.chatTurnId)
        }
        guard !turns.isEmpty else { return }
        realtimeVoiceTurnIds.formUnion(turns)
        messages.removeAll { message in
            turns.contains { Self.isTurnPlaceholder(message, turnId: $0) }
                && message.text.isEmpty && message.structuredResponse == nil
        }
        recomputeRunningServerTurn()
    }

    private static func interruptedState(_ turnId: String, sessionId: String) -> ChatTurnState {
        ChatTurnState(
            chatTurnId: turnId, sessionId: sessionId, status: .failed,
            startedAt: 0, reason: ChatTurnState.interruptedReason)
    }

    /// The last message is a user turn with no assistant row of its own:
    /// after a restart (`turn_state: null`) that turn was interrupted.
    static func unansweredTrailingUserTurnId(_ messages: [ChatMessage]) -> String? {
        guard let last = messages.last, last.isUser,
              let turnId = nonEmptyTurnId(last.chatTurnId),
              !ChatTurnState.isRealtimeVoiceTurn(turnId),
              !messages.contains(where: { !$0.isUser && $0.chatTurnId == turnId })
        else { return nil }
        return turnId
    }

    /// The assistant bubble that holds a turn's place until its canonical
    /// reply replaces it (`reconcileCanonicalText`): the SSE placeholder, or
    /// one made from the turn's server state.
    static func isTurnPlaceholder(_ message: ChatMessage, turnId: String) -> Bool {
        !message.isUser && message.id.hasPrefix("streaming-") && message.chatTurnId == turnId
    }

    static func turnPlaceholderId(_ turnId: String) -> String { "streaming-turn-\(turnId)" }

    /// This device is still sending, or reading the reply stream of, the turn.
    private func turnOwnedByLocalStream(_ turnId: String) -> Bool {
        if activeSendingChatTurnId == turnId { return true }
        guard let streamId = streamingMessageId else { return false }
        return messages.first(where: { $0.id == streamId })?.chatTurnId == turnId
    }

    /// A running turn shows as a live assistant bubble with its steps from
    /// the start: keep this device's own placeholder, or make one (a stream
    /// drop, a relaunch, a turn queued or sent from another surface).
    private func ensureLiveTurnBubble(_ turnId: String) {
        if let index = messages.lastIndex(where: { Self.isTurnPlaceholder($0, turnId: turnId) }) {
            if messages[index].turnStatusNotice != nil {
                messages[index].turnStatusNotice = nil
            }
            return
        }
        var bubble = ChatMessage(id: Self.turnPlaceholderId(turnId), isUser: false, text: "", type: .text)
        bubble.chatTurnId = turnId
        bubble.createdAt = Date()
        bubble.activityRows = activityRowsByTurn[turnId] ?? []
        bubble.activityIsLive = true
        var next = messages
        next.append(bubble)
        // One activity disclosure per turn: the live bubble owns it now.
        next = Self.retainLatestActivityAnchorPerTurn(next)
        messages = Self.placeTaskCardsAfterTurnReply(next)
    }

    /// The stream dropped while the server still runs the turn.
    private func markTurnBubbleLive(_ turnId: String) {
        if let bubble = messages.last(where: { Self.isTurnPlaceholder($0, turnId: turnId) }),
           !bubble.text.isEmpty {
            // Partial streamed text; the canonical reply replaces it.
            partialReplyTurns.insert(turnId)
        }
        ensureLiveTurnBubble(turnId)
    }

    /// A settled turn: its reply (already in place), the watchdog's notice,
    /// or a status line ("Stopped", failed, interrupted).
    private func settleTurnBubble(_ state: ChatTurnState) {
        let turnId = state.chatTurnId
        guard !state.isRunning, !isRealtimeVoiceTurn(turnId) else { return }
        let placeholderIndex = messages.lastIndex { Self.isTurnPlaceholder($0, turnId: turnId) }
        // A stream still reading this turn settles it itself (`finishStreaming`).
        if let placeholderIndex, messages[placeholderIndex].id == streamingMessageId { return }
        defer { partialReplyTurns.remove(turnId) }
        if let stashed = suppressedCanonicalByTurn.removeValue(forKey: turnId) {
            adoptCanonicalReply(stashed)
            return
        }
        switch state.status {
        case .running:
            return
        case .completed:
            guard let placeholderIndex else { return }
            let placeholder = messages[placeholderIndex]
            if (placeholder.text.isEmpty && placeholder.structuredResponse == nil)
                || partialReplyTurns.contains(turnId) {
                // The reply never reached this device (missed realtime
                // frames): re-read the transcript, which has it.
                messages[placeholderIndex].activityIsLive = false
                scheduleTranscriptReload(for: turnId)
            }
            // Otherwise the streamed text is the reply.
        case .stopped where state.isWatchdogStop:
            // The watchdog's persisted notice replaces the placeholder when
            // it arrives; one still here means it was missed.
            guard let placeholderIndex else { return }
            messages[placeholderIndex].turnStatusNotice = ChatTurnState.stoppedNotice
            messages[placeholderIndex].activityIsLive = false
            scheduleTranscriptReload(for: turnId)
        case .stopped, .failed:
            guard let notice = state.notice else { return }
            if let placeholderIndex {
                messages[placeholderIndex].turnStatusNotice = notice
                messages[placeholderIndex].activityIsLive = false
            } else {
                insertTurnStatusLine(notice, turnId: turnId)
            }
        }
    }

    /// A settled turn with no bubble here gets its status line under its
    /// question, unless it already has an assistant row of its own.
    private func insertTurnStatusLine(_ notice: String, turnId: String) {
        guard messages.contains(where: { $0.isUser && $0.chatTurnId == turnId }),
              !messages.contains(where: {
                  !$0.isUser && $0.chatTurnId == turnId && Self.canOwnActivityTimeline($0)
              }),
              let anchor = messages.lastIndex(where: { $0.chatTurnId == turnId }) else { return }
        var line = ChatMessage(id: Self.turnPlaceholderId(turnId), isUser: false, text: "", type: .text)
        line.chatTurnId = turnId
        line.createdAt = messages[anchor].createdAt
        line.activityRows = activityRowsByTurn[turnId] ?? []
        line.turnStatusNotice = notice
        var next = messages
        next.insert(line, at: anchor + 1)
        messages = Self.placeTaskCardsAfterTurnReply(Self.retainLatestActivityAnchorPerTurn(next))
    }

    /// Fold one canonical assistant row into the transcript (replacing the
    /// turn's placeholder) with the turn's activity. False when present.
    @discardableResult
    private func adoptCanonicalReply(_ incoming: ChatMessage) -> Bool {
        var msg = incoming
        msg.activityRows = msg.chatTurnId.flatMap { activityRowsByTurn[$0] } ?? activity.snapshot()
        if let turnId = msg.chatTurnId {
            msg.activityIsLive = isActivityTurnLive(turnId)
        }
        var transcript = messages
        guard Self.reconcileCanonicalText(msg, into: &transcript) else { return false }
        messages = transcript
        if let turnId = msg.chatTurnId {
            partialReplyTurns.remove(turnId)
            refreshActivityRows(
                sessionId: currentSessionId,
                messageId: msg.id,
                chatTurnId: turnId,
                force: true
            )
        }
        if msg.chatTurnId?.hasPrefix("voice-request-") != true
            && msg.chatTurnId?.hasPrefix("voice-task-result-") != true {
            speakReplyIfEnabled(msg.text, messageId: msg.id)
        }
        return true
    }

    /// Re-read the transcript once (shortly, so a late reply can land
    /// first) when a settled turn's reply or notice is missing here.
    private func scheduleTranscriptReload(for turnId: String) {
        guard let sessionId = currentSessionId else { return }
        transcriptReloadTurns.insert(turnId)
        guard !transcriptReloadScheduled else { return }
        transcriptReloadScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self else { return }
            self.transcriptReloadScheduled = false
            let turns = self.transcriptReloadTurns
            self.transcriptReloadTurns = []
            // Only while a placeholder still waits for what the reload brings.
            guard self.currentSessionId == sessionId,
                  self.streamingMessageId == nil,
                  self.activeSendingChatTurnId == nil,
                  turns.contains(where: { turnId in
                      self.messages.contains { Self.isTurnPlaceholder($0, turnId: turnId) }
                  }) else { return }
            self.loadSession(sessionId)
        }
    }

    // MARK: - Canonical per-turn activity

    /// Lazily hydrate a historical response when its bubble enters the viewport.
    /// The backend projection is identical to the one used by web chat.
    func loadActivityIfNeeded(messageId: String, chatTurnId: String) {
        if activityStreamTurnId == chatTurnId {
            publishActivityRows(
                activityRowsByTurn[chatTurnId] ?? [],
                chatTurnId: chatTurnId,
                messageId: messageId
            )
            return
        }
        refreshActivityRows(
            sessionId: currentSessionId,
            messageId: messageId,
            chatTurnId: chatTurnId,
            force: false
        )
    }

    /// Follow one turn's live steps on the realtime hub. The REST turn log
    /// backfills once on start and again on every hub resync; live rows are
    /// the hub events that belong to the turn — its `chat_turn_id`, or (as the
    /// server's turn sink maps them) the task / execution of a task card this
    /// turn started.
    private func startActivityStream(sessionId: String, chatTurnId: String) {
        if activityStreamTurnId == chatTurnId, turnActivitySubscription != nil { return }
        stopActivityStream()
        activityStreamTurnId = chatTurnId
        if activityEventsByTurn[chatTurnId] == nil { activityEventsByTurn[chatTurnId] = [] }
        if activityRowsByTurn[chatTurnId] == nil { activityRowsByTurn[chatTurnId] = [] }

        turnActivitySubscription = RealtimeHub.shared.subscribe(
            where: { [weak self] event in
                self?.liveTurnOwns(event, chatTurnId: chatTurnId) ?? false
            },
            onEvent: { [weak self] event in
                guard let self, self.currentSessionId == sessionId,
                      self.activityStreamTurnId == chatTurnId else { return }
                self.ingestCanonicalActivity(event.object, chatTurnId: chatTurnId)
            },
            onResync: { [weak self] in
                guard let self, self.currentSessionId == sessionId,
                      self.activityStreamTurnId == chatTurnId else { return }
                self.refreshLiveTurnActivity(sessionId: sessionId, chatTurnId: chatTurnId)
            }
        )
        refreshLiveTurnActivity(sessionId: sessionId, chatTurnId: chatTurnId)
    }

    private func stopActivityStream() {
        turnActivitySubscription?.cancel()
        turnActivitySubscription = nil
        activityStreamTurnId = nil
    }

    /// Backfill the followed turn from its REST event log onto its anchor.
    private func refreshLiveTurnActivity(sessionId: String, chatTurnId: String) {
        let anchorId = messages.last {
            !$0.isUser && $0.chatTurnId == chatTurnId && Self.canOwnActivityTimeline($0)
        }?.id
        refreshActivityRows(
            sessionId: sessionId,
            messageId: anchorId ?? "",
            chatTurnId: chatTurnId,
            force: true
        )
    }

    /// Whether a hub event belongs to the followed turn: its own
    /// `chat_turn_id`, else the task or execution of a task card that carries
    /// this turn id.
    func liveTurnOwns(_ event: RealtimeHubEvent, chatTurnId: String) -> Bool {
        if let turn = event.refs.chatTurnId { return turn == chatTurnId }
        guard event.refs.taskId != nil || event.refs.executionId != nil else { return false }
        var taskIds: Set<String> = []
        var executionIds: Set<String> = []
        for message in messages where message.chatTurnId == chatTurnId {
            guard case .taskStatus(let task) = message.type else { continue }
            taskIds.insert(task.taskId)
            if let id = task.executionId { executionIds.insert(id) }
            if let id = task.activeRootExecutionId { executionIds.insert(id) }
        }
        if let taskId = event.refs.taskId, taskIds.contains(taskId) { return true }
        if let executionId = event.refs.executionId, executionIds.contains(executionId) { return true }
        return false
    }

    private func ingestCanonicalActivity(_ event: [String: Any], chatTurnId: String) {
        let normalized = Self.normalizedActivityEvents(
            (activityEventsByTurn[chatTurnId] ?? []) + [event]
        )
        activityEventsByTurn[chatTurnId] = normalized
        let accumulator = Self.accumulator(from: normalized)
        publishActivityRows(accumulator.snapshot(), chatTurnId: chatTurnId, messageId: nil)
    }

    private func refreshActivityRows(
        sessionId: String?,
        messageId: String,
        chatTurnId: String,
        force: Bool
    ) {
        guard let sessionId, !sessionId.isEmpty else { return }
        if !force, let cached = activityRowsByTurn[chatTurnId] {
            publishActivityRows(cached, chatTurnId: chatTurnId, messageId: messageId)
            if hydratedActivityTurns.contains(chatTurnId) { return }
        }
        if activityLoadsInFlight.contains(chatTurnId) {
            if force {
                forcedActivityRefreshPending[chatTurnId] = (sessionId, messageId)
            }
            return
        }
        guard let url = turnActivityURL(sessionId: sessionId, chatTurnId: chatTurnId) else { return }
        activityLoadsInFlight.insert(chatTurnId)
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let fetchedEvents = data.flatMap(Self.activityEventsFromResponse)
            DispatchQueue.main.async {
                guard let self else { return }
                self.activityLoadsInFlight.remove(chatTurnId)
                defer {
                    if let pending = self.forcedActivityRefreshPending.removeValue(
                        forKey: chatTurnId
                    ) {
                        self.refreshActivityRows(
                            sessionId: pending.sessionId,
                            messageId: pending.messageId,
                            chatTurnId: chatTurnId,
                            force: true
                        )
                    }
                }
                guard self.currentSessionId == sessionId,
                      error == nil,
                      (200..<300).contains(status),
                      let fetchedEvents else { return }
                let events = Self.normalizedActivityEvents(
                    (self.activityEventsByTurn[chatTurnId] ?? []) + fetchedEvents
                )
                let accumulator = Self.accumulator(from: events)
                let rows = accumulator.snapshot()
                self.hydratedActivityTurns.insert(chatTurnId)
                self.activityEventsByTurn[chatTurnId] = events
                self.publishActivityRows(rows, chatTurnId: chatTurnId, messageId: messageId)
            }
        }.resume()
    }

    private func publishActivityRows(
        _ rows: [ActivityRow],
        chatTurnId: String,
        messageId: String?
    ) {
        let settled = settleActivityRows(rows, terminalTaskStatuses: Self.terminalTaskStatuses(messages))
        activityRowsByTurn[chatTurnId] = settled
        let targetIndex = messageId.flatMap { id in messages.firstIndex { $0.id == id } }
            ?? messages.lastIndex {
                !$0.isUser && $0.chatTurnId == chatTurnId && Self.canOwnActivityTimeline($0)
            }
        guard let targetIndex else { return }
        messages[targetIndex].activityRows = settled
        messages[targetIndex].activityIsLive = isActivityTurnLive(chatTurnId, rows: settled)
    }

    /// Task id -> lowercased status, for each task whose latest transcript
    /// card is terminal. Feeds the activity-row safety net.
    static func terminalTaskStatuses(_ messages: [ChatMessage]) -> [String: String] {
        var latest: [String: TaskStatusModel] = [:]
        for message in messages {
            if case .taskStatus(let task) = message.type { latest[task.taskId] = task }
        }
        var terminal: [String: String] = [:]
        for (taskId, task) in latest where task.isTerminal && !task.synthesisPending {
            terminal[taskId] = task.normalizedStatus
        }
        return terminal
    }

    private static func canOwnActivityTimeline(_ message: ChatMessage) -> Bool {
        if case .text = message.type { return true }
        return false
    }

    /// Same safety contract as web: only an explicit turn id on this exact
    /// task may keep activity live. A later status row may inherit that task's
    /// prior explicit id, but no turn is guessed from another task or from the
    /// newest user message.
    static func latestActiveTaskTurnId(_ messages: [ChatMessage]) -> String? {
        var latestByTask: [String: (index: Int, message: ChatMessage)] = [:]
        var explicitTurnByTask: [String: String] = [:]
        for (index, message) in messages.enumerated() {
            guard case .taskStatus(let task) = message.type else { continue }
            if let turnId = message.chatTurnId?.trimmingCharacters(in: .whitespacesAndNewlines),
               !turnId.isEmpty {
                explicitTurnByTask[task.taskId] = turnId
            }
            latestByTask[task.taskId] = (index, message)
        }

        return latestByTask.values
            .filter { value in
                guard case .taskStatus(let task) = value.message.type else { return false }
                return !task.isTerminal
            }
            .sorted { $0.index > $1.index }
            .compactMap { value -> String? in
                guard case .taskStatus(let task) = value.message.type else { return nil }
                return (value.message.chatTurnId?.trimmingCharacters(in: .whitespacesAndNewlines))
                    .flatMap { $0.isEmpty ? nil : $0 }
                    ?? explicitTurnByTask[task.taskId]
            }
            .first
    }

    private func isActivityTurnLive(_ chatTurnId: String, rows: [ActivityRow]? = nil) -> Bool {
        if activeSendingChatTurnId == chatTurnId { return true }
        if runningServerTurnId == chatTurnId { return true }
        guard Self.latestActiveTaskTurnId(messages) == chatTurnId else { return false }
        let taskRows = (rows ?? activityRowsByTurn[chatTurnId] ?? [])
            .filter { $0.key.hasPrefix("task::") }
        return taskRows.isEmpty || taskRows.contains { $0.status == .running || $0.status == .waiting }
    }

    private func synchronizeActivityLivenessAndStream() {
        let desiredTurnId = activeSendingChatTurnId ?? runningServerTurnId
            ?? Self.latestActiveTaskTurnId(messages)
        for index in messages.indices where !messages[index].isUser
            && Self.canOwnActivityTimeline(messages[index]) {
            guard let turnId = messages[index].chatTurnId else {
                messages[index].activityIsLive = false
                continue
            }
            messages[index].activityIsLive = turnId == desiredTurnId
                && isActivityTurnLive(turnId)
        }

        guard let desiredTurnId,
              let sessionId = currentSessionId,
              !sessionId.isEmpty else {
            stopActivityStream()
            return
        }
        startActivityStream(sessionId: sessionId, chatTurnId: desiredTurnId)
    }

    /// The turn's REST event log (backfill). Live rows ride the realtime hub;
    /// the turn's NDJSON `/events/stream` is no longer held open.
    private func turnActivityURL(sessionId: String, chatTurnId: String) -> URL? {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let session = sessionId.addingPercentEncoding(withAllowedCharacters: allowed),
              let turn = chatTurnId.addingPercentEncoding(withAllowedCharacters: allowed)
        else { return nil }
        return URL(
            string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(session)/turns/\(turn)/events"
        )
    }

    static func activityRowsFromResponse(_ data: Data) -> [ActivityRow]? {
        guard let events = activityEventsFromResponse(data) else { return nil }
        return accumulator(from: events).snapshot()
    }

    private static func activityEventsFromResponse(_ data: Data) -> [[String: Any]]? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let events = object["events"] as? [[String: Any]] else { return nil }
        return normalizedActivityEvents(events)
    }

    /// Mirror web's durable-event normalization. Chat fan-out can persist a
    /// logical event twice and file order is not guaranteed to be chronological;
    /// lifecycle completion must be applied after its corresponding start.
    static func normalizedActivityEvents(_ events: [[String: Any]]) -> [[String: Any]] {
        var seen: Set<String> = []
        var unique: [(index: Int, timestamp: Double, event: [String: Any])] = []
        for (index, event) in events.enumerated() {
            if let key = activityEventDedupeKey(event), !seen.insert(key).inserted {
                continue
            }
            unique.append((index, activityEventTimestampMs(event) ?? 0, event))
        }
        // Equal timestamps: a lifecycle start sorts before its completion
        // (AgenticActionStarted before AgenticActionExecuted, LLMRequestSent
        // before LLMRouted before LLMResponseReceived, tool.call.started
        // before tool.call.finished), then arrival order.
        return unique.sorted {
            if $0.timestamp != $1.timestamp { return $0.timestamp < $1.timestamp }
            let lhs = activityLifecycleRank($0.event), rhs = activityLifecycleRank($1.event)
            return lhs != rhs ? lhs < rhs : $0.index < $1.index
        }.map(\.event)
    }

    /// 0 for a lifecycle start, 1 for an LLM call's route, 2 for everything else.
    private static func activityLifecycleRank(_ event: [String: Any]) -> Int {
        let outer = event["event_type"] as? String ?? ""
        if outer == "AgenticActionStarted" || outer == "LLMRequestSent" { return 0 }
        if outer == "LLMRouted" { return 1 }
        let inner = (event["data"] as? [String: Any])?["event"] as? [String: Any]
        let type = inner?["event_type"] as? String ?? outer
        return type.hasSuffix(".started") ? 0 : 2
    }

    private static func activityEventDedupeKey(_ event: [String: Any]) -> String? {
        let data = event["data"] as? [String: Any]
        let inner = data?["event"] as? [String: Any]
        let payload = inner?["payload"] as? [String: Any]
        if let id = payload?["event_id"] as? String, !id.isEmpty { return id }
        if let message = data?["message"] as? [String: Any],
           let id = message["id"] as? String, !id.isEmpty { return id }
        if let id = data?["event_id"] as? String, !id.isEmpty { return id }

        let type = (inner?["event_type"] as? String) ?? (event["event_type"] as? String) ?? ""
        guard !type.isEmpty else { return nil }
        let agent = (inner?["agent_id"] as? String) ?? (data?["agent_id"] as? String) ?? ""
        let execution = scalarString(data?["execution_id"])
        let discriminator = scalarString(
            data?["iteration"] ?? data?["step_index"] ?? data?["call_id"]
        )
        return "\(agent)|\(type)|\(execution)|\(discriminator)|\(activityEventTimestampMs(event) ?? 0)"
    }

    private static func activityEventTimestampMs(_ event: [String: Any]) -> Double? {
        let data = event["data"] as? [String: Any]
        let inner = data?["event"] as? [String: Any]
        let payload = inner?["payload"] as? [String: Any]
        let candidates = [
            payload?["timestamp_ms"], inner?["timestamp"], inner?["timestamp_ms"],
            data?["timestamp_ms"], data?["timestamp"], event["timestamp_ms"]
        ]
        for candidate in candidates {
            if let number = candidate as? NSNumber { return number.doubleValue }
        }
        return nil
    }

    private static func scalarString(_ value: Any?) -> String {
        if let value = value as? String { return value }
        if let value = value as? NSNumber { return value.stringValue }
        return ""
    }

    private static func accumulator(from events: [[String: Any]]) -> ChatTurnActivityAccumulator {
        let accumulator = ChatTurnActivityAccumulator()
        normalizedActivityEvents(events).forEach { accumulator.ingest($0) }
        return accumulator
    }

    // MARK: - Profiles
    func fetchChatHarnesses() {
        guard !isUITestLaunch else { return }
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/plane/engines") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, _, _ in
            guard let self, let data,
                  let roster = try? JSONDecoder().decode(ChatHarnessRoster.self, from: data) else { return }
            DispatchQueue.main.async {
                self.chatHarnesses = roster.engines.filter { $0.installed }
                self.runHarnessEngine = roster.current
                self.runHarnessModel = roster.runModel
                self.runModelSelection = roster.runModelSelection
                self.runFollowsChat = roster.followsChat ?? false
                self.serverChatEngine = roster.chatCurrent
                if !self.chatHarnesses.contains(where: { $0.name == self.selectedHarnessEngine }) {
                    self.selectedHarnessEngine = "magician"
                }
                let models = self.chatHarnesses.first(where: { $0.name == self.selectedHarnessEngine })?.availableModels ?? ["default"]
                if !models.contains(self.selectedHarnessModel) { self.selectedHarnessModel = "default" }
            }
        }.resume()
    }

    // MARK: - Slash commands (docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md)

    /// What the `/` picker's option rows read.
    var composerCommandContext: ComposerCommandContext {
        var context = ComposerCommandContext(
            engines: chatHarnesses.map { (name: $0.name, models: $0.availableModels) },
            engineCredentials: Dictionary(chatHarnesses.compactMap { harness in
                harness.credentials.map { (harness.name, $0) } }, uniquingKeysWith: { first, _ in first }),
            deviceEngine: selectedHarnessEngine,
            deviceModel: selectedHarnessModel,
            runEngine: runHarnessEngine,
            runModel: runHarnessModel,
            tasks: messages.compactMap { message in
                guard case .taskStatus(let task) = message.type else { return nil }
                return ComposerCommandTask(taskId: task.taskId, title: task.title, status: task.status)
            },
            budgetDefaults: budgetDefaults,
            budgetDefaultsError: budgetDefaultsError)
        var catalogs = commandCatalogs
        // The composer's own list stands in for its engine until a slash load lands.
        if ["magician", "pi"].contains(selectedHarnessEngine), catalogs[selectedHarnessEngine] == nil, !profiles.isEmpty {
            catalogs[selectedHarnessEngine] = profiles
        }
        context.catalogModels = catalogs.mapValues { $0.compactMap(\.modelChoice).map { ComposerModelOption($0) } }
        context.catalogErrors = commandCatalogErrors
        context.deviceModelSelection = currentModelSelection.map { ComposerModelSelection($0) }
        context.serverChatEngine = serverChatEngine
        context.runFollowsChat = runFollowsChat
        context.runModelSelection = runModelSelection.map { ComposerModelSelection($0) }
        context.loopModels = loopModels?.map { ComposerModelOption($0, operation: true) }
        context.loopSelection = loopSelection.map { ComposerModelSelection($0) }
        context.loopModelsError = loopModelsError
        // `voiceMode` / `voiceModesAvailable`: the composer fills them (it owns availability).
        return context
    }

    /// A command's options opened: refresh what they mark as current.
    func composerCommandOpened(_ id: ComposerCommandId) {
        switch id {
        case .budgetDefault: loadBudgetDefaults()
        case .engine:
            fetchChatHarnesses()
            loadCommandCatalog(selectedHarnessEngine)
        case .runEngine:
            fetchChatHarnesses()
            loadLoopRouting()
            if chatHarnesses.contains(where: { $0.name == "pi" }) { loadCommandCatalog("pi") }
        case .budget, .voiceMode: break
        }
    }

    /// An engine's models opened under `/engine` or `/run-engine`: load them.
    func composerCommandLevelOpened(_ level: ComposerCommandLevel) {
        switch level {
        case .runModels("magician"): loadLoopRouting()
        case .engineModels(let engine), .runModels(let engine): loadCommandCatalog(engine)
        case .command, .budgetTask, .thinking: break
        }
    }

    /// `GET /chat/models?engine=` for the `/` picker (magician, pi). Kept
    /// apart from `profiles`: browsing another engine's models must not change
    /// the chat's engine until a model is picked.
    func loadCommandCatalog(_ engine: String) {
        guard !isUITestLaunch, ["magician", "pi"].contains(engine) else { return }
        commandCatalogErrors[engine] = nil
        commandRequest("GET", "/api/magician/v2/chat/models?engine=\(engine)", body: nil) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let data):
                if let catalog = try? JSONDecoder().decode(ChatModelCatalog.self, from: data) {
                    self.commandCatalogs[engine] = catalog.rows
                } else {
                    self.commandCatalogErrors[engine] = "The model list could not be read."
                }
            case .failure(let message):
                self.commandCatalogErrors[engine] = message.text
            }
        }
    }

    /// `GET /llm/routing`: the model Magician's loop decides with.
    func loadLoopRouting() {
        guard !isUITestLaunch else { return }
        loopModelsError = nil
        commandRequest("GET", "/api/magician/v2/llm/routing", body: nil) { [weak self] result in
            guard let self else { return }
            switch result.flatMap(LoopModelRouting.decode) {
            case .success(let routing):
                self.loopModels = routing.models
                self.loopSelection = routing.selection
            case .failure(let message):
                self.loopModelsError = message.text
            }
        }
    }

    func loadBudgetDefaults() {
        commandRequest("GET", "/api/magician/v2/budgets/defaults", body: nil) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let data):
                if let defaults = BudgetDefaults.decode(data) {
                    self.budgetDefaults = defaults
                    self.budgetDefaultsError = nil
                } else {
                    self.budgetDefaultsError = "The server's budget defaults could not be read."
                }
            case .failure(let message):
                self.budgetDefaultsError = message.text
            }
        }
    }

    /// Run a picked command option. Never sends a chat message.
    func runComposerCommand(_ action: ComposerCommandAction) {
        switch action {
        case .setChatEngine(let engine, let model, let selection, let everywhere):
            let context = composerCommandContext
            guard applyChatChoice(engine: engine, model: model, selection: selection) else { return }
            guard everywhere else {
                postCommandNotice(ComposerCommands.chatEngineConfirmation(
                    engine: engine, model: model, selection: selection, everywhere: false, context: context))
                return
            }
            commandRequest("PUT", "/api/magician/v2/plane/chat-engine",
                           body: ComposerCommands.engineBody(engine: engine, model: model)) { [weak self] result in
                switch result {
                case .success:
                    self?.serverChatEngine = engine
                    self?.postCommandNotice(ComposerCommands.chatEngineConfirmation(
                        engine: engine, model: model, selection: nil, everywhere: true, context: context))
                case .failure(let message):
                    self?.postCommandNotice("Set on this device only: \(message.text)", isError: true)
                }
            }
        case .setRunEngine(let engine, let model, let selection):
            let notice = ComposerCommands.runEngineConfirmation(engine: engine, model: model, selection: selection,
                                                                context: composerCommandContext)
            commandRequest("PUT", "/api/magician/v2/plane/engine",
                           body: ComposerCommands.runEngineBody(engine: engine, model: model, selection: selection)) { [weak self] result in
                guard let self else { return }
                switch result {
                case .success:
                    let followsChat = engine == "chat"
                    self.runFollowsChat = followsChat
                    self.runHarnessEngine = followsChat ? (self.serverChatEngine ?? self.runHarnessEngine) : engine
                    self.runHarnessModel = model
                    self.runModelSelection = selection.map { ChatModelSelection($0) }
                    self.postCommandNotice(notice)
                    self.fetchChatHarnesses()
                case .failure(let message):
                    self.postCommandNotice(message.text, isError: true)
                }
            }
        case .setLoopModel(let selection):
            let context = composerCommandContext
            let notice = ComposerCommands.loopModelConfirmation(selection: selection, context: context)
            let save: () -> Void = { [weak self] in
                guard let self else { return }
                self.commandRequest("PUT", "/api/magician/v2/llm/routing/\(LoopModelRouting.operation)",
                                    body: ComposerCommands.loopModelBody(selection)) { [weak self] result in
                    guard let self else { return }
                    switch result {
                    case .success:
                        self.loopSelection = ChatModelSelection(selection)
                        self.postCommandNotice(notice)
                        self.loadLoopRouting()
                    case .failure(let message):
                        self.postCommandNotice(message.text, isError: true)
                    }
                }
            }
            // The loop model only drives runs Magician thinks: pin it first
            // when agentic flows run on another engine (Settings does the same).
            guard !ComposerCommands.runResolvesToMagician(context) else { save(); return }
            commandRequest("PUT", "/api/magician/v2/plane/engine",
                           body: ComposerCommands.engineBody(engine: "magician", model: "default")) { [weak self] result in
                guard let self else { return }
                switch result {
                case .success:
                    self.runFollowsChat = false
                    self.runHarnessEngine = "magician"
                    self.runHarnessModel = "default"
                    self.fetchChatHarnesses()
                    save()
                case .failure(let message):
                    self.postCommandNotice(message.text, isError: true)
                }
            }
        case .setVoiceMode(let mode):
            guard let launch = SystemVoiceLaunchMode(rawValue: mode.rawValue) else { return }
            // As the composer's voice-mode menu does.
            let audio = AudioSettings.shared
            audio.composerVoiceMode = launch
            if launch != .dictation { audio.liveVoiceEngine = launch == .realtime ? .realtime : .handsFree }
            postCommandNotice(ComposerCommands.voiceModeConfirmation(mode))
        case .grantBudget(let taskId, let title, let grant):
            let path = "/api/magician/v2/tasks/\(taskId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? taskId)/budget"
            commandRequest("POST", path, body: grant.body) { [weak self] result in
                switch result {
                case .success(let data):
                    let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                    let resumed = (object?["resumed"] as? Bool) ?? false
                    self?.postCommandNotice(ComposerCommands.grantConfirmation(grant, title: title, resumed: resumed))
                case .failure(let message):
                    self?.postCommandNotice(message.text, isError: true)
                }
            }
        case .setBudgetDefault(let kind, let cap):
            let body = ComposerCommands.defaultsBody(kind: kind, cap: cap, current: budgetDefaults)
            commandRequest("PUT", "/api/magician/v2/budgets/defaults", body: body) { [weak self] result in
                guard let self else { return }
                switch result {
                case .success(let data):
                    var next = BudgetDefaults.decode(data) ?? self.budgetDefaults ?? BudgetDefaults()
                    if kind == .harness { next.harness = next.harness ?? cap } else { next.native = next.native ?? cap }
                    self.budgetDefaults = next
                    self.postCommandNotice(ComposerCommands.defaultConfirmation(kind: kind, cap: cap))
                case .failure(let message):
                    self.postCommandNotice(message.text, isError: true)
                }
            }
        case .back, .open, .openModels, .openThinking, .toggleEngineScope, .pickBudgetTask, .none:
            break
        }
    }

    /// Set the chat's engine and model from `/engine`. A catalog engine's
    /// model goes through `selectedProfile` (the route for the selection, as
    /// the model sheet does), so the composer sends it as `model_selection`.
    private func applyChatChoice(engine: String, model: String, selection: ComposerModelSelection?) -> Bool {
        guard let selection, ["magician", "pi"].contains(engine) else {
            selectedHarnessEngine = engine
            selectedHarnessModel = model
            return true
        }
        let rows = commandCatalogs[engine] ?? (engine == selectedHarnessEngine ? profiles : [])
        let chosen = ChatModelSelection(selection)
        guard let choice = rows.compactMap(\.modelChoice).first(where: { $0.id == chosen.modelId }),
              let route = ComposerModelCatalogControls.route(for: choice, selection: chosen),
              rows.contains(where: { $0.name == route }) else {
            postCommandNotice("That model is not available on this Magician.", isError: true)
            return false
        }
        // An engine switch refetches the catalog and resolves the saved
        // choice: save this one first so the refetch lands on it.
        persistModelPreference(route, selection: chosen)
        selectedHarnessEngine = engine
        selectedHarnessModel = "default"
        profiles = rows
        selectedProfile = route
        return true
    }

    func postCommandNotice(_ text: String, isError: Bool = false) {
        commandNotice = ComposerCommandNotice(text: text, isError: isError)
    }

    struct CommandFailure: Error { let text: String }

    /// One authorized JSON call; the completion runs on the main queue with the
    /// body, or the server's own error message.
    private func commandRequest(_ method: String, _ path: String, body: [String: Any]?,
                                completion: @escaping (Result<Data, CommandFailure>) -> Void) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)\(path)") else {
            completion(.failure(CommandFailure(text: "No Magician host configured yet.")))
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        MagicianAccess.authorize(&request)
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        }
        networkSession.dataTask(with: request) { data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let result: Result<Data, CommandFailure>
            if let error, status == 0 {
                result = .failure(CommandFailure(text: error.localizedDescription))
            } else if (200..<300).contains(status) {
                result = .success(data ?? Data())
            } else {
                result = .failure(CommandFailure(text: ComposerCommands.serverErrorMessage(status: status, body: data)))
            }
            DispatchQueue.main.async { completion(result) }
        }.resume()
    }

    func fetchProfiles() {
        guard !isUITestLaunch else { return }   // UI tests: no real backend fetch (keeps launch idle)
        modelCatalogGeneration &+= 1
        let generation = modelCatalogGeneration
        let context = chatLocalStorageKey
        guard ["magician", "pi"].contains(selectedHarnessEngine) else { return }
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/models?engine=\(selectedHarnessEngine)") else { return }
        modelChoicesLoading = true
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            let decoded = data.flatMap { try? JSONDecoder().decode(ChatModelCatalog.self, from: $0) }
            DispatchQueue.main.async {
                guard let self, generation == self.modelCatalogGeneration, context == self.chatLocalStorageKey else { return }
                self.modelChoicesLoading = false
                guard (response as? HTTPURLResponse)?.statusCode == 200, let decoded else {
                    self.modelSelectionIssue = "Model choices could not be loaded. Reconnect and try again."
                    return
                }
                // The old device choice is a read-only migration baseline, never changed by a chat picker.
                let stored = UserDefaults.standard.object(forKey: self.preferenceKey(ChatModelPreference.storageKey))
                    ?? UserDefaults.standard.object(forKey: ChatModelPreference.storageKey)
                let raw = stored as? Data
                let saved = raw.flatMap { try? JSONDecoder().decode(ChatModelSelection.self, from: $0) }
                let hasSaved = stored != nil
                let invalid = hasSaved && (saved == nil || saved?.modelId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true || saved?.thinking.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == true)
                let resolved = ChatModelPreference.resolve(profiles: decoded.rows, legacyID: self.selectedProfile, saved: saved)
                self.profiles = decoded.rows
                // Do not run the selection observer on a malformed preference:
                // its original bytes must survive until an explicit new choice.
                self.selectedProfile = invalid ? ChatModelPreference.unavailableRoute : resolved.route
                self.modelSelectionIssue = invalid ? ChatModelPreference.unavailableMessage : resolved.issue
                if !invalid && resolved.issue == nil { self.persistModelPreference(resolved.route, selection: resolved.selection) }
            }
        }.resume()
    }

    // MARK: - Stop (cancel the in-flight chat run)
    @MainActor
    func cancelRun() {
        SpeechSynthesizer.shared.stop()  // stop any spoken reply too
        guard let sessionId = currentSessionId,
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/run") else {
            isThinking = false
            return
        }
        let executionId = currentRunExecutionId
        let coordinationSource = UUID()
        let coordinator = executionControlCoordinator ?? .shared
        if let executionId,
           !coordinator.begin(.cancel, for: executionId) {
            cancelRunErrorMessage = "Another action is already updating this run. Try Stop again when it finishes."
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { return }
                defer {
                    if let executionId {
                        coordinator.finish(
                            for: executionId,
                            invalidatedBy: coordinationSource
                        )
                    }
                }
                if let error {
                    self.cancelRunErrorMessage = error.localizedDescription
                    return
                }
                if let http = response as? HTTPURLResponse,
                   !(200..<300).contains(http.statusCode) {
                    let object = data.flatMap {
                        try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                    }
                    self.cancelRunErrorMessage = (object?["error"] as? String)
                        ?? (object?["message"] as? String)
                        ?? "Could not stop the run (HTTP \(http.statusCode))."
                    return
                }
                self.isThinking = false
                self.currentRunExecutionId = nil
                self.activeSendingChatTurnId = nil
                self.synchronizeActivityLivenessAndStream()
                // The stopped state arrives as `chat.turn.state_changed`; read
                // it back too in case the realtime feed missed it.
                DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
                    guard let self, self.currentSessionId == sessionId else { return }
                    self.readQueue(sessionId: sessionId) { _ in }
                }
            }
        }.resume()
    }

    // MARK: - HITL (answer an escalation)
    /// `onError` takes the failure in place of the chat-wide alert, for a
    /// surface that shows it inline (the composer ask dock). A landed answer
    /// marks the transcript card resolved and drops the ask from
    /// `PendingHitlTracker`, so it leaves every surface at once.
    func respondToEscalation(
        content: ChatMessageContentData,
        submission: ChatHitlSubmission,
        onError: ((String) -> Void)? = nil,
        completion: @escaping (Bool) -> Void = { _ in }
    ) {
        let report: (String) -> Void = onError ?? { [weak self] in self?.escalationResponseErrorMessage = $0 }
        guard let target = content.hitlTarget else {
            report("This request is missing its canonical response identity.")
            completion(false)
            return
        }
        guard respondingEscalationID == nil else {
            report("Another response is still being sent.")
            completion(false)
            return
        }
        let landed: (Bool) -> Void = { [weak self] ok in
            if ok {
                self?.markEscalationAnswered(content)
                PendingHitlTracker.shared.drop(target.correlationId)
            }
            completion(ok)
        }

        switch submission {
        case .response(let value):
            respondToCanonicalHitl(
                content: content,
                target: target,
                value: value,
                report: report,
                completion: landed
            )
        case .continueExecution:
            continueAgenticExecution(content: content, target: target, report: report, completion: landed)
        }
    }

    /// Flip the answered card to resolved (as an `escalation_resolved` would).
    private func markEscalationAnswered(_ content: ChatMessageContentData) {
        guard let index = messages.lastIndex(where: {
            guard case .escalation(let candidate) = $0.type else { return false }
            return Self.escalationMatches(candidate, content)
        }), case .escalation(var updated) = messages[index].type, updated.resolved != true else { return }
        updated.resolved = true
        messages[index].type = .escalation(updated)
    }

    private func respondToCanonicalHitl(
        content: ChatMessageContentData,
        target: ChatHitlTarget,
        value: ChatHitlResponseValue,
        report: @escaping (String) -> Void,
        completion: @escaping (Bool) -> Void
    ) {
        guard let endpoint = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/hitl") else {
            report("The response URL is invalid.")
            completion(false)
            return
        }
        let responseURL = endpoint
            .appendingPathComponent(target.correlationId)
            .appendingPathComponent("respond")
        let url = responseURL

        respondingEscalationID = target.correlationId
        escalationResponseErrorMessage = nil
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        var payload: [String: Any] = [
            "source": target.source,
            "value": value.wireValue,
            "input_type": target.inputType,
            "correlation_id": target.correlationId,
            "pause_state_id": target.correlationId,
            "channel": "ios"
        ]
        if let taskId = target.taskId { payload["task_id"] = taskId }
        if let executionId = target.executionId { payload["execution_id"] = executionId }
        request.httpBody = try? JSONSerialization.data(withJSONObject: payload)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { completion(false); return }
                self.respondingEscalationID = nil
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                let object = data.flatMap {
                    try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                }
                let reason = (object?["reason"] as? String)
                    ?? (object?["error"] as? String)
                    ?? (object?["message"] as? String)
                let accepted = object?["accepted"] as? Bool
                let resumed = object?["resumed"] as? Bool
                let reaskRequired = object?["status"] as? String == "reask_required"
                if error == nil, (200..<300).contains(status), reaskRequired {
                    guard let reask = Self.canonicalReaskData(
                        from: object,
                        fallbackQuestion: content.question
                    ) else {
                        report("The server requested another answer without returning its response identity.")
                        completion(false)
                        return
                    }
                    self.replaceEscalation(content, with: reask)
                    report(reason
                        .map { "More information is needed: \($0)" }
                        ?? "More information is needed. Please revise your answer.")
                    completion(false)
                    return
                }
                let staleAgenticResponse = (status == 404 || status == 410)
                    && (target.source == "agentic" || target.source == "escalation")
                let alreadyResolved = status == 409 && reason == "already_resolved"
                let succeeded = error == nil
                    && (((200..<300).contains(status) && accepted != false && resumed != false)
                        || staleAgenticResponse
                        || alreadyResolved)
                guard succeeded else {
                    report(error?.localizedDescription
                        ?? reason
                        ?? "Could not send the response (HTTP \(status)).")
                    completion(false)
                    return
                }
                completion(true)
            }
        }.resume()
    }

    /// Parse a validation re-ask without accepting a response that omits the new
    /// pause key. Retrying an old key can strand the live pause while making a
    /// stale card look resolved.
    static func canonicalReaskData(
        from object: [String: Any]?,
        fallbackQuestion: String?
    ) -> ChatHitlReaskData? {
        guard object?["status"] as? String == "reask_required",
              let rawPauseStateId = object?["pause_state_id"] as? String else { return nil }
        let pauseStateId = rawPauseStateId.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !pauseStateId.isEmpty else { return nil }
        let question = ((object?["question"] as? String) ?? fallbackQuestion ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !question.isEmpty else { return nil }
        return ChatHitlReaskData(
            pauseStateId: pauseStateId,
            question: question,
            hint: object?["hint"] as? String,
            previousAnswer: object?["previous_answer"] as? String
        )
    }

    private func replaceEscalation(
        _ original: ChatMessageContentData,
        with reask: ChatHitlReaskData
    ) {
        guard let index = messages.lastIndex(where: {
            guard case .escalation(let candidate) = $0.type else { return false }
            return Self.escalationMatches(candidate, original)
        }), case .escalation(var updated) = messages[index].type else { return }
        updated.applyCanonicalReask(reask)
        messages[index].type = .escalation(updated)
    }

    private func continueAgenticExecution(
        content: ChatMessageContentData,
        target: ChatHitlTarget,
        report: @escaping (String) -> Void,
        completion: @escaping (Bool) -> Void
    ) {
        guard content.escalationType == "max_iterations",
              let executionId = target.executionId,
              !executionId.isEmpty,
              let endpoint = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/executions") else {
            report("This continuation is missing its execution identity.")
            completion(false)
            return
        }
        let continuationURL = endpoint
            .appendingPathComponent(executionId)
            .appendingPathComponent("execution")
            .appendingPathComponent("agentic-continue")
        let url = continuationURL

        respondingEscalationID = target.correlationId
        escalationResponseErrorMessage = nil
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "pause_state_id": target.correlationId
        ])
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { completion(false); return }
                self.respondingEscalationID = nil
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                let object = data.flatMap {
                    try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                }
                let reason = (object?["reason"] as? String)
                    ?? (object?["error"] as? String)
                    ?? (object?["message"] as? String)
                let succeeded = error == nil && ((200..<300).contains(status) || status == 404)
                guard succeeded else {
                    report(error?.localizedDescription
                        ?? reason
                        ?? "Could not continue the run (HTTP \(status)).")
                    completion(false)
                    return
                }
                completion(true)
            }
        }.resume()
    }

    // MARK: - Structured-response actions
    func invokeStructuredResponseAction(
        sessionId: String,
        actionRef: String,
        completion: @escaping (Bool) -> Void = { _ in }
    ) {
        guard !sessionId.isEmpty,
              !actionRef.isEmpty,
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/actions/invoke") else {
            completion(false)
            return
        }

        structuredActionErrorMessage = nil
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["action_ref": actionRef])

        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { completion(false); return }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard error == nil, (200..<300).contains(status) else {
                    let object = data.flatMap {
                        try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                    }
                    self.structuredActionErrorMessage = (object?["error"] as? String)
                        ?? (object?["message"] as? String)
                        ?? error?.localizedDescription
                        ?? "Could not invoke action (HTTP \(status))."
                    completion(false)
                    return
                }

                completion(true)
            }
        }.resume()
    }

    private func createSession(completion: @escaping (String?) -> Void) {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/new") else { completion(nil); return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = "{}".data(using: .utf8)
        let context = chatLocalStorageKey
        networkSession.dataTask(with: request) { [weak self] data, _, _ in
            var sessionId: String? = nil
            if let data = data,
               let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let session = obj["session"] as? [String: Any],
               let id = session["id"] as? String {
                sessionId = id
            }
            DispatchQueue.main.async {
                guard let self, context == self.chatLocalStorageKey else { completion(nil); return }
                if let id = sessionId {
                    let target = Self.localStorageKey(origin: MagicianAccess.baseURL.absoluteString,
                        principal: MagicianAccess.principal, workspace: MagicianAccess.workspace, session: id)
                    for suffix in ["engine", "model", ChatModelPreference.storageKey, "draft"] {
                        if let value = UserDefaults.standard.object(forKey: context + "|" + suffix) {
                            UserDefaults.standard.set(value, forKey: target + "|" + suffix)
                        }
                    }
                }
                completion(sessionId)
            }
        }.resume()
    }

    // MARK: - Attachments
    /// Stage a file/image (shows a marker immediately) and upload it in the
    /// background; the id is attached to the next send once the upload lands.
    func uploadAttachment(_ data: Data, filename: String, mime: String) {
        let staged = StagedAttachment(id: UUID(), remoteId: nil, filename: filename, mime: mime,
                                      thumbnail: mime.hasPrefix("image/") ? data : nil, uploading: true)
        stagedAttachments.append(staged)
        let localId = staged.id
        if let sessionId = currentSessionId {
            performUpload(data, filename: filename, mime: mime, sessionId: sessionId, localId: localId)
        } else {
            createSession { [weak self] sid in
                guard let self = self else { return }
                guard let sid = sid else { self.markUpload(localId, failed: true); return }
                self.currentSessionId = sid
                self.sessionSelectionStore.remember(sid)
                self.performUpload(data, filename: filename, mime: mime, sessionId: sid, localId: localId)
            }
        }
    }

    /// Remove a staged attachment before send (the × on its chip).
    func removeStagedAttachment(_ id: UUID) {
        stagedAttachments.removeAll { $0.id == id }
    }

    private func markUpload(_ localId: UUID, remoteId: String? = nil, failed: Bool = false) {
        DispatchQueue.main.async {
            guard let idx = self.stagedAttachments.firstIndex(where: { $0.id == localId }) else { return }
            self.stagedAttachments[idx].uploading = false
            self.stagedAttachments[idx].remoteId = remoteId
            self.stagedAttachments[idx].failed = failed || remoteId == nil
        }
    }

    private func performUpload(_ data: Data, filename: String, mime: String, sessionId: String, localId: UUID) {
        guard let request = ComposerAttachmentUpload.request(data, filename: filename, mime: mime, sessionID: sessionId) else {
            markUpload(localId, failed: true); return
        }
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            let valid = (response as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } ?? false
            self?.markUpload(localId, remoteId: valid ? ComposerAttachmentUpload.attachmentID(data) : nil)
        }.resume()
    }

    // MARK: - Canonical message projection

    /// Convert the persisted/realtime chat wire model into the native renderer
    /// model. Both paths must use this function so reload cannot silently lose a
    /// card that was visible when its websocket event first arrived.
    static func projectedMessage(from raw: ChatMessageRawData) -> ChatMessage {
        let content = raw.content
        let isUser = raw.direction == "user"
        let voiceOrigin = raw.voiceOrigin ?? false
        var message: ChatMessage
        var structuredResponseCandidate: ChatMessagePresentationData?

        switch content.type {
        case "text":
            let canonicalText = content.text ?? ""
            let projectedText = Self.projectedText(
                from: raw.presentation,
                fallback: canonicalText,
                canonical: canonicalText
            )
            structuredResponseCandidate = Self.validStructuredResponse(
                from: raw.presentation,
                canonicalText: canonicalText
            )
            message = ChatMessage(
                id: raw.id,
                isUser: isUser,
                text: projectedText,
                type: .text
            )
            message.planReplyContext = content.planReply?.taskTitle
            if !isUser { message.verificationSourceText = canonicalText }

        case "tool_call_executed", "rich_tool_result":
            let tool = content.toolName ?? "tool"
            var body = "**Action completed: \(tool)**"
            if let extra = content.summary ?? content.text, !extra.isEmpty {
                body += "\n\(extra)"
            }
            var imageURLs: [String] = []
            var richBlocks: [ContentBlock] = []
            for block in content.contentBlocks ?? [] {
                let isImage = block.type == "image"
                    || (block.mimeType?.hasPrefix("image/") ?? false)
                if isImage, let url = block.url {
                    imageURLs.append(url)
                } else if block.type == "url" || block.type == "file" {
                    richBlocks.append(block)
                } else if let text = block.text, !text.isEmpty {
                    body += "\n\n\(text)"
                }
            }
            let summary = content.summary?.trimmingCharacters(in: .whitespacesAndNewlines)
            let fallbackBlockText = content.contentBlocks?.first(where: {
                $0.type == "text" && !($0.text?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
            })?.text
            let canonicalText = (summary?.isEmpty == false ? content.summary : nil)
                ?? fallbackBlockText
                ?? ""
            let projectedText = Self.projectedText(
                from: raw.presentation,
                fallback: body,
                canonical: canonicalText
            )
            if Self.structuredResponseTextIfValid(
                from: raw.presentation,
                canonicalText: canonicalText
            ) != nil {
                structuredResponseCandidate = raw.presentation
                // The structured artifact block is now the sole presentation
                // for files and URLs. Keeping the legacy cards would duplicate
                // each output below the structured card.
                richBlocks.removeAll()
            }
            message = ChatMessage(
                id: raw.id,
                isUser: isUser,
                text: projectedText,
                type: .text,
                imageURLs: imageURLs
            )
            message.richBlocks = richBlocks

        case "task_status_update":
            let taskId = content.taskId?.trimmingCharacters(in: .whitespacesAndNewlines)
            let stableTaskId = taskId?.isEmpty == false ? taskId! : "unknown-task"
            let label = content.displayLabel?.trimmingCharacters(in: .whitespacesAndNewlines)
            let task = TaskStatusModel(
                taskId: stableTaskId,
                title: label?.isEmpty == false ? label! : stableTaskId,
                status: content.status ?? "updated",
                steps: [],
                summary: content.summary,
                executionId: content.executionId,
                uiThreadId: content.uiThreadId,
                outputFiles: content.outputFiles ?? [],
                synthesisPending: content.synthesisPending ?? false,
                completionKind: content.completionKind,
                startedAtMs: raw.createdAt.map(Double.init),
                updatedAtMs: raw.createdAt.map(Double.init)
            )
            message = ChatMessage(id: raw.id, isUser: false, text: "", type: .taskStatus(task))

        case "escalation":
            message = ChatMessage(
                id: raw.id,
                isUser: false,
                text: "",
                type: .escalation(content)
            )

        case "escalation_resolved":
            if content.question != nil, content.options != nil {
                var escalation = content
                escalation.resolved = true
                message = ChatMessage(
                    id: raw.id,
                    isUser: false,
                    text: "",
                    type: .escalation(escalation)
                )
            } else {
                let summary = content.summary ?? content.inactiveReason ?? "Request resolved"
                let resolvedText = Self.projectedText(
                    from: raw.presentation,
                    fallback: summary,
                    canonical: summary
                )
                structuredResponseCandidate = Self.validStructuredResponse(
                    from: raw.presentation,
                    canonicalText: summary
                )
                message = ChatMessage(
                    id: raw.id,
                    isUser: false,
                    text: resolvedText,
                    type: .system(resolvedText)
                )
            }

        case "attachment":
            let size = content.size.map(Self.fileSizeLabel)
            let filename = content.label ?? content.filename ?? "File"
            let canonicalText = content.filename ?? filename
            let attachmentText = Self.projectedText(
                from: raw.presentation,
                fallback: canonicalText,
                canonical: canonicalText
            )
            structuredResponseCandidate = Self.validStructuredResponse(
                from: raw.presentation,
                canonicalText: canonicalText
            )
            message = ChatMessage(
                id: raw.id,
                isUser: isUser,
                text: attachmentText,
                type: .attachment(filename: filename, size: size)
            )

        default:
            let fallback = content.text ?? content.summary
                ?? "Unsupported chat item: \(content.type.replacingOccurrences(of: "_", with: " "))"
            let systemText = Self.projectedText(
                from: raw.presentation,
                fallback: fallback,
                canonical: fallback
            )
            structuredResponseCandidate = Self.validStructuredResponse(
                from: raw.presentation,
                canonicalText: fallback
            )
            message = ChatMessage(
                id: raw.id,
                isUser: isUser,
                text: systemText,
                type: .system(systemText)
            )
        }

        message.voiceOrigin = voiceOrigin
        if let origin = raw.contextOrigin, !isUser, origin.sessionId != raw.sessionId {
            message.originalAnswer = OriginalAnswerLink(origin: origin, turnId: raw.chatTurnId, createdAt: raw.createdAt)
        }
        // Preserve turn identity for canonical user-echo matching and every
        // assistant projection, including task status after a hand-off.
        message.chatTurnId = raw.chatTurnId
        message.createdAt = raw.createdAt.flatMap { ChatDateDividers.date(epoch: Double($0)) }
        if !isUser, let structuredResponseCandidate {
            switch message.type {
            case .text, .system, .attachment:
                message.structuredResponse = structuredResponseCandidate
            default:
                break
            }
        }
        return message
    }

    private static func validStructuredResponse(
        from presentation: ChatMessagePresentationData?,
        canonicalText: String
    ) -> ChatMessagePresentationData? {
        guard structuredResponseTextIfValid(from: presentation, canonicalText: canonicalText) != nil else {
            return nil
        }
        guard let projection = presentation else { return nil }
        return projection
    }

    private static func projectedText(
        from presentation: ChatMessagePresentationData?,
        fallback: String,
        canonical: String? = nil,
    ) -> String {
        guard let plainText = structuredResponseTextIfValid(
            from: presentation,
            canonicalText: canonical ?? fallback
        ) else {
            return fallback
        }
        return plainText
    }

    private static func structuredResponseTextIfValid(
        from presentation: ChatMessagePresentationData?,
        canonicalText: String
    ) -> String? {
        guard let presentation else { return nil }
        guard presentation.schema == structuredResponseSchema else { return nil }
        guard presentation.version == structuredResponseVersion else { return nil }
        guard hasValidStructuredContract(presentation) else { return nil }
        let plainText = presentation.plainText
        guard plainText.utf8.count <= structuredResponseMaxBytes else { return nil }
        let trimmed = plainText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if hasDisallowedControlCharacters(plainText) { return nil }
        guard doStructuredTextMatch(
            canonicalText: canonicalText,
            presentedText: plainText
        ) else {
            return nil
        }
        return plainText
    }

    private static func doStructuredTextMatch(
        canonicalText: String,
        presentedText: String
    ) -> Bool {
        let normalizedCanonical = normalizeStructuredText(canonicalText)
        let normalizedPresented = normalizeStructuredText(presentedText)
        return normalizedCanonical == normalizedPresented
    }

    private static func hasValidStructuredContract(_ presentation: ChatMessagePresentationData) -> Bool {
        let validKinds: Set<String> = [
            "markdown", "text", "callout", "key_values", "table", "list", "artifacts", "sources", "metrics"
        ]
        let validTones: Set<String> = ["neutral", "success", "warning", "danger", "info"]
        let validCalloutTones: Set<String> = ["success", "warning", "danger", "info"]
        let validStyles: Set<String> = ["bullets", "steps", "checks"]
        let validAlignments: Set<String> = ["start", "center", "end"]
        let validTrends: Set<String> = ["up", "down", "flat"]
        let validActionKinds: Set<String> = ["copy_text", "open_url", "open_task", "open_artifact", "send_follow_up"]
        let keyValueKeys: Set<String> = ["label", "value", "hint"]
        let listKeys: Set<String> = ["text", "detail", "checked"]
        let artifactKeys: Set<String> = ["label", "href", "artifact_id", "mime_type", "size"]
        let sourceKeys: Set<String> = ["label", "href"]
        let metricKeys: Set<String> = ["label", "value", "trend", "unit"]

        func validString(_ value: ChatStructuredValue?, maxBytes: Int = structuredResponseValueMaxBytes) -> Bool {
            guard let string = value?.stringValue else { return false }
            return isValidStructuredString(string, maxBytes: maxBytes)
        }

        func optionalValidString(_ value: ChatStructuredValue?, maxBytes: Int = structuredResponseValueMaxBytes) -> Bool {
            value == nil || validString(value, maxBytes: maxBytes)
        }

        func optionalSafeHTTPURL(_ value: ChatStructuredValue?) -> Bool {
            value == nil || isSafeStructuredHttpURL(value?.stringValue)
        }

        func validArtifactSize(_ value: ChatStructuredValue) -> Bool {
            switch value {
            case .integer(let size):
                return size >= 0 && size <= Int(Int32.max)
            case .unsignedInteger(let size):
                return size <= UInt64(Int32.max)
            default:
                return false
            }
        }

        func containsOnly(_ item: ChatStructuredStringMap, _ keys: Set<String>) -> Bool {
            Set(item.values.keys).isSubset(of: keys)
        }

        guard !presentation.blocks.isEmpty, presentation.blocks.count <= 32 else { return false }
        guard let serialized = try? JSONEncoder().encode(presentation),
              serialized.count <= structuredResponsePresentationMaxBytes else { return false }
        if let tone = presentation.tone, !validTones.contains(tone) { return false }
        if let title = presentation.title, !isValidStructuredString(title, maxBytes: 160) { return false }
        if let summary = presentation.summary, !isValidStructuredString(summary) { return false }

        for block in presentation.blocks {
            guard validKinds.contains(block.kind) else { return false }
            if let title = block.title, !isValidStructuredString(title, maxBytes: 160) { return false }
            switch block.kind {
            case "markdown":
                guard isValidStructuredString(block.text, maxBytes: structuredResponseMaxBytes) else { return false }
            case "text":
                guard isValidStructuredString(block.text) else { return false }
            case "callout":
                guard let tone = block.tone, validCalloutTones.contains(tone), isValidStructuredString(block.text, maxBytes: structuredResponseMaxBytes) else { return false }
            case "key_values":
                guard let items = block.items, items.count <= 100 else { return false }
                for item in items {
                    guard containsOnly(item, keyValueKeys),
                          validString(item.values["label"], maxBytes: 160),
                          validString(item.values["value"]),
                          optionalValidString(item.values["hint"]) else { return false }
                }
            case "list":
                guard let items = block.items, items.count <= 100 else { return false }
                if let style = block.style, !validStyles.contains(style) { return false }
                for item in items {
                    guard containsOnly(item, listKeys),
                          validString(item.values["text"]),
                          optionalValidString(item.values["detail"]) else { return false }
                    if let checked = item.values["checked"] {
                        guard case .bool = checked else { return false }
                    }
                }
            case "artifacts":
                guard let items = block.items, items.count <= 100 else { return false }
                for item in items {
                    guard containsOnly(item, artifactKeys),
                          validString(item.values["label"], maxBytes: 160),
                          optionalSafeHTTPURL(item.values["href"]),
                          optionalValidString(item.values["artifact_id"], maxBytes: 160),
                          optionalValidString(item.values["mime_type"], maxBytes: 160) else { return false }
                    if let size = item.values["size"], !validArtifactSize(size) { return false }
                }
            case "sources":
                guard let items = block.items, items.count <= 100 else { return false }
                for item in items {
                    guard containsOnly(item, sourceKeys),
                          validString(item.values["label"], maxBytes: 160),
                          let href = item.values["href"]?.stringValue,
                          isSafeStructuredHttpURL(href) else { return false }
                }
            case "metrics":
                guard let items = block.items, items.count <= 100 else { return false }
                for item in items {
                    guard containsOnly(item, metricKeys),
                          validString(item.values["label"], maxBytes: 160),
                          validString(item.values["value"]),
                          optionalValidString(item.values["unit"]) else { return false }
                    if let trend = item.values["trend"]?.stringValue, !validTrends.contains(trend) { return false }
                    if item.values["trend"] != nil && item.values["trend"]?.stringValue == nil { return false }
                }
            case "table":
                guard let columns = block.columns, !columns.isEmpty, columns.count <= 12,
                      let rows = block.rows, rows.count <= 100 else { return false }
                let keys = columns.map(\.key)
                guard Set(keys).count == keys.count else { return false }
                for column in columns {
                    guard isValidStructuredString(column.key, maxBytes: 160),
                          isValidStructuredString(column.label, maxBytes: 160) else { return false }
                    if let alignment = column.alignment, !validAlignments.contains(alignment) { return false }
                }
                for row in rows {
                    guard Set(row.values.keys) == Set(keys) else { return false }
                    for value in row.values.values where !validString(value) { return false }
                }
            default:
                return false
            }
        }

        if let actions = presentation.actions {
            guard actions.count <= 8 else { return false }
            for action in actions {
                guard validActionKinds.contains(action.kind),
                      isValidStructuredString(action.label, maxBytes: 160) else { return false }
                switch action.kind {
                case "copy_text": guard isValidStructuredString(action.text) else { return false }
                case "open_url": guard isSafeStructuredHttpURL(action.url) else { return false }
                case "open_task": guard isValidStructuredString(action.taskId, maxBytes: 160) else { return false }
                case "open_artifact": guard isValidStructuredString(action.artifactId, maxBytes: 160) else { return false }
                case "send_follow_up": guard isValidStructuredString(action.prompt) else { return false }
                default: return false
                }
            }
        }
        if let modelContext = presentation.modelContext {
            guard isValidStructuredString(modelContext.summary),
                  modelContext.visibleFacts?.allSatisfy({ isValidStructuredString($0) }) ?? true,
                  modelContext.selectedItem.map({ isValidStructuredString($0, maxBytes: 160) }) ?? true,
                  ["model_visible", "local_only"].contains(modelContext.privacy ?? "") else { return false }
        }
        if let meta = presentation.meta {
            let identifiers = [meta.responseId, meta.sourceSurface, meta.taskId, meta.executionId, meta.chatTurnId]
            guard identifiers.allSatisfy({ $0.map { isValidStructuredString($0, maxBytes: 160) } ?? true }) else { return false }
            if let provenance = meta.provenance {
                for item in provenance {
                    guard isValidStructuredString(item.id, maxBytes: 160),
                          item.label.map({ isValidStructuredString($0, maxBytes: 160) }) ?? true,
                          item.ref.map({ isValidStructuredString($0) }) ?? true else { return false }
                }
            }
            if let locators = meta.artifactLocators {
                guard locators.count <= 100 else { return false }
                for locator in locators {
                    guard isValidStructuredString(locator.values["artifact_id"]?.stringValue, maxBytes: 160),
                          isValidStructuredString(locator.values["relative_path"]?.stringValue),
                          locator.values["task_id"] == nil
                            || isValidStructuredString(locator.values["task_id"]?.stringValue, maxBytes: 160)
                    else { return false }
                }
            }
            if let confidence = meta.confidence, (!confidence.isFinite || confidence < 0 || confidence > 1) { return false }
            if let cost = meta.cost {
                guard cost.inputTokens.map({ $0 >= 0 }) ?? true,
                      cost.outputTokens.map({ $0 >= 0 }) ?? true,
                      cost.totalTokens.map({ $0 >= 0 }) ?? true,
                      cost.costUsd.map({ $0.isFinite }) ?? true,
                      cost.model.map({ isValidStructuredString($0, maxBytes: 160) }) ?? true else { return false }
            }
        }
        return true
    }

    private static func isValidStructuredString(_ value: String?, maxBytes: Int = 2 * 1024) -> Bool {
        guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              value.utf8.count <= maxBytes else { return false }
        return !hasDisallowedControlCharacters(value)
    }

    private static func isSafeStructuredHttpURL(_ value: String?) -> Bool {
        guard isValidStructuredString(value), let value,
              let components = URLComponents(string: value),
              let scheme = components.scheme?.lowercased() else { return false }
        return scheme == "http" || scheme == "https"
    }

    private static func normalizeStructuredText(_ value: String) -> String {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    }

    private static func hasDisallowedControlCharacters(_ text: String) -> Bool {
        for scalar in text.unicodeScalars {
            if (0...8).contains(scalar.value)
                || scalar.value == 11
                || scalar.value == 12
                || (14...31).contains(scalar.value)
                || (0x7f...0x9f).contains(scalar.value) {
                return true
            }
        }
        return false
    }

    static func projectedMessages(from rawMessages: [ChatMessageRawData], target: OriginalAnswerLink? = nil) -> [ChatMessage] {
        var built: [ChatMessage] = []
        for raw in rawMessages {
            if raw.content.type == "escalation_resolved",
               let index = built.lastIndex(where: {
                   if case .escalation(let content) = $0.type {
                       return escalationMatches(content, raw.content)
                   }
                   return false
               }), case .escalation(var content) = built[index].type {
                content.resolved = true
                built[index].type = .escalation(content)
                continue
            }
            var projected = projectedMessage(from: raw)
            projected.linkedAnswerTarget = target?.matches(raw) == true
            appendProjectedMessage(projected, to: &built)
        }
        return placeTaskCardsAfterTurnReply(built)
    }

    /// A task card is stamped mid-turn, its turn's assistant reply at turn
    /// end, so server order puts the card above the reply that started it.
    /// Move a card whose `chatTurnId` names an assistant reply below it to
    /// directly after that turn's last reply. Cards without a turn id, or
    /// already below their reply, keep their position; the live streaming
    /// placeholder counts as the turn's reply.
    static func placeTaskCardsAfterTurnReply(_ messages: [ChatMessage]) -> [ChatMessage] {
        var lastReplyIndex: [String: Int] = [:]
        for index in messages.indices {
            guard !messages[index].isUser, canOwnActivityTimeline(messages[index]),
                  let turnId = nonEmptyTurnId(messages[index].chatTurnId) else { continue }
            lastReplyIndex[turnId] = index
        }
        guard !lastReplyIndex.isEmpty else { return messages }
        var ordered: [ChatMessage] = []
        ordered.reserveCapacity(messages.count)
        var deferred: [String: [ChatMessage]] = [:]
        for (index, message) in messages.enumerated() {
            if case .taskStatus = message.type,
               let turnId = nonEmptyTurnId(message.chatTurnId),
               let replyIndex = lastReplyIndex[turnId], replyIndex > index {
                deferred[turnId, default: []].append(message)
                continue
            }
            ordered.append(message)
            if !message.isUser, canOwnActivityTimeline(message),
               let turnId = nonEmptyTurnId(message.chatTurnId),
               lastReplyIndex[turnId] == index,
               let cards = deferred.removeValue(forKey: turnId) {
                ordered.append(contentsOf: cards)
            }
        }
        return ordered
    }

    private static func nonEmptyTurnId(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty else { return nil }
        return trimmed
    }

    private static func appendProjectedMessage(_ message: ChatMessage, to messages: inout [ChatMessage]) {
        guard case .taskStatus(let incoming) = message.type else {
            messages.append(message)
            return
        }
        var merged = message
        if let index = messages.lastIndex(where: {
            guard case .taskStatus(let existing) = $0.type else { return false }
            return existing.taskId == incoming.taskId
        }) {
            if messages[index].linkedAnswerTarget || message.linkedAnswerTarget {
                messages.append(message)
                return
            }
            // Some older/cross-surface producers omitted the correlation on a
            // later status row. Preserve only this exact task's previously
            // explicit provenance; never borrow from another task or turn.
            if merged.chatTurnId?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false {
                merged.chatTurnId = messages[index].chatTurnId
            }
            // One card for the task's whole life: its clock starts at the
            // first update, not the latest one.
            if case .taskStatus(let existing) = messages[index].type,
               case .taskStatus(var next) = merged.type {
                next.startedAtMs = [existing.startedAtMs, next.startedAtMs].compactMap { $0 }.min()
                merged.type = .taskStatus(next)
            }
            messages.remove(at: index)
        }
        messages.append(merged)
    }

    /// Fold a persisted assistant row into an SSE preview of the same turn.
    /// A radio loss can end the local stream after partial text while the
    /// durable server turn continues; keeping both rows would show a truncated
    /// reply beside its canonical replacement.
    @discardableResult
    static func reconcileCanonicalText(
        _ message: ChatMessage,
        into messages: inout [ChatMessage]
    ) -> Bool {
        guard !messages.contains(where: { $0.id == message.id }) else { return false }
        if let turnId = message.chatTurnId,
           let index = messages.firstIndex(where: {
               !$0.isUser && $0.id.hasPrefix("streaming-") && $0.chatTurnId == turnId
           }) {
            messages[index] = message
        } else {
            messages.append(message)
            // A reply that lands after its own turn's task card (no local
            // stream placeholder held its slot) goes above that card.
            messages = placeTaskCardsAfterTurnReply(messages)
        }
        return true
    }

    private func appendProjectedMessage(_ message: ChatMessage) {
        var next = messages
        Self.appendProjectedMessage(message, to: &next)
        messages = next
    }

    private func invalidateExecutionLookup(for taskId: String) {
        activeExecutionLookupCounter &+= 1
        activeExecutionLookupGeneration[taskId] = activeExecutionLookupCounter
    }

    private static func fileSizeLabel(_ bytes: Int) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: Int64(bytes))
    }

    // MARK: - Session Loading
    func loadSession(_ sessionId: String, target: OriginalAnswerLink? = nil) {
        sessionLoadTask?.cancel()
        resetTurnActivityState()
        if currentSessionId != sessionId { messages = [] }
        currentSessionId = sessionId
        resetOlderHistory()
        focusedMessageId = nil
        originalAnswerError = nil
        currentRunExecutionId = nil
        activeExecutionLookupGeneration.removeAll()
        let base = MagicianAccess.baseURL.absoluteString
        guard let url = URL(string: "\(base)/api/magician/v2/chat/sessions/\(sessionId)") else { return }
        sessionLoadTask = Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                var request = URLRequest(url: url)
                MagicianAccess.authorize(&request)
                let (data, response) = try await self.networkSession.data(for: request)
                guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                    throw URLError(.badServerResponse)
                }
                let decoded = try JSONDecoder().decode(ChatSessionDetailResponse.self, from: data)
                let turnSnapshot = ChatTurnStateSnapshot.parse(responseData: data, sessionId: sessionId)
                var rows = decoded.messages
                var hasMore = Self.initialHasMore(serverHasMore: decoded.hasMore, embeddedCount: rows.count)
                var cursors = Set<String>()
                while let target, !rows.contains(where: target.matches), hasMore {
                    try Task.checkCancellation()
                    guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base else { return }
                    guard let before = rows.first?.id, cursors.insert(before).inserted else {
                        throw URLError(.cannotParseResponse)
                    }
                    let page = try await self.fetchOlderMessagePage(
                        sessionURL: url, before: before, limit: Self.sessionEmbeddedPageSize)
                    rows = page.messages + rows
                    hasMore = page.hasMoreAfterThisPage
                }
                try Task.checkCancellation()
                guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base else { return }
                self.oldestLoadedRawMessageId = rows.first?.id
                self.olderHistory = OlderHistoryState(hasMore: hasMore && rows.first != nil)
                self.sessionSelectionStore.remember(sessionId)
                self.currentSessionOrigin = decoded.session.internalVoice
                let built = Self.projectedMessages(from: rows, target: target)
                self.messages = Self.retainLatestActivityAnchorPerTurn(built)
                self.focusedMessageId = self.messages.first(where: { $0.linkedAnswerTarget })?.id
                if target != nil && self.focusedMessageId == nil {
                    self.originalAnswerError = "The original answer is no longer available in this conversation."
                }
                let activeTaskIds = Set(self.messages.compactMap { message -> String? in
                    guard case .taskStatus(let task) = message.type, !task.isTerminal else { return nil }
                    return task.taskId
                })
                activeTaskIds.forEach { self.refreshAuthoritativeExecutionTarget(for: $0) }
                // A turn still running after a stream drop or relaunch shows
                // live with its steps; a settled one shows its status line.
                self.noteRealtimeVoiceTurns(rows)
                self.applyTurnStateSnapshot(turnSnapshot, sessionId: sessionId, fromTranscriptLoad: true)
                self.synchronizeActivityLivenessAndStream()
            } catch {
                guard !Task.isCancelled, self.currentSessionId == sessionId else { return }
                self.originalAnswerError = "Could not open this conversation. Please try again."
            }
        }
    }

    /// One `GET .../chat/sessions/{id}/messages?before=` page.
    struct OlderMessagePage: Decodable {
        let messages: [ChatMessageRawData]
        let hasMore: Bool
        enum CodingKeys: String, CodingKey { case messages, hasMore = "has_more" }
        /// An empty page ends paging even if the server still says more.
        var hasMoreAfterThisPage: Bool { hasMore && !messages.isEmpty }
    }

    /// The newest messages a session detail embeds.
    static let sessionEmbeddedPageSize = 200
    /// One scroll-to-top page, like the web transcript.
    static let olderHistoryPageSize = 50

    /// Whether older messages exist before the embedded page. A server that
    /// predates `has_more` leaves it out: a full page then means "maybe".
    static func initialHasMore(serverHasMore: Bool?, embeddedCount: Int) -> Bool {
        serverHasMore ?? (embeddedCount >= sessionEmbeddedPageSize)
    }

    private func fetchOlderMessagePage(sessionURL: URL, before: String, limit: Int) async throws -> OlderMessagePage {
        guard var components = URLComponents(url: sessionURL.appendingPathComponent("messages"), resolvingAgainstBaseURL: false) else {
            throw URLError(.badURL)
        }
        components.queryItems = [URLQueryItem(name: "limit", value: String(limit)), URLQueryItem(name: "before", value: before)]
        guard let pageURL = components.url else { throw URLError(.badURL) }
        var request = URLRequest(url: pageURL)
        MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
        return try JSONDecoder().decode(OlderMessagePage.self, from: data)
    }

    private func resetOlderHistory() {
        olderHistoryGeneration &+= 1
        olderHistoryTask?.cancel()
        olderHistoryTask = nil
        oldestLoadedRawMessageId = nil
        if olderHistory != OlderHistoryState() { olderHistory = OlderHistoryState() }
        olderHistoryPrepend = nil
    }

    /// The reader reached the top of the transcript: fetch ONE older page and
    /// prepend it. A no-op while a page is in flight, after the last page, or
    /// before the transcript has loaded; a reply for another chat is dropped.
    func loadOlderMessages() {
        guard olderHistory.hasMore, !olderHistory.isLoading,
              let sessionId = currentSessionId,
              let before = oldestLoadedRawMessageId else { return }
        let base = MagicianAccess.baseURL.absoluteString
        guard let url = URL(string: "\(base)/api/magician/v2/chat/sessions/\(sessionId)") else { return }
        let generation = olderHistoryGeneration
        olderHistory.isLoading = true
        olderHistory.failed = false
        olderHistoryTask = Task { @MainActor [weak self] in
            guard let self else { return }
            let isCurrent = {
                generation == self.olderHistoryGeneration && self.currentSessionId == sessionId
                    && MagicianAccess.baseURL.absoluteString == base
            }
            do {
                let page = try await self.fetchOlderMessagePage(
                    sessionURL: url, before: before, limit: Self.olderHistoryPageSize)
                guard !Task.isCancelled, isCurrent() else { return }
                let anchor = self.messages.first?.id
                let older = Self.retainLatestActivityAnchorPerTurn(Self.projectedMessages(from: page.messages))
                self.messages = Self.prependOlderMessages(older, to: self.messages)
                if let first = page.messages.first?.id { self.oldestLoadedRawMessageId = first }
                self.olderHistory = OlderHistoryState(hasMore: page.hasMoreAfterThisPage)
                if let anchor {
                    let token = (self.olderHistoryPrepend?.token ?? 0) &+ 1
                    self.olderHistoryPrepend = OlderHistoryPrepend(token: token, anchorMessageId: anchor)
                }
            } catch {
                guard !Task.isCancelled, isCurrent() else { return }
                self.olderHistory.isLoading = false
                self.olderHistory.failed = true
            }
        }
    }

    /// Put an older page above the loaded transcript. Rows already shown (by
    /// id, or a task whose newer card is shown) are dropped; an older reply
    /// of a turn whose newer reply is shown gives up the activity anchor.
    static func prependOlderMessages(_ older: [ChatMessage], to current: [ChatMessage]) -> [ChatMessage] {
        let shownIds = Set(current.map(\.id))
        var shownTaskIds = Set<String>()
        var anchoredTurns = Set<String>()
        for message in current {
            if case .taskStatus(let task) = message.type { shownTaskIds.insert(task.taskId) }
            if !message.isUser, canOwnActivityTimeline(message), let turnId = nonEmptyTurnId(message.chatTurnId) {
                anchoredTurns.insert(turnId)
            }
        }
        var seen = Set<String>()
        var fresh: [ChatMessage] = []
        for var message in older where !shownIds.contains(message.id) && seen.insert(message.id).inserted {
            if case .taskStatus(let task) = message.type, shownTaskIds.contains(task.taskId) { continue }
            if !message.isUser, canOwnActivityTimeline(message),
               let turnId = nonEmptyTurnId(message.chatTurnId), anchoredTurns.contains(turnId) {
                message.chatTurnId = nil
                message.activityRows = []
                message.activityIsLive = false
            }
            fresh.append(message)
        }
        guard !fresh.isEmpty else { return current }
        return placeTaskCardsAfterTurnReply(fresh + current)
    }

    static func retainLatestActivityAnchorPerTurn(_ messages: [ChatMessage]) -> [ChatMessage] {
        var latestAssistantIndex: [String: Int] = [:]
        for index in messages.indices {
            guard !messages[index].isUser,
                  canOwnActivityTimeline(messages[index]),
                  let turnId = messages[index].chatTurnId,
                  !turnId.isEmpty else { continue }
            latestAssistantIndex[turnId] = index
        }
        var anchored = messages
        for index in anchored.indices {
            guard !anchored[index].isUser, canOwnActivityTimeline(anchored[index]),
                  let turnId = anchored[index].chatTurnId,
                  latestAssistantIndex[turnId] != index else { continue }
            anchored[index].chatTurnId = nil
            anchored[index].activityRows = []
            anchored[index].activityIsLive = false
        }
        return anchored
    }

    /// Internal (not private) so realtime event routing can be unit-tested by
    /// feeding crafted event JSON directly (bypassing the WebSocket).
    func handleIncomingJSON(_ jsonString: String) {
        guard let data = jsonString.data(using: .utf8) else { return }
        do {
            if let jsonObj = try JSONSerialization.jsonObject(with: data, options: []) as? [String: Any] {
                handleIncomingObject(jsonObj)
            }
        } catch {
            debugLog("Failed to parse realtime JSON: \(error)")
        }
    }

    /// Route one decoded realtime frame (the hub hands frames over decoded).
    func handleIncomingObject(_ jsonObj: [String: Any]) {
        do {   // scope only: routing below never throws
            // Turn lifecycle (`chat.turn.state_changed`): drives the live
            // bubble, never a transcript row of its own.
            if let state = ChatTurnState.fromEvent(jsonObj) {
                guard let selected = currentSessionId, state.sessionId == selected else { return }
                applyTurnState(state)
                synchronizeActivityLivenessAndStream()
                return
            }

            if let eventType = jsonObj["event_type"] as? String {

                // The socket is workspace-scoped, while this transcript belongs
                // to one selected conversation. Check both canonical ids before
                // touching messages, activity or the current turn's spinner.
                if eventType == "ChatMessageReceived" {
                    guard let selected = currentSessionId, !selected.isEmpty,
                          let payload = jsonObj["data"] as? [String: Any],
                          payload["session_id"] as? String == selected,
                          let message = payload["message"] as? [String: Any],
                          message["session_id"] as? String == selected else { return }
                } else if eventType == "MessageCompleted" {
                    // Legacy orchestrator summaries have no conversation id.
                    // Persisted ChatMessageReceived events own chat replies.
                    return
                }

                self.activity.ingest(jsonObj)

                if eventType == "ExecutionPanelDelta" {
                    if let eventDataDict = jsonObj["data"] as? [String: Any],
                       let eventDataJson = try? JSONSerialization.data(withJSONObject: eventDataDict) {

                        if let delta = decodeExecutionPanelDelta(from: eventDataJson) {
                            self.updateExecutionPanel(with: delta.state)
                        }
                    }
                } else if eventType == "ShellOutputChunk" {
                    if let eventDataDict = jsonObj["data"] as? [String: Any],
                       let eventDataJson = try? JSONSerialization.data(withJSONObject: eventDataDict),
                       let chunkData = try? JSONDecoder().decode(ShellOutputChunkEventData.self, from: eventDataJson) {

                        // Append to the most recent TaskStatusModel
                        if let index = self.messages.lastIndex(where: {
                            if case .taskStatus(_) = $0.type { return true }
                            return false
                        }) {
                            if case .taskStatus(var s) = self.messages[index].type {
                                s.terminalLines.append(contentsOf: chunkData.lines)
                                self.messages[index].type = .taskStatus(s)
                            }
                        }
                    }
                } else if eventType == "V3PlanningStarted" {
                    if let eventDataDict = jsonObj["data"] as? [String: Any],
                       let eventDataJson = try? JSONSerialization.data(withJSONObject: eventDataDict),
                       let planData = try? JSONDecoder().decode(V3PlanningStartedEventData.self, from: eventDataJson) {

                        let planningStatus = TaskStatusModel(taskId: planData.taskId, title: "Planning: \(planData.taskTitle)", status: "running", steps: ["Architecting solution..."])
                        self.appendProjectedMessage(
                            ChatMessage(
                                id: UUID().uuidString,
                                isUser: false,
                                text: "",
                                type: .taskStatus(planningStatus)
                            )
                        )
                    }
                } else if eventType == "ChatMessageReceived" {
                    if let eventDataDict = jsonObj["data"] as? [String: Any],
                       let eventDataJson = try? JSONSerialization.data(withJSONObject: eventDataDict),
                       let chatMessageData = try? JSONDecoder().decode(ChatMessageReceivedEventData.self, from: eventDataJson) {

                        self.noteRealtimeVoiceTurns([chatMessageData.message])
                        let content = chatMessageData.message.content
                        if content.type == "escalation" {
                            let msg = Self.projectedMessage(from: chatMessageData.message)
                            if !self.messages.contains(where: { $0.id == msg.id }) {
                                self.appendProjectedMessage(msg)
                            }
                        } else if content.type == "escalation_resolved" {
                            self.markEscalationResolved(content)
                        } else if content.type == "task_status_update" {
                            let msg = Self.projectedMessage(from: chatMessageData.message)
                            self.appendProjectedMessage(msg)
                            if case .taskStatus(let task) = msg.type {
                                if task.isTerminal {
                                    self.invalidateExecutionLookup(for: task.taskId)
                                } else {
                                    self.refreshAuthoritativeExecutionTarget(for: task.taskId)
                                }
                                if let statusMessage = self.messages.last(where: {
                                    guard case .taskStatus(let candidate) = $0.type else {
                                        return false
                                    }
                                    return candidate.taskId == task.taskId
                                }), let turnId = statusMessage.chatTurnId,
                                   let anchor = self.messages.last(where: {
                                       !$0.isUser && $0.chatTurnId == turnId
                                           && Self.canOwnActivityTimeline($0)
                                   }) {
                                    // Reconcile from the durable projection as
                                    // the task state changes. This closes the
                                    // tiny race where a terminal chat card can
                                    // arrive just before its final stream row.
                                    // Settle the cached strip now so a failed or
                                    // slow backfill cannot leave it running.
                                    if task.isTerminal, let cached = self.activityRowsByTurn[turnId] {
                                        self.publishActivityRows(cached, chatTurnId: turnId, messageId: anchor.id)
                                    }
                                    self.refreshActivityRows(
                                        sessionId: self.currentSessionId,
                                        messageId: anchor.id,
                                        chatTurnId: turnId,
                                        force: true
                                    )
                                }
                            }
                            self.synchronizeActivityLivenessAndStream()
                        } else if content.type == "text" {
                            let raw = chatMessageData.message
                            let streamedTurn = self.streamingMessageId.flatMap { streamId in
                                self.messages.first { $0.id == streamId }?.chatTurnId
                            }
                            let isOwnStreamEcho = raw.chatTurnId != nil
                                && raw.chatTurnId == streamedTurn

                            if raw.direction == "user" {
                                let msg = Self.projectedMessage(from: raw)
                                if self.messages.contains(where: { $0.id == msg.id }) {
                                    // Replayed canonical event: already present.
                                } else if let turn = msg.chatTurnId, !turn.isEmpty,
                                          let index = self.messages.firstIndex(where: {
                                              $0.isUser && $0.chatTurnId == turn
                                          }) {
                                    // Reconcile this device's optimistic echo by
                                    // turn id; identical text from another device
                                    // is a distinct message and must stay visible.
                                    self.messages[index] = msg
                                } else if let turn = msg.chatTurnId, !turn.isEmpty,
                                          let placeholder = self.messages.firstIndex(where: {
                                              Self.isTurnPlaceholder($0, turnId: turn)
                                          }) {
                                    // The turn's live bubble (from its running
                                    // state) came first; the question goes above.
                                    self.messages.insert(msg, at: placeholder)
                                } else {
                                    self.messages.append(msg)
                                }
                            } else if isOwnStreamEcho {
                                // This device's stream owns the turn; keep the
                                // canonical row in case the stream ends without
                                // a reply of its own (a Stop's or watchdog's
                                // notice, a dropped connection).
                                if let turnId = raw.chatTurnId,
                                   !self.messages.contains(where: { $0.id == raw.id }) {
                                    self.suppressedCanonicalByTurn[turnId] = Self.projectedMessage(from: raw)
                                }
                            } else {
                                if self.activeSendingChatTurnId == nil
                                    || self.activeSendingChatTurnId == raw.chatTurnId {
                                    self.isThinking = false
                                }
                                let msg = Self.projectedMessage(from: raw)
                                if let turnId = msg.chatTurnId,
                                   self.activeSendingChatTurnId == turnId {
                                    self.activeSendingChatTurnId = nil
                                }
                                self.adoptCanonicalReply(msg)
                                self.synchronizeActivityLivenessAndStream()
                            }
                        } else if content.type == "tool_call_executed" || content.type == "rich_tool_result" {
                            self.isThinking = false
                            let msg = Self.projectedMessage(from: chatMessageData.message)
                            if !self.messages.contains(where: { $0.id == msg.id }) {
                                self.appendProjectedMessage(msg)
                            }
                        } else if content.type == "attachment" {
                            let msg = Self.projectedMessage(from: chatMessageData.message)
                            if !self.messages.contains(where: { $0.id == msg.id }) {
                                self.appendProjectedMessage(msg)
                            }
                        }
                    }
                }
            }
        }
    }

    private func updateExecutionPanel(with state: ExecutionPanelState) {
        let taskId = state.overview.taskId
        let title = state.overview.title
        let taskStatusString = state.overview.status

        // Use activityLog if available, otherwise recentActivity
        let activities = state.run.activityLog ?? state.run.recentActivity
        let steps = activities.compactMap { item -> String? in
            if let title = item.title, !title.isEmpty { return title }
            if let content = item.content, !content.isEmpty { return content }
            return nil
        }

        if let index = self.messages.lastIndex(where: {
            if case .taskStatus(let s) = $0.type { return s.taskId == taskId }
            return false
        }) {
            let existing: TaskStatusModel? = {
                if case .taskStatus(let model) = self.messages[index].type { return model }
                return nil
            }()
            let updatedStatus = TaskStatusModel(
                taskId: taskId,
                title: title,
                status: taskStatusString,
                steps: steps,
                terminalLines: existing?.terminalLines ?? [],
                summary: state.run.summary ?? existing?.summary,
                executionId: state.overview.executionId ?? existing?.executionId,
                uiThreadId: existing?.uiThreadId,
                outputFiles: existing?.outputFiles ?? [],
                synthesisPending: existing?.synthesisPending ?? false,
                activeRootExecutionId: existing?.activeRootExecutionId,
                completionKind: existing?.completionKind,
                storeStatus: existing?.storeStatus,
                startedAtMs: existing?.startedAtMs,
                updatedAtMs: existing?.updatedAtMs
            )
            self.messages[index].type = .taskStatus(updatedStatus)
        } else {
            let initialStatus = TaskStatusModel(
                taskId: taskId,
                title: title,
                status: taskStatusString,
                steps: steps,
                summary: state.run.summary,
                executionId: state.overview.executionId
            )
            self.messages.append(ChatMessage(id: UUID().uuidString, isUser: false, text: "", type: .taskStatus(initialStatus)))
        }
        refreshAuthoritativeExecutionTarget(for: taskId)
    }

    private func refreshAuthoritativeExecutionTarget(for taskId: String) {
        activeExecutionLookupCounter &+= 1
        let generation = activeExecutionLookupCounter
        activeExecutionLookupGeneration[taskId] = generation
        let sessionIdAtRequest = currentSessionId
        let pathCharacters = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let encodedTaskId = taskId.addingPercentEncoding(withAllowedCharacters: pathCharacters),
              let url = URL(
                string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3/tasks/\(encodedTaskId)"
              ) else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self, error == nil,
                  let http = response as? HTTPURLResponse,
                  (200..<300).contains(http.statusCode),
                  let data,
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  // `task.state.status` on the current record; `task.status` on an older one.
                  let record = TaskStoreRecord.parse(object) else { return }
            let status = record.status
            let activeRootExecutionId = TaskStatusModel.resolveActiveExecutionId(
                status: status,
                activeRootExecutionId: record.activeRootExecutionId
            )
            let taskSessionId = record.chatSessionId
            DispatchQueue.main.async {
                guard self.activeExecutionLookupGeneration[taskId] == generation else { return }
                if let index = self.messages.firstIndex(where: {
                    if case .taskStatus(let model) = $0.type { return model.taskId == taskId }
                    return false
                }), case .taskStatus(var model) = self.messages[index].type {
                    model = LiveTaskCardRules.reconciled(model, storeStatus: status)
                    model.activeRootExecutionId = activeRootExecutionId
                    self.messages[index].type = .taskStatus(model)
                }
                guard self.currentSessionId == sessionIdAtRequest,
                      taskSessionId == sessionIdAtRequest else { return }
                self.currentRunExecutionId = activeRootExecutionId
            }
        }.resume()
    }
}

/// Shared multipart transport for Chat and VibeDev project-session attachments.
enum ComposerAttachmentUpload {
    static func request(_ data: Data, filename: String, mime: String, sessionID: String) -> URLRequest? {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionID)/attachments") else { return nil }
        let boundary = "Boundary-\(UUID().uuidString)"
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        let name = filename.replacingOccurrences(of: "\"", with: "_").replacingOccurrences(of: "\r", with: "_").replacingOccurrences(of: "\n", with: "_")
        var body = Data("--\(boundary)\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\(name)\"\r\nContent-Type: \(mime)\r\n\r\n".utf8)
        body.append(data)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        request.httpBody = body
        return request
    }

    static func attachmentID(_ data: Data?) -> String? {
        guard let data, let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        if let id = obj["attachment_id"] as? String ?? obj["id"] as? String { return id }
        let attachment = obj["attachment"] as? [String: Any]
        return attachment?["attachment_id"] as? String ?? attachment?["id"] as? String
    }
}
