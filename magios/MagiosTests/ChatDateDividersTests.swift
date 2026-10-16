import XCTest
@testable import Magician

final class ChatDateDividersTests: XCTestCase {
    private var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "Asia/Kolkata")!
        return calendar
    }()
    private let locale = Locale(identifier: "en_US")

    /// Thu 1 Oct 2026, 12:00 IST.
    private var now: Date { date(2026, 10, 1, 12) }

    private func date(_ y: Int, _ m: Int, _ d: Int, _ h: Int, _ min: Int = 0) -> Date {
        calendar.date(from: DateComponents(year: y, month: m, day: d, hour: h, minute: min))!
    }

    func testFirstDatedMessageAndEachDayChangeOpenADivider() {
        let stamps: [Date?] = [
            date(2026, 9, 29, 9), date(2026, 9, 29, 22),
            date(2026, 9, 30, 0, 5),
            date(2026, 10, 1, 8), date(2026, 10, 1, 11)
        ]
        XCTAssertEqual(ChatDateDividers.insertionIndices(for: stamps, calendar: calendar), [0, 2, 3])
    }

    func testUndatedMessagesNeverOpenADayNorBreakTheRun() {
        let stamps: [Date?] = [nil, date(2026, 10, 1, 8), nil, date(2026, 10, 1, 9), date(2026, 10, 2, 1), nil]
        XCTAssertEqual(ChatDateDividers.insertionIndices(for: stamps, calendar: calendar), [1, 4])
        XCTAssertEqual(ChatDateDividers.insertionIndices(for: [], calendar: calendar), [])
        XCTAssertEqual(ChatDateDividers.insertionIndices(for: [nil, nil], calendar: calendar), [])
    }

    func testDayBoundaryFollowsTheLocalCalendar() {
        // 23:59 and 00:01 local are different days even 2 minutes apart.
        let stamps: [Date?] = [date(2026, 9, 30, 23, 59), date(2026, 10, 1, 0, 1)]
        XCTAssertEqual(ChatDateDividers.insertionIndices(for: stamps, calendar: calendar), [0, 1])
    }

    func testLabels() {
        func label(_ d: Date) -> String {
            ChatDateDividers.label(for: d, now: now, calendar: calendar, locale: locale)
        }
        XCTAssertEqual(label(date(2026, 10, 1, 0, 1)), "Today")
        XCTAssertEqual(label(date(2026, 9, 30, 23, 59)), "Yesterday")
        XCTAssertEqual(label(date(2026, 9, 29, 10)), "Tuesday")
        XCTAssertEqual(label(date(2026, 9, 25, 10)), "Friday")   // 6 days back
        XCTAssertEqual(label(date(2026, 9, 24, 10)), "Sep 24, 2026")  // 7 days back
        XCTAssertEqual(label(date(2025, 12, 31, 10)), "Dec 31, 2025")
    }

    func testLabelsMapEveryDividerIndex() {
        let stamps: [Date?] = [date(2026, 9, 30, 9), nil, date(2026, 10, 1, 9)]
        XCTAssertEqual(
            ChatDateDividers.labels(for: stamps, now: now, calendar: calendar, locale: locale),
            [0: "Yesterday", 2: "Today"])
    }

    func testEpochAcceptsMillisecondsAndSeconds() {
        XCTAssertEqual(ChatDateDividers.date(epoch: 1_759_300_000_000), Date(timeIntervalSince1970: 1_759_300_000))
        XCTAssertEqual(ChatDateDividers.date(epoch: 1_759_300_000), Date(timeIntervalSince1970: 1_759_300_000))
        XCTAssertNil(ChatDateDividers.date(epoch: 0))
        XCTAssertNil(ChatDateDividers.date(epoch: .nan))
    }

    func testFloatingLabelIsTheTopmostVisibleDatedMessage() {
        let stamps: [Date?] = [date(2026, 9, 30, 8), nil, date(2026, 9, 30, 9), date(2026, 10, 1, 9)]
        XCTAssertEqual(ChatDateDividers.floatingLabel(
            timestamps: stamps, visibleIndices: [3, 1, 2], now: now, calendar: calendar, locale: locale),
            "Yesterday")
        XCTAssertNil(ChatDateDividers.floatingLabel(
            timestamps: stamps, visibleIndices: [1], now: now, calendar: calendar, locale: locale))
        XCTAssertNil(ChatDateDividers.floatingLabel(
            timestamps: stamps, visibleIndices: [], now: now, calendar: calendar, locale: locale))
    }

    func testFloatingLabelNeverDoublesAVisibleDivider() {
        let stamps: [Date?] = [date(2026, 9, 30, 8), nil, date(2026, 9, 30, 9), date(2026, 10, 1, 9)]
        // Yesterday's divider (index 0) is on screen: no pill.
        XCTAssertNil(ChatDateDividers.floatingLabel(
            timestamps: stamps, visibleIndices: [0, 1, 2], now: now, calendar: calendar, locale: locale))
        // Only Today's divider is on screen; the top message is Yesterday's.
        XCTAssertEqual(ChatDateDividers.floatingLabel(
            timestamps: stamps, visibleIndices: [2, 3], now: now, calendar: calendar, locale: locale),
            "Yesterday")
    }
}
