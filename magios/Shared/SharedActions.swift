import Foundation

/// One-shot action handoff from an App Intent (which may run out-of-process) to
/// the app, via App Group UserDefaults. The intent sets a pending action and
/// opens the app; the app consumes it on foreground and routes it (e.g. start a
/// new chat). Foundation-only so the intents layer can link it.
public enum SharedActions {
    /// Stable WidgetKit identity for the system Talk control. Both the widget
    /// extension and the containing app use this value: the extension declares
    /// the control and the app asks Control Center to refresh an installed copy
    /// after an upgrade.
    public static let ambientControlKind = "ai.magicbeans.Magican.ambient-control"

    /// Compatibility route for older installed widgets and direct automations.
    /// A bare URL now migrates to ambient Talk; explicit `mode` query values keep
    /// their requested one-shot surface. Current Home Screen, Lock Screen,
    /// StandBy, Control Center, and Action Button surfaces invoke
    /// `ArmAmbientIntent` directly.
    public static let configuredVoiceURL = URL(string: "magican://voice")!

    /// Opens the app onto the ambient window that is already running without
    /// asking for another turn. The Live Activity uses this route; reusing
    /// `configuredVoiceURL` there would start a fresh turn on top of the ambient
    /// session it was meant only to reveal.
    public static let ambientURL = URL(string: "magican://ambient")!

    /// Stable wire values shared by the app, widgets, and App Intents. Legacy
    /// voice values remain decodable so installed controls survive an upgrade;
    /// the app migrates all three mode-neutral values onto `ambientArm`.
    public enum PendingAction {
        public static let newChat = "new-chat"
        public static let voiceDictation = "voice"
        public static let voiceConfigured = "voice-configured"
        public static let legacyVoiceCall = "voice-live"
        public static let observe = "observe"
        public static let tutorBlackboard = "tutor-blackboard"
        /// `ArmAmbientIntent`. Kept distinct from `observe`: that one starts a
        /// server-side room capture the user can stop from inside the app, while
        /// this one arms a local microphone tap whose only outside control is the
        /// orb — routing one to the other would put the wrong stop button on a
        /// live microphone.
        public static let ambientArm = "ambient-arm"
    }

    private static var store: UserDefaults { UserDefaults(suiteName: SharedInbox.appGroup) ?? .standard }
    private static let key = "pending_action"

    public static func setPending(_ action: String) { store.set(action, forKey: key) }

    public static func setPendingThread(_ threadID: String) {
        let trimmed = threadID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        setPending("thread:\(trimmed)")
    }

    public static func setPendingTask(_ taskID: String) {
        let trimmed = taskID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        setPending("task:\(trimmed)")
    }

    public static func consumePending() -> String? {
        guard let action = store.string(forKey: key) else { return nil }
        store.removeObject(forKey: key)
        return action
    }
}
