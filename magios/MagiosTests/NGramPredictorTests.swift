import XCTest
@testable import Magician

/// Phase-2b universal (all-devices) n-gram prediction unit tests.
///
/// These cover the app-host-testable seams in `Shared/Prediction/**`:
///   - `LearnedBigramsStore` record/rank (in-memory, no App Group touched),
///   - `NGramPredictor` ranking (seed baseline, learned boost, trigram-ish
///     refinement, top-3 / dedupe / empty-on-no-data / never-suggest-self),
///   - `PredictorSelector` fallback chain (FM-available → FM, else n-gram) via stubs.
///
/// The bundled seed file lives in the keyboard extension's bundle (not the test host),
/// so `NGramPredictor` is exercised with an **injected** seed + in-memory learned store.
final class NGramPredictorTests: XCTestCase {

    // MARK: - LearnedBigramsStore

    func testLearnedBigramRecordsAndRanksByCount() {
        let store = LearnedBigramsStore(inMemory: true)
        store.record(prev: "let", next: "me")
        store.record(prev: "let", next: "me")
        store.record(prev: "let", next: "us")
        let ranked = store.nextWords(after: "let")
        XCTAssertEqual(ranked.map { $0.0 }, ["me", "us"])   // "me" (2) outranks "us" (1)
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: ranked)["me"], 2)
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: ranked)["us"], 1)
    }

    func testLearnedBigramIsCaseInsensitiveAndPunctuationTolerant() {
        let store = LearnedBigramsStore(inMemory: true)
        store.record(prev: "Thank", next: "You!")
        store.record(prev: "thank", next: "you")
        let ranked = store.nextWords(after: "THANK")
        XCTAssertEqual(ranked.count, 1)                     // normalized to one key
        XCTAssertEqual(ranked.first?.0, "you")
        XCTAssertEqual(ranked.first?.1, 2)
    }

    func testLearnedBigramIgnoresEmptyTokensAndUnknownPrev() {
        let store = LearnedBigramsStore(inMemory: true)
        store.record(prev: "", next: "hi")
        store.record(prev: "hi", next: "   ")
        store.record(prev: "!!!", next: "???")
        XCTAssertTrue(store.nextWords(after: "hi").isEmpty)
        XCTAssertTrue(store.nextWords(after: "nope").isEmpty)
    }

    func testLearnedBigramResetClears() {
        let store = LearnedBigramsStore(inMemory: true)
        store.record(prev: "going", next: "to")
        XCTAssertFalse(store.nextWords(after: "going").isEmpty)
        store.reset()
        XCTAssertTrue(store.nextWords(after: "going").isEmpty)
    }

    // MARK: - NGramPredictor: seed ranking

    func testSeedBigramRankingReturnsTopThreeByWeight() async {
        let seed: [(prev: String, next: String, count: Int)] = [
            ("i", "am", 95), ("i", "have", 92), ("i", "will", 88), ("i", "think", 82),
        ]
        let predictor = NGramPredictor(seedEntries: seed, learned: LearnedBigramsStore(inMemory: true))
        let out = await predictor.predict(context: "hello i")
        XCTAssertEqual(out, ["am", "have", "will"])         // top-3 by weight, capped
    }

    func testEmptyWhenNoDataForLastWord() async {
        let seed: [(prev: String, next: String, count: Int)] = [("going", "to", 98)]
        let predictor = NGramPredictor(seedEntries: seed, learned: LearnedBigramsStore(inMemory: true))
        let out = await predictor.predict(context: "totally unknown zzz")
        XCTAssertTrue(out.isEmpty)
    }

    func testEmptyWhenNoContext() async {
        let seed: [(prev: String, next: String, count: Int)] = [("i", "am", 95)]
        let predictor = NGramPredictor(seedEntries: seed, learned: LearnedBigramsStore(inMemory: true))
        let empty = await predictor.predict(context: "")
        let blank = await predictor.predict(context: "   ")
        XCTAssertTrue(empty.isEmpty)
        XCTAssertTrue(blank.isEmpty)
    }

    func testDedupesSeedAndLearnedForSameNextWord() async {
        // Seed has "i -> am"; learned also has "i -> am". Must appear once.
        let seed: [(prev: String, next: String, count: Int)] = [("i", "am", 50), ("i", "will", 40)]
        let learned = LearnedBigramsStore(inMemory: true)
        learned.record(prev: "i", next: "am")
        let predictor = NGramPredictor(seedEntries: seed, learned: learned, learnedBoost: 4)
        let out = await predictor.predict(context: "i")
        XCTAssertEqual(out.filter { $0.lowercased() == "am" }.count, 1)
    }

    // MARK: - NGramPredictor: learned boost

    func testLearnedBigramBoostReordersAboveSeed() async {
        // Seed ranks "will" above "want" for "i". A repeatedly-learned "i want" should
        // outrank the seed's "will" once the boost applies.
        let seed: [(prev: String, next: String, count: Int)] = [
            ("i", "will", 88), ("i", "want", 30),
        ]
        let learned = LearnedBigramsStore(inMemory: true)
        for _ in 0..<20 { learned.record(prev: "i", next: "want") } // 20 * boost(4)=80 + seed30 = 110
        let predictor = NGramPredictor(seedEntries: seed, learned: learned, learnedBoost: 4)
        let out = await predictor.predict(context: "so i")
        XCTAssertEqual(out.first, "want", "Heavily-learned pair should outrank the seed")
        XCTAssertTrue(out.contains("will"))
    }

    func testLearnedOnlyWorksWithEmptySeed() async {
        // No seed at all → learned bigrams still drive predictions.
        let learned = LearnedBigramsStore(inMemory: true)
        learned.record(prev: "chai", next: "peelo")
        learned.record(prev: "chai", next: "peelo")
        learned.record(prev: "chai", next: "banao")
        let predictor = NGramPredictor(seedEntries: [], learned: learned)
        let out = await predictor.predict(context: "ek cup chai")
        XCTAssertEqual(out.first, "peelo")                  // higher learned count first
        XCTAssertTrue(out.contains("banao"))
    }

    // MARK: - NGramPredictor: trigram-ish refinement + self-suppression

    func testTrigramRefinementLiftsSecondWordLearnedNext() async {
        // "let me" — seed gives "me -> know/see"; the user has learned "let -> me" AND
        // (importantly) after the earlier word "let" also learned "know", which the
        // trigram refinement lifts. Baseline just checks two-word history is honoured.
        let seed: [(prev: String, next: String, count: Int)] = [
            ("me", "see", 40), ("me", "help", 20),
        ]
        let learned = LearnedBigramsStore(inMemory: true)
        // Earlier-word ("let") learned next "know" adds a light lift onto "me"'s row.
        for _ in 0..<5 { learned.record(prev: "let", next: "know") }
        for _ in 0..<10 { learned.record(prev: "me", next: "know") }
        let predictor = NGramPredictor(seedEntries: seed, learned: learned, learnedBoost: 4)
        let out = await predictor.predict(context: "let me")
        XCTAssertEqual(out.first, "know")                   // learned + trigram lift wins
        XCTAssertFalse(out.contains("me"), "Never suggest the just-typed word back")
        XCTAssertFalse(out.contains("let"), "Never suggest the previous word back")
    }

    func testNeverSuggestsTheJustTypedWord() async {
        // Seed pathologically points a word back at itself; it must be filtered.
        let seed: [(prev: String, next: String, count: Int)] = [
            ("the", "the", 99), ("the", "best", 50),
        ]
        let predictor = NGramPredictor(seedEntries: seed, learned: LearnedBigramsStore(inMemory: true))
        let out = await predictor.predict(context: "this is the")
        XCTAssertFalse(out.contains("the"))
        XCTAssertEqual(out, ["best"])
    }

    func testUsesOnlyLastTwoTokensOfLongContext() async {
        let seed: [(prev: String, next: String, count: Int)] = [("to", "be", 95), ("to", "do", 86)]
        let predictor = NGramPredictor(seedEntries: seed, learned: LearnedBigramsStore(inMemory: true))
        let out = await predictor.predict(context: "a very long sentence that ends with the word to")
        XCTAssertEqual(out, ["be", "do"])                   // keyed on the tail token "to"
    }

    // MARK: - PredictorSelector (fallback chain)

    func testSelectorPicksNGramWhenFMUnavailable() {
        // Simulator reality: FM unavailable → n-gram is chosen (exercises the fallback).
        var builtFM = false
        var builtNGram = false
        let chosen = PredictorSelector.select(
            isFMAvailable: false,
            makeFM: { builtFM = true; return StubNextWordPredictor(words: ["fm"]) },
            makeNGram: { builtNGram = true; return StubNextWordPredictor(words: ["ngram"]) }
        )
        XCTAssertFalse(builtFM, "FM must NOT be built when unavailable")
        XCTAssertTrue(builtNGram, "n-gram must be built as the fallback")
        _ = chosen
    }

    func testSelectorPicksFMWhenAvailable() async {
        var builtFM = false
        var builtNGram = false
        let chosen = PredictorSelector.select(
            isFMAvailable: true,
            makeFM: { builtFM = true; return StubNextWordPredictor(words: ["fm"]) },
            makeNGram: { builtNGram = true; return StubNextWordPredictor(words: ["ngram"]) }
        )
        XCTAssertTrue(builtFM, "FM must be built when available")
        XCTAssertFalse(builtNGram, "n-gram must NOT be built when FM is chosen")
        let out = await chosen.predict(context: "anything")
        XCTAssertEqual(out, ["fm"])                          // the FM predictor is the one returned
    }

    func testSelectorHonoursLiveFMAvailability() {
        // Exercise the *live* availability flag (whichever branch the current host
        // takes). The selector must build exactly one predictor and pick the branch
        // matching `FoundationModelsPredictor.isAvailable` — no crash either way.
        // (FM availability varies by simulator/host, so we don't hard-code it.)
        let live = FoundationModelsPredictor.isAvailable
        var builtFM = false
        var builtNGram = false
        _ = PredictorSelector.select(
            isFMAvailable: live,
            makeFM: { builtFM = true; return StubNextWordPredictor(words: ["fm"]) },
            makeNGram: { builtNGram = true; return StubNextWordPredictor(words: ["ngram"]) }
        )
        XCTAssertEqual(builtFM, live, "FM built iff available")
        XCTAssertEqual(builtNGram, !live, "n-gram built iff FM unavailable")
        XCTAssertTrue(builtFM != builtNGram, "Exactly one predictor is built")
    }
}
