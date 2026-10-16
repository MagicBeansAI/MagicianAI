import Foundation

/// A keyboard Skill chip: a one-tap binding to one of the three lanes. Pure
/// value type + App Group store (app-host testable).
///
/// - `write`: `template` is the rewrite guidance; the keyhole (typed text) is the
///   text to rewrite.
/// - `ask`:   `template` is the question; `{text}` is replaced with the typed text.
/// - `act`:   `template` is the task goal; `{text}` is replaced with the typed
///   text. Act ALWAYS shows a verification card before running.
public struct KeyboardSkill: Identifiable, Codable, Equatable {
    public enum Lane: String, Codable, Equatable {
        case write, ask, act
    }

    public var id: String
    public var label: String
    public var symbol: String   // SF Symbol name
    public var lane: Lane
    public var template: String
    /// For the Write lane only: which contextual-writing action —
    /// `"rewrite"` (default), `"reply"`, or `"continue"` (magician's Writing Help
    /// actions). `template` becomes extra guidance for that action.
    public var writeAction: String?

    public init(
        id: String = UUID().uuidString,
        label: String,
        symbol: String,
        lane: Lane,
        template: String,
        writeAction: String? = nil
    ) {
        self.id = id
        self.label = label
        self.symbol = symbol
        self.lane = lane
        self.template = template
        self.writeAction = writeAction
    }

    /// Resolve `{text}` against the typed text (for ask/act).
    public func resolved(text: String) -> String {
        template.replacingOccurrences(of: "{text}", with: text)
    }
}

/// App Group-backed store for the user's skill chips, seeded with a default pack.
public enum KeyboardSkillStore {
    private static let key = "keyboard.skills.v1"
    private static var store: UserDefaults { UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard }

    public static func load() -> [KeyboardSkill] {
        if let data = store.data(forKey: key),
           let skills = try? JSONDecoder().decode([KeyboardSkill].self, from: data),
           !skills.isEmpty {
            return skills
        }
        return defaultPack
    }

    public static func save(_ skills: [KeyboardSkill]) {
        if let data = try? JSONEncoder().encode(skills) {
            store.set(data, forKey: key)
        }
    }

    public static func resetToDefault() {
        store.removeObject(forKey: key)
    }

    public static let defaultPack: [KeyboardSkill] = [
        KeyboardSkill(label: "Fix tone", symbol: "wand.and.stars", lane: .write,
                      template: "Rewrite this to be clear, polished, and professional, keeping the meaning and language.",
                      writeAction: "rewrite"),
        KeyboardSkill(label: "Shorten", symbol: "scissors", lane: .write,
                      template: "Make this shorter and punchier without losing the point.",
                      writeAction: "rewrite"),
        KeyboardSkill(label: "Translate → EN", symbol: "character.book.closed", lane: .write,
                      template: "Translate this to natural English.", writeAction: "rewrite"),
        KeyboardSkill(label: "Reply", symbol: "arrowshape.turn.up.left", lane: .write,
                      template: "Keep it short and friendly.", writeAction: "reply"),
        KeyboardSkill(label: "Continue", symbol: "text.append", lane: .write,
                      template: "Continue in the same voice.", writeAction: "continue"),
        KeyboardSkill(label: "Summarize", symbol: "list.bullet.rectangle", lane: .ask,
                      template: "Summarize this concisely:\n\n{text}"),
        KeyboardSkill(label: "Add task", symbol: "checklist", lane: .act,
                      template: "Create a task from this and do it: {text}"),
    ]
}
