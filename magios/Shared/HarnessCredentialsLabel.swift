import Foundation

/// Whose account a harness CLI runs on, from the `credentials` field
/// `GET /plane/engines` and `GET /coding/models` put on each engine row
/// (docs/components/magician/harness-auth.md). Engine pickers append it to the
/// engine's name: "Codex · your login".
enum HarnessCredentialsLabel {
    /// `login` / `magician_key` / `none`; anything else (or absent) has no label.
    static func label(_ credentials: String?) -> String? {
        switch credentials {
        case "login": return "your login"
        case "magician_key": return "Magician’s key"
        case "none": return "not signed in"
        default: return nil
        }
    }

    /// `name` with the label appended; `magician` and unlabelled engines keep
    /// their plain name.
    static func engineName(_ name: String, engine: String?, credentials: String?) -> String {
        guard engine != "magician", let label = label(credentials) else { return name }
        return "\(name) · \(label)"
    }
}
