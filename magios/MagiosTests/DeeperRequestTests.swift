import XCTest

@testable import Magician

/// Mirrors `deeperRequest.test.ts` case for case. The two platforms must ask
/// for the same thing in the same words — the storyboard contract's
/// "change representation, not volume" rule is written against this phrasing.
final class DeeperRequestTests: XCTestCase {
    private let step = DeeperRequest.Step(
        revealId: "why-it-unrolls",
        label: "Why it unrolls",
        narration: "The curved side flattens into a sector."
    )

    func testOffersTheControlForANameableStep() {
        XCTAssertTrue(DeeperRequest.canRequest(step: step, sessionId: "session-1"))
        XCTAssertTrue(
            DeeperRequest.canRequest(
                step: DeeperRequest.Step(narration: "Only narration."), sessionId: "session-1"))
        XCTAssertTrue(
            DeeperRequest.canRequest(
                step: DeeperRequest.Step(label: "Only a label"), sessionId: "session-1"))
    }

    func testStillOffersTheControlWithNoStepToName() {
        // Naming the step sharpens the ask; it does not gate it.
        XCTAssertTrue(
            DeeperRequest.canRequest(
                step: DeeperRequest.Step(revealId: "step-1"), sessionId: "session-1"))
        XCTAssertTrue(DeeperRequest.canRequest(step: nil, sessionId: "session-1"))
    }

    func testDeclinesWithoutASession() {
        XCTAssertFalse(DeeperRequest.canRequest(step: step, sessionId: nil))
        XCTAssertFalse(DeeperRequest.canRequest(step: step, sessionId: "   "))
    }

    func testInvokesTheTutorRail() {
        // The rail is selected by invoke word. Without `@tutor` this is an
        // ordinary chat turn and draws nothing at all.
        XCTAssertTrue(DeeperRequest.buildPrompt(step: step).hasPrefix("@tutor "))
    }

    func testNamesTheOneStepAndKeepsTheRest() {
        let prompt = DeeperRequest.buildPrompt(step: step)
        XCTAssertTrue(prompt.contains("\"Why it unrolls\""))
        XCTAssertTrue(prompt.contains("keep the rest of the lesson as it was"))
    }

    func testCarriesWhatWasAlreadySaid() {
        XCTAssertTrue(
            DeeperRequest.buildPrompt(step: step)
                .contains("The curved side flattens into a sector."))
    }

    func testAsksForDecompositionNotMoreWords() {
        let prompt = DeeperRequest.buildPrompt(step: step)
        XCTAssertTrue(prompt.contains("sub-steps"))
        XCTAssertTrue(prompt.contains("intermediate stages"))
        XCTAssertTrue(prompt.contains("rather than restating"))
    }

    func testFlattensMultiLineNarration() {
        let prompt = DeeperRequest.buildPrompt(
            step: DeeperRequest.Step(label: "A\nstep", narration: "Line one.\n\n   Line two."))
        XCTAssertTrue(prompt.contains("\"A step\""))
        XCTAssertTrue(prompt.contains("\"Line one. Line two.\""))
        XCTAssertFalse(prompt.contains("\n"))
    }

    func testDeepensTheMostRecentExplanationWhenNoStepIsNamed() {
        XCTAssertTrue(
            DeeperRequest.buildPrompt(step: DeeperRequest.Step(narration: "Only narration."))
                .contains("go deeper on the part you just explained"))
        XCTAssertTrue(
            DeeperRequest.buildPrompt(step: DeeperRequest.Step())
                .contains("go deeper on the part you just explained"))
    }

    func testOmitsTheQuotedNarrationWhenThereIsNone() {
        XCTAssertFalse(
            DeeperRequest.buildPrompt(step: DeeperRequest.Step(label: "Just a label"))
                .contains("So far you explained"))
    }

    func testComposeTrimsAndMatchesTheBuilder() {
        let composed = DeeperRequest.compose(step: step, sessionId: " session-1 ")
        XCTAssertEqual(composed?.sessionId, "session-1")
        XCTAssertEqual(composed?.prompt, DeeperRequest.buildPrompt(step: step))
    }

    func testComposeReturnsNilExactlyWhenTheControlIsHidden() {
        // The guard and the builder must agree, or the UI shows a button that
        // does nothing.
        let cases: [(DeeperRequest.Step?, String?)] = [
            (step, nil),
            (step, "  "),
            (nil, nil),
        ]
        for (candidate, session) in cases {
            XCTAssertFalse(DeeperRequest.canRequest(step: candidate, sessionId: session))
            XCTAssertNil(DeeperRequest.compose(step: candidate, sessionId: session))
        }
    }
}
