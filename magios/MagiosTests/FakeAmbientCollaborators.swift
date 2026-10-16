import Combine
import Foundation
@testable import Magician

/// Test doubles for the four ambient collaborator seams.
///
/// They exist so the ambient controller can be driven end to end with no
/// microphone, no socket and no ActivityKit — none of which the simulator
/// offers.
///
/// Each of the three seams that can REFUSE models that refusal, not just the
/// happy path, because a double that is merely convenient lets a broken
/// controller pass: `FakeAmbientMicSource.startError`,
/// `FakeAmbientCallSink.failToStart`, `FakeAmbientActivitySink.failToStart`.
/// `FakeWakeSpotter` has no such flag because a spotter does not fail — it
/// simply does not hit, which a test expresses by not calling `simulateHit`.

// MARK: - WakeSpotter

/// Records what it was configured with and how much audio it was fed, and lets a
/// test fire a hit at the exact moment it wants one.
///
/// There is nothing here to reach the network — same as the real thing, and for
/// the same reason.
final class FakeWakeSpotter: WakeSpotter {
    /// The current match set. `configure` REPLACES it, because the real spotter
    /// replaces — fidelity is the only reason. Note what that costs: repeat
    /// `configure` calls leave no trace, so this cannot catch a controller that
    /// reconfigures more often than it should. Add a counter if that becomes
    /// worth asserting; do not make the fake accumulate, which would diverge
    /// from the real behaviour to buy the detection.
    private(set) var configuredPhrases: [String] = []
    private(set) var fedByteCount = 0
    private(set) var resetCount = 0

    var onHit: (@MainActor (WakeHit) -> Void)?

    /// Phrases this spotter refuses outright, as `vosk_model_find_word` does for
    /// a word the model has never heard. Matched case-insensitively against what
    /// `configure` was handed.
    var phrasesToReject: Set<String> = []

    /// Notes for phrases that arm, keyed by phrase. Separate knob from
    /// `phrasesToReject` because the two need separate answers from the arming
    /// path — one refuses, the other only reports — and a double that conflated
    /// them would let a controller that refused on a note pass.
    ///
    /// A phrase with no entry still gets a note, because the real spotter always
    /// has one: `VoskWakeSpotter.assessment(of:)` is total, and "nobody measured
    /// this" is a note rather than a silence. A fake that emitted nothing for
    /// unlisted phrases would rebuild exactly the gap the real type stopped
    /// having.
    var notesToReport: [String: String] = [:]

    /// The note an armed phrase gets when the test did not name one.
    static let unmeasuredNote = "This phrase has never been measured."

    /// The recognizer could not be built even though every phrase passed the
    /// lexicon check. Rare, real, and the reason `isArmed` is on the protocol at
    /// all rather than being inferred from `rejectedPhrases` — inferred, this
    /// state is invisible and arms an orb over a spotter that can never fire.
    var isInert = false

    private(set) var rejectedPhrases: [String] = []
    private(set) var phraseNotes: [PhraseNote] = []

    var isArmed: Bool {
        !isInert && !configuredPhrases.isEmpty && rejectedPhrases.count < configuredPhrases.count
    }

    func configure(phrases: [String]) {
        configuredPhrases = phrases
        rejectedPhrases = phrases.filter { phrase in
            phrasesToReject.contains { $0.caseInsensitiveCompare(phrase) == .orderedSame }
        }
        // Only phrases that armed get a note — a rejected phrase is reported by
        // `rejectedPhrases` and has no measurement to carry.
        phraseNotes = phrases
            .filter { !rejectedPhrases.contains($0) }
            .map { phrase in
                let match = notesToReport.first { $0.key.caseInsensitiveCompare(phrase) == .orderedSame }
                return PhraseNote(phrase: phrase, note: match?.value ?? Self.unmeasuredNote)
            }
    }

    func feed(_ pcm: Data) {
        fedByteCount += pcm.count
    }

    func reset() {
        resetCount += 1
    }

    /// `@MainActor` because `onHit` is: the seam types the hop rather than
    /// documenting it, and a double that could fire a hit off the main actor
    /// would be exercising a path the real spotter cannot take.
    @MainActor
    func simulateHit(phrase: String = "hey sam", at: Date = Date()) {
        onHit?(WakeHit(phrase: phrase, at: at))
    }
}

// MARK: - AmbientMicSource

/// A tap that hands frames to whoever is listening, and refuses to start when
/// told to.
final class FakeAmbientMicSource: AmbientMicSource {
    /// Something to throw when a test does not care which error it is; the real
    /// failures are opaque `AVAudioEngine` / audio-session `NSError`s.
    enum StartFailure: Error { case unavailable }

    private(set) var isRunning = false
    /// Counts ATTEMPTS, including failed ones — a controller that retries a
    /// denied microphone forever is the thing worth catching.
    private(set) var startCount = 0
    private(set) var stopCount = 0

    var startError: Error?

    /// Scripted per-attempt failures, consumed first-in-first-out by `start`;
    /// once exhausted — or never written — the flat `startError` answers, as
    /// before. Exists because `resumeSpotting` retries a transient start
    /// failure, so "throws once, then succeeds" is a reachable production shape
    /// a single stored error cannot express — same idiom, and the same reason,
    /// as `FakeAmbientCallSink.scriptedStartResults`.
    var scriptedStartErrors: [Error?] = []

    /// Fired from inside `stop()`, so a test can read the world at the instant the
    /// microphone goes off rather than after the operation that stopped it has
    /// returned.
    ///
    /// Same need `FakeAmbientCallSink.onStartCall` exists for: an assertion taken
    /// afterwards cannot tell "before" from "after", and there are claims in this
    /// feature whose entire meaning is the ordering — the cross-process
    /// acknowledgement says the microphone is already off, so a test that cannot
    /// see inside the stop cannot tell that promise from a fact.
    var onStop: (() -> Void)?

    private var onFrame: ((Data) -> Void)?
    private var onFailure: (@MainActor (Error) -> Void)?

    func start(onFrame: @escaping (Data) -> Void, onFailure: @escaping @MainActor (Error) -> Void) throws {
        startCount += 1
        // A failed start leaves NO tap: neither closure is retained and
        // `isRunning` stays false, so a controller that carries on as if
        // audio were flowing shows up as silence rather than passing.
        if !scriptedStartErrors.isEmpty {
            if let error = scriptedStartErrors.removeFirst() { throw error }
        } else if let startError {
            throw startError
        }
        self.onFrame = onFrame
        self.onFailure = onFailure
        isRunning = true
    }

    func stop() {
        stopCount += 1
        isRunning = false
        onFrame = nil
        onFailure = nil
        onStop?()
    }

    /// Kill a RUNNING tap, as an interruption, a route change or a revoked
    /// permission does. A no-op before `start` or after `stop`, because a tap
    /// that is not running cannot die. `@MainActor` because the seam types the
    /// hop, so a double that could fire from anywhere would exercise a path the
    /// real tap cannot take.
    @MainActor
    func simulateFailure(_ error: Error = StartFailure.unavailable) {
        onFailure?(error)
    }

    /// Push one frame through the stored closure. A no-op before `start` and
    /// after `stop`, holding the fake to the contract `AmbientMicSource.stop()`
    /// puts on the real engine: no callback in flight or delivered once `stop`
    /// returns. A double that kept delivering would be modelling the thing the
    /// engine is explicitly required to prevent.
    func emit(_ data: Data) {
        onFrame?(data)
    }
}

// MARK: - Ambient audio-session bridge

/// Silent-I/O bridge used to prove provider teardown cannot create a session
/// ownership gap before the wake microphone starts.
@MainActor
final class FakeAmbientSessionKeepalive: AmbientDictationKeepingAlive {
    var startSucceeds = true
    private(set) var startCount = 0
    private(set) var stopCount = 0
    private(set) var isActive = false

    func start() -> Bool {
        startCount += 1
        isActive = startSucceeds
        return startSucceeds
    }

    func stop() {
        guard isActive else { return }
        stopCount += 1
        isActive = false
    }
}

// MARK: - AmbientCallSink

/// Counts the connects it was asked for and lets a test drive the conversation
/// forward one turn at a time.
final class FakeAmbientCallSink: AmbientCallSink {
    /// One per `startCall`, INCLUDING a failed one. A count rather than a
    /// recorded payload since the no-pre-roll decision (2026-07-30): `startCall`
    /// carries nothing, so the attempts themselves are what tests assert over.
    private(set) var startCallCount = 0
    private(set) var endCount = 0

    /// One per `primeAudioGraph`, plus a hook fired from inside it (the
    /// arm-order claim — prime BEFORE the spotter's start — is only visible
    /// from in there, same need `onStartCall` exists for) and a scriptable
    /// verdict: the controller spends its once-latch only on success, so "a
    /// failed prime retries at the next arm" needs a fake that can refuse.
    private(set) var primeAudioGraphCount = 0
    var primeAudioGraphSucceeds = true
    var onPrimeAudioGraph: (() -> Void)?

    func primeAudioGraph() -> Bool {
        primeAudioGraphCount += 1
        onPrimeAudioGraph?()
        return primeAudioGraphSucceeds
    }

    var failToStart = false

    /// Scripted per-attempt results, consumed first-in-first-out by `startCall`;
    /// once the script is exhausted — or was never written — the flat
    /// `failToStart` answers, as before. Exists because the controller retries a
    /// failed connect once, so "fails, then succeeds" is a reachable production
    /// shape a single Bool cannot express: without this the retry's success path
    /// would be indistinguishable from a first attempt that never failed.
    var scriptedStartResults: [Bool] = []

    /// Fired from inside `startCall`, so a test can act at the one instant that
    /// is otherwise unreachable from outside: after the controller has had its
    /// chance to subscribe, but before `startCall` has returned.
    ///
    /// This is what makes the non-replaying `turnPublisher` contract testable at
    /// all. A controller that subscribes AFTER `startCall` drops a turn emitted
    /// here, and without this hook that bug is indistinguishable from correct
    /// behaviour — every turn a test could emit from the outside arrives after
    /// both orderings have already subscribed.
    var onStartCall: (() -> Void)?
    /// Fired while `endCall` is executing, so ownership-transfer tests can prove
    /// the silent bridge was already active before provider I/O stopped.
    var onEndCall: (() -> Void)?

    /// Parks `startCall` until `finishStartCall()` is called, so a test can run
    /// main-actor work — a disarm, the cap boundary landing — *inside* the
    /// connect.
    ///
    /// Without a real suspension point this double cannot express that at all:
    /// a same-actor async call that never suspends runs straight through without
    /// yielding the actor, so every sequential test sees an atomic handoff and
    /// the whole interleaved window is invisible. That window is where `disarm`
    /// races the connect, and it can need no user — the battery rails act on
    /// their own. (The cap timer no longer disarms there: it defers to a wake in
    /// flight and collects when the connect resolves, which is itself behaviour
    /// only reachable through this park.)
    var suspendStartCall = false
    private var startContinuation: CheckedContinuation<Void, Never>?

    private let turns = PassthroughSubject<AmbientTurn, Never>()
    private let captionLines = PassthroughSubject<AmbientCaptionLine, Never>()
    private let lifecycle = PassthroughSubject<AmbientCallLifecycle, Never>()

    var turnPublisher: AnyPublisher<AmbientTurn, Never> { turns.eraseToAnyPublisher() }

    /// Non-replaying, like the real one and like `turnPublisher` here: the real
    /// sink emits a line only at the instant a caption finalises, so a double
    /// that replayed the last line to a late subscriber would make the
    /// subscribe-before-`startCall` ordering untestable.
    var captionPublisher: AnyPublisher<AmbientCaptionLine, Never> { captionLines.eraseToAnyPublisher() }

    /// Settable, because the controller PULLS this rather than receiving it — so a
    /// test drives it by writing the value the real sink would have measured and
    /// then emitting the `.speaking` re-report the real sink uses to say "read it
    /// again". Nil by default, which is what the real sink reports for all but a
    /// settled reply.
    var speakingSpan: AmbientSpeakingSpan?

    /// Non-replaying, like the real one — and the reason bites harder here. The real
    /// sink arms the follow-up window INSIDE `startCall`, so its first `.quiet` is
    /// emitted before `startCall` returns; a replaying double would hand that to a
    /// late subscriber and make a controller that subscribes in the wrong order
    /// look correct. `onStartCall` is how a test reaches that instant.
    var lifecyclePublisher: AnyPublisher<AmbientCallLifecycle, Never> { lifecycle.eraseToAnyPublisher() }

    func startCall() async -> Bool {
        startCallCount += 1
        onStartCall?()
        if suspendStartCall {
            await withCheckedContinuation { startContinuation = $0 }
        }
        if !scriptedStartResults.isEmpty { return scriptedStartResults.removeFirst() }
        return !failToStart
    }

    /// Let a parked `startCall` return.
    func finishStartCall() {
        startContinuation?.resume()
        startContinuation = nil
    }

    func endCall() {
        endCount += 1
        onEndCall?()
        if emitEndedAfterHangUp { lifecycle.send(.ended(.remote)) }
    }

    func emit(_ turn: AmbientTurn) {
        turns.send(turn)
    }

    /// One finalised transcript line, as the real sink emits it: already reduced
    /// to role + text, finals only by construction — a partial has no way into
    /// this seam at all, which is the shape the pipeline's rate budget rests on.
    func emitCaption(_ line: AmbientCaptionLine) {
        captionLines.send(line)
    }

    /// Report a conversation going quiet, or ending.
    ///
    /// **Deliberately not tied to `endCall`.** The real sink is contractually
    /// forbidden from reporting a hangup the controller asked for, and a double
    /// that emitted one from `endCall` would model the forbidden implementation:
    /// every test of "disarm, then nothing comes back" would exercise the loop
    /// instead of the rule. A test that wants the forbidden behaviour has to ask
    /// for it explicitly, here — which is what `emitEndedAfterHangUp` is for.
    func emitLifecycle(_ event: AmbientCallLifecycle) {
        lifecycle.send(event)
    }

    /// The forbidden emission, on purpose: an `.ended` reported for a hangup the
    /// controller itself asked for. A controller that acts on it puts a microphone
    /// back that the user just stopped, so the guard is worth a test of its own.
    var emitEndedAfterHangUp = false
}

// MARK: - AmbientActivitySink

/// Records every call IN ORDER, because the ActivityKit verb the controller
/// picks is the thing that has to be pinned.
///
/// `nil` → non-`nil` must be `start`, non-`nil` → `nil` must be `end`, and
/// non-`nil` → non-`nil` must be `update`. A draft that called `update` on the
/// way in and never called `end` at all got as far as review once; it would have
/// meant the disarm reason never reached the user. Separate counters cannot see
/// that — an ordered log can.
///
/// It records faithfully rather than defensively: the real sink silently DROPS a
/// caption or a phase update that arrives before `start` or after `end`, so a
/// double that swallowed them would hide exactly that bug. Assert on the order.
@MainActor
final class FakeAmbientActivitySink: AmbientActivitySink {
    enum Call: Equatable {
        case start(phase: AmbientOrbPhase, armedAt: Date, expiresAt: Date)
        /// `announcing` is recorded because WHICH update carries the wake alert
        /// is a claim the controller owns: an alert on any other transition
        /// auto-expands the island for a transition the user did not cause.
        case update(phase: AmbientOrbPhase, announcing: Bool)
        case caption(String, role: AmbientCaptionRole?)
        case speaking(AmbientSpeakingSpan?)
        /// The pulse's mid-phase flips. The show that rides a phase transition
        /// is NOT a call — the real sink derives it from the phase inside
        /// `update` — so what this records is exactly what the controller's
        /// cadence published.
        case phaseWord(visible: Bool)
        case expiry(Date)
        case end(reason: String?)
        case endOrphans
    }

    /// `Call` with the payloads stripped, so the headline assertion is writable.
    ///
    /// `.start` carries an `armedAt`/`expiresAt` the controller generates from
    /// `Date()`, which a test cannot predict — so the sequence assertion the log
    /// exists for cannot be written against `Call` at all. The reachable
    /// substitutes are worse than useless: `count == 3` and
    /// `contains(.end(reason:))` both pass for `[.update, .update, .update]`,
    /// which is precisely the bug being hunted.
    enum Verb: Equatable { case start, update, caption, speaking, phaseWord, expiry, end, endOrphans }

    private(set) var calls: [Call] = []

    /// The one-liner: `XCTAssertEqual(sink.verbs, [.start, .update, .end])`.
    var verbs: [Verb] {
        calls.map {
            switch $0 {
            case .start: return .start
            case .update: return .update
            case .caption: return .caption
            case .speaking: return .speaking
            case .phaseWord: return .phaseWord
            case .expiry: return .expiry
            case .end: return .end
            case .endOrphans: return .endOrphans
            }
        }
    }

    /// `verbs` without the field-level writes — captions, which move on every
    /// partial transcript, speaking spans, which the controller offers on every
    /// reported turn, and phase-word flips, which the pulse cadence publishes
    /// twice per ten seconds — any of which would otherwise swamp a
    /// phase-sequence assertion.
    ///
    /// All are filtered here rather than swallowed at record time on purpose: the
    /// real sink deduplicates them and this double does not, so the *offers* stay
    /// visible to a test that wants to count them.
    var lifecycleVerbs: [Verb] { verbs.filter { $0 != .caption && $0 != .speaking && $0 != .phaseWord } }

    /// `calls` without the field-level writes — the payload-carrying counterpart to
    /// `lifecycleVerbs`, for assertions that need the phases or the disarm
    /// reason. Use `calls` when the interleaving is itself the point.
    var lifecycleCalls: [Call] {
        calls.filter {
            switch $0 {
            case .caption, .speaking, .phaseWord: return false
            default: return true
            }
        }
    }

    /// Every speaking span this sink was actually handed, in order — including the
    /// nils, since "the bar was cleared when the reply ended" is a claim worth
    /// asserting.
    /// Written as a fold rather than `compactMap`, which would have unwrapped the
    /// payload and silently dropped exactly the nils this exists to show.
    var speakingSpans: [AmbientSpeakingSpan?] {
        calls.reduce(into: []) { spans, call in
            if case .speaking(let span) = call { spans.append(span) }
        }
    }

    /// Makes the orb unavailable, as a denied `Activity.request` or
    /// `areActivitiesEnabled == false` does. Arming must fail: no visible
    /// indicator, no armed mic.
    var failToStart = false

    func start(phase: AmbientOrbPhase, armedAt: Date, expiresAt: Date) -> Bool {
        calls.append(.start(phase: phase, armedAt: armedAt, expiresAt: expiresAt))
        return !failToStart
    }

    func update(phase: AmbientOrbPhase, announcing: Bool) {
        calls.append(.update(phase: phase, announcing: announcing))
    }

    func updateCaption(_ caption: String, role: AmbientCaptionRole?) {
        calls.append(.caption(caption, role: role))
    }

    func updateSpeaking(span: AmbientSpeakingSpan?) {
        calls.append(.speaking(span))
    }

    func setPhaseWordVisible(_ visible: Bool) {
        calls.append(.phaseWord(visible: visible))
    }

    func updateExpiry(_ expiresAt: Date) {
        calls.append(.expiry(expiresAt))
    }

    func end(reason: String?) {
        calls.append(.end(reason: reason))
    }

    func endOrphans() {
        calls.append(.endOrphans)
    }
}
