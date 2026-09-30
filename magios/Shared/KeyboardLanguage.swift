import Foundation

/// The keyboard's autocorrect/suggestion language, chosen in the app and shared with
/// the extension through the App Group (the standard third-party-keyboard pattern —
/// iOS has no user-facing language picker for custom keyboards). Defaults to Indian
/// English.
public enum KeyboardLanguage: String, CaseIterable, Identifiable {
    case india = "en_IN"
    case uk    = "en_GB"
    case us    = "en_US"

    public var id: String { rawValue }

    public var displayName: String {
        switch self {
        case .india: return "English (India)"
        case .uk:    return "English (UK)"
        case .us:    return "English (US)"
        }
    }

    /// `UITextChecker` language candidates in preference order — falls back when the
    /// preferred dictionary isn't present on the device (Indian English follows
    /// British spellings, so it degrades to `en_GB` then `en_US`).
    public var checkerCandidates: [String] {
        switch self {
        case .india: return ["en_IN", "en-IN", "en_GB", "en-GB", "en_US"]
        case .uk:    return ["en_GB", "en-GB", "en_US"]
        case .us:    return ["en_US", "en-US"]
        }
    }

    /// The extension's declared primary language (BCP-47).
    public var primaryLanguage: String {
        switch self {
        case .india: return "en-IN"
        case .uk:    return "en-GB"
        case .us:    return "en-US"
        }
    }
}

public enum KeyboardLanguageStore {
    private static var store: UserDefaults? { UserDefaults(suiteName: MagicianAccess.appGroup) }
    private static let key = "keyboard.language.v1"

    /// The chosen language (default: Indian English).
    public static var current: KeyboardLanguage {
        guard let raw = store?.string(forKey: key), let lang = KeyboardLanguage(rawValue: raw) else {
            return .india
        }
        return lang
    }

    public static func set(_ language: KeyboardLanguage) {
        store?.set(language.rawValue, forKey: key)
    }
}
