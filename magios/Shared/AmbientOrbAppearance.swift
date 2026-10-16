import SwiftUI

/// The conversation accent, named once so retuning it is a single edit.
///
/// Explicitly the same `.purple` `MagiosLiveActivity` uses, rather than
/// `.accentColor`. Inheriting the app's accent was never actually on offer here,
/// only the illusion of it: this resolves in the **widget** process, which
/// carries no accent-colour asset of its own, so `.accentColor` out here is the
/// system default — and a `Color` cannot cross into a Live Activity through
/// `ContentState` either, since that payload is budgeted, persisted, and decoded
/// by a binary that may be a version behind. So the orb would have rendered
/// system blue: the one off-brand surface in a bundle whose other Live Activity
/// is purple and whose home-screen widget is a purple-to-pink gradient. Better
/// explicitly on-brand than implicitly wrong.
private let ambientOrbAccent = Color.purple

/// The sentences the orb can end on.
///
/// Named here, beside the `ended(reason:)` that renders them, because each is
/// published from more than one module and nothing else was keeping the copies
/// equal. `Shared/` is the only directory all three targets compile, so it is
/// the only place a single value can reach every publisher.
///
/// **The two are different events, and the wording difference is load-bearing.**
/// `capReached` asserts a cause: the window ran its full course and closed on
/// schedule, which the app knows because its own timer fired. `orphanCollected`
/// asserts nothing, because the process doing the collecting genuinely does not
/// know — the window died with a process that is gone, and the sweep is running
/// after the fact. Wording it as the cap would claim a cause nobody observed,
/// which is a small lie on the one surface whose entire job is not telling the
/// user the wrong thing about whether they are being heard. So do not collapse
/// these into one constant; that they differ is the point.
public enum AmbientEndedReason {
    /// The hard cap expired. Published by the cap timer, and reconstructed by
    /// the widget from `isStale` when no process was alive to publish it — two
    /// sides of one event, which is why this is one value.
    public static let capReached = "Listening window ended."

    /// An orb outlived the window it described and is being swept. Both sweeps
    /// use it — the app's on launch, and the disarm intent's in the widget
    /// process — because it is the same act from two places.
    public static let orphanCollected = "Listening ended."
}

/// What kind of continuous motion a phase is ENTITLED to, which is a claim about
/// what the app knows rather than a styling choice.
///
/// The distinction exists because the two available mechanisms assert different
/// things, and picking the wrong one is this feature's signature failure in a new
/// costume: a bar that fills over a range implies the app knows when the range
/// ends, and out here it usually does not. Only `speaking` has a real deadline
/// (`AssistantPlaybackClock` models the player's queue), so only `speaking` may
/// elapse. Everything else either has no end to depict or has an end nothing can
/// name: `thinking` deliberately has no response deadline, because one would cut
/// off a long tool call, and `heard` — which is the connect wait in practice —
/// ends whenever the backend finishes creating a provider session, measured at
/// 11–13 s and varying by 2.5 s between consecutive runs.
///
/// Kept in the mapping rather than in the view so "no phase claims a duration the
/// app does not know" is a test over `allCases` instead of a comment.
public enum AmbientOrbMotion: Equatable {
    /// Nothing continuous. The callers' identity-swap cross-dissolve between
    /// content states is the whole of the motion (a same-identity mutation
    /// hard-cuts out-of-process), which is what the fill/scale pair is varied
    /// for.
    case still

    /// Something is happening and its end is genuinely unknown, so the treatment
    /// must not imply one.
    case indeterminate

    /// A known span is running out, and may be drawn as progress across it.
    case elapsing
}

/// The aurora colour system for one phase: body gradient stops, the glow under
/// it, and the edge light. The knobs are numeric so the invariants that matter
/// ("armed is dimmer than every conversing phase", "ended has no halo") are
/// assertable in `AmbientOrbAppearanceTests` as numbers rather than eyeballed.
public struct AuroraPalette: Equatable {
    /// Angular-gradient stops for the orb body.
    public let coreStops: [Color]
    /// The glow rendered blurred underneath the body.
    public let halo: Color
    /// 0…1 opacity of the halo layer. 0 means no glow at all.
    public let haloStrength: Double
    /// The rim-light stroke.
    public let rim: Color
    /// `AuroraBlobShape`'s silhouette selector. Out-of-process surfaces have
    /// no render loop, so a phase cannot morph — instead each phase OWNS a
    /// still silhouette, and the identity-swap cross-dissolve between phases
    /// is the morph. Riding the palette keeps that honest: the seed changes
    /// exactly when the body the identity key watches changes.
    public let blobSeed: Double

    public init(coreStops: [Color], halo: Color, haloStrength: Double, rim: Color, blobSeed: Double) {
        self.coreStops = coreStops
        self.halo = halo
        self.haloStrength = haloStrength
        self.rim = rim
        self.blobSeed = blobSeed
    }

    /// The orb body's angular gradient — the one recipe every surface fills with,
    /// named here so the beacon and the orb view cannot drift.
    ///
    /// The leading stop is appended so the gradient's wrap-around seam closes on
    /// its own colour. An empty palette renders a flat halo-coloured disc rather
    /// than trapping the widget process; the preset invariant test keeps this
    /// theoretical.
    public var bodyGradient: AngularGradient {
        AngularGradient(colors: coreStops.first.map { coreStops + [$0] } ?? [halo],
                        center: .center,
                        angle: .degrees(220))
    }
}

public extension AuroraPalette {
    /// Named presets rather than per-callsite literals, so the task Live
    /// Activity and the in-app beacon draw from the same six values the orb
    /// does — the brand is single-sourced here.
    static let armedEmber = AuroraPalette(
        coreStops: [Color(red: 0.36, green: 0.30, blue: 0.52), Color(red: 0.20, green: 0.17, blue: 0.30)],
        halo: Color(red: 0.55, green: 0.42, blue: 0.95),
        haloStrength: 0.16,
        rim: .white.opacity(0.35),
        blobSeed: 0.9)

    static let violetSurge = AuroraPalette(
        coreStops: [Color(red: 0.62, green: 0.35, blue: 1.00), Color(red: 1.00, green: 0.42, blue: 0.78), Color(red: 0.62, green: 0.35, blue: 1.00)],
        halo: Color(red: 0.78, green: 0.45, blue: 1.00),
        haloStrength: 0.90,
        rim: .white.opacity(0.80),
        blobSeed: 2.1)

    static let calmAurora = AuroraPalette(
        coreStops: [Color(red: 0.55, green: 0.38, blue: 0.98), Color(red: 0.36, green: 0.48, blue: 1.00)],
        halo: Color(red: 0.60, green: 0.50, blue: 1.00),
        haloStrength: 0.55,
        rim: .white.opacity(0.60),
        blobSeed: 3.3)

    static let amberThinking = AuroraPalette(
        coreStops: [Color(red: 1.00, green: 0.62, blue: 0.26), Color(red: 0.94, green: 0.44, blue: 0.18)],
        halo: Color(red: 1.00, green: 0.65, blue: 0.30),
        haloStrength: 0.50,
        rim: .white.opacity(0.55),
        blobSeed: 4.6)

    static let tealSpeaking = AuroraPalette(
        coreStops: [Color(red: 0.20, green: 0.85, blue: 0.72), Color(red: 0.28, green: 0.78, blue: 0.40)],
        halo: Color(red: 0.30, green: 0.90, blue: 0.60),
        haloStrength: 0.60,
        rim: .white.opacity(0.60),
        blobSeed: 5.8)

    /// The done colour is the speaking colour on purpose — one teal in the
    /// kit — but a task surface should be able to say what it means.
    static let tealDone = tealSpeaking

    /// Seed 0 is cosmetic here: the orb view renders ended at amplitude 0 (a
    /// perfect circle), because stillness is graphite's message — geometry and
    /// motion both saying "over".
    static let graphite = AuroraPalette(
        coreStops: [Color(white: 0.45), Color(white: 0.28)],
        halo: .clear,
        haloStrength: 0,
        rim: .white.opacity(0.25),
        blobSeed: 0)
}

public extension AuroraPalette {
    /// The kit's control accent — the surge's leading stop, named once so the
    /// widgets' buttons and links cannot drift off the brand violet.
    static let controlAccent: Color = violetSurge.coreStops.first ?? .purple
}

/// How one `AmbientOrbPhase` looks, as a value rather than as view code.
///
/// This is split out of `AmbientLiveActivity` because a Live Activity view is
/// archived SwiftUI rendered out-of-process by WidgetKit: there is no way to
/// snapshot-test one that asserts anything about the product rather than about
/// the harness. The phase → appearance mapping is the part with decisions in it,
/// and as a pure function over value types it is provable in
/// `MagiosTests/AmbientOrbAppearanceTests` with no WidgetKit at all.
///
/// It lives in `Shared/` because that is the only directory both callers see:
/// `MagiosWidgets` compiles `Shared/` but not `Magios/`, and the tests reach it
/// through the app module, which compiles `Shared/` too.
public struct AmbientOrbAppearance: Equatable {
    /// The orb's flat colour, kept for the consumers a gradient cannot tint:
    /// the leash ring's progress, the motion treatment, and the island's
    /// keyline. The minimal dot, which this channel once existed for, now
    /// draws the palette like every other presentation.
    public let fill: Color

    /// Applied by the view as `scaleEffect`, and dropped under Reduce Motion.
    /// Never the sole difference between two states, for that reason.
    public let scale: CGFloat

    /// The status line, shown only in the presentations with room for text.
    public let status: String

    /// Whether the user and the assistant are mid-exchange. The Dynamic Island's
    /// compact trailing slot stays EMPTY when this is false, so an armed window
    /// reads as ambient presence rather than as active recording.
    public let isConversing: Bool

    /// The compact island's phase symbol: what is happening, beside the ring that
    /// says something is. Nil while merely armed and once ended — the compact
    /// slot's emptiness rule extends to it — and nil through heard/connecting,
    /// which carries `compactWord` instead: a glyph here MEANS the session is
    /// live (owner decision, 2026-07-30 — the slot must not claim listening
    /// before it is true, and the same day's capture decision made "not
    /// listening" literal: nothing before session-ready is captured at all; the
    /// `.heard` case has the story). The vocabulary stays deliberately small,
    /// three answers to WHAT (mic, thinking, speaking) rather than one symbol
    /// per phase.
    public let compactGlyph: String?

    /// The compact island's CONTINUOUS word, and non-nil ONLY for
    /// heard/connecting. It rides the LEADING slot, beside the orb (owner
    /// decision, 2026-07-30 — a caption sits with the thing it captions, not
    /// out across the sensor), with no mic anywhere (see `compactGlyph`); the
    /// trailing slot meanwhile holds the connect give-up gauge, drawn from
    /// `ContentState.connectingSince`. The word says why nothing is answering
    /// yet without claiming the session is listening, and the connected signal
    /// is one crossfade read from both slots at once: this word yielding (to
    /// the first pulsed conversing word) and mic + ring arriving trailing.
    /// The leash ring yielding that slot for the
    /// wait is doctrine, not necessity: a countdown matters less during a
    /// twelve-second connect than whether this attempt is going to make it.
    ///
    /// This is one of TWO word regimes, and the view reads both through
    /// `compactWord(showingPhaseWord:)`: the connect word here is continuous
    /// (the whole wait needs explaining), while the conversing phases' words
    /// are PULSED — shown a few seconds in ten, on the app's publish schedule,
    /// because a permanent label would cost the compact island the quiet that
    /// makes it glanceable.
    public let compactWord: String?

    /// What continuous motion this phase is entitled to. See `AmbientOrbMotion`:
    /// this is a statement about what the app knows, not about styling.
    public let motion: AmbientOrbMotion

    /// The full aurora treatment — body gradient, halo, rim — and what every
    /// presentation now draws, down to the minimal dot. `fill` stays alongside
    /// it as the flat tint for the consumers a gradient cannot colour: the
    /// leash ring, the motion treatment, and the island's keyline.
    /// Deliberately no default: a new phase must decide its palette or fail to
    /// compile, same doctrine as the fill.
    public let palette: AuroraPalette

    public init(
        fill: Color,
        scale: CGFloat,
        status: String,
        isConversing: Bool,
        compactGlyph: String?,
        compactWord: String? = nil,
        motion: AmbientOrbMotion = .still,
        palette: AuroraPalette
    ) {
        self.fill = fill
        self.scale = scale
        self.status = status
        self.isConversing = isConversing
        self.compactGlyph = compactGlyph
        self.compactWord = compactWord
        self.motion = motion
        self.palette = palette
    }
}

public extension AmbientOrbAppearance {
    /// The window is over.
    ///
    /// An ended Live Activity keeps rendering its final content for the whole
    /// dismissal window, so the reason has to REPLACE the status word rather
    /// than sit beside it — a finished window still saying "Available — say Hey Sam"
    /// is the same lie as an armed microphone with no orb, inverted. Grey and
    /// small for the same reason: nothing here is live.
    ///
    /// Motion is `.still` by the initialiser's default, and that default is doing
    /// real work here: motion that outlives the thing it depicts is the same lie
    /// in a new costume, and an ended activity keeps rendering for the whole
    /// dismissal window. A bar still elapsing over a window that has closed would
    /// be the worst version of it.
    static func ended(reason: String) -> AmbientOrbAppearance {
        AmbientOrbAppearance(fill: .gray, scale: 0.85, status: reason, isConversing: false, compactGlyph: nil, palette: .graphite)
    }

    /// What the orb should show for a published state — which is **not** always
    /// the phase.
    ///
    /// Lives here rather than in the widget's view file for the reason the whole
    /// type does: this is the last decision on the surface that can be proven,
    /// and taking `isStale` as a plain `Bool` instead of an
    /// `ActivityViewContext` is the only thing separating it from WidgetKit. The
    /// view keeps a two-line adapter and nothing else.
    static func forWindow(
        phase: AmbientOrbPhase,
        endedReason: String?,
        isStale: Bool,
        agentName: String
    ) -> AmbientOrbAppearance {
        if let endedReason { return .ended(reason: endedReason) }
        // Every publish carries `staleDate: expiresAt`, so stale means past the
        // hard cap — the SAME event the cap timer publishes a reason for, seen
        // from a process that was not alive to publish anything. One event, one
        // sentence, whichever side reaches the user first.
        if isStale { return .ended(reason: AmbientEndedReason.capReached) }
        return phase.appearance(agentName: agentName)
    }

    /// Whether the window is over at all — which gates the leash timer, the
    /// caption and the disarm control, none of which mean anything once it is.
    ///
    /// Deliberately beside `forWindow` and reading the same two inputs: if the
    /// two ever disagree, the orb renders a live status word with no timer under
    /// it, or an ended one with a countdown still running. Kept together so that
    /// is one edit to get wrong rather than two, and pinned by a test.
    static func windowIsOver(endedReason: String?, isStale: Bool) -> Bool {
        endedReason != nil || isStale
    }

    /// The compact leading slot's word for one published moment — both word
    /// regimes behind one read, so the view cannot combine them wrongly.
    ///
    /// The connect word (`compactWord`, heard/connecting) is CONTINUOUS and
    /// ignores the flag: the whole wait needs explaining, and its own doctrine
    /// says why. The conversing phases yield their word only while the pulse
    /// flag says so (owner ask, 2026-07-30: the expanded view's words in the
    /// contracted island, a few seconds in every ten), and the word IS the
    /// phase's `status` — "Listening", "Thinking", "Speaking", the exact
    /// vocabulary the expanded status line shows, derived here on both sides
    /// of the wire so the flag and the word cannot disagree. Armed and ended
    /// never yield a word: the flag is trusted only where a conversation is,
    /// so a stale true on a resting or ended payload renders nothing rather
    /// than a lie.
    func compactWord(showingPhaseWord: Bool) -> String? {
        if let compactWord { return compactWord }
        guard isConversing, showingPhaseWord else { return nil }
        return status
    }
}

public extension AmbientOrbPhase {
    /// - Parameter agentName: `AmbientActivityAttributes.agentName`. A blank name
    ///   degrades to a plain "Available" rather than a dangling wake instruction:
    ///   the armed status line is the user's proof that the window is open, so it
    ///   has to read correctly even when the roster lookup came back empty.
    func appearance(agentName: String) -> AmbientOrbAppearance {
        switch self {
        case .armed:
            // Dim, small, and — because there is no render loop out here — the
            // only phase that is *meant* to look static. Armed is ambient
            // presence, not active recording. The in-app orb is full accent and
            // pulsing because a call is genuinely in progress; borrowing that
            // weight for a resting microphone would misreport what is happening.
            let name = agentName.trimmingCharacters(in: .whitespacesAndNewlines)
            return AmbientOrbAppearance(
                fill: .white.opacity(0.35),
                scale: 0.85,
                status: name.isEmpty ? "Available" : "Available — say Hey \(name)",
                isConversing: false,
                compactGlyph: nil,
                palette: .armedEmber
            )
        case .heard:
            // The largest single jump in the whole sequence — dim white to full
            // accent, plus the widest scale. That jump IS the animation: the wake
            // is the one moment the user needs confirmed, and the callers'
            // identity swap cross-dissolves it (a same-identity mutation would
            // hard-cut out-of-process).
            //
            // **But this phase is almost never the wake instant it is named
            // for.** `AmbientState.orbPhase` reduces BOTH `.heard(phrase:)` and
            // `.connecting` onto it, and device instrumentation measured the
            // first at 3–6 ms: nobody has ever seen it as a state of its own.
            // What is on screen is `.connecting`, for a measured 11–13 s, because
            // the backend creates a provider session before `session.ready`. So
            // the wording and the motion below describe CONNECTING — the only
            // thing anyone actually sees. Splitting the two into separate phases
            // would spend a fill, a word and a rate-budgeted publish on rendering
            // 3 ms, and is why the reduction stays at five values.
            //
            // **The wording is activation-neutral because this phase now serves
            // both a wake and an explicit tap.** "Starting conversation" names
            // the wait without falsely claiming the app heard speech on a
            // tap-started turn, and without promising capture before ready.
            // Speech during the wait is deliberately NOT captured, buffered or
            // forwarded (owner decision, 2026-07-30): the wake pre-roll and the
            // connect-window hold-and-flush are gone, because replaying
            // twelve-second-stale speech at ready served no one. The assistant
            // hears from session-ready onward, which is exactly when
            // "Listening" takes over.
            //
            // Indeterminate rather than still because twelve seconds of a
            // motionless glyph reads as a crash. `.still` was never chosen here;
            // it is the initialiser's default, applied while everyone believed
            // this was the 3 ms state. Deliberately NOT `.elapsing`: the
            // backend's readiness is unknown and moved 2.5 s across two
            // consecutive runs, so a bar would be inventing the one thing this
            // surface refuses to invent.
            return AmbientOrbAppearance(
                fill: ambientOrbAccent,
                scale: 1.15,
                status: "Starting conversation…",
                isConversing: true,
                // Nil ON PURPOSE, and since the capture decision above simply
                // true: nothing said during the connect is captured, so a mic
                // glyph would claim a session listening in a window where
                // nothing hears at all. The word below rides beside the orb,
                // mic-free, and the mic first appears — beside the ring — at
                // connect, when hearing actually starts.
                compactGlyph: nil,
                // The doctrine above, at compact size: the word says the wait
                // is work, standing alone. Its removal and mic + ring arriving
                // together are one crossfade — the "now actually listening"
                // signal.
                compactWord: "Starting…",
                motion: .indeterminate,
                palette: .violetSurge
            )
        case .listening:
            // Settles back from `heard` without changing fill: the wake was
            // the event, this is the steady state it settles into. The aurora
            // body does dim — surge to calm — because on the surfaces with
            // room for a gradient, the settle is the palette's to draw.
            return AmbientOrbAppearance(
                fill: ambientOrbAccent,
                scale: 1.0,
                status: "Listening",
                isConversing: true,
                compactGlyph: "mic.fill",
                palette: .calmAurora
            )
        case .thinking:
            // Indeterminate ON PURPOSE, and the one phase where the difference
            // from `speaking` is a decision rather than a limitation. There is
            // deliberately no response deadline anywhere in this stack — a long
            // tool call must not be cut off — so nothing here knows when the
            // answer arrives. A bar across an invented duration would be the orb
            // asserting something the app does not know, which is the failure
            // this whole surface is built to refuse; a pulse asserts only
            // "working", which is true for as long as it runs.
            return AmbientOrbAppearance(
                fill: .orange,
                scale: 0.95,
                status: "Thinking",
                isConversing: true,
                compactGlyph: "ellipsis",
                motion: .indeterminate,
                palette: .amberThinking
            )
        case .speaking:
            // The one phase with a real end: `AssistantPlaybackClock` accumulates
            // each queued frame's own duration, so the instant the speaker runs
            // dry is computed rather than guessed. The view still draws nothing
            // until a span actually arrives (`ContentState.speakingSpan`), so a
            // reply whose span has not settled yet shows no bar rather than a
            // wrong one.
            return AmbientOrbAppearance(
                fill: .green,
                scale: 1.1,
                status: "Speaking",
                isConversing: true,
                compactGlyph: "speaker.wave.2.fill",
                motion: .elapsing,
                palette: .tealSpeaking
            )
        }
    }
}

/// The ended state's one quiet line: what the window actually did.
///
/// Pure and total over its inputs for the same reason the appearance mapping
/// is: this renders on a surface that cannot be tested, so the decision has to
/// be provable off it. `nil` means "show nothing" — a receipt over a duration
/// nobody measured (no `endedAt`, a window that ended the instant it armed, or
/// a clock that ran backwards) would be this feature's signature failure
/// wearing a summary.
public enum AmbientReceipt {
    /// Truncates to whole minutes — sub-minute precision is deliberately not
    /// reported — and speaks hours once a window earns them, because the
    /// ambient leash legally runs that long. An `exchangeCount` of zero or
    /// less drops the exchange clause rather than rendering a zero count.
    public static func line(armedAt: Date, endedAt: Date?, exchangeCount: Int) -> String? {
        guard let endedAt, endedAt > armedAt else { return nil }
        let totalMinutes = Int(endedAt.timeIntervalSince(armedAt) / 60)
        let listened: String
        if totalMinutes < 1 {
            listened = "Listened under a minute"
        } else if totalMinutes < 60 {
            listened = "Listened \(totalMinutes)m"
        } else if totalMinutes % 60 == 0 {
            listened = "Listened \(totalMinutes / 60)h"
        } else {
            listened = "Listened \(totalMinutes / 60)h \(totalMinutes % 60)m"
        }
        guard exchangeCount > 0 else { return listened }
        return "\(listened) · \(exchangeCount) exchange\(exchangeCount == 1 ? "" : "s")"
    }
}
