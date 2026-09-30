import XCTest
import Combine
@testable import Magician

/// Morning Edition (Today) — the pure helpers behind the masthead, Realtime
/// Wire, Morning Brief deck and Economics ledger, plus the view-model data it
/// adds (wire, fleet buckets, agents, reading-room mode, deck paging,
/// resolution completions). Web contract: `ui/unified-ui/src/lib/today/`.
final class TodayMorningEditionTests: XCTestCase {
    private var cancellables: Set<AnyCancellable> = []
    private var sessions: [URLSession] = []

    override func setUp() {
        super.setUp()
        MockURLProtocol.handler = nil
        cancellables.removeAll()
    }

    override func tearDown() {
        sessions.forEach { $0.invalidateAndCancel() }
        sessions.removeAll()
        MockURLProtocol.handler = nil
        cancellables.removeAll()
        super.tearDown()
    }

    // MARK: Masthead

    func testRomanNumeralsAreSubtractiveAndNeverBelowOne() {
        XCTAssertEqual(TodayMorningEdition.romanNumeral(1), "I")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(4), "IV")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(9), "IX")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(14), "XIV")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(40), "XL")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(1994), "MCMXCIV")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(0), "I")
        XCTAssertEqual(TodayMorningEdition.romanNumeral(-3), "I")
    }

    func testVolumeIssueAndDateFollowTheReadersCalendar() {
        let calendar = utcCalendar()
        let newYear = calendar.date(from: DateComponents(year: 2024, month: 1, day: 1, hour: 9))!
        let leapEnd = calendar.date(from: DateComponents(year: 2024, month: 12, day: 31, hour: 23))!
        let issue = calendar.date(from: DateComponents(year: 2026, month: 9, day: 28, hour: 7))!
        XCTAssertEqual(TodayMorningEdition.dayOfYear(newYear, calendar: calendar), 1)
        XCTAssertEqual(TodayMorningEdition.dayOfYear(leapEnd, calendar: calendar), 366)
        XCTAssertEqual(TodayMorningEdition.volumeLine(for: issue, calendar: calendar), "VOL. IV · NO. 271")
        XCTAssertEqual(TodayMorningEdition.volumeLine(for: newYear, calendar: calendar), "VOL. II · NO. 1")
        XCTAssertEqual(TodayMorningEdition.mastheadDate(issue, calendar: calendar), "Monday, September 28, 2026")

        // Just past local midnight in IST is still the previous day in UTC:
        // the issue number follows the reader, not the server.
        var ist = Calendar(identifier: .gregorian)
        ist.timeZone = TimeZone(identifier: "Asia/Kolkata")!
        let instant = calendar.date(from: DateComponents(year: 2026, month: 9, day: 27, hour: 19))!
        XCTAssertEqual(TodayMorningEdition.dayOfYear(instant, calendar: ist), 271)
        XCTAssertEqual(TodayMorningEdition.dayOfYear(instant, calendar: calendar), 270)
    }

    func testWeatherAndGreetingMatchTheWebContract() {
        XCTAssertEqual(TodayMorningEdition.weatherLine(urgent: 0), "WEATHER: ALL QUIET")
        XCTAssertEqual(TodayMorningEdition.weatherLine(urgent: 3), "WEATHER: 3 URGENT")
        let calendar = utcCalendar()
        func at(_ hour: Int, _ minute: Int = 0) -> Date {
            calendar.date(from: DateComponents(year: 2026, month: 9, day: 28, hour: hour, minute: minute))!
        }
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(4, 59), calendar: calendar), "Good evening")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(5), calendar: calendar), "Good morning")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(11, 59), calendar: calendar), "Good morning")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(12), calendar: calendar), "Good afternoon")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(17, 59), calendar: calendar), "Good afternoon")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(18), calendar: calendar), "Good evening")
        XCTAssertEqual(TodayMorningEdition.greeting(for: at(0), calendar: calendar), "Good evening")
        XCTAssertEqual(TodayViewModel.greeting(for: at(23), calendar: calendar), "Good evening")
    }

    // MARK: Realtime Wire

    func testCompact24hCount() {
        XCTAssertEqual(TodayMorningEdition.compact24hCount(0), "0/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(-4), "0/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(999), "999/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(1_000), "1K/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(1_500), "1.5K/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(35_086), "35K/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(10_600), "11K/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(1_500_000), "1.5M/24H")
        XCTAssertEqual(TodayMorningEdition.compact24hCount(12_345_678), "12M/24H")
    }

    func testEventTypeHumanizer() {
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("event.agent.cycle_completed"), "Agent: Cycle Completed")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("llm.call.failed"), "LLM: Call Failed")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("hitl_request"), "HITL Request")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("api.db.id_rotated"), "API: DB ID Rotated")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("MessageProcessingStarted"), "Message Processing Started")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("HITLRequestCreated"), "HITL Request Created")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType(""), "System event")
        XCTAssertEqual(TodayMorningEdition.humanizeEventType("event."), "System event")
    }

    func testFeedAndAgentUpdateNormalisation() throws {
        let feed = try JSONDecoder().decode(TodayActivityResponse.self, from: jsonData(["items": [
            ["id": "f1", "item_type": "agent_learning", "title": "", "status": "done", "updated_at": 5_000],
            ["id": "f2", "item_type": "task", "task_id": "task-9", "status": "failed", "created_at": 4_000],
            ["id": "f3", "item_type": "data_delivery", "title": "Report shipped", "summary": "Weekly", "status": "info", "updated_at": 3_000]
        ]])).items
        let lines = feed.map { TodayMorningEdition.wireItem(fromFeed: $0) }
        XCTAssertEqual(lines.map(\.id), ["feed-f1", "feed-f2", "feed-f3"])
        XCTAssertEqual(lines.map(\.kind), [.insight, .activity, .activity])
        XCTAssertEqual(lines[0].title, "Distilled Memory")
        XCTAssertEqual(lines[0].summary, "Feed insight recorded")
        XCTAssertEqual(lines[0].severity, .success)
        XCTAssertEqual(lines[1].title, "Fleet Activity")
        XCTAssertEqual(lines[1].summary, "Task task-9")
        XCTAssertEqual(lines[1].severity, .error)
        XCTAssertEqual(lines[1].timestamp, 4_000)
        XCTAssertEqual(lines[1].taskID, "task-9")
        XCTAssertEqual(lines[2].badge, "data delivery")
        XCTAssertEqual(lines[2].title, "Report shipped")
        // The Activity sheet still reads a titled row.
        XCTAssertEqual(feed[0].displayTitle, "Activity")

        let updates = try JSONDecoder().decode(TodayAgentUpdatesResponse.self, from: jsonData(["events": [
            ["id": "u1", "agent_id": "presto", "kind": "cycle_completed", "ts": 9_000, "focus_area": "Inbox", "reason": "ignored"],
            ["id": 7, "kind": "cycle_failed", "ts": 8_000, "error": "Timed out", "outcome": "failed", "thread_id": "th-1"],
            ["id": "u3", "agent_id": "scout", "kind": "goal_set"]
        ]])).events
        let agentLines = updates.map { TodayMorningEdition.wireItem(fromAgentUpdate: $0, now: Date(timeIntervalSince1970: 100)) }
        XCTAssertEqual(agentLines.map(\.id), ["agent-update-u1", "agent-update-7", "agent-update-u3"])
        XCTAssertEqual(agentLines[0].title, "Presto: cycle completed")
        XCTAssertEqual(agentLines[0].summary, "Inbox")
        XCTAssertEqual(agentLines[1].title, "Fleet Agent: cycle failed")
        XCTAssertEqual(agentLines[1].summary, "Timed out")
        XCTAssertEqual(agentLines[1].severity, .error)
        XCTAssertEqual(agentLines[1].threadID, "th-1")
        XCTAssertEqual(agentLines[2].summary, "Autonomous agent cycle logged")
        XCTAssertEqual(agentLines[2].timestamp, 100_000)
        XCTAssertTrue(agentLines.allSatisfy { $0.kind == .activity })
    }

    func testRealtimeFramesBecomeEventLinesAndControlFramesDoNot() throws {
        let failed = try XCTUnwrap(TodayMorningEdition.wireItem(fromRealtimeText: json([
            "event_type": "ExecutionFailed",
            "data": ["task_id": "task-3", "error": "Tool crashed", "timestamp": 1_783_900_800_000]
        ]), sequence: 1))
        XCTAssertEqual(failed.kind, .event)
        XCTAssertEqual(failed.title, "Execution Failed")
        XCTAssertEqual(failed.summary, "Tool crashed")
        XCTAssertEqual(failed.severity, .error)
        XCTAssertEqual(failed.taskID, "task-3")
        XCTAssertEqual(failed.timestamp, 1_783_900_800_000)

        let cycle = try XCTUnwrap(TodayMorningEdition.wireItem(fromRealtimeText: json([
            "event_type": "AgentEvent",
            "data": ["event": ["event_type": "agent.cycle.completed", "agent_id": "night-owl",
                               "payload": ["outcome": "completed", "summary": "Swept the inbox"]]]
        ]), sequence: 2, now: Date(timeIntervalSince1970: 50)))
        XCTAssertEqual(cycle.title, "Night Owl: Cycle Completed")
        XCTAssertEqual(cycle.summary, "Swept the inbox")
        XCTAssertEqual(cycle.severity, .success)
        XCTAssertEqual(cycle.timestamp, 50_000)

        let titled = try XCTUnwrap(TodayMorningEdition.wireItem(fromRealtimeText: json([
            "type": "published.surface", "payload": ["title": "Morning briefing published", "message": "Ready"]
        ]), sequence: 3))
        XCTAssertEqual(titled.title, "Morning briefing published")
        XCTAssertEqual(titled.summary, "Ready")
        XCTAssertNotEqual(cycle.id, titled.id)

        // The dominant frame shape on /v2/realtime/ws (RuntimeTransportEvent,
        // tag=event_type, content=data): the wire must unwrap it, never show
        // a bare "Agent Event".
        let toolFrame = #"{"event_type":"AgentEvent","data":{"event":{"event_type":"tool.call.started","agent_id":"personal-assistant","payload":{"tool_name":"web_search"},"timestamp":1783900900000}}}"#
        let tool = try XCTUnwrap(TodayMorningEdition.wireItem(fromRealtimeText: toolFrame, sequence: 7))
        XCTAssertEqual(tool.title, "Personal Assistant: Tool: Call Started")
        XCTAssertEqual(tool.timestamp, 1_783_900_900_000)
        XCTAssertEqual(tool.badge, "tool")
        XCTAssertEqual(tool.kind, .event)
        XCTAssertNil(TodayMorningEdition.wireItem(fromRealtimeText: #"{"event_type":"AgentEvent","data":{"event":{"event_type":"agent.ui.delta"}}}"#, sequence: 8))

        XCTAssertNil(TodayMorningEdition.wireItem(fromRealtimeText: json(["type": "ping"]), sequence: 4))
        XCTAssertNil(TodayMorningEdition.wireItem(fromRealtimeText: json(["event_type": "__events_lagged"]), sequence: 5))
        XCTAssertNil(TodayMorningEdition.wireItem(fromRealtimeText: "not json", sequence: 6))
    }

    func testWireMergeIsNewestFirstDedupedAndCapped() {
        func line(_ id: String, _ at: Int64, title: String = "t") -> TodayWireItem {
            TodayWireItem(id: id, kind: .event, title: title, summary: "", timestamp: at, badge: "",
                          severity: .info, taskID: nil, threadID: nil)
        }
        let merged = TodayMorningEdition.mergingWireItems([line("a", 1), line("b", 3)],
                                                          [line("a", 5, title: "fresh"), line("c", 2)])
        XCTAssertEqual(merged.map(\.id), ["a", "b", "c"])
        XCTAssertEqual(merged.first?.title, "fresh")
        let flood = (0..<80).map { line("e\($0)", Int64($0)) }
        let capped = TodayMorningEdition.mergingWireItems([], flood)
        XCTAssertEqual(capped.count, 50)
        XCTAssertEqual(capped.first?.id, "e79")
        XCTAssertEqual(TodayMorningEdition.wireTimeAgo(0, now: Date(timeIntervalSince1970: 44)), "just now")
        XCTAssertEqual(TodayMorningEdition.wireTimeAgo(0, now: Date(timeIntervalSince1970: 125)), "2m ago")
        XCTAssertEqual(TodayMorningEdition.wireTimeAgo(0, now: Date(timeIntervalSince1970: 7_300)), "2h ago")
        XCTAssertEqual(TodayMorningEdition.wireTimeAgo(0, now: Date(timeIntervalSince1970: 200_000)), "2d ago")
    }

    // MARK: Morning Brief deck

    func testDeckInterleavesTwoFollowUpsThenOneWorthCard() throws {
        let follow = try (1...5).map { try followUp("f\($0)") }
        let worth = try (1...2).map { try worthCard("w\($0)") }
        XCTAssertEqual(TodayMorningEdition.interleaveDeck(followUps: follow, worth: worth).map(\.id), [
            "followup:f1", "followup:f2", "worth:w1", "followup:f3", "followup:f4", "worth:w2", "followup:f5"
        ])
        XCTAssertEqual(TodayMorningEdition.interleaveDeck(followUps: [], worth: worth).map(\.id), ["worth:w1", "worth:w2"])
        let tail = TodayMorningEdition.interleaveDeck(followUps: [follow[0]], worth: try (1...3).map { try worthCard("w\($0)") })
        XCTAssertEqual(tail.map(\.id), ["followup:f1", "worth:w1", "worth:w2", "worth:w3"])
        XCTAssertEqual(TodayMorningEdition.deckCards(tail, in: .forYou).map(\.id), ["followup:f1"])
        XCTAssertEqual(TodayMorningEdition.deckCards(tail, in: .worth).count, 3)
        XCTAssertEqual(TodayMorningEdition.deckCountLabel(50), "50")
        XCTAssertEqual(TodayMorningEdition.deckCountLabel(51), "50+")
    }

    func testDeckCardCopyAndFallbacks() throws {
        let rich = TodayDeckCard(source: .followUp(try followUp("f1", extra: [
            "provider": "gmail", "subject": "Client reply", "sender": "Alex", "summary": "", "reason": "Direct question"
        ])))
        XCTAssertEqual(rich.category, "DISPATCH · GMAIL")
        XCTAssertEqual(rich.title, "Client reply")
        XCTAssertEqual(rich.summary, "Direct question")
        XCTAssertEqual(rich.sender, "Alex")
        XCTAssertEqual(rich.primaryLabel, "Do it")
        let bare = TodayDeckCard(source: .followUp(try followUp("f2", extra: ["provider": ""])))
        XCTAssertEqual(bare.category, "DISPATCH · CORRESPONDENCE")
        XCTAssertEqual(bare.title, "Untitled Message")
        XCTAssertEqual(bare.summary, "No preview available")

        let worth = TodayDeckCard(source: .worth(try worthCard("w1", extra: [
            "source_kind": "project_note", "source_title": "", "line": "Contract terms", "why_now": "Renews soon"
        ])))
        XCTAssertEqual(worth.category, "READING ROOM · PROJECT NOTE")
        XCTAssertEqual(worth.title, "Contract terms")
        XCTAssertEqual(worth.summary, "Renews soon")
        XCTAssertEqual(worth.primaryLabel, "Open")
        let empty = TodayDeckCard(source: .worth(try worthCard("w2", extra: ["source_kind": "", "line": ""])))
        XCTAssertEqual(empty.category, "READING ROOM · NOTE")
        XCTAssertEqual(empty.title, "Resurfaced Note")
        XCTAssertEqual(empty.summary, "Resurfaced for your attention")
    }

    func testDeckReleaseThresholdsAndStampInk() {
        func release(_ dx: CGFloat, _ dy: CGFloat, predicted: CGSize? = nil) -> TodayDeckSwipe? {
            TodayMorningEdition.deckRelease(translation: CGSize(width: dx, height: dy),
                                            predicted: predicted ?? CGSize(width: dx, height: dy))
        }
        XCTAssertEqual(release(96, 0), .useful)
        XCTAssertNil(release(95, 0))
        XCTAssertEqual(release(-96, 0), .dismiss)
        XCTAssertEqual(release(100, -60), .useful, "a mostly-horizontal drag still triages")
        // Vertical drags belong to the page scroll and NEVER triage — not
        // even far past the old 80pt Seen threshold, nor with a sideways drift.
        XCTAssertNil(release(0, -300))
        XCTAssertNil(release(0, 200))
        XCTAssertNil(release(100, -200), "vertical-dominant: a scroll, not a Useful")
        XCTAssertNil(release(30, -40, predicted: CGSize(width: 400, height: -900)))
        XCTAssertFalse(TodayMorningEdition.isHorizontalDeckDrag(CGSize(width: 10, height: -10)))
        XCTAssertTrue(TodayMorningEdition.isHorizontalDeckDrag(CGSize(width: 12, height: -4)))
        XCTAssertEqual(release(40, 0, predicted: CGSize(width: 300, height: 20)), .useful, "a fast flick commits")
        XCTAssertEqual(release(-30, 5, predicted: CGSize(width: -260, height: 40)), .dismiss)
        XCTAssertNil(release(40, 10, predicted: CGSize(width: 90, height: 10)))

        XCTAssertEqual(TodayMorningEdition.usefulStampOpacity(dx: 25), 0)
        XCTAssertEqual(TodayMorningEdition.usefulStampOpacity(dx: 62.5), 0.5, accuracy: 0.0001)
        XCTAssertEqual(TodayMorningEdition.usefulStampOpacity(dx: 300), 1)
        XCTAssertEqual(TodayMorningEdition.dismissStampOpacity(dx: -100), 1)
        XCTAssertEqual(TodayMorningEdition.dismissStampOpacity(dx: 100), 0)
    }

    // MARK: Economics of Operations

    func testSpendFormattingDeltaAndInvertedTone() {
        XCTAssertEqual(TodayMorningEdition.formatSpend(0), "$0.00")
        XCTAssertEqual(TodayMorningEdition.formatSpend(12.34), "$12.34")
        XCTAssertEqual(TodayMorningEdition.formatSpend(123.4), "$123")
        XCTAssertEqual(TodayMorningEdition.spendFigureParts(1.42).dollars, "1")
        XCTAssertEqual(TodayMorningEdition.spendFigureParts(1.42).cents, ".42")
        XCTAssertEqual(TodayMorningEdition.spendFigureParts(123).cents, "")

        XCTAssertEqual(TodayMorningEdition.formatSpendDelta(today: 0, yesterday: 0), "")
        XCTAssertEqual(TodayMorningEdition.formatSpendDelta(today: 1, yesterday: 0), "new today")
        XCTAssertEqual(TodayMorningEdition.formatSpendDelta(today: 1.004, yesterday: 1), "")
        XCTAssertEqual(TodayMorningEdition.formatSpendDelta(today: 1.42, yesterday: 0.91), "+$0.51")
        XCTAssertEqual(TodayMorningEdition.formatSpendDelta(today: 0.5, yesterday: 1.25), "\u{2212}$0.75")

        XCTAssertEqual(TodayMorningEdition.spendTone(today: 0, yesterday: 0), .neutral)
        XCTAssertEqual(TodayMorningEdition.spendTone(today: 1, yesterday: 0), .bad)
        XCTAssertEqual(TodayMorningEdition.spendTone(today: 1.004, yesterday: 1), .neutral)
        XCTAssertEqual(TodayMorningEdition.spendTone(today: 2, yesterday: 1), .bad)
        XCTAssertEqual(TodayMorningEdition.spendTone(today: 1, yesterday: 2), .good)

        XCTAssertEqual(TodayMorningEdition.spendDeltaLine(today: 0, yesterday: 0), "steady vs $0.00 yday")
        XCTAssertEqual(TodayMorningEdition.spendDeltaLine(today: 1.42, yesterday: 0.91), "+$0.51 vs $0.91 yday")
        XCTAssertEqual(TodayMorningEdition.providerSharePercent(0.72), "72")
        XCTAssertEqual(TodayMorningEdition.providerSharePercent(1.0 / 3.0), "33.33")
        XCTAssertEqual(TodayMorningEdition.hourLabel(0), "12a")
        XCTAssertEqual(TodayMorningEdition.hourLabel(9), "9a")
        XCTAssertEqual(TodayMorningEdition.hourLabel(12), "12p")
        XCTAssertEqual(TodayMorningEdition.hourDetail(hour: 15, spend: 0.42, calls: 12), "3p: $0.42 (12 calls)")
        XCTAssertEqual(TodayMorningEdition.hourDetail(hour: 1, spend: 0, calls: 1), "1a: $0.00 (1 call)")
    }

    func testPieSharesAndAgentCounts() {
        XCTAssertEqual(TodayMorningEdition.pieShares(succeeded: 0, failed: 0, inFlight: 0),
                       TodayTaskShares(succeeded: 0, failed: 0, inFlight: 0))
        XCTAssertEqual(TodayMorningEdition.pieShares(succeeded: 6, failed: 1, inFlight: 2),
                       TodayTaskShares(succeeded: 67, failed: 11, inFlight: 22))
        XCTAssertEqual(TodayMorningEdition.pieShares(succeeded: 1, failed: 1, inFlight: 1),
                       TodayTaskShares(succeeded: 33, failed: 33, inFlight: 34))
        XCTAssertEqual(TodayMorningEdition.pieShares(succeeded: 1, failed: 1, inFlight: 0),
                       TodayTaskShares(succeeded: 50, failed: 50, inFlight: 0))
        let counts = TodayMorningEdition.agentCounts([
            TodayAgentSummary(status: "running"), TodayAgentSummary(status: "triggered"),
            TodayAgentSummary(status: "idle", disabled: true), TodayAgentSummary(status: "disabled"),
            TodayAgentSummary(status: "idle")
        ])
        XCTAssertEqual(counts, TodayAgentCounts(total: 5, enabled: 3, active: 2))
    }

    // MARK: Broadsheet paging

    func testPageWindowMath() {
        let middle = TodayPageWindow(page: 2, pageSize: 5, total: 23, loaded: 5)
        XCTAssertEqual(middle.pageCount, 5)
        XCTAssertEqual(middle.rangeLabel, "6–10 of 23")
        XCTAssertEqual(middle.pageLabel, "2 / 5")
        XCTAssertTrue(middle.hasPrevious)
        XCTAssertTrue(middle.hasNext)
        let last = TodayPageWindow(page: 5, pageSize: 5, total: 23, loaded: 3)
        XCTAssertEqual(last.rangeLabel, "21–23 of 23")
        XCTAssertFalse(last.hasNext)
        let empty = TodayPageWindow(page: 1, pageSize: 5, total: 0, loaded: 0)
        XCTAssertEqual(empty.rangeLabel, "0 of 0")
        XCTAssertEqual(empty.pageLabel, "1 / 1")
        XCTAssertFalse(empty.hasPrevious)
        XCTAssertFalse(empty.hasNext)
        // Optimistic removal: one fewer row on the page and in the total.
        let afterRemoval = TodayPageWindow(page: 2, pageSize: 5, total: 22, loaded: 4)
        XCTAssertEqual(afterRemoval.rangeLabel, "6–9 of 22")
        XCTAssertEqual(TodayPageWindow(page: 1, pageSize: 5, total: 10, loaded: 5).pageCount, 2)
    }

    func testFollowUpCursorWalkReachesTargetOrLastReachablePage() async {
        var fetched: [String?] = []
        let chain: [String?: String?] = [nil: "c2", "c2": "c3", "c3": nil]
        let fetch: (String?) async throws -> (nextCursor: String?, hasMore: Bool) = { cursor in
            fetched.append(cursor)
            let next = chain[cursor] ?? nil
            return (next, next != nil)
        }
        let first = await TodayMorningEdition.walkFollowUpCursor(to: 1, known: [:], fetch: fetch)
        XCTAssertEqual(first.page, 1)
        XCTAssertNil(first.cursor)
        XCTAssertTrue(fetched.isEmpty)

        let third = await TodayMorningEdition.walkFollowUpCursor(to: 3, known: [1: nil], fetch: fetch)
        XCTAssertEqual(third.page, 3)
        XCTAssertEqual(third.cursor, "c3")
        XCTAssertEqual(fetched, [nil, "c2"])
        XCTAssertEqual(third.known[2], .some("c2"))

        fetched.removeAll()
        let fromKnown = await TodayMorningEdition.walkFollowUpCursor(to: 3, known: third.known, fetch: fetch)
        XCTAssertEqual(fromKnown.cursor, "c3")
        XCTAssertTrue(fetched.isEmpty, "a known page needs no walk")

        fetched.removeAll()
        let beyond = await TodayMorningEdition.walkFollowUpCursor(to: 7, known: third.known, fetch: fetch)
        XCTAssertEqual(beyond.page, 3, "cursors ran out: land on the last reachable page")
        XCTAssertEqual(beyond.cursor, "c3")
        XCTAssertEqual(fetched, ["c3"], "the walk resumes from the nearest known page")
    }

    @MainActor
    func testBroadsheetPagesWorthByOffsetAndFollowUpsByCursorWithOptimisticRemoval() throws {
        let sut = loadedSUT(followUps: ["items": [], "total": 0], resurfacing: ["cards": [], "total": 0])
        let lock = NSLock()
        var queries: [String] = []
        MockURLProtocol.handler = { request in
            let query = request.url!.query ?? ""
            lock.lock(); queries.append("\(request.url!.path)?\(query)"); lock.unlock()
            switch request.url!.path {
            case "/api/magician/v2/channel-assist/resurfacing/today":
                XCTAssertFalse(query.contains("cursor"), "worth pages by offset only")
                return (response(for: request), jsonData(["cards": [
                    ["candidate_id": "r6", "line": "Six", "source_kind": "memory"],
                    ["candidate_id": "r7", "line": "Seven", "source_kind": "memory"]
                ], "total": 7]))
            case "/api/magician/v2/channel-assist/follow-ups":
                if query.contains("cursor=p2") {
                    return (response(for: request), jsonData(["items": [
                        ["annotation_id": "f6", "provider": "gmail", "created_at": 6]
                    ], "total": 6, "has_more": false]))
                }
                return (response(for: request), jsonData(["items": (1...5).map {
                    ["annotation_id": "f\($0)", "provider": "gmail", "created_at": $0]
                }, "total": 6, "has_more": true, "next_cursor": "p2"]))
            case "/api/magician/v2/channel-assist/annotations/f6/useful":
                return (response(for: request), jsonData([:]))
            default:
                XCTFail("Unexpected broadsheet request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
        XCTAssertFalse(sut.isBroadsheetLoaded(.worth))
        waitForBroadsheet(sut, .worth) { sut.loadBroadsheetPage(.worth, page: 2) }
        XCTAssertEqual(sut.broadsheetWorth.map(\.id), ["r6", "r7"])
        XCTAssertEqual(sut.broadsheetWindow(.worth).rangeLabel, "6–7 of 7")
        lock.lock(); XCTAssertTrue(queries.last?.contains("limit=5") == true && queries.last?.contains("offset=5") == true); lock.unlock()

        waitForBroadsheet(sut, .forYou) { sut.loadBroadsheetPage(.forYou, page: 2) }
        XCTAssertEqual(sut.broadsheetFollowUps.map(\.id), ["f6"])
        XCTAssertEqual(sut.broadsheetFollowUpPage, 2)
        XCTAssertEqual(sut.broadsheetWindow(.forYou).pageLabel, "2 / 2")

        // A card only on the broadsheet page resolves optimistically and the
        // page re-reads itself afterwards.
        let f6 = try XCTUnwrap(sut.broadsheetFollowUps.first)
        let done = expectation(description: "resolved")
        sut.resolveFollowUp(f6, action: "useful") { ok in XCTAssertTrue(ok); done.fulfill() }
        XCTAssertTrue(sut.broadsheetFollowUps.isEmpty)
        XCTAssertEqual(sut.broadsheetFollowUpTotal, 5)
        wait(for: [done], timeout: 3)
        // The completed action re-reads the current page to backfill it.
        XCTAssertTrue(sut.broadsheetLoading.contains(.forYou))
        waitForBroadsheet(sut, .forYou) {}
        lock.lock(); let last = queries.last; lock.unlock()
        XCTAssertTrue(last?.contains("cursor=p2") == true)
    }

    // MARK: Operations carousel

    func testCarouselAutoAdvanceRules() {
        let start = Date(timeIntervalSince1970: 1_000)
        XCTAssertEqual(TodayCarouselAutoAdvance.nextIndex(after: 0, count: 2), 1)
        XCTAssertEqual(TodayCarouselAutoAdvance.nextIndex(after: 1, count: 2), 0, "wraps to the first slide")
        XCTAssertFalse(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(7.9), lastAdvance: start,
                                                             pausedUntil: nil, reduceMotion: false, slideCount: 2))
        XCTAssertTrue(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(8), lastAdvance: start,
                                                            pausedUntil: nil, reduceMotion: false, slideCount: 2))
        let paused = TodayCarouselAutoAdvance.pauseUntil(afterInteractionAt: start)
        XCTAssertEqual(paused, start.addingTimeInterval(15))
        XCTAssertFalse(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(14), lastAdvance: start,
                                                             pausedUntil: paused, reduceMotion: false, slideCount: 2))
        XCTAssertTrue(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(15), lastAdvance: start,
                                                            pausedUntil: paused, reduceMotion: false, slideCount: 2))
        XCTAssertFalse(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(60), lastAdvance: start,
                                                             pausedUntil: nil, reduceMotion: true, slideCount: 2),
                       "no auto-advance under Reduce Motion")
        XCTAssertFalse(TodayCarouselAutoAdvance.shouldAdvance(now: start.addingTimeInterval(60), lastAdvance: start,
                                                             pausedUntil: nil, reduceMotion: false, slideCount: 1))
        XCTAssertEqual(TodayCarouselAutoAdvance.dotLabel(index: 1, count: 2, title: "State of Operations"),
                       "Slide 2 of 2, State of Operations")
    }

    func testRecentTasksAndShortRelativeTime() {
        let tasks = (0..<25).map { TodayRecentTask(id: "t\($0)", title: "T\($0)", status: "running", updatedAt: Int64($0) * 1_000) }
            + [TodayRecentTask(id: "unknown", title: "No time", status: "ready", updatedAt: 0)]
        let recent = TodayMorningEdition.recentTasks(tasks)
        XCTAssertEqual(recent.count, 20)
        XCTAssertEqual(recent.first?.id, "t24")
        XCTAssertFalse(recent.contains { $0.id == "unknown" })
        let now = Date(timeIntervalSince1970: 100_000)
        XCTAssertEqual(TodayMorningEdition.shortRelativeTime(99_990_000, now: now), "now")
        XCTAssertEqual(TodayMorningEdition.shortRelativeTime(99_760_000, now: now), "4m")
        XCTAssertEqual(TodayMorningEdition.shortRelativeTime(92_800_000, now: now), "2h")
        XCTAssertEqual(TodayMorningEdition.shortRelativeTime(10_000_000, now: now), "1d")
        XCTAssertEqual(TodayMorningEdition.shortRelativeTime(0, now: now), "")
    }

    // MARK: Economics stats & State of the Crew

    func testPerCallFormattingAndPeakHour() {
        XCTAssertEqual(TodayMorningEdition.formatPerCall(0.0016), "$0.0016")
        XCTAssertEqual(TodayMorningEdition.formatPerCall(0.063), "$0.06")
        XCTAssertEqual(TodayMorningEdition.formatPerCall(0), "$0.00")
        XCTAssertEqual(TodayMorningEdition.averagePerCall(spend: 1.42, calls: 18), "$0.08")
        XCTAssertEqual(TodayMorningEdition.averagePerCall(spend: 0.029, calls: 18), "$0.0016")
        XCTAssertEqual(TodayMorningEdition.averagePerCall(spend: 1, calls: 0), "\u{2014}")
        var hours = Array(repeating: 0.0, count: 24)
        XCTAssertEqual(TodayMorningEdition.peakHour(hours), "\u{2014}")
        hours[9] = 0.77; hours[15] = 0.9
        XCTAssertEqual(TodayMorningEdition.peakHour(hours), "3p")
        hours[0] = 2
        XCTAssertEqual(TodayMorningEdition.peakHour(hours), "12a")
    }

    func testCrewSummaryJoinsAgentsUsageAndWindowedTasks() {
        let since: Int64 = 1_000_000
        let agents = [
            TodayAgentSummary(status: "idle", agentID: "pilot", name: "Pilot"),
            TodayAgentSummary(status: "running", agentID: "scout", name: "Scout"),
            TodayAgentSummary(status: "idle", agentID: "sleeper", name: "Sleeper"),
            TodayAgentSummary(status: "disabled", disabled: true, agentID: "off", name: "Off")
        ]
        let llm = [
            TodayCrewLLMRow(agentID: "pilot", calls: 22, costUSD: 0.063, okCalls: 22),
            TodayCrewLLMRow(agentID: "ghost", calls: 4, costUSD: 0.01, okCalls: 2)
        ]
        let tasks = [
            TodayCrewTaskRow(agentID: "pilot", status: "completed", updatedAt: since + 10),
            TodayCrewTaskRow(agentID: "pilot", status: "failed", updatedAt: since + 20),
            TodayCrewTaskRow(agentID: "pilot", status: "completed", updatedAt: since - 1),
            TodayCrewTaskRow(agentID: "sleeper", status: "done", updatedAt: since + 5)
        ]
        let crew = TodayMorningEdition.crewSummary(agents: agents, llm: llm, tasks: tasks, since: since)
        XCTAssertEqual(crew.members.map(\.id), ["scout", "pilot", "ghost", "sleeper"],
                       "active first, then cost, then tasks; idle agents without activity are left out")
        let pilot = crew.members[1]
        XCTAssertEqual(pilot.name, "Pilot")
        XCTAssertEqual(pilot.tasksDone, 1, "rows before the 24 h window are ignored")
        XCTAssertEqual(pilot.successPercent, 50)
        XCTAssertEqual(pilot.reliabilityPercent, 100)
        XCTAssertNil(crew.members[0].successPercent, "— without done or failed tasks")
        XCTAssertNil(crew.members[0].reliabilityPercent, "— without calls")
        XCTAssertEqual(crew.members[2].reliabilityPercent, 50)
        XCTAssertEqual(crew.activeAgents, 1)
        XCTAssertEqual(crew.totalAgents, 4)
        XCTAssertEqual(crew.costUSD, 0.073, accuracy: 0.00001)
        XCTAssertEqual(crew.tasksDone, 2)
        XCTAssertEqual(crew.reliabilityPercent, 92)

        let resting = TodayMorningEdition.crewSummary(agents: [agents[0]], llm: [], tasks: [], since: since)
        XCTAssertTrue(resting.members.isEmpty)
        XCTAssertNil(resting.reliabilityPercent)
        XCTAssertEqual(TodayMorningEdition.percentTone(95), .good)
        XCTAssertEqual(TodayMorningEdition.percentTone(80), .neutral)
        XCTAssertEqual(TodayMorningEdition.percentTone(79), .bad)
    }

    func testCrewSQLAndTaskPageWalkStopCondition() throws {
        let sql = TodayMorningEdition.crewSQL(since: 42)
        XCTAssertTrue(sql.contains("WHERE timestamp_ms >= 42"))
        XCTAssertTrue(sql.contains("SUM(CASE WHEN success THEN 1 ELSE 0 END) AS ok_calls"))
        XCTAssertTrue(sql.hasSuffix("GROUP BY agent_id ORDER BY cost_usd DESC"))
        XCTAssertTrue(TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: 50, since: 42, nextCursor: "c", pagesFetched: 1))
        XCTAssertFalse(TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: 41, since: 42, nextCursor: "c", pagesFetched: 1),
                       "the page already reached past the window")
        XCTAssertFalse(TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: 50, since: 42, nextCursor: nil, pagesFetched: 1))
        XCTAssertFalse(TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: 50, since: 42, nextCursor: "c", pagesFetched: 5),
                       "at most five pages")
        XCTAssertFalse(TodayMorningEdition.shouldFetchNextCrewTaskPage(lastRowUpdatedAt: nil, since: 42, nextCursor: "c", pagesFetched: 1))

        let agent = try JSONDecoder().decode(TodayAgentSummary.self, from: jsonData([
            "status": "running", "definition": ["agent_id": "pilot", "name": "Pilot"]
        ]))
        XCTAssertEqual(agent.agentID, "pilot")
        XCTAssertEqual(agent.name, "Pilot")
    }

    // MARK: View model

    @MainActor
    func testFetchBuildsWireLedgerBucketsAndAgentsFromRealSources() throws {
        let feedItems: [[String: Any]] = (0..<20).map { index in
            ["id": "feed-\(index)", "item_type": index == 0 ? "agent_learning" : "task",
             "title": "Row \(index)", "status": "done", "updated_at": 10_000 - index]
        }
        let lock = NSLock()
        var countSQL: String?
        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/today":
                XCTAssertTrue(request.url!.query!.contains("digest_limit=6"), "§ 5 pages the digest six at a time")
                return (response(for: request), jsonData(Self.todayBody()))
            case "/api/magician/v2/today/visibility":
                return (response(for: request), jsonData(["items": []]))
            case "/api/magician/v2/channel-assist/resurfacing/today":
                return (response(for: request), jsonData(["cards": [], "total": 0]))
            case "/api/magician/v2/channel-assist/follow-ups":
                return (response(for: request), jsonData(["items": [], "total": 0]))
            case "/api/magician/v2/feed":
                XCTAssertEqual(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
                    .queryItems?.first(where: { $0.name == "limit" })?.value, "80",
                               "the wire reuses the Activity sheet's feed read")
                return (response(for: request), jsonData(["items": feedItems]))
            case "/api/magician/v3/published-surfaces/projections":
                return (response(for: request), jsonData(["surfaces": []]))
            case "/api/magician/v2/analytics/llm_calls/query":
                return (response(for: request), jsonData([
                    "columns": ["section", "k", "model", "v1", "v2"],
                    "rows": [["today_hour", "9", NSNull(), 0.42, 12], ["today_total", "all", NSNull(), 0.42, 12],
                             ["yesterday_total", "all", NSNull(), 0, 0]]
                ]))
            case "/api/magician/v2/analytics/query":
                let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                let sql = body?["sql"] as? String ?? ""
                if sql.contains("total_24h") {
                    lock.lock(); countSQL = sql; lock.unlock()
                    return (response(for: request), jsonData(["columns": ["total_24h"], "rows": [[35_086]]]))
                }
                return (response(for: request), jsonData(["columns": ["section", "n"], "rows": []]))
            case "/api/magician/v2/analytics/memory_events/query":
                return (response(for: request), jsonData(["columns": ["section", "n", "passes"], "rows": []]))
            case "/api/magician/v3/tasks":
                return (response(for: request), jsonData(["tasks": [
                    ["status": "completed"], ["status": "completed"], ["status": "failed"],
                    ["status": "running"], ["status": "paused"], ["status": "planning"],
                    ["status": "ready", "task_id": "t-new", "title": "Newest", "updated_at": "2026-09-28T06:00:00Z"],
                    ["status": "failed", "task_id": "t-old", "title": "Older", "updated_at": "2026-09-27T06:00:00Z"]
                ]]))
            case "/api/magician/v2/agents":
                return (response(for: request), jsonData(["agents": [
                    ["status": "running"], ["status": "idle"], ["status": "idle", "disabled": true]
                ], "system_agents": [["status": "running"]]]))
            case "/api/magician/v2/agents/updates":
                return (response(for: request), jsonData(["events": [
                    ["id": "u1", "agent_id": "presto", "kind": "cycle_completed", "ts": 20_000]
                ]]))
            default:
                XCTFail("Unexpected request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
        let sut = loadedSUT()

        XCTAssertEqual(sut.wireEventCount24h, 35_086)
        lock.lock(); let sql = countSQL; lock.unlock()
        XCTAssertTrue(sql?.hasPrefix("SELECT COUNT(*) AS total_24h FROM events WHERE epoch_ms(timestamp) >= ") == true)
        XCTAssertEqual(sut.wireItems.count, 16, "15 newest feed rows + 1 agent update")
        XCTAssertEqual(sut.wireItems.first?.id, "agent-update-u1")
        XCTAssertEqual(sut.wireItems[1].kind, .insight)
        XCTAssertFalse(sut.wireItems.contains { $0.id == "feed-feed-15" })

        let pulse = try XCTUnwrap(sut.pulse)
        XCTAssertEqual(pulse.hourlyCalls[9], 12)
        XCTAssertEqual(pulse.hourlySpend[9], 0.42, accuracy: 0.0001)
        XCTAssertEqual(pulse.tasksSucceeded, 2)
        XCTAssertEqual(pulse.tasksFailed, 2)
        XCTAssertEqual(pulse.recentTasks.map(\.id), ["t-new", "t-old"], "State of Operations lists the same read, newest first")
        XCTAssertEqual(pulse.tasksInFlight, 3)
        XCTAssertEqual(pulse.agents, TodayAgentCounts(total: 3, enabled: 2, active: 1))
        XCTAssertNotNil(pulse.crew, "the crew joins the same agents read")
        XCTAssertNil(pulse.crewError)
    }

    @MainActor
    func testWireSourcesFailSoftAndKeepTheLastCount() {
        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/today": return (response(for: request), jsonData(Self.todayBody()))
            case "/api/magician/v2/agents/updates", "/api/magician/v2/agents", "/api/magician/v2/analytics/query":
                return (response(for: request, status: 503), Data("down".utf8))
            case "/api/magician/v2/feed": return (response(for: request), jsonData(["items": []]))
            case "/api/magician/v2/today/visibility": return (response(for: request), jsonData(["items": []]))
            case "/api/magician/v2/channel-assist/resurfacing/today": return (response(for: request), jsonData(["cards": [], "total": 0]))
            case "/api/magician/v2/channel-assist/follow-ups": return (response(for: request), jsonData(["items": [], "total": 0]))
            case "/api/magician/v3/published-surfaces/projections": return (response(for: request), jsonData(["surfaces": []]))
            case "/api/magician/v2/analytics/llm_calls/query":
                return (response(for: request), jsonData(["columns": ["section", "k", "model", "v1", "v2"], "rows": []]))
            case "/api/magician/v2/analytics/memory_events/query":
                return (response(for: request), jsonData(["columns": ["section", "n", "passes"], "rows": []]))
            case "/api/magician/v3/tasks": return (response(for: request), jsonData(["tasks": []]))
            default: return (response(for: request, status: 404), Data())
            }
        }
        let sut = loadedSUT()
        XCTAssertEqual(sut.wireEventCount24h, 0, "a failed count query never invents a number")
        XCTAssertTrue(sut.wireItems.isEmpty, "no placeholder transmissions")
        XCTAssertNil(sut.pulse?.agents)
        XCTAssertNil(sut.error)

        sut.receiveWireEvent(#"{"event_type":"TaskCreated","data":{"task_id":"t-1","timestamp":1783900800000}}"#)
        sut.receiveWireEvent(#"{"type":"pong"}"#)
        sut.receiveWireEvent(#"{"event_type":"AgentEvent","data":{"event":{"event_type":"tool.call.started","agent_id":"personal-assistant","payload":{},"timestamp":1783900700000}}}"#)
        XCTAssertEqual(sut.wireEventCount24h, 2)
        XCTAssertEqual(sut.wireItems.map(\.title), ["Task Created", "Personal Assistant: Tool: Call Started"])
        XCTAssertEqual(sut.wireItems.first?.summary, "Task t-1")
    }

    @MainActor
    func testReadingRoomModePersistsPerDevice() {
        let suite = "today-morning-edition-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let first = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!,
                                   defaults: defaults)
        XCTAssertEqual(first.readingRoomMode, .deck, "Morning Brief is the default")
        first.readingRoomMode = .broadsheet
        let second = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!,
                                    defaults: defaults)
        XCTAssertEqual(second.readingRoomMode, .broadsheet)
        defaults.set("bogus", forKey: TodayViewModel.readingRoomModeKey)
        let third = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!,
                                   defaults: defaults)
        XCTAssertEqual(third.readingRoomMode, .deck)
    }

    @MainActor
    func testDeckLoadMorePullsBothSourcesThroughTheirCursorsOnlyWhenMoreExists() {
        let sut = loadedSUT(
            followUps: ["items": [["annotation_id": "f1", "provider": "gmail", "created_at": 1]],
                        "total": 3, "next_cursor": "follow-cursor"],
            resurfacing: ["cards": [["candidate_id": "r1", "line": "One", "source_kind": "memory"]],
                          "total": 2, "next_cursor": ["surfaced_at": 5, "score": 0.5, "candidate_id": "r1"]]
        )
        XCTAssertTrue(sut.hasMoreMessageFollowUps)
        XCTAssertTrue(sut.hasMoreResurfacing)
        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/channel-assist/follow-ups":
                XCTAssertTrue(request.url!.query!.contains("cursor=follow-cursor"))
                return (response(for: request), jsonData(["items": [
                    ["annotation_id": "f2", "provider": "gmail", "created_at": 2]
                ], "total": 3, "next_cursor": NSNull()]))
            case "/api/magician/v2/channel-assist/resurfacing/today":
                XCTAssertTrue(request.url!.query!.contains("cursor_candidate_id=r1"))
                return (response(for: request), jsonData(["cards": [
                    ["candidate_id": "r2", "line": "Two", "source_kind": "memory"]
                ], "total": 2]))
            default:
                XCTFail("Unexpected deck page request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
        let done = expectation(description: "deck loaded more")
        sut.$isDeckLoadingMore.dropFirst().filter { !$0 }.prefix(1).sink { _ in done.fulfill() }.store(in: &cancellables)
        sut.loadMoreDeckCards()
        XCTAssertTrue(sut.isDeckLoadingMore)
        wait(for: [done], timeout: 3)
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f1", "f2"])
        XCTAssertEqual(sut.resurfacingCards.map(\.id), ["r1", "r2"])
        XCTAssertFalse(sut.hasMoreMessageFollowUps, "no cursor → no phantom next page")
        XCTAssertFalse(sut.hasMoreResurfacing)

        MockURLProtocol.handler = { request in
            XCTFail("Exhausted sources must not be re-read: \(request.url!.absoluteString)")
            return (response(for: request, status: 404), Data())
        }
        sut.loadMoreDeckCards()
        XCTAssertFalse(sut.isDeckLoadingMore)
    }

    @MainActor
    func testResolutionCompletionsReportSuccessAndRollback() throws {
        let sut = loadedSUT(
            followUps: ["items": [["annotation_id": "f1", "provider": "gmail", "created_at": 1]], "total": 1],
            resurfacing: ["cards": [["candidate_id": "r1", "line": "One", "source_kind": "memory"]], "total": 1]
        )
        let followUp = try XCTUnwrap(sut.messageFollowUps.first)
        let card = try XCTUnwrap(sut.resurfacingCards.first)

        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url!.path, "/api/magician/v2/channel-assist/annotations/f1/approve")
            return (response(for: request, status: 500), Data("approve failed".utf8))
        }
        let rolledBack = expectation(description: "follow-up rolled back")
        sut.resolveFollowUp(followUp, action: "approve") { accepted in
            XCTAssertFalse(accepted)
            rolledBack.fulfill()
        }
        XCTAssertTrue(sut.messageFollowUps.isEmpty, "optimistic removal")
        wait(for: [rolledBack], timeout: 3)
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f1"])
        XCTAssertTrue(sut.error?.contains("approve failed") == true, "a failed action is an error, never a success")

        MockURLProtocol.handler = { request in
            let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
            XCTAssertEqual(body["action"] as? String, "dismiss")
            XCTAssertEqual(body["reason"] as? String, "not_relevant")
            return (response(for: request), jsonData([:]))
        }
        let accepted = expectation(description: "resurfacing accepted")
        sut.resolveResurfacing(card, action: .dismiss, reason: "not_relevant") { ok in
            XCTAssertTrue(ok)
            accepted.fulfill()
        }
        wait(for: [accepted], timeout: 3)
        XCTAssertTrue(sut.resurfacingCards.isEmpty)
    }

    @MainActor
    func testResurfacingDropsReasonsOutsideItsVocabulary() throws {
        XCTAssertEqual(ResurfacingDismissOption.allowedCodes,
                       ["spam", "already_handled", "duplicate", "delegated", "not_relevant"])
        XCTAssertEqual(ResurfacingDismissOption.all.first?.code, nil)
        let sut = loadedSUT(resurfacing: ["cards": [["candidate_id": "r1", "line": "One", "source_kind": "memory"]], "total": 1])
        let card = try XCTUnwrap(sut.resurfacingCards.first)
        MockURLProtocol.handler = { request in
            let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
            XCTAssertNil(body["reason"], "wrong_classification is a follow-up reason, not a resurfacing one")
            return (response(for: request), jsonData([:]))
        }
        let done = expectation(description: "dismissed")
        sut.resolveResurfacing(card, action: .dismiss, reason: "wrong_classification") { _ in done.fulfill() }
        wait(for: [done], timeout: 3)
    }

    func testResurfacingCardCarriesItsOpenTargets() throws {
        let card = try worthCard("w1", extra: ["open_url": "https://example.com/doc", "source_route": "/tasks?selected=t1"])
        XCTAssertEqual(card.openURL, "https://example.com/doc")
        XCTAssertEqual(card.sourceRoute, "/tasks?selected=t1")
        XCTAssertNil(try worthCard("w2").openURL)
    }

    // MARK: Helpers

    private func utcCalendar() -> Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        return calendar
    }

    private func json(_ object: [String: Any]) -> String {
        String(data: jsonData(object), encoding: .utf8)!
    }

    private func followUp(_ id: String, extra: [String: Any] = [:]) throws -> ChannelFollowUp {
        var object: [String: Any] = ["annotation_id": id, "provider": "gmail", "created_at": 1]
        object.merge(extra) { _, new in new }
        return try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData(object))
    }

    private func worthCard(_ id: String, extra: [String: Any] = [:]) throws -> ResurfacingCard {
        var object: [String: Any] = ["candidate_id": id, "line": "Line \(id)", "source_kind": "memory"]
        object.merge(extra) { _, new in new }
        return try JSONDecoder().decode(ResurfacingCard.self, from: jsonData(object))
    }

    private static func todayBody() -> [String: Any] {
        [
            "generated_at": 1, "headline": "",
            "digest": ["generated_at": 1, "total": 0, "limit": 6, "offset": 0, "bullets": []],
            "sections": ["needs_you": [], "followups": [], "active_work": [], "delivered": [], "changed": []],
            "counts": ["needs_you": 0, "followups": 0, "active_work": 0, "delivered": 0, "changed": 0, "total": 0]
        ]
    }

    /// Loads a view model against a handler. When `followUps`/`resurfacing`
    /// are given, a default handler serves them; otherwise the handler
    /// already installed by the test is used as-is.
    @MainActor
    private func loadedSUT(followUps: [String: Any]? = nil, resurfacing: [String: Any]? = nil) -> TodayViewModel {
        if followUps != nil || resurfacing != nil {
            let follow = followUps ?? ["items": [], "total": 0]
            let worth = resurfacing ?? ["cards": [], "total": 0]
            MockURLProtocol.handler = { request in
                switch request.url!.path {
                case "/api/magician/v2/today": return (response(for: request), jsonData(Self.todayBody()))
                case "/api/magician/v2/channel-assist/follow-ups": return (response(for: request), jsonData(follow))
                case "/api/magician/v2/channel-assist/resurfacing/today": return (response(for: request), jsonData(worth))
                case "/api/magician/v2/today/visibility", "/api/magician/v2/feed":
                    return (response(for: request), jsonData(["items": []]))
                case "/api/magician/v3/published-surfaces/projections": return (response(for: request), jsonData(["surfaces": []]))
                case "/api/magician/v2/analytics/llm_calls/query":
                    return (response(for: request), jsonData(["columns": ["section", "k", "model", "v1", "v2"], "rows": []]))
                case "/api/magician/v2/analytics/query":
                    return (response(for: request), jsonData(["columns": ["section", "n"], "rows": []]))
                case "/api/magician/v2/analytics/memory_events/query":
                    return (response(for: request), jsonData(["columns": ["section", "n", "passes"], "rows": []]))
                case "/api/magician/v3/tasks": return (response(for: request), jsonData(["tasks": []]))
                case "/api/magician/v2/agents": return (response(for: request), jsonData(["agents": []]))
                case "/api/magician/v2/agents/updates": return (response(for: request), jsonData(["events": []]))
                default:
                    XCTFail("Unexpected fixture request: \(request.url!.absoluteString)")
                    return (response(for: request, status: 404), Data())
                }
            }
        }
        let sut = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!,
                                 principal: "person-1", workspace: "space-1", refreshesAfterActions: false,
                                 mutationCoordinator: CardMutationCoordinator(),
                                 defaults: UserDefaults(suiteName: "today-morning-edition-tests")!)
        let loaded = expectation(description: "Today loaded")
        sut.$isLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &cancellables)
        sut.fetch()
        wait(for: [loaded], timeout: 3)
        return sut
    }

    @MainActor
    private func waitForBroadsheet(_ sut: TodayViewModel, _ tab: TodayBroadsheetTab, action: () -> Void) {
        let idle = expectation(description: "broadsheet page loaded")
        sut.$broadsheetLoading.dropFirst().filter { !$0.contains(tab) }.prefix(1)
            .sink { _ in idle.fulfill() }.store(in: &cancellables)
        action()
        wait(for: [idle], timeout: 3)
    }

    private func mockSession() -> URLSession {
        let session = makeMockSession()
        sessions.append(session)
        return session
    }
}
