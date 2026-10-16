import ActivityKit
import Foundation

/// The five values the ambient orb can show. Deliberately small: Live Activity
/// updates are rate-budgeted by the system, so the orb is driven by state
/// TRANSITIONS, never by microphone level. (Two priced exceptions: the connect
/// RETRY republishes `heard` without a transition, because the island's
/// give-up gauge must restart with the fresh attempt — one extra publish per
/// retry, bounded by the single automatic retry per wake — and the compact
/// PHASE-WORD PULSE flips `showsPhaseWord` mid-phase, ≤2 publishes per
/// `AmbientPhaseWordPulse.cadenceSeconds`, only while conversing — bounded
/// minutes — and zero while armed, where the emptiness rule stands.)
///
/// `CaseIterable` exists for the tests rather than for any production caller,
/// and it closes a real gap. The phase → appearance mapping is a `switch`, so a
/// sixth case cannot be *rendered* without someone deciding how — the compiler
/// says so. But the invariants that matter here are quantified over ALL phases
/// (no two share a colour, none differs from `armed` by scale alone, none reads
/// as ended), and a hand-maintained list of cases would let a new phase slip
/// past every one of them while still compiling. On the one surface that must
/// not lie about whether the user is being heard, the coverage has to come from
/// the type rather than from someone remembering.
public enum AmbientOrbPhase: String, CaseIterable, Codable, Hashable {
    case armed, heard, listening, thinking, speaking

    /// The value half of `ContentState`'s forward-compatibility defence: that
    /// decoder covers a *missing* key, this covers a key present with a raw
    /// value this binary does not know.
    ///
    /// A Live Activity started by a previous build can still be on screen after
    /// an app update, and the system hands its persisted `ContentState` to the
    /// new widget binary. The synthesised `RawRepresentable` decoding throws on
    /// an unrecognised raw value, and that throw would still propagate out and
    /// take the whole `ContentState` down — leaving the user an armed microphone
    /// and no rendered orb, which is the only disarm control. Degrade instead:
    /// `armed` is the honest reading, since an activity exists only while armed.
    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .armed
    }
}

/// Who a transcript caption line belongs to. String-raw for the wire; decoded
/// tolerantly in `ContentState` so an unknown role degrades to nil rather than
/// un-rendering the orb.
///
/// Deliberately `Encodable` and not `Decodable`. The decode seam reads the raw
/// String and maps it, so a direct `decodeIfPresent(AmbientCaptionRole.self, …)`
/// anywhere would reintroduce the throw-on-unknown the mapping exists to avoid.
/// Withholding the conformance makes that a compile error until someone decides
/// what an unknown role should mean — the same property the hand-written
/// `ContentState` decoder claims for the whole state.
public enum AmbientCaptionRole: String, Encodable, Hashable {
    case user, agent
}

/// When the reply the user is hearing started, and when it runs out.
///
/// **The only duration in this feature the app actually knows**, which is the
/// whole reason it is carried: `AssistantPlaybackClock` models the audio player's
/// queue frame by frame, so the end of a reply is a computed fact rather than an
/// estimate, and a progress bar drawn across it asserts nothing the app cannot
/// back up. Nothing else here has a knowable end — `thinking` deliberately has no
/// response deadline, so it gets an indeterminate treatment instead.
///
/// **`init` is failable, and that is the point of the type.** A `ClosedRange<Date>`
/// would have been the obvious shape and is the wrong one: constructing one with
/// `until < from` *traps*, and `ClosedRange`'s own `Decodable` preconditions the
/// same way — so a stale or reordered payload could crash the widget while
/// decoding, which is precisely the class of failure `ContentState.init(from:)`
/// exists to make impossible. Two plain `Date`s on the wire plus one failable
/// construction means an inverted pair degrades to "no bar" instead.
public struct AmbientSpeakingSpan: Equatable, Hashable {
    public let from: Date
    public let until: Date

    /// `nil` unless the two dates describe a forward span of nonzero length.
    public init?(from: Date, until: Date) {
        guard until > from else { return nil }
        self.from = from
        self.until = until
    }
}

/// The connect attempt's give-up clock, single-sourced so the island's gauge
/// and the client's watchdog can never drift.
///
/// During heard/connecting the compact island draws a gauge filling toward
/// "this attempt gives up" — the one deadline the app genuinely enforces
/// during a connect (`thinking` deliberately has none, and the backend's
/// readiness is unknowable). The deadline is the transport's:
/// `RealtimeVoiceClient.startupTimeoutSeconds` bounds each SOCKET attempt and
/// `readyWaitTimeoutSeconds` — one second past it — is the ambient attempt's
/// overall wall clock, the moment `startCall` returns false. Both are defined
/// FROM these values, so a retuned watchdog retunes the gauge in the same
/// edit. Lives in `Shared/` because the widget draws the window and the app
/// enforces it; two copies is how a gauge comes to depict a deadline nobody
/// enforces.
public enum AmbientConnectAttemptWindow {
    /// Bounds each SOCKET attempt (the client's startup watchdog).
    public static let socketAttemptSeconds: TimeInterval = 45
    /// The ambient attempt's overall give-up clock (the sink's ready wait) —
    /// the range the island's connect gauge fills over.
    public static let attemptSeconds: TimeInterval = socketAttemptSeconds + 1
}

/// The compact island's phase-word pulse: how long the word shows, and how
/// often it returns while a phase persists (owner ask, 2026-07-30: the
/// expanded view's words in the contracted island, "for ~2-3 s, roughly every
/// 10 s"). Both owner-tunable — but the cadence must EXCEED the show, because
/// the pulse driver disables itself otherwise rather than spin publishes
/// through a zero-length hidden window; equal or inverted values degrade to a
/// word that rides phase transitions only and never hides mid-phase, which is
/// still phase-true, just not a pulse.
///
/// In `Shared/` beside the flag it drives (`ContentState.showsPhaseWord`)
/// even though only the app schedules with these: the widget's comments cite
/// the cadence as the accepted width-breathing cost, and a retune should be
/// one edit next to the field it shapes.
public enum AmbientPhaseWordPulse {
    /// How long each pulse keeps the word on screen.
    public static let showSeconds: TimeInterval = 3
    /// How often a pulse begins while the phase persists.
    public static let cadenceSeconds: TimeInterval = 10
}

/// Live Activity backing the ambient orb (Dynamic Island + lock screen).
public struct AmbientActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        public var phase: AmbientOrbPhase
        /// Latest caption line, shown only in the expanded presentations.
        public var caption: String
        /// Who said the caption line. Nil for system lines (the power warning)
        /// and for payloads from builds that predate the field — an
        /// unattributed caption still renders.
        public var captionRole: AmbientCaptionRole?
        /// Completed exchanges (assistant replies) this window. Rides publishes
        /// that happen anyway rather than earning its own; the receipt renders
        /// it when the window ends. Zero for payloads that predate it.
        public var exchangeCount: Int
        /// Set when the window ended for a reason worth explaining
        /// (cap reached, low battery, microphone permission revoked).
        public var endedReason: String?
        /// When the window actually ended. Set only on the final publish, so
        /// the receipt never invents a duration; nil while the window is live
        /// and for payloads that predate it.
        public var endedAt: Date?
        /// When the reply currently coming out of the speaker began, and when the
        /// audio queued for it runs dry. Both nil unless the assistant is audibly
        /// speaking AND the queue has stopped growing — see
        /// `RealtimeAmbientCallSink.queueSettleSeconds`. Read through
        /// `speakingSpan`, never as a pair.
        public var speakingFrom: Date?
        public var speakingUntil: Date?

        /// The instant the CURRENT connect attempt began — the compact island's
        /// give-up gauge fills from here toward
        /// `AmbientConnectAttemptWindow.attemptSeconds`. Non-nil ONLY while
        /// heard/connecting, and each retry republishes a fresh value, so the
        /// gauge honestly restarts with the attempt; nil everywhere else
        /// (armed, conversing, ended), which is the gauge's existence rule.
        /// Nil also for payloads from builds that predate the field: the slot
        /// degrades to empty, never to a guessed deadline.
        public var connectingSince: Date?

        /// The current leash expiry. The initial value duplicates the immutable
        /// attribute deliberately: ActivityKit attributes cannot change, while
        /// the Live Activity's Extend button must move the countdown and stale
        /// date without replacing the activity. Nil means an old payload; every
        /// reader then falls back to `AmbientActivityAttributes.expiresAt`.
        public var expiresAt: Date?

        /// The compact slot's phase-word pulse flag: while true, the leading
        /// slot shows the conversing phase's word beside the orb. The word
        /// itself is NEVER carried on the wire — both sides derive it from
        /// `phase` (`AmbientOrbAppearance.compactWord(showingPhaseWord:)`), so
        /// the flag and the word cannot disagree, and a dropped hide-publish
        /// leaves a TRUE word up longer rather than a wrong one. False for
        /// payloads from builds that predate the field, which is also the
        /// honest resting reading: no pulse, no word.
        public var showsPhaseWord: Bool

        /// The span as one value, or nil when there is no honest one to show.
        ///
        /// Every reader goes through here so an inverted or half-written pair
        /// renders as "no bar" rather than trapping — see `AmbientSpeakingSpan`.
        public var speakingSpan: AmbientSpeakingSpan? {
            guard let speakingFrom, let speakingUntil else { return nil }
            return AmbientSpeakingSpan(from: speakingFrom, until: speakingUntil)
        }

        public init(
            phase: AmbientOrbPhase,
            caption: String = "",
            captionRole: AmbientCaptionRole? = nil,
            exchangeCount: Int = 0,
            endedReason: String? = nil,
            endedAt: Date? = nil,
            speakingSpan: AmbientSpeakingSpan? = nil,
            connectingSince: Date? = nil,
            expiresAt: Date? = nil,
            showsPhaseWord: Bool = false
        ) {
            self.phase = phase
            self.caption = caption
            self.captionRole = captionRole
            self.exchangeCount = exchangeCount
            self.endedReason = endedReason
            self.endedAt = endedAt
            speakingFrom = speakingSpan?.from
            speakingUntil = speakingSpan?.until
            self.connectingSince = connectingSince
            self.expiresAt = expiresAt
            self.showsPhaseWord = showsPhaseWord
        }

        public func effectiveExpiresAt(fallback: Date) -> Date {
            expiresAt ?? fallback
        }

        /// Decoded as a wire format rather than as a struct: no single field may
        /// throw the whole state away.
        ///
        /// The system persists this and hands it to whichever widget binary is
        /// installed later, so a payload written by an older build can reach a
        /// newer one after an app update. Synthesised decoding throws
        /// `keyNotFound` for any non-optional field the old payload predates, and
        /// one throw destroys the entire `ContentState` — which un-renders the
        /// orb, the only disarm control the user has. So every field falls back
        /// to the same default its memberwise `init` uses.
        ///
        /// Writing this out by hand is also what keeps the hole shut: adding a
        /// field becomes a compile error here until someone decides what an old
        /// payload without it should mean.
        public init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            phase = try container.decodeIfPresent(AmbientOrbPhase.self, forKey: .phase) ?? .armed
            caption = try container.decodeIfPresent(String.self, forKey: .caption) ?? ""
            endedReason = try container.decodeIfPresent(String.self, forKey: .endedReason)
            // Added after the first shipped build, which is exactly the case this
            // decoder was hand-written for: an activity started by that build is
            // still on screen after the update, and its persisted payload has
            // neither key. Absent means "no reply is playing", which is also what
            // the memberwise default means.
            speakingFrom = try container.decodeIfPresent(Date.self, forKey: .speakingFrom)
            speakingUntil = try container.decodeIfPresent(Date.self, forKey: .speakingUntil)
            // The same doctrine, both directions at once: an OLDER build's
            // payload has none of these keys, and a NEWER build's may carry a
            // role this binary has never heard of. Either way the state must
            // render, not throw. Decoding the role as a plain String and then
            // mapping it is what turns an unknown role into an unattributed
            // caption instead of a `DecodingError`.
            captionRole = (try container.decodeIfPresent(String.self, forKey: .captionRole))
                .flatMap(AmbientCaptionRole.init(rawValue:))
            exchangeCount = try container.decodeIfPresent(Int.self, forKey: .exchangeCount) ?? 0
            endedAt = try container.decodeIfPresent(Date.self, forKey: .endedAt)
            // Absent — an old build's payload — means "don't draw the connect
            // gauge", which is also the honest reading of not knowing when the
            // attempt began: the slot degrades to empty, never to a guess.
            connectingSince = try container.decodeIfPresent(Date.self, forKey: .connectingSince)
            // A payload from before Extend used only the immutable attribute.
            // Nil retains that exact behavior through `effectiveExpiresAt`.
            expiresAt = try container.decodeIfPresent(Date.self, forKey: .expiresAt)
            // Same doctrine for the pulse flag: absent means no pulse, which
            // renders as no word — the resting reading, never a stuck one.
            showsPhaseWord = try container.decodeIfPresent(Bool.self, forKey: .showsPhaseWord) ?? false
        }
    }

    /// The agent's display name, so the orb says "Sam" rather than a build constant.
    public var agentName: String
    public var armedAt: Date
    /// Hard cap, so the lock-screen presentation can show the remaining leash.
    public var expiresAt: Date

    public init(agentName: String, armedAt: Date, expiresAt: Date) {
        self.agentName = agentName
        self.armedAt = armedAt
        self.expiresAt = expiresAt
    }
}
