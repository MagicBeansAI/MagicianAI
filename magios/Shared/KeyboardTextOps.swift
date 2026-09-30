import Foundation

/// Pure text operations for the keyboard — no UIKit, so they're unit-testable
/// without a document proxy.
enum KeyboardTextOps {
    /// How many trailing characters a word-delete removes from `before` (the text
    /// left of the caret): the trailing whitespace run plus the word run before it,
    /// and always ≥1 whenever there's any text. This is the escalation used when the
    /// delete key is held past a few characters (standard iOS behaviour).
    static func trailingWordDeleteCount(before: String) -> Int {
        guard !before.isEmpty else { return 0 }
        let ws = CharacterSet.whitespacesAndNewlines
        func isWhitespace(_ c: Character) -> Bool { c.unicodeScalars.allSatisfy { ws.contains($0) } }
        var count = 0
        var idx = before.endIndex
        // trailing whitespace
        while idx > before.startIndex {
            let prev = before.index(before: idx)
            guard isWhitespace(before[prev]) else { break }
            count += 1; idx = prev
        }
        // then the word run
        while idx > before.startIndex {
            let prev = before.index(before: idx)
            guard !isWhitespace(before[prev]) else { break }
            count += 1; idx = prev
        }
        return max(count, 1)
    }
}
