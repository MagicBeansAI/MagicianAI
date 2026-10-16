import Combine
import Foundation

/// One finalised transcript line from the live call, reduced to what the orb
/// can render: who, and what they said.
struct AmbientCaptionLine: Equatable {
    let role: AmbientCaptionRole
    let text: String
}

/// Why a live ambient conversation is over.
///
/// Three causes rather than one because they are three genuinely different
/// events, and a single `ended` would have hidden the only one that is
/// *ordinary*. `wentQuiet` is the exchange finishing the way every exchange
/// finishes; the other two are the conversation being taken away.
///
/// **What is deliberately NOT a case here: the controller's own `endCall()`.**
/// A hangup the controller asked for is not news to the controller, and
/// reporting it would close a loop with a live microphone at the end of it —
/// `disarm` calls `endCall`, the sink reports an end, and the controller
/// answers an end by putting the spotting tap back, *after* the window it
/// belonged to was torn down. See `endCall`.
enum AmbientCallEnded: Equatable {
    /// The follow-up window ran out: the assistant finished, or the user's
    /// speech was never addressed to it, and nothing has happened since.
    ///
    /// The ORDINARY end of an ambient exchange, and the reason this seam exists
    /// at all — without it an armed window gives the user exactly one
    /// conversation and then sits there until the cap expires.
    case wentQuiet

    /// The server ended the session (`RealtimeVoiceProtocol.Event.ended`).
    case remote

    /// The transport gave up. `RealtimeVoiceClient` retries a lost socket on its
    /// own and only reaches `.failed` once its backoff is exhausted or the server
    /// reported a non-recoverable error, so this is already the *end* of a
    /// reconnect attempt rather than the start of one.
    case dropped
}

/// What a live conversation reports about its own ending, as distinct from the
/// turns it reports while it is running.
///
/// **Deliberately not folded into `AmbientTurn`.** `AmbientTurn` answers "who is
/// making a sound right now" — `speaking` means a voice is coming out of the
/// speaker — and this answers "is the microphone still admitted to the
/// conversation without the activation phrase". They overlap (the follow-up
/// window runs while the turn is `listening`) and they are not the same
/// question: collapsing them would make the orb's `speaking` claim double as an
/// admission rule, which is exactly the conflation Task 10 kept apart.
enum AmbientCallLifecycle: Equatable {
    /// Nothing is addressed to the assistant, and the follow-up window runs
    /// until `until`. The user may continue **without** repeating the activation
    /// phrase for that long.
    ///
    /// Re-emitted with a later deadline every time the window is pushed out, so
    /// the controller's `AmbientState.cooldown(until:)` renders the sink's
    /// deadline rather than guessing at it. That matters because **there is
    /// exactly one timer** and the sink owns it: a controller running its own
    /// copy would be a second authority on when to hang up, and the two would
    /// disagree in the direction that cuts a user off mid-sentence.
    case quiet(until: Date)

    /// Over. The sink has stopped its own timers and will emit nothing further
    /// for this call, but it has **not** hung up — see `endCall`.
    case ended(AmbientCallEnded)
}

/// The awake conversation path — realtime streaming or bounded Dictation —
/// behind a seam so the controller is testable without a socket or microphone.
///
/// `startCall` takes nothing on purpose (owner decision, 2026-07-30). It used
/// to carry the drained wake pre-roll so speech that predated the socket
/// arrived with the call; that replay is gone — the assistant hears from
/// session-ready onward and nothing earlier, so there is nothing for the
/// controller to hand over.
///
/// `@MainActor` because the real conforming type will be — `RealtimeVoiceClient`
/// and `VoiceCallViewModel` both already are — and a `@MainActor` witness for a
/// nonisolated requirement is a `#ConformanceIsolation` warning today and an
/// error under Swift 6. It also keeps `endCall()` synchronous on the actor that
/// owns the state machine: wrapping it in `Task { @MainActor in … }` to satisfy
/// a nonisolated requirement would let the call outlive the transition claiming
/// it was torn down.
@MainActor
protocol AmbientCallSink: AnyObject {
    /// Spend the audio stack's once-per-process first run — the VP AudioUnit
    /// build, the I/O rebuild, and the FIRST stop of a VP-armed engine — while
    /// the app is foregrounded and no window exists. The controller decides
    /// WHEN (once per process, at arm, strictly before the spotter's start,
    /// and never while a live voice call holds `VoiceCallAudioFocus`);
    /// implementations run a GENUINE engine start/stop cycle — a flag alone
    /// was the reverted 0.1.157 failure — wire no capture anywhere, and must
    /// be non-fatal on refusal: a prime that cannot run logs and arming
    /// proceeds. Returns whether the cycle ran, because the controller's
    /// once-latch is spent only by a run that did — a throw may have built
    /// nothing, and retrying at the next arm is cheap where a latched failure
    /// is first-wake exposure until relaunch.
    func primeAudioGraph() -> Bool

    /// Returns false if the call could not be established. The controller's
    /// whole response is one automatic retry and then the fall back to armed, so
    /// the reason is not returned here — the implementation logs it at the layer
    /// that knows what it means.
    ///
    /// The user-facing caption after a failed connect is deliberately GENERIC —
    /// `AmbientController.connectFailedNotice` says THAT it failed and what to
    /// do about it, never why. Ambient mode is for someone who is not looking at
    /// their phone, and a socket error string is worth nothing to them; the
    /// specific reason belongs in the log, where it can carry the context the
    /// controller does not have. This is a decision, not an omission — do not
    /// widen the return type to surface a reason the orb was never going to
    /// show.
    /// May be RE-ENTERED while a previous invocation is still outstanding, and
    /// the implementation has to tolerate it. The controller cannot prevent it:
    /// a connect it no longer wants is parked in this await, and nothing here is
    /// cancellation-aware, so a window that ends mid-connect can be replaced by
    /// a new one that arms, wakes and calls this again before the first returns.
    /// The controller discards the stale result — it compares the window it
    /// started against the one that is live — but that only protects the
    /// controller's state, not this type's.
    func startCall() async -> Bool

    /// **Safe with no call to end, and safe to call twice.** Neither is an edge
    /// case: `disarm` issues this on EVERY disarm, including from `.armed` where
    /// no call was ever started, so "nothing to end" is the common path rather
    /// than the rare one. An implementation shaped like `activeCall!.end()`, or
    /// one that asserts a call is live, crashes on an ordinary disarm.
    ///
    /// The double call is the race: `disarm` can run inside `startCall`'s await
    /// and issue this before the socket has come up, in which case the handoff
    /// issues it again once the connect lands, because the first one may have
    /// found nothing to end.
    ///
    /// **It must not emit on `lifecyclePublisher`, and that is a hard rule rather
    /// than a nicety.** Every hangup in this feature is the controller's own —
    /// there is no user-facing hang-up button in ambient mode — so an
    /// implementation that reported its own teardown would report it to the one
    /// object that already knew. The consequence is not a redundant event: the
    /// controller answers `.ended` by resuming the spotting tap, so a hangup
    /// issued *by disarm* would put a microphone back that the user had just
    /// stopped, with no orb, no arm record and no cap timer left to stop it
    /// again. The `.ended` cases name the three ends that are NOT this one.
    func endCall()

    /// Turn changes for the live call.
    ///
    /// Does NOT replay: a subscriber receives only turns emitted after it
    /// subscribes, so the controller must subscribe BEFORE `startCall` can
    /// produce one. Pinned here because the two obvious implementations differ —
    /// a `PassthroughSubject` drops what it emitted before anyone attached, a
    /// `@Published` projection replays the current value on subscribe — and a
    /// controller that subscribes late works against one and silently loses its
    /// first turn against the other. The non-replaying reading is the contract;
    /// an implementation backed by `@Published` must not be erased into this
    /// without accounting for the difference.
    var turnPublisher: AnyPublisher<AmbientTurn, Never> { get }

    /// Finalised transcript lines, one per final caption, already reduced to
    /// role + text. FINALS ONLY, and that is a rate-budget decision rather than a
    /// styling one: partials mutate per token, every emission here becomes a real
    /// ActivityKit publish, and the budget is the orb's scarcest resource. Same
    /// non-replaying contract as `turnPublisher`: subscribe before `startCall`.
    var captionPublisher: AnyPublisher<AmbientCaptionLine, Never> { get }

    /// When the reply currently coming out of the speaker began, and when the audio
    /// queued for it runs dry — or nil whenever that is not a knowable fact, which
    /// is most of the time.
    ///
    /// **Pulled rather than published, and read only when the orb publishes.** It is
    /// a property of the current `speaking` turn rather than an event, and the three
    /// event-shaped alternatives each break something specific — see
    /// `RealtimeAmbientCallSink.speakingSpan`, which records why. The one thing that
    /// IS pushed is a re-emission of the unchanged `.speaking` turn, meaning "read
    /// this again"; a controller that only ever read it on a turn *change* would draw
    /// no bar at all, because the span is never known at the instant `speaking`
    /// begins.
    ///
    /// **Nil is the honest answer, not a missing one.** It is nil while the audio
    /// queue is still growing, because a deadline taken from a partly-streamed reply
    /// would complete while the assistant was still talking. A reader must render
    /// nothing rather than substitute an estimate.
    var speakingSpan: AmbientSpeakingSpan? { get }

    /// How the live conversation is going to end, and then that it has.
    ///
    /// **This is the seam that lets an armed window hold more than one
    /// conversation.** Nothing else in the call stack reports that an exchange is
    /// over: `RealtimeVoiceClient` reports a *transport* phase, and turns and
    /// audio frames are both arrivals, so without this the controller has no
    /// route from `.conversing` back to `.armed` and the window is spent after
    /// one conversation — strictly worse than the tap-to-talk it replaced.
    ///
    /// The contract, all four parts of which are load-bearing:
    ///
    /// 1. **Does not replay**, for the same reason and with the same consequence
    ///    as `turnPublisher`: the controller subscribes BEFORE `startCall`,
    ///    because the follow-up window is armed the moment the session is ready
    ///    and can therefore emit before `startCall` returns.
    /// 2. **`.ended` is terminal and at most once per `startCall`.** Nothing
    ///    follows it — no late `.quiet`, no second `.ended` — so the controller
    ///    can treat the first one as the answer and a repeat as a stale hop.
    /// 3. **Never emitted for an `endCall()` the caller asked for.** See
    ///    `endCall`.
    /// 4. **The sink does not hang up on `.ended`.** It stops its own timers and
    ///    says so; the controller calls `endCall()`. One object decides when the
    ///    socket closes, and it is the one that also owns the microphone.
    var lifecyclePublisher: AnyPublisher<AmbientCallLifecycle, Never> { get }
}
