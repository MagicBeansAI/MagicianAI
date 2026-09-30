import Foundation

/// The correction core: merges the base dictionary with learned words, builds the
/// symmetric-delete index, and exposes three operations behind a small API:
///
/// - `suggestions(for:)`  — completions/corrections for the suggestion strip.
/// - `bestCorrection(for:)` — the single best candidate after keyboard-adjacency
///   re-ranking (used when the caller has already decided to correct).
/// - `autocorrection(for:)` — the confidence-gated auto-replace: `nil` unless we
///   are confident the token is a typo of a clearly-better word.
///
/// Building the index is the heavy step (delete neighborhood over the whole
/// vocabulary) — construct this off the main / keypress thread on the pre-warm
/// path. Once built, every query is index-lookup + a handful of
/// Damerau-Levenshtein verifications, so it's cheap enough for per-keystroke use.
struct CorrectionEngine {
    private let dictionary: CorrectionDictionary
    private let core: SymSpellCore
    private let adjacency: KeyboardAdjacency
    private let learned: LearnedWordsStore?
    let maxEditDistance: Int

    /// The frequency multiplier applied to a learned word when folding it into
    /// the effective dictionary — enough to promote it above lookalikes without
    /// swamping genuine high-frequency English words.
    private static let learnedBoost = 1000

    // Confidence-gate + re-rank tuning.
    private static let frequencyWeight = 1.0
    private static let adjacencyWeight = 2.0
    /// A candidate must clear this frequency to be auto-applied (keeps the gate
    /// from "correcting" a common typo into a rare dictionary word).
    private static let minAutocorrectFrequency = 1

    init(dictionary: CorrectionDictionary,
         learned: LearnedWordsStore? = nil,
         maxEditDistance: Int = 2,
         adjacency: KeyboardAdjacency = .qwerty) {
        self.learned = learned
        self.maxEditDistance = max(1, maxEditDistance)
        self.adjacency = adjacency

        // Fold learned words in as valid + boosted so they're never corrected
        // and start being suggested.
        let learnedPairs: [(String, Int)] = (learned?.learnedEntries() ?? []).map { word, count in
            (word, count * Self.learnedBoost)
        }
        let effective = learnedPairs.isEmpty ? dictionary : dictionary.merging(learnedPairs)
        self.dictionary = effective
        self.core = SymSpellCore(dictionary: effective, maxEditDistance: self.maxEditDistance)
    }

    /// True when the token is a known-valid word (dictionary or learned).
    /// Valid words are never autocorrected.
    func isValid(_ word: String) -> Bool {
        if dictionary.contains(word) { return true }
        if let learned, learned.isLearned(word) { return true }
        return false
    }

    /// Up to `limit` suggestions for the strip. If the token is an in-progress
    /// prefix we surface frequency-ranked completions; we always also fold in the
    /// closest corrections so a misspelling gets a fix offered. The exact typed
    /// word is dropped from the list.
    func suggestions(for word: String, limit: Int = 3) -> [String] {
        let input = word.lowercased()
        guard !input.isEmpty else { return [] }

        var ordered: [String] = []
        var seen = Set<String>()
        func add(_ term: String) {
            let key = term.lowercased()
            guard key != input, !seen.contains(key) else { return }
            seen.insert(key)
            ordered.append(term)
        }

        // 1. Completions (word-in-progress) — highest value for the strip.
        for s in core.completions(startingWith: input, limit: limit * 3) {
            add(s.term)
        }
        // 2. Corrections (typo fixes), adjacency-re-ranked, to fill remaining slots.
        for s in rankedCandidates(for: input) {
            add(s.term)
        }

        return Array(ordered.prefix(limit))
    }

    /// The single best correction for a token, after keyboard-adjacency
    /// re-ranking of same-distance candidates. Returns `nil` only when there is
    /// no candidate within the edit-distance budget. Does NOT apply the
    /// confidence gate — that's `autocorrection`'s job.
    func bestCorrection(for word: String) -> String? {
        rankedCandidates(for: word.lowercased()).first?.term
    }

    /// The confidence gate. Returns a replacement ONLY when we're confident the
    /// token is a typo of a clearly better word:
    /// - `nil` if the token is valid (in the dictionary or a learned word).
    /// - otherwise the best correction, but only if:
    ///   * its edit distance ≤ `maxEditDistance` (≤ 1 for short tokens ≤ 4 chars,
    ///     where a 2-edit "fix" is usually a different word), AND
    ///   * its frequency clears the floor, AND
    ///   * it clearly beats the runner-up (or there is no same-distance rival).
    func autocorrection(for word: String) -> String? {
        let input = word.lowercased()
        guard input.count >= 2 else { return nil }
        // Never correct a valid / learned word.
        guard !isValid(input) else { return nil }

        let candidates = rankedCandidates(for: input)
        guard let best = candidates.first else { return nil }

        // Distance gate (tighter for short tokens).
        let distanceCap = input.count <= 4 ? 1 : maxEditDistance
        guard best.suggestion.distance <= distanceCap else { return nil }
        // Frequency floor.
        guard best.suggestion.frequency >= Self.minAutocorrectFrequency else { return nil }

        // Confidence: the best must clearly beat any OTHER candidate at the same
        // edit distance. If a same-distance rival scores nearly as well, we're not
        // confident enough to auto-apply (leave it to the suggestion strip).
        if let rival = candidates.dropFirst().first(where: { $0.suggestion.distance == best.suggestion.distance }) {
            // "Clearly beats" = at least a small combined-score margin.
            guard best.score - rival.score >= 0.20 else { return nil }
        }

        return best.suggestion.term
    }

    // MARK: - Ranking

    private struct Scored {
        let suggestion: SymSpellCore.Suggestion
        let score: Double
        var term: String { suggestion.term }
    }

    /// Candidates from the symmetric-delete index, re-ranked within each
    /// edit-distance band by a combined score of `log(frequency)` and keyboard
    /// adjacency. Distance is the primary key (a distance-1 fix always beats a
    /// distance-2 one); adjacency + frequency break ties within a band.
    private func rankedCandidates(for word: String) -> [Scored] {
        let input = word.lowercased()
        let raw = core.lookup(input, maxEditDistance: maxEditDistance)
        guard !raw.isEmpty else { return [] }

        let scored = raw.map { s -> Scored in
            let freqScore = Self.frequencyWeight * log(Double(max(s.frequency, 1)))
            let adjScore = Self.adjacencyWeight * adjacency.adjacencyScore(typed: input, candidate: s.term)
            return Scored(suggestion: s, score: freqScore + adjScore)
        }

        // Primary: edit distance ascending. Secondary: combined score descending.
        return scored.sorted { a, b in
            if a.suggestion.distance != b.suggestion.distance {
                return a.suggestion.distance < b.suggestion.distance
            }
            return a.score > b.score
        }
    }
}
