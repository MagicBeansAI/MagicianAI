import UIKit

/// Light haptic feedback for key presses. Custom keyboards can only play haptics
/// with Full Access, so every call is gated on it.
enum KeyboardHaptics {
    private static let light = UIImpactFeedbackGenerator(style: .light)
    private static let soft = UIImpactFeedbackGenerator(style: .rigid)

    static var enabled = false   // set from hasFullAccess

    /// Warm the Taptic Engine so the first key press isn't a no-op / laggy. Call
    /// when the keyboard appears (once `enabled` is known).
    static func prepare() {
        guard enabled else { return }
        light.prepare()
        soft.prepare()
    }

    static func keyTap() {
        guard enabled else { return }
        light.impactOccurred(intensity: 0.85)
        light.prepare()   // ready the next tap immediately
    }

    static func special() {
        guard enabled else { return }
        soft.impactOccurred(intensity: 1.0)
        soft.prepare()
    }
}
