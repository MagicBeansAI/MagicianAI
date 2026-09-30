import ActivityKit
import AppIntents
import Foundation

/// Backs the **Stop** button on the observation Live Activity / Dynamic Island.
/// Runs WITHOUT opening the app: it stops the server session (which cascades to
/// the in-app capture via a `410` on the next chunk) and ends any observation
/// Live Activities — so a tap on the lock screen fully stops capture even if the
/// app is suspended.
public struct StopObservationIntent: AppIntent {
    public static var title: LocalizedStringResource = "Stop observing"
    public static var description = IntentDescription("Stops the current Magican observation.")
    public static var openAppWhenRun = false

    public init() {}

    public func perform() async throws -> some IntentResult {
        if let arm = ObservationArm.claim() {
            await ObservationUplinkClient().stopSession(sessionId: arm.sessionId)
            ObservationArm.clear()
        }
        for activity in Activity<ObservationActivityAttributes>.activities {
            await activity.end(
                ActivityContent(state: ObservationActivityAttributes.ContentState(phase: "ended"), staleDate: nil),
                dismissalPolicy: .immediate
            )
        }
        return .result()
    }
}
