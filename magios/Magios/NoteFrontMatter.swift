import Foundation

/// A note's Markdown as the reader sees it: without the YAML front matter
/// Magician writes at the top of its notes (`magician_kind`, `created_at`, …).
///
/// Only the rendered view uses this; editing and saving keep the whole file,
/// front matter included.
///
/// The block is recognised when, after an optional BOM and leading blank
/// space, the first line is exactly `---` and a later line is exactly `---` or
/// `...` (trailing spaces allowed; LF or CRLF). Its first non-blank line must
/// be a `key:` mapping, so a note that merely opens with a `---` rule and a
/// paragraph keeps both. Anything else — no front matter, or a block that is
/// never closed — comes back unchanged. Same rules as Android's
/// `NoteFrontMatter.kt` and the web reader.
enum NoteFrontMatter {
    static func body(_ markdown: String) -> String {
        // Unicode scalars, not Characters: Swift folds "\r\n" into one
        // Character, which would hide the line breaks this walks.
        let scalars = markdown.unicodeScalars
        var at = scalars.startIndex
        if at < scalars.endIndex, scalars[at] == "\u{FEFF}" { at = scalars.index(after: at) }
        while at < scalars.endIndex, CharacterSet.whitespacesAndNewlines.contains(scalars[at]) {
            at = scalars.index(after: at)
        }

        var first = true
        var sawContent = false
        while at < scalars.endIndex {
            let newline = scalars[at...].firstIndex(of: "\n")
            let end = newline ?? scalars.endIndex
            var lineScalars = String.UnicodeScalarView(scalars[at..<end])
            if lineScalars.last == "\r" { lineScalars.removeLast() }
            let line = String(lineScalars)
            let next = newline.map { scalars.index(after: $0) } ?? scalars.endIndex
            if first {
                guard newline != nil, line.trimmingCharacters(in: .whitespaces) == "---" else { return markdown }
                first = false
            } else {
                let fence = trimmingTrailingBlanks(line)
                if fence == "---" || fence == "..." {
                    var rest = next
                    while rest < scalars.endIndex, scalars[rest] == "\r" || scalars[rest] == "\n" {
                        rest = scalars.index(after: rest)
                    }
                    return String(String.UnicodeScalarView(scalars[rest...]))
                }
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if !sawContent, !trimmed.isEmpty, !trimmed.hasPrefix("#") {
                    guard isMappingKey(line) else { return markdown }
                    sawContent = true
                }
            }
            at = next
        }
        return markdown
    }

    private static func trimmingTrailingBlanks(_ line: String) -> String {
        var scalars = line.unicodeScalars
        while let last = scalars.last, last == " " || last == "\t" { scalars.removeLast() }
        return String(scalars)
    }

    /// A top-level mapping key: `name:`, `"quoted name":` or `'quoted':`.
    private static let mappingKey = try? NSRegularExpression(
        pattern: #"^(?:[A-Za-z0-9_][A-Za-z0-9_ .\-]*|"[^"]*"|'[^']*')[ \t]*:(?:[ \t]|$)"#
    )

    private static func isMappingKey(_ line: String) -> Bool {
        guard let mappingKey else { return false }
        let range = NSRange(line.startIndex..., in: line)
        return mappingKey.firstMatch(in: line, range: range) != nil
    }
}
