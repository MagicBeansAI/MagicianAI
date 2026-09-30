import Foundation
import UIKit

enum TutorOverlayPhase: Equatable {
    case ready, connecting, uploading, teaching, completed, failed
}

struct TutorVisibleShape: Identifiable {
    let id: UUID
    let shape: TutorShape
    let appearedAt: Date
    let duration: TimeInterval
}

@MainActor
final class TutorOverlayViewModel: NSObject, ObservableObject, URLSessionWebSocketDelegate {
    @Published private(set) var phase: TutorOverlayPhase = .ready
    @Published private(set) var visibleShapes: [TutorVisibleShape] = []
    @Published private(set) var caption: String?
    /// Structured step-bubble content (web parity): the step title + narration for
    /// the current teaching step, shown above the plain status caption.
    @Published private(set) var stepLabel: String?
    @Published private(set) var stepNarration: String?
    @Published private(set) var errorMessage: String?
    @Published private(set) var isKeptShowing = false
    @Published private(set) var dismissalRequested = false
    /// True only after the latest queued draw/narration has fully settled.
    /// This is the safe live boundary for asking about the step on screen.
    @Published private(set) var isStepSettled = false

    /// nil in blackboard mode (source-free); a visual source in screen_overlay mode.
    private let screenshot: UIImage?
    private let canvasMode: TutorCanvasMode
    private let networkSession: URLSession
    private let playback: TutorPlaybackCoordinator
    private let connectsRealtime: Bool
    private let expirySeconds: TimeInterval
    private let replayInterStepMilliseconds: Double
    private var socketSession: URLSession?
    private var webSocket: URLSessionWebSocketTask?
    private var chatTurnId = ""
    /// Readable by the view so the "Explain deeper" control can be disabled
    /// when there is no session to send into. Still only WRITTEN here.
    private(set) var currentSessionId: String?
    private var backendRunTerminal = false
    private var cancellationRequested = false
    private var revealState = TutorRevealState()
    private var bufferedShapes: [TutorShape] = []
    private var playbackTasks: [Task<Void, Never>] = []
    private var playbackEpoch = 0
    private var playbackRevision = 0
    private var playbackTail: Task<Void, Never>?
    private var expiryTask: Task<Void, Never>?
    private var requestTask: Task<Void, Never>?
    /// Per-shape TTL timers (web parity) — keyed by the visible shape id.
    private var shapeExpiryTasks: [UUID: Task<Void, Never>] = [:]

    init(
        screenshot: UIImage? = nil,
        canvasMode: TutorCanvasMode = .screenOverlay,
        networkSession: URLSession = .shared,
        narrator: TutorNarrating? = nil,
        connectsRealtime: Bool = true,
        expirySeconds: TimeInterval = 60,
        replayInterStepMilliseconds: Double = 650,
        sleepMilliseconds: @escaping TutorPlaybackCoordinator.Sleeper = { milliseconds in
            guard milliseconds > 0 else { return }
            try? await Task.sleep(nanoseconds: UInt64(milliseconds * 1_000_000))
        }
    ) {
        self.screenshot = screenshot
        self.canvasMode = canvasMode
        self.networkSession = networkSession
        self.playback = TutorPlaybackCoordinator(
            narrator: narrator ?? SystemTutorNarrator(),
            sleepMilliseconds: sleepMilliseconds
        )
        self.connectsRealtime = connectsRealtime
        self.expirySeconds = expirySeconds
        self.replayInterStepMilliseconds = replayInterStepMilliseconds
        super.init()
    }

    var canReplay: Bool { !bufferedShapes.isEmpty }

    var canExplainDeeper: Bool {
        let requestable = DeeperRequest.canRequest(
            step: DeeperRequest.Step(label: stepLabel, narration: stepNarration),
            sessionId: currentSessionId
        )
        let safePhase = phase == .completed || (phase == .teaching && isStepSettled)
        return requestable && safePhase && !visibleShapes.isEmpty
    }

    func start(question: String) {
        let trimmed = question.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            errorMessage = "Ask a question first."
            return
        }
        if !backendRunTerminal, !cancellationRequested, let currentSessionId {
            cancelActiveTutorTurn(sessionId: currentSessionId)
        }
        stopPlayback()
        stopExpiry()
        visibleShapes.removeAll(); bufferedShapes.removeAll(); revealState.reset()
        currentSessionId = nil; backendRunTerminal = false; cancellationRequested = false
        isStepSettled = false
        isKeptShowing = false; dismissalRequested = false
        stepLabel = nil; stepNarration = nil
        errorMessage = nil; caption = "Connecting to Tutor…"; phase = .connecting
        let initialChatTurnId = "ios-tutor-\(UUID().uuidString)"
        chatTurnId = initialChatTurnId
        if connectsRealtime { connectRealtime() }
        requestTask?.cancel()
        // Refresh the primitive recipe set in the background so a newly-authored
        // primitive renders without an app rebuild. Best-effort and decoupled from
        // the tutor-run session (its own `URLSession.shared` GET) — the run never
        // blocks on it; the bundled/cached set is used until it lands.
        Task { await TutorPrimitiveRegistry.shared.refresh() }
        requestTask = Task {
            do {
                let sessionId = try await createSession()
                guard !Task.isCancelled else { return }
                currentSessionId = sessionId
                if canvasMode == .blackboard {
                    // Source-free: no upload; the backend derives blackboard from the
                    // absence of a visual source and draws the concept from scratch.
                    phase = .teaching; caption = "Tutor is thinking…"
                    try await sendTutorMessage(
                        sessionId: sessionId,
                        attachmentId: nil,
                        text: "@tutor \(trimmed)",
                        chatTurnId: initialChatTurnId
                    )
                } else {
                    phase = .uploading; caption = "Uploading screenshot…"
                    let attachmentId = try await uploadScreenshot(sessionId: sessionId)
                    guard !Task.isCancelled else { return }
                    phase = .teaching; caption = "Tutor is studying the screen…"
                    try await sendTutorMessage(
                        sessionId: sessionId,
                        attachmentId: attachmentId,
                        text: "@tutor \(trimmed)",
                        chatTurnId: initialChatTurnId
                    )
                }
            } catch {
                if !Task.isCancelled { fail(error.localizedDescription) }
            }
        }
    }

    /// Ask the tutor to decompose the step currently on screen.
    ///
    /// A normal `@tutor` turn rather than a mutation of this run. It is offered
    /// only after the latest draw/narration settles; a fresh correlated turn
    /// reuses the whole pipeline — plan, milestone roles, figure-backed coverage.
    func explainDeeper() {
        let step = DeeperRequest.Step(label: stepLabel, narration: stepNarration)
        guard canExplainDeeper,
              let composed = DeeperRequest.compose(step: step, sessionId: currentSessionId) else {
            return
        }
        let deeperChatTurnId = "ios-tutor-\(UUID().uuidString)"
        chatTurnId = deeperChatTurnId
        backendRunTerminal = false
        cancellationRequested = false
        // A replay or prior live-step tail must not continue drawing over the
        // new explanation once the correction turn is admitted.
        stopPlayback()
        stopExpiry()
        phase = .teaching
        caption = "Tutor is going deeper…"
        errorMessage = nil
        requestTask?.cancel()
        requestTask = Task { [weak self] in
            do {
                try await self?.sendTutorMessage(
                    sessionId: composed.sessionId,
                    attachmentId: nil,
                    text: composed.prompt,
                    chatTurnId: deeperChatTurnId
                )
            } catch {
                guard !Task.isCancelled else { return }
                self?.fail("Could not ask for more detail.")
            }
        }
    }

    func replay() {
        guard !bufferedShapes.isEmpty else { return }
        stopPlayback(); stopExpiry(); visibleShapes.removeAll(); revealState.reset()
        phase = .teaching; caption = "Replaying guide…"
        let shapes = bufferedShapes
        let epoch = playbackEpoch
        let task = Task { @MainActor in
            for (index, shape) in shapes.enumerated() {
                guard !Task.isCancelled, epoch == playbackEpoch else { return }
                await play(shape)
                guard !Task.isCancelled, epoch == playbackEpoch else { return }
                isStepSettled = !visibleShapes.isEmpty
                if index < shapes.count - 1, replayInterStepMilliseconds > 0 {
                    try? await Task.sleep(nanoseconds: UInt64(replayInterStepMilliseconds * 1_000_000))
                    guard !Task.isCancelled, epoch == playbackEpoch else { return }
                    isStepSettled = false
                }
            }
            guard !Task.isCancelled, epoch == playbackEpoch else { return }
            completePlayback()
        }
        playbackTail = task
        playbackTasks.append(task)
    }

    func keepShowing() {
        isKeptShowing = true
        stopExpiry()
        caption = "Guide kept on screen"
    }

    func askAgain() {
        requestTask?.cancel(); requestTask = nil
        stopPlayback(); stopExpiry()
        visibleShapes.removeAll(); bufferedShapes.removeAll(); revealState.reset()
        currentSessionId = nil; backendRunTerminal = true; cancellationRequested = false
        isStepSettled = false
        isKeptShowing = false; dismissalRequested = false
        stepLabel = nil; stepNarration = nil
        errorMessage = nil; caption = nil; phase = .ready
    }

    func dismissTutor() {
        let shouldCancelServer = !backendRunTerminal && !cancellationRequested
        cancellationRequested = true
        if shouldCancelServer, let sessionId = currentSessionId {
            cancelActiveTutorTurn(sessionId: sessionId)
        }
        requestTask?.cancel(); requestTask = nil
        stopPlayback(); stopExpiry()
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil; socketSession?.invalidateAndCancel(); socketSession = nil
    }

    func cancel() { dismissTutor() }

    private func connectRealtime() {
        guard webSocket == nil else { return }
        var request = URLRequest(url: URL(string: "\(MagicianAccess.webSocketBaseURL.absoluteString)/api/magician/v2/realtime/ws")!)
        MagicianAccess.authorize(&request)
        let session = URLSession(configuration: .default, delegate: self, delegateQueue: OperationQueue.main)
        socketSession = session
        webSocket = session.webSocketTask(with: request)
        webSocket?.resume()
        receiveNext()
    }

    private func createSession() async throws -> String {
        var request = URLRequest(url: endpoint("chat/new"))
        request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data("{}".utf8); MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try validate(response)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let session = object["session"] as? [String: Any],
              let id = session["id"] as? String else { throw TutorOverlayError.invalidResponse }
        return id
    }

    private func uploadScreenshot(sessionId: String) async throws -> String {
        guard let screenshot, let png = screenshot.pngData() else { throw TutorOverlayError.imageEncoding }
        let pixelWidth = screenshot.cgImage?.width ?? Int(screenshot.size.width * screenshot.scale)
        let pixelHeight = screenshot.cgImage?.height ?? Int(screenshot.size.height * screenshot.scale)
        let context = try JSONSerialization.data(withJSONObject: [
            "mode": "screenshot", "coordinate_space": "capture",
            "image_size": ["width": pixelWidth, "height": pixelHeight]
        ])
        let boundary = "TutorBoundary-\(UUID().uuidString)"
        var body = Data()
        body.appendMultipart(boundary: boundary, name: "screen_capture", filename: nil,
                             contentType: "application/json", data: context)
        body.appendMultipart(boundary: boundary, name: "file", filename: "tutor-screenshot.png",
                             contentType: "image/png", data: png)
        body.append(Data("--\(boundary)--\r\n".utf8))
        var request = URLRequest(url: endpoint("chat/sessions/\(sessionId)/attachments"))
        request.httpMethod = "POST"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        request.httpBody = body; MagicianAccess.authorize(&request)
        let (data, response) = try await networkSession.data(for: request)
        try validate(response)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = object["attachment_id"] as? String else { throw TutorOverlayError.invalidResponse }
        return id
    }

    private func sendTutorMessage(
        sessionId: String,
        attachmentId: String?,
        text: String,
        chatTurnId: String
    ) async throws {
        var request = URLRequest(url: endpoint("chat/sessions/\(sessionId)/messages"))
        request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        var payload: [String: Any] = [
            "text": text,
            "chat_turn_id": chatTurnId,
            "source_surface": "ios_tutor_overlay"
        ]
        // Blackboard sends no attachment — the backend rejects a visual source there.
        if let attachmentId { payload["attachment_ids"] = [attachmentId] }
        request.httpBody = try JSONSerialization.data(withJSONObject: payload)
        MagicianAccess.authorize(&request)
        let (_, response) = try await networkSession.data(for: request)
        try validate(response)
    }

    private func receiveNext() {
        webSocket?.receive { [weak self] result in
            guard let self else { return }
            Task { @MainActor in
                switch result {
                case .success(.string(let text)):
                    self.handleRealtimeEvent(text)
                    self.receiveNext()
                case .success:
                    self.receiveNext()
                case .failure(let error):
                    self.webSocket = nil
                    self.socketSession?.invalidateAndCancel(); self.socketSession = nil
                    if self.phase != .completed && !self.cancellationRequested {
                        self.fail("Realtime connection lost: \(error.localizedDescription)")
                    }
                }
            }
        }
    }

    func handleRealtimeEvent(_ text: String) {
        guard let data = text.data(using: .utf8),
              let outer = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        var eventType = outer["event_type"] as? String ?? ""
        var payload = outer["data"] as? [String: Any] ?? [:]
        if eventType == "AgentEvent", let event = payload["event"] as? [String: Any] {
            eventType = event["event_type"] as? String ?? ""
            payload = event["payload"] as? [String: Any] ?? [:]
        }
        guard let turn = payload["chat_turn_id"] as? String, turn == chatTurnId else { return }
        switch eventType {
        case "tutor.draw.shape":
            guard let raw = payload["shape_json"],
                  JSONSerialization.isValidJSONObject(raw),
                  let shapeData = try? JSONSerialization.data(withJSONObject: raw),
                  let shape = try? JSONDecoder().decode(TutorShape.self, from: shapeData) else { return }
            isStepSettled = false
            bufferedShapes.append(shape)
            enqueue(shape)
        case "tutor.step.observed": caption = "Tutor observed the screenshot"
        case "tutor.step.target_resolved": caption = "Tutor found the target"
        case "tutor.run.completed":
            backendRunTerminal = true
            enqueueCompletion()
        case "tutor.run.failed", "tutor.step.failed":
            backendRunTerminal = true
            fail((payload["note"] as? String) ?? (payload["error"] as? String) ?? "Tutor couldn't complete this guide.")
        default: break
        }
    }

    private func enqueue(_ shape: TutorShape) {
        playbackRevision += 1
        let revision = playbackRevision
        let prior = playbackTail
        let epoch = playbackEpoch
        let task = Task { @MainActor [weak self] in
            await prior?.value
            guard let self, !Task.isCancelled, epoch == self.playbackEpoch else { return }
            await self.play(shape)
            guard !Task.isCancelled, epoch == self.playbackEpoch else { return }
            if revision == self.playbackRevision, !self.visibleShapes.isEmpty {
                self.isStepSettled = true
            }
        }
        playbackTail = task
        playbackTasks.append(task)
    }

    private func enqueueCompletion() {
        let prior = playbackTail
        let epoch = playbackEpoch
        let task = Task { @MainActor [weak self] in
            await prior?.value
            guard let self, !Task.isCancelled, epoch == self.playbackEpoch else { return }
            if self.visibleShapes.isEmpty {
                self.fail("Tutor couldn't ground a guide on this screenshot.")
            } else {
                self.completePlayback()
            }
        }
        playbackTail = task
        playbackTasks.append(task)
    }

    private func play(_ shape: TutorShape) async {
        if shape.type.lowercased() == "clear" {
            revealState.reset(); visibleShapes.removeAll(); caption = shape.caption
            cancelShapeExpiries()
            return
        }
        // Lifecycle (web parity): clear_previous drops prior non-persistent shapes,
        // and an incoming storyboard step clears shapes that were persisting until it.
        if shape.clearPrevious == true { removeShapes { $0.shape.persist != true } }
        if let step = shape.storyboardStepId, !step.isEmpty {
            removeShapes { $0.shape.persistUntilStep == step }
        }
        if let label = shape.tutorStepLabel ?? shape.stepLabel, !label.isEmpty { stepLabel = label }
        if let narration = shape.narration, !narration.isEmpty { stepNarration = narration }
        let items = revealState.ingest(shape)
        await playback.play(
            items: items,
            fallbackCaption: shape.caption,
            fallbackNarration: shape.narration,
            fallbackWaitForVoice: shape.waitForVoice == true
        ) { [weak self] item, caption in
            guard let self else { return }
            self.caption = caption ?? self.caption
            self.visibleShapes.append(TutorVisibleShape(
                id: item.id, shape: item.shape, appearedAt: Date(), duration: item.durationMs / 1000
            ))
            self.scheduleShapeExpiry(item.id, shape: item.shape)
        }
    }

    /// Per-shape auto-expiry (web `shapeTtlMs`): only when the backend sets `ttl_ms`
    /// and the shape is not `persist` — otherwise overlay-level expiry applies.
    private func scheduleShapeExpiry(_ id: UUID, shape: TutorShape) {
        guard shape.persist != true, let ttlMs = shape.ttlMs, ttlMs > 0 else { return }
        let ttl = max(20, ttlMs / 1000)   // web MIN_SHAPE_TTL_MS = 20s
        shapeExpiryTasks[id]?.cancel()
        shapeExpiryTasks[id] = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(ttl * 1_000_000_000))
            guard let self, !Task.isCancelled, !self.isKeptShowing else { return }
            self.visibleShapes.removeAll { $0.id == id }
            self.shapeExpiryTasks[id] = nil
        }
    }

    private func removeShapes(_ predicate: (TutorVisibleShape) -> Bool) {
        for vs in visibleShapes where predicate(vs) {
            shapeExpiryTasks[vs.id]?.cancel(); shapeExpiryTasks[vs.id] = nil
        }
        visibleShapes.removeAll(where: predicate)
    }

    private func cancelShapeExpiries() {
        shapeExpiryTasks.values.forEach { $0.cancel() }
        shapeExpiryTasks.removeAll()
    }

    private func completePlayback() {
        isStepSettled = !visibleShapes.isEmpty
        phase = .completed
        caption = "Guide complete"
        scheduleExpiry()
    }

    private func endpoint(_ path: String) -> URL {
        URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/\(path)")!
    }

    private func validate(_ response: URLResponse) throws {
        guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
            throw TutorOverlayError.server
        }
    }

    private func fail(_ message: String) {
        stopPlayback()
        phase = .failed; errorMessage = message; caption = nil
    }

    /// Stop the whole active Tutor turn, not only its Tutor state record.
    ///
    /// Tutor runs execute inside a chat turn and may currently be waiting in an
    /// LLM/tool call. Cancelling only `/tutor/cancel` leaves that enclosing turn
    /// alive, allowing a late tool result to redraw an overlay the user already
    /// dismissed. The shared `/run` endpoint atomically signals the chat turn
    /// and cancels the scoped Tutor run while preserving the reusable session.
    private func cancelActiveTutorTurn(sessionId: String) {
        var request = URLRequest(url: endpoint("chat/sessions/\(sessionId)/run"))
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        let session = networkSession
        Task {
            _ = try? await session.data(for: request)
        }
    }

    private func scheduleExpiry() {
        stopExpiry()
        guard !isKeptShowing, expirySeconds > 0 else { return }
        let seconds = expirySeconds
        expiryTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            guard let self, !Task.isCancelled, !self.isKeptShowing else { return }
            self.dismissalRequested = true
        }
    }

    private func stopExpiry() {
        expiryTask?.cancel()
        expiryTask = nil
    }

    private func stopPlayback() {
        playbackEpoch += 1
        playbackRevision += 1
        isStepSettled = false
        playbackTasks.forEach { $0.cancel() }
        playbackTasks.removeAll()
        playbackTail = nil
        playback.cancel()
        cancelShapeExpiries()
    }
}

private enum TutorOverlayError: LocalizedError {
    case imageEncoding, invalidResponse, server
    var errorDescription: String? {
        switch self {
        case .imageEncoding: return "Couldn't encode the screenshot."
        case .invalidResponse: return "The backend returned an unexpected response."
        case .server: return "The backend rejected the tutor request."
        }
    }
}

private extension Data {
    mutating func appendMultipart(boundary: String, name: String, filename: String?, contentType: String, data: Data) {
        append(Data("--\(boundary)\r\n".utf8))
        let filenamePart = filename.map { "; filename=\"\($0)\"" } ?? ""
        append(Data("Content-Disposition: form-data; name=\"\(name)\"\(filenamePart)\r\n".utf8))
        append(Data("Content-Type: \(contentType)\r\n\r\n".utf8))
        append(data); append(Data("\r\n".utf8))
    }
}
