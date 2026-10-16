import XCTest
@testable import Magician

final class UnifiedDiffParserTests: XCTestCase {

    func testParsesPathsHunksAndLineNumbers() {
        let text = """
        diff --git a/src/app.ts b/src/app.ts
        index 1111111..2222222 100644
        --- a/src/app.ts
        +++ b/src/app.ts
        @@ -10,4 +10,5 @@ export function main() {
         const a = 1;
        -const b = 2;
        +const b = 3;
        +const c = 4;
         return a;
         }

        """
        let diff = UnifiedDiffParser.parse(text)
        XCTAssertEqual(diff.oldPath, "src/app.ts")
        XCTAssertEqual(diff.newPath, "src/app.ts")
        XCTAssertEqual(diff.hunks.count, 1)
        let hunk = diff.hunks[0]
        XCTAssertEqual(hunk.oldStart, 10)
        XCTAssertEqual(hunk.oldCount, 4)
        XCTAssertEqual(hunk.newStart, 10)
        XCTAssertEqual(hunk.newCount, 5)
        XCTAssertEqual(hunk.section, "export function main() {")
        XCTAssertEqual(hunk.lines.map(\.kind), [.context, .removed, .added, .added, .context, .context])
        XCTAssertEqual(hunk.lines.map(\.oldNumber), [10, 11, nil, nil, 12, 13])
        XCTAssertEqual(hunk.lines.map(\.newNumber), [10, nil, 11, 12, 13, 14])
        XCTAssertEqual(hunk.lines[1].text, "const b = 2;")
        XCTAssertEqual(hunk.lines[2].text, "const b = 3;")
        XCTAssertEqual(diff.additions, 2)
        XCTAssertEqual(diff.deletions, 1)
    }

    func testHeaderWithoutCountsMeansOneLine() {
        let header = UnifiedDiffParser.parseHeader("@@ -1 +1 @@")
        XCTAssertEqual(header, UnifiedDiffParser.Header(oldStart: 1, oldCount: 1, newStart: 1, newCount: 1, section: ""))
        XCTAssertNil(UnifiedDiffParser.parseHeader("@@ nonsense @@"))
    }

    func testNewFileHasNoOldPath() {
        let text = """
        --- /dev/null
        +++ b/db/migrations/0042.sql
        @@ -0,0 +1,2 @@
        +CREATE EXTENSION pg_trgm;
        +CREATE INDEX t ON products USING gin (title gin_trgm_ops);
        """
        let diff = UnifiedDiffParser.parse(text)
        XCTAssertNil(diff.oldPath)
        XCTAssertEqual(diff.newPath, "db/migrations/0042.sql")
        XCTAssertEqual(diff.hunks.first?.lines.map(\.newNumber), [1, 2])
        XCTAssertEqual(diff.additions, 2)
    }

    func testNoNewlineMarkerIsANoteWithoutNumbers() {
        let text = """
        @@ -1,1 +1,1 @@
        -old
        \\ No newline at end of file
        +new
        \\ No newline at end of file
        """
        let lines = UnifiedDiffParser.parse(text).hunks[0].lines
        XCTAssertEqual(lines.map(\.kind), [.removed, .note, .added, .note])
        XCTAssertEqual(lines[1].text, "No newline at end of file")
        XCTAssertNil(lines[1].oldNumber)
        XCTAssertEqual(lines[2].newNumber, 1, "a note does not advance the line numbers")
    }

    func testUnchangedLinesBetweenHunks() {
        let text = """
        @@ -1,2 +1,2 @@
         a
        -b
        +B
        @@ -20,2 +20,3 @@
         t
        +u
         v
        """
        let hunks = UnifiedDiffParser.parse(text).hunks
        XCTAssertEqual(hunks.count, 2)
        XCTAssertEqual(hunks[1].unchangedBefore, 17, "lines 3 to 19 are unchanged")
        XCTAssertEqual(hunks[1].lines.map(\.newNumber), [20, 21, 22])
    }

    func testCRLFAndEmptyContextLines() {
        let text = "@@ -1,3 +1,3 @@\r\n a\r\n\r\n-c\r\n+C\r\n"
        let lines = UnifiedDiffParser.parse(text).hunks[0].lines
        XCTAssertEqual(lines.map(\.kind), [.context, .context, .removed, .added])
        XCTAssertEqual(lines[0].text, "a")
        XCTAssertEqual(lines[1].text, "")
        XCTAssertEqual(lines[1].oldNumber, 2)
    }

    func testBinaryFile() {
        let diff = UnifiedDiffParser.parse("diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n")
        XCTAssertTrue(diff.isBinary)
        XCTAssertTrue(diff.hunks.isEmpty)
    }

    func testOnlyTheFirstFileOfAMultiFileDiff() {
        let text = """
        diff --git a/a b/a
        @@ -1 +1 @@
        -x
        +y
        diff --git a/b b/b
        @@ -1 +1 @@
        -p
        +q
        """
        let diff = UnifiedDiffParser.parse(text)
        XCTAssertEqual(diff.hunks.count, 1)
        XCTAssertEqual(diff.hunks[0].lines.map(\.text), ["x", "y"])
    }

    func testLongUnchangedRunsFoldKeepingThreeLinesEachSide() {
        var body = "@@ -1,22 +1,22 @@\n-old\n+new\n"
        for index in 0..<20 { body += " same \(index)\n" }
        body += "-tail\n+TAIL\n"
        let hunk = UnifiedDiffParser.parse(body).hunks[0]
        let segments = hunk.segments(threshold: 8, keep: 3)
        XCTAssertEqual(segments.count, 3)
        guard case .lines(let head) = segments[0],
              case .fold(_, let hidden) = segments[1],
              case .lines(let tail) = segments[2] else {
            return XCTFail("expected lines, fold, lines: \(segments)")
        }
        XCTAssertEqual(head.count, 2 + 3)
        XCTAssertEqual(hidden.count, 14)
        XCTAssertEqual(tail.count, 3 + 2)
        XCTAssertEqual(hidden.first?.text, "same 3")
    }

    func testShortUnchangedRunsAndLeadingContextStayVisible() {
        var body = "@@ -1,12 +1,12 @@\n"
        for index in 0..<10 { body += " lead \(index)\n" }
        body += "-a\n+b\n same\n"
        let segments = UnifiedDiffParser.parse(body).hunks[0].segments(threshold: 8, keep: 3)
        // The leading run folds entirely except the 3 lines next to the change.
        guard case .fold(_, let hidden) = segments.first else { return XCTFail("\(segments)") }
        XCTAssertEqual(hidden.count, 7)
        guard case .lines(let rest) = segments.last else { return XCTFail("\(segments)") }
        XCTAssertEqual(rest.count, 3 + 2 + 1)
    }
}
