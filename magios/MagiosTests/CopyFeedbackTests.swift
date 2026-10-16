import XCTest
import SwiftUI
@testable import Magician

/// The shared tap-to-copy feedback: what is copied, the notices, the hints,
/// and Reduce Motion.
final class CopyFeedbackTests: XCTestCase {
    func testCopiesTheFullValueAndReturnsTheNotice() {
        var written: [String] = []
        let notice = CopyFeedback.copy("  task_2679cefd6f01  ", kind: .taskId) { written.append($0) }
        XCTAssertEqual(written, ["task_2679cefd6f01"])
        XCTAssertEqual(notice, "Copied task id")

        let pathNotice = CopyFeedback.copy("/Users/me/out/brief.md", kind: .path) { written.append($0) }
        XCTAssertEqual(written.last, "/Users/me/out/brief.md")
        XCTAssertEqual(pathNotice, "Copied path")
    }

    func testNothingToCopyWritesNothing() {
        var written: [String] = []
        XCTAssertNil(CopyFeedback.copy(nil, kind: .path) { written.append($0) })
        XCTAssertNil(CopyFeedback.copy("   ", kind: .taskId) { written.append($0) })
        XCTAssertTrue(written.isEmpty)
    }

    func testLabelsAndHints() {
        XCTAssertEqual(CopyFeedback.copiedLabel, "Copied ✓")
        XCTAssertEqual(CopyFeedback.announcement, "Copied")
        XCTAssertEqual(CopyFeedback.revertAfter, 1.2, accuracy: 0.001)
        XCTAssertEqual(CopyKind.taskId.hint, "Double-tap to copy the full id")
        XCTAssertEqual(CopyKind.path.hint, "Double-tap to copy the full path")
    }

    func testReduceMotionSwapsInstantly() {
        XCTAssertNil(CopyFeedback.animation(reduceMotion: true))
        XCTAssertNotNil(CopyFeedback.animation(reduceMotion: false))
    }

    func testCardShortIdIsTheTitleRulesShortId() {
        XCTAssertEqual(TaskTitleRules.shortTaskId("task_2679cefd6f01"), "2679ce")
    }
}
