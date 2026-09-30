import Combine
import Foundation
import os
import UIKit

/// Owns the armed ambient window: the local wake-spotting tap, the handoff to
/// the realtime call stack, the hard cap, and the orb.
///
/// Exactly one path owns the microphone at any instant — spotting XOR call — so
/// no arbitration with `VoiceAudioEngine` is required. The handoff releases the
/// spotting tap before the call takes the microphone, and the failure path puts
/// it back.
///
/// Every collaborator is injected so the whole lifecycle is testable without
/// audio hardware: `AVAudioEngine` microphone capture has no input in the
/// simulator, ActivityKit needs a real Dynamic Island to mean anything, and the
/// call stack needs a socket and a backend. This is the one piece of the feature
/// that decides when a microphone turns off, so it is the piece that has to be
/// provable.
@MainActor
final class AmbientController: ObservableObject {

    /// The one controller, and the reason there may only be one.
    ///
    /// `ownerID` below is generated in `init`, and `AmbientArm`'s doc promises it
    /// is generated **once per launch** — which is what makes
    /// `AmbientArm.isStale(currentOwnerID:)` decidable at all. Until this
    /// singleton existed that promise was false by construction: the id was
    /// per-*instance*, so a second controller would have produced a second owner
    /// id and each would have read the other's live record as garbage to collect.
    /// The singleton is what makes the code match the doc rather than the other
    /// way round. The injecting initialiser stays for tests, which want a fresh
    /// window per case and never touch the App Group record concurrently.
    ///
    /// Constructing it is cheap and side-effect-free: the Vosk model loads lazily
    /// on the first `configure`, and the mic engine and call sink only register
    /// notification observers.
    static let shared = AmbientController(
        mic: AmbientMicEngine(),
        spotter: VoskWakeSpotter(),
        call: AmbientConversationCallSink(),
        activity: AmbientActivity()
    )

    /// Do not re-arm the spotter the instant a call ends.
    ///
    /// The one behavioural lesson the desktop integration transferred
    /// (`8bd5c2f9e`, where the companion constant is
    /// `WAKE_RESUME_COOLDOWN_MS = 2500`): the conversation's tail — the
    /// assistant's reply still coming out of the speaker, or the end of the
    /// user's own request — lands straight back in a freshly armed decoder and
    /// re-fires it.
    ///
    /// `VoskWakeSpotter.fireCooldown` does **not** cover this, which is why a
    /// second constant is needed rather than a larger first one. That one
    /// suppresses a repeat of a fire that already happened, and `resumeSpotting`
    /// calls `reset()`, which builds a new recognizer with no memory of one.
    static let wakeResumeCooldown: TimeInterval = 2.5

    /// The settle pause before the ambient connect retry.
    ///
    /// The first wake of a process used to fail its connect because the
    /// lazily-paid voice-processing rebuild had just destabilised the audio
    /// session — and the immediate retry re-entered the SAME disturbed session
    /// in the same runloop breath and died with it. The pause gives
    /// mediaserverd time to settle; with VP latched true from the first
    /// attempt's own rebuild, the second start has a real chance. Cost
    /// accepted: a genuinely failed connect takes this much longer to fall
    /// back to armed.
    static let wakeConnectSettleDelay: TimeInterval = 1.5

    /// How many times `resumeSpotting` may ask for the tap in one resume, and
    /// the spacing between asks — FOREGROUNDED ONLY. Three attempts, short
    /// gaps: a foregrounded re-arm runs session activation, so the first
    /// failure after a conversation can be a real transient that settles out
    /// by itself. Backgrounded there are no retries at all — activation is
    /// skipped by design there (DTS 826462), so a failed start is StartIO
    /// refusing an inactive session, and attempt N+1 fails identically while
    /// each blocking session round stalls the main actor (the 0.1.158 trace).
    /// The orb claims `armed` over no usable wake decoder for as long as retries
    /// run, so the whole budget stays under ~2 s and the final disarm stays.
    static let micStartAttempts = 3
    static let micStartRetryDelay: TimeInterval = 0.8

    @Published private(set) var state: AmbientState = .off {
        didSet { publishIfNeeded(from: oldValue, to: state) }
    }

    /// What the live window is listening for, or nil when no window exists.
    ///
    /// Published because the orb is not reachable as a control while the user is
    /// inside the app, so the in-app bar is where an armed window proves itself
    /// and names the phrase. `notes` is still carried here but read nowhere
    /// post-arm — kept deliberately (owner decision 2026-07-30, when the chip
    /// went single-line); Settings' pre-arm assessment is where the user reads
    /// it. See `AmbientPhraseSet`.
    @Published private(set) var listeningFor: AmbientPhraseSet?

    /// A power condition the live window is running in spite of, or nil when there
    /// is none.
    ///
    /// **Published, and set for the whole window rather than reported once.** Low
    /// Power Mode no longer refuses a window (`AmbientPowerMonitor.admission`), and
    /// the warning is the entire justification for arming anyway — so it has to be
    /// readable for as long as the window is open, on both surfaces that can show
    /// it. The orb gets it as a caption; the in-app bar reads this, because the
    /// Dynamic Island does not present an app its own Live Activity while that app
    /// is in the foreground.
    ///
    /// Cleared with `listeningFor`, so no surface can show a warning for a window
    /// that is not open.
    @Published private(set) var powerWarning: AmbientPowerBlock?

    /// The newest finalised line the live call reported, or nil until the
    /// conversation says something — fed by `captionPublisher`, rendered by the
    /// composition in `publishOrbCaption`. Cleared with the window, so a dead
    /// window's last words cannot lead the next window's caption.
    private var latestTranscript: AmbientCaptionLine?

    /// A transient system sentence the window needs the user to see, or nil when
    /// there is none. Exactly one thing sets it today: the failed-connect
    /// fallback, which used to return the orb to `armed` with nothing said — a
    /// wake that was heard, waited for, and then silently unhappened.
    ///
    /// It OUTLIVES the failed connect on purpose. Ambient mode is for someone
    /// who is not looking at their phone, so a sentence published only for the
    /// instant of the failure would be gone before it was ever seen; this one
    /// stands until the user acts on it or the window it described is over. But
    /// it never outlives either: cleared on `arm` (a fresh window owes nothing
    /// for the last one's connect), on the next wake or explicit Talk activation
    /// (the instruction was followed, and the caption belongs to the new
    /// attempt), and on every teardown that clears per-window state.
    private var connectNotice: String?

    /// Injected so tests can assert the refusal without a live capture session.
    var observationIsActive: () -> Bool = { ListenController.shared.state.isActive }

    /// Injected so the suite can park a window inside the cooldown, or skip it
    /// entirely. Production never assigns it — same hook shape, and the same
    /// reason, as `observationIsActive`.
    var resumeCooldown: TimeInterval = AmbientController.wakeResumeCooldown

    /// The other two pauses, injected on the same lever and for the same
    /// determinism: the settle pause before the connect retry, and the spacing
    /// between `resumeSpotting`'s re-arm attempts. Production never assigns
    /// either.
    var connectRetryDelay: TimeInterval = AmbientController.wakeConnectSettleDelay
    var micRetryDelay: TimeInterval = AmbientController.micStartRetryDelay

    /// The compact phase-word pulse's two clocks, injected on the same lever.
    /// Zeroing BOTH is how the suite disables the pulse outright — the driver
    /// refuses a cadence that does not exceed the show (`resetPhaseWordPulse`
    /// has the safety argument). Production never assigns them.
    var phaseWordShowSeconds: TimeInterval = AmbientPhaseWordPulse.showSeconds
    var phaseWordCadenceSeconds: TimeInterval = AmbientPhaseWordPulse.cadenceSeconds

    /// The pulse's timer, cancelled and rebuilt by `resetPhaseWordPulse` on
    /// every published phase change and on every teardown.
    private var phaseWordPulse: Task<Void, Never>?

    /// Injected so the re-arm's retry gate is deterministic under test; the
    /// default reads the real application state. Same hook shape, and the same
    /// reason, as `observationIsActive`. It exists for `resumeSpotting`'s
    /// retry policy — a backgrounded process skips session activation by
    /// design, so retrying a failed start there is futile main-thread pain
    /// against an inactive session, while a foregrounded one can hit a real
    /// transient worth retrying. (It once also gated the reverted audio
    /// warm-up; the re-arm is its owner now.)
    var isAppForegrounded: () -> Bool = { UIApplication.shared.applicationState != .background }

    /// Spent by the audio-graph prime in `arm`, once per process — the
    /// first-run VP build it spends only exists once, and only a prime that
    /// actually RAN ITS CYCLE may burn it. A backgrounded call does not spend
    /// it, a deferral behind a live voice call does not, and neither does a
    /// prime whose engine start THREW — the AU may never have built, and a
    /// spent latch there would silently restore first-wake exposure until the
    /// next relaunch. The trade is accepted: a persistently failing prime
    /// retries at every arm, which is foregrounded, fast-failing and
    /// self-healing — the opposite corner from a one-shot that can fail shut.
    private var didPrimeAudioGraph = false

    private let mic: AmbientMicSource
    private let call: AmbientCallSink
    /// Silent local output used only across provider-call -> wake-microphone
    /// ownership transfer.
    ///
    /// A `.keepActive` voice graph never calls `setActive(false)`, but stopping a
    /// voice-processing AudioUnit can still lapse the underlying recording
    /// session on device. Starting this graph before `call.endCall()` gives the
    /// audio session continuous active I/O until `AmbientMicSource.start` has
    /// installed the next input graph. It never records, uploads, or plays
    /// audible content.
    private let resumeKeepalive: AmbientDictationKeepingAlive
    /// Not optional, and deliberately so. An optional sink would mean arming can
    /// succeed with a live microphone and no orb — the exact outcome this feature
    /// refuses everywhere else — with a plausible-looking justification attached.
    private let activity: AmbientActivitySink

    /// The controller's own log line, in the sink's category on purpose: the
    /// connect is one story, and the retry decision made here belongs beside the
    /// failure reasons `RealtimeAmbientCallSink` records in the same stream.
    private let log = Logger(subsystem: "ai.magicbeans.magican", category: "ambient.call")

    /// `nonisolated(unsafe)` because `WakeSpotter.feed` is contractually called
    /// on the audio thread — the frame handler below is not main-actor work, and
    /// pretending otherwise is what the `@MainActor` on `onHit` exists to avoid.
    /// The reference is set once in `init` and never reassigned.
    private nonisolated(unsafe) let spotter: WakeSpotter

    /// Whether microphone frames are allowed to reach the wake decoder.
    ///
    /// This is deliberately independent of whether the microphone tap is
    /// installed. On the return from a conversation, iOS needs active audio I/O
    /// continuously or it may suspend the background process in the short pause
    /// between provider teardown and wake re-arm. We therefore reinstall the tap
    /// immediately, but keep this gate closed through the 2.5-second acoustic-tail
    /// cooldown. Frames are dropped in memory; none reaches Vosk until `reset()`
    /// has completed and the gate opens.
    ///
    /// The callback reads this on the audio thread, so main-actor state is not a
    /// safe substitute. The unfair lock also orders `reset()` before the first
    /// admitted frame: the main actor resets while the gate is false, then flips
    /// it true; a callback can only observe true after that write.
    private let wakeDecoderEnabled = OSAllocatedUnfairLock<Bool>(initialState: false)

    /// The most recent turn the live call reported, recorded the instant it
    /// arrives and on whatever thread it arrives on.
    ///
    /// `AmbientCallSink` is `@MainActor`, but that constrains its METHODS, not
    /// its publisher: `turnPublisher` hands out an `AnyPublisher` that escapes
    /// the actor, and `Subject.send` is nonisolated, so a socket-backed
    /// implementation can emit from whatever thread the transport runs on. The
    /// state assignment then has to hop, and that hop is asynchronous — which
    /// would leave "has a turn already arrived?", the question the handoff must
    /// answer before defaulting to `listening`, decided by a race. Recording the
    /// turn synchronously makes the answer exact whenever the hop lands.
    private let latestTurn = OSAllocatedUnfairLock<AmbientTurn?>(initialState: nil)

    /// The most recent thing the live call said about its own ending, recorded the
    /// instant it arrives and on whatever thread it arrives on.
    ///
    /// The same box, for the same reason, as `latestTurn` — and the reason binds
    /// harder here. The follow-up window is armed the moment the session is ready,
    /// which is *inside* `startCall`, so a `.quiet` can be emitted before
    /// `startCall` returns; and a conversation the server refuses can be `.ended`
    /// before it either. Both would then be racing the handoff's own
    /// `state = .conversing(.listening)`, and a lifecycle event that lost that race
    /// would be overwritten by a state describing a call that was already over.
    /// Recording synchronously and applying LAST is what makes the outcome exact
    /// rather than a matter of which hop lands first.
    private let latestLifecycle = OSAllocatedUnfairLock<AmbientCallLifecycle?>(initialState: nil)

    /// The newest finalised line the live call reported, recorded the instant it
    /// arrives and on whatever thread it arrives on.
    ///
    /// The same box, for the same reason, as `latestTurn`: the seam licenses
    /// `captionPublisher` to emit from any thread, and two off-main sends would
    /// otherwise queue two unstructured main-actor hops with no ordering
    /// guarantee between them — line A painting over line B. Recording
    /// synchronously and reading the box when a hop lands makes the newest line
    /// win whichever order the hops arrive in. Last-wins coalescing also spends
    /// LESS ActivityKit budget on a burst: every landed hop reads the same
    /// newest line, `AmbientActivity.updateCaption` dedups the repeats, and the
    /// lines that were superseded before any hop landed are never published at
    /// all — ordering safety and the smaller publish bill come from one move.
    ///
    /// Distinct from `latestTranscript`, deliberately: this is the arrival
    /// mailbox, living and dying with the call's subscription, while
    /// `latestTranscript` is the applied composition input `publishOrbCaption`
    /// reads, cleared with the window.
    private let latestCaption = OSAllocatedUnfairLock<AmbientCaptionLine?>(initialState: nil)

    private var capTimer: Task<Void, Never>?

    /// The cap fired while a wake was in flight and the teardown is deferred
    /// until the wake resolves — `startCapTimer` says why the leash yields.
    /// Named for the WAKE, not the connect: it arms in `.heard`, before any
    /// socket exists, and on a successful connect it rides through the whole
    /// conversation. Honoured on the wake's two exits: a failed connect disarms
    /// for the cap instead of falling back to armed (and sets no connect notice
    /// — the cap's sentence wins), and a conversation that ends disarms for the
    /// cap instead of returning the window. Cleared on `arm` and on every
    /// teardown, so a latch can never leak into a window it was not set for.
    ///
    /// The accepted visual while the latch waits: every publish carries
    /// `staleDate: expiresAt`, so past the cap the system marks the activity
    /// stale and `AmbientOrbAppearance.forWindow` renders the ended sentence
    /// even though the connect — or the conversation it bought — is still
    /// running. Tolerated rather than repainted, because the boundary is rare (a
    /// wake has to land in the window's final seconds) and the overhang is
    /// bounded — the failed exit by the two connect attempts, the successful one
    /// by the conversation itself — so the premature sentence only ever resolves
    /// into the exact teardown it predicted.
    private var capExpiredDuringWake = false

    private var turnSubscription: AnyCancellable?
    private var captionSubscription: AnyCancellable?
    private var lifecycleSubscription: AnyCancellable?
    private var pending: Task<Void, Never>?
    private var focusOwner: UUID?

    /// The battery rails. Owned rather than injected: it is a notification
    /// registration plus a pure decision, and the decision is `static` so it is
    /// assertable directly, while `readBlock` is the seam a test moves.
    let power = AmbientPowerMonitor()

    /// The armed window's identity, set only by a successful `arm` and cleared
    /// by `disarm`. `nil` means no window exists — which is also the answer to
    /// whether an orb should, so it gates publishing on both ends.
    ///
    /// It has to, because a refused or failed `arm` lands in `.recoverableError`
    /// and that state reduces to the `.armed` phase (it is named for a window
    /// that is still listening). Driving the verb off the reduction alone would
    /// therefore request an orb saying "armed" for a window that never opened —
    /// a dead window showing as live, the exact failure the state's naming is
    /// meant to prevent — and later end one that was never requested.
    private var armedAt: Date?

    /// Whether a window is open **right now**.
    ///
    /// Reads `armedAt` rather than the state for the reason `armedAt` documents:
    /// `.recoverableError` reduces to the `armed` orb phase and is reached by a
    /// window that never opened, so a state-shaped answer would report a live
    /// microphone for a refused arm. Every neighbour that is about to deactivate
    /// the shared audio session asks this question — see `AmbientRail` — so it
    /// must answer about the tap, not about the orb.
    ///
    /// It says nothing about which *phase* the window is in. That is deliberate:
    /// a window mid-conversation depends on the session exactly as much as one
    /// waiting for a wake word, and more of it is in someone else's hands.
    var windowIsLive: Bool { armedAt != nil }

    /// Readable so the in-app bar can show the remaining leash. Published now
    /// that Extend can move the value without a phase transition; arm/disarm
    /// still write beside their state changes, while an extension earns exactly
    /// this one local redraw plus one ActivityKit publish.
    @Published private(set) var expiresAt: Date?

    /// Whether the installed microphone tap is currently feeding the wake
    /// decoder.
    ///
    /// **Not the same question as `state == .armed`, and the gap between them is
    /// what this exists for.** A conversation that ends leaves the window in
    /// `.armed` for the 2.5 s of the resume cooldown (see `handleCallEnded`), and
    /// during that stretch the microphone tap is alive but frames are gated away
    /// from the decoder. A wake hit there is meaningless — and worse than
    /// meaningless, because `handleWake` REPLACES `pending`, so a hit would
    /// discard the resume that was about to enable the decoder and leave the
    /// window `.armed` with nothing spotting until the cap expires: the orb
    /// asserting something untrue, which is the failure this feature exists to
    /// refuse.
    ///
    /// In production no hit can arrive there — `wakeDecoderEnabled` refuses every
    /// frame and `ingest` is the spotter's only feeder — so this closes the door
    /// *structurally* instead of leaving it closed by an argument two files away. Design §15's
    /// lesson, applied to the state machine rather than to the audio session.
    private var wakeDecoderIsSpotting = false

    /// Whether an activity is actually on screen. `publishIfNeeded` owns the
    /// ActivityKit verbs, so it is also the only thing that knows whether the
    /// request succeeded — and a `didSet` cannot return a value. `arm` reads
    /// this back; the `end` path reads it so a request that was refused is not
    /// followed by an end for an activity that never existed.
    private var orbIsLive = false

    /// Distinguishes this process's arm record from a dead one. Generated once
    /// per launch, which is what makes `AmbientArm.isStale(currentOwnerID:)`
    /// decidable at all.
    private let ownerID = UUID().uuidString

    init(
        mic: AmbientMicSource,
        spotter: WakeSpotter,
        call: AmbientCallSink,
        activity: AmbientActivitySink,
        resumeKeepalive: AmbientDictationKeepingAlive? = nil
    ) {
        self.mic = mic
        self.spotter = spotter
        self.call = call
        self.activity = activity
        self.resumeKeepalive = resumeKeepalive ?? NativeAmbientDictationKeepalive()
        // `onHit` is typed `@MainActor`, so the hop is the spotter's obligation
        // and the hit reaches the state machine synchronously here — which is
        // also what lets `settle()` be deterministic.
        self.spotter.onHit = { [weak self] hit in self?.handleWake(hit) }
    }

    /// Test hook: await any in-flight transition. Production code never calls it.
    func settle() async {
        await pending?.value
    }

    // MARK: - arm / disarm

    func arm(phrases: [String], capSeconds: Int) async {
        // `.recoverableError` is admitted alongside `.off` because every failure
        // path lands there and the captions say "try again" — refusing would
        // make that a lie and leave the user no way back without relaunching.
        // `AmbientState` states the invariant (terminal failures disarm instead
        // of landing here); honouring it is what makes the state recoverable.
        // Mirrors `ListenController.start`, which admits `.idle, .error, .ended`.
        switch state {
        case .off, .recoverableError: break
        default: return
        }
        // A request belongs to the window visible when it was tapped. It must
        // never survive into a later window opened in the same app process.
        AmbientSignal.clearPendingExtension()
        // Observation already owns the microphone, and exactly one path may.
        guard !observationIsActive() else {
            state = .recoverableError(message: "Magican is recording a session right now. Stop it first.")
            return
        }
        // The battery rails, asked BEFORE the window opens rather than only while
        // it is running.
        //
        // Both of design §9's power conditions are reported by notifications that
        // fire on a *change*, so a window armed while Low Power Mode is already on
        // — the common case, since Low Power Mode is sticky and often on for days
        // — would never hear from one, and the rail would look like it worked
        // because the transition case does. Asking here is what closes that.
        //
        // The two answers differ, and `AmbientPowerMonitor.admission` says why: the
        // battery floor refuses (nothing has been opened, so `refusal` names the fix
        // rather than reporting a stop) and Low Power Mode arms with a warning
        // carried for the whole window.
        switch power.admission() {
        case .refused(let block):
            state = .recoverableError(message: block.refusal)
            AmbientArm.clear()
            return
        case .allowedWithWarning(let block):
            // Set BEFORE the orb exists, because `publishOrbCaption` below reads it
            // and the caption has to be on the activity's first update rather than
            // arriving a turn later.
            powerWarning = block
        case .allowed:
            powerWarning = nil
        }
        // Unconditionally, unlike `powerWarning` above: a fresh window opens
        // with no transcript whatever the last one said — and owes nothing for
        // the last one's failed connect or deferred cap either.
        latestTranscript = nil
        connectNotice = nil
        capExpiredDuringWake = false
        state = .arming
        // `WakeSpotter` carries no locking, and needs none because no admitted
        // `feed` overlaps these calls. Initial configuration/reset happens before
        // `mic.start`; the post-call reset happens with a running tap but behind
        // `wakeDecoderEnabled == false`, and that gate opens only after reset.
        spotter.configure(phrases: phrases)
        spotter.reset()
        // Both of the spotter's verdicts are read here, and this one is a
        // refusal. Vosk drops out-of-lexicon words from a grammar with nothing
        // but a log line, so a phrase set that is entirely rejected leaves a
        // spotter that can never call `onHit` — and a window opened over it is an
        // orb saying "listening" above a microphone the wake word cannot reach.
        //
        // Refusing HERE rather than after `mic.start` is deliberate: nothing has
        // been opened yet, so there is no tap to unwind and no focus token to
        // release, only a reason to give. Reaching this with an empty `phrases`
        // is the ordinary degradation, not an edge case — see
        // `AmbientActivationPhrases`.
        guard spotter.isArmed else {
            state = .recoverableError(
                message: Self.unusablePhrasesMessage(
                    requested: phrases,
                    rejected: spotter.rejectedPhrases
                )
            )
            AmbientArm.clear()
            return
        }
        // Once per process, strictly BEFORE the spotter takes the microphone:
        // spend the audio stack's first-run voice-processing build — and its
        // first VP stop — here, foregrounded, where a lapsed session
        // activation is repairable. Left to the first wake, that pair ran
        // backgrounded and killed the window (the first-wake bug;
        // `VoiceCallViewModel.primeAudioGraph` carries the mechanism and why
        // this cure runs the graph where the reverted 0.1.157 warm set a
        // flag). The mic indicator blipping at arm is accepted: the line
        // below opens the microphone for the spotter anyway. `arm` is
        // foregrounded by contract at both doors; the guard re-checks because
        // contracts drift, and a backgrounded call does not spend the latch.
        //
        // Deferred outright while a LIVE voice call holds the audio session —
        // the intent door reaches this mid-in-app-call, and a prime there
        // would build a second VP-armed engine under running IO (the exact
        // churn class being cured) and then have its `.release` stop's
        // deactivate refused busy. `VoiceCallAudioFocus` is held for the whole
        // life of every call, so it is the fact the guard needs; the next
        // call-free arm pays the prime normally, latch untouched. The latch
        // is spent only when the cycle reports it RAN — see its declaration.
        if !didPrimeAudioGraph, isAppForegrounded() {
            if VoiceCallAudioFocus.shared.isActive {
                log.info("audio-graph prime deferred: a voice call holds the audio session")
            } else if call.primeAudioGraph() {
                didPrimeAudioGraph = true
            }
        }
        wakeDecoderEnabled.withLock { $0 = true }
        do {
            try mic.start(onFrame: { [weak self] frame in self?.ingest(frame) }, onFailure: { [weak self] error in
                self?.handleMicFailure(error)
            })
        } catch {
            wakeDecoderEnabled.withLock { $0 = false }
            // No tap means no window. Land somewhere the user can see and leave
            // nothing behind for the widget process to find.
            state = .recoverableError(message: Self.micUnavailableMessage(error))
            AmbientArm.clear()
            return
        }
        wakeDecoderIsSpotting = true
        // Chat narration that was ALREADY playing when this window opened must not
        // hand the shared session back when it finishes.
        //
        // `VoiceCallAudioFocus` below covers narration that starts *after* this
        // point — `SpeechSynthesizer.speak` refuses while the token is held — but it
        // cannot recall an utterance that is already in the air, and that
        // utterance's `finish()` ends in a deactivation the window could never
        // recover from in the background. This tells it the claim is void, which it
        // is: the session it configured for `.playback` has just been reconfigured
        // to `.playAndRecord` and reactivated by `mic.start` above.
        SpeechSynthesizer.shared.abandonSessionClaim()
        // From here the microphone is LIVE, so every remaining failure has to
        // unwind it rather than return.
        // Chat auto-speak seizes the shared `AVAudioSession` and would kill the
        // capture engine out from under the armed window.
        focusOwner = VoiceCallAudioFocus.shared.acquire()
        let arm = AmbientArm(armedAt: Date(), capSeconds: capSeconds, ownerID: ownerID)
        arm.save()
        armedAt = arm.armedAt
        expiresAt = arm.expiresAt
        // The other half of what the spotter has to say, and this half is
        // REPORTED rather than enforced — whether a phrase should be refused on
        // its measured rate is a product decision nobody has taken, and the rates
        // form a continuum with nowhere to take it. Published alongside `armedAt`
        // and cleared with it, so the in-app bar can never show a phrase set for
        // a window that is not open.
        listeningFor = AmbientPhraseSet(
            phrases: phrases.filter { !spotter.rejectedPhrases.contains($0) },
            notes: spotter.phraseNotes
        )
        // The orb's Disarm button runs in the widget process and has nothing to
        // trip this process up with — no server session, no upload. Listening for
        // its signal is what makes the button work at all, and it starts here
        // rather than at `state = .armed` because the window exists from the
        // moment the tap does: every path that unwinds one unregisters again.
        startObservingDisarmSignal()
        startObservingExtensionSignal()
        startCapTimer(seconds: TimeInterval(capSeconds))
        // Publishes the orb: `arming` reduces to nil and `armed` does not, so
        // `publishIfNeeded` picks `start` off the pair. The verb is never sent
        // from here — that is the whole point of deriving it from both
        // reductions, and calling `start` directly as well would request the
        // activity twice.
        state = .armed
        // The orb is not decoration, it is the disarm control, and the only one
        // reachable without opening the app. If it did not come up, the user has
        // no proof of life and no way to stop the tap from outside — so the
        // window does not stay open. No visible indicator, no armed mic.
        if !orbIsLive {
            unwindLiveWindow(
                message: "Couldn't show the ambient orb. Turn Live Activities on for Magican and try again."
            )
            return
        }
        // The orb exists now, so anything the window has to say for its whole
        // duration is published here — see `publishOrbCaption`.
        publishOrbCaption()
        // LAST, and after the orb check, because `start` re-asks the question
        // immediately and an answer of "no" disarms synchronously. Anything placed
        // after this line would run against a window that had already been torn
        // down — and `state = .armed` after a disarm would resurrect one with no
        // tap behind it, which is this feature's signature failure.
        //
        // The callback disarms rather than hops. `disarmNow` is main-actor and
        // synchronous by construction, so the microphone is off before the caller
        // sees anything; a `Task { await disarm(…) }` here would leave a window of
        // queued main-actor work running against a window this rail had already
        // decided to end.
        power.start { [weak self] block in
            self?.disarmNow(reason: block.endedReason)
        }
    }

    /// Tear down a window whose microphone is already running, without going
    /// through `disarm`: there is no orb to end and nothing to tell the user
    /// about a window that never opened, only a live tap to close.
    private func unwindLiveWindow(message: String) {
        capTimer?.cancel()
        capTimer = nil
        pending?.cancel()
        AmbientSignal.stopObservingDisarm()
        AmbientSignal.stopObservingExtension()
        AmbientSignal.clearPendingExtension()
        // Idempotent, and reached before `arm` ever starts it on the one path that
        // gets here today. Unregistering unconditionally is what stops that from
        // being a fact a future reordering can silently invalidate.
        power.stop()
        wakeDecoderEnabled.withLock { $0 = false }
        mic.stop()
        resumeKeepalive.stop()
        wakeDecoderIsSpotting = false
        if let focusOwner { VoiceCallAudioFocus.shared.release(focusOwner) }
        focusOwner = nil
        AmbientArm.clear()
        // Cleared before the state moves so `publishIfNeeded` cannot end an
        // activity that was never successfully requested.
        armedAt = nil
        expiresAt = nil
        listeningFor = nil
        powerWarning = nil
        latestTranscript = nil
        connectNotice = nil
        capExpiredDuringWake = false
        state = .recoverableError(message: message)
    }

    /// `async` for its callers' sake rather than its own — see `disarmNow`, which
    /// is the whole of it.
    func disarm(reason: String?) async {
        disarmNow(reason: reason)
    }

    /// Stop everything, synchronously.
    ///
    /// **The body contains no `await`, which is a property rather than an
    /// accident**: a disarm that suspended halfway would leave a live microphone
    /// on the far side of a window it had already declared closed, and every
    /// interleaving test in this suite depends on `disarm` being atomic against
    /// the handoff. It is spelled as a synchronous function so that stays true by
    /// construction and so the rails can call it: `AmbientRail.yield` and the
    /// battery monitor both run from contexts that cannot await, and both are
    /// promises that the microphone is off *now*.
    ///
    /// `disarm(reason:)` remains as the `async` face for the callers that already
    /// have one.
    func disarmNow(reason: String?) {
        guard state != .off else { return }
        // Set first, because this is the state whose reason fills
        // `ContentState.endedReason` — publishing happens on the way in.
        state = .disarming(reason: reason)
        capTimer?.cancel()
        capTimer = nil
        // An in-flight handoff is parked inside `startCall`'s await. Cancelling
        // does not interrupt that await, so the re-checks in `handoff` are what
        // actually protect the teardown; this is what lets a cancellation-aware
        // sink bail early rather than connect a socket nobody wants.
        pending?.cancel()
        // Nothing left to disarm, so nothing left to hear about it. Unregistering
        // here rather than leaving a permanent observer also keeps the signal
        // meaningful: a delivery outside an armed window is the widget asking
        // about a window this process does not have.
        AmbientSignal.stopObservingDisarm()
        AmbientSignal.stopObservingExtension()
        AmbientSignal.clearPendingExtension()
        power.stop()
        turnSubscription = nil
        captionSubscription = nil
        lifecycleSubscription = nil
        latestTurn.withLock { $0 = nil }
        latestLifecycle.withLock { $0 = nil }
        latestCaption.withLock { $0 = nil }
        wakeDecoderEnabled.withLock { $0 = false }
        mic.stop()
        resumeKeepalive.stop()
        wakeDecoderIsSpotting = false
        call.endCall()
        if let focusOwner { VoiceCallAudioFocus.shared.release(focusOwner) }
        focusOwner = nil
        AmbientArm.clear()
        state = .off
        armedAt = nil
        expiresAt = nil
        listeningFor = nil
        powerWarning = nil
        latestTranscript = nil
        connectNotice = nil
        capExpiredDuringWake = false
    }

    /// End the window because a foreground capture the user just asked for needs
    /// the microphone. See `AmbientRail`.
    ///
    /// **Why this yields rather than refuses.** A user who has opened the app and
    /// pressed hold-to-talk, or tapped "Listen here", is holding their phone and
    /// looking at it. Refusing that to protect a background convenience produces a
    /// microphone that appears broken, with the explanation on a Live Activity the
    /// system does not even show an app while that app is in the foreground.
    /// Design §9 already ranks observation above ambient; dictation is the same
    /// judgement for the same reason. The window ends *with a reason*, so the orb's
    /// last frame says which of the user's own actions closed it.
    ///
    /// A no-op with nothing armed, which is the overwhelmingly common case — every
    /// dictation and every observation start calls this.
    func yieldForForegroundCapture(reason: String) {
        guard windowIsLive else { return }
        disarmNow(reason: reason)
    }

    // MARK: - phrase-set verdicts

    /// Why a phrase set cannot open a window, in the user's terms.
    ///
    /// Pure and `static` so all three cases are assertable, and because each of
    /// them is reached by a genuinely different failure that the user can do a
    /// genuinely different thing about. Naming the rejected phrases matters: the
    /// user never typed them here — they are derived from the assistant's name —
    /// so "Magican can't listen" without saying for *what* is unactionable.
    static func unusablePhrasesMessage(requested: [String], rejected: [String]) -> String {
        guard !requested.isEmpty else {
            // The identity cache has nothing in it yet, which is the ordinary
            // first-run state. Refusing beats arming on a name the user never
            // chose, so there is no invented fallback phrase to fall back to.
            return "Magican doesn't know what to listen for yet. Open Magican so it can load your assistant's name, then try again."
        }
        guard !rejected.isEmpty else {
            // Every phrase passed the lexicon check and the spotter still armed
            // nothing, so the decoder itself did not come up.
            return "Magican's on-device wake model didn't load, so it can't listen for anything. Try again."
        }
        let list = rejected.map { "“\($0)”" }.joined(separator: ", ")
        return "Magican can't hear \(list) — the on-device wake model doesn't know those words. Rename your assistant, or give it an alias the model can hear."
    }

    // MARK: - microphone failure verdicts
    //
    // `AmbientMicFailure` documents that the controller does not read it, and one
    // case is now the exception. Widening the orb to show route-change reason
    // codes remains wrong for the reason that comment gives — ambient mode is for
    // someone not looking at their phone — but "revoked permission" is not a
    // reason code, it is a DIFFERENT INSTRUCTION. Every other failure is answered
    // by trying again, and this one never will be: the microphone switch is off in
    // Settings and no amount of re-arming turns it on. Telling that user to try
    // again is the same class of lie as an orb over a dead tap, so exactly one
    // case is split out and the rest keep the generic caption.

    /// Why arming could not open a tap, in the user's terms.
    nonisolated static func micUnavailableMessage(_ error: Error) -> String {
        guard isPermissionFailure(error) else { return "Couldn't open the microphone." }
        return permissionRevokedMessage
    }

    /// Why a window that WAS running has ended, in the user's terms. Published to
    /// the orb, which is where a window that ended off screen is explained.
    ///
    /// A second case earned its own sentence the way permission did: the
    /// backgrounded start-failure class is not "the microphone broke", it is
    /// iOS declining to give a backgrounded process the session back — and its
    /// instruction genuinely differs (open the app; foregrounded, activation
    /// runs and arming works). "Lost the microphone." for that user names no
    /// cause and no fix; this names both.
    nonisolated static func micFailureReason(_ error: Error) -> String {
        if isPermissionFailure(error) { return permissionRevokedMessage }
        if isBackgroundedStartFailure(error) { return backgroundedMicLossMessage }
        return "Lost the microphone."
    }

    /// The backgrounded start-failure class, as the investigation identified
    /// it: activation is skipped in the background by design (DTS 826462), so
    /// a start there either reads a degenerate input format off the inactive
    /// session (`AmbientMicFailure.inputFormatInvalid`) or reaches AURemoteIO
    /// and has StartIO refuse with the CoreAudio OSStatus 'what' (0x77686174).
    /// Both mean the same thing to the user, so both map to one sentence.
    private nonisolated static func isBackgroundedStartFailure(_ error: Error) -> Bool {
        if (error as? AmbientMicFailure) == .inputFormatInvalid { return true }
        return (error as NSError).code == 2_003_329_396
    }

    /// One sentence for both surfaces, because the fix is the same from either and
    /// two copies would drift.
    nonisolated static let permissionRevokedMessage =
        "Microphone access is off for Magican. Turn it on in Settings."

    /// The backgrounded start-failure sentence: names what happened (iOS took
    /// the session back, not a broken microphone) and the one fix that works.
    nonisolated static let backgroundedMicLossMessage =
        "iOS released the microphone while Magican was in the background — open Magican to listen again."

    /// What the orb says after a wake's connect failed twice — the one fallback
    /// that used to be silent. Generic BY DECISION: the specific cause belongs
    /// in the log at the layer that understands it (see
    /// `AmbientCallSink.startCall`), and the only thing the user can do about
    /// any of them is the thing this sentence says.
    nonisolated static let connectFailedNotice =
        "Couldn't connect — say the wake word to try again."

    private nonisolated static func isPermissionFailure(_ error: Error) -> Bool {
        (error as? AmbientMicFailure) == .recordPermissionMissing
    }

    // MARK: - cross-process disarm

    /// Listen for the orb's Disarm button, which runs in the widget process.
    ///
    /// The registration is process-wide, which is correct because an armed window
    /// is: there is one microphone and one controller holding it.
    private func startObservingDisarmSignal() {
        AmbientSignal.startObservingDisarm { [weak self] in
            // The signal arrives on whatever thread `CFNotificationCenter` uses;
            // everything it leads to is main-actor state driving a live
            // microphone, so the hop is made here rather than assumed.
            Task { @MainActor in await self?.handleDisarmSignal() }
        }
    }

    /// The disarm signal arrived from the widget process.
    ///
    /// **The delivered notification IS the command.** It does not wait for the
    /// App Group record to become readable, because the two travel by different
    /// routes and the notification is the faster one — gating the microphone on
    /// the slower route would mean a tap that visibly did nothing until the user
    /// next opened the app. The record is read only for the identity to
    /// acknowledge with; a request that is not readable yet costs the intent its
    /// acknowledgement, not the user their disarm.
    ///
    /// A delivery for a window that no longer exists is not an error and not a
    /// no-op — see `performRequestedDisarm`.
    func handleDisarmSignal() async {
        await performRequestedDisarm(request: AmbientSignal.consumePendingDisarm())
    }

    /// Listen for the Live Activity's Extend button. Unlike Disarm, missing the
    /// request record is a no-op: extending without a matching command would keep
    /// a microphone alive longer than the user asked. A persisted request is
    /// consumed on foreground by `AmbientEntryPoint` as the second route.
    private func startObservingExtensionSignal() {
        AmbientSignal.startObservingExtension { [weak self] in
            Task { @MainActor in await self?.handleExtensionSignal() }
        }
    }

    /// The App Group write and Darwin post are two cross-process transports and
    /// can become visible in either order. Unlike Stop, the notification alone
    /// is not authority to prolong a microphone lease, so wait briefly for its
    /// matching record instead of extending on faith or dropping a fast tap.
    private func handleExtensionSignal() async {
        let deadline = Date().addingTimeInterval(0.5)
        repeat {
            if AmbientSignal.consumePendingExtension() != nil {
                _ = extendWindow()
                return
            }
            guard Date() < deadline else { return }
            do {
                try await Task.sleep(nanoseconds: 20_000_000)
            } catch {
                return
            }
        } while true
    }

    func consumePendingExtensionRequest() {
        guard AmbientSignal.consumePendingExtension() != nil else { return }
        _ = extendWindow()
    }

    /// Add one policy increment to the live window, updating every authority in
    /// one main-actor turn: persisted ownership, controller deadline, timer, and
    /// ActivityKit content/stale date. Returns the new deadline for focused tests
    /// and for callers that need to distinguish a cap/no-window no-op.
    @discardableResult
    func extendWindow(now: Date = Date()) -> Date? {
        guard let armedAt, let currentExpiry = expiresAt,
              currentExpiry > now, orbIsLive,
              let extended = AmbientExtensionPolicy.extendedExpiry(
                  armedAt: armedAt,
                  currentExpiry: currentExpiry
              ) else { return nil }

        let capSeconds = Int(extended.timeIntervalSince(armedAt).rounded())
        AmbientArm(armedAt: armedAt, capSeconds: capSeconds, ownerID: ownerID).save()
        expiresAt = extended
        capExpiredDuringWake = false
        startCapTimer(seconds: extended.timeIntervalSince(now))
        activity.updateExpiry(extended)
        return extended
    }

    /// The backstop for an app that was not listening when the tap happened —
    /// suspended despite `UIBackgroundModes: audio`, or launched afterwards. Here
    /// the record is the ONLY evidence a tap ever occurred, so its absence means
    /// there is nothing to do; call it on every foreground.
    func consumePendingDisarmRequest() async {
        guard let request = AmbientSignal.consumePendingDisarm() else { return }
        await performRequestedDisarm(request: request)
    }

    /// Stop for real, then say so.
    ///
    /// The acknowledgement is written strictly AFTER the disarm, because that is
    /// the whole meaning of it: the intent treats it as proof the microphone is
    /// off, and an acknowledgement sent first would be a promise rather than a
    /// fact — the same optimism as ending the orb before anything stopped.
    private func performRequestedDisarm(request: AmbientDisarmRequest?) async {
        guard armedAt != nil else {
            // Nothing armed in this process means nothing armed anywhere, since an
            // ambient window cannot outlive the process that opened it. So the orb
            // the user just tapped belongs to a window that is already gone: collect
            // it here, where the sink is, rather than leaving the intent to infer it
            // — same act, and same reasoning, as `reconcileOnLaunch`'s sweep.
            //
            // Acknowledging is then honest rather than generous: "stop listening"
            // has been satisfied, and the orb is gone. Staying silent would leave
            // the intent to time out and take the orphan branch to the same place,
            // a second later.
            activity.endOrphans()
            if let request { AmbientSignal.acknowledgeDisarm(request) }
            return
        }
        // No reason: this is the user's own doing, and `AmbientActivity.end`
        // dismisses a reasonless end immediately instead of lingering with an
        // explanation nobody needs.
        await disarm(reason: nil)
        if let request { AmbientSignal.acknowledgeDisarm(request) }
    }

    /// A window cannot survive termination — it is a local microphone tap with
    /// no server-side existence — so a surviving record is garbage to collect,
    /// never a session to resume. Writing adoption logic here is the bug
    /// `AmbientArm` exists as its own type to prevent.
    func reconcileOnLaunch() {
        // Nothing ambient can be live at launch, so this cannot collect a real
        // window. The guard is here so a mistaken call mid-window cannot end the
        // live orb and manufacture the exact failure this method exists to clear.
        guard armedAt == nil else { return }
        // A disarm request left by a tap on an orb whose process was already gone.
        // Nobody is waiting for it — the intent's wait expired long before this
        // launch — but it must not survive to disarm the window the user arms
        // NEXT, which is what a request record outliving its question would do.
        AmbientSignal.clearPendingDisarm()
        AmbientSignal.clearPendingExtension()
        // Unconditional, and deliberately NOT gated on finding a record. An
        // activity lives in the system's hands rather than the process's, so it
        // can outlive the launch that created it even when the arm record did
        // not — a termination between the two writes leaves precisely that. The
        // leftover orb claims to be listening when nothing is, and offers a
        // disarm control for a window that no longer exists: the same lie as an
        // armed mic with no orb, inverted. Both tell the user the wrong thing
        // about whether they are being heard, so both are closed.
        activity.endOrphans()
        guard let arm = AmbientArm.claim() else { return }
        guard arm.isStale(currentOwnerID: ownerID) || arm.isExpired() else { return }
        AmbientArm.clear()
        state = .off
    }

    // MARK: - the armed tap

    /// The whole of what happens to audio while armed: spot it, and nothing
    /// else.
    ///
    /// `nonisolated` because this runs on the audio thread. There is
    /// deliberately no second destination — that absence is the privacy
    /// invariant, and it tightened on 2026-07-30 (owner decision): the
    /// `WakePreRoll` ring that used to buffer these frames for replay into the
    /// call is gone, so nothing said while armed is retained at all. A frame
    /// is inspected for the wake phrase and dropped on the floor.
    private nonisolated func ingest(_ frame: Data) {
        guard wakeDecoderEnabled.withLock({ $0 }) else { return }
        spotter.feed(frame)
    }

    // MARK: - activation

    /// Begin a conversation because the user explicitly tapped the system's
    /// "Talk to Magican" action.
    ///
    /// The action opens the same ambient window as a wake phrase; this is not a
    /// second voice stack. The only difference is the first transition: a tap
    /// has already expressed intent, so making the user say a wake phrase as
    /// well would turn one action into two. Once this conversation ends, the
    /// existing resume path reinstalls the spotter and later turns use the wake
    /// phrase normally.
    ///
    /// `async` matters only for the narrow post-conversation cooldown. During
    /// that interval `state` is already `.armed` and audio I/O is live, but wake
    /// decoding is still gated. An explicit tap waits for that bounded transition
    /// instead of silently doing nothing. Every other invalid phase fails
    /// immediately, and `beginConversation` changes state synchronously before
    /// it enqueues work, so two taps cannot start two calls.
    @discardableResult
    func talkNow() async -> Bool {
        if case .armed = state, !wakeDecoderIsSpotting {
            await pending?.value
        }
        return beginConversation(phrase: "")
    }

    private func handleWake(_ hit: WakeHit) {
        _ = beginConversation(phrase: hit.phrase)
    }

    /// The single entrance to the existing spotting-to-call handoff. Wake-word
    /// and explicit-tap activations share it so microphone ownership, retries,
    /// subscriptions, teardown, and the return to wake listening cannot drift.
    @discardableResult
    private func beginConversation(phrase: String) -> Bool {
        // Ignore hits mid-conversation: the user is talking TO the assistant,
        // not asking for a second one. The same guard makes an explicit double
        // tap idempotent: the first transition leaves `.armed` synchronously.
        guard case .armed = state else { return false }
        // And ignore an activation while wake decoding is gated. `.armed` is also
        // the state during the post-conversation resume cooldown: `talkNow` waits
        // for that bounded transition, while a physically impossible wake hit
        // during it is simply rejected.
        guard wakeDecoderIsSpotting else { return false }
        // This activation ANSWERS a standing connect notice, if one is up. The
        // user followed its instruction either by saying the wake phrase or by
        // tapping Talk again, so the caption belongs to the new attempt.
        // Recomposed and republished before the phase moves, so the failed
        // attempt's sentence can never sit under an orb that is already trying.
        if connectNotice != nil {
            connectNotice = nil
            publishOrbCaption()
        }
        state = .heard(phrase: phrase)
        // Instrumentation only, and stamped HERE rather than inside `handoff` so
        // the enqueued hop below is itself priced. See `VoiceConnectTrace`.
        VoiceConnectTracer.shared.wake()
        pending = Task { [weak self] in await self?.handoff() }
        return true
    }

    private func handoff() async {
        // `beginConversation` ENQUEUES this rather than inlining it, so main-actor work
        // already queued — the orb's disarm intent, or the cap timer — runs to
        // completion between `state = .heard` and here. The window may already
        // be gone, in which case `disarm` has taken everything down and there is
        // nothing left to hand off.
        guard case .heard = state else { return }
        // Instrumentation only. Everything between here and `startCall` — the
        // mic stop — is the `handoff>start` delta.
        VoiceConnectTracer.shared.mark(.handoff)
        // The window this handoff belongs to. `armedAt` is the window's identity
        // and changes on every arm, so comparing it after the connect separates
        // "my window is still open" from "a LATER window is open" — which a
        // state-shaped check cannot tell apart. Reachable with no unusual user
        // action: the cap disarms, the user re-arms and speaks, and window one's
        // connect is still parked on a slow socket.
        let window = armedAt
        // Release the spotting tap BEFORE the call takes the microphone. Nothing
        // travels from this side of the handoff into the call: the wake phrase
        // and whatever followed it were spotted and dropped, never buffered
        // (owner decision, 2026-07-30 — the assistant hears from session-ready
        // onward), so the call starts empty-handed.
        wakeDecoderEnabled.withLock { $0 = false }
        mic.stop()
        wakeDecoderIsSpotting = false
        state = .connecting
        latestTurn.withLock { $0 = nil }
        latestLifecycle.withLock { $0 = nil }
        latestCaption.withLock { $0 = nil }
        // Subscribe BEFORE starting the call. `turnPublisher` does not replay,
        // so a turn emitted while connecting would otherwise be dropped — and
        // to the user a dropped first turn reads as the assistant ignoring them.
        // The bug is invisible against a replaying publisher, so the ordering is
        // load-bearing rather than stylistic.
        turnSubscription = call.turnPublisher.sink { [weak self] turn in
            guard let self else { return }
            self.latestTurn.withLock { $0 = turn }
            // The call stack may emit from any thread (see `latestTurn`), and
            // `state` is main-actor state driving a live microphone, so the
            // isolation is enforced rather than assumed. Delivering inline when
            // already on the main actor also keeps the common case ordered
            // exactly as it was emitted, instead of behind a hop.
            if Thread.isMainThread {
                MainActor.assumeIsolated { self.applyLatestTurn() }
            } else {
                Task { @MainActor [weak self] in self?.applyLatestTurn() }
            }
        }
        // Same ordering, same reason, and a sharper one: the follow-up window is
        // armed inside `startCall`, so this publisher emits before `startCall`
        // returns on every single conversation. Subscribed after it, the controller
        // would miss the first `.quiet` of every call — and on a false wake, where
        // nothing else ever arrives, `.quiet` is followed only by the `.ended` that
        // is the sole route back to the wake word.
        lifecycleSubscription = call.lifecyclePublisher.sink { [weak self] event in
            guard let self else { return }
            self.latestLifecycle.withLock { $0 = event }
            if Thread.isMainThread {
                MainActor.assumeIsolated { self.applyLatestLifecycle() }
            } else {
                Task { @MainActor [weak self] in self?.applyLatestLifecycle() }
            }
        }
        // Before `startCall` like its two siblings — the publisher does not
        // replay, and the user's own opening request can finalise during the
        // connect. Same box and same hop idiom too, because the seam constrains
        // the sink's methods, not its publisher: the send can arrive from any
        // thread, and unstructured main-actor hops carry no ordering, so the
        // line is recorded synchronously and the hop only says "read the box" —
        // see `latestCaption` for why last-wins is also the cheaper cadence.
        captionSubscription = call.captionPublisher.sink { [weak self] line in
            guard let self else { return }
            self.latestCaption.withLock { $0 = line }
            if Thread.isMainThread {
                MainActor.assumeIsolated { self.applyLatestCaption() }
            } else {
                Task { @MainActor [weak self] in self?.applyLatestCaption() }
            }
        }
        var connected = await call.startCall()
        // One automatic retry, and only with the window still live and still
        // OURS — the same pair the post-connect guard below re-checks, for the
        // same reason: a disarm can have run inside the await, and a retry for a
        // window that is gone would connect a socket nobody can stop.
        //
        // Why retry at all: the user committed these seconds at the wake — they
        // have already stood through one whole failed connect — and giving up
        // costs the wake itself: the fallback re-arms the spotter, but the user
        // has to notice the fallback and say the phrase again. A second wait is
        // cheap next to that. Both attempts log distinctly, so a device trace
        // can tell one slow connect from two failed ones.
        if !connected, callPhaseIsActive, armedAt == window {
            log.warning("Ambient connect failed; retrying once after the settle pause.")
            // NOT in the same runloop breath as the failure. The retry used to
            // re-enter the still-disturbed audio session immediately — on the
            // first wake of a process, the one the just-paid voice-processing
            // rebuild had destabilised — and die exactly as the first attempt
            // did. The pause gives mediaserverd time to settle; with VP latched
            // true from the first attempt's own rebuild, the second start has a
            // real chance. Cost accepted: a genuinely failed connect takes this
            // much longer to fall back to armed.
            if connectRetryDelay > 0 {
                try? await Task.sleep(nanoseconds: UInt64(connectRetryDelay * 1_000_000_000))
            }
            // The pause is a suspension point like the connect itself: a disarm
            // can land inside it, and a retry for a window that is gone would
            // connect a socket nobody can stop — the same pair, re-checked.
            if callPhaseIsActive, armedAt == window {
                // A fresh attempt deserves a fresh gauge: re-publish `heard` so
                // the island's give-up clock (`ContentState.connectingSince`)
                // restarts with the attempt it depicts — a gauge still draining
                // the FIRST attempt's window over the second attempt would be
                // depicting a deadline nobody is enforcing anymore. NOT
                // announcing: the wake spent its one earned alert already. One
                // extra rate-budgeted publish per retry, priced where the
                // budget is documented (`AmbientOrbPhase`'s header).
                activity.update(phase: .heard, announcing: false)
                connected = await call.startCall()
                if !connected {
                    log.error("Ambient connect retry failed too; falling back to armed.")
                }
            }
        }
        // Those awaits release the main actor for the whole connect — and for
        // the retry — and `disarm` can have run start to finish inside either,
        // needing no user at all: the battery rails act on their own, and the
        // cap timer, though it now defers to a wake in flight (`startCapTimer`),
        // still disarms from every other state. Everything below assumes a
        // window that still exists, so re-check before touching any of it.
        // Without this, a failed connect restarts the tap with no orb, no arm
        // record, no cap timer and no focus token: a microphone nothing can stop.
        guard callPhaseIsActive, armedAt == window else {
            // Our window is gone. `disarm` already stopped the mic, dropped the
            // subscription and released focus; the one thing it could not do is
            // end a call that had not connected yet, so that part is ours.
            //
            // Unless a LATER window has taken the sink over in the meantime, in
            // which case the call that answered is no longer the one we asked
            // for and hanging it up would cut off a live conversation belonging
            // to the window that replaced ours.
            //
            // A conversation that ENDED during the connect also lands here, and
            // for it neither reading is quite right: `handleCallEnded` has already
            // hung up and queued the resume, so there is nothing left to do. The
            // `endCall` below then fires a second time, which is the documented-safe
            // double rather than a mistake, and the `return` is what matters —
            // without it this would fall through and describe a finished call as
            // `conversing`.
            //
            // When it does fire, `endCall` is the second one on this path by
            // design: disarm's went out before the socket came up and may have
            // found nothing to end.
            if connected, !callPhaseIsActive { call.endCall() }
            return
        }
        guard connected else {
            // Recoverable: fall back to armed so the wake word keeps working.
            turnSubscription = nil
            captionSubscription = nil
            lifecycleSubscription = nil
            latestTurn.withLock { $0 = nil }
            latestLifecycle.withLock { $0 = nil }
            latestCaption.withLock { $0 = nil }
            // Every other teardown clears the applied line with the boxes; the
            // symmetry costs less than its absence costs a reader.
            latestTranscript = nil
            // The cap fired during this connect and was deferred — see
            // `startCapTimer`. The connect it yielded to has now failed, so the
            // leash collects: the window closes with the cap's own reason, and
            // the connect notice below is deliberately NOT set, because "say the
            // wake word to try again" over a window the cap is closing would
            // invite a wake nothing is listening for.
            if capExpiredDuringWake {
                disarmNow(reason: AmbientEndedReason.capReached)
                return
            }
            // Out of the call phase BEFORE the resume suspends, exactly as
            // `handleCallEnded` does and for the same two reasons. Left in
            // `.connecting`, the orb would assert `heard` — "I heard you, I am
            // connecting" — for the whole 2.5 s cooldown after the connect had
            // *already failed*, with the microphone off. That is the MOST-claiming
            // phase reachable here; `.armed` is the least, and it is where this is
            // going anyway. It also stops a turn or lifecycle event from the call
            // that never came up moving the state, since both apply only while
            // `callPhaseIsActive`.
            state = .armed
            // The one fallback that used to be SILENT, and the notice is the
            // fix: both attempts failed, the user is not looking at their phone,
            // and an orb that simply dims back to armed reads as the wake never
            // happening. The sentence is published now but built to be read
            // later — `connectNotice` says why it stands until the next activation or
            // the window's end — and it is generic by decision; the specific
            // cause is in the sink's log (`AmbientCallSink.startCall`).
            connectNotice = Self.connectFailedNotice
            publishOrbCaption()
            await resumeSpotting()
            return
        }
        // Only default to `listening` if no turn has arrived — otherwise this
        // would clobber a `thinking` that beat us here. Asking `latestTurn`
        // rather than `state` is what makes that true even when the turn's
        // main-actor hop has not landed yet.
        if latestTurn.withLock({ $0 }) == nil {
            state = .conversing(.listening)
        } else {
            applyLatestTurn()
        }
        // LAST, over the turn, and the ordering is the decision. A lifecycle event
        // can arrive during the connect — the follow-up window is armed the moment
        // the session is ready — and it is strictly newer information than the turn
        // above: `.quiet` says the same microphone is open with a deadline on it,
        // and `.ended` says the conversation is already over. Applied first, either
        // would then be overwritten by `conversing(.listening)`, leaving the orb
        // describing a call that had finished and no route back to the wake word.
        applyLatestLifecycle()
    }

    /// Publishes whatever the call last reported. Reads the box rather than
    /// taking a turn as an argument, so a hop that lands late cannot install a
    /// stale turn over a newer one; re-applying the same turn is a no-op.
    private func applyLatestTurn() {
        guard let turn = latestTurn.withLock({ $0 }) else { return }
        // A hop can land after the window closed. Do not resurrect it.
        guard callPhaseIsActive else { return }
        state = .conversing(turn)
        // AFTER the state, and that ordering is the whole cost control. The phase
        // change above already published, and `AmbientActivity.update(phase:announcing:)`
        // clears the span in that same publish — so on the way into a reply this
        // finds nothing new to say and spends nothing. It is only the sink's
        // "the queue has settled" re-emission, where the state does not change,
        // that publishes: one extra update per reply, in exchange for a bar drawn
        // across a measured deadline instead of a guessed one.
        publishOrbSpeaking()
    }

    /// Puts the newest finalised transcript line on the orb. Reads the box
    /// rather than taking the line as an argument, for the reason
    /// `applyLatestTurn` gives: a hop that lands late cannot install a stale
    /// line over a newer one. The sink emits finals only, so the caption spends
    /// the budget of speech rather than of tokens.
    ///
    /// Guarded by `callPhaseIsActive` rather than `windowIsLive`, and the gap
    /// between them is exactly the failure this guard is for: a caption
    /// describes the CURRENT conversation, and after `handleCallEnded` the
    /// window is still live — `.armed`, waiting for the next wake — with the
    /// blank already published. A hop that lands there under `windowIsLive`
    /// would repaint the finished conversation's words over that blank; the
    /// same class of bug as a resurrected window, caught by the same guard the
    /// turn path uses.
    private func applyLatestCaption() {
        guard let line = latestCaption.withLock({ $0 }) else { return }
        guard callPhaseIsActive else { return }
        latestTranscript = line
        publishOrbCaption()
    }

    /// Publishes whatever the call last said about its own ending. Reads the box
    /// rather than taking the event as an argument, for the reason
    /// `applyLatestTurn` gives: a hop that lands late cannot install a stale
    /// event over a newer one.
    ///
    /// Both cases are idempotent, which matters because two hops can be queued at
    /// once. `.quiet` re-assigns the same deadline. `.ended` leaves the call phase
    /// as its first act, so the guard below turns a second delivery into a no-op —
    /// without which a `.dropped` chasing a `.wentQuiet` would start a second
    /// resume, and two overlapping resumes are two attempts to reinstall one tap.
    private func applyLatestLifecycle() {
        guard let event = latestLifecycle.withLock({ $0 }) else { return }
        // A hop can land after the window closed. Do not resurrect it.
        guard callPhaseIsActive else { return }
        switch event {
        case .quiet(let until):
            state = .cooldown(until: until)
        case .ended(let cause):
            handleCallEnded(cause)
        }
    }

    /// The conversation is over. Hang up, and give the window back to the wake
    /// word.
    ///
    /// **This is the route that makes an armed window worth arming.** Without it a
    /// window holds exactly one conversation and then sits in `.conversing` until
    /// the cap expires or the user reaches for their phone — which is strictly
    /// worse than the tap-to-talk it replaced, since the whole justification for a
    /// wake word is talking repeatedly without touching anything.
    ///
    /// **All three causes return to `armed`. None of them disarms**, and the
    /// reasoning is the same one in three shapes: an ambient window is a *local*
    /// microphone lease with no server-side existence (see `reconcileOnLaunch`),
    /// and the wake spotter that guards it is entirely on-device. So nothing the
    /// network does is evidence about whether the user still wants to be heard.
    /// `wentQuiet` is the exchange finishing normally. `remote` is the server
    /// closing one conversation, which says nothing about the next one. `dropped`
    /// is the transport having exhausted its own reconnect backoff, and design §9
    /// already decided that case explicitly — *"on exhaustion fall back to
    /// `.armed`, not `.off`, so the wake word survives"* — which is also the
    /// disposition `handoff` already gives a connect that never came up.
    ///
    /// The one exception is not the network's doing: a cap that expired while
    /// this wake was in flight. The leash deferred to the connect (see
    /// `startCapTimer`), and the end of the conversation it bought is where it
    /// collects — `capExpiredDuringWake` below. That is still the cap's own
    /// decision about a window that ran its course, not the transport's about
    /// the user.
    ///
    /// The window still ends when the *microphone* is what was lost, rather than
    /// the conversation: `resumeSpotting`'s `mic.start` throws and disarms with a
    /// reason. That is the one failure that must not fall back to `armed`, because
    /// an orb over a tap that cannot open is the lie this feature is built to
    /// refuse — and it is detected where it actually happens rather than inferred
    /// from a socket.
    private func handleCallEnded(_ cause: AmbientCallEnded) {
        guard callPhaseIsActive else { return }
        // The deferred cap collects here instead of the window returning to
        // armed. `disarmNow` does everything the return path below does — hangs
        // up, drops the subscriptions, clears the boxes and the caption — and
        // then closes the window with the cap's own reason: the sentence the orb
        // would have shown at the boundary, had the wake not been in flight.
        if capExpiredDuringWake {
            disarmNow(reason: AmbientEndedReason.capReached)
            return
        }
        turnSubscription = nil
        // Same lifetime as the turn subscription, torn down at every site it is:
        // an orphaned caption subscription republishing after the window moved
        // on is the resurrected-window bug class, worn as a caption.
        captionSubscription = nil
        lifecycleSubscription = nil
        latestTurn.withLock { $0 = nil }
        latestLifecycle.withLock { $0 = nil }
        latestCaption.withLock { $0 = nil }
        // A caption describes the CURRENT turn, so a finished conversation's last
        // line must not lead the next conversation's caption in this same window.
        // The blank PUBLISHES here, not merely clears: a window back at `.armed`
        // must not keep the last conversation's words on the lock screen, and a
        // conversation end is rare, so the one extra publish it costs is bounded
        // by ends rather than by lines. It recomposes rather than erases — the
        // power warning, when the window carries one, is what the caption
        // returns to.
        latestTranscript = nil
        publishOrbCaption()
        // Bridge the two audio graphs BEFORE the call graph stops. Merely leaving
        // `AVAudioSession` activated is insufficient on device: stopping a
        // voice-processing AudioUnit can lapse the recording session underneath
        // it. Silent output keeps real I/O alive until `resumeSpotting` has
        // successfully installed the wake input graph.
        if !resumeKeepalive.start() {
            log.warning("Ambient audio ownership bridge failed to start; attempting direct wake resume.")
        }
        // The controller hangs up, never the sink — see
        // `AmbientCallSink.lifecyclePublisher`. Safe for `.remote` and `.dropped`,
        // where the transport has already reached a terminal phase: `endCall` is
        // contractually safe with nothing to end.
        call.endCall()
        // Out of the call phase BEFORE the resume suspends, for two reasons.
        //
        // It is what makes a second delivery a no-op (see `applyLatestLifecycle`),
        // and it is the honest orb. For the 2.5 s resume cooldown the provider
        // call is gone and `resumeSpotting` keeps local audio I/O alive while
        // gating every frame away from the decoder. `.cooldown` and
        // `.conversing(.listening)` would both claim the assistant is accepting a
        // follow-up, which it is not, so the least-claiming state wins. `.armed`
        // is the orb's dimmest phase and the state the window is heading to
        // anyway; `wakeDecoderIsSpotting` remains false until the decoder gate
        // opens, so an explicit activation waits and a wake hit cannot be admitted.
        state = .armed
        pending = Task { [weak self] in await self?.resumeSpotting() }
    }

    /// Whether a call is in progress for SOME window — deliberately not "for
    /// mine", which is why the post-connect re-check pairs this with an
    /// `armedAt` comparison.
    ///
    /// `.conversing` counts, not just `.connecting`: a turn arriving during the
    /// connect legitimately moves the state on, and treating that as "no longer
    /// ours" would end a call that had only just come up.
    ///
    /// **`.cooldown` counts too.** During the follow-up window the microphone is
    /// still the call's and still open for the user, so a call is very much in
    /// progress — a turn arriving then is a follow-up question, not a stale hop
    /// from a window that closed. This one predicate is shared by the post-connect
    /// re-check, `applyLatestTurn` and `applyLatestLifecycle`, so the case covers
    /// all three.
    private var callPhaseIsActive: Bool {
        switch state {
        case .connecting, .conversing, .cooldown: return true
        default: return false
        }
    }

    /// Put local audio I/O back immediately, then admit it to the wake decoder
    /// only after the acoustic-tail cooldown. See `wakeResumeCooldown`.
    ///
    /// The cooldown is applied on every route back to spotting rather than only
    /// after a conversation, because the audio it is protecting against is
    /// present on both: a failed connect still leaves the tail of the user's own
    /// request in the air, and a conversation that ended leaves the tail of the
    /// reply. Both routes come through here — `handoff`'s failed connect and
    /// `handleCallEnded` — which is what keeps "the spotter never hears the tail"
    /// a property of the controller rather than of one call site. Keeping the
    /// tap alive while its frames are discarded is equally important: on a
    /// locked/backgrounded device, an active session with no active I/O is not a
    /// background-execution guarantee, and the sleeping task may never get the
    /// chance to reopen the microphone.
    private func resumeSpotting() async {
        // The window this resume belongs to, captured BEFORE any suspension
        // below. `armedAt` changes on every arm, so comparing it afterwards
        // separates "my window is still open" from "a later window is open",
        // which a state-shaped check cannot tell apart.
        guard let window = armedAt else { return }
        wakeDecoderEnabled.withLock { $0 = false }

        // Up to three asks for the tap, spaced short — but only for failures a
        // retry could actually outlive. A FOREGROUNDED re-arm runs session
        // activation, so its first failure can be a real transient (an
        // interruption still resolving, the audio system mid-settle) that
        // settles out by itself; one throw must not spend a window the user
        // armed on purpose. A BACKGROUNDED re-arm skips activation by design
        // (DTS 826462), so a failed start there is StartIO refusing an
        // inactive session — attempt N+1 fails identically, and the 0.1.158
        // device trace showed each blocking session round stalling the main
        // actor for visible seconds. So the foreground state is re-read before
        // every retry, and backgrounded failures disarm on the first throw,
        // exactly as before the retries existed. The honesty cost is owned:
        // between foregrounded attempts the orb claims `armed` over no tap,
        // for up to ~2 s. Kept short, and the
        // final disarm is kept — a microphone that stays gone is still a dead
        // window, and claiming otherwise is this feature's signature failure.
        var started = false
        for attempt in 1 ... Self.micStartAttempts {
            guard armedAt == window else { return }
            do {
                try mic.start(onFrame: { [weak self] frame in self?.ingest(frame) }, onFailure: { [weak self] error in
                    self?.handleMicFailure(error)
                })
                resumeKeepalive.stop()
                started = true
                break
            } catch {
                // A permission failure is final on the FIRST throw: retrying
                // would nag a microphone the user just turned off. **This is
                // where a permission revoked mid-window lands after a
                // conversation**, and it needs no detection of its own:
                // `AmbientMicEngine.handleDidBecomeActive` only fires while a
                // tap is running, and there is no tap during a call, so a
                // revocation between the wake hit and here is invisible to it.
                // `mic.start` then throws `recordPermissionMissing` and the
                // reason says so rather than suggesting the user try again at
                // something that never will.
                if Self.isPermissionFailure(error) || attempt == Self.micStartAttempts || !isAppForegrounded() {
                    await disarm(reason: Self.micFailureReason(error))
                    return
                }
                log.warning("Ambient re-arm attempt \(attempt, privacy: .public) failed; retrying: \(error.localizedDescription, privacy: .public)")
                if micRetryDelay > 0 {
                    try? await Task.sleep(nanoseconds: UInt64(micRetryDelay * 1_000_000_000))
                }
                // The pause is a suspension point: a disarm can land inside it,
                // and a retry then would open a microphone for a window that is
                // already gone.
                guard armedAt == window else { return }
            }
        }
        guard started else { return }
        log.info(
            "Ambient wake I/O restored; decoder gated for \(self.resumeCooldown, privacy: .public)s."
        )

        // The tap is active, which keeps the already-open ambient audio session
        // and process alive, but `wakeDecoderEnabled` is still false: the
        // assistant's tail and the user's final syllables are discarded, not
        // decoded. The suspension is therefore safe for both wake accuracy and
        // iOS background execution.
        if resumeCooldown > 0 {
            do {
                try await Task.sleep(nanoseconds: UInt64(resumeCooldown * 1_000_000_000))
            } catch {
                // Disarm and unwind both cancel this task and synchronously stop
                // the tap. Do not resurrect it after either teardown.
                return
            }
        }

        // The cooldown releases the main actor; the cap timer or user may have
        // taken the window down while active I/O kept the task runnable. Without
        // this identity check a stale resume could enable a decoder underneath a
        // later window or with no orb at all.
        guard armedAt == window else { return }
        spotter.reset()
        wakeDecoderEnabled.withLock { $0 = true }
        wakeDecoderIsSpotting = true
        state = .armed
        log.info("Ambient wake decoder resumed for the existing window.")
    }

    /// A tap that died mid-window. The orb is claiming the user is being heard
    /// and no audio is reaching the spotter, so the window is over — same
    /// destination `resumeSpotting`'s catch already picks.
    ///
    /// Wrapped in a `Task` because the channel is synchronous and `disarm` is
    /// not. The task INHERITS main-actor isolation rather than escaping it —
    /// `Task.detached` here would be wrong, since every line of `disarm` is
    /// main-actor state. The cap timer reaches `disarm` the same way.
    private func handleMicFailure(_ error: Error) {
        Task { [weak self] in await self?.disarm(reason: Self.micFailureReason(error)) }
    }

    // MARK: - cap

    /// The leash on a microphone the user is not watching.
    ///
    /// **The leash yields to a wake in flight.** The cap exists to bound an IDLE
    /// armed microphone — hours of silence nobody is attending to — and a wake
    /// mid-connect is the opposite of idle: it is the one moment the window
    /// exists to permit, already paid for with the user's own speech. Torn down
    /// at the boundary it produced exactly the failure this feature refuses
    /// everywhere else — "Starting conversation…" followed by silence — so a cap
    /// landing in `.heard`/`.connecting` latches instead
    /// (`capExpiredDuringWake`) and is honoured when the wake resolves: on a
    /// failed connect, or at the end of the conversation a successful one
    /// bought. The deferral is bounded on both exits, though not by one number:
    /// the failed exit by the two connect attempts (plus the settle pause
    /// between them), each on the sink's
    /// ready-wait clock; the successful exit by the conversation the wake
    /// bought, whose every lull the quiet window bounds. So the leash slips by
    /// a finished exchange — microphone-ACTIVE time, the thing the leash was
    /// never for — and never indefinitely. Every other state disarms
    /// immediately, exactly as before: a conversation already RUNNING at the
    /// boundary was and is cut off, because by then the user has had the
    /// exchange the wake was for.
    private func startCapTimer(seconds: TimeInterval) {
        capTimer?.cancel()
        capTimer = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.handleCapExpiry()
        }
    }

    /// The cap timer's landing, split from the timer so the decision reads at
    /// the state machine rather than inside a task closure — and so a test can
    /// land the boundary at an exact instant, which a real timer cannot promise.
    /// The timer above is its only production caller; the test hook is the same
    /// licence `settle()` and `handleDisarmSignal` already take.
    func handleCapExpiry() async {
        switch state {
        case .heard, .connecting:
            // See `startCapTimer`: the leash yields to the wake it exists to
            // permit. `handoff`'s failed-connect path and `handleCallEnded`
            // honour the latch.
            capExpiredDuringWake = true
        default:
            await disarm(reason: AmbientEndedReason.capReached)
        }
    }

    // MARK: - orb

    /// Everything the orb's caption line says, composed in one place.
    ///
    /// **A single composition point rather than a call to `updateCaption` at each
    /// site that has something to say.** `AmbientActivity.updateCaption` REPLACES
    /// the line, so independent writers would each blank the others — and the
    /// power warning is the one caption that has to survive the whole window
    /// (`powerWarning` says why). The transcript caption and the connect notice
    /// compose here rather than overwriting it, which is the collision the
    /// single point exists to make unwritable.
    private func publishOrbCaption() {
        guard orbIsLive else { return }
        let line = Self.orbCaption(
            powerWarning: powerWarning,
            notice: connectNotice,
            transcript: latestTranscript
        )
        activity.updateCaption(line.text, role: line.role)
    }

    /// Pure and `static` so the composition is assertable without ActivityKit.
    /// Precedence follows the lines' lifetimes, longest-standing first. The
    /// power warning wins outright: it is the one line that must survive the
    /// whole window. The notice-over-transcript rank is defensive doctrine for
    /// a pure function rather than a reachable collision: in production the
    /// notice composes over a nil transcript by construction — the fallback
    /// that sets it has just cleared `latestTranscript`, and every conversation
    /// end clears it again before the next one could speak. Both system
    /// sentences carry no speaker.
    ///
    /// Warning-wins also starves the activity's exchange counting:
    /// `AmbientActivity.exchangeCount` counts PUBLISHED agent lines, and a
    /// window whose caption the warning holds publishes none, so its receipt
    /// omits the exchange clause even when exchanges happened. An accepted
    /// omission rather than an accident — the receipt degrades by saying less,
    /// never by claiming a false zero. Counting upstream of this composition
    /// is the shape of the fix if it ever matters.
    nonisolated static func orbCaption(
        powerWarning: AmbientPowerBlock?,
        notice: String?,
        transcript: AmbientCaptionLine?
    ) -> (text: String, role: AmbientCaptionRole?) {
        if let warning = powerWarning?.warning { return (warning, nil) }
        if let notice { return (notice, nil) }
        guard let transcript else { return ("", nil) }
        return (transcript.text, transcript.role)
    }

    /// The reply's measured span, on its OWN `ContentState` field.
    ///
    /// Deliberately not routed through `publishOrbCaption`: that composes one
    /// replaceable line of text, and the power warning has to hold it for the whole
    /// window. A span published as caption text would blank the warning — the exact
    /// regression the single composition point exists to prevent — so this is a
    /// separate field with a separate verb, and the two cannot collide.
    ///
    /// Reads the sink rather than taking an argument, for the same reason
    /// `applyLatestTurn` reads its box: whatever the sink says now is newer than
    /// anything a caller could have captured, and re-publishing the same value is
    /// deduplicated in `AmbientActivity` rather than guarded here.
    private func publishOrbSpeaking() {
        guard orbIsLive else { return }
        activity.updateSpeaking(span: call.speakingSpan)
    }

    /// Pick the ActivityKit verb from the two reductions. `orbPhaseChanged` is
    /// only the *phase* half of the decision — see its docstring — so captions
    /// are published separately, and the end carries the disarm reason.
    private func publishIfNeeded(from old: AmbientState, to new: AmbientState) {
        switch (old.orbPhase, new.orbPhase) {
        case (nil, .some(let phase)):
            // No window, no orb. See `armedAt`.
            guard let armedAt, let expiresAt else { break }
            orbIsLive = activity.start(phase: phase, armedAt: armedAt, expiresAt: expiresAt)
            resetPhaseWordPulse(for: phase)
        case (.some, nil):
            // `disarming` reduces to nil, so this is where its reason reaches
            // `ContentState.endedReason` — the only path that publishes it.
            resetPhaseWordPulse(for: nil)
            guard orbIsLive else { break }
            var reason: String?
            if case .disarming(let value) = new { reason = value }
            activity.end(reason: reason)
            orbIsLive = false
        case (.some(let oldPhase), .some(let phase)) where oldPhase != phase:
            // The island auto-presents its expanded view for an alert-carrying
            // update. A WAKE earns that interruption: the user just spoke to a
            // lock screen and deserves to see the full surface answer, Stop and
            // Open in reach without a long-press. An explicit Talk tap does not:
            // iOS has already foregrounded Magican, and an alert would add a sound
            // to an action the user is watching. The empty associated phrase is
            // the controller's structural marker for that explicit activation.
            // Connecting re-entries and the cooldown's return to listening are
            // the machine moving and never announce either.
            let announcesWake: Bool
            if case .heard(let phrase) = new {
                announcesWake = oldPhase == .armed && !phrase.isEmpty
            } else {
                announcesWake = false
            }
            activity.update(phase: phase, announcing: announcesWake)
            resetPhaseWordPulse(for: phase)
        default:
            break
        }
    }

    /// Restart the compact phase-word pulse for a freshly published phase, or
    /// cancel it outright for one that has none.
    ///
    /// The pulse is APP-driven because nothing out-of-process can schedule:
    /// every show and every hide of the compact word is a publish. The show at
    /// each transition rides the phase publish that was happening anyway (the
    /// sink derives the flag — `AmbientActivity.phaseWordOnPublish`); this
    /// timer speaks only the mid-phase flips, ≤2 rate-budgeted publishes per
    /// cadence and ONLY while conversing — a bounded few minutes, with the
    /// armed emptiness rule untouched. `.cooldown` reduces to `.listening`, so
    /// a lull neither restarts nor kills the pulse: the word stays honest
    /// because it always equals the published phase, which is also why a
    /// dropped hide-publish merely leaves a TRUE word up longer.
    ///
    /// Every scheduled flip re-checks the window identity AND that the phase
    /// is still the one it was scheduled for — the same disarm/supersede idiom
    /// as the connect and the resume — so a pulse can never publish against a
    /// window that disarmed or a phase that moved on; a phase change lands in
    /// `publishIfNeeded`, which cancels this task and starts the next phase's.
    /// The guard requiring cadence > show > 0 is what makes misconfiguration
    /// degrade safely: equal or inverted values disable the pulse (the word
    /// then rides transitions only) rather than spinning publishes through a
    /// zero-length hidden window, and a zero-length SHOW disables it too —
    /// otherwise it would spend two publishes per cadence on a word no frame
    /// ever renders. Zeroing both is the test suite's lever, same idiom as
    /// `resumeCooldown`.
    private func resetPhaseWordPulse(for phase: AmbientOrbPhase?) {
        phaseWordPulse?.cancel()
        phaseWordPulse = nil
        guard let phase, AmbientActivity.phaseWordOnPublish(for: phase) else { return }
        guard phaseWordCadenceSeconds > phaseWordShowSeconds, phaseWordShowSeconds > 0 else { return }
        let window = armedAt
        phaseWordPulse = Task { [weak self] in
            while true {
                guard let self, !Task.isCancelled else { return }
                try? await Task.sleep(nanoseconds: UInt64(self.phaseWordShowSeconds * 1_000_000_000))
                guard !Task.isCancelled, self.armedAt == window, self.state.orbPhase == phase else { return }
                self.activity.setPhaseWordVisible(false)
                let hidden = max(0, self.phaseWordCadenceSeconds - self.phaseWordShowSeconds)
                try? await Task.sleep(nanoseconds: UInt64(hidden * 1_000_000_000))
                guard !Task.isCancelled, self.armedAt == window, self.state.orbPhase == phase else { return }
                self.activity.setPhaseWordVisible(true)
            }
        }
    }
}
