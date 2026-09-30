import Foundation

/// The one extension rule shared by the widget button, the app-owned leash,
/// and Settings' longest initial leash.
///
/// A Live Activity cannot remain on screen past eight hours, so an ambient
/// microphone must never be extended beyond the point where its only external
/// Stop control disappears. Each tap adds a deliberately legible 30 minutes;
/// the final tap may add less when it lands near the eight-hour ceiling.
public enum AmbientExtensionPolicy {
    public static let incrementSeconds: TimeInterval = 30 * 60
    public static let maximumWindowSeconds: TimeInterval = 8 * 60 * 60

    /// The next expiry, or nil when the window is already at its hard ceiling.
    public static func extendedExpiry(armedAt: Date, currentExpiry: Date) -> Date? {
        let ceiling = armedAt.addingTimeInterval(maximumWindowSeconds)
        let candidate = min(
            currentExpiry.addingTimeInterval(incrementSeconds),
            ceiling
        )
        return candidate > currentExpiry ? candidate : nil
    }
}

/// Persisted record of an armed ambient window, kept in the App Group so the
/// widget-process disarm intent and the app agree on what is running.
///
/// Unlike `ObservationArm`, this is NOT adoptable after a relaunch. An
/// observation session lives on the server and can be rejoined; an ambient
/// window is a local microphone tap that dies with the process. A record whose
/// owner is not the running process is garbage to collect.
public struct AmbientArm: Codable, Equatable {
    public let armedAt: Date
    public let capSeconds: Int
    /// Identifies the process that armed. Regenerated per launch.
    public let ownerID: String

    public init(armedAt: Date, capSeconds: Int, ownerID: String) {
        self.armedAt = armedAt
        self.capSeconds = capSeconds
        self.ownerID = ownerID
    }

    public var expiresAt: Date { armedAt.addingTimeInterval(TimeInterval(capSeconds)) }

    public func isExpired(now: Date = Date()) -> Bool { now >= expiresAt }

    public func isStale(currentOwnerID: String) -> Bool { ownerID != currentOwnerID }

    // MARK: storage

    private static let key = "ambient.activeArm"
    private static var store: UserDefaults {
        UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
    }

    public func save() {
        if let data = try? JSONEncoder().encode(self) {
            Self.store.set(data, forKey: Self.key)
        }
    }

    /// Returns whatever is stored, WITHOUT judging whether it is still ours.
    /// Staleness is relative to the process holding the live microphone tap, and
    /// only the app can supply that owner id — the widget extension runs in its
    /// own process and would find every record stale, including a live one. So
    /// the read stays dumb about ownership and the caller decides.
    ///
    /// Undecodable bytes are a different question and are dropped on the spot:
    /// corrupt is not stale, and a record no build can read is useless to both
    /// processes. Without this the first commit to add a non-optional field
    /// leaves every previously-saved record permanently resident, since a caller
    /// seeing `nil` has no reason to clear.
    public static func claim() -> AmbientArm? {
        guard let data = store.data(forKey: key) else { return nil }
        guard let arm = try? JSONDecoder().decode(AmbientArm.self, from: data) else {
            clear()
            return nil
        }
        return arm
    }

    public static func clear() {
        store.removeObject(forKey: key)
    }
}
