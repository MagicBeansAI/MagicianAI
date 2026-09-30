import Foundation

/// Bridges keyboard-install status from the extension to the app via the App
/// Group, so the in-app guide can confirm each step instead of just listing them.
/// The keyboard `record`s its state whenever it runs; the app reads it.
public enum KeyboardInstall {
    private static var store: UserDefaults { UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard }
    private static let fullAccessKey = "keyboard.install.fullAccess"
    private static let lastSeenKey = "keyboard.install.lastSeenAt"

    // MARK: written by the keyboard extension

    public static func record(hasFullAccess: Bool) {
        store.set(hasFullAccess, forKey: fullAccessKey)
        store.set(Date().timeIntervalSince1970, forKey: lastSeenKey)
    }

    // MARK: read by the app

    /// Whether the keyboard reported Full Access the last time it ran.
    public static var hasFullAccess: Bool { store.bool(forKey: fullAccessKey) }

    /// The last time the keyboard extension ran (nil = never — not enabled/used yet).
    public static var lastSeen: Date? {
        let t = store.double(forKey: lastSeenKey)
        return t > 0 ? Date(timeIntervalSince1970: t) : nil
    }

    /// The keyboard has run at least once — a reliable proxy for "added and used"
    /// (iOS gives no clean API to detect an enabled-but-never-used keyboard).
    public static var hasRun: Bool { lastSeen != nil }
}
