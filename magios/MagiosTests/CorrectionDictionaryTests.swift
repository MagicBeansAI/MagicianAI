import XCTest
@testable import Magician

/// Tests for the merged frequency dictionary (Task 1 of the keyboard
/// Indian-English + Hinglish autocorrect plan).
final class CorrectionDictionaryTests: XCTestCase {
    func testMergesBaseIndianHinglishWithFrequencies() {
        let dict = CorrectionDictionary(entries: [
            ("hello", 1000), ("lakh", 500), ("yaar", 300), ("hello", 50) // dup: max wins
        ])
        XCTAssertEqual(dict.frequency(of: "hello"), 1000)   // max wins on dup
        XCTAssertEqual(dict.frequency(of: "lakh"), 500)
        XCTAssertEqual(dict.frequency(of: "yaar"), 300)
        XCTAssertTrue(dict.contains("lakh"))
        XCTAssertFalse(dict.contains("zzzz"))
    }

    func testIsCaseInsensitive() {
        let dict = CorrectionDictionary(entries: [("Aarav", 900)])
        XCTAssertTrue(dict.contains("aarav"))
        XCTAssertTrue(dict.contains("AARAV"))
        XCTAssertEqual(dict.frequency(of: "Aarav"), 900)
    }

    func testDropsEmptyAndNonPositiveEntries() {
        let dict = CorrectionDictionary(entries: [("", 100), ("ok", 0), ("go", 5)])
        XCTAssertFalse(dict.contains(""))
        XCTAssertFalse(dict.contains("ok"))   // freq 0 dropped
        XCTAssertTrue(dict.contains("go"))
    }

    func testTolerantParserSkipsMalformedLines() {
        var table: [String: Int] = [:]
        CorrectionDictionary.parse(line: "hello\t1000", into: &table)
        CorrectionDictionary.parse(line: "world 500", into: &table)          // space-delimited
        CorrectionDictionary.parse(line: "# a comment", into: &table)        // comment
        CorrectionDictionary.parse(line: "", into: &table)                   // blank
        CorrectionDictionary.parse(line: "noCountHere", into: &table)        // no count
        CorrectionDictionary.parse(line: "bad\tNaN", into: &table)           // non-numeric count
        CorrectionDictionary.parse(line: "lakh\t500000", into: &table)
        XCTAssertEqual(table["hello"], 1000)
        XCTAssertEqual(table["world"], 500)
        XCTAssertEqual(table["lakh"], 500000)
        XCTAssertNil(table["# a comment"])
        XCTAssertNil(table["nocounthere"])
        XCTAssertNil(table["bad"])
        XCTAssertEqual(table.count, 3)
    }

    func testMergingAddsAndBoosts() {
        let base = CorrectionDictionary(entries: [("chai", 9000)])
        let merged = base.merging([("chaiwala", 3000), ("chai", 100)])
        XCTAssertEqual(merged.frequency(of: "chaiwala"), 3000)
        XCTAssertEqual(merged.frequency(of: "chai"), 9000) // max preserved, not lowered
    }

    func testCapKeepsTopByFrequencyAndForcedKeeps() {
        let dict = CorrectionDictionary(entries: [
            ("common", 10000), ("frequent", 5000), ("rare", 3), ("yaar", 2)
        ])
        // Cap to top-2 but force-keep "yaar" (a curated Indian/Hinglish word).
        let capped = dict.cappedToTop(2, keeping: ["yaar"])
        XCTAssertTrue(capped.contains("common"))
        XCTAssertTrue(capped.contains("frequent"))
        XCTAssertFalse(capped.contains("rare"))   // dropped tail
        XCTAssertTrue(capped.contains("yaar"))    // force-kept despite low freq
    }
}
