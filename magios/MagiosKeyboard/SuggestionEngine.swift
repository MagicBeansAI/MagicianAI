import UIKit

/// Local suggestion/autocorrect engine (no network). Primary source is the
/// in-house `CorrectionEngine` (SymSpell symmetric-delete over a bundled
/// English + Indian-English + Hinglish frequency dictionary, with keyboard-
/// adjacency re-ranking, a confidence gate, and learn-your-words). `UILexicon`
/// (contacts + Text-Replacement shortcuts) is merged in as the highest-priority
/// candidate and is always treated as valid (never autocorrected). `UITextChecker`
/// remains only as a thin fallback for tokens the bundled dictionary can't cover.
///
/// The heavy dictionary/index build runs once on the background `prewarm()` path,
/// never on the keypress thread. Until it's ready, correction is a no-op and
/// suggestions fall back to `UITextChecker` so the first keystrokes still work.
///
/// Public API is unchanged: `suggestions(forWord:lexicon:)`,
/// `autocorrection(for:lexicon:)`, `prewarm()`.
struct SuggestionEngine {
    private let checker = UITextChecker()
    /// The `UITextChecker` language for the user's chosen keyboard language (App
    /// Group; default Indian English), resolved to a dictionary the device
    /// actually has. Used only for the fallback path now.
    private let language: String = SuggestionEngine.resolvedLanguage()
    /// Shared holder for the lazily-built correction core (see `CorrectionStore`).
    private let store = CorrectionStore.shared

    /// Phase-2a next-word predictor (Apple Foundation Models), or `nil` when FM is
    /// unavailable (older OS/device, Apple Intelligence off, or the framework isn't
    /// importable). Built + prewarmed on the background `prewarm()` path so the first
    /// after-space prediction isn't slow; correction is unaffected either way. The
    /// predictor is a class so it survives across `SuggestionEngine` value copies.
    var nextWordPredictor: NextWordPredictor? { PredictorStore.shared.predictor }

    static func resolvedLanguage() -> String {
        let available = Set(UITextChecker.availableLanguages)
        for candidate in KeyboardLanguageStore.current.checkerCandidates where available.contains(candidate) {
            return candidate
        }
        return "en_US"
    }

    /// One-time background warm-up. Builds the `CorrectionEngine` (dictionary
    /// load + symmetric-delete index) off the hot path so the first real
    /// correction isn't slow, and pre-loads the `UITextChecker` language model
    /// used by the fallback path. Cheap to call more than once; wire it once at
    /// load.
    func prewarm() {
        store.warmUp()
        PredictorStore.shared.warmUp()
        let language = self.language
        let checker = self.checker
        DispatchQueue.global(qos: .utility).async {
            let dummy = "teh"
            let full = NSRange(location: 0, length: dummy.utf16.count)
            _ = checker.rangeOfMisspelledWord(
                in: dummy, range: full, startingAt: 0, wrap: false, language: language
            )
            _ = checker.guesses(forWordRange: full, in: dummy, language: language)
            _ = checker.completions(forPartialWordRange: full, in: dummy, language: language)
        }
    }

    /// Up to three suggestions for the word currently being typed. Empty when
    /// there's no in-progress word. Order: UILexicon matches, then the
    /// `CorrectionEngine`'s completions/corrections, then a `UITextChecker`
    /// fallback if the engine isn't ready or returned nothing.
    func suggestions(forWord word: String, lexicon: UILexicon?) -> [String] {
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count >= 1 else { return [] }

        var ranked: [String] = []

        // Lexicon shortcuts/names (e.g. "omw" → "On my way!") come first.
        if let lexicon {
            for entry in lexicon.entries where entry.userInput.caseInsensitiveCompare(trimmed) == .orderedSame {
                ranked.append(entry.documentText)
            }
        }

        // Primary: the correction core (if built).
        if let engine = store.engine {
            ranked += engine.suggestions(for: trimmed, limit: 3)
        }

        // Fallback: only touch UITextChecker if the core gave us too little.
        if ranked.count < 3 {
            ranked += checkerSuggestions(for: trimmed)
        }

        return dedupe(ranked, excluding: trimmed, limit: 3)
    }

    /// A **confidence-gated** auto-replacement for a just-completed word, or nil
    /// to leave it alone. Delegates to `CorrectionEngine.autocorrection` (which
    /// never corrects a valid dictionary or learned word). UILexicon shortcuts
    /// (omw → On my way!) expand and are treated as valid. Preserves the original
    /// word's leading capitalization. No-op until the core is built.
    func autocorrection(for word: String, lexicon: UILexicon?) -> String? {
        let trimmed = word.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count >= 3 else { return nil }

        // Lexicon expansion (and lexicon words are never "corrected").
        if let lexicon {
            for entry in lexicon.entries where entry.userInput.caseInsensitiveCompare(trimmed) == .orderedSame {
                return entry.documentText
            }
        }

        // Primary path: the correction core's confidence gate.
        guard let engine = store.engine else { return nil }   // not ready yet → hands off
        guard let correction = engine.autocorrection(for: trimmed) else { return nil }
        guard correction.lowercased() != trimmed.lowercased() else { return nil }
        return preserveCapitalization(of: trimmed, applyingTo: correction)
    }

    // MARK: - Helpers

    /// `UITextChecker` fallback: completions for an in-progress word or guesses
    /// for a misspelled one. Used only when the bundled dictionary didn't cover
    /// the token.
    private func checkerSuggestions(for trimmed: String) -> [String] {
        let full = NSRange(location: 0, length: trimmed.utf16.count)
        let misspelled = checker.rangeOfMisspelledWord(
            in: trimmed, range: full, startingAt: 0, wrap: false, language: language
        )
        if misspelled.location != NSNotFound {
            return checker.guesses(forWordRange: full, in: trimmed, language: language) ?? []
        } else {
            return checker.completions(forPartialWordRange: full, in: trimmed, language: language) ?? []
        }
    }

    /// Case-insensitive dedupe, dropping the exact word and empties, capped.
    private func dedupe(_ items: [String], excluding word: String, limit: Int) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        let lowerWord = word.lowercased()
        for s in items {
            let key = s.lowercased()
            guard !seen.contains(key), key != lowerWord, !s.isEmpty else { continue }
            seen.insert(key)
            out.append(s)
            if out.count == limit { break }
        }
        return out
    }

    /// Copy the leading capitalization of `original` onto `correction`.
    private func preserveCapitalization(of original: String, applyingTo correction: String) -> String {
        if let first = original.first, first.isUppercase {
            return correction.prefix(1).uppercased() + correction.dropFirst()
        }
        return correction
    }
}

/// Process-wide holder for the lazily-built `CorrectionEngine`. The engine is
/// expensive to build (dictionary load + symmetric-delete index), so it's built
/// exactly once on a background queue via `warmUp()` and then read (lock-free
/// after the initial publish) by every `SuggestionEngine` instance. Reads before
/// the build finishes see `nil` and callers fall back gracefully.
final class CorrectionStore {
    static let shared = CorrectionStore()

    private let lock = NSLock()
    private var built: CorrectionEngine?
    private var building = false
    /// Learned-words store shared with the keyboard's learn-your-words hook.
    let learned = LearnedWordsStore()

    /// Cap the merged dictionary to the top-N English words by frequency. Kept
    /// well UNDER the extension's ~60–70 MB jetsam budget (not at its edge) so the
    /// background SymSpell index build can't tip the process over and make iOS
    /// jetsam the keyboard back to the system one. The top-30k words cover the vast
    /// majority of real typing; all curated Indian/Hinglish + learned words are
    /// force-kept regardless of rank, and the long-word delete cap in `SymSpellCore`
    /// shrinks the index further without changing any correction result.
    private static let dictionaryCap = 30_000

    private init() {}

    /// The built engine, or `nil` if the background build hasn't finished.
    var engine: CorrectionEngine? {
        lock.lock(); defer { lock.unlock() }
        return built
    }

    /// Kick off the one-time background build (idempotent).
    func warmUp() {
        lock.lock()
        if built != nil || building { lock.unlock(); return }
        building = true
        lock.unlock()

        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self else { return }
            let engine = Self.buildEngine(learned: self.learned)
            self.lock.lock()
            self.built = engine
            self.building = false
            self.lock.unlock()
        }
    }

    /// Load the bundled dictionaries and build the correction engine. Tolerates
    /// any of the three resource files being absent (an empty/partial dictionary
    /// just yields fewer corrections — never a crash).
    private static func buildEngine(learned: LearnedWordsStore) -> CorrectionEngine {
        let names = [
            "frequency_dictionary_en_82_765",
            "indian_english_seed",
            "hinglish_seed",
        ]
        let urls = names.compactMap { Bundle.main.url(forResource: $0, withExtension: "txt") }
        let base = CorrectionDictionary.load(fromTabSeparated: urls)
        // Force-keep the curated (non-English-base) + learned words past the cap.
        let curatedNames = ["indian_english_seed", "hinglish_seed"]
        let curatedURLs = curatedNames.compactMap { Bundle.main.url(forResource: $0, withExtension: "txt") }
        let curated = CorrectionDictionary.load(fromTabSeparated: curatedURLs)
        let keep = Set(curated.entries.keys).union(learned.learnedEntries().map { $0.0.lowercased() })
        let capped = base.cappedToTop(dictionaryCap, keeping: keep)
        return CorrectionEngine(dictionary: capped, learned: learned, maxEditDistance: 2)
    }
}

/// Process-wide holder for the next-word predictor. Built once and prewarmed on the
/// background `prewarm()` path — mirroring `CorrectionStore`. The predictor is a
/// `CompositeNextWordPredictor` fallback chain: **Foundation Models first when
/// available**, then the **universal n-gram predictor** (`NGramPredictor`) — so FM wins
/// when it actually generates, and the n-gram fills in whenever FM yields nothing
/// (including a device that reports FM available but isn't ready yet), so *every* device
/// gets next-word prediction. Reads are lock-free after the initial publish; callers get
/// `nil` only until the background build publishes one (then the after-space slot fills
/// on any device). Correction is never affected.
final class PredictorStore {
    static let shared = PredictorStore()

    private let lock = NSLock()
    private var built: NextWordPredictor?
    private var warmed = false

    /// Shared learned-bigrams store, mirrored by the keyboard's learn-bigram hook.
    let learnedBigrams = LearnedBigramsStore()

    private init() {}

    /// The built predictor, or `nil` only if warm-up hasn't published one yet.
    var predictor: NextWordPredictor? {
        lock.lock(); defer { lock.unlock() }
        return built
    }

    /// One-time build + prewarm (idempotent). Selects FM when available, else the
    /// n-gram predictor (which loads the bundled bigram seed off-thread here).
    func warmUp() {
        lock.lock()
        if warmed { lock.unlock(); return }
        warmed = true
        lock.unlock()

        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self else { return }
            // Fallback chain: try Foundation Models first (when it reports available),
            // then the universal n-gram. `CompositeNextWordPredictor` returns the first
            // non-empty result, so a device where FM is *reported* available but can't
            // actually generate yet (Apple-Intelligence assets still downloading, model
            // not ready, a refusal) still gets n-gram predictions instead of nothing.
            var chain: [NextWordPredictor] = []
            if FoundationModelsPredictor.isAvailable {
                let fm = FoundationModelsPredictor()
                fm.prewarm()
                chain.append(fm)
            }
            // Loads the bundled seed off-thread; shares the learned store with the
            // keyboard's learn-bigram hook. Always present as the universal baseline.
            chain.append(NGramPredictor(learned: self.learnedBigrams))
            let predictor: NextWordPredictor =
                chain.count == 1 ? chain[0] : CompositeNextWordPredictor(chain)
            self.lock.lock()
            self.built = predictor
            self.lock.unlock()
        }
    }
}
