import XCTest
@testable import Magician

final class NoteFrontMatterTests: XCTestCase {
    func testMagicianNoteHidesItsFrontMatter() {
        let note = "---\nmagician_kind: note\ncreated_at: 2026-10-21T09:00:00Z\n---\n\n# Weekly report\n\nBody.\n"
        XCTAssertEqual(NoteFrontMatter.body(note), "# Weekly report\n\nBody.\n")
    }

    func testCrlfAndDotsTerminator() {
        let note = "---\r\nmagician_kind: note\r\n...\r\n\r\nBody\r\n"
        XCTAssertEqual(NoteFrontMatter.body(note), "Body\r\n")
    }

    func testBomLeadingBlankLinesAndTrailingFenceSpaces() {
        let note = "\u{FEFF}\n  \n---  \ntitle: x\n---\t \nBody"
        XCTAssertEqual(NoteFrontMatter.body(note), "Body")
    }

    func testQuotedKeysCommentsAndNestedValues() {
        let note = "---\n# generated\n\"my key\": 1\ntags:\n  - a\n---\nBody"
        XCTAssertEqual(NoteFrontMatter.body(note), "Body")
    }

    func testEmptyBlockIsHidden() {
        XCTAssertEqual(NoteFrontMatter.body("---\n---\nBody"), "Body")
    }

    func testFrontMatterOnlyLeavesNothing() {
        XCTAssertEqual(NoteFrontMatter.body("---\na: 1\n---"), "")
    }

    func testNoFrontMatterIsUnchanged() {
        let note = "# Title\n\n---\n\nmore"
        XCTAssertEqual(NoteFrontMatter.body(note), note)
        XCTAssertEqual(NoteFrontMatter.body(""), "")
    }

    func testUnterminatedBlockIsUnchanged() {
        let note = "---\nmagician_kind: note\n# Title\nBody"
        XCTAssertEqual(NoteFrontMatter.body(note), note)
    }

    func testRuleFollowedByProseIsNotFrontMatter() {
        let note = "---\nJust a paragraph after a rule.\n---\nMore"
        XCTAssertEqual(NoteFrontMatter.body(note), note)
    }

    func testOpeningFenceMustBeTheFirstLine() {
        let note = "Intro\n---\na: 1\n---\nBody"
        XCTAssertEqual(NoteFrontMatter.body(note), note)
    }

    func testOnlyAnOpeningFenceIsUnchanged() {
        XCTAssertEqual(NoteFrontMatter.body("---"), "---")
    }

    func testFencesWithOtherTextDoNotClose() {
        let note = "---\na: 1\n----\nb: 2\n--- x\n"
        XCTAssertEqual(NoteFrontMatter.body(note), note)
    }
}
