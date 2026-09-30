import Foundation

/// The armed-window microphone tap: 16 kHz mono PCM16 frames, no sink.
///
/// Separate from `WakeSpotter` because the tap is what cannot run in the
/// simulator — `AVAudioEngine` microphone capture has no input there — and the
/// controller has to be testable anyway.
///
/// `@MainActor` because the real conforming type will be: every audio
/// collaborator in the app already is (`VoiceAudioEngine`), and a `@MainActor`
/// witness for a nonisolated requirement is a `#ConformanceIsolation` warning
/// today and an error under Swift 6. Typing it also stops the next author
/// reaching for `Task { @MainActor in … }` inside `stop()`, which would make
/// disarm's microphone close asynchronous and let the tap outlive the state
/// transition claiming it was released.
@MainActor
protocol AmbientMicSource: AnyObject {
    /// `onFrame` is called on the AUDIO THREAD, not the main actor — deliberately
    /// a plain closure, since it feeds `WakeSpotter.feed`, which expects that
    /// thread. (It fed `WakePreRoll.append` too, until the 2026-07-30 decision
    /// removed pre-ready capture; the spotter is now the frame's only consumer.)
    ///
    /// `onFailure` is the MID-WINDOW failure channel, and it exists because
    /// throwing can only report a tap that never started. A tap that started
    /// dies routinely — an incoming call or another app taking the microphone,
    /// a route change, permission revoked while armed — and without somewhere to
    /// report it the default is silence: the state stays `.armed`, the orb keeps
    /// saying the user is being heard, and the wake word is dead until the cap
    /// expires. That is the inverse of an armed mic with no orb and lies to the
    /// user just as badly. `ListenController` handles the same class of event
    /// explicitly; this is the seam that lets the ambient tap do the same.
    ///
    /// `@MainActor` on the closure type for the same reason as `WakeSpotter.onHit`:
    /// what a failure leads to is the state machine and the orb, so the hop is
    /// the implementation's obligation and typing it makes that a compile error
    /// to skip rather than a comment to overlook.
    func start(onFrame: @escaping (Data) -> Void, onFailure: @escaping @MainActor (Error) -> Void) throws

    /// Stop capturing.
    ///
    /// **The implementation must guarantee that no `onFrame` OR `onFailure`
    /// callback is in flight or will be delivered once this returns.**
    /// `AVAudioEngine`'s `removeTap`/`stop()` does NOT give that guarantee on
    /// its own, so the conforming type owes the synchronisation — this is a
    /// contract, not best-effort.
    ///
    /// It covers both callbacks for different reasons. A late `onFrame` would
    /// feed a spotter the handoff has already moved past — and until the
    /// 2026-07-30 decision removed the pre-roll ring, it raced that ring's
    /// drain, an exclusivity trap at the wake moment; the contract predates the
    /// decision and stands without it. A late `onFailure` is worse than
    /// useless: the window it refers to is already gone, and if a re-arm
    /// happened in between, it tears down a perfectly healthy window with
    /// "Lost the microphone." Fail-safe in direction — the microphone ends up
    /// off, never on — but the user loses a window that was working.
    func stop()
}
