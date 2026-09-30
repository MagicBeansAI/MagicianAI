import Foundation
import Combine
import os

/// Focus flag held for the whole lifetime of a realtime Live call. While it is
/// active, chat auto-speak must NOT run its on-device `SpeechSynthesizer`: the
/// realtime model already speaks its own reply (a second voice would double the
/// audio), and `SpeechSynthesizer` also seizes the shared `AVAudioSession`
/// (`.playback` then deactivate), which would kill the still-running capture
/// engine and leave the next turn's mic dead. Mirrors `TutorAudioFocus`.
final class VoiceCallAudioFocus {
    static let shared = VoiceCallAudioFocus()

    private let lock = NSLock()
    private var owners: Set<UUID> = []

    private init() {}

    var isActive: Bool {
        lock.lock()
        defer { lock.unlock() }
        return !owners.isEmpty
    }

    func acquire() -> UUID {
        let owner = UUID()
        lock.lock()
        owners.insert(owner)
        lock.unlock()
        return owner
    }

    func release(_ owner: UUID) {
        lock.lock()
        owners.remove(owner)
        lock.unlock()
    }
}

/// Which surface a `VoiceCallViewModel` serves, and therefore three things the
/// call stack cannot infer for itself.
///
/// One type rather than three flags, because all three consequences are the
/// same fact — *is a human looking at a call panel, or is this an ambient wake
/// handoff?* — and splitting them would let a future call site set two of the
/// three and produce a live microphone with no way to speak into it.
///
/// Only one of the three is answerable from the mode alone. The session
/// disposition also needs to know whether an ambient window is live, so it is a
/// function of the rail rather than a property — see `sessionDisposition`.
enum VoiceCallMode: Equatable {
    /// The in-app Live call behind `VoiceCallPanel`. The user's persisted
    /// microphone mode applies (they have a button to hold), and the shared
    /// `AVAudioSession` is released when the call ends.
    case inApp
    /// A call started by the ambient wake handoff, with no panel and no user
    /// watching.
    case ambient

    /// Who owns the shared session for this call — **and the answer depends on
    /// whether an ambient window is live, not on the mode alone.**
    ///
    /// `.ambient` is unconditional: the window owns the session, so the call is a
    /// guest at both ends. See `VoiceSessionDisposition`.
    ///
    /// **`.inApp` is `.keepActive` while a window is live, and that is a rail rather
    /// than a special case.** The in-app Live call is the fourteenth session site,
    /// and it was reachable straight through the front door: arm a two-hour window,
    /// tap Live, hang up, and `.release` deactivated the window's session from a
    /// place nothing could reactivate it — the window died at the next wake word
    /// with no cause the user could connect it to. This is design §15's fourth
    /// correction in its purest form: the `VoiceAudioEngine.stop()` row is marked
    /// RESOLVED, and it was resolved for *the caller that found it*.
    ///
    /// **It does not YIELD, and that is the interesting part of the decision.**
    /// Dictation and observation yield (design §17) because they take the microphone
    /// for a *different purpose*, so ending the window is honest. An in-app call is
    /// the **same** purpose — the same user talking to the same agent — started
    /// *inside* a window they deliberately armed. Ending it because they used the app
    /// is the wrong outcome. So the window survives the call, and nothing about the
    /// call changes: `.release` exists so the session is handed back politely and the
    /// user's music resumes, and while a window is armed the session is staying
    /// active regardless, so not releasing costs nothing.
    ///
    /// That also collapses the general form rather than one instance of it. The
    /// question stops being *"which caller am I?"* and becomes *"is a window
    /// live?"* — the same shape as the other four rails, and no new
    /// `AmbientYieldReason`.
    ///
    /// **The rail is a closure so `.ambient` never consults it.** `AmbientRail`'s
    /// production `windowIsLive` reads `AmbientController.shared`, and constructing
    /// that controller means a Vosk spotter, a microphone engine and a call stack;
    /// the ambient answer does not depend on the rail, so it must not pay for it or
    /// appear to. That independence is asserted directly — see
    /// `testAnAmbientCallDoesNotEvenConsultTheRail`.
    func sessionDisposition(ambientWindowIsLive: () -> Bool) -> VoiceSessionDisposition {
        switch self {
        case .ambient: return .keepActive
        case .inApp: return ambientWindowIsLive() ? .keepActive : .release
        }
    }

    /// Whether the persisted hold-to-talk preference applies.
    ///
    /// It must not in `.ambient`: hold-to-talk mutes local capture *and* asks the
    /// provider for a `push_to_talk` turn boundary, and an ambient conversation
    /// has no button to hold. A user whose stored preference is on — or whose
    /// realtime profile `defaultsToPushToTalk`, which seeds the same stored value
    /// — would get an orb reporting a live conversation into a muted microphone,
    /// with no turn ever committed. That is the indicator lying about whether
    /// they are being heard.
    var appliesStoredHoldToTalk: Bool { self == .inApp }

    /// Whether starting a call should hush chat narration first.
    ///
    /// `.inApp` does: a voice session owns reply audio, and a narrated chat reply
    /// would talk over the model. `.ambient` must NOT, and the decision is
    /// unchanged — but **the original reason for it has since been fixed and this
    /// comment is the corrected one.**
    ///
    /// It used to read that `SpeechSynthesizer.stop()` routes through `finish()`,
    /// which deactivated the shared session *unconditionally, even when nothing was
    /// speaking* — so hushing on the ambient handoff would have deactivated the
    /// armed window's session from the background, where it cannot be reactivated.
    /// That defect is gone: `finish()` now deactivates only if `configureSession()`
    /// actually ran. Leaving the stale justification here is how a guard gets
    /// "tidied away" later by someone who checks the stated reason, finds it no
    /// longer true, and concludes the guard is obsolete.
    ///
    /// **Two reasons remain, and either is sufficient.** First, there is nothing to
    /// hush: `AmbientController` holds `VoiceCallAudioFocus` for the whole armed
    /// window, and `SpeechSynthesizer.speak` refuses while that token is held.
    /// Second, and the one that would still bite: a hush is not a no-op against an
    /// utterance already in the air. `stop()` cancels it — cutting off a reply the
    /// user asked for and was listening to — purely so the ambient call could take a
    /// microphone it was going to take anyway.
    var hushesChatNarration: Bool { self == .inApp }
}

/// The Live voice call's presentation model (Task 5).
///
/// Sits between the two device-verified halves of the call — the transport
/// (`RealtimeVoiceClient`) and the full-duplex audio core (`VoiceAudioEngine`)
/// — and the floating call UI (`VoiceCallPanel`). It owns both, wires their
/// audio paths to each other, and re-publishes exactly what the panel renders
/// (status text, captions, the mic level for the orb, an mm:ss timer, and the
/// active microphone control).
///
/// ## Audio wiring (the whole point of this class)
/// A live call is full-duplex, so audio flows both directions and each
/// direction crosses the transport↔engine boundary in `startCall`:
///
///  - **Downstream (provider → speaker):** `client.onIncomingAudio` fires with
///    each downstream PCM frame; we forward it to `engine.play(_:)`.
///  - **Upstream (mic → provider):** `engine.start(onFrameOut:)`'s callback
///    fires with each captured 24 kHz frame; we forward it to
///    `client.sendAudio(_:)`.
///
/// Neither half knows about the other — this ViewModel is the only place the
/// two are joined, which keeps the transport and the audio engine independently
/// unit-testable / device-verifiable.
@MainActor
final class VoiceCallViewModel: ObservableObject {

    /// Which surface this instance serves. `let` and required at `init`, so the
    /// answer is fixed for the call's whole lifetime — every teardown path,
    /// including the ones a terminal server event drives rather than the user,
    /// reads the same value.
    let mode: VoiceCallMode

    /// The same injected rail `DictationController`, `ListenController`,
    /// `BackgroundEngine` and `SpeechSynthesizer` consult before they touch the
    /// shared session — deliberately the same seam rather than a parallel one, so
    /// there is one answer to "is a window live?" in the app.
    ///
    /// `var` with a production default for the reason `AmbientRail` states: a test
    /// swaps the whole rail, and reaching for `AmbientController.shared` inline would
    /// make every call test construct a Vosk spotter to answer one boolean.
    var ambientRail = AmbientRail.live

    /// What this call is entitled to do to the shared session, **asked at the moment
    /// it matters rather than latched at `startCall`.**
    ///
    /// Deliberately re-read on every teardown and on every start. A window can open
    /// *during* an in-app call — Settings has an arm control, and the call panel does
    /// not block it — and a snapshot taken at `startCall` would then release a
    /// session the new window depends on. Design §19's second lesson is exactly this:
    /// a snapshot latch is not a fix if the deferred path can change the answer under
    /// it. The reverse case is equally correct: a window whose leash expires
    /// mid-call leaves nothing needing the session, and the hangup releases it
    /// politely as it always did.
    var sessionDisposition: VoiceSessionDisposition {
        mode.sessionDisposition(ambientWindowIsLive: { ambientRail.windowIsLive() })
    }

    // MARK: - Owned collaborators

    /// The transport. `@Published` so the panel re-renders on phase/caption
    /// changes (its own `@Published` members drive `objectWillChange`, which we
    /// re-broadcast via the sink below).
    @Published private(set) var client = RealtimeVoiceClient()

    /// The full-duplex audio core. Not itself published (the panel reads the
    /// mic `level` we mirror below), but retained for the call's lifetime so
    /// capture/playback stay live.
    let engine = VoiceAudioEngine()

    // MARK: - Panel-facing published state

    /// Mic RMS (0…1), mirrored from the engine — drives the orb.
    @Published private(set) var level: Float = 0
    /// mm:ss elapsed since the call connected, refreshed by a 1 s timer.
    @Published private(set) var elapsedText: String = "0:00"
    /// Which provider family the call runs on. Persisted via `AudioSettings`
    /// so a deliberate switch survives the call. Named `voiceEngine`, not
    /// `engine` — `engine` is already the audio engine on this type.
    @Published private(set) var voiceEngine: VoiceEngine = AudioSettings.shared.liveVoiceEngine
    @Published private(set) var realtimeProfile: String = AudioSettings.shared.realtimeVoiceProfile
    /// Microphone mode: true = hold-to-talk, false = open mic. It is independent
    /// of the selected Realtime or Hands-free provider family and remains local
    /// to this iOS device.
    @Published private(set) var pttOn: Bool = AudioSettings.shared.liveVoicePttOn

    /// Thread the live call is bound to. Retained so a mid-call provider-family
    /// switch can reconnect on the same thread.
    private var activeUiThreadId: String?
    /// Audio-focus owner held for the call's lifetime so chat auto-speak stays
    /// suppressed (see `VoiceCallAudioFocus`). Released in `teardownLocal`.
    private var voiceCallFocusOwner: UUID?
    /// Whether the mic is currently muted (mirrors `engine.muted`).
    @Published private(set) var muted: Bool = false

    /// Called on the main actor once per downstream assistant audio frame, right
    /// after it has been queued for playback.
    ///
    /// An observation hook, not a second consumer: playback is unaffected. It
    /// exists because "a voice is coming out of the speaker" is the only honest
    /// source for the ambient orb's `speaking` turn, and the alternative — an
    /// outside caller wrapping `client.onIncomingAudio` after `startCall` — would
    /// be silently unwrapped by the next `wireAudioPaths()`.
    ///
    /// It carries the frame rather than merely announcing one, because knowing
    /// *how much* audio was queued is what separates "the speaker is busy for
    /// another 4 seconds" from "a frame arrived recently" — see
    /// `AssistantPlaybackClock`.
    var onAssistantAudio: ((Data) -> Void)?

    /// Testable native handoff seams. Production closes the live audio graph
    /// before presenting/speaking so AVAudioSession ownership never overlaps.
    var guidedFlowScreenIsLocked: () -> Bool = { DeviceScreenLock.isLocked }
    var tutorBlackboardPresenter: (String) -> Void = { question in
        TutorOverlayRouter.shared.present(question: question, image: nil, autoStart: true)
    }
    var guidedFlowSpeaker: (String) -> Void = { message in
        SpeechSynthesizer.shared.speak(message, messageId: "guided-flow-screen-gate")
    }

    // MARK: - Derived, panel-facing

    /// A call is "active" (panel visible) whenever the transport is doing
    /// anything other than sitting idle or fully ended.
    var isActive: Bool {
        switch client.phase {
        case .idle, .ended:
            return false
        case .connecting, .reconnecting, .ready, .rotating, .failed:
            return true
        }
    }

    var isCallLive: Bool {
        switch client.phase {
        case .connecting, .reconnecting, .ready, .rotating:
            return true
        case .idle, .ended, .failed:
            return false
        }
    }

    /// The recent captions to show (user + assistant transcript lines).
    var captions: [RealtimeVoiceClient.Caption] { client.captions }

    /// Any transport error to surface in the panel.
    var errorMessage: String? { client.errorMessage }
    /// Feature guidance is transient and clears on the next admitted turn.
    var guidedFlowNoticeMessage: String? { client.guidedFlowNoticeMessage }

    var voiceAddressPhrase: String? {
        guard client.addressing.required else { return nil }
        return client.addressing.activationPhrases.first
    }

    /// Human-readable call status derived from the transport phase.
    var statusText: String {
        switch client.phase {
        case .idle:
            return "Ready"
        case .connecting:
            return "Connecting…"
        case .reconnecting:
            return "Reconnecting…"
        case .rotating:
            return "Refreshing session…"
        case .failed:
            return "Voice error"
        case .ended:
            return "Call ended"
        case .ready:
            // Between "let me check…" and the answer the line is silent; say
            // so rather than inviting the next question.
            if client.assistantWorking { return "Working…" }
            // In PTT we prompt to hold; otherwise capture is continuous.
            if let phrase = voiceAddressPhrase {
                return pttOn ? "Hold, then say \(phrase)" : "Listening for \(phrase)"
            }
            return pttOn ? "Hold to talk" : "Listening"
        }
    }

    // MARK: - Internals

    private var timer: Timer?
    private var cancellables: Set<AnyCancellable> = []
    private let log = Logger(subsystem: "ai.magicbeans.magios", category: "voice.call")

    /// The pre-ready gate: until the transport reports `session.ready`, captured
    /// frames are DROPPED — counted, never buffered, never sent. The assistant
    /// hears from ready onward and nothing earlier, on BOTH surfaces: the in-app
    /// call is gated exactly like the ambient one, one contract.
    ///
    /// This replaced `UpstreamHold` (owner decision, 2026-07-30), which buffered
    /// the ambient wake's pre-roll plus everything captured while the socket came
    /// up and flushed it all at ready, oldest byte first — up to ~20 s of it,
    /// sized to clear the measured 11–13 s provider bootstrap. The hold's own
    /// argument was real (speech that predates the socket arrives out of order or
    /// not at all), and the owner overruled it at the use-case level: there is no
    /// use case for hearing during wake-up and replaying 12-second-stale speech
    /// at connect, and the connect wording — "Starting conversation…" — never
    /// means "you can talk". The UI already reads as "not listening yet"; this
    /// makes it literally true.
    ///
    /// Locked because the producer is the audio render thread and the release
    /// runs on the main actor — same idiom as `RealtimeVoiceClient.audioSocket`.
    /// Behaviour lives on the struct so the count-and-release-once semantics are
    /// testable as pure state with no socket, engine or hardware; `internal` for
    /// that reason — and the lock is too, the same licence `wireAudioPaths`
    /// takes: `performLiveProviderSwap` must be PROVABLY arming it (the swap
    /// shipped once with the gate left open), and armed-ness is unobservable
    /// from outside the lock.
    struct PreReadyGate {
        /// While true, captured frames are discarded instead of sent.
        private(set) var isGating = false
        /// How many bytes the gate discarded — the tripwire. Logged once at
        /// ready, so if the no-pre-ready-audio decision ever needs revisiting,
        /// the evidence of how much speech was thrown away is already on device.
        private(set) var droppedBytes = 0

        static func armed() -> PreReadyGate {
            var gate = PreReadyGate()
            gate.isGating = true
            return gate
        }

        /// Take one captured frame. Returns true when it was discarded (and so
        /// must NOT be sent), false when the caller should send it directly.
        mutating func drop(_ frame: Data) -> Bool {
            guard isGating else { return false }
            droppedBytes += frame.count
            return true
        }

        /// Open at `.ready`. Returns the discarded byte count exactly once —
        /// nil on a repeat `.ready` (a reconnect re-runs the handshake and a
        /// rotation returns via `audio.rebind`), which is what keeps the
        /// tripwire log to one line per call.
        mutating func release() -> Int? {
            guard isGating else { return nil }
            defer { self = PreReadyGate() }
            return droppedBytes
        }
    }
    let preReadyGate = OSAllocatedUnfairLock(initialState: PreReadyGate())

    init(mode: VoiceCallMode) {
        self.mode = mode
        // Re-broadcast the transport's changes as our own so SwiftUI re-renders
        // the panel when phase / captions / error update (the panel observes
        // this ViewModel, not the nested client directly).
        client.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        // Mirror the engine's mic level for the orb.
        engine.$level
            .receive(on: RunLoop.main)
            .sink { [weak self] value in self?.level = value }
            .store(in: &cancellables)

        // The provider catalog may normalize a stale persisted profile after
        // this view model is created. Keep the call selection aligned with the
        // canonical AudioSettings value so the first tap cannot start an old or
        // browser-only profile while the menu displays the backend fallback.
        AudioSettings.shared.$realtimeVoiceProfile
            .receive(on: RunLoop.main)
            .sink { [weak self] profileID in self?.realtimeProfile = profileID }
            .store(in: &cancellables)
        AudioSettings.shared.$liveVoiceEngine
            .receive(on: RunLoop.main)
            .sink { [weak self] engine in
                guard let self else { return }
                // Ambient owns a separate, per-device engine snapshot. A chat
                // setting changed while an ambient call is open must not relabel
                // or reconfigure that conversation underneath it.
                guard self.mode == .inApp else { return }
                self.voiceEngine = engine
                self.refreshEffectivePttOnFromSettings()
            }
            .store(in: &cancellables)
        AudioSettings.shared.$liveVoicePttOn
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in self?.refreshEffectivePttOnFromSettings() }
            .store(in: &cancellables)

        // CRITICAL: the call can reach a terminal state without the user tapping
        // "end" — a server `session.end`, a dropped socket, or any transport error
        // all drive `client.phase` to `.ended`/`.failed`. The transport can't stop
        // the audio engine (it has no reference to it), so if we don't react here
        // the mic stays live, the `.voiceChat` session stays active (breaking
        // Phase-1 dictation/TTS), and the timer runs forever. Tear the local half
        // down on any terminal phase. Idempotent with `hangUp()`.
        client.$phase
            .receive(on: RunLoop.main)
            .sink { [weak self] phase in
                guard let self else { return }
                // `receive(on:)` can deliver an old `.ended` after a provider switch
                // has already started the replacement session. Never let that
                // stale terminal event stop the freshly re-armed audio graph.
                guard self.client.phase == phase else { return }
                if phase == .ended || phase == .failed { self.teardownLocal() }
            }
            .store(in: &cancellables)
    }

    /// Stop the local (audio/timer) half of the call. Safe to call repeatedly —
    /// `engine.stop(session:)` guards on `isRunning`.
    ///
    /// This runs on **every** terminal phase, including a server-sent
    /// `session.end` and a dropped socket that no user asked for — which is
    /// exactly why the disposition is read from `mode` here rather than written as
    /// a literal. An ambient call that ends by itself must leave the armed
    /// window's session alone just as carefully as one the controller ends.
    private func teardownLocal() {
        endConcurrentInteraction()
        engine.stop(session: sessionDisposition)
        stopTimer()
        // Nothing about a call that is over may leak into a later one: resetting
        // the gate discards its drop count, so a torn-down connect cannot log
        // its bytes against the next call's ready.
        preReadyGate.withLock { $0 = PreReadyGate() }
        level = 0
        // Release audio focus so chat auto-speak resumes once the call is gone.
        // Idempotent: release of an already-removed owner is a no-op, so the
        // repeated terminal-phase / hangUp calls are safe.
        if let owner = voiceCallFocusOwner {
            VoiceCallAudioFocus.shared.release(owner)
            voiceCallFocusOwner = nil
        }
    }

    deinit {
        timer?.invalidate()
    }

    // MARK: - Lifecycle

    /// Spend the audio graph's once-per-process first run — the VP AudioUnit
    /// instantiation, the I/O rebuild, AND the first stop of a VP-armed engine
    /// — while the app is foregrounded and nothing is listening.
    ///
    /// The first-wake investigation (0.1.158 → 0.1.160) pinned the mechanism:
    /// the once-per-process voice-processing build churns the shared session's
    /// activation, the session rides the running IO for the life of the call,
    /// and the FIRST stop of a VP-armed engine lapses the activation —
    /// survivable foregrounded (the next start re-activates) and fatal
    /// backgrounded, where activation is skipped by design (DTS 826462), so the
    /// re-arm's StartIO refuses with 'what' and the window dies "Lost the
    /// microphone." This prime RUNS the graph — a genuine start/stop cycle, the
    /// AU builds at start — where the reverted 0.1.157 warm only set the VP
    /// flag on a graph that never ran (`ambient-call-sink.md` device item 2
    /// names that failure; never add a `setVoiceProcessingEnabled(false)`
    /// either, which would make every wake first-run again).
    ///
    /// `.release` for both halves, deliberately. `.keepActive`'s start skips
    /// session configuration entirely and could StartIO-fail against a fresh
    /// process's unconfigured session — the very class being cured — while
    /// `.release` configures + activates first (legal foregrounded), and its
    /// stop deactivates a session NOTHING OWNS YET: the never-deactivate rule
    /// protects a live window's session, this runs strictly before any window
    /// exists, and the claim is ENFORCED at the arm site — with a live voice
    /// call holding `VoiceCallAudioFocus`, the controller defers the prime
    /// rather than running it, so the cycle can never churn a session another
    /// call's IO is riding. The spotter's own foregrounded `mic.start` then
    /// configures and activates fresh. The tap is installed for the cycle —
    /// `VoiceAudioEngine.start` builds it inline and needs the closure — but
    /// every frame is dropped on the floor: nothing is retained, wired, or
    /// sent. Non-fatal by design: a refusal logs one line and arming proceeds
    /// exactly as it would have.
    ///
    /// Reports whether the cycle actually ran, because the caller's
    /// once-per-process latch must not be spent by a throw: a failed
    /// `engine.start` may never have built the AU, and latching on it would
    /// silently restore first-wake exposure until relaunch.
    func primeAudioGraph() -> Bool {
        do {
            try engine.start(session: .release, onFrameOut: { _ in })
            engine.stop(session: .release)
            log.info("audio graph primed — first VP build and first VP stop spent foregrounded")
            return true
        } catch {
            // `start`'s own catch already unwound the partial graph; nothing
            // to stop here. The failure is a log line, never a refused arm.
            log.warning("audio graph prime failed; arming proceeds without it: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    /// Start a Live call: wire both audio directions, start the mic (unmuted for
    /// hands-free by default), open the transport, and begin the elapsed timer.
    ///
    /// Nothing captured before `session.ready` is sent — or kept. The pre-ready
    /// gate drops those frames and counts them: hearing starts when the session
    /// does. See `PreReadyGate` for what this replaced and why.
    func startCall(uiThreadId: String, engineOverride: VoiceEngine? = nil) {
        // Instrumentation only — see `VoiceConnectTrace`. First point on the path
        // both surfaces share that knows WHICH surface is connecting, so it is also
        // where an in-app trace begins; an ambient one begun at the wake continues.
        VoiceConnectTracer.shared.callStart(mode: mode.connectTraceMode)
        // Snapshot the iOS-local split-button choices together. This prevents a
        // Combine delivery delay from starting the profile shown before the
        // user's latest menu selection.
        let settings = AudioSettings.shared
        voiceEngine = Self.resolvedVoiceEngine(
            defaultEngine: settings.liveVoiceEngine,
            surfaceOverride: engineOverride
        )
        realtimeProfile = settings.realtimeVoiceProfile
        let selectedProfile = settings.realtimeVoiceProfiles.first {
            $0.id == realtimeProfile
        }
        pttOn = Self.effectivePttOn(
            requested: settings.liveVoicePttOn,
            engine: voiceEngine,
            profile: selectedProfile,
            mode: mode
        )
        // Armed BEFORE capture starts, so no captured frame can slip out ahead of
        // `.ready`. This is ordered rather than raced: `client.startCall` opens the
        // socket from inside a `Task`, so `sendAudio` is a no-op until this
        // main-actor run finishes regardless — but relying on that would make the
        // ordering an accident of the transport's internals.
        preReadyGate.withLock { $0 = PreReadyGate.armed() }
        // A voice session owns reply audio. Stop any normal chat narration
        // before configuring the shared AVAudioSession; VoiceCallAudioFocus
        // blocks subsequent reply speech until every terminal path releases it.
        // NOT on the ambient path — `SpeechSynthesizer.stop()` deactivates the
        // shared session unconditionally. See `VoiceCallMode.hushesChatNarration`.
        if mode.hushesChatNarration { SpeechSynthesizer.shared.stop() }
        wireAudioPaths()
        guard startAudioCapture() else { return }

        // Hold audio focus for the whole call so chat auto-speak can't add a
        // second voice or seize the AVAudioSession. Released in `teardownLocal`,
        // which fires on every terminal phase (ended/failed) and on hangUp.
        if voiceCallFocusOwner == nil {
            voiceCallFocusOwner = VoiceCallAudioFocus.shared.acquire()
        }

        // PTT Off default = unmuted continuous capture.
        applyLocalMicMode()

        activeUiThreadId = uiThreadId
        client.startCall(
            uiThreadId: uiThreadId,
            engine: voiceEngine,
            pttOn: pttOn,
            realtimeProfile: realtimeProfile,
            requireVoicePrefix: false
        )
        startTimer()
    }

    /// `internal` rather than `private` so a test can prove the two hooks below are
    /// still attached. Deleting either line leaves every other test green while the
    /// orb never reports speaking and the pre-ready gate never opens — a call that
    /// connects and stays deaf.
    func wireAudioPaths() {
        client.concurrentRequests = mode == .inApp
        if client.concurrentRequests {
            let coordinator = ConcurrentVoiceCoordinator.shared
            coordinator.liveActive = true
            coordinator.voiceInteracted = true
            coordinator.activate()
            coordinator.liveOutputBusy = { [weak self] in
                guard let self else { return false }
                return client.concurrentResponsePending || engine.hasPendingPlayback
            }
            coordinator.focusChanged = { [weak self] row in self?.client.selectConcurrentContext(row?.branchSessionId) }
            client.onConcurrentEvent = { [weak self] kind, _ in
                guard self != nil else { return }
                switch kind {
                case "speech.started": coordinator.captureStarted()
                case "speech.stopped": coordinator.captureStopped()
                case "transcript.user", "transcript.user.ignored": coordinator.inputSettled()
                case "input.cleared": coordinator.captureStopped(); coordinator.inputSettled()
                case "audio.output.ended", "voice.request.accepted": coordinator.inputSettled(); coordinator.foregroundStopped()
                default: break
                }
            }
        }
        // Downstream: provider audio → speaker.
        client.onIncomingAudio = { [weak self] data in
            guard let self else { return }
            if self.client.concurrentRequests { ConcurrentVoiceCoordinator.shared.foregroundStarted() }
            self.engine.play(data)
            // Reported after the frame is queued, so an observer that treats this
            // as "the assistant is speaking" is not ahead of the speaker.
            self.onAssistantAudio?(data)
        }
        // Interrupted playback must STOP, not drain. The server collapses its
        // self-echo windows the moment it interrupts a reply
        // (`media_rails/self_echo.rs` — `note_assistant_audio_done` /
        // `truncate_playback`): from that instant it stops accounting for the
        // bytes it already streamed, while this client may still hold seconds
        // of them in the player queue. On a device whose armed AEC still
        // leaks, the echo of that remainder finalizes AFTER the collapsed
        // window plus its tail — an admitted "user turn" that can re-seed the
        // very self-hearing loop the collapse closed. Flushing here is the
        // client keeping the server's promise. A natural end
        // (`interrupted == false`) asks nothing: the queue drains truthfully
        // on its own clock.
        client.onAssistantAudioEnded = { [weak self] interrupted in
            guard interrupted else { return }
            self?.engine.flushPlayback()
        }
        client.onTutorBlackboardRequested = { [weak self] text, quick in
            self?.handleTutorBlackboardHandoff(text: text, quick: quick)
        }
        client.onGuidedFlowRejected = { [weak self] message, backendAnnounced in
            self?.handleGuidedFlowRejection(
                message: message,
                backendAnnounced: backendAnnounced
            )
        }
        // Opened from the transport's own `.ready` handling rather than from the
        // phase subscriber, which defers to the run loop and then drops any
        // delivery whose phase has already moved on — correct for suppressing a
        // stale `.ended`, but it would silently skip the open when `.ready` is
        // superseded (by `.rotating`, say) inside one run-loop turn, leaving the
        // gate closed and the microphone deaf for the rest of the call.
        client.onReady = { [weak self] in self?.openUpstreamAtReady() }
        // The server can re-point the audio path on the same WS (upstream
        // rotation). The engine already rebuilds its converter on a route/format
        // change inside the capture tap, so there's nothing to re-arm here yet —
        // log for now (Task 6+ may add explicit re-arming if needed).
        client.onRebind = {
            #if DEBUG
            print("[VoiceCall] server requested audio rebind")
            #endif
        }
    }

    /// Apply a server-authorized native blackboard handoff exactly once. A lock
    /// transition can race the server event, so the device rechecks here and
    /// never queues a presentation to appear after a later unlock.
    func handleTutorBlackboardHandoff(text: String, quick: Bool) {
        if guidedFlowScreenIsLocked() {
            hangUp()
            guidedFlowSpeaker(DeviceScreenLock.message(for: .tutor))
            return
        }
        guard let invocation = TutorInvoke.parseVoiceGuidedFlow(text),
              invocation.feature == .tutor,
              invocation.canvasMode == .blackboard else { return }
        var concept = TutorInvoke.strip(invocation.normalizedText)
        if quick, !invocation.quick, !concept.lowercased().hasPrefix("#quick") {
            concept = "#quick \(concept)".trimmingCharacters(in: .whitespacesAndNewlines)
        }
        hangUp()
        tutorBlackboardPresenter(concept)
    }

    /// The backend-proxied provider normally owns rejection speech. Direct or
    /// older servers may not; in that compatibility path the live audio graph
    /// must be closed before local TTS claims the shared AVAudioSession.
    func handleGuidedFlowRejection(message: String, backendAnnounced: Bool) {
        guard !backendAnnounced else { return }
        hangUp()
        guidedFlowSpeaker(message)
    }

    /// Start or restart capture after a provider reconnect.
    /// Returns false only for a real-device audio failure.
    private func startAudioCapture() -> Bool {
        // Upstream: mic → provider. The `onFrameOut` callback fires on the audio
        // render thread. `client.sendAudio` is `nonisolated` (lock-guarded socket),
        // so we hand frames straight to it — no `Task`/main hop on the render thread.
        let client = self.client
        // Captured as a local: the lock is `Sendable`, `self` is not, and this
        // closure runs on the render thread where main-actor state is off limits.
        let gate = self.preReadyGate
        do {
            try engine.start(session: sessionDisposition, onFrameOut: { [weak client] data in
                let dropped = gate.withLock { $0.drop(data) }
                guard !dropped else { return }
                client?.sendAudio(data)
            })
        } catch {
            // The audio graph can't start in the simulator — tolerate there. On a
            // real device a start failure (mic held by another app, AEC init fail)
            // means a dead-mic call, so surface it instead of connecting silently.
            #if !targetEnvironment(simulator)
            client.failLocally("Microphone unavailable: \(error.localizedDescription)")
            return false
            #endif
        }
        return true
    }

    /// The moment the assistant starts hearing. Everything captured before this
    /// was discarded at the gate, and the one log line below is the record of
    /// how much — the tripwire for revisiting the no-pre-ready-audio decision
    /// (owner decision, 2026-07-30). Nothing is sent from here: there is nothing
    /// held to send.
    ///
    /// `release()` returning nil on a repeat `.ready` — a reconnect re-runs the
    /// handshake, and a rotation returns via `audio.rebind` — is what keeps the
    /// line to one per call, and keeps the connect trace from double-closing.
    private func openUpstreamAtReady() {
        guard let dropped = preReadyGate.withLock({ $0.release() }) else { return }
        // Instrumentation only. The trace closes at ready for both surfaces —
        // the in-app gate drops near-zero bytes, and that closing this trace
        // must not depend on bytes existing is unchanged from the flush era.
        VoiceConnectTracer.shared.finishReady(droppedPreReadyBytes: dropped)
        log.info("Discarded \(dropped, privacy: .public) bytes of pre-ready audio at session.ready — hearing starts now.")
    }

    /// `mode` is a required parameter rather than a defaulted one because the two
    /// answers differ in kind: `.ambient` has no button to hold, so a stored
    /// hold-to-talk preference there would mute a conversation the orb claims is
    /// live. See `VoiceCallMode.appliesStoredHoldToTalk`.
    nonisolated static func effectivePttOn(
        requested: Bool,
        engine: VoiceEngine,
        profile: RealtimeVoiceProfileOption?,
        mode: VoiceCallMode
    ) -> Bool {
        guard mode.appliesStoredHoldToTalk else { return false }
        return AudioSettings.resolveLiveVoicePttOn(
            requested: requested,
            engine: engine,
            profile: profile
        )
    }

    /// A surface-specific choice is a snapshot for this call, not a write to
    /// the ordinary in-app preference. This lets Ambient Listening and system
    /// launchers share the call implementation without sharing settings.
    nonisolated static func resolvedVoiceEngine(
        defaultEngine: VoiceEngine,
        surfaceOverride: VoiceEngine?
    ) -> VoiceEngine {
        surfaceOverride ?? defaultEngine
    }

    /// Hold to talk gates capture independently of the selected voice engine.
    nonisolated static func micMuted(pttOn: Bool, engine: VoiceEngine) -> Bool {
        pttOn
    }

    private func refreshEffectivePttOnFromSettings() {
        let settings = AudioSettings.shared
        let selectedProfile = settings.realtimeVoiceProfiles.first {
            $0.id == realtimeProfile
        }
        pttOn = Self.effectivePttOn(
            requested: settings.liveVoicePttOn,
            engine: voiceEngine,
            profile: selectedProfile,
            mode: mode
        )
    }

    private func applyLocalMicMode() {
        engine.muted = Self.micMuted(pttOn: pttOn, engine: voiceEngine)
        muted = engine.muted
    }

    /// End the call and tear both halves down.
    ///
    /// **Safe with no call to end, and safe to call twice** — which is a contract
    /// rather than a happy accident, because `RealtimeAmbientCallSink.endCall()` is
    /// this method and `AmbientController.disarm` issues it on every disarm,
    /// including from `.armed` where no call was ever started. `client.end()` sends
    /// nothing without a socket, `engine.stop(session:)` guards on `isRunning`, and
    /// the focus release is nil-guarded, so a second pass changes nothing.
    func hangUp() {
        endConcurrentInteraction()
        client.end()
        engine.stop(session: sessionDisposition)
        preReadyGate.withLock { $0 = PreReadyGate() }
        stopTimer()
        level = 0
        elapsedText = "0:00"
        activeUiThreadId = nil
        // Release audio focus on the explicit hangUp path too (belt-and-suspenders
        // vs the terminal-phase teardownLocal); idempotent, so no double-release.
        if let owner = voiceCallFocusOwner {
            VoiceCallAudioFocus.shared.release(owner)
            voiceCallFocusOwner = nil
        }
    }

    // MARK: - Mic controls

    private func endConcurrentInteraction() {
        guard client.concurrentRequests else { return }
        let coordinator = ConcurrentVoiceCoordinator.shared
        coordinator.foregroundStarted()
        coordinator.liveActive = false
        coordinator.liveOutputBusy = { false }
        coordinator.focusChanged = { _ in }
        coordinator.captureStopped(); coordinator.inputSettled()
    }

    /// Flip the mute state (hands-free mode).
    func toggleMute() {
        engine.muted.toggle()
        muted = engine.muted
    }

    /// Switch provider family. The backend latches the provider when the session
    /// opens, so this restarts the call session on the same thread.
    func setVoiceEngine(_ next: VoiceEngine) {
        guard next != voiceEngine else { return }
        voiceEngine = next
        AudioSettings.shared.liveVoiceEngine = next
        refreshEffectivePttOnFromSettings()
        restartProviderOrApply()
    }

    /// Change the active microphone mode without exposing protocol terminology
    /// in the UI. The media session, WebSocket, timer, captions, and local audio
    /// graph stay alive; the transport applies the new provider turn boundary
    /// over the existing control connection.
    func setHoldToTalk(_ enabled: Bool) {
        let selectedProfile = AudioSettings.shared.realtimeVoiceProfiles.first {
            $0.id == realtimeProfile
        }
        guard AudioSettings.resolveLiveVoicePttOn(
            requested: enabled,
            engine: voiceEngine,
            profile: selectedProfile
        ) == enabled else { return }
        guard enabled != pttOn else { return }
        pttOn = enabled
        AudioSettings.shared.liveVoicePttOn = enabled
        if client.phase == .failed, let uiThreadId = activeUiThreadId {
            // Preserve the existing failed-call retry affordance.
            startCall(uiThreadId: uiThreadId)
            return
        }
        applyLocalMicMode()
        client.setTurnBoundary(pttOn: enabled)
    }

    /// Swap the live provider family, or just re-apply the mic gate when idle.
    /// `client.end()` publishes `.ended`, which correctly tears down the old
    /// audio graph; capture is restarted explicitly first so the new session
    /// cannot reconnect with a dead microphone.
    private func restartProviderOrApply() {
        guard let uiThreadId = activeUiThreadId else {
            applyLocalMicMode()
            return
        }
        if client.phase == .failed {
            // The failed panel intentionally stays visible so a user can select
            // another provider. Treat that selection as a retry; updating only
            // the label leaves the call failed and makes the menu appear inert.
            startCall(uiThreadId: uiThreadId)
            return
        }
        guard isCallLive else {
            applyLocalMicMode()
            return
        }
        performLiveProviderSwap(uiThreadId: uiThreadId)
    }

    /// The live-swap body, split from the guards above with zero behaviour
    /// change.
    ///
    /// `internal` rather than `private` for the reason `wireAudioPaths` is: the
    /// guards are gated on the real client's phase, which no unit test can
    /// drive to `.ready` without a socket — so the guards stay untested by
    /// choice, while the body's one load-bearing contract (the pre-ready gate
    /// arms BEFORE capture can start) is pinned in `VoiceCallViewModelTests`.
    /// This is the one path that re-handshakes without re-entering `startCall`,
    /// and the one that shipped with the gate left open.
    func performLiveProviderSwap(
        uiThreadId: String,
        captureStart: (() -> Bool)? = nil
    ) {
        client.end()
        // Stop synchronously. The terminal-phase subscriber is scheduled on the
        // run loop, so waiting for it would make `start()` see the old graph as
        // still running and then let the delayed event kill capture.
        //
        // Reachable only from the in-app panel's provider menu, so `mode` is
        // `.inApp` and this releases — but it reads the mode anyway rather than
        // hardcoding `.release`, because a literal here would be a second place
        // for the two to drift apart.
        engine.stop(session: sessionDisposition)
        wireAudioPaths()
        // The third handshake that arms the pre-ready gate — the ambient and
        // in-app connects both arm inside `startCall`, but the provider swap
        // re-handshakes WITHOUT re-entering `startCall`, so it must arm here or
        // frames captured while the NEW provider session's socket comes up
        // would reach `sendAudio` pre-ready, breaking the one contract
        // ("nothing heard before ready"), which holds per-handshake. Armed
        // before `startAudioCapture` for the same ordering reason as in
        // `startCall`, and `onReady` was just re-wired above, so the new
        // session's ready opens it — no deaf-mic path.
        preReadyGate.withLock { $0 = PreReadyGate.armed() }
        // The injected starter is a unit-test seam for proving the ordering
        // above without asking a simulator to initialize RemoteIO. Production
        // callers omit it and always enter the real capture path.
        guard captureStart?() ?? startAudioCapture() else { return }
        applyLocalMicMode()
        client.startCall(
            uiThreadId: uiThreadId,
            engine: voiceEngine,
            pttOn: pttOn,
            realtimeProfile: realtimeProfile,
            requireVoicePrefix: false
        )
        startTimer()
    }

    // MARK: - Hold to talk

    /// PTT button pressed: open the mic gate locally and on the server.
    func pttDown() {
        guard pttOn, isCallLive else { return }
        engine.muted = false
        muted = false
        client.engagePTT()
    }

    /// PTT button released: mute locally and commit the turn on the server.
    func pttUp() {
        guard pttOn, isCallLive else { return }
        engine.muted = true
        muted = true
        client.releasePTT()
    }

    // MARK: - Timer

    private func startTimer() {
        stopTimer()
        refreshElapsed()
        let timer = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refreshElapsed() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func stopTimer() {
        timer?.invalidate()
        timer = nil
    }

    private func refreshElapsed() {
        guard let started = client.startedAt else {
            elapsedText = "0:00"
            return
        }
        let total = max(0, Int(Date().timeIntervalSince(started)))
        let m = total / 60
        let s = total % 60
        elapsedText = String(format: "%d:%02d", m, s)
    }
}
