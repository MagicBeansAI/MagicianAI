import AVFoundation
import Combine
import Foundation
import os

/// When queued reply audio will have finished coming out of the speaker.
///
/// Exists because the orb's `speaking` claim needs an *end*, and the obvious
/// cheap answer is wrong. A "no audio frame for N ms" debounce measures when the
/// provider stopped *sending*, and a realtime provider streams a reply
/// considerably faster than realtime — the last frame of a ten-second answer can
/// be queued a second in — so a debounce drops the orb to `listening` while the
/// assistant is still audibly talking. Modelling the player's queue instead gives
/// the honest answer: each frame's own duration, accumulated, from whenever the
/// speaker was next free.
///
/// Pure and separate so the accumulation is asserted without sleeping through a
/// reply, and without an audio graph.
struct AssistantPlaybackClock: Equatable {
    /// Uptime at which the speaker runs dry. Zero before any audio.
    private(set) var idleAtUptime: TimeInterval = 0

    /// Uptime the currently audible stretch of reply began at. Zero before any
    /// audio.
    ///
    /// Tracked alongside the end because the orb draws a *span*, not a countdown:
    /// a bar anchored at the moment the span was published would read 0% when a
    /// second of the reply had already been heard. Re-anchored whenever the
    /// speaker was found free, on exactly the condition `queue` already branches
    /// on, so "one continuous stretch of audio" means the same thing to both.
    private(set) var startedAtUptime: TimeInterval = 0

    /// Queue one transport-rate PCM16 frame; returns the new idle time.
    ///
    /// `max(idleAtUptime, now)` is what makes this a queue rather than a running
    /// total: frames streamed ahead of playback stack up behind each other, while
    /// the first frame of a *new* reply starts from the present instead of
    /// inheriting a stale deadline from the last one.
    mutating func queue(frameBytes: Int, sampleRate: Int, now: TimeInterval) -> TimeInterval {
        guard sampleRate > 0 else { return idleAtUptime }
        let seconds = TimeInterval(frameBytes / 2) / TimeInterval(sampleRate)
        // The speaker was free, so this frame starts a new audible stretch rather
        // than extending one. Same test the line below makes, read for its other
        // meaning.
        if idleAtUptime <= now { startedAtUptime = now }
        idleAtUptime = max(idleAtUptime, now) + seconds
        return idleAtUptime
    }

    /// The queued reply as wall-clock dates, or nil when nothing is queued.
    ///
    /// The conversion happens here, once, because the two clocks are not
    /// interchangeable and mixing them is silent: `systemUptime` is monotonic and
    /// survives a clock change, which is why the queue is accumulated in it, while
    /// the Live Activity can only be handed `Date`s. Taking both the uptime and
    /// the wall-clock reference as parameters is what keeps that arithmetic
    /// assertable without sleeping through a reply.
    ///
    /// Deliberately ends at the audio's OWN end rather than at
    /// `speakerTailSeconds` past it. The tail is grace before the orb hands the
    /// microphone back; the bar depicts the audio, so it completes when the audio
    /// does. Adding the tail would have the bar claim a third of a second of
    /// speech that is silence.
    func wallClockSpan(now: TimeInterval, reference: Date = Date()) -> AmbientSpeakingSpan? {
        guard idleAtUptime > now else { return nil }
        return AmbientSpeakingSpan(
            from: reference.addingTimeInterval(startedAtUptime - now),
            until: reference.addingTimeInterval(idleAtUptime - now)
        )
    }

    mutating func reset() {
        idleAtUptime = 0
        startedAtUptime = 0
    }
}

/// The real `AmbientCallSink`: the ambient wake handoff wired to the existing
/// realtime call stack.
///
/// **Its own file rather than an addition to `AmbientCallSink.swift`**, for the
/// same reason `AmbientMicEngine` is not part of `AmbientMicSource.swift` and
/// `VoskWakeSpotter` is not part of `WakeSpotter.swift`: the protocol is the
/// socket-free seam the controller is *proved* against, and the socket-bearing
/// implementation is precisely the thing that seam exists to keep separable.
/// Putting the two in one file would mean every test of the state machine
/// compiled against a type that needs a backend.
///
/// ## What it adds to `VoiceCallViewModel`, and what it deliberately does not
///
/// It adds four things and holds no call state of its own beyond the last turn it
/// reported and the speaker's queue: a **rate conversion** on the way in, a **turn
/// projection** on the way out, an **end** for that projection (nothing else in the
/// stack reports when a reply stops), and a **log line** at the one layer that knows
/// why a connect failed.
///
/// It does *not* touch `VoiceCallAudioFocus`. `AmbientController` acquires it when
/// the window arms and releases it when the window ends — for the whole armed
/// window, calls and gaps alike — so acquiring here would be a second token, and
/// releasing here would drop the suppression the *window* depends on while the
/// window is still open.
@MainActor
final class RealtimeAmbientCallSink: AmbientCallSink {

    /// What the control socket's binary frames are, and it is **fixed rather than
    /// negotiated**: `RealtimeVoiceProtocol.startPayload` sends no sample rate,
    /// `session.ready` carries none (`addressing` is the whole of what is parsed
    /// back), and both `VoicePCM` and `VoiceAudioEngine.targetFormat` are written
    /// to 24 kHz against a backend that reserves binary frames for exactly that.
    /// There is nothing to ask.
    nonisolated static let transportSampleRate = 24_000

    /// What to do with the follow-up window when the conversation reports a turn.
    ///
    /// Pure, `static` and its own type because the wrong answer here is invisible
    /// in both directions and neither symptom looks like a timer bug. Cancel too
    /// eagerly and the conversation never ends — the wake word is spent for the
    /// rest of the window. Cancel too rarely and the sink hangs up on a user who
    /// is still mid-sentence.
    enum FollowUpDisposition: Equatable {
        /// The microphone is the user's, so start the window or push it out.
        case refresh
        /// The assistant is working or talking. There is nothing to wait for yet.
        case cancel
        /// The input said nothing about whose microphone it is; leave the window
        /// exactly as it was.
        case leave
    }

    /// Grace after the queued reply audio runs dry before the orb says the
    /// microphone is the user's again.
    ///
    /// Same value, and the same reasoning, as `VoiceAudioEngine`'s half-duplex
    /// hangover: it covers the speaker's decay tail and the gap between frames of
    /// one continuous utterance, so a reply that stutters mid-stream is not
    /// reported as two turns with a `listening` flicker between them.
    nonisolated static let speakerTailSeconds: TimeInterval = 0.35

    /// How long frame arrivals must pause before the queued reply is treated as
    /// complete enough to draw a bar across.
    ///
    /// **This is a debounce on ARRIVAL, which is the opposite question from the one
    /// `AssistantPlaybackClock` exists to refuse.** A debounce cannot tell when
    /// playback ends — that is the whole reason the queue is modelled — but "has the
    /// provider stopped sending" is exactly what a debounce answers, and it is the
    /// question that matters here: the queue's end is only the *reply's* end once
    /// nothing more is coming.
    ///
    /// Waiting for it is what makes the bar honest. A realtime provider streams a
    /// reply considerably faster than realtime, so the deadline known at the first
    /// frame is a few tens of milliseconds out and a bar drawn to it would fill and
    /// complete while the assistant talked for ten more seconds — this feature's
    /// characteristic failure, dressed as a progress indicator. A quarter second is
    /// long enough to span the gap between frames of one continuous stream and short
    /// enough that the bar appears within the first breath of the reply; the orb
    /// itself has already been green since the first frame, so nothing about the
    /// phase waits on this.
    nonisolated static let queueSettleSeconds: TimeInterval = 0.25

    /// The wrapped call.
    ///
    /// Owned rather than injected, and constructed `.ambient` here. The seam this
    /// type sits behind is `AmbientCallSink` — that is what the controller is
    /// tested against — so an injection point here would buy nothing and would
    /// admit the one value that must never appear: a `.inApp` view model, which
    /// releases the shared audio session on every teardown and would kill the
    /// armed window at the end of the first conversation.
    let call = VoiceCallViewModel(mode: .ambient)

    /// A `PassthroughSubject`, because `AmbientCallSink.turnPublisher` is pinned
    /// **non-replaying**: `AmbientController` subscribes *before* `startCall` so a
    /// turn emitted during the connect is not lost, and a `@Published` projection
    /// would replay its current value on subscribe and make a late-subscribe bug
    /// indistinguishable from correct behaviour.
    private let turns = PassthroughSubject<AmbientTurn, Never>()

    /// A `PassthroughSubject` for the reason `turns` is: `captionPublisher` is
    /// pinned non-replaying, and a `@Published` projection would replay the
    /// previous conversation's last line to a late subscriber as if the new call
    /// had said it.
    private let captionLines = PassthroughSubject<AmbientCaptionLine, Never>()

    /// Non-replaying for the same reason `turns` is, and it matters more here: the
    /// follow-up window is armed inside `startCall`, so its first `.quiet` is
    /// emitted before `startCall` returns. A replaying subject would hand that to
    /// a late subscriber and make a late-subscribe bug look like correct
    /// behaviour; this one drops it, exactly as the contract says.
    private let lifecycle = PassthroughSubject<AmbientCallLifecycle, Never>()

    /// The last turn reported, so a stream of partial transcripts or a stream of
    /// audio frames does not re-report the same turn hundreds of times.
    private var lastTurn: AmbientTurn?

    /// Ids of the finalised captions already emitted on `captionPublisher`.
    ///
    /// A `Set` rather than the single last-emitted id, because finalisation is
    /// not a property of the array's tail: `RealtimeVoiceCaptionState` replaces a
    /// user partial IN PLACE, keeping its id, and matches that partial by server
    /// item id precisely so a final can land on it across fallback and
    /// reconnects — after captions have been appended behind it. A single id
    /// would either re-emit every earlier final each pass or miss one that
    /// finalised behind the tail; the set makes "exactly once" a property of the
    /// caption's identity rather than of its position.
    ///
    /// Bounded by intersecting with the live array's ids on every snapshot. That
    /// costs nothing in correctness: an id leaves the array only through
    /// `reset()` (which empties both) or the removal of a non-final user caption
    /// (which was never emitted), so a dropped id can never be asked about again.
    /// "The reset event empties both" is a cross-file coupling rather than a law
    /// of the type: `RealtimeVoiceCaptionState.reset()` fires only inside
    /// `call.startCall`, which this sink's own `startCall` — the one place this
    /// set is cleared — is the sole caller of. If that single-caller invariant
    /// ever breaks, the very next snapshot's intersection self-heals the set
    /// against the emptied array.
    private var emittedFinalIDs: Set<RealtimeVoiceCaption.ID> = []

    /// Whether a conversation this sink started is still admitted to report.
    ///
    /// **The whole of the "never report an `endCall()` the caller asked for"
    /// rule.** `endCall` clears it *before* `hangUp()`, so the terminal transport
    /// phase that hangup produces finds nothing to report through. `startCall`
    /// clears it on entry and sets it only once the session is ready, so a connect
    /// that never came up cannot report an end either — the controller learns that
    /// from `startCall`'s `false`, and reporting it twice would send it down two
    /// recovery paths for one event.
    ///
    /// It is also what makes `.ended` terminal: `reportEnded` clears it, so a
    /// `.dropped` chasing a `.wentQuiet` down the same teardown is dropped here
    /// rather than in the controller.
    private var callIsLive = false

    private var playback = AssistantPlaybackClock()
    private var silenceWatch: Task<Void, Never>?

    /// The arrival debounce that decides when the queued reply is worth drawing.
    /// See `queueSettleSeconds`.
    private var spanWatch: Task<Void, Never>?

    /// The reply audio's span once the queue has settled, or nil.
    ///
    /// **A property that the controller PULLS, rather than an event it is pushed.**
    /// The three pushed alternatives are each worse in a specific way:
    ///
    /// - An associated value on `AmbientTurn` breaks its `String` raw-value
    ///   conformance, and — decisively — `report` deduplicates on turn equality, so
    ///   a `Date` inside the turn would make every ~20 ms audio frame a *new* value
    ///   and turn one publish per reply into hundreds. The dedup is the only thing
    ///   standing between the audio stream and the update budget.
    /// - A parallel `AmbientCallLifecycle` case lands in the controller's
    ///   single-slot `latestLifecycle` box, which is applied LAST precisely because
    ///   a lifecycle event is newer information than a turn. A span sharing that
    ///   slot with `.quiet` would have each clobber the other, and that type is
    ///   about whether the microphone is still admitted — not about the reply.
    /// - A third publisher is a third subscription and a third ordering to get
    ///   right, for one value that is only ever read at the instant the orb
    ///   publishes anyway.
    ///
    /// So the span is read where it is used, and the only thing pushed is the
    /// existing turn — see `announceSpeakingSpan`.
    private(set) var speakingSpan: AmbientSpeakingSpan?

    /// The one follow-up-window timer. See `AmbientCallLifecycle.quiet`.
    private var followUpWatch: Task<Void, Never>?

    /// When the pending follow-up window expires, or nil when none is running.
    ///
    /// `internal` for the same reason `silenceWatchFiresAtUptime` is: the deadline
    /// is the difference between a window that tracks the conversation and one
    /// that fires on the first guess, and a test that could not see it would pass
    /// against either.
    private(set) var followUpExpiresAt: Date?

    /// Uptime the pending silence watch will fire at, or nil when none is armed.
    ///
    /// `internal` so the queue accumulation is assertable without sleeping through
    /// a reply: it is the difference between this and a debounce, and a test that
    /// could not see it would pass against either.
    private(set) var silenceWatchFiresAtUptime: TimeInterval?

    private var cancellables: Set<AnyCancellable> = []
    private let log = Logger(subsystem: "ai.magicbeans.magios", category: "ambient.call")

    var turnPublisher: AnyPublisher<AmbientTurn, Never> { turns.eraseToAnyPublisher() }

    var captionPublisher: AnyPublisher<AmbientCaptionLine, Never> { captionLines.eraseToAnyPublisher() }

    var lifecyclePublisher: AnyPublisher<AmbientCallLifecycle, Never> { lifecycle.eraseToAnyPublisher() }

    init() {
        observe()
    }

    // MARK: - AmbientCallSink

    /// Forwarded whole: the engine that must spend its first run is the one
    /// that takes the wake handoff — this sink's own view model's — and the
    /// once-per-process decision stays in the controller.
    func primeAudioGraph() -> Bool {
        call.primeAudioGraph()
    }

    func startCall() async -> Bool {
        await startCall(
            engineOverride: AudioSettings.shared.ambientVoiceMode.streamingEngine ?? .handsFree
        )
    }

    /// Router entry that carries the settings snapshot taken for this exact
    /// conversation. A setting changed during an outstanding connect applies to
    /// the next call, not halfway through this one.
    func startCall(engineOverride: VoiceEngine) async -> Bool {
        // A new conversation reports its first turn even when it repeats the last
        // one the previous conversation ended on.
        lastTurn = nil
        // Emission tracking is per-call state: the client resets its caption
        // array inside `call.startCall` below, and ids never repeat across that
        // reset, so what this actually clears is the previous conversation's
        // finals — teardown keeps them in the array, and carrying their ids
        // forward would only pin memory for captions that can never re-finalise.
        emittedFinalIDs.removeAll()
        cancelSilenceWatch()
        // Cleared BEFORE anything else, so the terminal phase left behind by the
        // previous conversation — this is the second, third, nth call of an armed
        // window — cannot be reported as this one ending. See `callIsLive`.
        callIsLive = false
        cancelFollowUpWindow()
        // `uiThreadId` is empty on purpose: an ambient conversation begins with no
        // chat thread behind it, and `RealtimeVoiceProtocol.startPayload` omits the
        // field when it is blank, which gives the backend a fresh one. The in-app
        // path already passes `""` whenever no session is selected, so this is the
        // existing meaning of "no thread" rather than a new convention.
        call.startCall(
            uiThreadId: "",
            engineOverride: engineOverride
        )
        // Awaiting ready is what turns the transport's connect into a `Bool` for
        // the controller. Everything the microphone captures meanwhile is
        // dropped at the view model's pre-ready gate — not held, not flushed
        // (owner decision, 2026-07-30): the assistant hears from ready onward.
        guard await call.client.awaitReadySessionID() != nil else {
            // The user-facing caption is generic BY DECISION (`AmbientCallSink`):
            // ambient mode is for someone not looking at their phone, and a socket
            // error is worth nothing to them. But this layer holds the phase, the
            // routed `RealtimeVoiceProtocol.Event.error(message:recoverable:)` text
            // and the transport's own failures, so the specific reason is recorded
            // here — the only place it is understood.
            let phase = self.call.client.phase
            let reason = self.call.client.errorMessage ?? "none reported"
            if phase == .ended {
                // `.ended` here is the ORDINARY case, not a failure: `endCall` — a
                // disarm, a cap, the orb's button — tears the connect down and
                // lands exactly here. Logging that at `.error` would make the most
                // common line in the ambient log an alarm about the user getting
                // what they asked for.
                log.info("Ambient call ended before it was ready (disarmed during connect).")
            } else {
                log.error(
                    """
                    Ambient call did not reach ready — phase \
                    \(String(describing: phase), privacy: .public), \
                    reason: \(reason, privacy: .public)
                    """
                )
            }
            return false
        }
        // Live from here, and the follow-up window starts running immediately
        // rather than after the first reply.
        //
        // **That is the false-wake rail, and it is not an edge case.** A wake hit
        // on unrelated speech connects a socket the user never asked for and then
        // nothing arrives: no transcript to finalise, no reply to drain, so a
        // window armed only at the end of a reply would never arm at all and the
        // conversation would stay open until the hard cap. Measured false accepts
        // are common enough to make this the second-most-likely shape of an
        // ambient conversation (design §13.1), not a corner.
        callIsLive = true
        refreshFollowUpWindow()
        return true
    }

    /// **Safe with no call to end, and safe to call twice.** Both are ordinary
    /// paths rather than races: `AmbientController.disarm` issues this on every
    /// disarm, including from `.armed` where no call was ever started, and the
    /// handoff issues it a second time when a disarm ran inside `startCall`'s await
    /// and the first one found nothing to end.
    ///
    /// `VoiceCallViewModel.hangUp()` carries that contract — no socket means
    /// nothing sent, `engine.stop(session:)` guards on `isRunning`, the audio-focus
    /// release is nil-guarded — and it also *aborts a connect in flight*, because
    /// `RealtimeVoiceClient.teardown` bumps the start generation and disconnects a
    /// media session that registered after the call was abandoned.
    /// It reports NOTHING on `lifecyclePublisher`, and the ordering below is what
    /// makes that true: `callIsLive` is cleared *before* `hangUp()`, so the
    /// terminal transport phase the hangup produces arrives with nothing admitted
    /// to report it. See `AmbientCallSink.endCall` for why a reported hangup would
    /// leave a microphone running that the user had just stopped.
    func endCall() {
        callIsLive = false
        call.hangUp()
        lastTurn = nil
        cancelSilenceWatch()
        cancelFollowUpWindow()
    }

    // MARK: - Turn projection

    /// The turn a captions snapshot implies, or `nil` when it implies none.
    ///
    /// **Pure, `static` and outside the subscription** so the projection can be
    /// asserted without a socket, a backend or an audio graph — none of which the
    /// simulator offers. Buried in a `sink` closure it would only ever have been
    /// verified by talking to a phone.
    nonisolated static func turn(forLatestCaption caption: RealtimeVoiceCaption?) -> AmbientTurn? {
        guard let caption else { return nil }
        switch caption.role {
        case .user:
            // A FINAL user transcript is by construction an ADDRESSED one, so
            // there is no addressing check here and adding one would duplicate a
            // decision the server already made: the backend owns transcript
            // admission and reports an unaddressed utterance as
            // `transcript.user.ignored`, which `RealtimeVoiceCaptionState` *removes*
            // rather than finalises. A partial means the user is mid-utterance.
            return caption.isFinal ? .thinking : .listening
        case .assistant:
            // Deliberately NOT `.speaking`. The orb's `speaking` claim is that a
            // voice is coming out of the speaker, and an assistant transcript can
            // arrive before, with, or after the audio it describes — so it implies
            // nothing about the speaker, and returning `.speaking` here would light
            // the orb for a reply that has not started. Audio decides it (see
            // `observe`). Returning `nil` also means a transcript cannot *demote* a
            // `.speaking` the audio already established.
            return nil
        }
    }

    /// What a reported turn means for the follow-up window.
    ///
    /// **`.listening` refreshes rather than cancels, and that asymmetry is the
    /// decision.** The tempting reading is that any turn at all means the
    /// conversation is alive, so any turn should cancel the window — which leaves
    /// an exchange with no way to end: the user speaks, the server judges the
    /// utterance unaddressed and *removes* the caption
    /// (`RealtimeVoiceCaptionState` drops an ignored transcript rather than
    /// finalising it), and nothing further ever arrives. Refreshing instead means
    /// every sign of life from the user buys them another window and silence
    /// still closes the conversation.
    ///
    /// `nil` leaves the window alone rather than cancelling it, because `nil` is
    /// what an assistant *transcript* projects to — see `turn(forLatestCaption:)`
    /// — and a transcript is not audio. A reply whose text arrives and whose audio
    /// never does must still be able to time out.
    nonisolated static func followUpDisposition(for turn: AmbientTurn?) -> FollowUpDisposition {
        switch turn {
        case .listening: return .refresh
        case .thinking, .speaking: return .cancel
        case nil: return .leave
        }
    }

    /// The `AmbientCallEnded` a transport phase reports, or nil for a phase that
    /// is not an ending.
    ///
    /// Pure and `static` so the mapping is assertable without a socket — the whole
    /// point of the phase being an enum. The two terminal phases are genuinely
    /// different events: `.ended` is the server closing the session, `.failed` is
    /// the transport having exhausted its own reconnect backoff. Both leave the
    /// armed window intact (`AmbientController.handleCallEnded` says why), so the
    /// distinction is carried for the log and for the contract rather than for a
    /// branch — which is exactly why it must not be collapsed here, where a later
    /// author would have to guess it back.
    nonisolated static func endedCause(for phase: RealtimeVoiceClient.Phase) -> AmbientCallEnded? {
        switch phase {
        case .ended: return .remote
        case .failed: return .dropped
        case .idle, .connecting, .reconnecting, .ready, .rotating: return nil
        }
    }

    /// The follow-up window as a `TimeInterval`, from the milliseconds
    /// `session.ready` negotiated.
    ///
    /// Clamped, because the value arrives from the network and a zero or negative
    /// one would hang up the instant a conversation went quiet — the user would
    /// get one sentence per wake word. The floor falls back to
    /// `RealtimeVoiceProtocol.Addressing.disabled`'s own default rather than to a
    /// number invented here, so there is one answer to "how long is a follow-up
    /// window" in the whole app.
    nonisolated static func followUpWindow(millis: Int) -> TimeInterval {
        let fallback = RealtimeVoiceProtocol.Addressing.disabled.followUpWindowMs
        return TimeInterval(millis > 0 ? millis : fallback) / 1000
    }

    // MARK: - Wiring

    private func observe() {
        call.client.$captions
            .sink { [weak self] captions in
                // Delivered synchronously on the main actor — every write to
                // `captions` happens inside `RealtimeVoiceClient`'s main-actor
                // event handling — so the isolation is asserted rather than hopped.
                //
                // A `.receive(on: RunLoop.main)` here would be a real bug, not a
                // stylistic difference: assistant audio reaches this actor with no
                // hop at all, so a deferred caption could overtake it and report
                // `.thinking` over a `.speaking` that had already begun.
                MainActor.assumeIsolated {
                    guard let self else { return }
                    let turn = Self.turn(forLatestCaption: captions.last)
                    self.report(turn)
                    // OUTSIDE `report`, deliberately. `report` deduplicates, and a
                    // second partial from a user who is still mid-sentence is
                    // exactly the delivery that gets deduplicated — so a refresh
                    // performed inside it would be dropped precisely when it is
                    // needed, and the sink would hang up on someone who had been
                    // talking continuously for the whole window.
                    self.applyFollowUp(for: turn)
                    // After the turn, matching the controller's own ordering:
                    // the phase moves first, the caption composes onto it.
                    self.emitNewFinals(captions)
                }
            }
            .store(in: &cancellables)

        // Downstream audio is the only honest source for `.speaking`.
        call.onAssistantAudio = { [weak self] frame in self?.handleAssistantAudio(frame) }

        // The two ends nobody in this stack reports as a *conversation* event.
        //
        // `VoiceCallViewModel` already watches this phase to tear its own audio
        // half down, and that is a different question with a different answer: it
        // asks "is my graph still needed", this asks "is the user's exchange
        // over". Reusing its subscriber would tie the controller's recovery to a
        // `receive(on: RunLoop.main)` hop that deliberately discards a superseded
        // delivery.
        call.client.$phase
            .sink { [weak self] phase in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    // `@Published` replays on subscribe — this fires with `.idle`
                    // during `init` — and a phase left over from the previous
                    // conversation of the same armed window is delivered the same
                    // way. `callIsLive` is what tells those apart from an end.
                    guard self.callIsLive, let cause = Self.endedCause(for: phase) else { return }
                    self.log.info("Ambient conversation ended: \(String(describing: cause), privacy: .public).")
                    self.reportEnded(cause)
                }
            }
            .store(in: &cancellables)
    }

    /// Publish a turn if there is one and it is new.
    ///
    /// Deduplicated because both inputs repeat heavily — a partial transcript
    /// arrives per word and an audio frame per ~20 ms — and every emission crosses
    /// into the controller, a lock and (on the far side of it) an ActivityKit
    /// update.
    private func report(_ turn: AmbientTurn?) {
        guard let turn, turn != lastTurn else { return }
        // Anything that is not the reply cancels the reply's silence watch: a
        // barge-in has already moved the orb on, and letting a watch armed before
        // it fire later would drop `.listening` over a `.thinking` — the orb lying
        // in the other direction.
        if turn != .speaking { cancelSilenceWatch() }
        lastTurn = turn
        turns.send(turn)
    }

    // MARK: - Transcript emission

    /// The walk, pure: which lines in this snapshot are newly final, and what
    /// the emitted-set becomes. Static so the exactly-once decision is
    /// assertable without a socket — the subscription's only job is to send
    /// what this returns.
    ///
    /// A caption is emitted exactly once, at the snapshot where its id is
    /// first seen final; a final already emitted is not a new one. FINALS ONLY
    /// is the protocol's rate-budget decision, and the array's shape is what
    /// makes "when it finalises" decidable from snapshots at all: a partial
    /// mutates per token under a stable id, so its final is the first snapshot
    /// where that id carries `isFinal`, whether the reducer flipped it in
    /// place (a user partial) or appended it whole (an assistant line).
    nonisolated static func newFinals(
        in captions: [RealtimeVoiceCaption],
        alreadyEmitted: Set<RealtimeVoiceCaption.ID>
    ) -> (lines: [AmbientCaptionLine], emitted: Set<RealtimeVoiceCaption.ID>) {
        // The bound: ids that have left the array can never finalise again, so
        // forgetting them is free and keeps the set no larger than the array.
        var emitted = alreadyEmitted.intersection(captions.map(\.id))
        var lines: [AmbientCaptionLine] = []
        for caption in captions where caption.isFinal {
            // The reducer never stores empty text, but the skip is contract
            // rather than trust: a blank line would blank the orb's caption
            // while claiming a speaker said it.
            guard !caption.text.isEmpty else { continue }
            guard emitted.insert(caption.id).inserted else { continue }
            lines.append(AmbientCaptionLine(
                // The orb knows two speakers. `.user` is the user; anything
                // else the conversation says is the agent's side of it.
                role: caption.role == .user ? .user : .agent,
                text: caption.text
            ))
        }
        return (lines, emitted)
    }

    /// Send what the walk decided, and keep the set it decided with.
    ///
    /// No `callIsLive` check, for the reason `report` and `announceSpeakingSpan`
    /// give: admission is the controller's question — its subscription is torn
    /// down with the conversation — and teardown's own re-publish of the array
    /// (`clearUnfinishedUserCaptions`) removes only partials, which were never
    /// emitted, so a hangup cannot produce a line here.
    private func emitNewFinals(_ captions: [RealtimeVoiceCaption]) {
        let walk = Self.newFinals(in: captions, alreadyEmitted: emittedFinalIDs)
        emittedFinalIDs = walk.emitted
        for line in walk.lines { captionLines.send(line) }
    }

    // MARK: - The end of a reply
    //
    // Without this, nothing fires when the assistant stops talking: captions and
    // audio frames are both *arrivals*, so the orb would keep asserting `speaking`
    // over silence until the user's next word or the hard cap. An orb asserting
    // something over nothing is the failure this whole feature is built to refuse,
    // so the absence of a signal has to be turned into one.

    private func handleAssistantAudio(_ frame: Data) {
        let now = ProcessInfo.processInfo.systemUptime
        let idleAt = playback.queue(
            frameBytes: frame.count,
            sampleRate: Self.transportSampleRate,
            now: now
        )
        report(.speaking)
        applyFollowUp(for: .speaking)
        armSilenceWatch(firingAtUptime: idleAt + Self.speakerTailSeconds, now: now)
        armSpanWatch()
    }

    /// Re-armed on every frame, so it fires only once arrivals have actually
    /// paused — which is the moment the queue's end stops moving and becomes the
    /// reply's end. See `queueSettleSeconds`.
    private func armSpanWatch() {
        spanWatch?.cancel()
        spanWatch = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.queueSettleSeconds * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.announceSpeakingSpan()
        }
    }

    /// Record the settled span and tell the controller to re-read it.
    ///
    /// **The `turns.send` here deliberately bypasses `report`'s deduplication, and
    /// the distinction is exact: the TURN has not changed — it is still `speaking`
    /// — so `report` would correctly drop it. What has changed is the *span*, which
    /// the controller learns by pulling `speakingSpan` whenever a turn is applied.
    /// Sending the unchanged turn is the cheapest honest way to say "pull again".**
    ///
    /// It is safe to re-apply because everything downstream is idempotent:
    /// `AmbientController.applyLatestTurn` assigns the same `AmbientState`, whose
    /// publish decision reads the phase pair and finds no change, so no phase
    /// update is spent — and the span publish itself is deduplicated in
    /// `AmbientActivity`. The only update that reaches the system is the one
    /// carrying a span that is genuinely new.
    ///
    /// Guarded on the turn still being `speaking` rather than trusting
    /// cancellation, the same hazard `reportEndOfReply` guards: a watch can already
    /// be executing when the turn moves on, and `Task.cancel` cannot recall it. A
    /// span applied then would draw a reply's bar under `thinking` or `listening`.
    ///
    /// It can announce a SECOND, longer span for one reply, and that is deliberate
    /// rather than a leak: a stream that pauses for longer than `queueSettleSeconds`
    /// and then resumes is a reply that turned out longer than the audio in hand.
    /// The span's start is unchanged (the reply's real start), so the bar's fraction
    /// moves backwards — the one place this motion is not monotone. Revising is
    /// still the honest option; the alternative is a bar that completes while the
    /// assistant is audibly still talking.
    private func announceSpeakingSpan() {
        // No `callIsLive` check, matching `report` and `reportEndOfReply` rather
        // than the lifecycle emissions: every path that clears `callIsLive` also
        // runs `cancelSilenceWatch`, which cancels this watch, so a hung-up call
        // cannot reach here. Adding the flag would only make the audio path
        // untestable without a socket.
        guard lastTurn == .speaking else { return }
        let span = playback.wallClockSpan(now: ProcessInfo.processInfo.systemUptime)
        guard let span, span != speakingSpan else { return }
        speakingSpan = span
        turns.send(.speaking)
    }

    /// Re-armed on every frame, so the deadline tracks the growing playback queue
    /// rather than the first frame's guess.
    private func armSilenceWatch(firingAtUptime uptime: TimeInterval, now: TimeInterval) {
        silenceWatch?.cancel()
        silenceWatchFiresAtUptime = uptime
        let delay = max(0, uptime - now)
        silenceWatch = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.reportEndOfReply()
        }
    }

    private func reportEndOfReply() {
        silenceWatchFiresAtUptime = nil
        playback.reset()
        // Only the reply's own silence turns the orb back. Re-checked rather than
        // trusted to cancellation, because a watch can already be executing when
        // the turn moves on and `Task.cancel` cannot recall it.
        guard lastTurn == .speaking else { return }
        report(.listening)
        // After the turn, so the controller sees `conversing(.listening)` and then
        // `cooldown(until:)` rather than the reverse. Both reduce to the same orb
        // phase, so the ordering costs nothing visually and keeps the state the
        // in-app bar reads honest about which deadline is running.
        applyFollowUp(for: .listening)
    }

    /// Also drops the span, because every caller of this is a reason the reply is
    /// no longer playing — a barge-in, a hangup, a new call, the reply finishing.
    /// Motion that outlives what it depicts is the failure mode this feature keeps
    /// coming back to, so the span dies with the audio it measured rather than
    /// waiting for someone to remember to clear it.
    private func cancelSilenceWatch() {
        silenceWatch?.cancel()
        silenceWatch = nil
        silenceWatchFiresAtUptime = nil
        spanWatch?.cancel()
        spanWatch = nil
        speakingSpan = nil
        playback.reset()
    }

    // MARK: - The end of a conversation
    //
    // Distinct from the end of a reply above, and further from it than the two
    // names suggest: a reply ending hands the microphone back to the user, and a
    // conversation ending hands it back to the wake spotter. The follow-up window
    // is the whole of the distance between them.

    private func applyFollowUp(for turn: AmbientTurn?) {
        switch Self.followUpDisposition(for: turn) {
        case .refresh: refreshFollowUpWindow()
        case .cancel: cancelFollowUpWindow()
        case .leave: break
        }
    }

    /// Start the follow-up window, or push an already-running one out.
    ///
    /// Re-armed rather than extended, so the deadline is always "one whole window
    /// from the last sign of life" — the same shape `armSilenceWatch` uses, for the
    /// same reason: a growing deadline computed from the start would drift.
    private func refreshFollowUpWindow() {
        guard callIsLive else { return }
        let window = Self.followUpWindow(millis: call.client.addressing.followUpWindowMs)
        let deadline = Date().addingTimeInterval(window)
        followUpWatch?.cancel()
        followUpExpiresAt = deadline
        followUpWatch = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(window * 1_000_000_000))
            guard !Task.isCancelled else { return }
            // The deadline it was armed for, carried rather than re-read, so a
            // watch that was already executing when the window was pushed out
            // cannot end a conversation on a deadline that has since moved.
            // `Task.cancel` does not recall a body that has started.
            self?.followUpWindowExpired(armedFor: deadline)
        }
        // Emitted on every refresh, not only on the first. The controller renders
        // this deadline; a push it never heard about would leave the in-app bar
        // counting down to a moment that has already moved.
        lifecycle.send(.quiet(until: deadline))
    }

    private func cancelFollowUpWindow() {
        followUpWatch?.cancel()
        followUpWatch = nil
        followUpExpiresAt = nil
    }

    private func followUpWindowExpired(armedFor deadline: Date) {
        // Re-checked rather than trusted to cancellation: a watch can already be
        // executing when a turn arrives, and `Task.cancel` cannot recall it. Same
        // hazard, and same guard, as `reportEndOfReply`.
        guard callIsLive, followUpExpiresAt == deadline else { return }
        log.info("Ambient conversation went quiet; the follow-up window expired.")
        reportEnded(.wentQuiet)
    }

    /// Report the end once, and never again for this call.
    ///
    /// It does NOT hang up. The controller does, because the controller is what
    /// owns the microphone — see `AmbientCallSink.lifecyclePublisher`.
    private func reportEnded(_ cause: AmbientCallEnded) {
        guard callIsLive else { return }
        callIsLive = false
        cancelFollowUpWindow()
        cancelSilenceWatch()
        lifecycle.send(.ended(cause))
    }
}

// MARK: - Turn-based Ambient Dictation

@MainActor
protocol AmbientDictationCapturing: AnyObject {
    func startCapture(
        onSpeechBegan: @escaping () -> Void,
        onResult: @escaping (AmbientDictationCaptureResult) -> Void,
        onStarted: @escaping (Bool) -> Void
    )
    func cancelCapture()
}

@MainActor
final class NativeAmbientDictationCapture: AmbientDictationCapturing {
    private let controller: DictationController

    init(controller: DictationController? = nil) {
        self.controller = controller ?? DictationController()
    }

    func startCapture(
        onSpeechBegan: @escaping () -> Void,
        onResult: @escaping (AmbientDictationCaptureResult) -> Void,
        onStarted: @escaping (Bool) -> Void
    ) {
        controller.startAmbientCapture(
            onSpeechBegan: onSpeechBegan,
            onResult: onResult,
            onStarted: onStarted
        )
    }

    func cancelCapture() {
        controller.cancelAmbientCapture()
    }
}

@MainActor
protocol AmbientDictationResponding: AnyObject {
    func resetConversation()
    func respond(to transcript: String) async throws -> String
}

private struct AmbientDictationResponseError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// Minimal canonical Chat client for an eyes-free Dictation turn. One session is
/// retained across follow-ups, while `voice_origin` preserves the backend's
/// speech-oriented response contract. The client returns text only; the selected
/// Dictation TTS profile remains an iPhone-local execution choice.
@MainActor
final class NetworkAmbientDictationResponder: AmbientDictationResponding {
    private let urlSession: URLSession
    private let principal: String
    private let workspace: String
    private var sessionID: String?
    private var generation = 0

    init(
        urlSession: URLSession = .shared,
        principal: String = MagicianAccess.principal,
        workspace: String = MagicianAccess.workspace
    ) {
        self.urlSession = urlSession
        self.principal = principal
        self.workspace = workspace
    }

    func resetConversation() {
        generation += 1
        sessionID = nil
    }

    func respond(to transcript: String) async throws -> String {
        let requestGeneration = generation
        let id: String
        if let sessionID {
            id = sessionID
        } else {
            id = try await createSession()
            guard requestGeneration == generation else { throw CancellationError() }
            sessionID = id
        }

        let allowed = CharacterSet.alphanumerics.union(
            CharacterSet(charactersIn: "-._~")
        )
        guard let encoded = id.addingPercentEncoding(withAllowedCharacters: allowed),
              let url = URL(
                string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/sessions/\(encoded)/messages/stream"
              ) else {
            throw AmbientDictationResponseError(message: "Could not create the ambient chat URL.")
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        MagicianAccess.authorize(&request)
        request.httpBody = try JSONSerialization.data(
            withJSONObject: Self.messageBody(
                transcript: transcript,
                chatTurnID: "ios-ambient-dictation-\(UUID().uuidString)"
            )
        )

        let (bytes, response) = try await urlSession.bytes(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw AmbientDictationResponseError(
                message: "Ambient chat failed (HTTP \(status))."
            )
        }
        var eventName = ""
        var accumulated = ""
        var canonicalAnswer: String?
        for try await line in bytes.lines {
            try Task.checkCancellation()
            guard requestGeneration == generation else { throw CancellationError() }
            if line.hasPrefix("event:") {
                eventName = line.dropFirst(6)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                continue
            }
            guard line.hasPrefix("data:") else {
                if line.isEmpty { eventName = "" }
                continue
            }
            switch eventName {
            case "done":
                canonicalAnswer = Self.doneResponseText(fromSSELine: line)
            case "error":
                throw AmbientDictationResponseError(
                    message: Self.responseError(fromSSELine: line)
                        ?? "The ambient chat stream failed."
                )
            case "token", "":
                if let token = Self.responseToken(fromSSELine: line) {
                    accumulated += token
                }
            default:
                break
            }
        }
        let answer = (canonicalAnswer ?? accumulated)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !answer.isEmpty else {
            throw AmbientDictationResponseError(
                message: "The ambient chat turn returned no spoken response."
            )
        }
        return answer
    }

    private func createSession() async throws -> String {
        guard let url = URL(
            string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/chat/new"
        ) else {
            throw AmbientDictationResponseError(message: "Could not create the chat-session URL.")
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = Data("{}".utf8)
        let (data, response) = try await urlSession.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode),
              let id = Self.sessionID(from: data) else {
            throw AmbientDictationResponseError(message: "Could not open an ambient chat session.")
        }
        return id
    }

    nonisolated static func messageBody(
        transcript: String,
        chatTurnID: String
    ) -> [String: Any] {
        [
            "text": transcript,
            "chat_turn_id": chatTurnID,
            "source_surface": "ios",
            "voice_origin": true,
            "continue_on_disconnect": true,
        ]
    }

    nonisolated static func responseToken(fromSSELine line: String) -> String? {
        guard let object = responseJSONObject(fromSSELine: line),
              let token = object["text"] as? String,
              !token.isEmpty else { return nil }
        return token
    }

    /// The terminal SSE frame carries the server's complete persisted reply.
    /// Prefer it over the token accumulator so a bounded channel drop cannot
    /// make Ambient Dictation speak only part of an otherwise successful turn.
    nonisolated static func doneResponseText(fromSSELine line: String) -> String? {
        guard let object = responseJSONObject(fromSSELine: line),
              let message = object["assistant_message"] as? [String: Any],
              let content = message["content"] as? [String: Any],
              content["type"] as? String == "text",
              let text = content["text"] as? String,
              !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return text
    }

    nonisolated static func responseError(fromSSELine line: String) -> String? {
        guard let object = responseJSONObject(fromSSELine: line),
              let message = object["error"] as? String,
              !message.isEmpty else { return nil }
        return message
    }

    private nonisolated static func responseJSONObject(
        fromSSELine line: String
    ) -> [String: Any]? {
        guard line.hasPrefix("data:") else { return nil }
        let json = line.dropFirst(5).trimmingCharacters(in: .whitespaces)
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    nonisolated static func sessionID(from data: Data) -> String? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let session = object["session"] as? [String: Any],
              let id = session["id"] as? String,
              !id.isEmpty else { return nil }
        return id
    }
}

@MainActor
protocol AmbientDictationSpeaking: AnyObject {
    @discardableResult
    func speak(
        _ text: String,
        onStart: @escaping () -> Void,
        completion: @escaping (SpeechPlaybackResult) -> Void
    ) -> Bool
    func stop()
}

@MainActor
final class NativeAmbientDictationSpeaker: AmbientDictationSpeaking {
    private let synthesizer: SpeechSynthesizer

    init(synthesizer: SpeechSynthesizer? = nil) {
        self.synthesizer = synthesizer ?? .shared
    }

    @discardableResult
    func speak(
        _ text: String,
        onStart: @escaping () -> Void,
        completion: @escaping (SpeechPlaybackResult) -> Void
    ) -> Bool {
        synthesizer.speak(
            text,
            focusPolicy: .ambientDictationOwner,
            onStart: onStart,
            completion: completion
        )
    }

    func stop() {
        synthesizer.stop()
    }
}

@MainActor
protocol AmbientDictationKeepingAlive: AnyObject {
    /// Keep the process admitted under the already-active ambient audio session.
    /// Implementations must not recategorize, activate, or deactivate that session.
    func start() -> Bool
    func stop()
}

/// A silent output graph that bridges the short admission interval before the
/// continuous Ambient Dictation input graph starts. Once capture is admitted,
/// the input graph itself stays alive through STT, agent work, TTS, and all
/// follow-ups, so this graph is stopped and never alternates with it.
///
/// The ambient window configured and activated `.voiceChat` in the foreground.
/// This type deliberately never touches `AVAudioSession`, so it cannot destroy
/// the background-reactivation invariant or steal the microphone from capture.
@MainActor
final class NativeAmbientDictationKeepalive: AmbientDictationKeepingAlive {
    private var engine: AVAudioEngine?
    private var player: AVAudioPlayerNode?

    func start() -> Bool {
        stop()
        guard !isRunningUnderTests else { return false }

        let engine = AVAudioEngine()
        let player = AVAudioPlayerNode()
        engine.attach(player)
        let format = engine.outputNode.inputFormat(forBus: 0)
        guard format.sampleRate > 0,
              format.channelCount > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4_096) else {
            engine.detach(player)
            return false
        }
        buffer.frameLength = buffer.frameCapacity
        if let channels = buffer.floatChannelData {
            for index in 0..<Int(format.channelCount) {
                memset(
                    channels[index],
                    0,
                    Int(buffer.frameLength) * MemoryLayout<Float>.size
                )
            }
        } else {
            engine.detach(player)
            return false
        }

        engine.connect(player, to: engine.outputNode, format: format)
        player.scheduleBuffer(buffer, at: nil, options: .loops, completionHandler: nil)
        engine.prepare()
        do {
            try engine.start()
            player.play()
            self.engine = engine
            self.player = player
            return true
        } catch {
            player.stop()
            engine.stop()
            engine.detach(player)
            Logger(
                subsystem: "ai.magicbeans.magios",
                category: "ambient.dictation"
            ).error("Ambient Dictation keepalive failed: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    func stop() {
        player?.stop()
        engine?.stop()
        if let engine, let player {
            engine.detach(player)
        }
        player = nil
        engine = nil
    }
}

@MainActor
protocol AmbientDictationDeadlineScheduling: AnyObject {
    func schedule(after seconds: TimeInterval, action: @escaping () -> Void)
    func cancel()
}

@MainActor
final class NativeAmbientDictationDeadlineScheduler: AmbientDictationDeadlineScheduling {
    private var task: Task<Void, Never>?

    func schedule(after seconds: TimeInterval, action: @escaping () -> Void) {
        cancel()
        task = Task {
            let nanos = UInt64(max(0, seconds) * 1_000_000_000)
            try? await Task.sleep(nanoseconds: nanos)
            guard !Task.isCancelled else { return }
            action()
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}

/// A real AmbientCallSink for the slower Dictation pipeline. Every turn is
/// iterative and bounded: capture finishes before network work begins, network
/// work finishes before TTS begins, and TTS completion schedules (rather than
/// recursively calls) the next capture. That sequencing gives one microphone
/// owner and a constant call stack for an arbitrarily long armed window.
enum AmbientDictationGuidedFlowDecision: Equatable {
    case ordinaryChat
    case presentTutorBlackboard(String)
    case reject(String)
}

@MainActor
final class DictationAmbientCallSink: AmbientCallSink {
    nonisolated static let followUpSeconds: TimeInterval =
        AmbientDictationSilenceGate.noSpeechSeconds
    /// AVSpeechSynthesizer/AVAudioPlayer completion means the playback object is
    /// finished; it does not guarantee that the device's output-to-input route
    /// transition has settled in the same run-loop turn. Opening AVAudioRecorder
    /// synchronously from that callback intermittently returns false on device,
    /// which used to end an otherwise healthy ambient window immediately.
    nonisolated static let followUpPlaybackSettleSeconds: TimeInterval = 0.6
    nonisolated static let followUpCaptureAttempts = 3
    nonisolated static let followUpCaptureRetrySeconds: TimeInterval = 0.4
    /// A complete voice turn should be short, but this deliberately leaves room
    /// for the maximum 45-second utterance, local STT, an agent/tool turn, and
    /// spoken playback. It is a safety ceiling, not a latency target.
    nonisolated static let maximumTurnSeconds: TimeInterval = 180

    private let capture: AmbientDictationCapturing
    private let responder: AmbientDictationResponding
    private let speaker: AmbientDictationSpeaking
    private let keepalive: AmbientDictationKeepingAlive
    private let deadlineScheduler: AmbientDictationDeadlineScheduling
    private let followUpPlaybackSettleSeconds: TimeInterval
    private let followUpCaptureRetrySeconds: TimeInterval
    private let guidedFlowScreenIsLocked: () -> Bool
    private let tutorBlackboardPresenter: (String) -> Void
    private let turns = PassthroughSubject<AmbientTurn, Never>()
    private let captions = PassthroughSubject<AmbientCaptionLine, Never>()
    private let lifecycle = PassthroughSubject<AmbientCallLifecycle, Never>()
    private var responseTask: Task<Void, Never>?
    private var followUpTask: Task<Void, Never>?
    private var generation = 0
    private var callIsLive = false
    private var lastTurn: AmbientTurn?

    init(
        capture: AmbientDictationCapturing? = nil,
        responder: AmbientDictationResponding? = nil,
        speaker: AmbientDictationSpeaking? = nil,
        keepalive: AmbientDictationKeepingAlive? = nil,
        deadlineScheduler: AmbientDictationDeadlineScheduling? = nil,
        followUpPlaybackSettleSeconds: TimeInterval =
            DictationAmbientCallSink.followUpPlaybackSettleSeconds,
        followUpCaptureRetrySeconds: TimeInterval =
            DictationAmbientCallSink.followUpCaptureRetrySeconds,
        guidedFlowScreenIsLocked: (() -> Bool)? = nil,
        tutorBlackboardPresenter: ((String) -> Void)? = nil
    ) {
        self.capture = capture ?? NativeAmbientDictationCapture()
        self.responder = responder ?? NetworkAmbientDictationResponder()
        self.speaker = speaker ?? NativeAmbientDictationSpeaker()
        self.keepalive = keepalive ?? NativeAmbientDictationKeepalive()
        self.deadlineScheduler =
            deadlineScheduler ?? NativeAmbientDictationDeadlineScheduler()
        self.followUpPlaybackSettleSeconds = max(0, followUpPlaybackSettleSeconds)
        self.followUpCaptureRetrySeconds = max(0, followUpCaptureRetrySeconds)
        self.guidedFlowScreenIsLocked =
            guidedFlowScreenIsLocked ?? { DeviceScreenLock.isLocked }
        self.tutorBlackboardPresenter = tutorBlackboardPresenter ?? { question in
            TutorOverlayRouter.shared.present(
                question: question,
                image: nil,
                autoStart: true
            )
        }
    }

    var turnPublisher: AnyPublisher<AmbientTurn, Never> {
        turns.eraseToAnyPublisher()
    }

    var captionPublisher: AnyPublisher<AmbientCaptionLine, Never> {
        captions.eraseToAnyPublisher()
    }

    var lifecyclePublisher: AnyPublisher<AmbientCallLifecycle, Never> {
        lifecycle.eraseToAnyPublisher()
    }

    /// Turn-based TTS does not expose a truthful queue deadline. Nil is the
    /// AmbientCallSink contract's explicit answer for an unknowable span.
    var speakingSpan: AmbientSpeakingSpan? { nil }

    /// The Dictation path has no voice-processing graph to warm, so it must not
    /// claim that the controller's once-per-process prime actually ran. The
    /// production facade primes its streaming sink directly for later mode
    /// changes; a directly injected Dictation sink truthfully leaves the latch
    /// available for a future attempt.
    func primeAudioGraph() -> Bool { false }

    func startCall() async -> Bool {
        generation += 1
        let callGeneration = generation
        responseTask?.cancel()
        responseTask = nil
        followUpTask?.cancel()
        followUpTask = nil
        capture.cancelCapture()
        speaker.stop()
        keepalive.stop()
        deadlineScheduler.cancel()
        responder.resetConversation()
        callIsLive = true
        lastTurn = nil

        guard keepalive.start() else {
            callIsLive = false
            return false
        }

        let started = await beginCapture(generation: callGeneration)
        guard generation == callGeneration else { return false }
        if !started {
            callIsLive = false
            keepalive.stop()
        }
        return started
    }

    func endCall() {
        generation += 1
        callIsLive = false
        responseTask?.cancel()
        responseTask = nil
        followUpTask?.cancel()
        followUpTask = nil
        capture.cancelCapture()
        speaker.stop()
        keepalive.stop()
        deadlineScheduler.cancel()
        responder.resetConversation()
        lastTurn = nil
        // Deliberately no lifecycle emission: this is the controller's own end.
    }

    private func beginCapture(generation callGeneration: Int) async -> Bool {
        guard callIsLive, generation == callGeneration else { return false }
        // Transfer initial admission from the silent output bridge to the one
        // input graph that owns the complete conversation. On follow-up calls
        // this stop is idempotent: the existing input graph remains alive and
        // only its per-turn PCM gate reopens.
        keepalive.stop()
        deadlineScheduler.schedule(after: Self.maximumTurnSeconds) { [weak self] in
            self?.handleTurnDeadline(generation: callGeneration)
        }
        let started = await withCheckedContinuation { continuation in
            capture.startCapture(
                onSpeechBegan: { [weak self] in
                    self?.handleSpeechBegan(generation: callGeneration)
                },
                onResult: { [weak self] result in
                    self?.handleCaptureResult(result, generation: callGeneration)
                },
                onStarted: { started in
                    continuation.resume(returning: started)
                }
            )
        }
        guard started else {
            deadlineScheduler.cancel()
            if callIsLive, generation == callGeneration, !keepalive.start() {
                Logger(
                    subsystem: "ai.magicbeans.magios",
                    category: "ambient.dictation"
                ).error("Ambient Dictation could not restore gap keepalive after recorder-start failure.")
            }
            return false
        }
        guard callIsLive, generation == callGeneration else { return false }
        report(.listening, force: true)
        lifecycle.send(
            .quiet(until: Date().addingTimeInterval(Self.followUpSeconds))
        )
        return true
    }

    private func handleSpeechBegan(generation callGeneration: Int) {
        guard callIsLive, generation == callGeneration else { return }
        // Force the unchanged listening turn so it supersedes the earlier quiet
        // deadline in the controller while this bounded utterance is in progress.
        report(.listening, force: true)
    }

    private func handleCaptureResult(
        _ result: AmbientDictationCaptureResult,
        generation callGeneration: Int
    ) {
        guard callIsLive, generation == callGeneration else { return }
        switch result {
        case .noSpeech:
            reportEnded(.wentQuiet)
        case .failedToTranscribe:
            reportEnded(.dropped)
        case .transcript(let raw):
            let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else {
                reportEnded(.dropped)
                return
            }
            // The capture graph stays alive but its PCM gate is closed across
            // agent/TTS. This keeps background execution admitted without
            // allowing the spoken reply into the next user transcript.
            captions.send(AmbientCaptionLine(role: .user, text: text))
            switch Self.guidedFlowDecision(
                for: text,
                screenIsLocked: guidedFlowScreenIsLocked()
            ) {
            case .ordinaryChat:
                break
            case .presentTutorBlackboard(let concept):
                // Audio/session ownership is released before the native overlay
                // starts. TutorOverlayRouter repeats the real lock check and
                // carries its one-shot admission across SwiftUI presentation.
                reportEnded(.wentQuiet)
                tutorBlackboardPresenter(concept)
                return
            case .reject(let message):
                captions.send(AmbientCaptionLine(role: .agent, text: message))
                report(.thinking)
                speakTerminalNotice(message, generation: callGeneration)
                return
            }
            report(.thinking)
            responseTask?.cancel()
            responseTask = Task { [weak self] in
                guard let self else { return }
                do {
                    let response = try await self.responder.respond(to: text)
                    guard !Task.isCancelled else { return }
                    self.handleResponse(response, generation: callGeneration)
                } catch is CancellationError {
                    return
                } catch {
                    self.handleResponseFailure(error, generation: callGeneration)
                }
            }
        }
    }

    private func handleResponse(_ response: String, generation callGeneration: Int) {
        guard callIsLive, generation == callGeneration else { return }
        responseTask = nil
        let displayText = Self.displayText(for: response)
        if !displayText.isEmpty {
            captions.send(AmbientCaptionLine(role: .agent, text: displayText))
        }
        _ = speaker.speak(
            response,
            onStart: { [weak self] in
                guard let self,
                      self.callIsLive,
                      self.generation == callGeneration else { return }
                self.report(.speaking)
            },
            completion: { [weak self] result in
                guard let self,
                      self.callIsLive,
                      self.generation == callGeneration else { return }
                if result != .completed {
                    Logger(
                        subsystem: "ai.magicbeans.magios",
                        category: "ambient.dictation"
                    ).warning(
                        "Ambient Dictation TTS ended as \(String(describing: result), privacy: .public); preserving the conversation and reopening capture."
                    )
                }
                // Playback delivery and conversation admission are independent
                // contracts. A late AVSpeechSynthesizer cancellation, an audio
                // route failure, or a deliberately skipped empty response must
                // not spend the user's otherwise healthy ambient window. The
                // caption is already available, and the next bounded capture is
                // the recovery rail for every terminal playback outcome.
                self.report(.listening, force: true)
                self.scheduleFollowUpCapture(generation: callGeneration)
            }
        )
    }

    private func handleResponseFailure(_ error: Error, generation callGeneration: Int) {
        guard callIsLive, generation == callGeneration else { return }
        responseTask = nil
        Logger(
            subsystem: "ai.magicbeans.magios",
            category: "ambient.dictation"
        ).error("Ambient Dictation response failed: \(error.localizedDescription, privacy: .public)")
        reportEnded(.dropped)
    }

    /// Reopen Dictation after audible playback without allocating a new audio
    /// graph or treating a transient route transition as the end of the window.
    ///
    /// The retained input graph remains active throughout while its PCM gate is
    /// closed, so the short settle and retry gaps preserve background execution
    /// without collecting microphone input. The sequence is iterative and bounded: at most three attempts,
    /// all owned by one cancellable task, with generation checks after every
    /// suspension. Only exhausting the budget is terminal.
    private func scheduleFollowUpCapture(generation callGeneration: Int) {
        followUpTask?.cancel()
        followUpTask = Task { [weak self] in
            guard let self else { return }
            guard await self.waitForFollowUpDelay(self.followUpPlaybackSettleSeconds),
                  self.callIsLive,
                  self.generation == callGeneration else { return }

            for attempt in 1 ... Self.followUpCaptureAttempts {
                let started = await self.beginCapture(generation: callGeneration)
                guard self.callIsLive,
                      self.generation == callGeneration else { return }
                if started {
                    self.followUpTask = nil
                    return
                }
                if attempt < Self.followUpCaptureAttempts {
                    guard await self.waitForFollowUpDelay(self.followUpCaptureRetrySeconds),
                          self.callIsLive,
                          self.generation == callGeneration else { return }
                }
            }

            self.followUpTask = nil
            Logger(
                subsystem: "ai.magicbeans.magios",
                category: "ambient.dictation"
            ).error("Ambient Dictation could not reopen follow-up capture after \(Self.followUpCaptureAttempts, privacy: .public) attempts.")
            self.reportEnded(.dropped)
        }
    }

    private func waitForFollowUpDelay(_ seconds: TimeInterval) async -> Bool {
        guard seconds > 0 else { return !Task.isCancelled }
        do {
            try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            return !Task.isCancelled
        } catch {
            return false
        }
    }

    private func handleTurnDeadline(generation callGeneration: Int) {
        guard callIsLive, generation == callGeneration else { return }
        Logger(
            subsystem: "ai.magicbeans.magios",
            category: "ambient.dictation"
        ).error("Ambient Dictation turn exceeded its bounded deadline.")
        reportEnded(.dropped)
    }

    private func speakTerminalNotice(_ message: String, generation callGeneration: Int) {
        _ = speaker.speak(
            message,
            onStart: { [weak self] in
                guard let self,
                      self.callIsLive,
                      self.generation == callGeneration else { return }
                self.report(.speaking)
            },
            completion: { [weak self] result in
                guard let self,
                      self.callIsLive,
                      self.generation == callGeneration else { return }
                self.reportEnded(result == .completed ? .wentQuiet : .dropped)
            }
        )
    }

    nonisolated static func guidedFlowDecision(
        for transcript: String,
        screenIsLocked: Bool
    ) -> AmbientDictationGuidedFlowDecision {
        guard let invocation = TutorInvoke.parseVoiceGuidedFlow(transcript) else {
            return .ordinaryChat
        }
        if screenIsLocked {
            return .reject(DeviceScreenLock.message(for: invocation.feature))
        }
        guard invocation.feature == .tutor else {
            return .reject("App Copilot isn't available on this device yet.")
        }
        guard !invocation.requiresScreenCapture else {
            return .reject(
                "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
            )
        }
        return .presentTutorBlackboard(TutorInvoke.strip(invocation.normalizedText))
    }

    /// Orb captions are presentation text, so the backend's voice-only protocol
    /// wrappers must never reach them. TTS still receives the original response
    /// and independently selects the audible `<speech>` portion.
    nonisolated static func displayText(for response: String) -> String {
        SpeechTags.stripped(response)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func report(_ turn: AmbientTurn, force: Bool = false) {
        guard force || turn != lastTurn else { return }
        lastTurn = turn
        turns.send(turn)
    }

    private func reportEnded(_ cause: AmbientCallEnded) {
        guard callIsLive else { return }
        callIsLive = false
        responseTask?.cancel()
        responseTask = nil
        followUpTask?.cancel()
        followUpTask = nil
        capture.cancelCapture()
        speaker.stop()
        keepalive.stop()
        deadlineScheduler.cancel()
        lifecycle.send(.ended(cause))
    }
}

/// Stable AmbientCallSink facade that snapshots the independent iPhone mode at
/// each wake. The controller subscribes once to this facade; mode-gated
/// forwarding combines with each implementation's generation guards so a
/// superseded engine cannot leak a late event into the next call.
@MainActor
final class AmbientConversationCallSink: AmbientCallSink {
    enum Route: Equatable {
        case dictation
        case streaming(VoiceEngine)
    }

    private let realtime: RealtimeAmbientCallSink
    private let dictation: DictationAmbientCallSink
    private let turns = PassthroughSubject<AmbientTurn, Never>()
    private let captions = PassthroughSubject<AmbientCaptionLine, Never>()
    private let lifecycle = PassthroughSubject<AmbientCallLifecycle, Never>()
    private var activeMode: AmbientVoiceMode?
    private var generation = 0
    private var cancellables: Set<AnyCancellable> = []

    init(
        realtime: RealtimeAmbientCallSink? = nil,
        dictation: DictationAmbientCallSink? = nil
    ) {
        self.realtime = realtime ?? RealtimeAmbientCallSink()
        self.dictation = dictation ?? DictationAmbientCallSink()
        observe()
    }

    var turnPublisher: AnyPublisher<AmbientTurn, Never> {
        turns.eraseToAnyPublisher()
    }

    var captionPublisher: AnyPublisher<AmbientCaptionLine, Never> {
        captions.eraseToAnyPublisher()
    }

    var lifecyclePublisher: AnyPublisher<AmbientCallLifecycle, Never> {
        lifecycle.eraseToAnyPublisher()
    }

    var speakingSpan: AmbientSpeakingSpan? {
        switch activeMode {
        case .some(.dictation): return dictation.speakingSpan
        case .some(.handsFree), .some(.realtime): return realtime.speakingSpan
        case .none: return nil
        }
    }

    func primeAudioGraph() -> Bool {
        // Prime the graph even when Dictation is selected today: changing the
        // local preference later must not restore first-streaming-wake latency.
        realtime.primeAudioGraph()
    }

    func startCall() async -> Bool {
        generation += 1
        let callGeneration = generation
        realtime.endCall()
        dictation.endCall()
        let mode = AudioSettings.shared.ambientVoiceMode
        activeMode = mode
        let connected: Bool
        switch Self.route(for: mode) {
        case .dictation:
            connected = await dictation.startCall()
        case .streaming(let engine):
            connected = await realtime.startCall(engineOverride: engine)
        }
        guard generation == callGeneration else { return false }
        if !connected { activeMode = nil }
        return connected
    }

    nonisolated static func route(for mode: AmbientVoiceMode) -> Route {
        guard let engine = mode.streamingEngine else { return .dictation }
        return .streaming(engine)
    }

    func endCall() {
        generation += 1
        activeMode = nil
        realtime.endCall()
        dictation.endCall()
    }

    private func observe() {
        realtime.turnPublisher
            .sink { [weak self] turn in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode?.streamingEngine != nil else { return }
                    self.turns.send(turn)
                }
            }
            .store(in: &cancellables)
        realtime.captionPublisher
            .sink { [weak self] line in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode?.streamingEngine != nil else { return }
                    self.captions.send(line)
                }
            }
            .store(in: &cancellables)
        realtime.lifecyclePublisher
            .sink { [weak self] event in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode?.streamingEngine != nil else { return }
                    self.lifecycle.send(event)
                }
            }
            .store(in: &cancellables)

        dictation.turnPublisher
            .sink { [weak self] turn in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode == .dictation else { return }
                    self.turns.send(turn)
                }
            }
            .store(in: &cancellables)
        dictation.captionPublisher
            .sink { [weak self] line in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode == .dictation else { return }
                    self.captions.send(line)
                }
            }
            .store(in: &cancellables)
        dictation.lifecyclePublisher
            .sink { [weak self] event in
                MainActor.assumeIsolated {
                    guard let self, self.activeMode == .dictation else { return }
                    self.lifecycle.send(event)
                }
            }
            .store(in: &cancellables)
    }
}
