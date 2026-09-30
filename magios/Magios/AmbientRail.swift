import Foundation

/// The sentences an armed window can end on when something else needs the
/// microphone.
///
/// Beside `AmbientRail` rather than in `Shared/AmbientEndedReason` for a reason
/// that type states itself: the shared end reasons are published from more than one module —
/// the app's cap timer and the widget's orphan sweep — and `Shared/` is the only
/// place a single value can reach both. These are published by the app and only
/// the app, because only the app has a dictation session or an observation
/// session to yield to. Putting them in `Shared/` would mean compiling them into
/// the widget extension, the keyboard and the share sheet to say something none
/// of them can ever say.
///
/// All name the thing the user just did, not the thing that stopped. "Magican
/// stopped listening" on its own reads as a fault; naming the cause makes it
/// read as a consequence, which is what it is — and tells them re-arming will
/// work.
enum AmbientYieldReason {
    /// The user started dictating. See `AmbientRail`.
    static let dictationStarted = "Stopped listening so you could dictate."

    /// The user started an observation session.
    static let observationStarted = "Stopped listening so Magican could record this session."

    /// A voice request is handing control to the full-screen Tutor experience.
    /// Tutor owns narration and interactive focus, so keeping the ambient window
    /// armed would suppress its speech behind `VoiceCallAudioFocus`.
    static let tutorStarted = "Stopped listening so Tutor could take over."
}

/// The rail every other audio path in the app checks before it touches the
/// shared `AVAudioSession`.
///
/// ## Why this exists at all
///
/// An armed ambient window depends on one property that no other feature in the
/// app has any reason to know about: **the shared audio session is activated in
/// the foreground and never deactivated.** Apple DTS (thread 826462) states the
/// rule as a recipe — hold the `audio` background mode, only ever activate while
/// visible — and the consequence of breaking it is silent and delayed. A
/// deactivated session cannot be reactivated from the background, so the window
/// does not fail at the moment of the deactivation; it fails at the *next wake
/// word*, off screen, with the orb still saying the user is being heard. Nobody
/// is watching a screen at that moment, which is the whole point of the feature.
///
/// `AmbientMicEngine` therefore contains no `setActive(false)` at all, and says
/// so three times. But three neighbours do contain one, and none of them is
/// wrong to: `DictationController`, `ListenController` and `BackgroundEngine`
/// each own a capture or playback session of their own and each releases it
/// politely when finished. The hazard is only the *combination*, so the fix
/// belongs at the combination rather than inside any one of them.
///
/// ## Two verbs, because the three cases are not the same case
///
/// - **`yield`** — the user has just asked, in the foreground, for something
///   that needs the microphone. Dictation and observation are both this. The
///   right answer is to end the ambient window *and say why*: a user holding
///   their phone and pressing hold-to-talk wants dictation, and refusing it to
///   protect a background convenience would read as a broken microphone.
///   Design §9 already ranks observation above ambient for the same reason.
/// - **`windowIsLive`** — nobody asked for anything; a background task keepalive
///   is finishing and is about to tidy up a session it does not exclusively own.
///   The right answer is to **leave the session alone**, because a Siri task
///   dispatch completing is not a decision by the user to stop listening. This
///   is the backstop underneath `yield` as well: it makes a deactivation
///   *structurally* impossible under an armed window rather than unreachable by
///   inspection, which is the lesson design §15 records after that table was
///   wrong twice.
///
/// ## Why it is injected rather than reaching for the singleton
///
/// Same shape and same reason as `AmbientController.observationIsActive`: the
/// production default reads `AmbientController.shared`, and a test swaps the
/// whole rail. Reaching for the singleton inline would make every dictation and
/// observation test construct a Vosk spotter, a microphone engine and a realtime
/// call stack to answer one boolean.
struct AmbientRail {

    /// Whether an ambient window is open right now. **A `false` answer is the
    /// only licence to deactivate the shared audio session.**
    var windowIsLive: @MainActor () -> Bool = { AmbientController.shared.windowIsLive }

    /// End the armed window, with a reason the orb shows, because a foreground
    /// capture the user just asked for needs the microphone.
    var yield: @MainActor (String) -> Void = { reason in
        AmbientController.shared.yieldForForegroundCapture(reason: reason)
    }

    /// The production rail. A `static let` so the closures above are *built* once
    /// and `AmbientController.shared` is only *touched* when one of them actually
    /// runs — a test that never trips a rail never constructs the controller.
    static let live = AmbientRail()
}
