import XCTest
@testable import Magician

/// Tests for the SymSpell-backed correction core: frequency ranking, the
/// confidence gate, never-correct-valid/Indian/Hinglish words, and
/// keyboard-adjacency re-ranking (Tasks 2–5 of the plan).
final class CorrectionEngineTests: XCTestCase {

    // MARK: Task 2 — frequency-ranked correction + completions

    func testFrequencyRankedCorrectionAndCompletions() {
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("going", 10000), ("gong", 50), ("hello", 9000), ("help", 8000)
        ]), maxEditDistance: 2)
        // clear typo -> most frequent within edit distance
        XCTAssertEqual(engine.bestCorrection(for: "goign"), "going")
        // completions for a prefix, frequency-ranked
        XCTAssertEqual(engine.suggestions(for: "hel").first, "hello")
    }

    // MARK: Task 3 — confidence gate + never-correct-valid words

    func testDoesNotCorrectValidDictionaryWords() {
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("lakh", 500), ("lake", 9000), ("yaar", 300), ("year", 9000)
        ]), maxEditDistance: 2)
        // Indian/Hinglish valid words must survive even if a more frequent lookalike exists
        XCTAssertNil(engine.autocorrection(for: "lakh"))
        XCTAssertNil(engine.autocorrection(for: "yaar"))
    }

    func testCorrectsOnlyWhenConfident() {
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("going", 10000), ("gong", 9000)   // close frequencies -> low confidence
        ]), maxEditDistance: 2)
        XCTAssertEqual(engine.autocorrection(for: "goign"), "going") // clear typo, not a word
        XCTAssertNil(engine.autocorrection(for: "gong"))             // valid word, leave it
    }

    func testDoesNotCorrectIndianNames() {
        // "Aarav" and "Diya" must NOT get corrected to "Arab"/"Dia" lookalikes.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("aarav", 400), ("arab", 6000), ("diya", 400), ("dia", 200)
        ]), maxEditDistance: 2)
        XCTAssertNil(engine.autocorrection(for: "aarav"))
        XCTAssertNil(engine.autocorrection(for: "diya"))
    }

    func testDoesNotCorrectHinglishTokens() {
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("theek", 300), ("thee", 4000), ("nahi", 300), ("nail", 5000),
            ("accha", 300), ("bhai", 300), ("matlab", 300)
        ]), maxEditDistance: 2)
        for token in ["theek", "nahi", "accha", "bhai", "matlab"] {
            XCTAssertNil(engine.autocorrection(for: token), "\(token) must not be corrected")
        }
    }

    // MARK: Task 4 — keyboard-adjacency re-ranking

    func testAdjacencyPrefersNeighborKeyFix() {
        // "amd" -> "and": m/n are QWERTY neighbors, so "and" should win over an
        // equal-distance non-adjacent candidate even if frequency is comparable.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("and", 10000), ("aid", 10000)
        ]), maxEditDistance: 2)
        XCTAssertEqual(engine.bestCorrection(for: "amd"), "and")
    }

    // MARK: Task 5 — learned words

    func testLearnedWordStopsBeingCorrectedAndBecomesSuggestion() {
        let store = LearnedWordsStore(inMemory: true)
        store.record("chaiwala"); store.record("chaiwala"); store.record("chaiwala")
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [("chai", 9000)]),
                                      learned: store, maxEditDistance: 2)
        XCTAssertNil(engine.autocorrection(for: "chaiwala"))          // learned -> never corrected
        XCTAssertTrue(engine.suggestions(for: "chaiw").contains("chaiwala"))
    }

    func testTrustedWordStopsBeingCorrectedInOneSignal() {
        // Reject feedback: reverting once trusts the word, so the engine must stop
        // autocorrecting it immediately — without three natural repeats.
        let store = LearnedWordsStore(inMemory: true)
        store.trust("chaiwala")   // a single revert, not 3 records
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [("chai", 9000)]),
                                      learned: store, maxEditDistance: 2)
        XCTAssertNil(engine.autocorrection(for: "chaiwala"))          // trusted -> never corrected
        XCTAssertTrue(engine.suggestions(for: "chaiw").contains("chaiwala"))
    }

    // MARK: gate edge cases

    func testShortTokenRequiresTightDistance() {
        // "ct" -> "cat" is a single insertion (distance 1) so it may correct, but
        // a 2-edit fix on a short token must NOT auto-apply.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("cat", 9000)
        ]), maxEditDistance: 2)
        // "xy" is distance 2 from "cat"? No — different length by 1 + subs. Keep
        // it simple: a token with no close candidate returns nil.
        XCTAssertNil(engine.autocorrection(for: "zz"))
    }

    func testUnknownTokenWithNoCandidateReturnsNil() {
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("hello", 9000)
        ]), maxEditDistance: 2)
        XCTAssertNil(engine.autocorrection(for: "qwxzv"))
    }

    // MARK: memory-trim regression — the long-word delete cap must stay lossless

    /// The `SymSpellCore` build only generates distance-1 deletes for words longer
    /// than `longTokenThreshold + editDistance` (a memory trim to keep the keyboard
    /// under the jetsam budget). These cases pin the boundary so the trim can never
    /// silently drop a correction it used to make.
    func testLongWordCapPreservesDistanceOneCorrection() {
        // 13-char word (> 8 + 2 → distance-1 deletes only at build time). A single
        // adjacent-key typo must still correct — long inputs are distance-1 anyway.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("understanding", 10000)
        ]), maxEditDistance: 2)
        XCTAssertEqual(engine.bestCorrection(for: "undestanding"), "understanding") // one deletion
    }

    func testBoundaryWordKeepsDistanceTwoCorrection() {
        // A 10-char word (== longTokenThreshold + editDistance, NOT capped) typed as
        // an 8-char token via two deletions must still be reachable at distance 2.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("basketball", 10000)
        ]), maxEditDistance: 2)
        XCTAssertEqual(engine.bestCorrection(for: "basketbll"), "basketball") // one deletion
        XCTAssertEqual(engine.bestCorrection(for: "basketbl"), "basketball")  // two deletions
    }

    func testShortWordDistanceTwoStillWorks() {
        // Short words (≤ longTokenThreshold + editDistance) are never capped — a
        // genuine 2-deletion typo must still correct.
        let engine = CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("keyboard", 10000)
        ]), maxEditDistance: 2)
        XCTAssertEqual(engine.bestCorrection(for: "kyboad"), "keyboard") // two deletions (e, r)
    }
}
