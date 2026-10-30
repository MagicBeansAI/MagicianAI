import XCTest
@testable import Magician

/// A scripted VibeDev server: reads return what the test set, writes are recorded.
final class StubVibeDevAPI: VibeDevServing {
    var live = VibeDevLiveRunsResponse(runs: [])
    var projectList = VibeDevProjectsResponse(projects: [], activeProjectId: nil)
    var turns: VibeDevRunTurns?
    var controlError: Error?
    var failReads = false
    /// Root runs `runsPage` pages through, newest first (server cap: 50 a page).
    var runRoots: [VibeDevTaskItem] = []
    var historyPages: [String: VibeDevRunHistory] = [:]
    private(set) var historyCursors: [String?] = []
    /// Seconds each next `runTurns` / `liveRuns` call waits, in call order. A
    /// call returns what the stub held when it was made.
    var turnsDelays: [Double] = []
    var liveDelays: [Double] = []
    /// Seconds a `changes` read for a turn (nil: the whole run) waits.
    var changesDelays: [String?: Double] = [:]

    private(set) var controlCalls: [(String, VibeDevControlAction, String?)] = []
    private(set) var crewActions: [(String, VibeDevCrewAction)] = []
    private(set) var chooseCalls: [Int?] = []
    private(set) var answers: [(String, String)] = []
    private(set) var startedRuns: [VibeDevStartRunRequest] = []
    private(set) var diffAnswers: [(String, Bool)] = []
    private(set) var discards: [String] = []
    private(set) var gitInits: [String] = []
    /// What `gitInit` throws, when set.
    var gitInitError: Error?
    /// Calls arrive from concurrent tasks off the main actor.
    private let lock = NSLock()
    private func record(_ change: () -> Void) { lock.lock(); change(); lock.unlock() }
    private func nextDelay(_ queue: ReferenceWritableKeyPath<StubVibeDevAPI, [Double]>) -> Double {
        lock.lock(); defer { lock.unlock() }
        return self[keyPath: queue].isEmpty ? 0 : self[keyPath: queue].removeFirst()
    }
    private func wait(_ seconds: Double) async {
        if seconds > 0 { try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000)) }
    }

    func liveRuns() async throws -> VibeDevLiveRunsResponse {
        if failReads { throw URLError(.notConnectedToInternet) }
        let value = live
        await wait(nextDelay(\.liveDelays))
        return value
    }
    func runHistory(limit: Int, cursor: String?) async throws -> VibeDevRunHistory {
        if failReads { throw URLError(.notConnectedToInternet) }
        historyCursors.append(cursor)
        return historyPages[cursor ?? "first"] ?? VibeDevRunHistory(runs: live.runs, hasMore: false, nextCursor: nil)
    }
    func projects() async throws -> VibeDevProjectsResponse {
        if failReads { throw URLError(.notConnectedToInternet) }
        return projectList
    }
    func runsPage(projectID: String, limit: Int, offset: Int, query: String) async throws -> VibeDevRunsPage {
        let page = Array(runRoots.dropFirst(offset).prefix(min(limit, 50)))
        return VibeDevRunsPage(tasks: page, pagination: VibeDevRunsPagination(
            runs: page.count, totalRuns: runRoots.count, offset: offset,
            hasMore: offset + page.count < runRoots.count, nextCursor: nil))
    }
    func crews(projectID: String) async throws -> [VibeDevCrew] {
        projectList.projects.first { $0.projectId == projectID }?.crews ?? []
    }
    func runTurns(runID: String) async throws -> VibeDevRunTurns {
        let value = turns
        await wait(nextDelay(\.turnsDelays))
        // Like URLSession: a read whose task was cancelled fails.
        if Task.isCancelled { throw CancellationError() }
        guard let value else { throw VibeDevAPIError.http(404, "") }
        return value
    }
    func timeline(turnID: String, limit: Int) async throws -> VibeDevTimeline {
        VibeDevTimeline(taskId: turnID, cards: [], truncated: false)
    }
    func changes(runID: String, turn: String?, path: String?) async throws -> VibeDevRunChanges {
        await wait(changesDelays[turn] ?? 0)
        return VibeDevRunChanges(runId: runID, turn: turn, source: "branch", files: [], patch: nil)
    }
    func repoSuggestions() async throws -> [VibeDevRepoSuggestion] { [] }
    func codingProfiles() async throws -> [VibeDevCodingProfile] { [] }
    func startRun(_ request: VibeDevStartRunRequest) async throws -> VibeDevStartRunResponse {
        record { startedRuns.append(request) }
        return VibeDevStartRunResponse(taskId: "task_new")
    }
    func createProject(name: String, repoPath: String) async throws -> VibeDevProject {
        VibeDevProject(projectId: "p_new", name: name, repoPath: repoPath)
    }
    func updateProject(id: String, trust: VibeDevTrust?, baseBranch: String?) async throws -> VibeDevProject {
        VibeDevProject(projectId: id, name: "x", trust: trust ?? .sandboxed, baseBranch: baseBranch)
    }
    func gitInit(projectID: String) async throws -> VibeDevGitInitResponse {
        record { gitInits.append(projectID) }
        if let gitInitError { throw gitInitError }
        let data = Data(#"{"project_id": "\#(projectID)", "health": {"status": "ok"}}"#.utf8)
        return try JSONDecoder().decode(VibeDevGitInitResponse.self, from: data)
    }
    func control(liveTaskID: String, action: VibeDevControlAction, message: String?) async throws {
        if let controlError { throw controlError }
        record { controlCalls.append((liveTaskID, action, message)) }
    }
    func createCrew(projectID: String, request: VibeDevCreateCrewRequest) async throws -> VibeDevCrew {
        VibeDevCrew(crewId: "c_new", goal: request.goal, status: .proposed)
    }
    func crewAction(projectID: String, crewID: String, action: VibeDevCrewAction) async throws -> VibeDevCrew {
        record { crewActions.append((crewID, action)) }
        return VibeDevCrew(crewId: crewID, goal: "g", status: .running)
    }
    func chooseCrew(projectID: String, crewID: String, laneIndex: Int?) async throws -> VibeDevCrew {
        record { chooseCalls.append(laneIndex) }
        return VibeDevCrew(crewId: crewID, goal: "g", status: laneIndex == nil ? .cancelled : .ready)
    }
    func answerQuestion(requestID: String, optionID: String) async throws {
        record { answers.append((requestID, optionID)) }
    }
    func answerDiffApproval(approvalID: String, apply: Bool) async throws {
        record { diffAnswers.append((approvalID, apply)) }
    }
    func merge(rootID: String) async throws {}
    func openPullRequest(rootID: String, title: String, body: String) async throws -> String? { nil }
    func discard(rootID: String) async throws { record { discards.append(rootID) } }
}

final class StubApprovals: VibeDevApprovals {
    var outcome: VibeDevApprovalOutcome = .sent
    private(set) var decisions: [([String], Bool)] = []
    @MainActor func decide(taskIDs: [String], apply: Bool) async -> VibeDevApprovalOutcome {
        decisions.append((taskIDs, apply))
        return outcome
    }
}

@MainActor
final class VibeDevViewModelTests: XCTestCase {
    private var api: StubVibeDevAPI!
    private var approvals: StubApprovals!
    private var context: VibeDevContext!

    override func setUp() async throws {
        api = StubVibeDevAPI()
        approvals = StubApprovals()
        context = VibeDevContext(api: api, approvals: approvals, undo: VibeDevUndoCenter(delay: 0.1, haptics: false), cache: nil)
    }

    private func run(_ id: String, _ state: VibeDevRunState, lane: String? = nil,
                     updated: String = "2026-10-13T09:41:00Z") -> VibeDevLiveRun {
        VibeDevLiveRun(runId: id, liveTaskId: "\(id)_live", projectId: "p1", projectName: "Shop",
                       title: "VibeDev · \(id)", latestTurnTitle: nil, state: state,
                       status: state == .done ? "completed" : "running", step: "Editing a.ts", summary: nil,
                       turns: 1, crewId: lane == nil ? nil : "c1", lane: lane, updatedAt: updated,
                       lastProgressAt: nil)
    }

    private func lane(_ name: String, _ status: VibeDevLaneStatus, role: VibeDevLaneRole = .work) -> VibeDevCrewLane {
        VibeDevCrewLane(lane: name, runTaskId: "t_\(name)", status: status, role: role)
    }

    private func sleep(_ seconds: Double) async {
        try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }

    // MARK: Needs you

    func testNeedsYouPutsCrewQuestionsNewestFirstThenRunsInServerOrder() async {
        let crews = [
            VibeDevCrew(crewId: "start", goal: "a", status: .proposed, startRequestId: "q1", updatedAtMs: 100),
            VibeDevCrew(crewId: "race", goal: "b", status: .choosing, mode: .race,
                        lanes: [lane("A1", .done), lane("A2", .done)], chooseRequestId: "q2", updatedAtMs: 300),
            VibeDevCrew(crewId: "busy", goal: "c", status: .running, lanes: [lane("L", .running)], updatedAtMs: 400),
            VibeDevCrew(crewId: "pr", goal: "d", status: .integrated, pullRequestRequestId: "q4",
                        branch: "vibedev/crew", updatedAtMs: 200),
            VibeDevCrew(crewId: "merged", goal: "e", status: .integrated, branch: "b",
                        pullRequestUrl: "https://example.invalid/pr/1", updatedAtMs: 500),
        ]
        let archived = VibeDevProject(projectId: "old", name: "Old",
                                      crews: [VibeDevCrew(crewId: "gone", goal: "x", status: .proposed, updatedAtMs: 999)],
                                      archived: true)
        let projects = [VibeDevProject(projectId: "p1", name: "Shop", trust: .trusted, crews: crews), archived]
        let runs = [run("r1", .working), run("r2", .needsYou), run("r3", .done), run("r4", .needsYou)]

        let items = VibeDevQuestions.needsYou(projects: projects, liveRuns: runs)
        XCTAssertEqual(items.map(\.id), ["crew:race:choose", "crew:pr:pr", "crew:start:start", "run:r2", "run:r4"])
        XCTAssertEqual(items.first?.eyebrow, "VibeDev race · Shop")
        XCTAssertEqual(items[1].options.map(\.action), [.openCrewPullRequest, .notNow])
        XCTAssertEqual(items[2].options.map(\.action), [.startCrew, .dontStartCrew])
    }

    func testRaceKeepOptionsUseLaneIndexesOfFinishedWorkLanes() throws {
        let crew = VibeDevCrew(crewId: "c", goal: "g", status: .choosing, mode: .race, lanes: [
            lane("Attempt 1", .done), lane("Attempt 2", .failed), lane("Attempt 3", .done),
            lane("Review", .done, role: .review),
        ])
        let item = try XCTUnwrap(VibeDevQuestions.question(for: crew, projectID: "p", projectName: ""))
        XCTAssertEqual(item.options.map(\.action), [
            .keepLane(index: 0, lane: "Attempt 1"), .keepLane(index: 2, lane: "Attempt 3"), .keepNone,
        ])
        XCTAssertEqual(item.options.map(\.style), [.primary, .plain, .danger])
    }

    func testConflictAsksOnlyWhileTheResolveQuestionIsOpen() {
        let open = VibeDevCrew(crewId: "c", goal: "g", status: .conflict, resolveRequestId: "q",
                               conflict: VibeDevCrewConflict(lane: "UI", files: ["web/api.ts"]))
        XCTAssertEqual(VibeDevQuestions.question(for: open, projectID: "p", projectName: "")?.body,
                       "UI conflicts in web/api.ts.")
        let answered = VibeDevCrew(crewId: "c", goal: "g", status: .conflict, resolveRequestId: nil)
        XCTAssertNil(VibeDevQuestions.question(for: answered, projectID: "p", projectName: ""))
        XCTAssertTrue(answered.isFinished)
    }

    func testHistoryWireDecodesOriginalFolderAndAttentionCount() throws {
        let data = Data(#"{"runs":[{"run_id":"r","live_task_id":"turn","project_id":"p","project_name":"App","title":"Change title","state":"needs_you","status":"running","turns":2,"updated_at":"2026-10-22T00:00:00Z","work_location":"original","repo_path":"/owner/app","needs_you_count":2}],"has_more":true,"next_cursor":"2026-10-22T00:00:00Z|r"}"#.utf8)
        let page = try JSONDecoder().decode(VibeDevRunHistory.self, from: data)
        XCTAssertTrue(page.hasMore)
        XCTAssertEqual(page.runs.first?.workLocation, "original")
        XCTAssertEqual(page.runs.first?.needsYouCount, 2)
        XCTAssertEqual(page.runs.first?.repoPath, "/owner/app")
        XCTAssertEqual(page.nextCursor, "2026-10-22T00:00:00Z|r")
    }

    func testRunHistoryPagesAcrossProjectsAndRefreshKeepsOlderRows() async {
        var first = run("new", .working)
        first.needsYouCount = 2
        first.workLocation = "original"
        first.repoPath = "/owner/repo"
        let older = run("old", .done, updated: "2020-01-01T00:00:00Z")
        api.historyPages["first"] = VibeDevRunHistory(runs: [first], hasMore: true, nextCursor: "cursor-one")
        api.historyPages["cursor-one"] = VibeDevRunHistory(runs: [first, older], hasMore: false, nextCursor: nil)
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        XCTAssertEqual(model.history.map(\.runId), ["new"])
        XCTAssertEqual(model.history.first?.workLocation, "original")
        XCTAssertEqual(model.history.first?.needsYouCount, 2)
        await model.loadMore()
        XCTAssertEqual(model.history.map(\.runId), ["new", "old"])
        XCTAssertNil(model.nextCursor)
        await model.refresh()
        XCTAssertEqual(model.history.map(\.runId), ["new", "old"])
        XCTAssertEqual(api.historyCursors.compactMap { $0 }, ["cursor-one"])
        api.historyPages["first"] = VibeDevRunHistory(runs: [run("newer", .working)], hasMore: true, nextCursor: "new-head")
        await model.refresh()
        XCTAssertEqual(model.nextCursor, "new-head") // offline activity can exceed the old first page
        XCTAssertEqual(Set(model.history.map(\.runId)), Set(["newer", "new", "old"]))
    }

    func testRefreshSplitsWorkingAndJustFinished() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("a", .working), run("b", .paused), run("c", .done),
                                                  run("d", .failed), run("e", .needsYou)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        XCTAssertTrue(model.loaded)
        XCTAssertNotNil(model.lastUpdated)
        XCTAssertEqual(model.working.map(\.runId), ["a", "b"])
        XCTAssertEqual(model.justFinished.map(\.runId), ["c", "d"])
        XCTAssertEqual(model.needsYou.map(\.id), ["run:e"])
    }

    func testFailedRefreshKeepsTheLastData() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("a", .working)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let firstRead = model.lastUpdated
        api.failReads = true
        await model.refresh()
        XCTAssertTrue(model.refreshFailed, "the screen says it is stale")
        XCTAssertNil(model.errorMessage, "a later failure keeps the data instead of an error")
        XCTAssertEqual(model.working.map(\.runId), ["a"])
        XCTAssertEqual(model.lastUpdated, firstRead)
        api.failReads = false
        await model.refresh()
        XCTAssertFalse(model.refreshFailed)
    }

    func testAFirstReadThatFailsSaysWhy() async {
        api.failReads = true
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        XCTAssertTrue(model.loaded)
        XCTAssertNotNil(model.errorMessage)
    }

    // MARK: Swipe actions and undo

    func testSwipeActionsOnlyOnLiveRuns() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("w", .working), run("p", .paused),
                                                  run("n", .needsYou), run("d", .done)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let actions = Dictionary(uniqueKeysWithValues: model.liveRuns.map { ($0.runId, model.rowActions(for: $0)) })
        XCTAssertEqual(actions["w"], [.steer, .stop])
        XCTAssertEqual(actions["p"], [.steer, .stop])
        XCTAssertEqual(actions["n"], [])
        XCTAssertEqual(actions["d"], [])

        model.stop(model.liveRuns[0])
        XCTAssertEqual(model.rowActions(for: model.liveRuns[0]), [], "a run being stopped offers nothing more")
        context.undo.undo()
    }

    func testStopWaitsForTheUndoWindowAndUndoCancelsIt() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("w", .working)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let target = model.liveRuns[0]

        model.stop(target)
        XCTAssertTrue(model.isStopping(target))
        XCTAssertTrue(context.undo.hasPending)
        XCTAssertEqual(context.undo.toast?.undoable, true)
        context.undo.undo()
        await sleep(0.3)
        XCTAssertTrue(api.controlCalls.isEmpty, "undo sends nothing")
        XCTAssertFalse(model.isStopping(target))
        XCTAssertNil(context.undo.toast)
    }

    func testStopIsSentAfterTheUndoWindow() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("w", .working)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        model.stop(model.liveRuns[0])
        XCTAssertTrue(api.controlCalls.isEmpty, "nothing is sent inside the window")
        await sleep(0.4)
        XCTAssertEqual(api.controlCalls.count, 1)
        XCTAssertEqual(api.controlCalls.first?.0, "w_live", "stop names the live turn")
        XCTAssertEqual(api.controlCalls.first?.1, .stop)
    }

    func testStartingAnotherUndoableActionSendsTheWaitingOne() async {
        // A long window, so only the flush can send the first stop.
        let slow = VibeDevContext(api: api, approvals: approvals, undo: VibeDevUndoCenter(delay: 5, haptics: false))
        api.live = VibeDevLiveRunsResponse(runs: [run("a", .working), run("b", .working)])
        let model = VibeDevNowModel(context: slow)
        await model.refresh()
        model.stop(model.liveRuns[0])
        model.stop(model.liveRuns[1])
        await sleep(0.2)
        XCTAssertEqual(api.controlCalls.map(\.0), ["a_live"], "the waiting stop went out when the next began")
        XCTAssertTrue(model.isStopping(model.liveRuns[1]))
        slow.undo.undo()
        XCTAssertFalse(model.isStopping(model.liveRuns[1]))
        await sleep(0.1)
        XCTAssertEqual(api.controlCalls.map(\.0), ["a_live"])
    }

    func testStopIsSentAfterTheScreenThatAskedIsGone() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("w", .working)])
        var model: VibeDevNowModel? = VibeDevNowModel(context: context)
        await model?.refresh()
        model?.stop(model!.liveRuns[0])
        weak let released = model
        model = nil
        XCTAssertNil(released, "the screen's model is gone inside the undo window")
        await sleep(0.4)
        XCTAssertEqual(api.controlCalls.map(\.0), ["w_live"], "the undo center still sends the stop")
    }

    func testDiscardIsSentAfterTheRunScreenIsGone() async {
        api.turns = turns(["completed"], trusted: true)
        var model: VibeDevRunModel? = VibeDevRunModel(runID: "root", context: context)
        await model?.refresh()
        model?.discard()
        weak let released = model
        model = nil
        XCTAssertNil(released)
        await sleep(0.4)
        XCTAssertEqual(api.discards, ["root"])
    }

    func testClosingVibeDevSendsAWaitingAnswerOfAReleasedScreen() async throws {
        // A long window, so only the flush on close can send it.
        let slow = VibeDevContext(api: api, approvals: approvals, undo: VibeDevUndoCenter(delay: 5, haptics: false))
        let crew = VibeDevCrew(crewId: "race", goal: "g", status: .choosing, mode: .race,
                               lanes: [lane("A1", .done)], chooseRequestId: "q")
        api.projectList = VibeDevProjectsResponse(
            projects: [VibeDevProject(projectId: "p1", name: "Shop", trust: .trusted, crews: [crew])],
            activeProjectId: nil)
        var model: VibeDevNowModel? = VibeDevNowModel(context: slow)
        await model?.refresh()
        let item = try XCTUnwrap(model?.needsYou.first)
        let keepNone = try XCTUnwrap(item.options.first { $0.action == .keepNone })
        model?.perform(keepNone, of: item, open: { _ in })
        model = nil
        slow.undo.flush()
        await sleep(0.2)
        XCTAssertEqual(api.chooseCalls.count, 1, "the flush sends what the gone screen scheduled")
    }

    func testKeepNoneHidesTheCardUntilUndoThenSendsNullLaneIndex() async throws {
        let crew = VibeDevCrew(crewId: "race", goal: "g", status: .choosing, mode: .race,
                               lanes: [lane("A1", .done)], chooseRequestId: "q")
        api.projectList = VibeDevProjectsResponse(
            projects: [VibeDevProject(projectId: "p1", name: "Shop", trust: .trusted, crews: [crew])],
            activeProjectId: nil)
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let item = try XCTUnwrap(model.needsYou.first)
        let keepNone = try XCTUnwrap(item.options.first { $0.action == .keepNone })

        model.perform(keepNone, of: item, open: { _ in })
        XCTAssertTrue(model.needsYou.isEmpty, "the card hides while the undo window runs")
        context.undo.undo()
        XCTAssertEqual(model.needsYou.map(\.id), [item.id])
        await sleep(0.3)
        XCTAssertTrue(api.chooseCalls.isEmpty)

        model.perform(keepNone, of: item, open: { _ in })
        await sleep(0.4)
        XCTAssertEqual(api.chooseCalls.count, 1)
        XCTAssertEqual(api.chooseCalls.first!, nil, "keep none posts lane_index null")
    }

    func testCrewOptionsCallTheirEndpointsAndAnswers() async throws {
        let crews = [
            VibeDevCrew(crewId: "s", goal: "g", status: .proposed),
            VibeDevCrew(crewId: "r", goal: "g", status: .conflict, resolveRequestId: "vibedev-crew-r-resolve"),
            VibeDevCrew(crewId: "pr", goal: "g", status: .integrated, pullRequestRequestId: "vibedev-crew-pr-pr"),
        ]
        api.projectList = VibeDevProjectsResponse(
            projects: [VibeDevProject(projectId: "p1", name: "Shop", trust: .trusted, crews: crews)],
            activeProjectId: nil)
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        func option(_ crew: String, _ action: VibeDevQuestionAction) throws -> (VibeDevNeedsYouItem, VibeDevQuestionOption) {
            let item = try XCTUnwrap(model.needsYou.first { $0.crew?.crewId == crew })
            return (item, try XCTUnwrap(item.options.first { $0.action == action }))
        }
        var (item, choice) = try option("s", .startCrew)
        model.perform(choice, of: item, open: { _ in })
        (item, choice) = try option("r", .resolveMyself)
        model.perform(choice, of: item, open: { _ in })
        (item, choice) = try option("pr", .notNow)
        model.perform(choice, of: item, open: { _ in })
        await sleep(0.2)
        XCTAssertEqual(api.crewActions.map(\.0), ["s"])
        XCTAssertEqual(api.crewActions.first?.1, .start)
        let answers = Dictionary(uniqueKeysWithValues: api.answers.map { ($0.0, $0.1) })
        XCTAssertEqual(answers, ["vibedev-crew-r-resolve": "manual", "vibedev-crew-pr-pr": "later"])
    }

    func testApproveGoesThroughTheAttentionApproval() async throws {
        api.live = VibeDevLiveRunsResponse(runs: [run("n", .needsYou)])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let item = try XCTUnwrap(model.needsYou.first)
        let approve = try XCTUnwrap(item.options.first { $0.action == .approveRun })
        var opened: [VibeDevRoute] = []
        model.perform(approve, of: item, open: { opened.append($0) })
        await sleep(0.1)
        XCTAssertEqual(approvals.decisions.first?.0, ["n_live", "n"])
        XCTAssertEqual(approvals.decisions.first?.1, true)

        let review = try XCTUnwrap(item.options.first { $0.action == .reviewRun })
        model.perform(review, of: item, open: { opened.append($0) })
        XCTAssertEqual(opened, [.run("n", .changes)])
    }

    func testApproveAnswersTheApprovalIdDirectlyWhenTheServerNamesIt() async throws {
        var waiting = run("n", .needsYou)
        waiting.approvalId = "ccp-1"
        api.live = VibeDevLiveRunsResponse(runs: [waiting])
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        let item = try XCTUnwrap(model.needsYou.first)
        let approve = try XCTUnwrap(item.options.first { $0.action == .approveRun })
        model.perform(approve, of: item, open: { _ in })
        await sleep(0.1)
        XCTAssertEqual(api.diffAnswers.map(\.0), ["ccp-1"])
        XCTAssertEqual(api.diffAnswers.first?.1, true)
        XCTAssertTrue(approvals.decisions.isEmpty, "no Attention search when the id is known")

        // The run screen answers its own run's approval.
        var root = run("root", .needsYou)
        root.approvalId = "ccp-2"
        api.live = VibeDevLiveRunsResponse(runs: [root])
        api.turns = turns(["running"], trusted: false)
        let runModel = VibeDevRunModel(runID: "root", context: context)
        await runModel.refresh()
        XCTAssertEqual(runModel.bar, .needsYou)
        runModel.decide(apply: false)
        await sleep(0.1)
        XCTAssertEqual(api.diffAnswers.last?.0, "ccp-2")
        XCTAssertEqual(api.diffAnswers.last?.1, false)
        XCTAssertTrue(approvals.decisions.isEmpty)
    }

    func testSteerBetweenTurnsSaysToFollowUp() async {
        api.controlError = VibeDevAPIError.http(409, "no live run")
        let actions = VibeDevActions(context: context)
        let message = await actions.steer(liveTaskID: "t", name: "API lane", message: "use postgres")
        XCTAssertEqual(message, "This run is between turns. Open it to send a follow-up.")
        let empty = await actions.steer(liveTaskID: "t", name: "x", message: "  ")
        XCTAssertNotNil(empty)
    }

    // MARK: Run screen

    private func turns(_ statuses: [String], trusted: Bool) -> VibeDevRunTurns {
        VibeDevRunTurns(
            runId: "root", projectId: "p1", projectName: "Shop", title: "VibeDev · Speed up search",
            status: statuses.last ?? "running", trusted: trusted, branch: trusted ? "vibedev/x" : nil,
            crewId: nil, lane: nil,
            turns: statuses.enumerated().map { index, status in
                VibeDevRunTurn(taskId: index == 0 ? "root" : "turn\(index)", prompt: "p\(index)", mode: .build,
                               status: status, createdAt: "2026-10-13T09:00:00Z", updatedAt: "2026-10-13T09:10:00Z",
                               summary: nil, engine: "codex", costUsd: 0.5, commit: nil, filesChanged: 1)
            })
    }

    func testRunBarFollowsTheRunState() async {
        api.turns = turns(["completed", "completed"], trusted: true)
        let ended = VibeDevRunModel(runID: "root", context: context)
        await ended.refresh()
        XCTAssertEqual(ended.bar, .ended(trusted: true))
        XCTAssertEqual(ended.title, "Speed up search")
        XCTAssertEqual(ended.turns?.latestTurnId, "turn1", "a follow-up threads onto the newest turn")
        XCTAssertEqual(ended.turns?.totalCost, 1.0)
        XCTAssertTrue(ended.expanded.isEmpty, "an ended run opens with its turns collapsed")

        api.turns = turns(["completed", "running"], trusted: false)
        let running = VibeDevRunModel(runID: "turn1", context: context)
        await running.refresh()
        XCTAssertEqual(running.bar, .live)
        XCTAssertEqual(running.expanded, ["turn1"], "the live turn opens on its timeline")
        XCTAssertEqual(running.liveTaskID, "turn1")

        api.live = VibeDevLiveRunsResponse(runs: [VibeDevLiveRun(
            runId: "root", liveTaskId: "turn1", projectId: "p1", projectName: "Shop", title: "t",
            latestTurnTitle: nil, state: .needsYou, status: "running", step: nil, summary: nil, turns: 2,
            crewId: nil, lane: nil, updatedAt: "2026-10-13T09:41:00Z", lastProgressAt: nil)])
        let waiting = VibeDevRunModel(runID: "root", context: context)
        await waiting.refresh()
        XCTAssertEqual(waiting.bar, .needsYou)
        waiting.decide(apply: false)
        await sleep(0.1)
        XCTAssertEqual(approvals.decisions.last?.1, false)
        XCTAssertTrue(approvals.decisions.last?.0.contains("turn1") ?? false)
    }

    func testRunStopUsesTheUndoWindow() async {
        api.turns = turns(["running"], trusted: false)
        let model = VibeDevRunModel(runID: "root", context: context)
        await model.refresh()
        model.stop()
        XCTAssertTrue(model.stopping)
        XCTAssertEqual(model.bar, .none, "Steer and Stop leave while the stop waits")
        context.undo.undo()
        XCTAssertFalse(model.stopping)
        XCTAssertEqual(model.bar, .live)
    }

    func testAnOlderRunReadCannotOverwriteANewerOne() async {
        api.turns = turns(["running"], trusted: false)
        api.turnsDelays = [0.3, 0]
        let model = VibeDevRunModel(runID: "root", context: context)
        let slow = Task { await model.refresh() }
        await sleep(0.05)
        api.turns = turns(["completed", "completed"], trusted: true)
        await model.refresh()
        await slow.value
        XCTAssertEqual(model.turns?.turns.count, 2, "the slow first read lands last and is dropped")
        XCTAssertEqual(model.bar, .ended(trusted: true))
    }

    func testRunReadsLandWhileEventsKeepArriving() async {
        api.turns = turns(["running"], trusted: false)
        api.turnsDelays = Array(repeating: 0.1, count: 40)  // a slow tunnel
        let model = VibeDevRunModel(runID: "root", context: context)
        var pokes: [Task<Void, Never>] = []
        for _ in 0..<12 {  // an event every 0.03 s, faster than a read
            pokes.append(Task { await model.refresh() })
            await sleep(0.03)
        }
        XCTAssertTrue(model.loaded, "a read lands even while events keep coming")
        XCTAssertEqual(model.turns?.turns.count, 1)
        api.turns = turns(["completed", "completed"], trusted: true)
        await model.refresh()  // queued behind the read in flight
        for poke in pokes { await poke.value }
        XCTAssertEqual(model.turns?.turns.count, 2, "the queued read saw the newest turns")
        XCTAssertEqual(model.bar, .ended(trusted: true))
    }

    func testRefreshReturnsAfterItsReadWhileEventsKeepArriving() async {
        api.turns = turns(["running"], trusted: false)
        api.turnsDelays = Array(repeating: 0.1, count: 200)  // a slow tunnel
        let model = VibeDevRunModel(runID: "root", context: context)
        let events = Task {  // an event every 0.03 s for 3 s, faster than a read
            for _ in 0..<100 where !Task.isCancelled {
                Task { await model.refresh() }
                await sleep(0.03)
            }
        }
        let start = Date()
        await model.refresh()  // the pull that started the reads
        let waited = Date().timeIntervalSince(start)
        events.cancel()
        await events.value
        XCTAssertTrue(model.loaded)
        XCTAssertLessThan(waited, 1.0, "the pull waits for its read, not for the events to stop")
    }

    func testRefreshDuringAReadWaitsForTheNextRead() async {
        api.turns = turns(["running"], trusted: false)
        api.turnsDelays = [0.3, 0.1]
        let model = VibeDevRunModel(runID: "root", context: context)
        let first = Task { await model.refresh() }
        await sleep(0.05)  // the first read is out, holding the old turns
        api.turns = turns(["completed", "completed"], trusted: true)
        await model.refresh()
        XCTAssertEqual(model.turns?.turns.count, 2, "the pull returned after a read that began after it")
        XCTAssertEqual(model.bar, .ended(trusted: true))
        await first.value
    }

    func testACancelledCallerDoesNotFailTheReads() async {
        api.turns = turns(["running"], trusted: false)
        api.turnsDelays = [0.2, 0.1]
        let model = VibeDevRunModel(runID: "root", context: context)
        let screen = Task { await model.refresh() }  // the screen's `.task`
        await sleep(0.05)
        let pull = Task { await model.refresh() }  // queued behind the read in flight
        await sleep(0.02)
        screen.cancel()  // the screen went away
        await pull.value
        await screen.value
        XCTAssertFalse(model.refreshFailed, "the reads are the model's, not the cancelled caller's")
        XCTAssertEqual(model.turns?.turns.count, 1)
    }

    func testChangesOfAnEarlierTurnFilterAreDropped() async {
        api.turns = turns(["completed", "completed"], trusted: true)
        api.changesDelays = [nil: 0.3]
        let model = VibeDevRunModel(runID: "root", context: context)
        await model.refresh()
        model.selectTab(.changes)  // the whole run, slow
        await sleep(0.05)
        model.filterChanges(turn: "turn1")
        await sleep(0.5)
        XCTAssertEqual(model.changesTurn, "turn1")
        XCTAssertEqual(model.changes?.turn, "turn1", "the whole-run files never show under turn1")
        XCTAssertFalse(model.loadingChanges)
    }

    // MARK: Project screen

    func testProjectRefreshKeepsRunsPagedInPastTheFirstFifty() async {
        api.runRoots = (0..<70).map {
            VibeDevTaskItem(id: "r\($0)", title: "VibeDev · \($0)", status: "completed", tags: ["vibedev"])
        }
        let model = VibeDevProjectModel(projectID: "p1", context: context)
        await model.refresh()
        XCTAssertEqual(model.rows.count, 20)
        await model.loadMore()
        await model.loadMore()
        await model.loadMore()
        XCTAssertEqual(model.rows.count, 70)
        XCTAssertFalse(model.hasMore)

        await model.refresh()
        XCTAssertEqual(model.rows.map(\.id), api.runRoots.map(\.id), "a refresh re-reads 50 and keeps the rest")
        XCTAssertFalse(model.hasMore)

        // A run past the span that moves to the top shows once.
        api.runRoots.insert(api.runRoots.remove(at: 60), at: 0)
        await model.refresh()
        XCTAssertEqual(model.rows.first?.id, "r60")
        XCTAssertEqual(model.rows.count, 70)
        XCTAssertEqual(Set(model.rows.map(\.id)).count, 70)
    }

    // MARK: Now refresh

    func testARefreshAskedForMidReadRunsOnceMore() async {
        api.live = VibeDevLiveRunsResponse(runs: [run("a", .working)])
        api.liveDelays = [0.3, 0]
        let model = VibeDevNowModel(context: context)
        let first = Task { await model.refresh() }
        await sleep(0.05)
        api.live = VibeDevLiveRunsResponse(runs: [run("a", .working), run("b", .working)])
        await model.refresh()  // queued behind the read in flight
        await first.value
        XCTAssertEqual(model.working.map(\.runId), ["a", "b"], "the queued refresh read the newer runs")
    }

    // MARK: Live updates

    func testLiveUpdatesListenToLifecycleNotStreamingDeltas() {
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("TaskCreated"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("TaskUpdated"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("TaskDeleted"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("task.updated"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("hitl.requested"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("execution.completed"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("coding.completed"))
        XCTAssertTrue(VibeDevLiveUpdates.isRelevant("coding.approval_requested"))
        XCTAssertFalse(VibeDevLiveUpdates.isRelevant("coding.message"))
        XCTAssertFalse(VibeDevLiveUpdates.isRelevant("coding.tool.started"))
        XCTAssertFalse(VibeDevLiveUpdates.isRelevant("chat.delta"))
    }

    func testLiveUpdatesDebounceIntoOneRefresh() async {
        var refreshes = 0
        let live = VibeDevLiveUpdates(debounce: 0.1) { refreshes += 1 }
        live.poke(); live.poke(); live.poke()
        await sleep(0.3)
        XCTAssertEqual(refreshes, 1)
    }

    // MARK: Wire helpers

    func testCodingModelCatalogKeepsEngineAndThinkingWithoutLegacyRows() throws {
        let engines = ["pi", "claude_code", "codex_app_server", "grok_acp", "agy_cli"]
        let models: [[String: Any]] = engines.map { engine in
            ["id": engine == "pi" ? "provider/model" : "native/\(engine)/provider/model",
             "engine": engine, "provider": "provider", "connection": "provider", "model": "physical-model", "label": "Model",
             "thinking": ["mode": "effort", "levels": ["low", "high"], "default": "low"],
             "choices": ["auto": "\(engine)/auto", "high": "\(engine)/high"]]
        }
        let data = try JSONSerialization.data(withJSONObject: [
            "models": models,
            "engines": engines.map { ["id": $0, "selectable": true] as [String: Any] },
            "default_selection": ["kind": "model", "engine": "pi", "model_id": "provider/model", "thinking": "high"]
        ])
        let catalog = try JSONDecoder().decode(VibeDevModelCatalog.self, from: data)
        XCTAssertEqual(catalog.rows.count, 10)
        XCTAssertEqual(catalog.rows.filter(\.isDefault).map(\.id), ["pi/high"])
        for engine in engines {
            let row = try XCTUnwrap(catalog.rows.first { $0.id == "\(engine)/high" })
            let selected = VibeDevCodingSelection(profile: row)
            XCTAssertEqual(selected.wireValue["engine"], engine)
            XCTAssertEqual(selected.wireValue["thinking"], "high")
            XCTAssertEqual(selected.wireValue["model_id"], engine == "pi" ? "provider/model" : "native/\(engine)/provider/model")
            XCTAssertNil(selected.wireValue["profile_id"])
        }
    }

    func testStartRunBodyCarriesTheIdempotencyKeyAndThread() {
        var request = VibeDevStartRunRequest(prompt: "go", projectId: "p1", mode: .discuss)
        request.clientRunId = "abc"
        XCTAssertEqual(request.body["client_run_id"] as? String, "abc")
        XCTAssertEqual(request.body["mode"] as? String, "discuss")
        XCTAssertNil(request.body["threaded"], "a new run sends no thread fields")
        request.parentTaskId = "turn1"
        request.threaded = true
        request.costBudgetUsd = 2
        XCTAssertEqual(request.body["parent_task_id"] as? String, "turn1")
        XCTAssertEqual(request.body["threaded"] as? Bool, true)
        XCTAssertEqual(request.body["cost_budget_usd"] as? Double, 2)
        var row = VibeDevCodingProfile(id: "pi/model/provider/model/high", label: "Model", engine: "pi")
        row.modelSelection = ChatModelSelection(modelId: "provider/model", thinking: "high")
        request.codingChoice = VibeDevCodingSelection(profile: row)
        XCTAssertEqual(request.body["coding_choice"] as? [String: String],
            ["kind": "model", "engine": "pi", "model_id": "provider/model", "thinking": "high"])
        XCTAssertNil(request.body["coding_profile_id"])
    }

    func testProjectReferencesAndNightlyScheduleMatchWebContract() throws {
        var request = VibeDevStartRunRequest(prompt: "Build this", projectId: "p1", mode: .autopilot)
        request.nightly = true
        request.attachmentSessionId = "project-chat"
        request.attachments = [StagedAttachment(id: UUID(), remoteId: "remote", filename: "design.png", mime: "image/png", uploading: false)]
        XCTAssertEqual(request.body["attachment_session_id"] as? String, "project-chat")
        let attachment = try XCTUnwrap((request.body["attachments"] as? [[String: Any]])?.first)
        XCTAssertEqual(attachment["attachment_id"] as? String, "remote")
        let schedule = try XCTUnwrap(request.body["schedule"] as? [String: Any])
        let kind = try XCTUnwrap(schedule["kind"] as? [String: [String: String]])
        XCTAssertEqual(kind["Cron"]?["expression"], "0 2 * * *")
        XCTAssertEqual(schedule["timezone"] as? String, "UTC")
        request.parentTaskId = "existing"
        XCTAssertNil(request.body["schedule"])
        request.parentTaskId = nil
        request.mode = .discuss
        XCTAssertNil(request.body["schedule"])
        XCTAssertEqual(request.body["agent_id"] as? String, "cto")
        XCTAssertEqual(request.body["auto_apply"] as? Bool, false)
    }

    func testSharedAttachmentRequestUsesProjectSessionAndEscapesFilename() throws {
        let request = try XCTUnwrap(ComposerAttachmentUpload.request(Data("reference".utf8), filename: "a\"\r\nb.txt", mime: "text/plain", sessionID: "project-chat"))
        XCTAssertTrue(request.url!.path.hasSuffix("/chat/sessions/project-chat/attachments"))
        XCTAssertEqual(request.httpMethod, "POST")
        let body = String(decoding: request.httpBody!, as: UTF8.self)
        XCTAssertTrue(body.contains("filename=\"a___b.txt\""))
        XCTAssertEqual(ComposerAttachmentUpload.attachmentID(Data(#"{"attachment":{"attachment_id":"remote"}}"#.utf8)), "remote")
    }

    func testCrewRequestAsksTheOwnerBeforeStarting() {
        let race = VibeDevCreateCrewRequest(goal: "g", mode: .race, racers: 4, review: false, budgetUsd: 6).body
        XCTAssertEqual(race["start"] as? Bool, false)
        XCTAssertEqual(race["racers"] as? Int, 4)
        XCTAssertEqual(race["mode"] as? String, "race")
        let split = VibeDevCreateCrewRequest(goal: "g", mode: .split, racers: 4).body
        XCTAssertNil(split["racers"], "a split crew lets the planner choose lanes")
    }

    func testURLsEscapeIdsAndKeepThePrefix() {
        let url = VibeDevAPI.url(base: URL(string: "https://example.invalid/")!,
                                 path: "/vibedev/runs/\(VibeDevAPI.segment("a/b c"))/changes",
                                 query: [URLQueryItem(name: "path", value: "src/a b.ts")])
        XCTAssertEqual(url?.absoluteString,
                       "https://example.invalid/api/magician/v2/vibedev/runs/a%2Fb%20c/changes?path=src/a%20b.ts")
    }

    func testServerMessageWinsInErrors() {
        let data = Data(#"{"error":"vibedev_trusted_owner_only","message":"Only the owner's terminal can merge."}"#.utf8)
        XCTAssertThrowsError(try VibeDevAPI.check(data, 403)) { error in
            XCTAssertEqual((error as? VibeDevAPIError)?.errorDescription, "Only the owner's terminal can merge.")
        }
        XCTAssertEqual(VibeDevAPIError.http(403, "").errorDescription,
                       "Only the owner can do this, from their own machine.")
    }

    func testRunsPageFoldsByParentLineWhenThereIsNoLink() {
        let page = VibeDevRunsPage(tasks: [
            VibeDevTaskItem(id: "b", title: "VibeDev · B", status: "running", tags: ["vibedev"]),
            VibeDevTaskItem(id: "a", title: "VibeDev · A", status: "completed", tags: ["vibedev"]),
            VibeDevTaskItem(id: "a2", title: "f", description: "x\nParent task: a\n", status: "completed",
                            tags: ["vibedev-threaded"]),
            VibeDevTaskItem(id: "a3", title: "f", description: "Parent task: a2", status: "running",
                            tags: ["vibedev-threaded"]),
            VibeDevTaskItem(id: "orphan", title: "f", description: "Parent task: missing", status: "running",
                            tags: ["vibedev-threaded"]),
        ], pagination: VibeDevRunsPagination(runs: 3, totalRuns: 3, offset: 0, hasMore: false, nextCursor: nil))
        let rows = page.rows
        XCTAssertEqual(rows.map(\.id), ["b", "a", "orphan"])
        XCTAssertEqual(rows[1].turnCount, 3)
        XCTAssertEqual(rows[1].state, .working, "a run's state is its newest turn's")
        XCTAssertEqual(rows[0].title, "B")
    }

    // MARK: Project health git init

    func testGitInitReReadsProjectsOnSuccess() async {
        api.projectList = VibeDevProjectsResponse(projects: [VibeDevProject(projectId: "p1", name: "Shop")],
                                                  activeProjectId: "p1")
        let message = await context.gitInit(projectID: "p1")
        XCTAssertNil(message)
        XCTAssertEqual(api.gitInits, ["p1"])
        XCTAssertEqual(context.projectList.map(\.projectId), ["p1"])
    }

    func testGitInitFailureReturnsTheServersMessage() async {
        api.gitInitError = VibeDevAPIError.http(409, "The project folder is missing.")
        let message = await context.gitInit(projectID: "p1")
        XCTAssertEqual(message, "The project folder is missing.")
        XCTAssertTrue(context.projectList.isEmpty)
    }
}
