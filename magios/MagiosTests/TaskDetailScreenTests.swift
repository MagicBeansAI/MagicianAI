import XCTest
@testable import Magician

/// The task detail screen's pure parts: act order and the open act, which
/// files show in Output versus Technical details, the poll gate, the sticky
/// title threshold, the ⋯ menu and its requests, and the snapshot fields the
/// acts read (plan-step marks, planning activity, plan steps).
@MainActor
final class TaskDetailScreenTests: XCTestCase {
    private let now = Date()

    private func snapshot(_ taskId: String) -> TaskDetailSnapshot {
        let load = { (path: String) in TaskDetailFixtures.payload(path: "/tasks/\(taskId)" + path, taskId: taskId, now: self.now) }
        let seed = TaskDetailRoute(taskId: taskId).seed
        return TaskDetailSnapshot.parse(
            seed: seed,
            taskPayload: load(""),
            panelPayload: load("/execution-panel"),
            outputsPayload: load("/outputs"),
            detailsPayload: load("/details"),
            planPayload: load("/plan"))
    }

    // MARK: - Acts

    func testAFinishedTaskLeadsWithOutputAndOpensIt() {
        let done = snapshot("fixture-detail-done")
        XCTAssertEqual(TaskDetailLayout.actOrder(done, now: now), [.output, .plan, .run])
        XCTAssertEqual(TaskDetailLayout.initialOpenAct(done, focus: nil, inspectRun: false, now: now), .output)
    }

    func testARunningTaskLeadsWithRun() {
        let running = snapshot("fixture-detail-running")
        XCTAssertEqual(TaskDetailLayout.actOrder(running, now: now).first, .run)
        XCTAssertEqual(TaskDetailLayout.initialOpenAct(running, focus: nil, inspectRun: false, now: now), .run)
    }

    func testAnEntryPointFocusWinsWhenItsActExists() {
        let done = snapshot("fixture-detail-done")
        XCTAssertEqual(TaskDetailLayout.initialOpenAct(done, focus: .plan, inspectRun: false, now: now), .plan)
        XCTAssertEqual(TaskDetailLayout.initialOpenAct(done, focus: nil, inspectRun: true, now: now), .run)
        XCTAssertEqual(TaskDetailLayout.initialOpenAct(done, focus: .result, inspectRun: true, now: now), .output)
    }

    func testOnlyThePlanActKeepsItsSummaryWhileOpen() {
        XCTAssertTrue(TaskDetailLayout.keepsSummaryWhenOpen(.plan))
        XCTAssertFalse(TaskDetailLayout.keepsSummaryWhenOpen(.run))
        XCTAssertFalse(TaskDetailLayout.keepsSummaryWhenOpen(.output))
    }

    func testClosedCardsCarryTheirSummaries() {
        let done = snapshot("fixture-detail-done")
        XCTAssertEqual(TaskDetailLayout.summary(for: .output, done, now: now),
                       "launch-brief-for-the-mobile-app-final.md and numbers.csv")
        XCTAssertEqual(TaskDetailLayout.summary(for: .plan, done, now: now), "approved 15m ago")
        let running = snapshot("fixture-detail-running")
        XCTAssertEqual(TaskDetailLayout.summary(for: .run, running, now: now), "Step 2 of 4 · Draft the brief")
    }

    // MARK: - Output vs technical details

    func testOutputShowsDeliverablesOnlyAndTechnicalDetailsTheRest() {
        let done = snapshot("fixture-detail-done")
        XCTAssertEqual(TaskDetailLayout.outputFiles(done).map(\.displayName),
                       ["launch-brief-for-the-mobile-app-final.md", "numbers.csv"])
        let technical = TaskDetailLayout.technicalSections(done)
        XCTAssertEqual(technical.map(\.scope), [.internal])
        XCTAssertEqual(Set(technical.flatMap(\.files).map(\.displayName)), ["research-notes.md", "continuation.json"])
        XCTAssertTrue(technical.flatMap(\.files).allSatisfy { !$0.deliverable })
    }

    func testThePreviewIsTheFirstReadableDeliverableAndCopyPathPrefersTheAbsolutePath() {
        let files = TaskDetailLayout.outputFiles(snapshot("fixture-detail-done"))
        XCTAssertEqual(TaskDetailLayout.defaultPreview(files)?.displayName, "launch-brief-for-the-mobile-app-final.md")
        XCTAssertEqual(TaskDetailLayout.copyPath(files[0]),
                       "/Users/me/MagicianNotes/tasks/fixture/outputs/launch-brief-for-the-mobile-app-final.md")
        XCTAssertEqual(TaskDetailLayout.copyPath(files[1]), "outputs/numbers.csv")
        XCTAssertEqual(TaskDetailLayout.fileMeta(files[0]), "Markdown · 5 KB")
    }

    // MARK: - Run checklist and planning

    func testTheRunChecklistCarriesMarksAndNotesAndNotReachedAfterTheRun() {
        let failed = snapshot("fixture-detail-failed")
        XCTAssertTrue(failed.runEnded)
        XCTAssertEqual(failed.planStepRows.map(\.mark), [.done, .failed, .notReached, .notReached])
        XCTAssertEqual(failed.planStepRows[1].note, "The source spreadsheet could not be opened.")
        let running = snapshot("fixture-detail-running")
        XCTAssertFalse(running.runEnded)
        XCTAssertEqual(running.planStepRows.map(\.mark), [.done, .inProgress, .pending, .pending])
    }

    func testPlanningActivityIsLiveWhilePlanningAndCarriesModelCallCost() {
        let planning = snapshot("fixture-detail-planning")
        XCTAssertTrue(TaskDetailLayout.planningActivityIsLive(planning))
        XCTAssertEqual(planning.planningEntries.count, 4)
        let call = planning.planningEntries.first { $0.isModelCall }
        XCTAssertEqual(call?.costUsd, 0.004)
        XCTAssertEqual(call?.tokens, 3100)
        XCTAssertFalse(TaskDetailLayout.planningActivityIsLive(snapshot("fixture-detail-done")))
    }

    func testThePlanActListsThePlanGraphSteps() {
        XCTAssertEqual(TaskDetailLayout.planSteps(snapshot("fixture-detail-approved")),
                       ["Gather the product facts", "Draft the brief", "Check the numbers", "Polish and export"])
        XCTAssertEqual(TaskDetailLayout.planSteps(snapshot("fixture-detail-planning")), [])
    }

    func testARunPushedForAnotherExecutionDoesNotOverwriteTheSelectedRun() {
        let running = snapshot("fixture-detail-running")
        let other: [String: Any] = ["overview": ["execution_id": "exec-other", "status": "running"]]
        XCTAssertNil(running.applyingRealtimePanel(seed: TaskDetailRoute(taskId: "fixture-detail-running").seed,
                                                    panelPayload: other))
    }

    // MARK: - Poll gate and sticky title

    func testPollsOnlyWhileVisibleAndPlanningOrRunning() {
        XCTAssertTrue(TaskDetailLayout.shouldPoll(visible: true, status: "running"))
        XCTAssertTrue(TaskDetailLayout.shouldPoll(visible: true, status: "planning"))
        XCTAssertTrue(TaskDetailLayout.shouldPoll(visible: true, status: "pending", planStatus: "planning"))
        XCTAssertFalse(TaskDetailLayout.shouldPoll(visible: false, status: "running"))
        XCTAssertFalse(TaskDetailLayout.shouldPoll(visible: true, status: "completed"))
        XCTAssertFalse(TaskDetailLayout.shouldPoll(visible: true, status: "ready", planStatus: "approved"))
        XCTAssertFalse(TaskDetailLayout.shouldPoll(visible: true, status: "paused"))
        XCTAssertEqual(TaskDetailLayout.pollInterval, 4)
    }

    func testTheCompactTitleShowsOnceTheHeaderHasScrolledAway() {
        XCTAssertFalse(TaskDetailLayout.compactTitleVisible(headerBottom: 180))
        XCTAssertFalse(TaskDetailLayout.compactTitleVisible(headerBottom: 1))
        XCTAssertTrue(TaskDetailLayout.compactTitleVisible(headerBottom: 0))
        XCTAssertTrue(TaskDetailLayout.compactTitleVisible(headerBottom: -40))
    }

    // MARK: - ⋯ menu

    func testTheMenuOffersWhatTheOldPanelOffered() {
        let running = TaskDetailMenu.actions(snapshot("fixture-detail-running"))
        XCTAssertFalse(running.contains(.run))
        XCTAssertTrue(running.contains(.replan))
        XCTAssertTrue(running.contains(.cancelTask))
        XCTAssertFalse(running.contains(.markComplete))
        XCTAssertEqual(running.last, .delete)

        let approved = TaskDetailMenu.actions(snapshot("fixture-detail-approved"))
        XCTAssertEqual(approved.first, .run)

        let done = TaskDetailMenu.actions(snapshot("fixture-detail-done"))
        XCTAssertTrue(done.contains(.markNotDone))
        XCTAssertFalse(done.contains(.cancelTask))

        let failed = TaskDetailMenu.actions(snapshot("fixture-detail-failed"))
        XCTAssertTrue(failed.contains(.reset))
    }

    func testEachActionMapsToTheTaskEndpoint() {
        let make = { (action: TaskDetailAction) in
            TaskDetailActionRequest.make(action, taskId: "t 1", planId: "p1",
                                         synthesisExecutionId: "e1", lifecycle: nil)
        }
        XCTAssertEqual(make(.run), .init(method: "POST", path: "/tasks/t%201/execute", body: [:], notice: "Run started."))
        XCTAssertEqual(make(.cancelTask)?.body, ["status": "cancelled"])
        XCTAssertEqual(make(.approvePlan)?.path, "/tasks/t%201/plan/approve?plan_id=p1")
        XCTAssertEqual(make(.retrySynthesis)?.path, "/tasks/t%201/executions/e1/retry-synthesis")
        XCTAssertEqual(make(.delete)?.path, "/tasks/t%201?remove_files=false")
        XCTAssertEqual(TaskDetailActionRequest.make(.delete, taskId: "t", planId: nil, synthesisExecutionId: nil,
                                                    lifecycle: "internal")?.path, "/tasks/internal/t")
        XCTAssertNil(TaskDetailActionRequest.make(.approvePlan, taskId: "t", planId: nil,
                                                  synthesisExecutionId: nil, lifecycle: nil))
    }

    // MARK: - Route and model

    func testARouteIsTheSameScreenWhateverRidesAlong() {
        var a = TaskDetailRoute(taskId: "t", executionId: "e", titleHint: "One")
        let b = TaskDetailRoute(taskId: "t", executionId: "e", titleHint: "Two")
        XCTAssertEqual(a, b)
        XCTAssertEqual(a.hashValue, b.hashValue)
        a.focus = .plan
        XCTAssertNotEqual(a, b)
        XCTAssertEqual(TaskDetailRoute(taskId: "t", executionId: "e").seed.latestRootExecutionId, "e")
    }

    func testTheModelLoadsFiveEndpointsThenPollsOnlyThePanelAndPlan() async {
        final class Recorder: @unchecked Sendable {
            private let lock = NSLock()
            private var items: [String] = []
            func add(_ path: String) { lock.lock(); items.append(path); lock.unlock() }
            func take() -> [String] { lock.lock(); defer { items = []; lock.unlock() }; return items }
        }
        let paths = Recorder()
        let now = self.now
        let model = TaskDetailScreenModel(seed: TaskDetailRoute(taskId: "fixture-detail-running").seed) { path in
            paths.add(path)
            return TaskDetailFixtures.payload(path: path, taskId: "fixture-detail-running", now: now)
        }
        await model.refresh()
        XCTAssertEqual(paths.take().count, 5)
        XCTAssertEqual(model.snapshot?.status, "running")
        XCTAssertEqual(model.selectedExecutionId, "exec-fixture")
        await model.pollPanel()
        XCTAssertEqual(paths.take().sorted(), [
            "/tasks/fixture-detail-running/execution-panel?execution_id=exec-fixture",
            "/tasks/fixture-detail-running/plan"
        ])
    }

    func testObservedWorkSurvivesAStalePollForTheSameRootOnly() async {
        let seed = TaskDetailRoute(taskId: "task-progress").seed
        var root = "run"
        let model = TaskDetailScreenModel(seed: seed) { path in
            if path.contains("execution-panel") {
                return ["overview": ["execution_id": root, "status": "running"], "run": ["activity_log": []]]
            }
            if path == "/tasks/task-progress" { return ["task": ["state": ["status": "running", "active_root_execution_id": root, "last_progress_at": 1000]]] }
            throw NSError(domain: "TaskDetailFixture", code: 404)
        }
        await model.refresh()
        model.applyRealtimePanel(["overview": ["execution_id": "run", "status": "running"], "run": ["activity_log": [
            ["id": "tool", "created_at": 5000, "metadata": ["event_type": "tool.call.started"]]]]])
        XCTAssertEqual(model.snapshot?.lastProgressAt, Date(timeIntervalSince1970: 5))
        await model.refresh()
        XCTAssertEqual(model.snapshot?.lastProgressAt, Date(timeIntervalSince1970: 5))
        root = "new-run"
        await model.refresh()
        XCTAssertEqual(model.snapshot?.progressExecutionId, "new-run")
        XCTAssertEqual(model.snapshot?.lastProgressAt, Date(timeIntervalSince1970: 1))
    }

    // MARK: - Asks answered in place

    private func row(_ object: [String: Any]) -> AttentionItem {
        try! JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// A pending ask for `task` as a feed row (`metadata.hitl_request`).
    private func feedRow(id: String, correlationId: String, task: String, inputType: String = "text",
                         options: [[String: Any]]? = nil, context: [String: Any]? = nil) -> [String: Any] {
        var schema: [String: Any] = ["type": inputType]
        if let options { schema["options"] = options }
        if let context { schema["context"] = context }
        return ["id": id, "title": "Waiting", "item_type": "request", "status": "needs_action",
                "task_id": task, "created_at": 1_700_000_000_000, "updated_at": 1_700_000_000_000,
                "metadata": ["source": "user_request",
                             "hitl_request": ["correlation_id": correlationId, "input_type": inputType,
                                              "prompt": "Question \(correlationId)?", "input_schema": schema]]]
    }

    private var relentlessReview: AttentionItem {
        row(feedRow(id: "feed-review", correlationId: "feed-only", task: "task-1", inputType: "choice",
                    options: [["id": "approve", "label": "Approve draft"], ["id": "reject", "label": "Reject draft"]],
                    context: ["review_href": "/work?view=quality&run=run-7&task=task-1",
                              "review_label": "Review draft"]))
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), jsonData([:])) }
        PendingHitlTracker.shared.reset()
        super.tearDown()
    }

    func testThePanelsAnswerableAsksAreParsedAsFeedRows() {
        var topLevelOnly = feedRow(id: "panel-top", correlationId: "panel-top", task: "task-1")
        let target = (topLevelOnly["metadata"] as! [String: Any])["hitl_request"]!
        topLevelOnly["metadata"] = ["source": "user_request"]
        topLevelOnly["hitl_request"] = target
        let failure: [String: Any] = ["id": "failed-1", "title": "Run failed", "item_type": "failed",
                                      "status": "needs_action", "task_id": "task-1", "metadata": [:]]
        let rows = TaskDetailSnapshot.parseAskRows([
            feedRow(id: "panel-ask", correlationId: "panel-ask", task: "task-1"), failure, topLevelOnly,
        ])
        XCTAssertEqual(rows.map(\.correlationId), ["panel-ask", "panel-top"],
                       "an informational failure row is not an ask")
        XCTAssertEqual(rows.last?.owningTaskId, "task-1")
        XCTAssertEqual(rows.last?.prompt, "Question panel-top?")
    }

    func testTheDeepPanelListsAFeedOnlyAskMergedAndDeduped() throws {
        let panel = TaskDetailSnapshot.parseAskRows([
            feedRow(id: "panel-ask", correlationId: "panel-ask", task: "task-1"),
            feedRow(id: "panel-two", correlationId: "panel-two", task: "task-1"),
        ])
        let feed = [
            row(feedRow(id: "feed-dup", correlationId: "panel-ask", task: "task-1")),
            relentlessReview,
            row(feedRow(id: "feed-other", correlationId: "other-task", task: "task-2")),
        ]
        let items = ComposerAskModel.taskItems(taskId: "task-1", panelRows: panel, feed: feed)
        XCTAssertEqual(items.map(\.id), ["panel-ask", "feed-only", "panel-two"],
                       "feed asks for this task first, then the panel's own; one card per correlation id")

        let review = try XCTUnwrap(items[1].review)
        XCTAssertEqual(review.label, "Review draft")
        XCTAssertEqual(review.workTarget?.kind, .qualityRun)
        guard case .choices(let choices) = items[1].form else { return XCTFail("approve / reject inline") }
        XCTAssertEqual(choices.map(\.id), ["approve", "reject"])

        let settled = ComposerAskModel.taskItems(taskId: "task-1", panelRows: panel, feed: [],
                                                 isSettled: { $0.correlationId == "panel-two" })
        XCTAssertEqual(settled.map(\.id), ["panel-ask"], "an ask settled anywhere leaves the panel")
    }

    func testAFeedOnlyAskIsAnsweredInPlaceAndClearsEverywhere() throws {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.pages = AttentionPages()
        vm.lanes["requests"] = [relentlessReview]
        var posted: [String: Any]?
        var path: String?
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" {
                path = request.url?.path
                posted = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
            }
            return (response(for: request), jsonData([:]))
        }
        let item = try XCTUnwrap(ComposerAskModel.taskItems(
            taskId: "task-1", panelRows: [], feed: vm.items("all")).first)
        guard case .choices(let choices) = item.form, case .attention(let source) = item.source else {
            return XCTFail("a feed choice ask answers inline")
        }
        XCTAssertNil(vm.answerInPlace(source, form: item.form, reply: .choice(choices[0])))

        XCTAssertTrue(vm.items("all").isEmpty, "the row leaves Attention (and the dock) at once")
        XCTAssertTrue(vm.isSettled(source))
        XCTAssertTrue(ComposerAskModel.taskItems(taskId: "task-1", panelRows: [source], feed: vm.items("all"),
                                                 isSettled: { vm.isSettled($0) }).isEmpty,
                      "the panel's own copy of the row is settled too")
        waitUntil { posted != nil }
        XCTAssertEqual(path, "/api/magician/v2/hitl/feed-only/respond")
        XCTAssertEqual((posted?["value"] as? [String: Any])?["selected_id"] as? String, "approve")
    }

    func testAPanelOnlyAskIsNotRefusedAsStaleWhenTheFeedPagesMissIt() throws {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.pages = AttentionPages()
        vm.lanes["requests"] = []
        var posts = 0
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" {
                posts += 1
                return (response(for: request), jsonData(["accepted": true]))
            }
            return (response(for: request), jsonData([:]))
        }
        let panelRow = try XCTUnwrap(TaskDetailSnapshot.parseAskRows([
            feedRow(id: "panel-ask", correlationId: "panel-ask", task: "task-1"),
        ]).first)
        XCTAssertTrue(vm.isAnsweredElsewhere(panelRow), "absent from the loaded feed pages")
        XCTAssertNil(vm.answerInPlace(panelRow, form: .text(placeholder: "", guidance: false),
                                      reply: .text("ship it"), vouched: true))
        waitUntil { posts == 1 }
        XCTAssertTrue(vm.isSettled(panelRow))

        // A realtime resolution still wins over the panel's word.
        let other = try XCTUnwrap(TaskDetailSnapshot.parseAskRows([
            feedRow(id: "panel-late", correlationId: "panel-late", task: "task-1"),
        ]).first)
        vm.noteRemoteResolution(correlationId: "panel-late")
        _ = vm.answerInPlace(other, form: .text(placeholder: "", guidance: false), reply: .text("late"),
                             vouched: true)
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertEqual(posts, 1, "a stale answer never reaches /respond")
    }

    // MARK: - Completion kind

    private func completedSnapshot(_ state: [String: Any]) -> TaskDetailSnapshot {
        var record = state
        record["status"] = "completed"
        return TaskDetailSnapshot.parse(
            seed: TaskDetailRoute(taskId: "partly").seed,
            taskPayload: ["task": ["manifest": ["title": "Ship it"], "state": record]],
            panelPayload: nil, outputsPayload: nil, detailsPayload: nil, planPayload: nil)
    }

    func testACompletedPartialTaskHeadsPartlyDoneWithItsFirstOpenItem() {
        let partly = completedSnapshot(["completion_kind": "partial",
                                        "open_items": ["Blocked on the API key", "Tests"]])
        XCTAssertTrue(partly.isPartlyDone)
        XCTAssertEqual(partly.openItems, ["Blocked on the API key", "Tests"])
        let verdict = partly.displayVerdict(now: now)
        XCTAssertEqual(verdict.state, .finished, "act order keeps reading a finished task")
        XCTAssertEqual(verdict.headline, "Partly done")
        XCTAssertEqual(verdict.detail, "Open: Blocked on the API key · +1 more")
        XCTAssertEqual(TaskDetailVerdictLine.tone(for: verdict.state, partlyDone: partly.isPartlyDone), .partial)
    }

    func testACompletedFullOrUnmarkedTaskStaysFinished() {
        for state in [["completion_kind": "full"], [:]] as [[String: Any]] {
            let done = completedSnapshot(state)
            XCTAssertFalse(done.isPartlyDone)
            XCTAssertEqual(done.displayVerdict(now: now), done.verdict(now: now))
            XCTAssertEqual(TaskDetailVerdictLine.tone(for: .finished, partlyDone: done.isPartlyDone), .done)
        }
    }
}
