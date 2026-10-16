import Foundation
import Combine
import os
import UIKit

/// Transport layer for the Live voice call.
///
/// Owns two things and nothing else:
///  1. Registration of a media session against
///     `POST /api/magician/v2/media/sessions` (the same registry the web
///     surface uses — see `ui/unified-ui/src/lib/media/session.ts`).
///  2. The control WebSocket at
///     `wss://…/api/magician/v2/media/voice/{session_id}/control`, over
///     which we send the `session.start` handshake, PTT frames, and raw
///     upstream audio, and receive server control events + downstream PCM.
///
/// All protocol shaping (envelope encoding + event routing) lives in the
/// pure, unit-tested `RealtimeVoiceProtocol` (Task 2). This class is a
/// device-verified transport shim: it holds no business logic beyond
/// wiring bytes to hooks and mapping routed events onto `@Published`
/// call state. The audio engine (Task 4) and the call UI (Task 5) wire
/// into it via `onIncomingAudio` / `onRebind` and the `sendAudio` /
/// PTT / `end` API — this file deliberately builds neither.
@MainActor
final class RealtimeVoiceClient: NSObject, ObservableObject {

    // MARK: - Call state

    enum Phase: Equatable {
        case idle
        case connecting
        case reconnecting
        case ready
        case rotating
        case ended
        case failed
    }

    typealias Caption = RealtimeVoiceCaption

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var errorMessage: String?
    /// Recoverable, feature-specific guidance for the current turn. Unlike a
    /// transport error it clears when the next admitted user turn arrives.
    @Published private(set) var guidedFlowNoticeMessage: String?
    @Published private(set) var captions: [Caption] = []
    /// The assistant finished an utterance but is still on the request — the
    /// silent gap between "let me check…" and the answer on an engine that
    /// runs tools without blocking. Set by `interaction.status`; cleared by a
    /// fresh `session.ready` (a rotated upstream starts idle) and by an
    /// interrupt, which abandons whatever it was working on.
    @Published private(set) var assistantWorking = false
    @Published private(set) var startedAt: Date?
    @Published private(set) var addressing: RealtimeVoiceProtocol.Addressing = .disabled
    /// Backend-resolved call boundary, for display only. `nil` until
    /// `session.ready` lands and on any backend that does not send it —
    /// unknown, never assumed to be the owner's own private call.
    @Published private(set) var boundary: RealtimeVoiceProtocol.Boundary?

    // MARK: - Hooks for later tasks

    /// Called with each downstream binary audio frame (provider → device).
    /// The audio engine sets this to feed its playback path.
    var onIncomingAudio: ((Data) -> Void)?
    var concurrentRequests = false
    var onConcurrentEvent: ((String, [String: Any]) -> Void)?
    private var selectedConcurrentContext: String?
    private(set) var concurrentResponsePending = false
    func selectConcurrentContext(_ id: String?) {
        selectedConcurrentContext = id
        send(kind: "voice.context", payload: ["context_session_id": id as Any? ?? NSNull()])
    }

    /// Called when the server asks the client to rebind its audio path
    /// (e.g. upstream rotation). The audio engine re-arms capture/playback.
    var onRebind: (() -> Void)?

    /// Called when the server reports the reply's audio is over, with whether
    /// it was cut off (`RealtimeVoiceProtocol.Event.assistantAudioEnded`). On
    /// an interrupt the audio engine must flush what it still has queued: the
    /// server collapsed its self-echo window at that instant, so every second
    /// the client keeps playing the orphaned tail is playback the server no
    /// longer accounts for. `VoiceCallViewModel.wireAudioPaths` attaches this
    /// on both surfaces — the in-app panel and the ambient sink share this
    /// transport through the same view model.
    var onAssistantAudioEnded: ((Bool) -> Void)?

    /// Called the instant `session.ready` is applied — synchronously, inside the
    /// control-event handling, with the socket open and `phase == .ready`.
    ///
    /// Distinct from observing `$phase` for a reason that bit once: that
    /// subscriber is delivered on the run loop and then discards any delivery
    /// whose phase has since moved on, which is right for suppressing a stale
    /// `.ended` and wrong for anything that must *happen* at ready. A `.ready`
    /// superseded within one run-loop turn would simply never be acted on. Used
    /// by `VoiceCallViewModel` to open the pre-ready gate — the moment hearing
    /// starts, since nothing captured earlier is kept (owner decision,
    /// 2026-07-30).
    var onReady: (() -> Void)?

    /// Native presentation handoff for source-free Tutor. The transport never
    /// constructs UI; the owning call view model closes audio first, then opens
    /// the blackboard.
    var onTutorBlackboardRequested: ((String, Bool) -> Void)?

    /// Surfaces deterministic lock/capability rejection text in the call UI.
    /// `backendAnnounced` tells the owner whether the realtime provider already
    /// spoke it, avoiding a second local voice on top.
    var onGuidedFlowRejected: ((String, Bool) -> Void)?

    // MARK: - Config

    private var realtimeProfile: String

    // MARK: - Transport

    private var socketSession: URLSession?
    private var webSocket: URLSessionWebSocketTask?
    /// The same socket, behind a lock, for the **audio** path only: `sendAudio` is
    /// called from the audio render thread, so it can't read the main-actor
    /// `webSocket` directly (that races with teardown). `URLSessionWebSocketTask`
    /// itself is thread-safe; only the reference read needs guarding.
    private let audioSocket = OSAllocatedUnfairLock<URLSessionWebSocketTask?>(initialState: nil)
    private var sessionId: String?
    private var urlSession: URLSession
    /// The turn boundary latched at `startCall`: true = push-to-talk, false =
    /// continuous capture with VAD deciding. Independent of `engineMode` — PTT
    /// is valid on both engines.
    private var pttOnMode = false
    /// The provider family latched at `startCall`.
    private var engineMode: VoiceEngine = .realtime
    /// PTT can be pressed while the replacement realtime session is still
    /// connecting. Preserve the held edge and apply it after `session.ready`
    /// instead of silently dropping the first utterance.
    private var pttHeld = false
    private var pttEngageSent = false
    private var pttStartedAt: TimeInterval = 0
    /// Fires if `session.ready` never arrives, so a half-open socket can't strand
    /// the user on "Connecting…" forever. Cancelled on ready / teardown.
    private var connectTimeout: DispatchWorkItem?
    /// `session.ready` is emitted only after Magician has restored and, when
    /// needed, compacted the selected chat's resume context. A cold compaction
    /// can legitimately exceed 15 seconds under background load, so the native
    /// watchdog must cover that backend preparation window instead of deleting
    /// a healthy session just before it becomes ready.
    ///
    /// Defined FROM the shared `AmbientConnectAttemptWindow` values rather than
    /// as literals, because the island's compact connect gauge draws exactly
    /// this window ("time until this attempt gives up") from the widget
    /// process: single-sourcing is what keeps the gauge from ever depicting a
    /// deadline this client no longer enforces.
    nonisolated static let startupTimeoutSeconds: TimeInterval = AmbientConnectAttemptWindow.socketAttemptSeconds
    nonisolated static let readyWaitTimeoutSeconds: TimeInterval = AmbientConnectAttemptWindow.attemptSeconds
    /// Invalidates registration work when a call is ended or superseded before
    /// its async POST completes.
    private var startGeneration = 0
    /// Pure caption reducer shared with protocol tests. Server item ids select
    /// the exact partial to update or remove across fallback and reconnects.
    private var captionState = RealtimeVoiceCaptionState()
    private var activeUiThreadId: String?
    private var reconnectAttempt = 0
    private var reconnectWork: DispatchWorkItem?
    /// The MID-CALL budget: a socket that drops after `session.ready` gets three
    /// quick tries, because the user is mid-conversation and every second of
    /// backoff is dead air on an open microphone.
    private let reconnectDelays: [TimeInterval] = [0.25, 0.75, 1.5]
    /// The CONNECT-PHASE budget, for a call that has never been ready. A socket
    /// blip there is part of provider bootstrap — the backend is minting a
    /// session, compacting resume context, dialling a provider — and the
    /// mid-call budget above gave up in ~2.5 s flat, well inside the 11–13 s a
    /// normal connect measures. Nobody is mid-sentence yet, so patience costs
    /// only waiting the user already committed to. What bounds it, precisely:
    /// the 45 s startup watchdog bounds each SOCKET ATTEMPT — `scheduleReconnect`
    /// cancels and re-arms it per attempt, so it is no overall clock — and the
    /// ambient path's true overall bound is the sink's `awaitReadySessionID`
    /// wall clock (`readyWaitTimeoutSeconds`), which gives up however many
    /// attempts remain. The in-app panel has no overall bound by construction,
    /// which predates this budget.
    private let connectReconnectDelays: [TimeInterval] = [0.5, 1.5, 3.0, 5.0]
    /// Which budget applies: false until this call's first `session.ready`, true
    /// from then on. Reset only in `startCall`, never by a reconnect, and paired
    /// with `.ready` zeroing `reconnectAttempt` — together they keep the
    /// invariant that a drop AFTER a successful connect always gets the original
    /// mid-call budget with a fresh counter, exactly as before the connect
    /// budget existed.
    private var hasBeenReadyThisCall = false
    init(
        realtimeProfile: String = "voice_realtime_openai_backend_mini",
        urlSession: URLSession = .shared
    ) {
        self.realtimeProfile = realtimeProfile
        self.urlSession = urlSession
        super.init()
        let center = NotificationCenter.default
        // UIApplication posts these on main. Synchronous selector delivery
        // preserves the will-lock edge; an extra Task hop could let backend
        // capture advance before the cancellation state is reported.
        center.addObserver(
            self,
            selector: #selector(protectedDataWillBecomeUnavailable(_:)),
            name: UIApplication.protectedDataWillBecomeUnavailableNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(protectedDataDidBecomeAvailable(_:)),
            name: UIApplication.protectedDataDidBecomeAvailableNotification,
            object: nil
        )
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    @objc private func protectedDataWillBecomeUnavailable(_ notification: Notification) {
        _ = notification
        reportScreenState(locked: true)
    }

    @objc private func protectedDataDidBecomeAvailable(_ notification: Notification) {
        _ = notification
        reportScreenState(locked: false)
    }

    // MARK: - Public lifecycle

    /// Register a media session then open the control WS and send the
    /// `session.start` handshake. Idempotent guard: a call already in
    /// flight or connected is a no-op.
    /// `engine` picks the provider family; `pttOn` picks the turn boundary.
    /// They are independent — see `RealtimeVoiceProtocol`.
    func startCall(
        uiThreadId: String,
        engine: VoiceEngine = .realtime,
        pttOn: Bool = false,
        realtimeProfile: String? = nil,
        requireVoicePrefix: Bool = false
    ) {
        guard phase == .idle || phase == .ended || phase == .failed else { return }
        if let realtimeProfile = realtimeProfile?.trimmingCharacters(in: .whitespacesAndNewlines),
           !realtimeProfile.isEmpty {
            self.realtimeProfile = realtimeProfile
        }
        phase = .connecting
        errorMessage = nil
        guidedFlowNoticeMessage = nil
        captionState.reset()
        captions = captionState.captions
        startedAt = nil
        addressing = RealtimeVoiceProtocol.Addressing(
            required: requireVoicePrefix,
            activationPhrases: [],
            followUpWindowMs: 8_000
        )
        pttOnMode = pttOn
        engineMode = engine
        activeUiThreadId = uiThreadId
        reconnectAttempt = 0
        hasBeenReadyThisCall = false
        pttHeld = false
        pttEngageSent = false
        startGeneration += 1
        let generation = startGeneration
        armConnectTimeout()

        Task { [weak self] in
            guard let self else { return }
            do {
                let sessionId = try await self.register()
                guard generation == self.startGeneration, self.phase == .connecting else {
                    self.disconnectRegisteredSession(sessionId)
                    return
                }
                self.sessionId = sessionId
                self.openControl(sessionId: sessionId, uiThreadId: uiThreadId)
            } catch {
                guard generation == self.startGeneration else { return }
                self.fail(with: (error as? VoiceTransportError)?.message ?? error.localizedDescription)
            }
        }
    }

    /// Surface a client-side (non-transport) failure — e.g. the mic/audio engine
    /// couldn't start — through the same failed-call path so the panel shows it.
    func failLocally(_ message: String) {
        fail(with: message)
    }

    /// The registered media/voice session id for the CURRENT call, available once
    /// the media-session POST has returned (nil before registration / after end).
    /// This is the id the backend stamps as `presence_session_id` on each spoken
    /// turn, so a Thinking Map can attach it for ambient "Listen" mapping.
    var mediaSessionID: String? { sessionId }

    /// Await the media session id for the in-flight call, resolving once the call
    /// is fully live (`.ready`) so attaching it will actually receive turns.
    /// Returns nil if the call fails/ends or the wait exceeds `timeoutSeconds`
    /// (by default, just past the client's startup timeout so a stalled connect
    /// surfaces as `.failed` first). Polls on the main actor, yielding between
    /// checks so `phase`/`sessionId` can update.
    func awaitReadySessionID(
        timeoutSeconds: Double = RealtimeVoiceClient.readyWaitTimeoutSeconds
    ) async -> String? {
        let deadline = Date().addingTimeInterval(timeoutSeconds)
        while Date() < deadline {
            switch phase {
            case .ready: return sessionId
            case .failed, .ended: return nil
            default: break
            }
            try? await Task.sleep(nanoseconds: 120_000_000)  // 120ms
        }
        return nil
    }

    private func armConnectTimeout() {
        connectTimeout?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.phase == .connecting || self.phase == .reconnecting else { return }
            self.fail(with: "Voice call didn't start in time.")
        }
        connectTimeout = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + Self.startupTimeoutSeconds,
            execute: work
        )
    }

    private func cancelConnectTimeout() {
        connectTimeout?.cancel()
        connectTimeout = nil
    }

    /// Send a raw upstream audio frame (device → provider). Called on the audio
    /// render thread, so it's `nonisolated` and reads the socket via a lock rather
    /// than the main-actor `webSocket`. No-op until the control WS is open.
    nonisolated func sendAudio(_ data: Data) {
        guard let ws = audioSocket.withLock({ $0 }) else { return }
        ws.send(.data(data)) { [weak self] error in
            if let error {
                Task { @MainActor in
                    guard let self, self.webSocket === ws else { return }
                    self.handleSendError(error)
                }
            }
        }
    }

    /// Engage push-to-talk (open the mic gate on the server).
    func engagePTT() {
        // Gated on the TURN BOUNDARY, not the engine: the backend's ptt.engage /
        // ptt.release handlers are not engine-gated, so PTT is valid on the
        // cascaded engine too.
        guard pttOnMode else { return }
        pttHeld = true
        guard phase == .ready, !pttEngageSent else { return }
        pttEngageSent = true
        pttStartedAt = ProcessInfo.processInfo.systemUptime
        send(kind: "ptt.engage", payload: [:])
        if concurrentRequests {
            onConcurrentEvent?("speech.started", [:])
            send(kind: "speech.started", payload: [:])
        }
    }

    /// Release push-to-talk (close the mic gate / commit the turn).
    func releasePTT() {
        guard pttOnMode else { return }
        pttHeld = false
        guard phase == .ready, pttEngageSent else { return }
        pttEngageSent = false
        let discarded = concurrentRequests && ProcessInfo.processInfo.systemUptime - pttStartedAt < 0.18
        send(kind: discarded ? "input.clear" : "ptt.release", payload: [:])
        if concurrentRequests {
            concurrentResponsePending = !discarded && engineMode != .handsFree
            onConcurrentEvent?(discarded ? "input.cleared" : "speech.stopped", [:])
        }
    }

    /// Change Open mic/Hold to talk without replacing the registered media
    /// session, control WebSocket, or local audio graph. Realtime providers may
    /// still request an upstream audio rebind because some vendors latch VAD at
    /// setup time; that rebind remains inside this live control connection.
    func setTurnBoundary(pttOn: Bool) {
        guard pttOn != pttOnMode else { return }

        let wasEngaged = pttEngageSent
        pttOnMode = pttOn
        pttHeld = false
        pttEngageSent = false

        // Finish a held utterance before changing authority so it cannot be
        // stranded in the old provider input buffer.
        if wasEngaged {
            send(kind: "ptt.release", payload: [:])
            if concurrentRequests { onConcurrentEvent?("speech.stopped", [:]) }
        } else if pttOn {
            send(kind: "input.clear", payload: [:])
            concurrentResponsePending = false
            if concurrentRequests { onConcurrentEvent?("input.cleared", [:]) }
        }

        // Cascaded Hands-free uses the iPhone's local PCM gate for this choice;
        // the backend deliberately ignores realtime turn-boundary updates there.
        guard engineMode == .realtime, webSocket != nil else { return }
        switch phase {
        case .connecting, .reconnecting, .ready, .rotating:
            send(
                kind: "session.turn_boundary",
                payload: RealtimeVoiceProtocol.turnBoundaryPayload(pttOn: pttOn)
            )
        case .idle, .ended, .failed:
            break
        }
    }

    /// End the call: signal the server, then tear down the transport and
    /// clear state.
    func end() {
        if webSocket != nil {
            send(kind: "session.end", payload: [:])
        }
        teardown(finalPhase: .ended)
    }

    // MARK: - Registration

    /// `POST /api/magician/v2/media/sessions` — register a hands-free
    /// voice surface and return the `session_id` the control WS keys on.
    private func register() async throws -> String {
        let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/sessions")!
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)

        // Body mirrors the web `registerSession()` shape (session.ts).
        // This is a native iOS *voice* surface: mobile web is the closest
        // `SurfaceType` the backend models today, transport is a live WS
        // (not SSE), and we advertise the mic + realtime-voice capability
        // so the registry routes a live call rather than a text surface.
        let audio = AudioSettings.shared
        let body = RealtimeVoiceProtocol.mediaSessionRegistrationBody(
            includeHandsFreeAudio: engineMode == .handsFree,
            audioProfile: audio.requestProfile(for: .handsFree),
            audioStageOptions: audio.requestStageOptions(for: .handsFree)
        )
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        // Instrumentation only — see `VoiceConnectTrace`. Stamped either side of
        // the round trip and nowhere else in between, so `post>post_ok` is the
        // network and the backend's registry, with no parsing folded into it.
        VoiceConnectTracer.shared.mark(.post)
        let (data, response) = try await urlSession.data(for: request)
        VoiceConnectTracer.shared.mark(.postOK)
        guard let http = response as? HTTPURLResponse else {
            throw VoiceTransportError("No HTTP response from media session registry.")
        }
        guard (200..<300).contains(http.statusCode) else {
            let detail = String(data: data, encoding: .utf8) ?? ""
            throw VoiceTransportError("Media session registration failed (\(http.statusCode)). \(detail)")
        }

        // The backend returns `{ "session": { "session_id": …, … } }`
        // (media_api.rs `SessionEnvelope`). Read the nested id, with a
        // top-level fallback for forward-compat.
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        if let session = json?["session"] as? [String: Any],
           let id = session["session_id"] as? String {
            return id
        }
        if let id = json?["session_id"] as? String {
            return id
        }
        throw VoiceTransportError("Media session response missing session_id.")
    }

    /// iOS registers a call-scoped media session rather than reusing a page
    /// session. Disconnect it when the call ends so retries cannot accumulate
    /// stale registry entries and eventually hit the session limit.
    private func disconnectRegisteredSession(_ id: String) {
        let url = URL(
            string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/media/sessions/\(id)?revoke=false"
        )!
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        urlSession.dataTask(with: request).resume()
    }

    // MARK: - Control WS

    private func openControl(sessionId: String, uiThreadId: String) {
        let url = URL(string: "\(MagicianAccess.webSocketBaseURL.absoluteString)/api/magician/v2/media/voice/\(sessionId)/control")!
        var request = URLRequest(url: url)
        MagicianAccess.authorizeWebSocket(
            &request,
            applicationProtocols: ["magician-voice-control-v1"]
        )

        let session = URLSession(configuration: .default, delegate: nil, delegateQueue: .main)
        socketSession = session
        let ws = session.webSocketTask(with: request)
        webSocket = ws
        audioSocket.withLock { $0 = ws }
        // Instrumentation only — see `VoiceConnectTrace`.
        VoiceConnectTracer.shared.mark(.ws)
        ws.resume()

        // Send the start handshake. The routed `session.ready` reply
        // promotes us to `.ready` and stamps `startedAt`.
        var payload = RealtimeVoiceProtocol.startPayload(
            uiThreadId: uiThreadId,
            realtimeProfile: realtimeProfile,
            engine: engineMode,
            pttOn: pttOnMode,
            requireVoicePrefix: addressing.required,
            screenLocked: DeviceScreenLock.isLocked
        )
        payload["concurrent_requests"] = concurrentRequests
        ws.send(.string(RealtimeVoiceProtocol.envelopeText(kind: "session.start", payload: payload))) { [weak self] error in
            // Instrumentation only — see `VoiceConnectTrace`. This completing
            // without an error IS the socket being up: `URLSessionWebSocketTask`
            // queues a send until the HTTP upgrade finishes, so nothing extra has
            // to be wired (no delegate, no second observer) to time the handshake.
            // Stamped on THIS queue at this instant rather than after the main-actor
            // hop below, so the measurement never includes the scheduler.
            if error == nil { VoiceConnectTracer.shared.mark(.wsUp) }
            if let error {
                Task { @MainActor in
                    guard let self, self.webSocket === ws else { return }
                    self.handleSendError(error)
                }
            }
        }

        if concurrentRequests { selectConcurrentContext(selectedConcurrentContext) }
        receive(on: ws)
    }

    private func receive(on ws: URLSessionWebSocketTask) {
        ws.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.webSocket === ws else { return }
                switch result {
                case .success(let message):
                    self.handle(message)
                    // Re-arm the receive loop only while the socket is live.
                    if self.webSocket === ws {
                        self.receive(on: ws)
                    }
                case .failure(let error):
                    self.handleReceiveError(error)
                }
            }
        }
    }

    private func handle(_ message: URLSessionWebSocketTask.Message) {
        switch message {
        case .data(let bytes):
            concurrentResponsePending = true
            onIncomingAudio?(bytes)
        case .string(let text):
            handleControl(text: text)
        @unknown default:
            break
        }
    }

    /// Internal so protocol/lifecycle tests can drive the exact control-event
    /// path without opening a socket.
    func handleControl(text: String) {
        guard
            let data = text.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let kind = object["kind"] as? String
        else {
            return
        }
        let payload = (object["payload"] as? [String: Any]) ?? [:]
        if concurrentRequests {
            switch kind {
            case "speech.started": concurrentResponsePending = false
            case "speech.stopped": concurrentResponsePending = engineMode != .handsFree
            case "audio.output.ended", "response.interrupted", "voice.request.accepted", "session.ended": concurrentResponsePending = false
            case "interaction.status": concurrentResponsePending = payload["status"] as? String == "in_progress"
            default: break
            }
            onConcurrentEvent?(kind, payload)
        }
        let event = RealtimeVoiceProtocol.route(kind: kind, payload: payload)

        if case .transcript(.user, _, _, _) = event {
            guidedFlowNoticeMessage = nil
        }

        if captionState.apply(event) {
            captions = captionState.captions
            return
        }

        switch event {
        case .ready:
            // Instrumentation only, and the FIRST line of the case so `ws_up>ready`
            // is the backend alone — provider-session creation, prompt/tool setup,
            // resume-context compaction — with none of the client-side application
            // below folded into it. See `VoiceConnectTrace`.
            VoiceConnectTracer.shared.mark(.ready)
            addressing = RealtimeVoiceProtocol.addressing(from: payload)
            boundary = RealtimeVoiceProtocol.boundary(from: payload)
            cancelConnectTimeout()
            phase = .ready
            assistantWorking = false
            // The connect→ready transition: from here a drop is mid-call, and
            // the counter resets so the mid-call budget starts whole.
            hasBeenReadyThisCall = true
            reconnectAttempt = 0
            startedAt = Date()
            if pttOnMode, pttHeld, !pttEngageSent {
                pttEngageSent = true
                pttStartedAt = ProcessInfo.processInfo.systemUptime
                send(kind: "ptt.engage", payload: [:])
                if concurrentRequests {
                    onConcurrentEvent?("speech.started", [:])
                    send(kind: "speech.started", payload: [:])
                }
            }
            // Last, so anything it sends follows a PTT engage the user was already
            // holding, and so it runs against fully applied ready state.
            onReady?()
        case .rotating:
            phase = .rotating
        case .rebind:
            // Rotation completed on a fresh upstream — audio keeps flowing on the
            // same WS, so restore `.ready` (else the panel stays "Refreshing…"
            // for the rest of the call).
            onRebind?()
            if phase == .rotating { phase = .ready }
        case .ended:
            teardown(finalPhase: .ended)
        case .error(let message, let recoverable):
            // Recoverable errors (e.g. a transient tool failure) must not kill the
            // call — surface the message but keep it live, matching the web guard.
            if recoverable {
                errorMessage = message
            } else {
                fail(with: message)
            }
        case .transcript(_, _, _, _),
             .transcriptPartial(_, _, _, _),
             .transcriptCleared(_, _, _),
             .transcriptIgnored(_, _, _):
            // Handled by RealtimeVoiceCaptionState above.
            break
        case .toolResult:
            // Surfaced elsewhere; nothing to reflect in call state here.
            break
        case .tutorBlackboardRequested(let text, let quick):
            onTutorBlackboardRequested?(text, quick)
        case .guidedFlowRejected(let message, let backendAnnounced):
            guidedFlowNoticeMessage = message
            onGuidedFlowRejected?(message, backendAnnounced)
        case .assistantAudioEnded(let interrupted):
            if interrupted { assistantWorking = false }
            onAssistantAudioEnded?(interrupted)
        case .interactionStatus(let inProgress):
            if assistantWorking != inProgress { assistantWorking = inProgress }
        case .ignore:
            break
        }
    }

    // MARK: - Outgoing helpers

    private func send(kind: String, payload: [String: Any]) {
        guard let ws = webSocket else { return }
        ws.send(.string(RealtimeVoiceProtocol.envelopeText(kind: kind, payload: payload))) { [weak self] error in
            if let error {
                Task { @MainActor in
                    guard let self, self.webSocket === ws else { return }
                    self.handleSendError(error)
                }
            }
        }
    }

    private func reportScreenState(locked: Bool) {
        guard webSocket != nil else { return }
        switch phase {
        case .connecting, .reconnecting, .ready, .rotating:
            send(
                kind: "screen.state",
                payload: RealtimeVoiceProtocol.screenStatePayload(locked: locked)
            )
        case .idle, .ended, .failed:
            break
        }
    }

    // MARK: - Error / teardown

    private func handleSendError(_ error: Error) {
        // A send failure after we've already ended is expected — the
        // socket is torn down. Only surface it while the call is live.
        guard phase != .ended, phase != .failed else { return }
        scheduleReconnect(message: error.localizedDescription)
    }

    private func handleReceiveError(_ error: Error) {
        guard phase != .ended, phase != .failed else { return }
        scheduleReconnect(message: "Voice control connection lost.")
    }

    private func scheduleReconnect(message: String) {
        guard let sessionId, let activeUiThreadId else {
            fail(with: message)
            return
        }
        // Bootstrap and mid-call are different situations — see the two budgets.
        let delays = hasBeenReadyThisCall ? reconnectDelays : connectReconnectDelays
        guard reconnectAttempt < delays.count else {
            fail(with: "Voice control connection could not be restored.")
            return
        }
        reconnectWork?.cancel()
        cancelConnectTimeout()
        audioSocket.withLock { $0 = nil }
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil
        socketSession?.invalidateAndCancel()
        socketSession = nil
        phase = .reconnecting
        errorMessage = "Connection interrupted. Reconnecting…"
        let generation = startGeneration
        let delay = delays[reconnectAttempt]
        reconnectAttempt += 1
        let work = DispatchWorkItem { [weak self] in
            guard let self,
                  self.startGeneration == generation,
                  self.phase == .reconnecting,
                  self.sessionId == sessionId else { return }
            self.armConnectTimeout()
            self.openControl(sessionId: sessionId, uiThreadId: activeUiThreadId)
        }
        reconnectWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    private func fail(with message: String) {
        errorMessage = message
        teardown(finalPhase: .failed)
    }

    private func teardown(finalPhase: Phase) {
        // Instrumentation only — see `VoiceConnectTrace`. EVERY way a connect can
        // stop without reaching ready lands here — a registration error, an
        // exhausted reconnect backoff, the 45 s watchdog, a disarm, the orb's
        // button, a server `session.end` — so this is the one place that can name
        // the stage it died at. `errorMessage` is already set by `fail(with:)`
        // before it calls this. A no-op after a successful connect, whose line was
        // printed at the flush.
        VoiceConnectTracer.shared.finishTerminal(
            failed: finalPhase == .failed,
            error: errorMessage
        )
        cancelConnectTimeout()
        reconnectWork?.cancel()
        reconnectWork = nil
        startGeneration += 1
        let registeredSessionId = sessionId
        audioSocket.withLock { $0 = nil }
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil
        socketSession?.invalidateAndCancel()
        socketSession = nil
        sessionId = nil
        activeUiThreadId = nil
        pttHeld = false
        pttEngageSent = false
        captionState.clearUnfinishedUserCaptions()
        captions = captionState.captions
        if let registeredSessionId {
            disconnectRegisteredSession(registeredSessionId)
        }
        phase = finalPhase
    }
}

private struct VoiceTransportError: Error {
    let message: String
    init(_ message: String) { self.message = message }
}
