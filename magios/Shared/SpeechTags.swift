import Foundation

/// `<speech>`-tag protocol (mirrors ui/unified-ui speechTags.ts + the backend
/// `speech_segments`). The assistant can wrap the parts of a reply that should be
/// read aloud in `<speech …>…</speech>`; everything else is shown but not spoken.
///
/// - `spokenText`: what TTS should read — the concatenated `<speech>` bodies when
///   present, else the whole trimmed body (typed replies with no tags read fully).
/// - `stripped`: the reply with the `<speech>` wrappers removed (inner content
///   kept) so the chat bubble never shows protocol markers.
enum SpeechTags {
    private static let tagBodyPattern = "<speech\\b[^>]*>([\\s\\S]*?)</speech>"
    private static let openTagPattern = "<speech\\b[^>]*>"

    /// The portion to read aloud: `<speech>` bodies joined, or the whole text.
    static func spokenText(_ raw: String) -> String {
        let bodies = tagBodies(raw)
        if bodies.isEmpty {
            return raw.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return bodies.joined(separator: " ")
    }

    /// True when the reply carries at least one `<speech>` block.
    static func hasSpeechTags(_ raw: String) -> Bool { !tagBodies(raw).isEmpty }

    /// Remove `<speech>`/`</speech>` wrappers, keeping the inner content, so the
    /// bubble renders without protocol markers.
    static func stripped(_ raw: String) -> String {
        raw.replacingOccurrences(of: openTagPattern, with: "",
                                 options: [.regularExpression, .caseInsensitive])
           .replacingOccurrences(of: "</speech>", with: "",
                                 options: [.regularExpression, .caseInsensitive])
    }

    // MARK: - Internals

    private static func tagBodies(_ raw: String) -> [String] {
        guard let re = try? NSRegularExpression(pattern: tagBodyPattern, options: [.caseInsensitive]) else { return [] }
        let range = NSRange(raw.startIndex..<raw.endIndex, in: raw)
        return re.matches(in: raw, options: [], range: range).compactMap { match in
            guard match.numberOfRanges >= 2, let r = Range(match.range(at: 1), in: raw) else { return nil }
            let body = String(raw[r])
                .replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return body.isEmpty ? nil : body
        }
    }
}
