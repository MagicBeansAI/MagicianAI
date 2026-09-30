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

    var normalizedStatus: String { status.lowercased() }

    var isTerminal: Bool {
        ["complete", "completed", "done", "failed", "error", "cancelled", "canceled"]
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

    static func == (lhs: ChatMessage, rhs: ChatMessage) -> Bool {
        lhs.originalAnswer == rhs.originalAnswer && lhs.linkedAnswerTarget == rhs.linkedAnswerTarget && lhs.id == rhs.id && lhs.text == rhs.text && lhs.imageURLs == rhs.imageURLs
            && lhs.activityRows == rhs.activityRows && lhs.activityIsLive == rhs.activityIsLive
            && lhs.chatTurnId == rhs.chatTurnId
            && lhs.structuredResponse == rhs.structuredResponse
            && lhs.richBlocks == rhs.richBlocks
    }
}

struct ChatProfile: Codable, Identifiable, Equatable {
    let name: String
    let provider: String?
    let model: String?
    let isDefault: Bool?
    let supportsUserImageInputs: Bool?
    let isAdaptive: Bool?
    let adaptiveDescription: String?
    let adaptiveTier: String?
    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, provider, model
        case isDefault = "is_default"
        case supportsUserImageInputs = "supports_user_image_inputs"
        case isAdaptive = "is_adaptive"
        case adaptiveDescription = "adaptive_description"
        case adaptiveTier = "adaptive_tier"
    }
}

private struct ChatProfilesResponse: Codable {
    let profiles: [ChatProfile]
}

struct ChatHarnessOption: Codable, Identifiable, Equatable {
    let name: String
    let installed: Bool
    let models: [String]?
    var id: String { name }
    var availableModels: [String] { models ?? ["default"] }
}

private struct ChatHarnessRoster: Codable {
    let engines: [ChatHarnessOption]
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

class ChatViewModel: NSObject, ObservableObject, URLSessionWebSocketDelegate {
    @Published var messages: [ChatMessage] = [
        ChatMessage(id: UUID().uuidString, isUser: false, text: "I am Sam. How can I help you today?", type: .text)
    ]
    @Published var composerMode: ComposerMode = .ask {
        didSet {
            guard let permission = ComposerDoPermission(mode: composerMode),
                  composerDoPermission != permission else { return }
            composerDoPermission = permission
        }
    }
    /// Do remembers Ask/Accept while Plan is active, matching the Web composer.
    @Published var composerDoPermission: ComposerDoPermission = .ask
    @Published var focusedMessageId: String?
    @Published var originalAnswerError: String?
    private var sessionLoadTask: Task<Void, Never>?
    @Published var isConnected = false
    @Published var isThinking = false
    @Published var cancelRunErrorMessage: String?
    @Published var escalationResponseErrorMessage: String?
    @Published var structuredActionErrorMessage: String?
    @Published private(set) var respondingEscalationID: String?
    @Published private(set) var currentRunExecutionId: String?

    private var webSocket: URLSessionWebSocketTask?
    /// Session the composer posts to. Set when a thread is opened (loadSession);
    /// created on demand if a message is sent before a session exists. Whenever it
    /// changes, refresh the composer @-mention catalog for the new session's agent.
    private var currentSessionId: String? {
        didSet {
            guard currentSessionId != oldValue else { return }
            queuedMessages = []; serverRunning = false; currentSessionOrigin = nil
            sessionLoadTask?.cancel()
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
        didSet { UserDefaults.standard.set(selectedProfile, forKey: "magios.chat.profile") }
    }
    @Published var chatHarnesses: [ChatHarnessOption] = [ChatHarnessOption(name: "magician", installed: true, models: ["default"])]
    @Published var selectedHarnessEngine: String = UserDefaults.standard.string(forKey: "magios.chat.harnessEngine") ?? "magician" {
        didSet {
            UserDefaults.standard.set(selectedHarnessEngine, forKey: "magios.chat.harnessEngine")
            if oldValue != selectedHarnessEngine { selectedHarnessModel = "default" }
        }
    }
    @Published var selectedHarnessModel: String = UserDefaults.standard.string(forKey: "magios.chat.harnessModel") ?? "default" {
        didSet { UserDefaults.standard.set(selectedHarnessModel, forKey: "magios.chat.harnessModel") }
    }
    /// Files staged for the next send — shown as preview chips in the composer.
    @Published var stagedAttachments: [StagedAttachment] = []
    /// Messages queued behind the in-flight turn (surfaced by the QueueInspector).
    @Published var queuedMessages: [QueuedMessage] = []
    @Published private(set) var serverRunning = false
    @Published private(set) var currentSessionOrigin: ConcurrentSessionOrigin?
    private var queueReadInFlight = false
    @Published private(set) var queueMutationInFlight = false
    @Published var queueErrorMessage: String?
    @Published var failedConcurrentInput: String?
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
    private var activityStreamTask: Task<Void, Never>?
    private var activityStreamTurnId: String?
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
            SpeechSynthesizer.shared.speak(text, messageId: messageId)
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

    // Subscribe to magician's realtime event stream — the SAME WebSocket the web
    // UI and the background engine use. Chat messages are SENT over REST
    // (POST .../messages); the assistant reply + execution-panel updates arrive
    // here as broadcast events. (magician has no chat WebSocket, and /realtime/ws
    // lives under the v2 API scope — there is no v3.)
    func connect() {
        guard !isRunningUnderTests else { return }   // no real WebSocket in unit tests
        // The workspace-bound bearer authorizes the upgrade and the backend
        // filters every event to that resolved scope.
        guard let url = Self.realtimeSocketURL() else { return }

        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        let session = URLSession(configuration: .default, delegate: self, delegateQueue: OperationQueue.main)
        webSocket = session.webSocketTask(with: request)
        webSocket?.resume()
        isConnected = true

        receiveMessage()
    }

    static func realtimeSocketURL(baseURL: String = "\(MagicianAccess.webSocketBaseURL.absoluteString)") -> URL? {
        return URL(
            string: "\(baseURL)/api/magician/v2/realtime/ws?supports_structured_presentation=true"
        )
    }

    func disconnect() {
        webSocket?.cancel(with: .goingAway, reason: nil)
        activityStreamTask?.cancel()
        activityStreamTask = nil
        activityStreamTurnId = nil
        isConnected = false
    }

    // MARK: - Session actions
    private let greeting = "I am Sam. How can I help you today?"

    /// Adopt a freshly-created (or empty) session — reset the transcript.
    func startNewSession(_ id: String?) {
        resetTurnActivityState()
        if let id {
            sessionSelectionStore.remember(id)
        } else if let currentSessionId {
            sessionSelectionStore.forget(currentSessionId)
        }
        currentSessionId = id
        currentRunExecutionId = nil
        activeExecutionLookupGeneration.removeAll()
        messages = [ChatMessage(id: UUID().uuidString, isUser: false, text: greeting, type: .text)]
    }

    func forgetRememberedSession(_ id: String?) {
        sessionSelectionStore.forget(id)
    }

    /// Clear the current session's transcript locally and on the server.
    func clearMessages() {
        resetTurnActivityState()
        messages = [ChatMessage(id: UUID().uuidString, isUser: false, text: greeting, type: .text)]
        if let sessionId = currentSessionId { deleteRemote("chat/sessions/\(sessionId)/messages") }
    }

    /// Delete a single message locally and on the server.
    func deleteMessage(_ id: String) {
        messages.removeAll { $0.id == id }
        if let sessionId = currentSessionId { deleteRemote("chat/sessions/\(sessionId)/messages/\(id)") }
    }

    private func resetTurnActivityState() {
        transcriptGeneration &+= 1
        streamingMessageId = nil
        isThinking = false
        currentTurnViaVoice = false
        activityStreamTask?.cancel()
        activityStreamTask = nil
        activityStreamTurnId = nil
        activeSendingChatTurnId = nil
        activityEventsByTurn.removeAll()
        activityRowsByTurn.removeAll()
        hydratedActivityTurns.removeAll()
        activityLoadsInFlight.removeAll()
        forcedActivityRefreshPending.removeAll()
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
        guard !queueReadInFlight, let sessionId = currentSessionId,
              let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/queue") else { return }
        queueReadInFlight = true
        let base = MagicianAccess.baseURL.absoluteString
        var request = URLRequest(url: url); request.timeoutInterval = 10
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            let queue = data.flatMap { try? JSONDecoder().decode(QueueResponse.self, from: $0) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.queueReadInFlight = false
                guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base,
                      (response as? HTTPURLResponse)?.statusCode == 200, let queue else { return }
                self.queuedMessages = queue.queued; self.serverRunning = queue.active ?? false
            }
        }.resume()
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
            "harness_engine": selectedHarnessEngine, "harness_model": selectedHarnessModel,
            "attachment_ids": attachments.compactMap(\.remoteId), "source_surface": "mobile"]
        body["mode"] = composerMode.wire
        if !selectedProfile.isEmpty { body["profile"] = selectedProfile }
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

        if (viaVoice || background), composerMode != .plan, stagedAttachments.isEmpty {
            admittedConcurrently = true
            let parent = currentSessionId
            var options: [String: Any] = ["source_surface": "ios", "harness_engine": selectedHarnessEngine, "harness_model": selectedHarnessModel]
            options["mode"] = composerMode.wire
            if !selectedProfile.isEmpty && ["magician", "pi"].contains(selectedHarnessEngine) { options["profile"] = selectedProfile }
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
        let postedHarnessEngine = selectedHarnessEngine
        let postedHarnessModel = selectedHarnessModel
        let postedProfile = selectedProfile

        // Optimistic local echo of the user's message.
        var echo = ChatMessage(id: UUID().uuidString, isUser: true, text: trimmed, type: .text)
        echo.voiceOrigin = viaVoice
        echo.chatTurnId = chatTurnId
        messages.append(echo)
        isThinking = true
        activity.reset()  // fresh activity timeline for this turn
        SpeechSynthesizer.shared.stop()  // hush any prior spoken reply

        if let sessionId = currentSessionId {
            postMessageStreaming(
                trimmed, to: sessionId, chatTurnId: chatTurnId, mode: postedMode,
                harnessEngine: postedHarnessEngine, harnessModel: postedHarnessModel,
                profile: postedProfile
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
                    profile: postedProfile
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
        profile: String
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
        if !profile.isEmpty && (harnessEngine == "magician" || harnessEngine == "pi") {
            payload["profile"] = profile
        }
        payload["harness_engine"] = harnessEngine
        payload["harness_model"] = harnessModel
        let uploadedIds = stagedAttachments.compactMap { $0.remoteId }
        if !uploadedIds.isEmpty { payload["attachment_ids"] = uploadedIds }
        if viaVoice { payload["voice_origin"] = true }   // web parity: stamp the turn's origin
        if let mode { payload["mode"] = mode }
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
        placeholder.activityRows = activityRowsByTurn[chatTurnId] ?? activity.snapshot()
        placeholder.activityIsLive = isActivityTurnLive(chatTurnId)
        messages.append(placeholder)
        startActivityStream(sessionId: sessionId, chatTurnId: chatTurnId)
        Task { [weak self] in
            guard let self = self else { return }
            do {
                let (bytes, response) = try await networkSession.bytes(for: request)
                if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
                    await self.finishStreaming(streamId, chatTurnId: chatTurnId, generation: generation, gotContent: false)
                    return
                }
                var accumulated = ""
                // SSE frames are `event: token` / `data: {"text": "…"}`; parse data lines.
                for try await line in bytes.lines {
                    guard line.hasPrefix("data:") else { continue }
                    let json = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
                    guard let d = json.data(using: .utf8),
                          let obj = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
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
                            m.activityRows = self.activityRowsByTurn[chatTurnId]
                                ?? self.activity.snapshot()
                            m.activityIsLive = self.isActivityTurnLive(chatTurnId)
                            self.messages.append(m)
                        }
                    }
                }
                await self.finishStreaming(
                    streamId,
                    chatTurnId: chatTurnId,
                    generation: generation,
                    gotContent: !accumulated.isEmpty
                )
            } catch {
                await self.finishStreaming(streamId, chatTurnId: chatTurnId, generation: generation, gotContent: false)
                debugLog("Chat stream failed: \(error.localizedDescription)")
            }
        }
    }

    @MainActor
    private func finishStreaming(_ streamId: String, chatTurnId: String, generation: UInt64, gotContent: Bool) {
        guard transcriptGeneration == generation else { return }
        isThinking = false
        if activeSendingChatTurnId == chatTurnId {
            activeSendingChatTurnId = nil
        }
        if gotContent {
            // Attach the turn's activity timeline to the finalized streamed message.
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
            // Suppress the WS duplicate of this turn's final message for a short grace.
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
                if self?.streamingMessageId == streamId { self?.streamingMessageId = nil }
            }
        } else {
            // No tokens streamed — drop the empty placeholder and let the WS deliver it.
            if let idx = messages.firstIndex(where: { $0.id == streamId && $0.text.isEmpty }) {
                messages.remove(at: idx)
            }
            streamingMessageId = nil
        }
        synchronizeActivityLivenessAndStream()
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

    /// The turn-events stream's idle allowance: 15 minutes, because the server
    /// endpoint sends NO heartbeat — a healthy turn that is quietly inside a
    /// long tool call streams nothing for minutes, and URLSession's default
    /// 60 s idle timeout read that silence as death (-1001) on every such
    /// turn. Sized to outlast the longest quiet turn the panel follows; the
    /// systemic fix is a server-side heartbeat on the endpoint, deferred — its
    /// file is owned by a parallel change in flight.
    nonisolated static let turnActivityStreamIdleTimeout: TimeInterval = 15 * 60

    private func startActivityStream(sessionId: String, chatTurnId: String) {
        if activityStreamTurnId == chatTurnId, activityStreamTask != nil { return }
        activityStreamTask?.cancel()
        activityStreamTurnId = chatTurnId
        if activityEventsByTurn[chatTurnId] == nil { activityEventsByTurn[chatTurnId] = [] }
        if activityRowsByTurn[chatTurnId] == nil { activityRowsByTurn[chatTurnId] = [] }

        guard let url = turnActivityURL(
            sessionId: sessionId,
            chatTurnId: chatTurnId,
            streaming: true
        ) else { return }
        var request = URLRequest(url: url)
        request.setValue("application/x-ndjson", forHTTPHeaderField: "Accept")
        // Per-request rather than per-session: only this stream lives on a
        // heartbeat-free endpoint; every other request keeps the default that
        // makes real deadness visible. See `turnActivityStreamIdleTimeout`.
        request.timeoutInterval = Self.turnActivityStreamIdleTimeout
        MagicianAccess.authorize(&request)

        activityStreamTask = Task { [weak self] in
            guard let self else { return }
            var retryDelay: UInt64 = 1_000_000_000
            while !Task.isCancelled {
                var receivedEvent = false
                do {
                    let (bytes, response) = try await self.networkSession.bytes(for: request)
                    guard let http = response as? HTTPURLResponse,
                          (200..<300).contains(http.statusCode) else {
                        throw URLError(.badServerResponse)
                    }
                    for try await line in bytes.lines {
                        guard !Task.isCancelled else { break }
                        guard let data = line.data(using: .utf8),
                              let event = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                        else { continue }
                        receivedEvent = true
                        await MainActor.run {
                            guard self.currentSessionId == sessionId,
                                  self.activityStreamTurnId == chatTurnId else { return }
                            self.ingestCanonicalActivity(event, chatTurnId: chatTurnId)
                        }
                    }
                } catch is CancellationError {
                    break
                } catch {
                    // An idle timeout on this endpoint is a QUIET turn, not a
                    // failure: no heartbeat exists yet, so a long silent
                    // stretch reads as dead air to URLSession. The loop
                    // reconnects either way; logging -1001 as "failed" trained
                    // readers to skim past the line that matters when the
                    // stream genuinely breaks.
                    if (error as? URLError)?.code != .timedOut {
                        debugLog("Chat activity stream failed: \(error.localizedDescription)")
                    }
                }
                guard !Task.isCancelled else { break }
                if receivedEvent { retryDelay = 1_000_000_000 }
                do {
                    try await Task.sleep(nanoseconds: retryDelay)
                } catch {
                    break
                }
                retryDelay = min(retryDelay * 2, 30_000_000_000)
            }
            await MainActor.run {
                if self.activityStreamTurnId == chatTurnId {
                    self.activityStreamTurnId = nil
                    self.activityStreamTask = nil
                }
            }
        }
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
        guard let url = turnActivityURL(
                sessionId: sessionId,
                chatTurnId: chatTurnId,
                streaming: false
              ) else { return }
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
        activityRowsByTurn[chatTurnId] = rows
        let targetIndex = messageId.flatMap { id in messages.firstIndex { $0.id == id } }
            ?? messages.lastIndex {
                !$0.isUser && $0.chatTurnId == chatTurnId && Self.canOwnActivityTimeline($0)
            }
        guard let targetIndex else { return }
        messages[targetIndex].activityRows = rows
        messages[targetIndex].activityIsLive = isActivityTurnLive(chatTurnId, rows: rows)
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
        guard Self.latestActiveTaskTurnId(messages) == chatTurnId else { return false }
        let taskRows = (rows ?? activityRowsByTurn[chatTurnId] ?? [])
            .filter { $0.key.hasPrefix("task::") }
        return taskRows.isEmpty || taskRows.contains { $0.status == .running || $0.status == .waiting }
    }

    private func synchronizeActivityLivenessAndStream() {
        let desiredTurnId = activeSendingChatTurnId ?? Self.latestActiveTaskTurnId(messages)
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
            activityStreamTask?.cancel()
            activityStreamTask = nil
            activityStreamTurnId = nil
            return
        }
        startActivityStream(sessionId: sessionId, chatTurnId: desiredTurnId)
    }

    private func turnActivityURL(
        sessionId: String,
        chatTurnId: String,
        streaming: Bool
    ) -> URL? {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let session = sessionId.addingPercentEncoding(withAllowedCharacters: allowed),
              let turn = chatTurnId.addingPercentEncoding(withAllowedCharacters: allowed)
        else { return nil }
        let suffix = streaming ? "/stream" : ""
        return URL(
            string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(session)/turns/\(turn)/events\(suffix)"
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
        return unique.sorted {
            $0.timestamp == $1.timestamp ? $0.index < $1.index : $0.timestamp < $1.timestamp
        }.map(\.event)
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
                if !self.chatHarnesses.contains(where: { $0.name == self.selectedHarnessEngine }) {
                    self.selectedHarnessEngine = "magician"
                }
                let models = self.chatHarnesses.first(where: { $0.name == self.selectedHarnessEngine })?.availableModels ?? ["default"]
                if !models.contains(self.selectedHarnessModel) { self.selectedHarnessModel = "default" }
            }
        }.resume()
    }

    func fetchProfiles() {
        guard !isUITestLaunch else { return }   // UI tests: no real backend fetch (keeps launch idle)
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/profiles") else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, _, _ in
            guard let self = self, let data = data,
                  let decoded = try? JSONDecoder().decode(ChatProfilesResponse.self, from: data) else { return }
            DispatchQueue.main.async {
                self.profiles = decoded.profiles
                if !decoded.profiles.contains(where: { $0.name == self.selectedProfile }) {
                    self.selectedProfile = decoded.profiles.first(where: { $0.isDefault == true })?.name
                        ?? decoded.profiles.first?.name ?? ""
                }
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
            }
        }.resume()
    }

    // MARK: - HITL (answer an escalation)
    func respondToEscalation(
        content: ChatMessageContentData,
        submission: ChatHitlSubmission,
        completion: @escaping (Bool) -> Void = { _ in }
    ) {
        guard let target = content.hitlTarget else {
            escalationResponseErrorMessage = "This request is missing its canonical response identity."
            completion(false)
            return
        }
        guard respondingEscalationID == nil else {
            escalationResponseErrorMessage = "Another response is still being sent."
            completion(false)
            return
        }

        switch submission {
        case .response(let value):
            respondToCanonicalHitl(
                content: content,
                target: target,
                value: value,
                completion: completion
            )
        case .continueExecution:
            continueAgenticExecution(content: content, target: target, completion: completion)
        }
    }

    private func respondToCanonicalHitl(
        content: ChatMessageContentData,
        target: ChatHitlTarget,
        value: ChatHitlResponseValue,
        completion: @escaping (Bool) -> Void
    ) {
        guard let endpoint = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/hitl") else {
            escalationResponseErrorMessage = "The response URL is invalid."
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
                        self.escalationResponseErrorMessage =
                            "The server requested another answer without returning its response identity."
                        completion(false)
                        return
                    }
                    self.replaceEscalation(content, with: reask)
                    self.escalationResponseErrorMessage = reason
                        .map { "More information is needed: \($0)" }
                        ?? "More information is needed. Please revise your answer."
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
                    self.escalationResponseErrorMessage = error?.localizedDescription
                        ?? reason
                        ?? "Could not send the response (HTTP \(status))."
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
        completion: @escaping (Bool) -> Void
    ) {
        guard content.escalationType == "max_iterations",
              let executionId = target.executionId,
              !executionId.isEmpty,
              let endpoint = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/executions") else {
            escalationResponseErrorMessage = "This continuation is missing its execution identity."
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
                    self.escalationResponseErrorMessage = error?.localizedDescription
                        ?? reason
                        ?? "Could not continue the run (HTTP \(status))."
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
        networkSession.dataTask(with: request) { data, _, _ in
            var sessionId: String? = nil
            if let data = data,
               let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let session = obj["session"] as? [String: Any],
               let id = session["id"] as? String {
                sessionId = id
            }
            DispatchQueue.main.async { completion(sessionId) }
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
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(sessionId)/attachments") else {
            markUpload(localId, failed: true); return
        }
        let boundary = "Boundary-\(UUID().uuidString)"
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        var body = Data()
        body.append("--\(boundary)\r\n".data(using: .utf8)!)
        body.append("Content-Disposition: form-data; name=\"file\"; filename=\"\(filename)\"\r\n".data(using: .utf8)!)
        body.append("Content-Type: \(mime)\r\n\r\n".data(using: .utf8)!)
        body.append(data)
        body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
        request.httpBody = body
        networkSession.dataTask(with: request) { [weak self] data, _, _ in
            self?.markUpload(localId, remoteId: Self.parseAttachmentId(data))
        }.resume()
    }

    /// Robustly pull the attachment id from the upload response (top-level or
    /// nested), so a shape variant doesn't silently drop the marker.
    private static func parseAttachmentId(_ data: Data?) -> String? {
        guard let data = data,
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        if let id = obj["attachment_id"] as? String { return id }
        if let id = obj["id"] as? String { return id }
        if let a = obj["attachment"] as? [String: Any] {
            return (a["attachment_id"] as? String) ?? (a["id"] as? String)
        }
        return nil
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
                synthesisPending: content.synthesisPending ?? false
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
        return built
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
                var rows = decoded.messages
                var hasMore = rows.count >= 200
                var cursors = Set<String>()
                while let target, !rows.contains(where: target.matches), hasMore {
                    try Task.checkCancellation()
                    guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base else { return }
                    guard let before = rows.first?.id, cursors.insert(before).inserted else {
                        throw URLError(.cannotParseResponse)
                    }
                    var components = URLComponents(url: url.appendingPathComponent("messages"), resolvingAgainstBaseURL: false)!
                    components.queryItems = [URLQueryItem(name: "limit", value: "200"), URLQueryItem(name: "before", value: before)]
                    var olderRequest = URLRequest(url: components.url!)
                    MagicianAccess.authorize(&olderRequest)
                    let (olderData, olderResponse) = try await self.networkSession.data(for: olderRequest)
                    guard (olderResponse as? HTTPURLResponse)?.statusCode == 200 else { throw URLError(.badServerResponse) }
                    let page = try JSONDecoder().decode(LinkedAnswerMessagePage.self, from: olderData)
                    rows = page.messages + rows
                    hasMore = page.hasMore && !page.messages.isEmpty
                }
                try Task.checkCancellation()
                guard self.currentSessionId == sessionId, MagicianAccess.baseURL.absoluteString == base else { return }
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
                self.synchronizeActivityLivenessAndStream()
            } catch {
                guard !Task.isCancelled, self.currentSessionId == sessionId else { return }
                self.originalAnswerError = "Could not open this conversation. Please try again."
            }
        }
    }

    private struct LinkedAnswerMessagePage: Decodable {
        let messages: [ChatMessageRawData]
        let hasMore: Bool
        enum CodingKeys: String, CodingKey { case messages, hasMore = "has_more" }
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

    private func receiveMessage() {
        webSocket?.receive { [weak self] result in
            DispatchQueue.main.async {
                switch result {
                case .success(let message):
                    switch message {
                    case .string(let text):
                        self?.handleIncomingJSON(text)
                    default:
                        break
                    }
                    self?.receiveMessage()
                case .failure(let error):
                    debugLog("WebSocket receive error: \(error)")
                    self?.isConnected = false
                    // Optionally attempt reconnect
                }
            }
        }
    }

    /// Internal (not private) so realtime event routing can be unit-tested by
    /// feeding crafted event JSON directly (bypassing the WebSocket).
    func handleIncomingJSON(_ jsonString: String) {
        guard let data = jsonString.data(using: .utf8) else { return }

        do {
            if let jsonObj = try JSONSerialization.jsonObject(with: data, options: []) as? [String: Any],
               let eventType = jsonObj["event_type"] as? String {

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
                                } else {
                                    self.messages.append(msg)
                                }
                            } else if !isOwnStreamEcho {
                                if self.activeSendingChatTurnId == nil
                                    || self.activeSendingChatTurnId == raw.chatTurnId {
                                    self.isThinking = false
                                }
                                var msg = Self.projectedMessage(from: raw)
                                if let turnId = msg.chatTurnId,
                                   self.activeSendingChatTurnId == turnId {
                                    self.activeSendingChatTurnId = nil
                                }
                                msg.activityRows = msg.chatTurnId.flatMap {
                                    self.activityRowsByTurn[$0]
                                } ?? self.activity.snapshot()
                                if let turnId = msg.chatTurnId {
                                    msg.activityIsLive = self.isActivityTurnLive(turnId)
                                }
                                var transcript = self.messages
                                if Self.reconcileCanonicalText(msg, into: &transcript) {
                                    self.messages = transcript
                                    if let turnId = msg.chatTurnId {
                                        self.refreshActivityRows(
                                            sessionId: self.currentSessionId,
                                            messageId: msg.id,
                                            chatTurnId: turnId,
                                            force: true
                                        )
                                    }
                                    if msg.chatTurnId?.hasPrefix("voice-request-") != true && msg.chatTurnId?.hasPrefix("voice-task-result-") != true {
                                        self.speakReplyIfEnabled(msg.text, messageId: msg.id)
                                    }
                                }
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
        } catch {
            debugLog("Failed to parse websocket JSON: \(error)")
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
                activeRootExecutionId: existing?.activeRootExecutionId
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
                  let task = object["task"] as? [String: Any],
                  let status = task["status"] as? String else { return }
            let activeRootExecutionId = TaskStatusModel.resolveActiveExecutionId(
                status: status,
                activeRootExecutionId: task["active_root_execution_id"] as? String
            )
            let taskSessionId = task["chat_session_id"] as? String
            DispatchQueue.main.async {
                guard self.activeExecutionLookupGeneration[taskId] == generation else { return }
                if let index = self.messages.firstIndex(where: {
                    if case .taskStatus(let model) = $0.type { return model.taskId == taskId }
                    return false
                }), case .taskStatus(var model) = self.messages[index].type {
                    model.status = status
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
