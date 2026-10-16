import AVFoundation
import Combine
import Foundation
import QuartzCore

/// Drives in-app microphone capture for a "Listen here" observation session:
/// the phone mic → 16 kHz mono PCM16 → `ObservationUploadPump` → the backend's
/// `capture: "client"` meeting pipeline.
///
/// Device-only: `AVAudioEngine` mic capture cannot run in the simulator. The
/// pure chunking/upload logic lives in `Shared` (`ObservationChunkAccumulator`,
/// `ObservationUploadPump`) and is unit-tested there; this class is the
/// `AVAudioSession`/`AVAudioEngine` glue and lifecycle (start / pause / resume /
/// stop / interruption).
///
/// A single phone mic captures the whole room, so audio is pushed on the
/// diarized `primary` channel (never the hard-"You" `mic` track), and the listen
/// session is started with `mic:false`.
final class ListenController: ObservableObject {
    /// One in-app capture at a time — shared so the Observe surface and the
    /// root-level "now observing" mini-bar drive the same session.
    static let shared = ListenController()

    enum State: Equatable {
        case idle
        case starting
        case listening(threadId: String)
        case paused(threadId: String)
        case ended(threadId: String?, reason: String)
        case error(String)

        var threadId: String? {
            switch self {
            case .listening(let t), .paused(let t): return t
            case .ended(let t, _): return t
            default: return nil
            }
        }
        var isActive: Bool {
            if case .listening = self { return true }
            if case .paused = self { return true }
            return false
        }
    }

    @Published private(set) var state: State = .idle
    /// The live session id, mirrored so the Observe surface can dedupe this
    /// in-app session against the server's `/meetings/active` list. Nil unless
    /// listening or paused.
    @Published private(set) var activeSessionId: String?
    /// True when the last start reconnected to an already-live session for this
    /// thread (the server returned `reused`) rather than opening a fresh one.
    @Published private(set) var reused = false
    /// Smoothed mic input level (0…1) for the live waveform. Updated ~12 Hz while
    /// listening; 0 when paused/stopped.
    @Published private(set) var audioLevel: Float = 0
    /// When the current capture started — drives the live elapsed timer.
    @Published private(set) var startedAt: Date?

    /// Audio-thread throttle for the level meter.
    private var lastLevelAt: CFTimeInterval = 0

    private let client: ObservationUplinkClient
    private let targetFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32,
        sampleRate: 16_000,
        channels: 1,
        interleaved: false
    )!

    // Audio-thread state (touched only inside the serialized tap callback).
    private var engine: AVAudioEngine?
    private var converter: AVAudioConverter?
    private var accumulator = ObservationChunkAccumulator(chunkSeconds: 6)

    private var pump: ObservationUploadPump?
    private var session: ObservationSession?

    /// The armed-ambient-window rail. The reverse direction already existed —
    /// `AmbientController.arm` refuses while an observation owns the microphone —
    /// and this is the missing half: an observation started while a window is armed
    /// would take the input from under the ambient tap, reconfigure the shared
    /// session without `.allowBluetoothHFP` (a deliberate difference, see
    /// `startEngine`), and then deactivate it on the way out. See `AmbientRail`.
    var ambientRail = AmbientRail.live

    /// Deactivations actually issued. See `stopEngine`.
    private(set) var sessionReleaseCount = 0

    init(client: ObservationUplinkClient = ObservationUplinkClient()) {
        self.client = client
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
    }

    // MARK: lifecycle

    @MainActor
    func start(title: String?, url: String?) async {
        switch state {
        case .idle, .error, .ended: break
        default: return  // already starting / listening / paused
        }
        state = .starting
        do {
            let session = try await client.startSession(title: title, url: url, mic: false)
            self.session = session
            self.reused = session.reused
            let pump = ObservationUploadPump(
                client: client,
                sessionId: session.sessionId,
                token: session.uploadToken,
                channel: "primary"
            )
            await pump.setOnEnded { [weak self] in
                Task { @MainActor in self?.serverEnded() }
            }
            self.pump = pump
            ObservationArm(
                sessionId: session.sessionId,
                uploadToken: session.uploadToken,
                threadId: session.threadId,
                micEnabled: false
            ).save()
            try startEngine()
            state = .listening(threadId: session.threadId)
            activeSessionId = session.sessionId
            startedAt = Date()
            ObservationActivity.shared.start(
                title: title ?? "",
                kind: "mic",
                sessionId: session.sessionId,
                threadId: session.threadId
            )
        } catch ObservationUplinkError.alreadyObserved(let existing) {
            teardown()
            state = .error("This meeting is already being observed elsewhere (\(existing)).")
        } catch {
            teardown()
            state = .error("Couldn't start listening. \(Self.describe(error))")
        }
    }

    @MainActor
    func stop() async {
        let thread = session?.threadId
        let sessionId = session?.sessionId
        let activePump = pump
        stopEngine()
        let finalChunk = accumulator.flush()
        ObservationArm.clear()
        session = nil
        pump = nil
        activeSessionId = nil
        startedAt = nil
        audioLevel = 0
        ObservationActivity.shared.end()
        state = .ended(threadId: thread, reason: "stopped")
        await activePump?.finish(finalChunk: finalChunk)
        if let sessionId {
            await client.stopSession(sessionId: sessionId)
        }
    }

    // MARK: interruption (incoming call / another app takes the mic)

    @objc private func handleInterruption(_ note: Notification) {
        guard
            let info = note.userInfo,
            let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        switch type {
        case .began:
            Task { @MainActor in self.pauseForInterruption() }
        case .ended:
            let options = (info[AVAudioSessionInterruptionOptionKey] as? UInt)
                .map(AVAudioSession.InterruptionOptions.init(rawValue:)) ?? []
            Task { @MainActor in self.resumeAfterInterruption(shouldResume: options.contains(.shouldResume)) }
        @unknown default:
            break
        }
    }

    @MainActor
    private func pauseForInterruption() {
        guard case .listening(let thread) = state else { return }
        stopEngine()
        audioLevel = 0
        state = .paused(threadId: thread)
        ObservationActivity.shared.update(phase: "paused")
    }

    @MainActor
    private func resumeAfterInterruption(shouldResume: Bool) {
        guard case .paused(let thread) = state else { return }
        // Only auto-resume when the system says it's safe; otherwise leave paused
        // for a one-tap resume from the Observe surface.
        guard shouldResume, session != nil else { return }
        do {
            try startEngine()
            state = .listening(threadId: thread)
            ObservationActivity.shared.update(phase: "listening")
        } catch {
            state = .error("Couldn't resume after the interruption. \(Self.describe(error))")
        }
    }

    /// Manual resume (the "Resume" affordance) after a non-auto-resumable pause.
    @MainActor
    func resume() {
        resumeAfterInterruption(shouldResume: true)
    }

    /// Return to the idle state from a terminal one so the surface can offer a
    /// fresh "Listen here".
    @MainActor
    func reset() {
        switch state {
        case .ended, .error: state = .idle
        default: break
        }
    }

    /// Reconnect to a session we armed before the app was backgrounded or killed,
    /// if the server still lists it as live. Rebuilds the pump + mic engine
    /// against the stored session id + upload token and resumes pushing. If the
    /// armed session is gone (it idle-timed-out while we were away), the stale
    /// arm is cleared. Called on the Observe surface once the active list loads.
    ///
    /// A broadcast arm (`micEnabled`, created by "Prepare session") belongs to the
    /// ReplayKit extension, which captures app audio + mic itself: the in-app mic
    /// must not start for it (that would double-capture the room into the
    /// diarized track and show a mic Live Activity for a screen capture). Returns
    /// the live broadcast session id in that case so the Observe surface can keep
    /// showing it as prepared.
    @MainActor
    @discardableResult
    func reattachIfArmed(activeSessions: [ActiveMeeting]) async -> String? {
        guard case .idle = state else { return nil }
        let arm = ObservationArm.claim()
        switch ObserveCaptureRules.reattach(arm: arm, activeSessionIds: activeSessions.map(\.sessionId)) {
        case .none:
            return nil
        case .clearStale:
            ObservationArm.clear()  // stale — the session ended while we were away
            return nil
        case .broadcastLive(let sessionId):
            return sessionId
        case .resumeMic:
            break
        }
        guard let arm else { return nil }
        let session = ObservationSession(
            sessionId: arm.sessionId,
            uploadToken: arm.uploadToken,
            threadId: arm.threadId,
            reused: true
        )
        self.session = session
        self.reused = true
        let pump = ObservationUploadPump(
            client: client,
            sessionId: arm.sessionId,
            token: arm.uploadToken,
            channel: "primary"
        )
        await pump.setOnEnded { [weak self] in
            Task { @MainActor in self?.serverEnded() }
        }
        self.pump = pump
        do {
            try startEngine()
            state = .listening(threadId: arm.threadId)
            activeSessionId = arm.sessionId
            startedAt = Date()
            // Adopt the Live Activity that survived the app being backgrounded.
            // Only in-app mic sessions reach here (broadcast arms return above).
            ObservationActivity.shared.start(
                title: "",
                kind: "mic",
                sessionId: arm.sessionId,
                threadId: arm.threadId
            )
        } catch {
            teardown()
            ObservationArm.clear()
            state = .idle
        }
        return nil
    }

    /// Reconcile a locally-held capture with the server's canonical live set.
    /// This closes paused sessions too, which cannot discover a remote stop via
    /// the upload pump because they are not sending audio chunks.
    @MainActor
    func reconcileServerPresence(activeSessions: [ActiveMeeting]) {
        guard let activeSessionId else { return }
        guard !activeSessions.contains(where: { $0.sessionId == activeSessionId }) else { return }
        serverEnded()
    }

    // MARK: engine

    /// Take the microphone for an observation session.
    ///
    /// **The ambient rail is here rather than in `start`, because this is every
    /// path that takes the input**: a fresh start, a reattach after the app was
    /// away, and a resume after an interruption all come through this one function,
    /// and all three would otherwise collide with an armed window. Placing it here
    /// also puts the yield immediately before the session is reconfigured, so a
    /// `start` whose server call failed never costs the user their window.
    ///
    /// Observation wins, which is design §9's ranking read in the other direction:
    /// that table refuses *arming* while an observation is live, and the same
    /// judgement — a deliberate, foreground recording of a meeting outranks a
    /// background convenience — says the observation proceeds here and the ambient
    /// window ends with a reason.
    @MainActor
    private func startEngine() throws {
        ambientRail.yield(AmbientYieldReason.observationStarted)
        let audioSession = AVAudioSession.sharedInstance()
        // Keep capture on the built-in mics: a "phone on the table" listens to the
        // whole room, so we deliberately do NOT route to a Bluetooth headset (its
        // near-field, low-quality HFP mic would miss the room).
        try audioSession.setCategory(
            .playAndRecord,
            mode: .spokenAudio,
            options: [.defaultToSpeaker]
        )
        try audioSession.setActive(true)

        let engine = AVAudioEngine()
        let input = engine.inputNode
        let inputFormat = input.outputFormat(forBus: 0)
        converter = AVAudioConverter(from: inputFormat, to: targetFormat)
        input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] buffer, _ in
            self?.handleTap(buffer)
        }
        engine.prepare()
        try engine.start()
        self.engine = engine
    }

    /// Give the microphone and the shared session back — **the session only if no
    /// ambient window is armed.**
    ///
    /// The backstop underneath `startEngine`'s yield, and it covers a case the
    /// yield cannot: this runs from `stop()`, which has no state guard, so a tap on
    /// "Stop" against an already-`ended` session deactivates the shared session
    /// with no `startEngine` ever having run — and therefore with an ambient window
    /// that was legitimately armed afterwards (`AmbientController.arm` admits
    /// `.ended`, since it is not `isActive`). A deactivated session cannot be
    /// reactivated from the background, so that tap would kill the window silently,
    /// at the next wake word, off screen (Apple DTS 826462).
    ///
    /// `@MainActor` for the rail; every caller already was. `internal` and counted for
    /// the reason `DictationController.releaseSession` gives: "it did not deactivate"
    /// is not observable through any `AVAudioSession` API, so a private backstop with
    /// no counter is one nothing can prove exists.
    @MainActor
    func stopEngine() {
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
        engine = nil
        converter = nil
        guard !ambientRail.windowIsLive() else { return }
        sessionReleaseCount += 1
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// Tap callback — audio thread, serialized by AVAudioEngine. Resample to
    /// 16 kHz mono, encode PCM16-LE, accumulate into 6 s chunks, hand each to the
    /// pump.
    private func handleTap(_ buffer: AVAudioPCMBuffer) {
        guard let converter, buffer.frameLength > 0 else { return }
        let ratio = targetFormat.sampleRate / buffer.format.sampleRate
        let outCapacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1024
        guard let out = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: outCapacity) else {
            return
        }
        var consumed = false
        var err: NSError?
        converter.convert(to: out, error: &err) { _, status in
            if consumed {
                status.pointee = .noDataNow
                return nil
            }
            consumed = true
            status.pointee = .haveData
            return buffer
        }
        guard err == nil, out.frameLength > 0, let ch = out.floatChannelData?[0] else { return }
        publishLevel(from: ch, count: Int(out.frameLength))
        let pcm = VoicePCM.floatToInt16LE(UnsafeBufferPointer(start: ch, count: Int(out.frameLength)))
        let chunks = accumulator.append(pcm)
        guard !chunks.isEmpty, let pump else { return }
        for chunk in chunks {
            Task { await pump.submit(chunk) }
        }
    }

    /// Compute a smoothed RMS level from the resampled buffer (audio thread) and
    /// publish it ~12 Hz for the live waveform.
    private func publishLevel(from ch: UnsafeMutablePointer<Float>, count: Int) {
        guard count > 0 else { return }
        let now = CACurrentMediaTime()
        guard now - lastLevelAt >= 0.08 else { return }
        lastLevelAt = now
        var sum: Float = 0
        for i in 0..<count { let s = ch[i]; sum += s * s }
        let rms = (sum / Float(count)).squareRoot()
        // Speech RMS is roughly 0…0.3; scale to 0…1 with headroom.
        let level = min(1, rms * 6)
        Task { @MainActor in self.setLevel(level) }
    }

    @MainActor
    private func setLevel(_ level: Float) {
        // Attack fast, release slow — a lively but not jittery meter.
        audioLevel = max(level, audioLevel * 0.82)
    }

    // MARK: helpers

    @MainActor
    private func serverEnded() {
        guard state.isActive else { return }
        let thread = session?.threadId
        let activePump = pump
        stopEngine()
        _ = accumulator.flush()
        Task { await activePump?.stop() }
        ObservationArm.clear()
        session = nil
        pump = nil
        activeSessionId = nil
        startedAt = nil
        audioLevel = 0
        ObservationActivity.shared.end()
        state = .ended(threadId: thread, reason: "ended_by_server")
    }

    /// Tear down the engine + session/pump state without changing the published
    /// state (callers set the terminal/error state themselves).
    ///
    /// `@MainActor` because `stopEngine` now is; both callers already were.
    @MainActor
    private func teardown() {
        let activePump = pump
        stopEngine()
        _ = accumulator.flush()
        Task { await activePump?.stop() }
        ObservationArm.clear()
        session = nil
        pump = nil
    }

    private static func describe(_ error: Error) -> String {
        switch error {
        case ObservationUplinkError.offline: return "You appear to be offline."
        case ObservationUplinkError.unauthorized: return "Open Magican once to finish setup."
        default: return ""
        }
    }
}
