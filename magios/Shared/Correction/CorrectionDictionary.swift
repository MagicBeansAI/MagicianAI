import Foundation

/// The merged **word → frequency** vocabulary that backs the correction engine.
///
/// A single case-insensitive table assembled from three sources at load time
/// (English base + curated Indian-English + curated Hinglish). Words are stored
/// lowercased; on a duplicate the **higher** frequency wins (so a curated seed
/// can raise, never lower, an English-base weight). The engine treats every key
/// here as a *valid word* — so anything in this table is never autocorrected.
///
/// Pure value type with no UIKit / no I/O in the hot path — lives in `Shared/`
/// so both the keyboard extension and the test target compile it.
struct CorrectionDictionary {
    /// lowercased word → frequency (occurrence count / weight).
    private(set) var entries: [String: Int]

    /// The number of distinct words. Used by the memory-budget cap.
    var count: Int { entries.count }

    /// Build from in-memory `(word, frequency)` pairs. Words are lowercased and
    /// the **max** frequency is kept on a duplicate.
    init(entries pairs: [(String, Int)]) {
        var table: [String: Int] = [:]
        table.reserveCapacity(pairs.count)
        for (word, freq) in pairs {
            Self.insert(word, freq, into: &table)
        }
        self.entries = table
    }

    /// Build directly from a prepared table (already lowercased).
    init(table: [String: Int]) {
        self.entries = table
    }

    /// Insert one `(word, freq)` pair with lowercasing + max-on-duplicate.
    /// Empty / non-positive-frequency entries are dropped.
    private static func insert(_ word: String, _ freq: Int, into table: inout [String: Int]) {
        let key = word.lowercased().trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty, freq > 0 else { return }
        if let existing = table[key] {
            if freq > existing { table[key] = freq }
        } else {
            table[key] = freq
        }
    }

    /// True when the word (case-insensitive) is a known valid word.
    func contains(_ word: String) -> Bool {
        entries[word.lowercased()] != nil
    }

    /// The stored frequency for a word, or `0` when unknown.
    func frequency(of word: String) -> Int {
        entries[word.lowercased()] ?? 0
    }

    /// Return a copy with extra `(word, frequency)` pairs merged in
    /// (max-on-duplicate). Used to fold learned words into the effective
    /// dictionary without mutating the shared base.
    func merging(_ pairs: [(String, Int)]) -> CorrectionDictionary {
        var table = entries
        for (word, freq) in pairs {
            Self.insert(word, freq, into: &table)
        }
        return CorrectionDictionary(table: table)
    }

    /// Keep only the top-`limit` words by frequency (drops the low-frequency
    /// tail to fit the extension's memory budget). `keep` lets callers force
    /// entries to survive the cap regardless of rank (e.g. every curated
    /// Indian/Hinglish word). No-op when already within `limit`.
    func cappedToTop(_ limit: Int, keeping keep: Set<String> = []) -> CorrectionDictionary {
        guard limit > 0, entries.count > limit else { return self }
        // Sort by frequency desc, break ties by word for determinism.
        let ranked = entries.sorted { lhs, rhs in
            lhs.value != rhs.value ? lhs.value > rhs.value : lhs.key < rhs.key
        }
        var table: [String: Int] = [:]
        table.reserveCapacity(limit)
        for (word, freq) in ranked.prefix(limit) {
            table[word] = freq
        }
        // Re-add any forced-keep words that fell outside the cap.
        for word in keep {
            let key = word.lowercased()
            if table[key] == nil, let freq = entries[key] {
                table[key] = freq
            }
        }
        return CorrectionDictionary(table: table)
    }

    /// Parse `word<TAB>count` files (SymSpell's format) and merge them into one
    /// dictionary. Tolerant: skips blank lines, comment lines (`#`/`//`), lines
    /// without a numeric count, and unreadable URLs. Whitespace-separated is
    /// accepted too (SymSpell's English list is space-delimited). Later files
    /// can raise a word's frequency (max-on-duplicate) but never lower it.
    static func load(fromTabSeparated urls: [URL]) -> CorrectionDictionary {
        var table: [String: Int] = [:]
        for url in urls {
            guard let text = try? String(contentsOf: url, encoding: .utf8) else { continue }
            text.enumerateLines { line, _ in
                parse(line: line, into: &table)
            }
        }
        return CorrectionDictionary(table: table)
    }

    /// Parse a single `word<sep>count` line into `table`. Exposed
    /// (internal) for direct unit testing of the tolerant parser.
    static func parse(line rawLine: String, into table: inout [String: Int]) {
        let line = rawLine.trimmingCharacters(in: .whitespaces)
        guard !line.isEmpty, !line.hasPrefix("#"), !line.hasPrefix("//") else { return }
        // Split on tab first (SymSpell dialect), fall back to any whitespace.
        let fields: [Substring]
        if line.contains("\t") {
            fields = line.split(separator: "\t", omittingEmptySubsequences: true)
        } else {
            fields = line.split(whereSeparator: { $0 == " " || $0 == "\t" })
        }
        guard fields.count >= 2 else { return }
        let word = fields[0].trimmingCharacters(in: .whitespaces)
        guard let freq = Int(fields[1].trimmingCharacters(in: .whitespaces)), freq > 0 else { return }
        insert(word, freq, into: &table)
    }
}
