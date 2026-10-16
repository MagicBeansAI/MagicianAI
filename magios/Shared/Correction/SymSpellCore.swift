import Foundation

/// In-house implementation of the **SymSpell symmetric-delete** spelling
/// algorithm (Wolf Garbe's approach), self-contained — no external package.
///
/// The idea: for every dictionary word we precompute the set of strings reachable
/// by deleting up to `maxEditDistance` characters (its "delete neighborhood"),
/// and index each deletion → the original words that produced it. At lookup time
/// we generate the same deletes for the *input* word; any dictionary word that
/// shares a deletion is a candidate within `2 * maxEditDistance` deletes, which
/// we then verify with the true Damerau-Levenshtein distance. This makes
/// correction candidate generation orders of magnitude faster than scanning the
/// whole dictionary, at the cost of a one-time index build (done off-thread on
/// the pre-warm path — never on the keypress thread).
///
/// Candidates are returned ranked by (edit distance ascending, frequency
/// descending). This is `Verbosity.closest`-style behavior; the caller layers
/// keyboard-adjacency re-ranking and the confidence gate on top.
struct SymSpellCore {
    struct Suggestion: Equatable {
        let term: String
        let distance: Int
        let frequency: Int
    }

    /// The maximum edit distance the index was built for.
    let maxEditDistance: Int

    /// deletion string → the set of dictionary word indices that generate it.
    private let deletes: [String: [Int]]
    /// The dictionary words (lowercased), parallel to `frequencies`.
    private let words: [String]
    private let frequencies: [Int]
    /// word → its index in `words` (for the prefix-completion path & membership).
    private let wordIndex: [String: Int]
    /// Longest word length in the dictionary (bounds the delete generation).
    private let maxDictWordLength: Int

    /// Tokens longer than this cap their edit distance to 1 (long words rarely
    /// have two independent typos and the delete neighborhood explodes).
    static let longTokenThreshold = 8

    /// Build the symmetric-delete index from a `CorrectionDictionary`.
    /// This is the heavy step — callers MUST run it off the main/keypress thread.
    init(dictionary: CorrectionDictionary, maxEditDistance: Int = 2) {
        let editDistance = max(0, maxEditDistance)
        self.maxEditDistance = editDistance

        var words: [String] = []
        var frequencies: [Int] = []
        var wordIndex: [String: Int] = [:]
        words.reserveCapacity(dictionary.entries.count)
        frequencies.reserveCapacity(dictionary.entries.count)
        wordIndex.reserveCapacity(dictionary.entries.count)

        var deletes: [String: [Int]] = [:]
        var maxLen = 0

        for (word, freq) in dictionary.entries {
            let idx = words.count
            words.append(word)
            frequencies.append(freq)
            wordIndex[word] = idx
            maxLen = max(maxLen, word.count)

            // Cap the delete neighborhood for long words. A word longer than
            // `longTokenThreshold + editDistance` can never be a distance-`editDistance`
            // match for any input: `lookup` caps tokens longer than `longTokenThreshold`
            // to distance 1, and the length pre-filter rejects a length gap above the
            // edit budget. So a long word's full (O(L²)) delete set is pure memory waste
            // — generate only distance-1 deletes for it. This is provably lossless (no
            // query result changes) and it's the long words that dominate the index size.
            let perWordDistance = word.count > Self.longTokenThreshold + editDistance
                ? min(editDistance, 1) : editDistance
            // The word itself is always a "delete" of itself (distance 0).
            let edits = Self.editsPrefix(word, maxEditDistance: perWordDistance)
            for d in edits {
                deletes[d, default: []].append(idx)
            }
            // Ensure the exact word maps to itself even if edits() excluded it
            // for very short words.
            if deletes[word]?.contains(idx) != true {
                deletes[word, default: []].append(idx)
            }
        }

        self.words = words
        self.frequencies = frequencies
        self.wordIndex = wordIndex
        self.deletes = deletes
        self.maxDictWordLength = maxLen
    }

    /// True when the exact (lowercased) word is in the indexed dictionary.
    func contains(_ word: String) -> Bool {
        wordIndex[word.lowercased()] != nil
    }

    /// The stored frequency for a word, or 0.
    func frequency(of word: String) -> Int {
        guard let idx = wordIndex[word.lowercased()] else { return 0 }
        return frequencies[idx]
    }

    /// Correction candidates for `word`, ranked by (distance asc, frequency
    /// desc). `maxEditDistance` may be lowered per-call but never above the
    /// index's build distance. Long tokens are capped to distance 1.
    func lookup(_ word: String, maxEditDistance: Int? = nil) -> [Suggestion] {
        let input = word.lowercased()
        guard !input.isEmpty else { return [] }

        var maxDistance = min(maxEditDistance ?? self.maxEditDistance, self.maxEditDistance)
        if input.count > Self.longTokenThreshold { maxDistance = min(maxDistance, 1) }
        maxDistance = max(0, maxDistance)

        // Exact hit shortcuts to distance 0 but we still gather near neighbors so
        // the caller can compare (needed for the confidence gate / completions).
        var candidateIndices = Set<Int>()

        // 1. Generate deletes of the INPUT and match against the delete index.
        let inputDeletes = Self.editsPrefix(input, maxEditDistance: maxDistance)
        for d in inputDeletes {
            if let hits = deletes[d] {
                for idx in hits { candidateIndices.insert(idx) }
            }
        }
        // 2. The input itself may be a delete of a longer dictionary word.
        if let hits = deletes[input] {
            for idx in hits { candidateIndices.insert(idx) }
        }

        var suggestions: [Suggestion] = []
        suggestions.reserveCapacity(candidateIndices.count)
        for idx in candidateIndices {
            let term = words[idx]
            // Length pre-filter: true distance is at least the length delta.
            if abs(term.count - input.count) > maxDistance { continue }
            let distance = Self.damerauLevenshtein(input, term, maxDistance: maxDistance)
            guard distance <= maxDistance else { continue }
            suggestions.append(Suggestion(term: term, distance: distance, frequency: frequencies[idx]))
        }

        suggestions.sort { a, b in
            a.distance != b.distance ? a.distance < b.distance : a.frequency > b.frequency
        }
        return suggestions
    }

    /// Prefix-completion candidates: dictionary words that start with `prefix`,
    /// ranked by frequency descending, up to `limit`. Used for the suggestion
    /// strip while a word is in progress. Linear over the vocabulary — cheap for
    /// the top-N-capped dictionary and only runs on the background suggestion
    /// queue, not the keypress thread.
    func completions(startingWith prefix: String, limit: Int = 10) -> [Suggestion] {
        let p = prefix.lowercased()
        guard !p.isEmpty else { return [] }
        var out: [Suggestion] = []
        for idx in words.indices {
            let term = words[idx]
            if term.count > p.count, term.hasPrefix(p) {
                out.append(Suggestion(term: term, distance: 0, frequency: frequencies[idx]))
            }
        }
        out.sort { $0.frequency > $1.frequency }
        if out.count > limit { out = Array(out.prefix(limit)) }
        return out
    }

    // MARK: - Delete-edit generation

    /// The recursive delete neighborhood of `word` up to `maxEditDistance`
    /// deletions (not including the original word unless it's short). This is the
    /// SymSpell "prefix" precompute — for a dictionary word we index every string
    /// reachable by deleting characters.
    static func editsPrefix(_ word: String, maxEditDistance: Int) -> Set<String> {
        var result = Set<String>()
        guard maxEditDistance > 0, !word.isEmpty else { return result }
        edits(Array(word), editDistanceRemaining: maxEditDistance, into: &result)
        return result
    }

    private static func edits(_ chars: [Character], editDistanceRemaining: Int, into result: inout Set<String>) {
        // Stop when we're out of edit budget or the string is down to one char
        // (deleting the last char yields "" — intentionally never indexed, since
        // an empty deletion would match every word).
        guard editDistanceRemaining > 0, chars.count > 1 else { return }
        for i in chars.indices {
            var deleted = chars
            deleted.remove(at: i)
            let s = String(deleted)
            let isNew = result.insert(s).inserted
            // Recurse for deeper deletions. Re-recursing an already-seen deletion
            // is redundant (its children were generated the first time), so only
            // descend on a newly-inserted string.
            if isNew {
                edits(deleted, editDistanceRemaining: editDistanceRemaining - 1, into: &result)
            }
        }
    }

    // MARK: - Damerau-Levenshtein (optimal string alignment) verification

    /// True Damerau-Levenshtein distance (optimal string alignment variant:
    /// counts adjacent transpositions as a single edit). Early-exits once the
    /// running minimum for a row exceeds `maxDistance`.
    static func damerauLevenshtein(_ s1: String, _ s2: String, maxDistance: Int = Int.max) -> Int {
        let a = Array(s1)
        let b = Array(s2)
        let n = a.count
        let m = b.count
        if n == 0 { return m }
        if m == 0 { return n }
        if abs(n - m) > maxDistance { return maxDistance + 1 }

        // Three rolling rows for the OSA transposition rule.
        var prevPrev = [Int](repeating: 0, count: m + 1)
        var prev = [Int](repeating: 0, count: m + 1)
        var curr = [Int](repeating: 0, count: m + 1)
        for j in 0...m { prev[j] = j }

        for i in 1...n {
            curr[0] = i
            var rowMin = curr[0]
            for j in 1...m {
                let cost = a[i - 1] == b[j - 1] ? 0 : 1
                var value = min(
                    prev[j] + 1,        // deletion
                    curr[j - 1] + 1,    // insertion
                    prev[j - 1] + cost  // substitution
                )
                // Transposition of two adjacent characters.
                if i > 1, j > 1, a[i - 1] == b[j - 2], a[i - 2] == b[j - 1] {
                    value = min(value, prevPrev[j - 2] + 1)
                }
                curr[j] = value
                rowMin = min(rowMin, value)
            }
            if rowMin > maxDistance { return maxDistance + 1 }
            // Rotate the rows.
            let temp = prevPrev
            prevPrev = prev
            prev = curr
            curr = temp
        }
        return prev[m]
    }
}
