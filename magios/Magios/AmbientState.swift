import Foundation

/// Which side of a conversation turn is active. Derived from the realtime call —
/// `RealtimeVoiceClient` reports connection phase and `VoiceAudioEngine` knows
/// when the assistant is speaking — but kept as its own type so the ambient
/// controller can be tested without the transport or the audio graph.
enum AmbientTurn: String, Equatable {
    case listening, thinking, speaking
}

/// What an armed window is actually listening for, and what the measurement
/// matrix has to say about each of those phrases.
///
/// One value rather than two properties because the second is meaningless
/// without the first: a surface holding only the notes would tell the user a
/// phrase wakes on 53% of similar-sounding speech without telling them which
/// one. Both are carried so the in-app bar can say *what* is being listened for
/// — the orb says it on the lock screen, and the orb is not a reachable control
/// while the user is inside the app.
///
/// `notes` is deliberately non-fatal, and deliberately not a warning list. Every
/// armed phrase has one, including the ones that measured best, because
/// `VoskWakeSpotter.assessment(of:)` refuses to draw a line through a continuum
/// of measured rates. Refusing a phrase on its number is a product decision
/// nobody has taken, so this type keeps carrying the number instead of letting
/// the arming path act on it — though no surface renders `notes` any more; the
/// reader is Settings' pre-arm assessment.
struct AmbientPhraseSet: Equatable {
    let phrases: [String]
    let notes: [PhraseNote]
}

/// The armed window's lifecycle.
///
/// `cooldown` is the follow-up window carried by
/// `RealtimeVoiceProtocol.Addressing.followUpWindowMs`: the user may continue
/// without repeating the activation phrase.
enum AmbientState: Equatable {
    case off
    case arming
    case armed
    case heard(phrase: String)
    case connecting
    case conversing(AmbientTurn)
    case cooldown(until: Date)
    /// Carries why the window is ending, when there is a reason worth showing
    /// (cap reached, low battery, microphone permission revoked). This is the
    /// state during which the final content is published, so the reason belongs
    /// here rather than in a variable kept alongside the machine — it is what
    /// fills `AmbientActivityAttributes.ContentState.endedReason`.
    case disarming(reason: String?)
    /// Recoverable only — terminal failures disarm instead of landing here,
    /// which is precisely why the reduction below renders this as `armed`. The
    /// name carries that invariant to every construction site, because the
    /// upstream `RealtimeVoiceProtocol.Event.error` already reports
    /// `recoverable` and dropping the bit here would let a dead window show as
    /// live.
    case recoverableError(message: String)

    /// The orb's five-value reduction. `nil` means no activity should exist.
    var orbPhase: AmbientOrbPhase? {
        switch self {
        case .off, .arming, .disarming:
            return nil
        case .armed, .recoverableError:
            return .armed
        case .heard, .connecting:
            return .heard
        case .cooldown:
            // The microphone is still open for the user specifically, which is
            // not the same as resting.
            return .listening
        case .conversing(let turn):
            switch turn {
            case .listening: return .listening
            case .thinking: return .thinking
            case .speaking: return .speaking
            }
        }
    }

    /// Whether a listening window is open — i.e. whether a surface should offer to
    /// STOP one.
    ///
    /// **Deliberately not `orbPhase != nil`, and that is the whole reason this
    /// exists as its own predicate.** The orb's reduction maps `.recoverableError`
    /// onto `.armed`, which is right there (it is named for a window that is still
    /// listening) and wrong for an in-app control: every refused arm lands in
    /// `.recoverableError` with no window at all, so a control keyed off the
    /// reduction would offer to stop a window that never opened — the inverse lie,
    /// and the one `AmbientMiniBar` was written to avoid.
    ///
    /// Lifted off that bar so the ambient control in Settings answers the same
    /// question the same way. Two copies of this `switch` is exactly how the bug
    /// comes back: a sixth state would be admitted by one and not the other, and
    /// the compiler would say nothing.
    ///
    /// It is state-derived rather than `AmbientController.windowIsLive`, which is
    /// the authoritative answer, because only `state` is `@Published` — a view
    /// keyed off `windowIsLive` would simply not re-render. The two agree
    /// everywhere a view can observe them; they differ only inside `disarmNow`,
    /// which is atomic.
    var windowIsOpen: Bool {
        switch self {
        case .armed, .heard, .connecting, .conversing, .cooldown: return true
        case .off, .arming, .disarming, .recoverableError: return false
        }
    }

    /// Whether the orb's reduction differs across a transition — the *phase*
    /// half of the publish decision, deliberately not the whole of it.
    ///
    /// `ContentState` also carries `caption` and `endedReason`, and this looks at
    /// neither. A caption-only change — the streaming partial transcript during
    /// `conversing(.listening)`, which is exactly when captions move most —
    /// leaves the reduction untouched, so a caller that gated captions on this
    /// alone would freeze the caption for a whole turn while the orb animated
    /// correctly above it. Gate captions separately.
    ///
    /// The caller also picks the ActivityKit verb from the two reductions rather
    /// than from this flag: `nil` → non-`nil` is `Activity.request`, non-`nil` →
    /// `nil` is `end`, and non-`nil` → non-`nil` is `update`.
    static func orbPhaseChanged(from old: AmbientState, to new: AmbientState) -> Bool {
        old.orbPhase != new.orbPhase
    }
}
