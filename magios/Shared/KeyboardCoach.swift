import Foundation

/// Live, in-keyboard onboarding: the keyboard coaches the user through each
/// feature *as they do it*, in the in-app playground. State lives in the App
/// Group so it survives the frequent keyboard reloads and is shared with the app.
public enum KeyboardCoachStep: Int, Codable, Equatable, CaseIterable {
    case typeSomething
    case holdToRewrite
    case tapToAsk
    case skills
    case done

    public var next: KeyboardCoachStep {
        KeyboardCoachStep(rawValue: rawValue + 1) ?? .done
    }

    /// The coach copy shown in the keyboard banner for this step.
    public var title: String {
        switch self {
        case .typeSomething: return "👋 Type a few words to try Magican"
        case .holdToRewrite: return "Hold the spacebar — or tap ✦ — to open Magican"
        case .tapToAsk:      return "Nice! Tap Rewrite or Ask to send your text"
        case .skills:        return "Now try a skill chip — one tap runs it"
        case .done:          return "You're all set ✦"
        }
    }
}

public enum KeyboardCoach {
    private static var store: UserDefaults { UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard }
    private static let armedKey = "keyboard.coach.armed"
    private static let seenKey = "keyboard.coach.seen"
    private static let stepKey = "keyboard.coach.step"

    /// The playground is open — the keyboard should run the coach.
    public static var isArmed: Bool {
        get { store.bool(forKey: armedKey) }
        set { store.set(newValue, forKey: armedKey) }
    }

    /// The user has completed (or skipped) the tutorial at least once.
    public static var isSeen: Bool {
        get { store.bool(forKey: seenKey) }
        set { store.set(newValue, forKey: seenKey) }
    }

    public static var step: KeyboardCoachStep {
        get { KeyboardCoachStep(rawValue: store.integer(forKey: stepKey)) ?? .typeSomething }
        set { store.set(newValue.rawValue, forKey: stepKey) }
    }

    /// The keyboard should show the coach right now.
    public static var isActive: Bool { isArmed }

    /// Called by the playground when it appears.
    public static func arm() {
        step = .typeSomething
        isArmed = true
    }

    /// Called by the playground when it disappears (without finishing).
    public static func disarm() {
        isArmed = false
    }

    /// Finished (reached the end or skipped).
    public static func finish() {
        isSeen = true
        isArmed = false
    }
}
