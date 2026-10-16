import AppIntents
import Foundation

/// Adds one bounded increment to the currently live ambient window without
/// opening Magican.
///
/// The widget process never changes `AmbientArm` or the Live Activity itself:
/// only the app process owns the microphone timer, so repainting the countdown
/// here would create a dangerous split where the UI promised more time but the
/// original timer still stopped capture. This intent records one command and
/// wakes the already-running app through the shared Darwin channel; the app
/// atomically updates its timer, persisted arm, and ActivityKit content.
public struct ExtendAmbientIntent: AppIntent {
    public static var title: LocalizedStringResource = "Extend ambient listening"
    public static var description = IntentDescription("Adds 30 minutes to the current Magican listening window.")
    public static var openAppWhenRun = false

    public init() {}

    public func perform() async throws -> some IntentResult {
        // A dead app cannot have a live ambient microphone. Refuse to leave a
        // request that a later, unrelated window could consume.
        guard let arm = AmbientArm.claim(), !arm.isExpired(),
              AmbientExtensionPolicy.extendedExpiry(
                  armedAt: arm.armedAt,
                  currentExpiry: arm.expiresAt
              ) != nil else {
            AmbientSignal.clearPendingExtension()
            return .result()
        }

        AmbientSignal.requestExtension()
        AmbientSignal.postExtension()
        return .result()
    }
}
