import Foundation

/// Universal (all-devices) next-word predictor backed by a compact curated English
/// **bigram seed** plus the user's own **learned bigrams** (App Group). Used as the
/// fallback when Apple Foundation Models isn't available (older OS/device, Apple
/// Intelligence off, or the framework isn't importable), so *every* device gets
/// next-word prediction in the after-space slot.
///
/// Ranking (per `predict(context:)`):
///   - Take the last one/two word tokens of the context.
///   - Baseline is a **bigram** match on the last word: merge the seed's ranked next
///     words with a boost for the user's learned `(prev → next)` pairs.
///   - When two words of history are present, an optional **trigram-ish refinement**
///     re-ranks by also boosting next words the user has learned after the *second*
///     word — a light lift toward context, kept simple (we have no bundled trigram
///     corpus; learned pairs key on the single previous word).
///   - Output is cleaned via `PredictionCleanup` (top-3, trimmed, deduped, non-empty)
///     and the just-typed previous word is never suggested back.
///
/// The seed is loaded off-thread (mirroring the correction-dictionary load); a missing
/// bundle resource is tolerated (an empty seed just yields learned-only predictions).
/// Never throws, never crashes — returns `[]` when there's no data.
public final class NGramPredictor: NextWordPredictor, @unchecked Sendable {

    /// `prev -> [(next, weight)]` from the bundled seed, ranked desc. Immutable after
    /// construction; safe to read concurrently.
    private let seed: [String: [(word: String, weight: Int)]]
    /// The user's learned bigrams (App Group in prod; in-memory in tests).
    private let learned: LearnedBigramsStore
    /// Weight multiplier applied to a learned bigram's count so a repeatedly-typed
    /// user pair outranks a generic seed pair once it's been seen a few times.
    private let learnedBoost: Int

    /// Test / advanced initializer: inject the seed table + a learned store directly.
    /// `seedEntries` are `(prev, next, count)` triples (lowercased by the predictor).
    public init(seedEntries: [(prev: String, next: String, count: Int)],
                learned: LearnedBigramsStore,
                learnedBoost: Int = 4) {
        self.seed = Self.buildSeed(from: seedEntries)
        self.learned = learned
        self.learnedBoost = max(1, learnedBoost)
    }

    /// Production initializer: load the bundled `english_bigrams_seed.txt` off-thread
    /// and use the shared (App Group) learned store. A missing resource is tolerated.
    public convenience init(learned: LearnedBigramsStore = LearnedBigramsStore(),
                            learnedBoost: Int = 4) {
        let entries = Self.loadBundledSeed()
        self.init(seedEntries: entries, learned: learned, learnedBoost: learnedBoost)
    }

    // MARK: - NextWordPredictor

    public func predict(context: String) async -> [String] {
        let tokens = Self.tailTokens(context, max: 2)
        guard let last = tokens.last, !last.isEmpty else { return [] }
        let prevPrev: String? = tokens.count >= 2 ? tokens[tokens.count - 2] : nil

        // Accumulate a weight per candidate next-word (case-insensitive key).
        var weights: [String: Int] = [:]
        // Preserve first-seen display casing (seed + learned are lowercase, so this is
        // just lowercase — kept for parity with the rest of the pipeline).
        var display: [String: String] = [:]

        func add(_ word: String, _ weight: Int) {
            let key = word.lowercased()
            guard !key.isEmpty, weight > 0 else { return }
            weights[key, default: 0] += weight
            if display[key] == nil { display[key] = word }
        }

        // Seed bigram: `last -> next`.
        for entry in seed[last] ?? [] { add(entry.word, entry.weight) }
        // Learned bigram: `last -> next`, boosted by the user's own counts.
        for (word, count) in learned.nextWords(after: last) { add(word, count * learnedBoost) }
        // Optional trigram-ish refinement: also lift next words the user has learned
        // after the *earlier* word (helps context without a bundled trigram corpus).
        if let prevPrev {
            for (word, count) in learned.nextWords(after: prevPrev) { add(word, count) }
        }

        guard !weights.isEmpty else { return [] }

        // Never suggest the just-typed word straight back.
        let banned = Set([last.lowercased(), (prevPrev ?? "").lowercased()])

        let ranked = weights
            .filter { !banned.contains($0.key) }
            .sorted { lhs, rhs in
                lhs.value != rhs.value ? lhs.value > rhs.value : lhs.key < rhs.key
            }
            .compactMap { display[$0.key] }

        return PredictionCleanup.clean(ranked, limit: 3)
    }

    // MARK: - Seed loading

    /// Build the `prev -> ranked [(next, weight)]` table from `(prev, next, count)`
    /// triples. Lowercased; on a duplicate `(prev, next)` the higher count wins.
    static func buildSeed(from entries: [(prev: String, next: String, count: Int)])
        -> [String: [(word: String, weight: Int)]] {
        var table: [String: [String: Int]] = [:]
        for e in entries {
            let prev = e.prev.lowercased().trimmingCharacters(in: .whitespacesAndNewlines)
            let next = e.next.lowercased().trimmingCharacters(in: .whitespacesAndNewlines)
            guard !prev.isEmpty, !next.isEmpty, e.count > 0 else { continue }
            var row = table[prev] ?? [:]
            if let existing = row[next] { if e.count > existing { row[next] = e.count } }
            else { row[next] = e.count }
            table[prev] = row
        }
        // Pre-rank each row (desc by weight, ties alphabetical) so `predict` is cheap.
        var ranked: [String: [(word: String, weight: Int)]] = [:]
        ranked.reserveCapacity(table.count)
        for (prev, row) in table {
            ranked[prev] = row
                .sorted { $0.value != $1.value ? $0.value > $1.value : $0.key < $1.key }
                .map { (word: $0.key, weight: $0.value) }
        }
        return ranked
    }

    /// Parse the bundled `english_bigrams_seed.txt` (`prev<TAB>next<TAB>count`) into
    /// `(prev, next, count)` triples. Tolerant: skips blanks, comment lines
    /// (`#`/`//`), and malformed rows. Returns `[]` if the resource is missing.
    static func loadBundledSeed() -> [(prev: String, next: String, count: Int)] {
        guard let url = Bundle.main.url(forResource: "english_bigrams_seed", withExtension: "txt"),
              let text = try? String(contentsOf: url, encoding: .utf8) else {
            return []
        }
        var out: [(prev: String, next: String, count: Int)] = []
        text.enumerateLines { rawLine, _ in
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty, !line.hasPrefix("#"), !line.hasPrefix("//") else { return }
            let fields: [Substring]
            if line.contains("\t") {
                fields = line.split(separator: "\t", omittingEmptySubsequences: true)
            } else {
                fields = line.split(whereSeparator: { $0 == " " || $0 == "\t" })
            }
            guard fields.count >= 3 else { return }
            let prev = fields[0].trimmingCharacters(in: .whitespaces)
            let next = fields[1].trimmingCharacters(in: .whitespaces)
            guard let count = Int(fields[2].trimmingCharacters(in: .whitespaces)), count > 0,
                  !prev.isEmpty, !next.isEmpty else { return }
            out.append((prev: prev, next: next, count: count))
        }
        return out
    }

    /// The last `max` word tokens of `context` (letters + intra-word apostrophes),
    /// lowercased. Punctuation and other separators split tokens. Empty tail → `[]`.
    static func tailTokens(_ context: String, max: Int) -> [String] {
        let allowed = CharacterSet.letters.union(CharacterSet(charactersIn: "'’"))
        var tokens: [String] = []
        var current: [Character] = []
        for ch in context {
            if ch.unicodeScalars.allSatisfy({ allowed.contains($0) }) {
                current.append(ch)
            } else if !current.isEmpty {
                tokens.append(String(current).lowercased())
                current.removeAll(keepingCapacity: true)
            }
        }
        if !current.isEmpty { tokens.append(String(current).lowercased()) }
        guard tokens.count > max else { return tokens }
        return Array(tokens.suffix(max))
    }
}

/// Pure, testable predictor **selection** (the Phase-2b fallback chain): choose the
/// Foundation-Models predictor when it's available on this device, else the universal
/// n-gram predictor. Kept framework-free and injectable so the decision is unit-tested
/// with stubs (in the simulator FM is unavailable → n-gram is chosen, exercising the
/// fallback). The real `PredictorStore` calls this with the live FM availability flag.
public enum PredictorSelector {
    /// - Parameters:
    ///   - isFMAvailable: `FoundationModelsPredictor.isAvailable` in prod.
    ///   - makeFM: builds the FM predictor (only invoked when available).
    ///   - makeNGram: builds the n-gram predictor (the always-available fallback).
    public static func select(isFMAvailable: Bool,
                              makeFM: () -> NextWordPredictor,
                              makeNGram: () -> NextWordPredictor) -> NextWordPredictor {
        isFMAvailable ? makeFM() : makeNGram()
    }
}
