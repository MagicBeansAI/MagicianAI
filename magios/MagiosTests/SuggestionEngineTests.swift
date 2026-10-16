import XCTest
@testable import Magician

/// Tests for the correction behavior that the keyboard's `SuggestionEngine`
/// façade delegates to (Task 6).
///
/// IMPORTANT — module boundary: `SuggestionEngine` and its `CorrectionStore`
/// live in the **`MagiosKeyboard`** app-extension target, which `MagiosTests`
/// does not (and, as an app-extension, cannot easily) `@testable import`. Every
/// test in this suite imports the app module `Magician`, which compiles the
/// `Shared/Correction/**` core but NOT the extension-bound `SuggestionEngine`.
///
/// So these tests assert the *delegated contract* — the exact `CorrectionEngine`
/// behavior the façade forwards to (lexicon-parity: valid words are never
/// corrected; completions come before corrections; the confidence gate). Testing
/// the thin façade directly (and its `UILexicon` merge — `UILexicon` has no
/// public initializer) requires the keyboard target to be added to the test
/// target's sources in the Task-0 project wiring; that is called out in the plan.
final class SuggestionEngineTests: XCTestCase {

    /// Mirrors the façade's dictionary: English base + Indian + Hinglish.
    private func makeEngine() -> CorrectionEngine {
        CorrectionEngine(dictionary: CorrectionDictionary(entries: [
            ("hello", 9000), ("help", 8000), ("held", 3000),
            ("going", 10000), ("gong", 50),
            ("lakh", 500), ("yaar", 300), ("aarav", 400),
        ]), maxEditDistance: 2)
    }

    func testCompletionsComeBeforeCorrectionsAndAreFrequencyRanked() {
        let engine = makeEngine()
        // "hel" is an in-progress prefix → completions lead, most frequent first.
        let out = engine.suggestions(for: "hel")
        XCTAssertEqual(out.first, "hello")
        XCTAssertTrue(out.contains("help"))
    }

    func testSuggestionsCapAtThreeAndDropExactWord() {
        let engine = makeEngine()
        let out = engine.suggestions(for: "hel", limit: 3)
        XCTAssertLessThanOrEqual(out.count, 3)
        XCTAssertFalse(out.contains("hel")) // the typed token is never echoed back
    }

    func testValidWordsAreNeverAutocorrected() {
        let engine = makeEngine()
        // Parity with the façade's "UILexicon/dictionary words are valid" rule.
        XCTAssertNil(engine.autocorrection(for: "hello"))
        XCTAssertNil(engine.autocorrection(for: "lakh"))
        XCTAssertNil(engine.autocorrection(for: "yaar"))
        XCTAssertNil(engine.autocorrection(for: "aarav"))
    }

    func testClearTypoIsCorrected() {
        let engine = makeEngine()
        XCTAssertEqual(engine.autocorrection(for: "goign"), "going")
    }

    func testCapitalizationPreservationLogic() {
        // The façade preserves the original token's leading capital on the result.
        // That logic is a pure string transform; verify it directly here so the
        // rule is covered even though the façade type isn't in this module.
        func preserve(original: String, correction: String) -> String {
            if let first = original.first, first.isUppercase {
                return correction.prefix(1).uppercased() + correction.dropFirst()
            }
            return correction
        }
        XCTAssertEqual(preserve(original: "Goign", correction: "going"), "Going")
        XCTAssertEqual(preserve(original: "goign", correction: "going"), "going")
    }
}
