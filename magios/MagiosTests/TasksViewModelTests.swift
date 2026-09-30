import XCTest
@testable import Magician

final class TasksViewModelTests: XCTestCase {
    private let today = "2026-07-13"

    private func task(_ id: String = "t", status: String = "pending", tags: [String] = [],
                      due: String? = nil, updated: String = "2026-07-13T00:00:00Z") -> TaskV3 {
        var d: [String: Any] = ["id": id, "title": "T \(id)", "status": status, "updated_at": updated]
        if !tags.isEmpty { d["tags"] = tags.map { ["id": $0, "name": $0] } }
        if let due = due { d["due_date"] = due }
        return try! JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: d))
    }

    override func setUp() {
        super.setUp()
        // Selecting a lane is now a request, so even the "pure state" tests can
        // reach the network. Start every one of them from a benign responder
        // instead of a nil handler that would XCTFail on the first filter tap.
        MockURLProtocol.handler = { req in (response(for: req), jsonData(["tasks": []])) }
    }

    override func tearDown() {
        // Never leave a nil handler that would XCTFail a late refresh from a prior test.
        MockURLProtocol.handler = { req in (response(for: req), jsonData(["tasks": []])) }
        super.tearDown()
    }

    // MARK: - Filter predicates (deterministic via the injected todayISO)

    func testFilterAll() {
        XCTAssertTrue(TaskFilter.all.matches(task(status: "running"), todayISO: today))
        XCTAssertFalse(TaskFilter.all.matches(task(status: "completed"), todayISO: today))
    }

    func testFilterInbox() {
        XCTAssertTrue(TaskFilter.inbox.matches(task(status: "pending", tags: []), todayISO: today))
        XCTAssertFalse(TaskFilter.inbox.matches(task(status: "pending", tags: ["x"]), todayISO: today))
        XCTAssertFalse(TaskFilter.inbox.matches(task(status: "running", tags: []), todayISO: today))
    }

    func testFilterToday() {
        XCTAssertTrue(TaskFilter.today.matches(task(due: "2026-07-13"), todayISO: today))
        XCTAssertFalse(TaskFilter.today.matches(task(due: "2026-07-14"), todayISO: today))
        XCTAssertFalse(TaskFilter.today.matches(task(), todayISO: today))
    }

    func testFilterOverdue() {
        XCTAssertTrue(TaskFilter.overdue.matches(task(status: "pending", due: "2026-07-12"), todayISO: today))
        XCTAssertFalse(TaskFilter.overdue.matches(task(status: "completed", due: "2026-07-12"), todayISO: today))
        XCTAssertFalse(TaskFilter.overdue.matches(task(status: "pending", due: "2026-07-14"), todayISO: today))
        XCTAssertFalse(TaskFilter.overdue.matches(task(status: "pending"), todayISO: today))
    }

    func testFilterRunning() {
        XCTAssertTrue(TaskFilter.running.matches(task(status: "running"), todayISO: today))
        XCTAssertTrue(TaskFilter.running.matches(task(status: "paused"), todayISO: today))
        XCTAssertFalse(TaskFilter.running.matches(task(status: "pending"), todayISO: today))
    }

    func testFilterCompleted() {
        XCTAssertTrue(TaskFilter.completed.matches(task(status: "completed"), todayISO: today))
        XCTAssertFalse(TaskFilter.completed.matches(task(status: "running"), todayISO: today))
    }

    // MARK: - Derived list state

    func testInitialTaskLaneIsLoadingUntilItsFirstRequestCompletes() {
        let vm = TasksViewModel(session: makeMockSession())

        XCTAssertEqual(vm.taskLoadState, .idle)
        XCTAssertEqual(vm.activeLoadState, .idle)
        XCTAssertTrue(vm.isLoading)
    }

    func testRegularAndInternalTaskLoadsHaveIndependentState() {
        let vm = TasksViewModel(session: makeMockSession())
        let internalStarted = expectation(description: "internal task request started")
        let releaseInternal = DispatchSemaphore(value: 0)
        MockURLProtocol.handler = { req in
            if req.url!.path.hasSuffix("/agents") {
                return (response(for: req), jsonData(["agents": []]))
            }
            if req.url!.path.hasSuffix("/tasks/internal") {
                internalStarted.fulfill()
                _ = releaseInternal.wait(timeout: .now() + 2)
                return (response(for: req), jsonData(["tasks": []]))
            }
            return (response(for: req), jsonData(["tasks": [[
                "id": "regular", "title": "Loaded task", "status": "pending",
                "updated_at": "2026-07-13T00:00:00Z"
            ]]]))
        }

        vm.load()

        XCTAssertEqual(vm.taskLoadState, .loading)
        XCTAssertEqual(vm.internalTaskLoadState, .loading)
        wait(for: [internalStarted], timeout: 2)
        waitUntil { vm.taskLoadState == .loaded }
        XCTAssertEqual(vm.tasks.map(\.id), ["regular"])
        XCTAssertEqual(vm.internalTaskLoadState, .loading)
        vm.lane = .internalTasks
        XCTAssertTrue(vm.isLoading)

        releaseInternal.signal()
        waitUntil { vm.internalTaskLoadState == .loaded }
        XCTAssertFalse(vm.isLoading)
    }

    func testFailedInitialTaskRequestDoesNotBecomeAnEmptySuccess() {
        let vm = TasksViewModel(session: makeMockSession())
        MockURLProtocol.handler = { req in
            if req.url!.path.hasSuffix("/agents") {
                return (response(for: req), jsonData(["agents": []]))
            }
            if req.url!.path.hasSuffix("/tasks/internal") {
                return (response(for: req), jsonData(["tasks": []]))
            }
            return (response(for: req, status: 503), jsonData(["error": "unavailable"]))
        }

        vm.load()

        waitUntil { vm.taskLoadState == .failed }
        XCTAssertTrue(vm.tasks.isEmpty)
        XCTAssertFalse(vm.isLoading)
    }

    func testAvailableTagsAndTagFilterIsExclusive() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [task("1", tags: ["a", "b"]), task("2", tags: ["b"])]
        XCTAssertEqual(vm.availableTags, ["a", "b"])       // sorted, deduped
        vm.selectedTag = "a"
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["1"])   // preset ignored while a tag is set
    }

    func testSearchMatchesTitleAndTags() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [task("1"), task("2", tags: ["urgent"])]
        vm.searchQuery = "urgent"
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["2"])
        vm.searchQuery = "t 1"                              // case-insensitive title
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["1"])
    }

    func testVisibleSortedNewestFirst() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [task("old", updated: "2026-07-13T00:00:00Z"),
                    task("new", updated: "2026-07-13T05:00:00Z")]
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["new", "old"])
    }

    func testLaneSelectsActiveTasks() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [task("reg")]; vm.internalTasks = [task("int")]
        vm.lane = .tasks; XCTAssertEqual(vm.activeTasks.map(\.id), ["reg"])
        vm.lane = .internalTasks; XCTAssertEqual(vm.activeTasks.map(\.id), ["int"])
    }

    func testNavigationTargetSelectsInternalLaneAndPrefersCanonicalDuplicate() {
        let vm = TasksViewModel(session: makeMockSession())
        let regular = fullTask("shared", lifecycle: "persistent")
        let internalTask = fullTask("shared", lifecycle: "internal")
        vm.tasks = [regular]
        vm.internalTasks = [internalTask]

        let target = vm.navigationTarget(for: "shared")

        XCTAssertEqual(target?.lane, .internalTasks)
        XCTAssertEqual(target?.task.lifecycle, "internal")
    }

    func testResolveNavigationTargetFetchesInternalTaskOutsideLoadedPage() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "resolved old internal task")
        MockURLProtocol.handler = { req in
            let components = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)
            XCTAssertTrue(req.url!.path.hasSuffix("/tasks/internal"))
            XCTAssertEqual(components?.queryItems?.first(where: { $0.name == "query" })?.value,
                           "internal-old")
            XCTAssertEqual(components?.queryItems?.first(where: { $0.name == "limit" })?.value, "25")
            return (response(for: req), jsonData(["tasks": [[
                "id": "internal-old", "title": "Older internal task", "status": "completed",
                "lifecycle": "internal", "updated_at": "2026-07-01T00:00:00Z"
            ]]]))
        }

        vm.resolveNavigationTarget(for: "internal-old") { target in
            XCTAssertEqual(target?.lane, .internalTasks)
            XCTAssertEqual(target?.task.id, "internal-old")
            exp.fulfill()
        }

        wait(for: [exp], timeout: 2)
        XCTAssertEqual(vm.internalTasks.map(\.id), ["internal-old"])
    }

    func testPrepareForInternalNavigationRevealsLinkedTaskAfterDetailCloses() {
        let vm = TasksViewModel(session: makeMockSession())
        let linked = fullTask("internal-complete", status: "completed", agent: "worker",
                              lifecycle: "internal")
        vm.internalTasks = [linked]
        vm.lane = .tasks
        vm.filter = .inbox
        vm.selectedTag = "hidden"
        vm.searchQuery = "different task"
        vm.internalStatusFilter = "running"
        vm.internalAgentFilter = "another-agent"

        vm.prepareForNavigation(to: TaskNavigationTarget(task: linked, lane: .internalTasks))

        XCTAssertEqual(vm.lane, .internalTasks)
        XCTAssertEqual(vm.filter, .completed)
        XCTAssertNil(vm.selectedTag)
        XCTAssertEqual(vm.searchQuery, "")
        XCTAssertNil(vm.internalStatusFilter)
        XCTAssertNil(vm.internalAgentFilter)
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["internal-complete"])
    }

    func testSetFilterClearsTag() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.selectedTag = "x"
        vm.setFilter(.running)
        XCTAssertNil(vm.selectedTag)
        XCTAssertEqual(vm.filter, .running)
    }

    func testCountByStatus() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [task("1", status: "running"), task("2", status: "running"), task("3", status: "pending")]
        XCTAssertEqual(vm.count(status: "running"), 2)
        XCTAssertEqual(vm.count(status: "pending"), 1)
    }

    // MARK: - Sort, internal filters, badges (web parity)

    private func fullTask(_ id: String, status: String = "pending", agent: String = "",
                          updated: String = "2026-07-13T00:00:00Z", created: String = "2026-07-13T00:00:00Z",
                          lifecycle: String? = nil, createdBy: String? = nil,
                          chatSessionId: String? = nil, synthFailedExec: String? = nil) -> TaskV3 {
        var d: [String: Any] = ["id": id, "title": "T \(id)", "status": status,
                                "updated_at": updated, "created_at": created, "agent_id": agent]
        if let l = lifecycle { d["lifecycle"] = l }
        if let c = createdBy { d["created_by"] = c }
        if let s = chatSessionId { d["chat_session_id"] = s }
        if let e = synthFailedExec { d["synthesis_failed_execution_id"] = e }
        return try! JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: d))
    }

    func testLifecycleBadge() {
        XCTAssertEqual(fullTask("a", lifecycle: "persistent").lifecycleBadge.kind, .persistent)
        XCTAssertEqual(fullTask("b", lifecycle: "internal").lifecycleBadge.kind, .internalTask)
        XCTAssertEqual(fullTask("c", lifecycle: "internal", createdBy: "__system__").lifecycleBadge.kind, .debug)
        XCTAssertEqual(fullTask("d", lifecycle: "internal", chatSessionId: "s1").lifecycleBadge.kind, .chat)
        // legacy wire alias is still treated as internal (→ chat here via chat_session_id)
        XCTAssertEqual(fullTask("e", lifecycle: "ephemeral_owned_by_chat", chatSessionId: "s").lifecycleBadge.kind, .chat)
    }

    func testSynthesisFailedFlag() {
        XCTAssertFalse(fullTask("a").synthesisFailed)
        XCTAssertTrue(fullTask("b", synthFailedExec: "exec-9").synthesisFailed)
        XCTAssertFalse(fullTask("c", synthFailedExec: "   ").synthesisFailed)   // blank → not failed
    }

    func testSortByFieldAndOrder() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.tasks = [fullTask("a", updated: "2026-07-13T01:00:00Z"),
                    fullTask("b", updated: "2026-07-13T03:00:00Z"),
                    fullTask("c", updated: "2026-07-13T02:00:00Z")]
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["b", "c", "a"])   // default: updated desc
        vm.setSort(field: .updatedAt)                                // same field → ascending
        XCTAssertTrue(vm.sortAscending)
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["a", "c", "b"])
        vm.setSort(field: .title)                                    // new field → resets to descending
        XCTAssertFalse(vm.sortAscending)
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["c", "b", "a"])   // title desc
    }

    func testInternalLaneStatusAndAgentFilters() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.internalTasks = [fullTask("a", status: "running", agent: "analyst"),
                            fullTask("b", status: "completed", agent: "analyst"),
                            fullTask("c", status: "running", agent: "writer")]
        vm.lane = .internalTasks
        XCTAssertEqual(vm.availableInternalAgents, ["analyst", "writer"])
        vm.internalStatusFilter = "running"
        XCTAssertEqual(Set(vm.visibleTasks.map(\.id)), ["a", "c"])
        vm.internalAgentFilter = "analyst"
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["a"])
        // The internal filters must NOT apply on the regular lane.
        vm.lane = .tasks
        vm.tasks = [fullTask("x", status: "pending", agent: "writer")]
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["x"])   // internalStatus="running"/agent="analyst" ignored here
    }

    func testInternalLaneDoesNotInheritRegularTaskPresetOrTag() {
        let vm = TasksViewModel(session: makeMockSession())
        vm.internalTasks = [fullTask("done", status: "completed"),
                            fullTask("failed", status: "failed")]
        vm.filter = .running
        vm.selectedTag = "regular-task-only"
        vm.lane = .internalTasks

        XCTAssertEqual(Set(vm.visibleTasks.map(\.id)), ["done", "failed"])

        vm.internalStatusFilter = "completed"
        XCTAssertEqual(vm.visibleTasks.map(\.id), ["done"])
    }

    func testCreateTaskIncludesOutputModeDependsOnAndRetention() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "create")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.path.hasSuffix("/tasks"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["output_mode"] as? String, "overwrite")
                XCTAssertEqual(json["depends_on"] as? [String], ["dep-1", "dep-2"])
                let retention = (json["schedule"] as? [String: Any])?["execution_history_retention"] as? [String: Any]
                XCTAssertEqual(retention?["max_records"] as? Int, 5)
                XCTAssertEqual(retention?["max_age_days"] as? Int, 30)
                exp.fulfill()
            }
            return (response(for: req), jsonData(["task": ["id": "new-1"]]))
        }
        vm.createTask(title: "New", description: "", agentId: "a", threadId: "general",
                      priority: nil, dueDate: nil, tagNames: [],
                      outputMode: "overwrite", dependsOn: ["dep-1", "dep-2"],
                      schedule: ["kind": ["Cron": ["expression": "0 9 * * *"]],
                                 "execution_history_retention": ["max_records": 5, "max_age_days": 30]]) {}
        wait(for: [exp], timeout: 2)
    }

    // MARK: - Networking

    func testLoadPopulatesBothLanes() {
        let vm = TasksViewModel(session: makeMockSession())
        MockURLProtocol.handler = { req in
            let url = req.url!.absoluteString
            if url.contains("/tasks/internal") {
                return (response(for: req), jsonData(["tasks": [["id": "int-1", "title": "I", "status": "running"]]]))
            } else if url.contains("/v2/agents") {
                return (response(for: req), jsonData(["agents": []]))
            }
            return (response(for: req), jsonData(["tasks": [["id": "reg-1", "title": "R", "status": "pending"]]]))
        }
        vm.load()
        waitUntil { !vm.tasks.isEmpty && !vm.internalTasks.isEmpty }
        XCTAssertEqual(vm.tasks.map(\.id), ["reg-1"])
        XCTAssertEqual(vm.internalTasks.map(\.id), ["int-1"])
    }

    // MARK: - Server-computed lanes (the lane is a query, not a page search)

    /// The pool the fixture serves from, held by reference so a test can act on
    /// it the way the reader does and have the next refetch see the result.
    /// Locked because the fixture answers on URL-loading threads while the test
    /// body reads from the main one.
    private final class TaskPool {
        private let lock = NSLock()
        private var storage: [[String: Any]]
        init(_ rows: [[String: Any]]) { storage = rows }

        var rows: [[String: Any]] {
            lock.lock(); defer { lock.unlock() }
            return storage
        }

        func setStatus(_ id: String, _ status: String) {
            lock.lock(); defer { lock.unlock() }
            guard let index = storage.firstIndex(where: { $0["id"] as? String == id }) else { return }
            storage[index]["status"] = status
        }
    }

    /// The requests the fixture saw, recorded off the loading thread.
    private final class RequestLog {
        private let lock = NSLock()
        private var storage: [URLRequest] = []

        func record(_ request: URLRequest) {
            lock.lock(); defer { lock.unlock() }
            storage.append(request)
        }

        var all: [URLRequest] {
            lock.lock(); defer { lock.unlock() }
            return storage
        }

        var count: Int { all.count }

        /// The query of the nth recorded request, as a dictionary.
        func query(_ index: Int) -> [String: String] {
            let requests = all
            guard requests.indices.contains(index) else { return [:] }
            let items = URLComponents(url: requests[index].url!, resolvingAgainstBaseURL: false)?
                .queryItems ?? []
            return Dictionary(items.compactMap { item in item.value.map { (item.name, $0) } },
                              uniquingKeysWith: { first, _ in first })
        }
    }

    /// The paged branch of `GET /v3/tasks` as the server actually behaves:
    /// `view=` narrows the WHOLE pool before paging, the two date lanes 400
    /// without a `today=`, and `counts` reports every lane over the pool
    /// *before* any narrowing — reported only when `today=` was supplied.
    ///
    /// The predicates are written out here rather than borrowed from
    /// `TaskFilter`, so the fixture cannot agree with the client by
    /// construction: a client-side lane bug shows up as a disagreement rather
    /// than as two copies of one mistake.
    private func installTaskLaneBackend(
        _ pool: TaskPool,
        log: RequestLog? = nil
    ) {
        func matches(_ lane: String, _ row: [String: Any], today: String) -> Bool {
            let status = row["status"] as? String ?? ""
            let due = row["due_date"] as? String
            let tags = row["tags"] as? [[String: Any]] ?? []
            switch lane {
            case "all": return status != "completed"
            case "inbox": return tags.isEmpty && status == "pending"
            case "today": return (due ?? "").hasPrefix(today)
            case "overdue":
                guard let due, !due.isEmpty else { return false }
                return due < today && status != "completed"
            case "running": return status == "running" || status == "paused"
            case "completed": return status == "completed"
            default: return true
            }
        }
        let laneNames = ["all", "inbox", "today", "overdue", "running", "completed"]

        MockURLProtocol.handler = { req in
            let url = req.url!
            if url.path.hasSuffix("/agents") {
                return (response(for: req), jsonData(["agents": []]))
            }
            if url.path.hasSuffix("/tasks/internal") {
                return (response(for: req), jsonData(["tasks": []]))
            }
            if req.httpMethod == "PUT", url.path.hasSuffix("/status") {
                let id = url.path.split(separator: "/").dropLast().last.map(String.init) ?? ""
                let body = requestBody(req)
                    .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                pool.setStatus(id, body?["status"] as? String ?? "")
                return (response(for: req), jsonData(["ok": true]))
            }
            log?.record(req)

            let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
            func query(_ name: String) -> String? { items.first { $0.name == name }?.value }
            let view = query("view")
            let today = query("today")
            if let view, ["today", "overdue"].contains(view), today == nil {
                return (response(for: req, status: 400),
                        jsonData(["error": "task_view_requires_today"]))
            }

            var body: [String: Any] = [:]
            if let today {
                var counts: [String: Int] = [:]
                for lane in laneNames {
                    counts[lane] = pool.rows.filter { matches(lane, $0, today: today) }.count
                }
                body["counts"] = counts
            }
            let narrowed = view.map { lane in
                pool.rows.filter { matches(lane, $0, today: today ?? "") }
            } ?? pool.rows
            let limit = Int(query("limit") ?? "") ?? narrowed.count
            let offset = Int(query("offset") ?? "") ?? 0
            let page = Array(narrowed.dropFirst(offset).prefix(limit))
            body["tasks"] = page
            body["pagination"] = [
                "total": narrowed.count, "limit": limit, "offset": offset,
                "has_more": offset + page.count < narrowed.count
            ]
            return (response(for: req), jsonData(body))
        }
    }

    private func pendingRow(_ id: String, due: String? = nil,
                            status: String = "pending") -> [String: Any] {
        var row: [String: Any] = ["id": id, "title": "T \(id)", "status": status,
                                  "updated_at": "2026-07-13T00:00:00Z"]
        if let due { row["due_date"] = due }
        return row
    }

    /// THE BUG. A lane whose only match sits past the first server page was
    /// invisible: the preset was applied to whatever rows the client happened
    /// to hold, so tapping Overdue searched the first 50 of 60 tasks and the
    /// reader had to tap Load more twice before the match appeared.
    func testOverdueLaneFindsAMatchPastTheFirstPageWithoutLoadingMore() {
        var rows = (1...59).map { pendingRow("fresh-\($0)", due: "2099-01-01") }
        rows.append(pendingRow("late", due: "2000-01-01"))
        let pool = TaskPool(rows)
        installTaskLaneBackend(pool)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == TasksViewModel.listPageSize }
        XCTAssertFalse(vm.tasks.contains { $0.id == "late" },
                       "precondition: the only overdue task is off the first page")

        vm.setFilter(.overdue)

        waitUntil { vm.visibleTasks.map(\.id) == ["late"] }
        XCTAssertFalse(vm.tasksHasMore, "one match, one page — nothing left to load")
    }

    func testTaskPageRequestCarriesTheLaneAndTheReaderLocalDate() {
        let log = RequestLog()
        installTaskLaneBackend(TaskPool([pendingRow("t")]), log: log)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { log.count >= 1 }
        vm.setFilter(.completed)
        waitUntil { log.count >= 2 }

        XCTAssertEqual(log.query(0)["view"], "all")
        XCTAssertEqual(log.query(1)["view"], "completed")

        // Derived without a DateFormatter, so a formatter quietly left on UTC
        // is caught rather than confirmed. `today` belongs to the reader: the
        // server computes the date lanes from it and cannot know their zone.
        let parts = Calendar.current.dateComponents([.year, .month, .day], from: Date())
        let readerToday = String(format: "%04d-%02d-%02d", parts.year!, parts.month!, parts.day!)
        XCTAssertEqual(log.query(0)["today"], readerToday)
        XCTAssertEqual(log.query(1)["today"], readerToday)
    }

    func testTodayIsTheReaderLocalDateAndNotTheUTCOne() {
        // 19:30Z on the 30th is already the 31st in Kolkata and still the 30th
        // in Los Angeles. Rendering a local instant in UTC — the bug the web
        // client shipped — names the wrong day for a whole day at a time, in
        // the local predicates AND in the `today=` the server's lane uses.
        let instant = Date(timeIntervalSince1970: 1_785_439_800)
        XCTAssertEqual(
            TasksViewModel.localDateISO(instant, in: TimeZone(identifier: "Asia/Kolkata")!),
            "2026-07-31"
        )
        XCTAssertEqual(
            TasksViewModel.localDateISO(instant, in: TimeZone(identifier: "America/Los_Angeles")!),
            "2026-07-30"
        )
        XCTAssertEqual(
            TasksViewModel.localDateISO(instant, in: TimeZone(identifier: "UTC")!),
            "2026-07-30"
        )
    }

    func testASelectedTagAsksForThePoolBecauseTheServerHasNoTagLane() {
        let pool = TaskPool([
            pendingRow("plain"),
            {
                var row = pendingRow("tagged", status: "completed")
                row["tags"] = [["id": "ops", "name": "ops"]]
                return row
            }()
        ])
        let log = RequestLog()
        installTaskLaneBackend(pool, log: log)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.map(\.id) == ["plain"] }   // view=all drops the completed row

        vm.toggleTag("ops")

        waitUntil { vm.visibleTasks.map(\.id) == ["tagged"] }
        let last = log.query(log.count - 1)
        XCTAssertNil(last["view"],
                     "a tag ignores the preset, and no server lane expresses it")
        XCTAssertNotNil(last["today"],
                        "today still travels so the badges keep their counts")
    }

    func testChangingTheLaneRestartsAtTheFirstPage() {
        let log = RequestLog()
        installTaskLaneBackend(TaskPool((1...120).map { pendingRow("t-\($0)") }), log: log)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == 50 }
        vm.loadMore(lane: .tasks)
        waitUntil { vm.tasks.count == 100 }
        XCTAssertEqual([log.query(0)["offset"], log.query(1)["offset"]], ["0", "50"])

        vm.setFilter(.inbox)

        waitUntil { log.count == 3 }
        // A lane change is the one refetch that DOES go back to one page: the
        // span was walked in the lane the reader has just left, so carrying it
        // over would ask the new lane for rows nobody has scrolled to.
        XCTAssertEqual([log.query(2)["limit"], log.query(2)["offset"]], ["50", "0"],
                       "a new lane is a new query, not a re-read of what is held")
        waitUntil { vm.tasks.count == 50 }

        // And the walk resumes from where the NEW lane's first page ended.
        vm.loadMore(lane: .tasks)
        waitUntil { log.count == 4 }
        XCTAssertEqual(log.query(3)["offset"], "50")
    }

    func testTheOldLaneDoesNotShowUnderTheNewChipWhileTheAnswerIsInFlight() {
        let pool = TaskPool([pendingRow("pending-one"), pendingRow("done", status: "completed")])
        let release = DispatchSemaphore(value: 0)
        let laneRequested = expectation(description: "the completed lane was requested")
        laneRequested.assertForOverFulfill = false
        installTaskLaneBackend(pool)
        let paged = MockURLProtocol.handler!
        MockURLProtocol.handler = { req in
            if (req.url!.query ?? "").contains("view=completed") {
                laneRequested.fulfill()
                _ = release.wait(timeout: .now() + 3)   // hold the answer in flight
            }
            return try paged(req)
        }

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.map(\.id) == ["pending-one"] }

        vm.setFilter(.completed)

        // The rows on hand still answer `all`. Showing them under the Completed
        // chip would be the old lane wearing the new label, so the local
        // predicate stands in until the server's answer lands.
        XCTAssertTrue(vm.visibleTasks.isEmpty)
        wait(for: [laneRequested], timeout: 2)
        release.signal()
        waitUntil { vm.visibleTasks.map(\.id) == ["done"] }
    }

    func testATickedTaskSurvivesTheLaneThatStopsReturningIt() {
        let pool = TaskPool([pendingRow("done-soon"), pendingRow("other")])
        installTaskLaneBackend(pool)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == 2 }
        let target = vm.tasks.first { $0.id == "done-soon" }!

        vm.setStatus(target, "completed")

        // The tick's own reload asks `view=all`, which no longer contains the
        // row. Vanishing on that reload is precisely what the grace period
        // exists to prevent, so the row is carried across it — beside the page,
        // not inside it, which keeps `tasks` exactly the server's answer.
        waitUntil { vm.gracedRows.map(\.id) == ["done-soon"] }
        XCTAssertEqual(vm.tasks.map(\.id), ["other"])
        XCTAssertTrue(vm.visibleTasks.contains { $0.id == "done-soon" })
    }

    /// THE BUG. Every mutation funnels through `load()`, which restarted at
    /// offset 0 with one page — so ticking a task on the third page collapsed
    /// a 150-row list back to 50, and took the grace period down with it: the
    /// just-ticked row was rebuilt from a `tasks` that no longer contained it,
    /// so the 5-second hold did not survive the collapse it triggered.
    func testATickOnALaterPageKeepsTheLoadedPagesAndTheGracedRow() {
        let pool = TaskPool((1...150).map { pendingRow(String(format: "t-%03d", $0)) })
        let log = RequestLog()
        installTaskLaneBackend(pool, log: log)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == 50 }
        vm.loadMore(lane: .tasks)
        waitUntil { vm.tasks.count == 100 }
        vm.loadMore(lane: .tasks)
        waitUntil { vm.tasks.count == 150 }
        XCTAssertEqual([log.query(1)["offset"], log.query(2)["offset"]], ["50", "100"])

        let target = vm.tasks[120]   // a row only the third page carries
        vm.setStatus(target, "completed")

        waitUntil(timeout: 4) { vm.tasks.count == 149 }
        XCTAssertEqual(log.query(3)["limit"], "150",
                       "the refetch asks for the span the reader loaded")
        XCTAssertEqual(log.query(3)["offset"], "0")
        XCTAssertFalse(vm.tasks.contains { $0.id == target.id },
                       "precondition: the server's lane has already dropped the ticked row")
        // The grace period exists to keep that row on screen for 5s. It has to
        // survive a refetch that reaches page three, not just one that reaches
        // page one.
        XCTAssertEqual(vm.gracedRows.map(\.id), [target.id])
        XCTAssertEqual(vm.visibleTasks.count, 150)
        XCTAssertTrue(vm.visibleTasks.contains { $0.id == target.id })
    }


    /// A binary from before this contract: rows and pagination, no `counts`,
    /// and `view=` ignored — so the response is the unfiltered pool.
    private func installLegacyUnlanedBackend() {
        MockURLProtocol.handler = { req in
            if req.url!.path.hasSuffix("/agents") {
                return (response(for: req), jsonData(["agents": []]))
            }
            if req.url!.path.hasSuffix("/tasks/internal") {
                return (response(for: req), jsonData(["tasks": []]))
            }
            let rows: [[String: Any]] = [
                ["id": "open", "title": "Open", "status": "pending",
                 "updated_at": "2026-07-13T00:00:00Z"],
                ["id": "done", "title": "Done", "status": "completed",
                 "updated_at": "2026-07-13T00:00:00Z"]
            ]
            return (response(for: req), jsonData([
                "tasks": rows,
                "pagination": ["total": rows.count, "limit": 50, "offset": 0, "has_more": false]
            ]))
        }
    }

    func testABinaryThatDoesNotAnswerLanesKeepsTheLocalPredicate() {
        // `counts` and `view=` shipped together, so no counts means the lane
        // was ignored and the pool came back whole. Trusting it as an answer
        // would put a completed task under All.
        installLegacyUnlanedBackend()
        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == 2 }

        XCTAssertEqual(vm.visibleTasks.map(\.id), ["open"])
        vm.setFilter(.completed)
        waitUntil { vm.visibleTasks.map(\.id) == ["done"] }
    }

    // MARK: - Lane badges (counts describe the corpus, absence describes nothing)

    func testLaneBadgesCountTheCorpusRatherThanTheLoadedPage() {
        var rows = (1...59).map { pendingRow("fresh-\($0)", due: "2099-01-01") }
        rows.append(pendingRow("late", due: "2000-01-01"))
        installTaskLaneBackend(TaskPool(rows))

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.laneCounts != nil }

        XCTAssertEqual(vm.tasks.count, 50, "only the first page is held")
        XCTAssertEqual(vm.laneCount(.all), 60, "the badge counts the pool, not the page")
        XCTAssertEqual(vm.laneCount(.inbox), 60)
        XCTAssertEqual(vm.laneCount(.overdue), 1)
        XCTAssertEqual(vm.laneCount(.completed), 0,
                       "a counted zero is an answer and keeps its badge")
    }

    func testAnUnreportedCountRendersNoBadgeRatherThanAZero() {
        installLegacyUnlanedBackend()
        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.tasks.count == 2 }

        for preset in TaskFilter.allCases {
            XCTAssertNil(vm.laneCount(preset),
                         "\(preset.rawValue) was never counted, so it has no number to show")
        }
    }

    func testTheInternalLaneIsNotCaptionedWithTheTaskLaneTotals() {
        installTaskLaneBackend(TaskPool([pendingRow("t")]))
        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.laneCount(.all) == 1 }

        vm.lane = .internalTasks

        XCTAssertNil(vm.laneCount(.all), "counts describe the /v3/tasks pool, not this one")
    }

    func testATickedTaskAndTheBadgeAboveItMoveTogether() {
        let pool = TaskPool([pendingRow("done-soon"), pendingRow("other")])
        installTaskLaneBackend(pool)

        let vm = TasksViewModel(session: makeMockSession())
        vm.load()
        waitUntil { vm.laneCount(.all) == 2 }
        let target = vm.tasks.first { $0.id == "done-soon" }!

        vm.setStatus(target, "completed")

        // The server counted stored state, where the tick already landed, so
        // its `all` is 1. The row is still on screen for the grace period, and
        // a badge reading 1 above two rows is the same defect from the other
        // end — so the badge is moved by the same row the list holds.
        waitUntil { vm.gracedRows.map(\.id) == ["done-soon"] }
        XCTAssertEqual(vm.visibleTasks.count, 2)
        XCTAssertEqual(vm.laneCount(.all), 2,
                       "the badge counts what is under it, not one fewer")
        XCTAssertEqual(vm.laneCount(.completed), 1,
                       "the lanes the reader is not looking at are reported as counted")
    }

    func testExecutePostsToExecute() {
        assertAction(method: "POST", pathContains: "/tasks/t/execute") { $0.execute($1) }
    }

    func testSetStatusPutsStatus() {
        assertAction(method: "PUT", pathContains: "/tasks/t/status",
                     bodyEquals: ["status": "completed"]) { $0.setStatus($1, "completed") }
    }

    func testResetPostsReadyAndSurfacesSuccess() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "reset request")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "PUT", req.url!.path.hasSuffix("/tasks/t/status") {
                XCTAssertNil(req.value(forHTTPHeaderField: "X-Principal"))
                XCTAssertNil(req.value(forHTTPHeaderField: "X-Workspace"))
                let json = requestBody(req).flatMap {
                    try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                }
                XCTAssertEqual(json?["status"] as? String, "ready")
                exp.fulfill()
                return (response(for: req), jsonData(["task": ["id": "t"]]))
            }
            return (response(for: req), jsonData(["tasks": []]))
        }

        vm.setStatus(task(status: "failed"), "ready")

        wait(for: [exp], timeout: 2)
        waitUntil { vm.mutatingTaskID == nil }
        XCTAssertEqual(vm.actionNoticeMessage, "Task reset to ready.")
        XCTAssertNil(vm.actionErrorMessage)
    }

    func testRejectedResetIsVisibleInsteadOfSilentlyReloading() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "rejected reset")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "PUT", req.url!.path.hasSuffix("/tasks/t/status") {
                exp.fulfill()
                return (response(for: req, status: 409), jsonData([
                    "error": "Task still has an active execution."
                ]))
            }
            return (response(for: req), jsonData(["tasks": []]))
        }

        vm.setStatus(task(status: "paused"), "ready")

        wait(for: [exp], timeout: 2)
        waitUntil { vm.mutatingTaskID == nil && vm.actionErrorMessage != nil }
        XCTAssertEqual(vm.actionErrorMessage, "Task still has an active execution.")
        XCTAssertNil(vm.actionNoticeMessage)
    }

    func testSetStatusRefusesManualCompletionWhileExecutionLifecycleIsActive() {
        let vm = TasksViewModel(session: makeMockSession())
        var requestCount = 0
        MockURLProtocol.handler = { req in
            requestCount += 1
            return (response(for: req), jsonData(["tasks": []]))
        }

        for status in ["queued", "running", "planning", "paused", "executing"] {
            vm.setStatus(task(status: status), "completed")
        }

        waitForMainQueue()
        XCTAssertEqual(requestCount, 0)
    }

    func testDeleteUsesRemoveFilesFalse() {
        assertAction(method: "DELETE", pathContains: "/tasks/t?remove_files=false") { $0.deleteTask($1) }
    }

    func testDeleteInternalTaskUsesDedicatedInternalEndpoint() {
        let vm = TasksViewModel(session: makeMockSession())
        let internalTask = fullTask("internal-1", lifecycle: "internal")
        let exp = expectation(description: "internal delete")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "DELETE",
               req.url?.path.hasSuffix("/tasks/internal/internal-1") == true {
                exp.fulfill()
            }
            return (response(for: req), jsonData(["tasks": []]))
        }

        vm.deleteTask(internalTask)

        wait(for: [exp], timeout: 2)
    }

    func testUpdateTaskSendsFields() {
        assertAction(method: "PUT", pathContains: "/tasks/t",
                     bodyEquals: ["priority": "p1"]) { $0.updateTask($1, fields: ["priority": "p1"]) }
    }

    func testUpdateScheduleSendsCanonicalCronShapeAndRetention() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "schedule update")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "PUT", req.url!.path.hasSuffix("/tasks/t"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
               let schedule = json["schedule"] as? [String: Any],
               let kind = schedule["kind"] as? [String: Any],
               let cron = kind["Cron"] as? [String: Any],
               let retention = schedule["execution_history_retention"] as? [String: Any] {
                XCTAssertEqual(cron["expression"] as? String, "0 9 * * 1-5")
                XCTAssertEqual(cron["timezone"] as? String, "Asia/Kolkata")
                XCTAssertEqual(schedule["timezone"] as? String, "Asia/Kolkata")
                XCTAssertEqual(retention["max_records"] as? Int, 8)
                XCTAssertEqual(retention["max_age_days"] as? Int, 30)
                exp.fulfill()
            }
            return (response(for: req), jsonData(["tasks": []]))
        }

        vm.updateSchedule(task(), cron: "0 9 * * 1-5", timezone: "Asia/Kolkata",
                          maxRecords: "8", maxDays: "30")

        wait(for: [exp], timeout: 2)
    }

    func testUpdateScheduleRejectsInvalidCronWithoutNetworkRequest() {
        let vm = TasksViewModel(session: makeMockSession())
        MockURLProtocol.handler = { req in
            XCTFail("Invalid cron should not send a request: \(req)")
            return (response(for: req), jsonData([:]))
        }

        vm.updateSchedule(task(), cron: "daily", timezone: "UTC", maxRecords: "", maxDays: "")

        XCTAssertEqual(
            vm.actionErrorMessage,
            "Schedule must be a five-field cron expression, for example 0 9 * * *."
        )
    }

    @MainActor
    func testCancelExecutionTargetsExecutionId() {
        let vm = TasksViewModel(session: makeMockSession())
        var t = task("t")
        t = try! JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: [
            "id": "t", "title": "T", "status": "running", "active_root_execution_id": "exec-9"
        ]))
        let exp = expectation(description: "cancel")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/executions/exec-9/cancel") { exp.fulfill() }
            return (response(for: req), jsonData(["tasks": []]))
        }
        vm.cancelExecution(t)
        wait(for: [exp], timeout: 2)
    }

    @MainActor
    func testCancelExecutionEncodesExecutionIdAsOnePathSegment() {
        let vm = TasksViewModel(session: makeMockSession())
        let t = try! JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: [
            "id": "t", "title": "T", "status": "running",
            "active_root_execution_id": "exec/with space"
        ]))
        let exp = expectation(description: "encoded cancel")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST" {
                XCTAssertTrue(req.url!.absoluteString.contains("/executions/exec%2Fwith%20space/cancel"))
                exp.fulfill()
            }
            return (response(for: req), jsonData(["tasks": []]))
        }

        vm.cancelExecution(t)

        wait(for: [exp], timeout: 2)
    }

    func testCreateTaskBuildsBody() {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: "create")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.path.hasSuffix("/tasks"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["title"] as? String, "New")
                XCTAssertEqual(json["agent_id"] as? String, "personal-assistant")
                XCTAssertEqual(json["ui_thread_id"] as? String, "general")
                XCTAssertEqual(json["priority"] as? String, "p1")
                XCTAssertEqual((json["tags"] as? [[String: Any]])?.first?["name"] as? String, "x")
                exp.fulfill()
            }
            return (response(for: req), jsonData(["task": ["id": "new-1"]]))
        }
        vm.createTask(title: "New", description: "", agentId: "personal-assistant",
                      threadId: "general", priority: "p1", dueDate: nil, tagNames: ["x"]) {}
        wait(for: [exp], timeout: 2)
    }

    // MARK: - Execution controls

    @MainActor
    func testExecutionControlStateDecodesAuthoritativeCapabilities() throws {
        let state = try JSONDecoder().decode(ExecutionControlState.self, from: jsonData([
            "execution_id": "exec-1",
            "waiting_state": "paused",
            "paused_from_state": "waiting_user",
            "pause_kind": "manual",
            "active": false,
            "can_pause": false,
            "can_resume": true,
            "can_steer": false,
            "can_cancel": true
        ]))

        XCTAssertEqual(state.executionId, "exec-1")
        XCTAssertEqual(state.waitingState, "paused")
        XCTAssertEqual(state.pausedFromState, "waiting_user")
        XCTAssertEqual(state.pauseKind, "manual")
        XCTAssertTrue(state.canResume)
        XCTAssertTrue(state.canCancel)
        XCTAssertFalse(state.canPause)
        XCTAssertFalse(state.canSteer)
        XCTAssertTrue(state.hasAvailableAction)
    }

    @MainActor
    func testExecutionControlCoordinatorSerializesPerExecutionAndInvalidates() {
        let coordinator = ExecutionControlCoordinator()
        let source = UUID()

        XCTAssertTrue(coordinator.begin(.pause, for: "exec-1"))
        XCTAssertFalse(coordinator.begin(.cancel, for: "exec-1"))
        XCTAssertTrue(coordinator.begin(.cancel, for: "exec-2"))
        XCTAssertEqual(coordinator.snapshot(for: "exec-1").busyAction, .pause)
        XCTAssertEqual(coordinator.snapshot(for: "exec-2").busyAction, .cancel)

        coordinator.finish(for: "exec-1", invalidatedBy: source)
        let finished = coordinator.snapshot(for: "exec-1")
        XCTAssertNil(finished.busyAction)
        XCTAssertEqual(finished.revision, 1)
        XCTAssertEqual(finished.invalidatedBy, source)
        XCTAssertTrue(coordinator.begin(.resume, for: "exec-1"))

        coordinator.invalidate("exec-2")
        XCTAssertEqual(coordinator.snapshot(for: "exec-2").revision, 1)
    }

    @MainActor
    func testExecutionControlConflictIsSurfacedWithoutSubmittingTheAction() {
        let coordinator = ExecutionControlCoordinator()
        let vm = ExecutionControlViewModel(
            executionId: "exec-busy",
            session: makeMockSession(),
            coordinator: coordinator
        )
        var requestCount = 0
        MockURLProtocol.handler = { req in
            requestCount += 1
            return (response(for: req), jsonData([:]))
        }
        XCTAssertTrue(coordinator.begin(.pause, for: "exec-busy"))

        XCTAssertFalse(vm.perform(.cancel))

        XCTAssertEqual(
            vm.errorMessage,
            "Another action is already updating this run. Try again when it finishes."
        )
        XCTAssertEqual(requestCount, 0)
    }

    @MainActor
    func testExecutionControlRefreshReloadsAfterCoordinatorInvalidation() {
        let coordinator = ExecutionControlCoordinator()
        let vm = ExecutionControlViewModel(
            executionId: "exec-invalidation",
            session: makeMockSession(),
            coordinator: coordinator
        )
        var gets = 0
        MockURLProtocol.handler = { req in
            if req.httpMethod == "GET" { gets += 1 }
            return (response(for: req), self.controlStateData(executionId: "exec-invalidation"))
        }

        vm.refresh(hostToken: "running|1", coordination: coordinator.snapshot(for: "exec-invalidation"))
        waitUntil { gets == 1 && !vm.isLoading }

        coordinator.invalidate("exec-invalidation")
        vm.refresh(hostToken: "running|1", coordination: coordinator.snapshot(for: "exec-invalidation"))
        waitUntil { gets == 2 && !vm.isLoading }
    }

    @MainActor
    func testExecutionControlRefreshQueuesInvalidationDuringInflightLoad() {
        let coordinator = ExecutionControlCoordinator()
        let vm = ExecutionControlViewModel(
            executionId: "exec-inflight",
            session: makeMockSession(),
            coordinator: coordinator
        )
        let firstRequestStarted = expectation(description: "first state request started")
        let releaseFirstRequest = DispatchSemaphore(value: 0)
        var gets = 0
        MockURLProtocol.handler = { req in
            if req.httpMethod == "GET" {
                gets += 1
                if gets == 1 {
                    firstRequestStarted.fulfill()
                    releaseFirstRequest.wait()
                }
            }
            return (response(for: req), self.controlStateData(executionId: "exec-inflight"))
        }

        vm.refresh(hostToken: "running|1", coordination: coordinator.snapshot(for: "exec-inflight"))
        wait(for: [firstRequestStarted], timeout: 2)
        coordinator.invalidate("exec-inflight")
        vm.refresh(hostToken: "running|1", coordination: coordinator.snapshot(for: "exec-inflight"))
        releaseFirstRequest.signal()

        waitUntil { gets == 2 && !vm.isLoading }
    }

    @MainActor
    func testExecutionControlLoadUsesV2PathAndBearerScope() {
        let vm = ExecutionControlViewModel(executionId: "exec/with slash", session: makeMockSession())
        let requested = expectation(description: "control-state request")
        MockURLProtocol.handler = { req in
            XCTAssertEqual(req.httpMethod, "GET")
            XCTAssertTrue(req.url!.absoluteString.contains("/executions/exec%2Fwith%20slash/control-state"))
            let query = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)?.queryItems
            XCTAssertNil(query?.first(where: { $0.name == "principal" }))
            XCTAssertNil(query?.first(where: { $0.name == "workspace" }))
            requested.fulfill()
            return (response(for: req), self.controlStateData())
        }

        vm.load()

        wait(for: [requested], timeout: 2)
        waitUntil { !vm.isLoading && vm.state?.canSteer == true }
        XCTAssertNil(vm.loadErrorMessage)
    }

    @MainActor
    func testExecutionControlMutationsUseCanonicalRequestsAndRefreshState() {
        let changed = expectation(description: "changed callbacks")
        changed.expectedFulfillmentCount = 4
        let vm = ExecutionControlViewModel(
            executionId: "exec-actions",
            session: makeMockSession(),
            onChanged: { changed.fulfill() }
        )
        var posts: [(String, Data?)] = []
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST" {
                posts.append((req.url!.lastPathComponent, requestBody(req)))
                return (response(for: req), jsonData([:]))
            }
            return (response(for: req), self.controlStateData())
        }

        for action in [ExecutionControlAction.pause, .resume, .cancel] {
            vm.perform(action)
            waitUntil { vm.busyAction == nil && posts.count == self.actionIndex(action) + 1 }
        }
        vm.perform(.steer, message: "Prioritize ₹ checks")
        waitUntil { vm.busyAction == nil && posts.count == 4 }
        wait(for: [changed], timeout: 2)

        XCTAssertEqual(posts.map(\.0), ["pause", "resume", "cancel", "steer"])
        XCTAssertNil(posts[0].1)
        XCTAssertNil(posts[1].1)
        XCTAssertNil(posts[2].1)
        let steer = try? JSONSerialization.jsonObject(with: posts[3].1 ?? Data()) as? [String: String]
        XCTAssertEqual(steer?["message"], "Prioritize ₹ checks")
        XCTAssertEqual(vm.state?.executionId, "exec-actions")
    }

    @MainActor
    func testExecutionControlMutationSurfacesStructuredBackendError() {
        let vm = ExecutionControlViewModel(executionId: "exec-error", session: makeMockSession())
        var stateRequests = 0
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST" {
                return (
                    response(for: req, status: 409),
                    jsonData(["code": "resource_conflict", "error": "Steer queue is full."])
                )
            }
            stateRequests += 1
            return (response(for: req), self.controlStateData(executionId: "exec-error"))
        }

        vm.perform(.steer, message: "Try again")

        waitUntil { vm.busyAction == nil && vm.errorMessage != nil }
        XCTAssertEqual(vm.errorMessage, "Steer queue is full.")
        XCTAssertEqual(stateRequests, 1)
        XCTAssertEqual(vm.state?.executionId, "exec-error")
        XCTAssertNil(vm.loadErrorMessage)
    }

    @MainActor
    func testExecutionControlReconciliationFailureRetainsStateAndCanRetry() {
        let coordinator = ExecutionControlCoordinator()
        let vm = ExecutionControlViewModel(
            executionId: "exec-reconcile",
            session: makeMockSession(),
            coordinator: coordinator
        )
        var stateRequests = 0
        MockURLProtocol.handler = { req in
            guard req.httpMethod == "GET" else {
                return (response(for: req), jsonData([:]))
            }
            stateRequests += 1
            if stateRequests == 2 {
                return (response(for: req, status: 503), jsonData(["message": "Refresh unavailable"]))
            }
            if stateRequests >= 3 {
                return (response(for: req), jsonData([
                    "execution_id": "exec-reconcile",
                    "waiting_state": "paused",
                    "active": false,
                    "can_pause": false,
                    "can_resume": true,
                    "can_steer": false,
                    "can_cancel": true
                ]))
            }
            return (response(for: req), self.controlStateData(executionId: "exec-reconcile"))
        }

        vm.load()
        waitUntil { !vm.isLoading && vm.state?.canPause == true }
        let priorState = vm.state

        vm.perform(.pause)
        waitUntil { vm.busyAction == nil && vm.loadErrorMessage != nil }
        XCTAssertEqual(vm.state, priorState)
        XCTAssertEqual(vm.loadErrorMessage, "Refresh unavailable")
        XCTAssertNil(vm.errorMessage)

        vm.load()
        waitUntil { !vm.isLoading && vm.state?.canResume == true }
        XCTAssertNil(vm.loadErrorMessage)
        XCTAssertEqual(stateRequests, 3)
    }

    @MainActor
    func testExecutionControlLoadFailureIsRetryableAndSeparateFromActionError() {
        let vm = ExecutionControlViewModel(executionId: "exec-load-error", session: makeMockSession())
        var attempts = 0
        MockURLProtocol.handler = { req in
            attempts += 1
            if attempts == 1 {
                return (response(for: req, status: 503), jsonData(["message": "Temporarily unavailable"] ))
            }
            return (response(for: req), self.controlStateData(executionId: "exec-load-error"))
        }

        vm.load()
        waitUntil { !vm.isLoading && vm.loadErrorMessage != nil }
        XCTAssertEqual(vm.loadErrorMessage, "Temporarily unavailable")
        XCTAssertNil(vm.errorMessage)

        vm.load()
        waitUntil { !vm.isLoading && vm.state != nil }
        XCTAssertNil(vm.loadErrorMessage)
        XCTAssertEqual(attempts, 2)
    }

    @MainActor
    func testExecutionControlSteerLimitCountsUtf8Bytes() {
        XCTAssertEqual(ExecutionControlViewModel.steerByteCount("a"), 1)
        XCTAssertEqual(ExecutionControlViewModel.steerByteCount("₹"), 3)
        XCTAssertEqual(ExecutionControlViewModel.steerByteCount(String(repeating: "₹", count: 1_365)), 4_095)
        XCTAssertEqual(ExecutionControlViewModel.steerByteCount(String(repeating: "₹", count: 1_366)), 4_098)
        XCTAssertEqual(ExecutionControlViewModel.maximumSteerBytes, 4_096)
    }

    @MainActor
    func testExecutionControlPauseTimeoutExceedsBackendBound() {
        XCTAssertGreaterThanOrEqual(ExecutionControlViewModel.timeoutInterval(for: .pause), 40)
        XCTAssertEqual(ExecutionControlViewModel.timeoutInterval(for: .resume), 15)
        XCTAssertEqual(ExecutionControlViewModel.timeoutInterval(for: .steer), 15)
        XCTAssertEqual(ExecutionControlViewModel.timeoutInterval(for: .cancel), 15)
    }

    func testChatExecutionControlTargetRequiresActiveStatus() {
        let running = TaskStatusModel(
            taskId: "t", title: "T", status: "running", steps: [],
            activeRootExecutionId: " exec-live "
        )
        let completed = TaskStatusModel(
            taskId: "t", title: "T", status: "completed", steps: [],
            activeRootExecutionId: "exec-history"
        )

        XCTAssertEqual(running.activeExecutionIdForControls, "exec-live")
        XCTAssertNil(completed.activeExecutionIdForControls)
    }

    func testTaskDetailSnapshotParsesCompleteExecutionPanelParityPayload() {
        let seed = TaskDetailSeed(TaskStatusModel(
            taskId: "task-1", title: "Fallback", status: "running", steps: []
        ))
        let taskPayload: [String: Any] = [
            "task": [
                "manifest": [
                    "title": "Research launch", "description": "**Investigate** the launch",
                    "agent_id": "researcher", "ui_thread_id": "launch", "priority": "p1",
                    "due_date": "2026-07-16", "created_by": "user", "lifecycle": "internal",
                    "chat_session_id": "chat-7", "created_at": "2026-07-15T06:00:00Z",
                    "tags": [["id": "urgent", "name": "urgent"]]
                ],
                "state": [
                    "status": "running", "active_root_execution_id": "exec-live",
                    "latest_root_execution_id": "exec-old", "updated_at": "2026-07-15T07:00:00Z"
                ],
                "refs": ["outputs": [["output_id": "o1", "relative_path": "report.md",
                                          "media_type": "text/markdown"]]]
            ]
        ]
        let panelPayload: [String: Any] = [
            "default_tab": "run",
            "overview": [
                "title": "Research launch", "description": "**Investigate** the launch",
                "status": "running", "assigned_agent_id": "researcher", "active_agent_id": "writer",
                "ui_thread_id": "launch", "progress": 62, "current_step": 1,
                "created_at": 1_752_559_200_000, "updated_at": 1_752_562_800_000,
                "execution_id": "exec-old"
            ],
            "run": [
                "summary": "The delegated writer is producing the brief.",
                "pending_questions": [[
                    "id": "q1", "question_text": "Which audience?", "status": "pending",
                    "options": [["value": "board", "label": "Board"]]
                ]],
                "needs_attention": [["id": "attention-1"]],
                "activity_log": [[
                    "id": "a1", "title": "Research complete", "summary": "Found three sources",
                    "status": "done", "item_type": "agent_message", "agent_id": "researcher",
                    "created_at": 1_752_562_700_000
                ]],
                "responsibility": [
                    "responsibility_summary": "Writer owns the active child", "active_owner_agent_id": "writer",
                    "waiting_state": "waiting_for_children", "current_stage": "compose",
                    "active_children": [[
                        "execution_id": "exec-child", "title": "Write brief",
                        "active_owner_agent_id": "writer", "waiting_state": "running", "is_blocking": true
                    ]]
                ]
            ],
            "output": [
                "result": ["summary": "Launch brief ready", "outcome": "success",
                           "artifact_names": ["report.md"]],
                "deliveries": [["id": "delivery-1"]],
                "recent_runs": [[
                    "execution_id": "exec-old", "status": "completed",
                    "started_at": 1_752_550_000_000, "ended_at": 1_752_551_000_000,
                    "completion_summary": "Previous run", "completion_artifact_names": ["old.md"]
                ]]
            ],
            "debug": [
                "selected_execution": [
                    "execution_id": "exec-old", "status": "completed", "progress": 100,
                    "started_at": 1_752_550_000_000, "ended_at": 1_752_551_000_000,
                    "plan_id": "plan-1", "linked_inputs": [["task_id": "upstream"]],
                    "step_statuses": [[
                        "number": 0, "name": "Research", "status": "completed",
                        "progress": "done", "step_id": "step-1", "capability": "browser"
                    ], [
                        "number": 1, "name": "Write", "status": "running",
                        "progress": "drafting", "step_id": "step-2", "delegate_agent_id": "writer"
                    ]]
                ],
                "taskplan": ["execution_id": "exec-old", "markdown": "# Plan\n- [x] Research"],
                "shell_entries": [[
                    "step_id": "step-1", "command": "search launch", "is_complete": true,
                    "exit_code": 0, "lines": [["text": "3 sources"]]
                ]],
                "observations": [["observation_id": "obs-1"]]
            ]
        ]
        let outputsPayload: [String: Any] = [
            "outputs": ["outputs": [[
                "output_id": "o1", "relative_path": "report.md", "media_type": "text/markdown",
                "role": "deliverable", "size_bytes": 2048, "body_snippet": "# Launch",
                "source_execution_id": "exec-old"
            ]]]
        ]
        let detailsPayload: [String: Any] = [
            "executions": [[
                "state": [
                    "execution_id": "exec-old", "status": "completed", "agent_id": "researcher",
                    "started_at": "2026-07-15T05:00:00Z", "completed_at": "2026-07-15T05:30:00Z"
                ],
                "refs": ["output_refs": [["relative_path": "report.md"]],
                         "child_output_refs": [["relative_path": "notes.md"]]],
                "artifacts": [["path": "capture.png"]]
            ]]
        ]
        let planPayload: [String: Any] = [
            "plan": ["plan_id": "plan-1", "status": "approved", "pending_questions": []]
        ]

        let snapshot = TaskDetailSnapshot.parse(
            seed: seed, taskPayload: taskPayload, panelPayload: panelPayload,
            outputsPayload: outputsPayload, detailsPayload: detailsPayload, planPayload: planPayload
        )

        XCTAssertEqual(snapshot.title, "Research launch")
        XCTAssertEqual(snapshot.activeRootExecutionId, "exec-live", "historical selection must not become a control target")
        XCTAssertEqual(snapshot.selectedExecutionId, "exec-old")
        // Derived, not read off `default_tab`: a running task's story is in the
        // Run act. The payload's own `default_tab` agrees here, which is exactly
        // why it is no longer consulted — two mechanisms agreeing is how one of
        // them goes untested.
        XCTAssertEqual(snapshot.defaultOpenTab(now: Date()), .run)
        XCTAssertEqual(snapshot.progress, 62)
        XCTAssertEqual(snapshot.steps.map(\.id), ["step-1", "step-2"])
        XCTAssertEqual(snapshot.activity.first?.title, "Research complete")
        XCTAssertEqual(snapshot.questions.first?.options, ["Board"])
        XCTAssertEqual(snapshot.attentionCount, 1)
        XCTAssertEqual(snapshot.responsibility?.children.first?.id, "exec-child")
        XCTAssertEqual(snapshot.artifacts.count, 1, "task refs and output endpoint must deduplicate")
        XCTAssertEqual(snapshot.history.first?.outputCount, 2)
        XCTAssertEqual(snapshot.history.first?.persistedArtifactCount, 1)
        XCTAssertEqual(snapshot.history.first?.outputPaths, ["report.md", "notes.md"])
        XCTAssertEqual(snapshot.history.first?.persistedArtifactLabels, ["capture.png"])
        XCTAssertEqual(snapshot.resultSummary, "Launch brief ready")
        XCTAssertEqual(snapshot.deliveryCount, 1)
        XCTAssertEqual(snapshot.shellEntries.first?.lines, ["3 sources"])
        XCTAssertEqual(snapshot.observationCount, 1)
        XCTAssertEqual(snapshot.linkedInputCount, 1)
        XCTAssertTrue(snapshot.visibleTabs.contains(.plan))
    }

    func testRunTimelineRetainsCanonicalTimingAndLLMMetadata() {
        let startMs = 1_752_550_000_000
        let panel: [String: Any] = [
            "overview": [
                "status": "completed", "execution_id": "exec-timed",
                "title": "Timed run", "description": "Inspect the timeline"
            ],
            "run": [
                "activity_log": [[
                    "id": "event-1", "title": "LLM succeeded", "summary": "Answer ready",
                    "status": "done", "item_type": "task", "agent_id": "researcher",
                    "created_at": startMs + 5_000,
                    "metadata": [
                        "event_type": "llm.succeeded", "capability": "chat.fast",
                        "latency_ms": 2_450, "model": "gpt-5.6-terra",
                        "input_tokens": 1_250, "output_tokens": 87,
                        "cache_read_tokens": 1_000,
                        "cost_usd": 0.00610875, "cost": 99.0
                    ]
                ], [
                    "id": "event-2", "title": "Tool succeeded", "status": "done",
                    "item_type": "task", "created_at": startMs + 65_000,
                    "metadata": [
                        "event_type": "tool.succeeded", "tool_name": "search_memory",
                        "latency_ms": 780
                    ]
                ]]
            ],
            "output": [:],
            "debug": [
                "selected_execution": [
                    "execution_id": "exec-timed", "status": "completed",
                    "started_at": startMs, "ended_at": startMs + 70_000,
                    "step_statuses": [[
                        "step_id": "step-1", "number": 0, "name": "Recall",
                        "status": "completed", "duration_ms": 12_000
                    ]]
                ]
            ]
        ]
        let seed = TaskDetailSeed(TaskStatusModel(
            taskId: "task-timed", title: "Timed run", status: "completed", steps: []
        ))
        let snapshot = TaskDetailSnapshot.parse(
            seed: seed, taskPayload: nil, panelPayload: panel,
            outputsPayload: nil, detailsPayload: nil, planPayload: nil
        )

        XCTAssertEqual(snapshot.activity.map(\.id), ["event-1", "event-2"])
        XCTAssertEqual(snapshot.activity[0].title, "Thinking with chat.fast")
        XCTAssertEqual(snapshot.activity[0].kind, "llm")
        XCTAssertEqual(snapshot.activity[0].latencyMs, 2_450)
        XCTAssertEqual(snapshot.activity[0].model, "gpt-5.6-terra")
        XCTAssertEqual(snapshot.activity[0].costUsd, 0.00610875)
        XCTAssertEqual(snapshot.activity[0].inputTokens, 1_250)
        XCTAssertEqual(snapshot.activity[0].cacheReadTokens, 1_000)
        XCTAssertEqual(snapshot.activity[1].title, "search_memory returned")
        XCTAssertEqual(snapshot.steps.first?.durationMs, 12_000)
        XCTAssertEqual(snapshot.runDuration(at: Date.distantFuture), 70)
        XCTAssertEqual(
            TaskTimelineFormatting.offset(
                event: snapshot.activity[1].timestamp, origin: snapshot.timelineOrigin
            ),
            "+1m 5s"
        )
        XCTAssertEqual(TaskTimelineFormatting.duration(milliseconds: 2_450), "2s")
        XCTAssertEqual(TaskTimelineFormatting.duration(milliseconds: 3_605_000), "1h")
        XCTAssertEqual(TaskTimelineFormatting.usd(0.00610875), "$0.00610875")
        XCTAssertEqual(
            TaskRunSummary.rows(
                executionId: snapshot.selectedExecutionId,
                activity: snapshot.activity,
                startedAt: snapshot.selectedExecutionStartedAt,
                endedAt: snapshot.selectedExecutionEndedAt
            ),
            [
                TaskRunSummaryRow(label: "Execution id", value: "exec-timed"),
                TaskRunSummaryRow(label: "Cost", value: "$0.00610875"),
                TaskRunSummaryRow(label: "Tokens", value: "1.3k → 87 tok · 1 call"),
                TaskRunSummaryRow(label: "Prompt cache", value: "80% cached · 1.0k of 1.3k tok"),
                TaskRunSummaryRow(label: "Model time", value: "3s of 1m 10s observed · 5%"),
                TaskRunSummaryRow(label: "Model", value: "gpt-5.6-terra")
            ]
        )
    }

    func testRunSummaryNeverEstimatesMissingDollarCostFromModelAndTokens() {
        let activity = TaskDetailActivity(
            id: "call-1", title: "LLM response", body: nil, status: "done",
            kind: "llm", agentId: nil, timestamp: Date(timeIntervalSince1970: 100),
            eventType: "llm.succeeded", latencyMs: 500, model: "gpt-5.6-terra",
            costUsd: nil, inputTokens: 12_000, outputTokens: 384,
            cacheReadTokens: 9_600
        )

        let rows = TaskRunSummary.rows(executionId: "exec-unpriced", activity: [activity])
        XCTAssertFalse(rows.contains { $0.label == "Cost" })
        XCTAssertTrue(rows.contains {
            $0 == TaskRunSummaryRow(label: "Tokens", value: "12k → 384 tok · 1 call")
        })
        XCTAssertNil(TaskTimelineFormatting.usd(nil))
        XCTAssertNil(TaskTimelineFormatting.usd(-0.01))
        XCTAssertEqual(TaskTimelineFormatting.usd(0), "$0.00")
    }

    func testRunSummaryAggregatesTheCompleteSelectedRunAndKeepsModelOrderOnTies() {
        let first = TaskDetailActivity(
            id: "call-1", title: "First response", body: nil, status: "done",
            kind: "llm", agentId: nil, timestamp: Date(timeIntervalSince1970: 100),
            eventType: "llm.succeeded", latencyMs: 4_200, model: "gpt-5.6-terra",
            costUsd: 0.00610875, inputTokens: 12_000, outputTokens: 384,
            cacheReadTokens: 9_600
        )
        let second = TaskDetailActivity(
            id: "call-2", title: "Second response", body: nil, status: "done",
            kind: "llm", agentId: nil, timestamp: Date(timeIntervalSince1970: 115),
            eventType: "llm.succeeded", latencyMs: 800, model: "claude-opus-4",
            costUsd: 0.0042, inputTokens: 1_000, outputTokens: 16,
            cacheReadTokens: nil
        )
        let failedTool = TaskDetailActivity(
            id: "tool-1", title: "Tool failed", body: nil, status: "failed",
            kind: "tool", agentId: nil, timestamp: Date(timeIntervalSince1970: 130),
            eventType: "tool.failed", latencyMs: nil, model: nil, costUsd: nil,
            inputTokens: nil, outputTokens: nil, cacheReadTokens: nil
        )

        XCTAssertEqual(
            TaskRunSummary.rows(
                executionId: "exec-complete",
                activity: [first, second, failedTool]
            ),
            [
                TaskRunSummaryRow(label: "Execution id", value: "exec-complete"),
                TaskRunSummaryRow(label: "Cost", value: "$0.01030875"),
                TaskRunSummaryRow(label: "Tokens", value: "13k → 400 tok · 2 calls"),
                TaskRunSummaryRow(label: "Prompt cache", value: "74% cached · 9.6k of 13k tok"),
                TaskRunSummaryRow(label: "Model time", value: "5s of 30s observed · 17%"),
                TaskRunSummaryRow(label: "Failed calls", value: "1"),
                TaskRunSummaryRow(
                    label: "Models",
                    value: "gpt-5.6-terra (1) · claude-opus-4 (1)"
                )
            ]
        )
    }

    func testTaskTimelineBoundsLongActivityToTheNewestRows() {
        let visible = TaskTimelineFormatting.latest(Array(0..<205))
        XCTAssertEqual(visible.count, 200)
        XCTAssertEqual(visible.first, 5)
        XCTAssertEqual(visible.last, 204)
    }

    func testRealtimePanelSnapshotReplacesLiveCollectionsButKeepsDownloadedOutput() {
        let seed = TaskDetailSeed(TaskStatusModel(
            taskId: "task-live", title: "Live", status: "running", steps: []
        ))
        let initial = TaskDetailSnapshot.parse(
            seed: seed,
            taskPayload: nil,
            panelPayload: [
                "overview": ["status": "running", "execution_id": "exec-live"],
                "run": ["activity_log": [["id": "old", "title": "Old", "status": "info"]]],
                "output": [:],
                "debug": ["selected_execution": ["execution_id": "exec-live", "status": "running"]]
            ],
            outputsPayload: ["outputs": ["outputs": [["relative_path": "report.md"]]]],
            detailsPayload: nil, planPayload: nil,
            unavailableSections: ["live run"]
        )
        let pushed: [String: Any] = [
            "overview": ["status": "completed", "execution_id": "exec-live", "progress": 100],
            "run": ["activity_log": [["id": "new", "title": "Finished", "status": "done"]]],
            "output": [:],
            "debug": ["selected_execution": ["execution_id": "exec-live", "status": "completed"]]
        ]

        let updated = initial.applyingRealtimePanel(seed: seed, panelPayload: pushed)
        XCTAssertEqual(updated?.activity.map(\.id), ["new"], "a full delta replaces, rather than appends")
        XCTAssertEqual(updated?.status, "completed")
        XCTAssertEqual(updated?.progress, 100)
        XCTAssertEqual(updated?.artifacts.map(\.relativePath), ["report.md"])
        XCTAssertEqual(updated?.unavailableSections, [])

        var wrong = pushed
        wrong["overview"] = ["status": "completed", "execution_id": "exec-other"]
        XCTAssertNil(initial.applyingRealtimePanel(seed: seed, panelPayload: wrong))
    }

    @MainActor
    func testTaskDetailRealtimeAcceptsOnlyExactScopedExecutionSnapshots() throws {
        let stream = TaskDetailRealtime()
        var received: [[String: Any]] = []
        stream.start(executionId: "exec-visible") { received.append($0) }
        defer { stream.stop() }

        func frame(principal: String = MagicianAccess.principal,
                   workspace: String = MagicianAccess.workspace,
                   execution: String) throws -> String {
            let object: [String: Any] = [
                "event_type": "ExecutionPanelDelta",
                "data": [
                    "principal": principal, "workspace": workspace,
                    "state": [
                        "overview": ["status": "running", "execution_id": execution],
                        "run": ["activity_log": []], "output": [:], "debug": [:]
                    ]
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: object)
            return try XCTUnwrap(String(data: data, encoding: .utf8))
        }

        XCTAssertFalse(try stream.handleIncomingJSON(frame(execution: "exec-other")))
        XCTAssertFalse(try stream.handleIncomingJSON(frame(principal: "someone-else", execution: "exec-visible")))
        XCTAssertTrue(try stream.handleIncomingJSON(frame(execution: "exec-visible")))
        XCTAssertEqual(received.count, 1)
    }

    // MARK: - The verdict layer, wired

    /// An outputs request that answered, with nothing in it. Distinct from `nil`
    /// — which is a request that did not answer — and the distinction is the
    /// whole point of `hasOutputAct`.
    private var emptyOutputsPayload: [String: Any] {
        ["outputs": ["outputs": [Any]()] as [String: Any]]
    }

    /// Build a snapshot from the pieces a verdict actually reads, so each case
    /// below pins one mapping rather than a whole payload.
    private func verdictSnapshot(
        status: String = "running",
        lastProgressAt: String? = nil,
        currentStep: Int? = nil,
        stepNames: [String] = [],
        needsAttention: [[String: Any]] = [],
        startedAt: Any? = nil,
        endedAt: Any? = nil,
        errorMessage: String? = nil,
        hasPlan: Bool = false,
        outputs: [String: Any]? = nil
    ) -> TaskDetailSnapshot {
        var state: [String: Any] = ["status": status]
        if let lastProgressAt = lastProgressAt { state["last_progress_at"] = lastProgressAt }
        var selected: [String: Any] = ["execution_id": "exec-1"]
        if let startedAt = startedAt { selected["started_at"] = startedAt }
        if let endedAt = endedAt { selected["ended_at"] = endedAt }
        if let errorMessage = errorMessage { selected["error_message"] = errorMessage }
        selected["step_statuses"] = stepNames.enumerated().map { offset, name -> [String: Any] in
            ["number": offset, "name": name, "status": "pending", "step_id": "s\(offset)"]
        }
        var overview: [String: Any] = ["status": status, "has_plan": hasPlan]
        if let currentStep = currentStep { overview["current_step"] = currentStep }
        let taskRecord: [String: Any] = ["manifest": [String: Any](), "state": state]
        let panel: [String: Any] = [
            "overview": overview,
            "run": ["needs_attention": needsAttention] as [String: Any],
            "output": [String: Any](),
            "debug": ["selected_execution": selected] as [String: Any]
        ]
        return TaskDetailSnapshot.parse(
            seed: TaskDetailSeed(TaskStatusModel(
                taskId: "task-v", title: "Verdict", status: status, steps: []
            )),
            taskPayload: ["task": taskRecord],
            panelPayload: panel,
            outputsPayload: outputs, detailsPayload: nil, planPayload: nil
        )
    }

    func testSnapshotReadsLastProgressAtAndReportsAStalledRun() {
        // `last_progress_at` is the one field this layer added a reader for; the
        // panel fetched it inside `/v3/tasks/{id}` and threw it away.
        let progressed = Date(timeIntervalSince1970: 1_752_562_000)
        let snapshot = verdictSnapshot(
            status: "running",
            lastProgressAt: ISO8601DateFormatter().string(from: progressed),
            currentStep: 3, stepNames: ["A", "B", "C", "Searching memory", "E", "F", "G"]
        )
        XCTAssertEqual(snapshot.lastProgressAt, progressed)

        let stalled = snapshot.verdict(now: progressed.addingTimeInterval(6 * 60))
        XCTAssertEqual(stalled.state, .stalled)
        XCTAssertEqual(stalled.headline, "Stalled · no progress for 6m")
        XCTAssertEqual(stalled.detail, "Still on step 4: Searching memory")

        // The same snapshot a minute earlier is not stalled — the verdict is a
        // function of `now`, so nothing here is baked in at parse time.
        XCTAssertEqual(snapshot.verdict(now: progressed.addingTimeInterval(60)).state, .running)
    }

    func testSnapshotCountsStepsTheWayAReaderDoes() {
        // The wire's `current_step` is 0-based and the verdict line is 1-based.
        // Seven steps and a current step of 3 — three values that all differ, so
        // an off-by-one or a swapped pair cannot pass.
        let snapshot = verdictSnapshot(
            currentStep: 3, stepNames: ["A", "B", "C", "D", "E", "F", "G"]
        )
        let input = snapshot.verdictInput(now: Date())
        XCTAssertEqual(input.currentStep, 4)
        XCTAssertEqual(input.totalSteps, 7)
        XCTAssertEqual(snapshot.verdict(now: Date()).headline, "Running · step 4 of 7")
    }

    func testSnapshotRanksARespondableAskAboveTheStatusAndCarriesItsInstant() {
        let raised = Date(timeIntervalSince1970: 1_752_562_000)
        let snapshot = verdictSnapshot(
            status: "completed",
            needsAttention: [[
                "id": "a1", "summary": "A lane label",
                "hitl_request": [
                    "source": "diff_approval", "prompt": "Apply 3 file edits?",
                    "at": raised.timeIntervalSince1970 * 1000
                ]
            ]]
        )
        XCTAssertEqual(snapshot.attention?.source, .diffApproval)
        XCTAssertEqual(snapshot.attention?.summary, "Apply 3 file edits?")
        XCTAssertEqual(snapshot.attention?.raisedAt, raised)

        let verdict = snapshot.verdict(now: raised.addingTimeInterval(4 * 60))
        XCTAssertEqual(verdict.state, .waiting, "a finished task with an open ask is waiting")
        XCTAssertEqual(verdict.headline, "Waiting on you · 4m")
        XCTAssertEqual(verdict.detail, "Apply 3 file edits?")
    }

    func testSnapshotIgnoresAnAttentionRowThatIsNotAnAsk() {
        // A terminal-failure row reaches the same list. Ranking it as `waiting`
        // would paint a failed task `Waiting on you` and hide the error text —
        // which is why the filter is `hitl_request` and not the row's presence.
        let snapshot = verdictSnapshot(
            status: "failed",
            needsAttention: [["id": "a1", "summary": "Execution failed"]],
            errorMessage: "Couldn't read revenue.csv"
        )
        XCTAssertEqual(snapshot.attentionCount, 1, "the row still counts for the attention card")
        XCTAssertNil(snapshot.attention)
        let verdict = snapshot.verdict(now: Date())
        XCTAssertEqual(verdict.state, .failed)
        XCTAssertEqual(verdict.detail, "Couldn't read revenue.csv")
    }

    func testSnapshotSkipsAnAskWhoseSourceThisClientDoesNotModel() {
        let snapshot = verdictSnapshot(
            status: "running",
            needsAttention: [["id": "a1", "hitl_request": ["source": "telepathy"]]]
        )
        XCTAssertNil(snapshot.attention)
        XCTAssertEqual(snapshot.verdict(now: Date()).state, .running)
    }

    func testSnapshotOmitsTheElapsedTimeWhenTheRunHasNotEnded() {
        let started: Any = 1_752_550_000_000
        XCTAssertNil(verdictSnapshot(status: "completed", startedAt: started).runElapsed)
        XCTAssertEqual(
            verdictSnapshot(status: "completed", startedAt: started).verdict(now: Date()).headline,
            "Finished"
        )
        XCTAssertEqual(
            verdictSnapshot(
                status: "completed", startedAt: started, endedAt: 1_752_550_192_000
            ).verdict(now: Date()).headline,
            "Finished · 3m 12s"
        )
    }

    func testActsAreAbsentRatherThanEmptyWhenTheirPayloadFailedToLoad() {
        // The §6 rule at the seam: `nil` payload IS the load failure, and an
        // empty Output act would assert "no output" about something never
        // observed.
        let unloaded = verdictSnapshot(status: "completed")
        XCTAssertFalse(unloaded.hasOutputAct)
        XCTAssertEqual(unloaded.acts, [.run])
        XCTAssertEqual(unloaded.visibleTabs, [.overview, .run, .history])

        // Loaded and empty is a different answer: the act exists and can say so.
        let empty = verdictSnapshot(status: "completed", outputs: emptyOutputsPayload)
        XCTAssertTrue(empty.hasOutputAct)
        XCTAssertEqual(empty.visibleTabs, [.overview, .run, .output, .history])
    }

    func testARunActSurvivesAPanelOutageWhenSomethingElseSawTheExecution() {
        // The panel request is what carries the run, but the details request
        // carries history. Dropping the Run tab because one of two sources
        // failed would hide runs we can actually show.
        let snapshot = TaskDetailSnapshot.parse(
            seed: TaskDetailSeed(TaskStatusModel(
                taskId: "t", title: "T", status: "completed", steps: []
            )),
            taskPayload: nil, panelPayload: nil, outputsPayload: nil,
            detailsPayload: ["executions": [[
                "state": ["execution_id": "exec-9", "status": "completed"],
                "refs": [String: Any](), "artifacts": [Any]()
            ]]],
            planPayload: nil, unavailableSections: ["live run", "outputs"]
        )
        XCTAssertTrue(snapshot.hasRunAct)
        XCTAssertFalse(snapshot.hasOutputAct)
    }

    func testTheTabStripFollowsLifecycleOrderAndKeepsOverviewAndHistory() {
        let planned = verdictSnapshot(
            status: "running", hasPlan: true, outputs: emptyOutputsPayload
        )
        XCTAssertEqual(planned.visibleTabs, [.overview, .plan, .run, .output, .history])
    }

    func testTheOpenTabFollowsTheAskRatherThanTheState() {
        // Both of these tasks are `waiting`; only the source says where the
        // answer lives, which is the whole reason it is a parameter.
        func opened(_ source: String) -> TaskDetailTab {
            verdictSnapshot(
                status: "running",
                needsAttention: [["id": "a", "hitl_request": ["source": source]]],
                hasPlan: true, outputs: emptyOutputsPayload
            ).defaultOpenTab(now: Date())
        }
        XCTAssertEqual(opened("plan_approval"), .plan)
        XCTAssertEqual(opened("diff_approval"), .run)
    }

    func testTheOpenTabFallsBackToOverviewOnlyWhenTheTaskHasNoActs() {
        let bare = TaskDetailSnapshot.parse(
            seed: TaskDetailSeed(TaskStatusModel(
                taskId: "t", title: "T", status: "pending", steps: []
            )),
            taskPayload: nil, panelPayload: nil, outputsPayload: nil,
            detailsPayload: nil, planPayload: nil
        )
        XCTAssertEqual(bare.acts, [])
        XCTAssertEqual(bare.visibleTabs, [.overview, .history])
        XCTAssertEqual(bare.defaultOpenTab(now: Date()), .overview)
    }

    func testTaskAndRunPresentationsRemainDistinct() {
        XCTAssertTrue(TaskDetailPresentation.taskDetails.showsTaskTabs)
        XCTAssertEqual(TaskDetailPresentation.taskDetails.navigationTitle, "Task")
        XCTAssertFalse(TaskDetailPresentation.runInspection.showsTaskTabs)
        XCTAssertEqual(TaskDetailPresentation.runInspection.navigationTitle, "Run activity")
    }

    func testTaskDetailSnapshotRetainsInternalHistoryWhenPanelIsUnavailable() {
        let source = fullTask(
            "internal-2", status: "completed", agent: "system-agent",
            lifecycle: "internal", createdBy: "__system__"
        )
        let seed = TaskDetailSeed(source)
        let details: [String: Any] = [
            "task": [
                "manifest": [
                    "title": "Nightly memory sweep", "description": "Compact old episodes",
                    "agent_id": "system-agent", "lifecycle": "internal", "created_by": "__system__"
                ],
                "state": ["status": "completed", "latest_root_execution_id": "exec-nightly"],
                "refs": ["outputs": [["relative_path": "memory-summary.json",
                                          "media_type": "application/json"]]]
            ],
            "executions": [[
                "state": [
                    "execution_id": "exec-nightly", "status": "completed", "agent_id": "system-agent",
                    "started_at": "2026-07-15T01:00:00Z", "completed_at": "2026-07-15T01:01:00Z"
                ],
                "refs": ["output_refs": [["relative_path": "memory-summary.json"]]],
                "artifacts": []
            ]]
        ]

        let snapshot = TaskDetailSnapshot.parse(
            seed: seed, taskPayload: nil, panelPayload: nil, outputsPayload: nil,
            detailsPayload: details, planPayload: nil,
            unavailableSections: ["live run", "outputs"]
        )

        XCTAssertEqual(snapshot.title, "Nightly memory sweep")
        XCTAssertEqual(snapshot.status, "completed")
        XCTAssertNil(snapshot.activeRootExecutionId)
        XCTAssertEqual(snapshot.latestRootExecutionId, "exec-nightly")
        XCTAssertEqual(snapshot.artifacts.map(\.relativePath), ["memory-summary.json"])
        XCTAssertEqual(snapshot.history.map(\.id), ["exec-nightly"])
        XCTAssertEqual(snapshot.history.first?.outputCount, 1)
        XCTAssertEqual(snapshot.unavailableSections, ["live run", "outputs"])
        XCTAssertFalse(snapshot.visibleTabs.contains(.plan))
    }

    @MainActor
    func testTaskDetailSelectionRefreshIgnoresStaleExecutionResponse() {
        let seed = TaskDetailSeed(TaskStatusModel(
            taskId: "task-race", title: "Race", status: "completed", steps: []
        ))
        let model = TaskDetailViewModel(seed: seed) { seed, executionId in
            if executionId == "old" { try? await Task.sleep(nanoseconds: 160_000_000) }
            else { try? await Task.sleep(nanoseconds: 10_000_000) }
            return TaskDetailSnapshot.parse(
                seed: seed, taskPayload: nil,
                panelPayload: [
                    "overview": ["status": "completed", "execution_id": executionId ?? "initial"],
                    "run": [:], "output": [:],
                    "debug": ["selected_execution": ["execution_id": executionId ?? "initial"]]
                ],
                outputsPayload: nil, detailsPayload: nil, planPayload: nil
            )
        }

        model.selectExecution("old")
        model.selectExecution("new")
        waitUntil { model.snapshot?.selectedExecutionId == "new" }
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))

        XCTAssertEqual(model.snapshot?.selectedExecutionId, "new")
        XCTAssertEqual(model.selectedExecutionId, "new")
    }

    @MainActor
    func testDeepWorkTaskDetailRefreshIgnoresAnOlderResponse() {
        var calls = 0
        let model = DeepWorkTaskDetailModel(
            initial: DeepWorkTaskDetailProjection(
                description: "Initial",
                agentId: nil,
                status: "running",
                activeRootExecutionId: "exec-initial"
            )
        ) { _ in
            calls += 1
            let call = calls
            if call == 1 {
                try? await Task.sleep(nanoseconds: 180_000_000)
                return DeepWorkTaskDetailProjection(
                    description: "Stale",
                    agentId: "old-agent",
                    status: "running",
                    activeRootExecutionId: "exec-old"
                )
            }
            try? await Task.sleep(nanoseconds: 10_000_000)
            return DeepWorkTaskDetailProjection(
                description: "Current",
                agentId: "new-agent",
                status: "paused",
                activeRootExecutionId: "exec-new"
            )
        }

        model.refresh(taskId: "task-1")
        waitUntil { calls == 1 }
        model.refresh(taskId: "task-1")
        waitUntil { model.projection.description == "Current" }
        RunLoop.main.run(until: Date().addingTimeInterval(0.25))

        XCTAssertEqual(model.projection.description, "Current")
        XCTAssertEqual(model.projection.activeRootExecutionId, "exec-new")
    }

    @MainActor
    func testExecutionControlRejectsInvalidSteerBeforeNetworkRequest() {
        let vm = ExecutionControlViewModel(executionId: "exec-invalid-steer", session: makeMockSession())
        var requestCount = 0
        MockURLProtocol.handler = { req in
            requestCount += 1
            return (response(for: req), jsonData([:]))
        }

        vm.perform(.steer, message: "   \n")
        XCTAssertEqual(vm.errorMessage, "Steer guidance cannot be empty.")
        XCTAssertNil(vm.busyAction)

        vm.perform(.steer, message: String(repeating: "₹", count: 1_366))
        XCTAssertEqual(vm.errorMessage, "Steer guidance must be 4,096 bytes or less.")
        XCTAssertNil(vm.busyAction)
        XCTAssertEqual(requestCount, 0)
    }

    private func controlStateData(executionId: String = "exec-actions") -> Data {
        jsonData([
            "execution_id": executionId,
            "waiting_state": "executing",
            "active": true,
            "can_pause": true,
            "can_resume": false,
            "can_steer": true,
            "can_cancel": true
        ])
    }

    private func actionIndex(_ action: ExecutionControlAction) -> Int {
        switch action {
        case .pause: return 0
        case .resume: return 1
        case .cancel: return 2
        case .steer: return 3
        }
    }

    // Shared: fire an action, assert the outbound request, tolerate the follow-up refresh GETs.
    private func assertAction(method: String, pathContains: String, bodyEquals: [String: String]? = nil,
                              _ action: (TasksViewModel, TaskV3) -> Void) {
        let vm = TasksViewModel(session: makeMockSession())
        let exp = expectation(description: pathContains)
        MockURLProtocol.handler = { req in
            if req.httpMethod == method, req.url!.absoluteString.contains(pathContains) {
                if let want = bodyEquals, let body = requestBody(req),
                   let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                    for (k, v) in want { XCTAssertEqual(json[k] as? String, v) }
                }
                exp.fulfill()
            }
            return (response(for: req), jsonData(["tasks": []]))
        }
        action(vm, task("t"))
        wait(for: [exp], timeout: 2)
    }
}
