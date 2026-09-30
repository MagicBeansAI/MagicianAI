import ActivityKit
import AppIntents
import Foundation

/// Backs the **Disarm** button on the ambient orb — the only way to stop an armed
/// microphone without opening the app.
///
/// Runs in the widget process with `openAppWhenRun = false`, like
/// `StopObservationIntent`, and that is where the resemblance stops. Stopping an
/// observation is a server call the in-app capture trips over on its next chunk
/// upload; an armed ambient window is a local microphone tap with no server side
/// at all, so this intent has to reach the app process itself and then find out
/// whether it arrived.
///
/// **The orb disappearing must never be optimistic.** Ending the activity because
/// we asked for a disarm — rather than because one happened — is this feature's
/// signature failure: the orb vanishes, the app never got the signal, and the
/// microphone keeps running while the user believes it stopped. So the activity
/// is ended here only in the one case where the evidence says nothing is
/// listening, and never merely because a timer ran out.
public struct DisarmAmbientIntent: AppIntent {
    public static var title: LocalizedStringResource = "Stop ambient listening"
    public static var description = IntentDescription("Stops Magican listening for its activation phrase.")
    public static var openAppWhenRun = false

    /// How long to wait for the app to confirm it actually stopped.
    ///
    /// The budget is one-sided: waiting LONGER is strictly safer, because the
    /// only thing it buys is tolerance for a busy app, and the only thing it
    /// costs is a moment's delay in the case where nothing is listening anyway.
    /// A budget too short is what produces the failure being designed against —
    /// a busy app reads as a dead one and its live orb gets collected. So this is
    /// the longest wait that still feels like a button rather than a hang.
    ///
    /// It is also not the safety mechanism. See `perform()`: the decision to end
    /// the orb rests on whether the request was picked up, not on this expiring.
    private static let acknowledgementBudget: TimeInterval = 1.0

    public init() {}

    public func perform() async throws -> some IntentResult {
        let request = AmbientSignal.requestDisarm()
        // Record first, notification second: the notification is the faster
        // route, and an app that acts on it goes looking for the record.
        AmbientSignal.postDisarm()

        switch await AmbientSignal.awaitAcknowledgement(of: request, within: Self.acknowledgementBudget) {
        case .acknowledged:
            // The app stopped the tap for real and has taken the orb down. There
            // is nothing left here to do, and nothing to guess about.
            return .result()
        case .cancelled:
            // Cut short — possibly after a single poll interval. The app has not
            // had its budget, so the evidence check below would be reading an
            // outstanding request that means "not yet" as if it meant "nobody is
            // there", and collecting the orb off a healthy, still-listening app.
            // Leave everything standing: the request survives for the app's
            // resume path, and a genuinely orphaned orb is swept by
            // `AmbientActivity.start` and `reconcileOnLaunch` regardless.
            return .result()
        case .silent:
            break
        }

        // A full budget with no acknowledgement — which on its own still says
        // nothing about the microphone. The evidence that does is whether the
        // request was PICKED UP:
        //
        // - Still outstanding: no live app read it. Either nothing is running —
        //   in which case nothing is listening either, since an ambient window
        //   cannot survive its process (see `AmbientArm`) — or a process that is
        //   running took the notification and stopped without being able to read
        //   the record. Both mean the microphone is off and the orb on screen is
        //   an orphan, so collect it.
        //
        // - Consumed: a live app is disarming right now and owes the acknowledgement
        //   this wait did not see. It ends the orb itself, a moment after this
        //   returns. Ending it here would be racing the only process that knows.
        //
        // Note how narrowly the first branch reasons: it is safe *precisely
        // because* ambient dies with its process. Do not generalise it to
        // anything that has a server side.
        guard AmbientSignal.pendingDisarm() == request else { return .result() }
        // The request is deliberately LEFT STANDING. It is the app's resume-path
        // evidence, and the one scenario this branch can be wrong about — an app
        // that is armed but whose main actor stalled past the budget — is exactly
        // the scenario where that evidence is the difference between a microphone
        // that stops when the app unblocks and one that keeps running with no orb
        // above it. Clearing it here would delete the backstop while ending the
        // orb, in the single case where both matter.
        //
        // What a surviving request risks is a later window being disarmed by a tap
        // meant for an earlier one: microphone off, the safe direction, and
        // `reconcileOnLaunch` already sweeps one left by a dead process.
        await endOrphanedOrbs()
        return .result()
    }

    /// Identical content and dismissal to `AmbientActivity.endOrphans`, because it
    /// is the same act: an orb whose window no longer exists. The sentence is
    /// `AmbientEndedReason.orphanCollected` rather than a literal, so that the
    /// sameness is held by the type across the process boundary instead of by two
    /// comments agreeing.
    ///
    /// **Not the cap's sentence, and the difference is load-bearing.**
    /// `capReached` asserts a *cause* — a window that ran its full course, which
    /// the app knows because its own timer fired. A sweep asserts nothing, because
    /// whoever is sweeping genuinely does not know why the window died. Claiming a
    /// cause nobody observed is the failure mode of the one surface whose entire
    /// job is not telling the user the wrong thing about whether they are heard.
    ///
    /// Written honestly even though `.immediate` should mean it never renders —
    /// the one frame it might get must not be the orb claiming the user is still
    /// being heard.
    private func endOrphanedOrbs() async {
        for activity in Activity<AmbientActivityAttributes>.activities {
            await activity.end(
                ActivityContent(
                    state: AmbientActivityAttributes.ContentState(
                        phase: .armed,
                        endedReason: AmbientEndedReason.orphanCollected
                    ),
                    staleDate: nil
                ),
                dismissalPolicy: .immediate
            )
        }
    }
}
