import Foundation

/// A wake-phrase hit.
struct WakeHit: Equatable {
    let phrase: String
    let at: Date
}

/// What is known about one armed phrase, in a sentence meant for the user.
///
/// **The phrase travels with the note, and that is the point of the type.** An
/// arm can carry several phrases and the surfaces render one line each; a bare
/// list of sentences would tell somebody a phrase is 53% wrong without telling
/// them which one. `VoskWakeSpotter.PhraseAssessment` produces the sentence and
/// this pairs it back up.
struct PhraseNote: Equatable {
    let phrase: String
    let note: String
}

/// On-device activation-phrase detection.
///
/// Implementations MUST NOT have a networking collaborator. That is the whole
/// privacy guarantee of ambient mode: while armed, no audio leaves the device.
/// Keeping the guarantee structural — a type that simply has no way to send
/// anything — is why this is a protocol with a narrow surface rather than a
/// flag on the audio engine.
///
/// The seam is also what keeps the engine choice reversible: the controller is
/// built and tested against this, so Vosk vs. Porcupine (a ~40 MB bundle
/// difference) stays a decision about one conforming type.
protocol WakeSpotter: AnyObject {
    /// Phrases to match, supplied by `RealtimeVoiceProtocol.Addressing`.
    ///
    /// Replaces the match set; it is not additive.
    func configure(phrases: [String])

    /// Feed 16 kHz mono PCM16. Called on the audio thread.
    func feed(_ pcm: Data)

    /// Fires on a match. Invoked on the main actor.
    ///
    /// The two halves of this protocol sit on different actors on purpose:
    /// detection has to run where the audio arrives, but everything a hit leads
    /// to — the state machine, the orb, the call — is main-actor state. So the
    /// hop is the IMPLEMENTATION's obligation, not the caller's, and whoever
    /// sets this may touch controller and UI state directly.
    ///
    /// `@MainActor` is on the closure TYPE so that obligation is enforced rather
    /// than documented: `feed` is nonisolated and synchronous, so a spotter that
    /// invokes this straight from its audio callback does not compile. Written
    /// as a plain closure it would compile clean and corrupt main-actor state at
    /// exactly the moment the feature is doing something visible.
    var onHit: (@MainActor (WakeHit) -> Void)? { get set }

    /// Drop accumulated detection state, so the next armed stretch cannot be
    /// triggered by audio from the previous one.
    func reset()

    /// Whether the last `configure` left anything that can actually fire.
    ///
    /// **The arming path must gate on this.** A spotter with nothing armed never
    /// calls `onHit`, so a window opened over one is an orb claiming the user is
    /// heard above a microphone the wake word can never reach — this feature's
    /// signature failure with the sound turned down. It is on the protocol rather
    /// than only on the concrete type because the controller is the thing that
    /// must refuse, and the controller only ever sees this seam.
    ///
    /// True is necessary but **not** sufficient for a useful window: the phrases
    /// the matrix measured worst all arm and set this. Both have to be read.
    var isArmed: Bool { get }

    /// Phrases from the last `configure` that could not be armed at all.
    ///
    /// Separate from `phraseNotes` because the two need different answers: this
    /// one is un-armable and is the reason a refusal can name, the other armed
    /// and merely has something to say for itself.
    var rejectedPhrases: [String] { get }

    /// What is known about each phrase that DID arm — one sentence per phrase.
    ///
    /// **Reported, never enforced, and never a verdict.** This is not a list of
    /// bad phrases: it is a list of every armed phrase with what the measurement
    /// matrix does or does not say about it, and a phrase nobody has measured
    /// carries a note saying exactly that. It replaced a `weakPhrases: [String]`
    /// bucket, which could only be filled by a threshold on measured rates that
    /// run 6, 20, 33, 40, 46, 53, 60, 66, 80, 93 with no gap — see
    /// `VoskWakeSpotter.assessment(of:)`.
    ///
    /// Arming *silently* is what this exists to prevent: the two-sided version of
    /// the signature failure is a microphone that both fails to wake when called
    /// and wakes when it was not, and the user cannot weigh that without the
    /// number. So it has to reach a surface, not just the log.
    var phraseNotes: [PhraseNote] { get }
}
