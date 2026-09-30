//  MonitorsViewModelTests.swift
//  Recurring Monitors (Phase 5, iOS) — list pagination/filter/refresh, detail
//  loading, and lifecycle-action dispatch, all through mock transports (no
//  network).

import XCTest
@testable import Magician

/// Routes by path suffix — the detail VM fires detail/updates/runs
/// CONCURRENTLY, so a sequential script would be racy.
final class MonitorRouteTransport: Monitors.Transport, @unchecked Sendable {
    /// path-suffix → (json, status); first match wins.
    var routes: [(suffix: String, body: String, status: Int)] = []
    private(set) var seenPaths: [String] = []
    private let lock = NSLock()

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let path = request.url?.path ?? ""
        lock.withLock {
            seenPaths.append(path)
        }
        let match = routes.first { path.hasSuffix($0.suffix) }
        let body = match?.body ?? "{\"error\":\"unrouted\"}"
        let status = match?.status ?? 500
        let http = HTTPURLResponse(url: request.url!, statusCode: status,
                                   httpVersion: "HTTP/1.1",
                                   headerFields: ["Content-Type": "application/json"])!
        return (Data(body.utf8), http)
    }
}

/// A transport that can HOLD a response in flight until released — for
/// preemption tests (a reload racing an in-flight load-more).
final class MonitorHoldTransport: Monitors.Transport, @unchecked Sendable {
    private let lock = NSLock()
    private var held: [CheckedContinuation<Void, Never>] = []
    /// Steps consumed front-first; `hold: true` suspends before replying.
    /// Empty script → an empty page.
    var script: [(body: String, hold: Bool)] = []

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let step = lock.withLock {
            script.isEmpty
                ? (body: #"{"items":[],"next_cursor":null,"limit":50}"#, hold: false)
                : script.removeFirst()
        }
        if step.hold {
            await withCheckedContinuation { continuation in
                lock.withLock {
                    held.append(continuation)
                }
            }
        }
        let http = HTTPURLResponse(url: request.url!, statusCode: 200,
                                   httpVersion: "HTTP/1.1",
                                   headerFields: ["Content-Type": "application/json"])!
        return (Data(step.body.utf8), http)
    }

    var heldCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return held.count
    }

    func releaseAll() {
        lock.lock()
        let continuations = held
        held = []
        lock.unlock()
        continuations.forEach { $0.resume() }
    }
}

@MainActor
final class MonitorsViewModelTests: XCTestCase {

    private func client(_ transport: Monitors.Transport) -> Monitors.APIClient {
        Monitors.APIClient(
            baseURL: URL(string: "https://ios.example.test")!,
            scope: Monitors.Scope(principal: "anonymous", workspace: "default"),
            transport: transport)
    }

    private func listItem(_ id: String, state: String = "active") -> String {
        """
        {"task_id":"\(id)","title":"\(id)","objective":"o","state":"\(state)",
         "cadence_summary":"Every 3600s","monitor_revision":1,
         "last_run_status":"never_ran","health":"ok"}
        """
    }

    // MARK: - List: pagination

    func testFirstPageThenLoadMoreAccumulatesAndDedupes() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))

        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":"cur_task_b","limit":50}
        """)
        await vm.loadIfNeeded()
        XCTAssertEqual(vm.items.map(\.taskID), ["task_a", "task_b"])
        XCTAssertEqual(vm.nextCursor, "cur_task_b")
        XCTAssertTrue(vm.hasLoadedOnce)
        XCTAssertNil(vm.errorMessage)

        // Page 2 overlaps task_b (mutation between pages) — rows dedupe.
        transport.seed("""
        {"items":[\(listItem("task_b")),\(listItem("task_c"))],
         "next_cursor":null,"limit":50}
        """)
        await vm.loadMore()
        XCTAssertEqual(vm.items.map(\.taskID), ["task_a", "task_b", "task_c"])
        XCTAssertNil(vm.nextCursor, "null cursor ends pagination")

        // No cursor → loadMore is a no-op (no extra request).
        let requestsBefore = transport.requestCount
        await vm.loadMore()
        XCTAssertEqual(transport.requestCount, requestsBefore)
    }

    func testLoadIfNeededOnlyLoadsOnce() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"items":[],"next_cursor":null,"limit":50}
        """)
        let vm = MonitorsListViewModel(client: client(transport))
        await vm.loadIfNeeded()
        await vm.loadIfNeeded()
        XCTAssertEqual(transport.requestCount, 1)
    }

    // MARK: - List: the envelope's total

    func testTotalCountsTheCorpusAcrossEveryPage() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))

        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":"cur_task_b","limit":50,"total":5,"offset":0}
        """)
        await vm.loadIfNeeded()
        XCTAssertEqual(vm.total, 5, "total is the pool, not the page (2 rows)")

        // Every page reports the SAME total. A total that shrank per page
        // would say one page of results however many there are.
        transport.seed("""
        {"items":[\(listItem("task_c"))],"next_cursor":null,"limit":50,
         "total":5,"offset":2}
        """)
        await vm.loadMore()
        XCTAssertEqual(vm.items.count, 3)
        XCTAssertEqual(vm.total, 5, "the second page counts the same corpus")
    }

    func testAbsentTotalIsNoPageCountRatherThanZeroPages() async throws {
        // The pre-`total` envelope an older server still sends. Absent means
        // the binary never counted the pool — NOT that the pool is empty.
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":null,"limit":50}
        """)
        await vm.reload()
        XCTAssertEqual(vm.items.count, 1, "rows still arrive without a total")
        XCTAssertNil(vm.total, "absent total is unknown, not 0")

        // And a client-side removal must not conjure a count that was never
        // reported — decrementing from a default zero would invent one.
        vm.remove(taskID: "task_a")
        XCTAssertNil(vm.total, "nothing to decrement when nothing was counted")
    }

    func testFilterSwitchDropsThePreviousLanesTotal() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":null,"limit":50,
         "total":9,"offset":0}
        """)
        await vm.loadIfNeeded()
        XCTAssertEqual(vm.total, 9)

        // The paused lane is a different corpus, and this response counted
        // none of it — 9 must not survive to label the rows it never saw.
        transport.seed("""
        {"items":[\(listItem("task_p", state: "paused"))],"next_cursor":null,"limit":50}
        """)
        await vm.setFilter(.paused)
        XCTAssertEqual(vm.items.map(\.taskID), ["task_p"])
        XCTAssertNil(vm.total, "a stale total must not label another lane's rows")
    }

    /// Monitors has no completion grace period to be broken by server paging
    /// (the Tasks list's 5s hold has no analogue here), but the two paths that
    /// drop a row WITHOUT a refetch do have to move the count with it.
    func testClientSideRowRemovalFollowsTheTotalDown() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":"cur_task_b","limit":50,"total":4,"offset":0}
        """)
        await vm.loadIfNeeded()

        // Delete: the row goes, and nothing else will correct the count.
        vm.remove(taskID: "task_a")
        XCTAssertEqual(vm.items.map(\.taskID), ["task_b"])
        XCTAssertEqual(vm.total, 3)

        // A row that was never held decrements nothing.
        vm.remove(taskID: "task_absent")
        XCTAssertEqual(vm.total, 3, "removing an unheld id must not move the count")
    }

    func testPausingUnderTheActiveFilterFollowsTheTotalDown() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":null,"limit":50,"total":2,"offset":0}
        """)
        await vm.setFilter(.active)
        XCTAssertEqual(vm.total, 2)

        // Pausing drops the row out of the ACTIVE corpus it was counted in.
        vm.applyState(taskID: "task_a", state: "paused")
        XCTAssertEqual(vm.items.map(\.taskID), ["task_b"])
        XCTAssertEqual(vm.total, 1)

        // Under All, a state change keeps the row — and the count.
        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":null,"limit":50,"total":2,"offset":0}
        """)
        await vm.setFilter(.all)
        vm.applyState(taskID: "task_a", state: "paused")
        XCTAssertEqual(vm.items.count, 2)
        XCTAssertEqual(vm.total, 2, "a row that stayed must not be counted out")
    }

    // MARK: - List: filter

    func testFilterSwitchResetsAndSendsStateQuery() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":"cur_task_a","limit":50}
        """)
        await vm.loadIfNeeded()
        XCTAssertEqual(vm.items.count, 1)

        transport.seed("""
        {"items":[\(listItem("task_p", state: "paused"))],"next_cursor":null,"limit":50}
        """)
        await vm.setFilter(.paused)
        XCTAssertEqual(vm.items.map(\.taskID), ["task_p"])
        XCTAssertNil(vm.nextCursor)
        let url = try XCTUnwrap(transport.lastRequest?.url)
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        XCTAssertEqual(components.queryItems?.first { $0.name == "state" }?.value, "paused")

        // Same filter again is a no-op.
        let before = transport.requestCount
        await vm.setFilter(.paused)
        XCTAssertEqual(transport.requestCount, before)
    }

    // MARK: - List: error + retry + refresh

    func testErrorSurfacesAndRetryRecovers() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"error":"boom"}
        """, status: 500)
        await vm.reload()
        XCTAssertNotNil(vm.errorMessage)
        XCTAssertTrue(vm.items.isEmpty)

        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":null,"limit":50}
        """)
        await vm.reload()
        XCTAssertNil(vm.errorMessage)
        XCTAssertEqual(vm.items.count, 1)
    }

    func testLoadMoreErrorKeepsAccumulatedRows() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":"cur_task_a","limit":50}
        """)
        await vm.reload()
        transport.seed("""
        {"error":"boom"}
        """, status: 500)
        await vm.loadMore()
        XCTAssertEqual(vm.items.map(\.taskID), ["task_a"], "page error must not clear rows")
        XCTAssertNotNil(vm.errorMessage)
    }

    /// Adversarial-review I1: a load-more held in flight while TWO rapid
    /// reloads preempt it must not leave `isLoadingMore` stuck true (the
    /// preempted task's trailing reset loses the generation race; reload
    /// clears the flag up-front instead).
    func testReloadPreemptionClearsOrphanedLoadMoreFlag() async throws {
        let transport = MonitorHoldTransport()
        transport.script = [
            (body: """
             {"items":[\(listItem("task_a"))],"next_cursor":"cur_a","limit":50}
             """, hold: false),                                   // first page
            (body: """
             {"items":[\(listItem("task_b"))],"next_cursor":null,"limit":50}
             """, hold: true),                                    // held load-more
            (body: """
             {"items":[\(listItem("task_c"))],"next_cursor":null,"limit":50}
             """, hold: false),                                   // reload #1
            (body: """
             {"items":[\(listItem("task_d"))],"next_cursor":null,"limit":50}
             """, hold: false),                                   // reload #2
        ]
        let vm = MonitorsListViewModel(client: client(transport))
        await vm.reload()
        XCTAssertEqual(vm.nextCursor, "cur_a")

        let loadMore = Task { await vm.loadMore() }
        // Wait until the load-more request is actually suspended in flight.
        var spins = 0
        while transport.heldCount == 0 {
            await Task.yield()
            spins += 1
            if spins > 100_000 { return XCTFail("load-more never reached the transport") }
        }
        XCTAssertTrue(vm.isLoadingMore)

        // Two rapid reloads preempt the held load-more.
        async let first: Void = vm.reload()
        async let second: Void = vm.reload()
        _ = await (first, second)
        XCTAssertFalse(vm.isLoadingMore,
                       "reload must clear the orphaned load-more flag up-front")

        transport.releaseAll()
        _ = await loadMore.value
        XCTAssertFalse(vm.isLoadingMore)
        XCTAssertFalse(vm.isLoading)
        XCTAssertFalse(vm.items.map(\.taskID).contains("task_b"),
                       "the preempted load-more's rows are discarded")
    }

    func testReloadReplacesRows() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":null,"limit":50}
        """)
        await vm.reload()
        transport.seed("""
        {"items":[\(listItem("task_b"))],"next_cursor":null,"limit":50}
        """)
        await vm.reload()
        XCTAssertEqual(vm.items.map(\.taskID), ["task_b"], "pull-to-refresh replaces the list")
    }

    /// A delete is not a restart. The reader walked two pages to reach that
    /// monitor and must still have them afterwards — and the cursor must come
    /// back anchored on a row that still exists.
    ///
    /// The second half is the trap: `next_cursor` is minted from the LAST row
    /// of the page that carried it, and `/monitors` resolves a cursor it can
    /// no longer find to the END of the list (an empty page, not an error). So
    /// deleting the last-loaded monitor and keeping its cursor would stop
    /// pagination with nothing on screen to say so.
    func testDeletingTheLastLoadedMonitorKeepsThePagesAndRePrimesTheCursor() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport), pageSize: 2)

        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b"))],
         "next_cursor":"cur_task_b","limit":2,"total":6,"offset":0}
        """)
        await vm.loadIfNeeded()
        transport.seed("""
        {"items":[\(listItem("task_c")),\(listItem("task_d"))],
         "next_cursor":"cur_task_d","limit":2,"total":6,"offset":2}
        """)
        await vm.loadMore()
        XCTAssertEqual(vm.items.map(\.taskID), ["task_a", "task_b", "task_c", "task_d"])
        XCTAssertEqual(vm.nextCursor, "cur_task_d")

        // task_d is BOTH the deleted row and the row the cursor names.
        vm.remove(taskID: "task_d")
        transport.seed("""
        {"items":[\(listItem("task_a")),\(listItem("task_b")),
                  \(listItem("task_c")),\(listItem("task_e"))],
         "next_cursor":"cur_task_e","limit":4,"total":5,"offset":0}
        """)
        await vm.refreshLoadedSpan()

        let query = Dictionary(uniqueKeysWithValues:
            (URLComponents(url: transport.lastRequest!.url!, resolvingAgainstBaseURL: false)?
                .queryItems ?? []).map { ($0.name, $0.value ?? "") })
        XCTAssertEqual(query["limit"], "4",
                       "the window the reader asked for, not one page and not one row fewer")
        XCTAssertNil(query["cursor"], "a span is re-read from the top")
        XCTAssertEqual(vm.items.map(\.taskID), ["task_a", "task_b", "task_c", "task_e"],
                       "the pages walked to reach the deleted monitor are still here")
        XCTAssertEqual(vm.nextCursor, "cur_task_e", "re-anchored on a row that still exists")
        XCTAssertEqual(vm.total, 5)

        // The proof the stale cursor never shipped: paging still advances.
        transport.seed("""
        {"items":[\(listItem("task_f"))],"next_cursor":null,"limit":2,"total":5,"offset":4}
        """)
        await vm.loadMore()
        XCTAssertEqual(vm.items.map(\.taskID),
                       ["task_a", "task_b", "task_c", "task_e", "task_f"])
    }

    // MARK: - List: row mutations

    func testApplyStateUpdatesOrRemovesFilteredRow() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_a"))],"next_cursor":null,"limit":50}
        """)
        await vm.reload()
        vm.applyState(taskID: "task_a", state: "paused")
        XCTAssertEqual(vm.items.first?.state, "paused", "all-filter keeps the row, state flipped")

        // Under state=active, pausing removes the row.
        transport.seed("""
        {"items":[\(listItem("task_b"))],"next_cursor":null,"limit":50}
        """)
        await vm.setFilter(.active)
        vm.applyState(taskID: "task_b", state: "paused")
        XCTAssertTrue(vm.items.isEmpty)

        vm.remove(taskID: "task_missing") // no-op, no crash
    }

    /// T4: a row transitioning paused → active while the PAUSED filter is
    /// live must leave the filtered list (it no longer matches the query
    /// the server answered).
    func testResumedRowLeavesPausedFilteredList() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorsListViewModel(client: client(transport))
        transport.seed("""
        {"items":[\(listItem("task_p", state: "paused"))],"next_cursor":null,"limit":50}
        """)
        await vm.setFilter(.paused)
        XCTAssertEqual(vm.items.map(\.taskID), ["task_p"])
        vm.applyState(taskID: "task_p", state: "active")
        XCTAssertTrue(vm.items.isEmpty,
                      "a resumed monitor is REMOVED from the paused-filtered list")
    }

    // MARK: - Detail: load + sections

    private func routedDetailTransport() throws -> MonitorRouteTransport {
        let fixturesDir = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("magician/tests/fixtures/monitors", isDirectory: true)
        let spec = try String(contentsOf: fixturesDir
            .appendingPathComponent("monitor_spec_v1.json"), encoding: .utf8)
        let run = try String(contentsOf: fixturesDir
            .appendingPathComponent("monitor_run_result_v1_changed.json"), encoding: .utf8)
        let update = try String(contentsOf: fixturesDir
            .appendingPathComponent("monitor_update_detail_v1.json"), encoding: .utf8)
        let transport = MonitorRouteTransport()
        transport.routes = [
            (suffix: "/runs", body: """
                {"items":[\(run)],"next_cursor":null,"limit":50}
                """, status: 200),
            (suffix: "/updates", body: """
                {"items":[\(update)],"next_cursor":null,"limit":50}
                """, status: 200),
            (suffix: "/monitors/task_monitor_fixture_001", body: """
                {"task_id":"task_monitor_fixture_001","title":"Acme Robotics pricing",
                 "spec":\(spec),"monitor_revision":2,
                 "schedule":{"kind":{"Cron":{"expression":"0 6 * * 1",
                   "timezone":"America/Los_Angeles"}},"paused":false},
                 "state":{"status":"pending","schedule_fire_count":4},
                 "created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-23T06:01:05Z",
                 "tags":["system:monitor"]}
                """, status: 200),
        ]
        return transport
    }

    func testDetailLoadPopulatesSections() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        highlightUpdateID: "mu_fixture_0001",
                                        client: client(transport))
        await vm.load()
        XCTAssertNil(vm.loadErrorMessage)
        XCTAssertEqual(vm.detail?.monitorRevision, 2)
        XCTAssertEqual(vm.cadenceSummary, "Cron 0 6 * * 1 (America/Los_Angeles)")
        XCTAssertFalse(vm.isPaused)
        XCTAssertEqual(vm.updates.count, 1)
        XCTAssertEqual(vm.latestUpdate?.updateID, "mu_fixture_0001")
        XCTAssertEqual(vm.runs.count, 1)
        XCTAssertEqual(vm.highlightUpdateID, "mu_fixture_0001")
    }

    func testDetailLoadErrorSurfaces() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"error":"monitor_not_found","task_id":"task_x"}
        """, status: 404)
        let vm = MonitorDetailViewModel(taskID: "task_x", client: client(transport))
        await vm.load()
        XCTAssertNil(vm.detail)
        XCTAssertNotNil(vm.loadErrorMessage)
    }

    /// T5 (partial tolerance): detail 200 + updates 500 must still render —
    /// detail non-nil, updates degrade to an empty section, and NO error
    /// state blocks the screen.
    func testDetailToleratesUpdatesEndpointFailure() async throws {
        let transport = try routedDetailTransport()
        transport.routes.insert((suffix: "/updates", body: """
            {"error":"boom"}
            """, status: 500), at: 0)
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        XCTAssertNotNil(vm.detail, "detail 200 renders even when updates 500")
        XCTAssertEqual(vm.updates, [], "failed updates degrade to an empty section")
        XCTAssertEqual(vm.runs.count, 1, "the runs section is unaffected")
        XCTAssertNil(vm.loadErrorMessage, "no error state blocks the screen")
    }

    // MARK: - Detail: action dispatch

    func testPauseFlipsScheduleAndHitsPausePath() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        transport.routes.insert((suffix: "/pause", body: """
            {"task_id":"task_monitor_fixture_001","state":"paused"}
            """, status: 200), at: 0)
        let ok = await vm.pause()
        XCTAssertTrue(ok)
        XCTAssertTrue(vm.isPaused, "returned state folds into the loaded schedule")
        XCTAssertTrue(transport.seenPaths.contains {
            $0.hasSuffix("/monitors/task_monitor_fixture_001/pause")
        })

        transport.routes.insert((suffix: "/resume", body: """
            {"task_id":"task_monitor_fixture_001","state":"active"}
            """, status: 200), at: 0)
        let resumed = await vm.resume()
        XCTAssertTrue(resumed)
        XCTAssertFalse(vm.isPaused)
    }

    func testRunNowDispatchesAndPauseErrorSurfaces() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        transport.routes.insert((suffix: "/run", body: """
            {"task":{},"execution":{}}
            """, status: 202), at: 0)
        let ran = await vm.runNow()
        XCTAssertTrue(ran)
        XCTAssertTrue(transport.seenPaths.contains {
            $0.hasSuffix("/monitors/task_monitor_fixture_001/run")
        })

        transport.routes.insert((suffix: "/pause", body: """
            {"error":"monitor_unscheduled","task_id":"task_monitor_fixture_001"}
            """, status: 409), at: 0)
        let pausedOK = await vm.pause()
        XCTAssertFalse(pausedOK)
        XCTAssertNotNil(vm.actionErrorMessage)
        XCTAssertFalse(vm.isPaused, "a failed pause must not flip the local state")
    }

    func testDeleteMarksDeleted() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        transport.routes.insert((suffix: "/monitors/task_monitor_fixture_001", body: """
            {"ok":true,"task_id":"task_monitor_fixture_001","files_removed":false}
            """, status: 200), at: 0)
        let deleted = await vm.delete()
        XCTAssertTrue(deleted)
        XCTAssertTrue(vm.wasDeleted)
    }

    // MARK: - Detail: feedback (Phase 6, plan §10)

    private func feedbackRecord(_ id: String, _ updateID: String,
                                _ verdict: Monitors.FeedbackVerdict,
                                _ recordedAt: String) -> Monitors.FeedbackRecordV1 {
        Monitors.FeedbackRecordV1(feedbackID: id, updateID: updateID,
                                  verdict: verdict, note: nil, recordedAt: recordedAt)
    }

    func testFeedbackStatesMergeNewestWinsRegardlessOfOrder() throws {
        let newestFirst = Monitors.feedbackStates(from: [
            feedbackRecord("mf_new", "mu_a", .notRelevant, "2026-07-22T12:00:00Z"),
            feedbackRecord("mf_old", "mu_a", .useful, "2026-07-22T08:00:00Z"),
            feedbackRecord("mf_b", "mu_b", .useful, "2026-07-22T09:00:00Z"),
        ])
        XCTAssertEqual(newestFirst["mu_a"]?.verdict, .notRelevant)
        XCTAssertEqual(newestFirst["mu_a"]?.feedbackID, "mf_new")
        XCTAssertEqual(newestFirst["mu_b"]?.verdict, .useful)
        XCTAssertEqual(newestFirst.count, 2, "one verdict per update_id")

        let oldestFirst = Monitors.feedbackStates(from: [
            feedbackRecord("mf_old", "mu_a", .useful, "2026-07-22T08:00:00Z"),
            feedbackRecord("mf_new", "mu_a", .notRelevant, "2026-07-22T12:00:00Z"),
        ])
        XCTAssertEqual(oldestFirst["mu_a"]?.feedbackID, "mf_new",
                       "the newest recorded_at wins regardless of item order")

        // Unparseable timestamps keep the FIRST-SEEN record (the server
        // returns newest first, so first-seen = newest under server order).
        let garbled = Monitors.feedbackStates(from: [
            feedbackRecord("mf_first", "mu_a", .useful, "not-a-date"),
            feedbackRecord("mf_second", "mu_a", .notRelevant, "also-bad"),
        ])
        XCTAssertEqual(garbled["mu_a"]?.feedbackID, "mf_first")

        XCTAssertTrue(Monitors.feedbackStates(from: []).isEmpty)
    }

    func testIsMaterialUpdateAcceptsOnlyChanged() throws {
        func update(_ status: Monitors.RunStatus) -> Monitors.UpdateDetailV1 {
            Monitors.UpdateDetailV1(
                updateID: "mu_x", monitorTaskID: "task_1", monitorRevision: 1,
                executionID: "exec_1", occurredAt: "2026-07-22T10:00:00Z",
                status: status, changeFingerprint: nil, headline: "h", summary: "s",
                findings: [],
                notification: Monitors.NotificationV1(
                    policy: .materialChanges, emitted: false, channel: "app",
                    dedupeKey: "k"))
        }
        XCTAssertTrue(Monitors.isMaterialUpdate(update(.changed)))
        XCTAssertFalse(Monitors.isMaterialUpdate(update(.baseline)))
        XCTAssertFalse(Monitors.isMaterialUpdate(update(.unchanged)))
        XCTAssertFalse(Monitors.isMaterialUpdate(update(.degraded)))
        XCTAssertFalse(Monitors.isMaterialUpdate(update(.failed)))
    }

    func testDetailLoadMergesStoredFeedback() async throws {
        let transport = try routedDetailTransport()
        transport.routes.insert((suffix: "/feedback", body: """
            {"items":[
              {"feedback_id":"mf_2","update_id":"mu_fixture_0001","verdict":"not_relevant",
               "recorded_at":"2026-07-23T10:00:00Z"},
              {"feedback_id":"mf_1","update_id":"mu_fixture_0001","verdict":"useful",
               "recorded_at":"2026-07-22T10:00:00Z"}
            ],"next_cursor":null,"limit":50}
            """, status: 200), at: 0)
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .notRelevant,
                       "the newest stored record wins the merge")
        XCTAssertFalse(vm.isFeedbackInFlight("mu_fixture_0001"))
        XCTAssertNil(vm.verdict(for: "mu_other"))
    }

    func testDetailToleratesFeedbackEndpointFailure() async throws {
        let transport = try routedDetailTransport()
        transport.routes.insert((suffix: "/feedback", body: """
            {"error":"boom"}
            """, status: 500), at: 0)
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        XCTAssertNotNil(vm.detail, "detail 200 renders even when feedback 500s")
        XCTAssertTrue(vm.feedbackByUpdate.isEmpty,
                      "failed feedback degrades to no stored verdicts")
        XCTAssertNil(vm.loadErrorMessage, "no error state blocks the screen")
    }

    func testSubmitFeedbackOptimisticSettleAndReplaceVerdict() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        XCTAssertNil(vm.verdict(for: "mu_fixture_0001"))

        transport.routes.insert(
            (suffix: "/updates/mu_fixture_0001/feedback", body: """
             {"task_id":"task_monitor_fixture_001","update_id":"mu_fixture_0001",
              "verdict":"useful","recorded":true,"feedback_id":"mf_0001"}
             """, status: 200), at: 0)
        let ok = await vm.submitFeedback(updateID: "mu_fixture_0001", verdict: .useful)
        XCTAssertTrue(ok)
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .useful)
        XCTAssertEqual(vm.feedbackByUpdate["mu_fixture_0001"]?.feedbackID, "mf_0001")
        XCTAssertFalse(vm.isFeedbackInFlight("mu_fixture_0001"))
        XCTAssertTrue(transport.seenPaths.contains {
            $0.hasSuffix("/monitors/task_monitor_fixture_001/updates/mu_fixture_0001/feedback")
        })

        // The OPPOSITE verdict replaces the stored one.
        transport.routes.insert(
            (suffix: "/updates/mu_fixture_0001/feedback", body: """
             {"task_id":"task_monitor_fixture_001","update_id":"mu_fixture_0001",
              "verdict":"not_relevant","recorded":true,"feedback_id":"mf_0002"}
             """, status: 200), at: 0)
        let replaced = await vm.submitFeedback(updateID: "mu_fixture_0001",
                                               verdict: .notRelevant)
        XCTAssertTrue(replaced)
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .notRelevant)
        XCTAssertEqual(vm.feedbackByUpdate["mu_fixture_0001"]?.feedbackID, "mf_0002")
    }

    func testSubmitFeedbackIdempotentReplaySettlesLikeRecorded() async throws {
        let transport = try routedDetailTransport()
        transport.routes.insert((suffix: "/feedback", body: """
            {"items":[{"feedback_id":"mf_1","update_id":"mu_fixture_0001",
              "verdict":"useful","recorded_at":"2026-07-22T10:00:00Z"}],
             "next_cursor":null,"limit":50}
            """, status: 200), at: 0)
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .useful)

        // Tapping Useful AGAIN: the server replies recorded:false with the
        // SAME verdict + id — the client settles identically (no error, no
        // stuck in-flight state).
        transport.routes.insert(
            (suffix: "/updates/mu_fixture_0001/feedback", body: """
             {"task_id":"task_monitor_fixture_001","update_id":"mu_fixture_0001",
              "verdict":"useful","recorded":false,"feedback_id":"mf_1"}
             """, status: 200), at: 0)
        let ok = await vm.submitFeedback(updateID: "mu_fixture_0001", verdict: .useful)
        XCTAssertTrue(ok)
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .useful)
        XCTAssertEqual(vm.feedbackByUpdate["mu_fixture_0001"]?.feedbackID, "mf_1")
        XCTAssertFalse(vm.isFeedbackInFlight("mu_fixture_0001"))
        XCTAssertNil(vm.actionErrorMessage)
    }

    func testSubmitFeedbackErrorRollsBackToStoredVerdict() async throws {
        let transport = try routedDetailTransport()
        transport.routes.insert((suffix: "/feedback", body: """
            {"items":[{"feedback_id":"mf_1","update_id":"mu_fixture_0001",
              "verdict":"useful","recorded_at":"2026-07-22T10:00:00Z"}],
             "next_cursor":null,"limit":50}
            """, status: 200), at: 0)
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()

        transport.routes.insert(
            (suffix: "/updates/mu_fixture_0001/feedback", body: """
             {"error":"monitor_feedback_verdict_invalid"}
             """, status: 400), at: 0)
        let ok = await vm.submitFeedback(updateID: "mu_fixture_0001", verdict: .notRelevant)
        XCTAssertFalse(ok)
        XCTAssertEqual(vm.verdict(for: "mu_fixture_0001"), .useful,
                       "the optimistic verdict rolls back to the stored one")
        XCTAssertFalse(vm.isFeedbackInFlight("mu_fixture_0001"))
        XCTAssertNotNil(vm.actionErrorMessage)
    }

    func testSubmitFeedbackErrorWithNoPriorVerdictClearsEntry() async throws {
        let transport = try routedDetailTransport()
        let vm = MonitorDetailViewModel(taskID: "task_monitor_fixture_001",
                                        client: client(transport))
        await vm.load()
        transport.routes.insert(
            (suffix: "/updates/mu_fixture_0001/feedback", body: """
             {"error":"update_not_found"}
             """, status: 404), at: 0)
        let ok = await vm.submitFeedback(updateID: "mu_fixture_0001", verdict: .useful)
        XCTAssertFalse(ok)
        XCTAssertNil(vm.verdict(for: "mu_fixture_0001"),
                     "no prior verdict → the optimistic entry is removed")
        XCTAssertNil(vm.feedbackByUpdate["mu_fixture_0001"])
        XCTAssertNotNil(vm.actionErrorMessage)
    }

    // MARK: - Composer: review-before-activate

    private func validForm() -> MonitorForm {
        var form = MonitorForm()
        form.objective = "Watch the pricing page"
        form.urlsText = "https://acme.example/pricing"
        form.cadence = "daily-9"
        return form
    }

    func testActivateRefusesWithoutReview() async throws {
        let transport = MonitorMockTransport()
        let vm = MonitorComposerViewModel(mode: .create, form: validForm(),
                                          client: client(transport))
        let submitted = await vm.activate()
        XCTAssertFalse(submitted, "the POST happens ONLY from the review step")
        XCTAssertEqual(transport.requestCount, 0)
    }

    func testReviewRejectsWithStableReasonLabel() throws {
        let vm = MonitorComposerViewModel(mode: .create, form: MonitorForm(),
                                          client: client(MonitorMockTransport()))
        XCTAssertFalse(vm.review())
        XCTAssertEqual(vm.errorMessage,
                       MonitorForm.reasonLabel("monitor_objective_required"))
        XCTAssertFalse(vm.isReviewing)
    }

    func testReviewThenActivateCreates() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"task_id":"task_new","monitor_revision":1}
        """, status: 201)
        let vm = MonitorComposerViewModel(mode: .create, form: validForm(),
                                          client: client(transport))
        XCTAssertTrue(vm.review())
        XCTAssertTrue(vm.isReviewing)
        // The review step carries the NORMALIZED contract + exact cadence.
        guard case .review(let spec, let schedule) = vm.step else {
            return XCTFail("expected review step")
        }
        XCTAssertEqual(spec.objective, "Watch the pricing page")
        XCTAssertEqual(Monitors.cadenceSummary(schedule), "Cron 0 9 * * *")

        let submitted = await vm.activate()
        XCTAssertTrue(submitted)
        XCTAssertEqual(transport.lastRequest?.httpMethod, "POST")
        XCTAssertEqual(transport.lastRequest?.url?.path, "/api/magician/v3/monitors")
    }

    func testActivateEditPatchesTask() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"task_id":"task_1","monitor_revision":2}
        """)
        let vm = MonitorComposerViewModel(mode: .edit(taskID: "task_1"),
                                          form: validForm(),
                                          client: client(transport))
        XCTAssertTrue(vm.review())
        let submitted = await vm.activate()
        XCTAssertTrue(submitted)
        XCTAssertEqual(transport.lastRequest?.httpMethod, "PATCH")
        XCTAssertEqual(transport.lastRequest?.url?.path, "/api/magician/v3/monitors/task_1")
    }

    /// Adversarial-review I2: EDIT with cadence "none" (the state an
    /// Interval/Once/OnEvent/unscheduled monitor opens the editor in) must
    /// build a nil schedule and PATCH with NO `schedule` key at all — the
    /// existing schedule stays untouched (the picker hides "On demand only"
    /// in edit mode; the review step discloses the no-change).
    func testEditOnDemandCadenceOmitsScheduleKeyFromPatch() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"task_id":"task_1","monitor_revision":4}
        """)
        var form = validForm()
        form.cadence = "none"
        let vm = MonitorComposerViewModel(mode: .edit(taskID: "task_1"),
                                          form: form, client: client(transport))
        XCTAssertTrue(vm.review())
        guard case .review(_, let schedule) = vm.step else {
            return XCTFail("expected review step")
        }
        XCTAssertNil(schedule, "cadence none builds a nil schedule")

        let submitted = await vm.activate()
        XCTAssertTrue(submitted)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "PATCH")
        let body = try XCTUnwrap(try JSONSerialization.jsonObject(
            with: XCTUnwrap(request.httpBody)) as? [String: Any])
        XCTAssertEqual(Set(body.keys), ["spec"],
                       "the PATCH body must carry NO schedule key (and no title — blank)")
    }

    func testActivateServerRejectionSurfacesAndStaysOnReview() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"error":"monitor_sources_required"}
        """, status: 400)
        let vm = MonitorComposerViewModel(mode: .create, form: validForm(),
                                          client: client(transport))
        XCTAssertTrue(vm.review())
        let submitted = await vm.activate()
        XCTAssertFalse(submitted)
        XCTAssertEqual(vm.errorMessage,
                       MonitorForm.reasonLabel("monitor_sources_required"))
        XCTAssertTrue(vm.isReviewing, "a server rejection keeps the review visible")
        vm.backToForm()
        XCTAssertFalse(vm.isReviewing)
        XCTAssertNil(vm.errorMessage)
    }

    // MARK: - Composer: convert mode (Phase 7)

    func testConvertReviewBuildsNilScheduleAndPostsConvertBody() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"task_id":"task_1","monitor_revision":1,"converted":true}
        """)
        var form = MonitorForm.convertPrefill(
            taskTitle: "Acme pricing check",
            taskDescription: "Check the Acme pricing page for plan changes")
        form.urlsText = "https://acme.example/pricing"
        let vm = MonitorComposerViewModel(
            mode: .convert(taskID: "task_1", keptCadence: "Cron 0 9 * * *"),
            form: form, client: client(transport))
        XCTAssertTrue(vm.isConvert)
        XCTAssertTrue(vm.review())
        guard case .review(let spec, let schedule) = vm.step else {
            return XCTFail("expected review step")
        }
        XCTAssertNil(schedule, "conversion never authors a schedule")
        XCTAssertEqual(spec.objective, "Check the Acme pricing page for plan changes")

        let submitted = await vm.activate()
        XCTAssertTrue(submitted)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1/convert")
        let body = try XCTUnwrap(try JSONSerialization.jsonObject(
            with: XCTUnwrap(request.httpBody)) as? [String: Any])
        XCTAssertEqual(Set(body.keys), ["spec", "title"],
                       "the convert body is {spec, title?} — NEVER a schedule key")
        XCTAssertEqual(body["title"] as? String, "Acme pricing check")
    }

    func testConvertConflictSurfacesFriendlyMessageAndStaysOnReview() async throws {
        let transport = MonitorMockTransport()
        transport.seed("""
        {"error":"monitor_already_exists"}
        """, status: 409)
        var form = MonitorForm.convertPrefill(taskTitle: "T", taskDescription: "D")
        form.urlsText = "https://acme.example/pricing"
        let vm = MonitorComposerViewModel(
            mode: .convert(taskID: "task_1", keptCadence: "unscheduled"),
            form: form, client: client(transport))
        XCTAssertTrue(vm.review())
        let submitted = await vm.activate()
        XCTAssertFalse(submitted)
        XCTAssertEqual(vm.errorMessage, "This task is already a monitor.")
        XCTAssertTrue(vm.isReviewing, "a server refusal keeps the review visible")
    }

    func testConvertReviewStillRejectsInvalidSpecsLocally() throws {
        // Convert mode skips schedule building but NOT the admission
        // mirror — a source-less prefill is refused before any POST.
        let form = MonitorForm.convertPrefill(taskTitle: "T", taskDescription: "D")
        let vm = MonitorComposerViewModel(
            mode: .convert(taskID: "task_1", keptCadence: "unscheduled"),
            form: form, client: client(MonitorMockTransport()))
        XCTAssertFalse(vm.review())
        XCTAssertEqual(vm.errorMessage,
                       MonitorForm.reasonLabel("monitor_sources_required"))
        XCTAssertFalse(vm.isReviewing)
    }
}
