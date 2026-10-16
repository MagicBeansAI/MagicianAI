import Foundation

/// Web-parity Tutor canvas mode (mirrors `TutorCanvasMode` in magician `tutor.rs`).
/// `screenOverlay` annotates a visual source; `blackboard` is source-free (darkened
/// canvas, the concept drawn from scratch).
enum TutorCanvasMode: Equatable { case screenOverlay, blackboard }

/// Composer `@tutor` detection + concept extraction — a faithful port of the web
/// `isTutorInvokeText` rule (`ChatPanel.svelte`):
/// `/^\s*(@tut(?:or|ur)|hey[\s,]+tut(?:or|ur))(?=$|[\s:,])/i`.
///
/// Keyboard `@copilot` (App Copilot) is intentionally NOT handled by
/// `isTutorInvoke`: it drives desktop UI mutation. The spoken grammar below
/// still recognizes it so iOS can give an explicit unsupported response instead
/// of accidentally posting the command as an ordinary chat message.
enum TutorInvoke {
    enum VoiceFeature: Equatable { case tutor, appCopilot }

    struct VoiceInvocation: Equatable {
        let feature: VoiceFeature
        let canvasMode: TutorCanvasMode
        let quick: Bool
        let normalizedText: String

        var requiresScreenCapture: Bool { canvasMode == .screenOverlay }
    }

    private struct CommandToken {
        let value: String
        let end: String.Index
    }

    private static let pattern = "^\\s*(@tut(?:or|ur)|hey[\\s,]+tut(?:or|ur))(?=$|[\\s:,])"
    private static let starters: Set<String> = ["hey", "start", "open", "launch", "use"]
    private static let quickTokens: Set<String> = ["quick", "#quick"]

    /// True when the composer text is a `@tutor` / `hey tutor` invoke.
    static func isTutorInvoke(_ text: String) -> Bool {
        text.range(of: pattern, options: [.regularExpression, .caseInsensitive]) != nil
    }

    /// Strip a leading `@tutor` / `hey tutor` token, returning the trimmed concept.
    /// The overlay re-prepends `@tutor ` when it sends, so we hand it the bare concept.
    /// Text with no leading invoke token is returned trimmed and unchanged.
    static func strip(_ text: String) -> String {
        let leading = "^\\s*(@tut(?:or|ur)|hey[\\s,]+tut(?:or|ur))(?=$|[\\s:,])[\\s,:]*"
        let stripped = text.replacingOccurrences(
            of: leading, with: "", options: [.regularExpression, .caseInsensitive])
        return stripped.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Web parity: a staged image ⇒ annotate it (`screenOverlay`); none ⇒ `blackboard`.
    static func mode(hasImage: Bool) -> TutorCanvasMode { hasImage ? .screenOverlay : .blackboard }

    /// Deterministic spoken-command grammar shared with Web and the backend.
    /// Only a leading command can take over; incidental mentions remain normal
    /// chat. iOS callers use the returned capability facts to reject screen and
    /// App Copilot requests without silently sending them as ordinary messages.
    static func parseVoiceGuidedFlow(_ text: String) -> VoiceInvocation? {
        let tokens = commandTokens(text)
        var cursor = 0
        if starters.contains(tokens[safe: cursor]?.value ?? "") { cursor += 1 }

        var quick = false
        if quickTokens.contains(tokens[safe: cursor]?.value ?? "") {
            quick = true
            cursor += 1
        }

        let feature: VoiceFeature
        switch tokens[safe: cursor]?.value {
        case "tutor", "tutur", "@tutor", "@tutur":
            feature = .tutor
            cursor += 1
        case "copilot", "app-copilot", "@copilot", "@appcopilot", "@app-copilot", "@app_copilot":
            feature = .appCopilot
            cursor += 1
        case "app" where tokens[safe: cursor + 1]?.value == "copilot":
            feature = .appCopilot
            cursor += 2
        default:
            return nil
        }

        if quickTokens.contains(tokens[safe: cursor]?.value ?? "") {
            quick = true
            cursor += 1
        }
        let hasCanonicalQuick = tokens.dropFirst(cursor).contains { $0.value == "#quick" }
        quick = quick || hasCanonicalQuick
        // Source selection is command grammar, not topic inference. Only the
        // selector immediately after `Tutor [Quick]` may choose the canvas.
        let explicitBlackboard = tokens[safe: cursor]?.value == "blackboard"
        let screen = feature == .appCopilot
            || (!explicitBlackboard && requestsScreen(Array(tokens.dropFirst(cursor))))
        let canvasMode: TutorCanvasMode = screen ? .screenOverlay : .blackboard

        let commandEnd = tokens[safe: max(0, cursor - 1)]?.end ?? text.startIndex
        let remainder = String(text[commandEnd...])
            .replacingOccurrences(of: "^[\\s,:;-]+", with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        var normalized = feature == .appCopilot ? "@copilot" : "@tutor"
        if quick && !hasCanonicalQuick { normalized += " #quick" }
        if !remainder.isEmpty { normalized += " \(remainder)" }
        return VoiceInvocation(
            feature: feature,
            canvasMode: canvasMode,
            quick: quick,
            normalizedText: normalized
        )
    }

    private static func commandTokens(_ text: String) -> [CommandToken] {
        guard let regex = try? NSRegularExpression(pattern: "[\\p{L}\\p{N}@#_-]+") else {
            return []
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        return regex.matches(in: text, range: range).compactMap { match in
            guard let swiftRange = Range(match.range, in: text) else { return nil }
            return CommandToken(
                value: text[swiftRange].lowercased(),
                end: swiftRange.upperBound
            )
        }
    }

    private static func requestsScreen(_ tokens: [CommandToken]) -> Bool {
        let words = tokens.map(\.value)
        if let first = words.first,
           ["screen", "screenshot", "screencap", "screen-capture", "display"].contains(first) {
            return true
        }
        guard let action = words.first,
              ["take", "capture", "use", "share", "show"].contains(action) else {
            return false
        }
        let sourceIndex = words[safe: 1] == "a" ? 2 : 1
        guard let source = words[safe: sourceIndex] else { return false }
        return ["screenshot", "screen"].contains(source)
    }
}

private extension Array {
    subscript(safe index: Index) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
