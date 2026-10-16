import XCTest
@testable import Magician

final class LiveTaskCardSupportTests: XCTestCase {
    private func task(
        _ status: String,
        id: String = "task_abc123",
        storeStatus: String? = nil,
        outputs: Int = 0,
        steps: [String] = []
    ) -> TaskStatusModel {
        var model = TaskStatusModel(
            taskId: id, title: "Write the brief", status: status, steps: steps,
            executionId: "exec_1",
            outputFiles: (0..<outputs).map { _ in
                ContentBlock(type: "file", text: nil, filename: "a.md", url: nil, mimeType: nil,
                             source: nil, relativePath: "outputs/a.md", displayName: nil,
                             absolutePath: nil, label: nil, size: nil)
            }
        )
        model.storeStatus = storeStatus
        model.startedAtMs = 1_759_300_000_000
        model.updatedAtMs = 1_759_300_065_000
        return model
    }

    private func state(_ model: TaskStatusModel) -> (LiveTaskCardState, LiveTaskCardPhase) {
        let input = LiveTaskCardRules.input(for: model, messageId: "m1")
        let state = LiveTaskCardState(input)
        return (state, LiveTaskCardRules.phase(state, effectiveStatus: LiveTaskCardRules.effectiveStatus(input)))
    }

    // MARK: Phase

    func testPhaseFollowsTheTaskLife() {
        XCTAssertEqual(state(task("planning")).1, .planning)
        XCTAssertEqual(state(task("draft")).1, .planReady)
        XCTAssertEqual(state(task("ready")).1, .notStarted)
        XCTAssertEqual(state(task("pending")).1, .notStarted)
        XCTAssertEqual(state(task("running")).1, .running)
        XCTAssertEqual(state(task("paused")).1, .running)
        XCTAssertEqual(state(task("completed")).1, .finished)
        XCTAssertEqual(state(task("failed")).1, .finished)
    }

    func testStoreStatusWinsOverAMessageStuckAtPlanningOrDraft() {
        let (cardState, phase) = state(task("draft", storeStatus: "ready"))
        XCTAssertEqual(phase, .notStarted)
        XCTAssertTrue(cardState.startable, "an approved plan offers Run")
        // A store status never overrides a live run.
        XCTAssertEqual(state(task("running", storeStatus: "ready")).1, .running)
    }

    // MARK: Action cluster

    private func actions(_ phase: LiveTaskCardPhase, taskId: Bool = true, startable: Bool = false,
                         canCancel: Bool = false, showsPlan: Bool = false) -> [LiveTaskCardAction] {
        LiveTaskCardRules.actions(phase: phase, hasTaskId: taskId, startable: startable,
                                  canCancel: canCancel, showsPlan: showsPlan)
    }

    func testActionsSitInspectFirstPrimaryNextCancelLast() {
        // Planning: Inspect, then Cancel beside the chevron.
        XCTAssertEqual(actions(.planning, canCancel: true), [.inspect, .cancel])
        // Plan ready: the primary before the secondary; Review stands in for Plan graph.
        XCTAssertEqual(actions(.planReady, canCancel: true, showsPlan: true),
                       [.inspect, .approveAndRun, .review, .cancel])
        // Not started and startable: Run right next to Inspect, then the plan.
        XCTAssertEqual(actions(.notStarted, startable: true, showsPlan: true),
                       [.inspect, .run, .planGraph])
        XCTAssertEqual(actions(.notStarted, startable: false), [.inspect])
        // Running: Cancel last; a waiting run with a known plan offers it first.
        XCTAssertEqual(actions(.running, canCancel: true), [.inspect, .cancel])
        XCTAssertEqual(actions(.running, canCancel: true, showsPlan: true),
                       [.inspect, .planGraph, .cancel])
        XCTAssertEqual(actions(.finished), [.inspect])
    }

    func testWithoutATaskIdOnlyInspectIsOffered() {
        XCTAssertEqual(actions(.planReady, taskId: false, canCancel: true, showsPlan: true), [.inspect])
        XCTAssertEqual(actions(.notStarted, taskId: false, startable: true), [.inspect])
    }

    func testCardStatesMapToTheirActions() {
        func cluster(_ model: TaskStatusModel, plan: Bool = false) -> [LiveTaskCardAction] {
            let (cardState, phase) = state(model)
            return LiveTaskCardRules.actions(
                phase: phase, hasTaskId: cardState.taskId != nil, startable: cardState.startable,
                canCancel: cardState.canCancel,
                showsPlan: cardState.showsPlanPreview(planStepCount: plan ? 3 : 0))
        }
        XCTAssertEqual(cluster(task("draft", storeStatus: "ready"), plan: true), [.inspect, .run, .planGraph])
        XCTAssertEqual(cluster(task("running")), [.inspect, .cancel])
        XCTAssertEqual(cluster(task("completed")), [.inspect])
    }

    func testTheCardHasAClockFromItsFirstUpdate() {
        let (cardState, _) = state(task("running"))
        XCTAssertTrue(cardState.showsTimer)
        XCTAssertEqual(cardState.startedAt, Date(timeIntervalSince1970: 1_759_300_000))
        let (done, _) = state(task("completed"))
        XCTAssertEqual(done.elapsedText(now: .distantFuture), "1:05")
    }

    func testFinishedOutputsSitInTheBody() {
        let (cardState, _) = state(task("completed", outputs: 3))
        XCTAssertTrue(cardState.outputsInBody)
        XCTAssertEqual(cardState.hiddenOutputCount(expanded: false), 1)
    }

    // MARK: Polling

    func testPollsOnlyWhileVisibleActiveAndPlanningOrRunning() {
        XCTAssertTrue(LiveTaskCardRules.shouldPoll(phase: .planning, hasTaskId: true, visible: true, appActive: true))
        XCTAssertTrue(LiveTaskCardRules.shouldPoll(phase: .running, hasTaskId: true, visible: true, appActive: true))
        XCTAssertFalse(LiveTaskCardRules.shouldPoll(phase: .running, hasTaskId: true, visible: false, appActive: true))
        XCTAssertFalse(LiveTaskCardRules.shouldPoll(phase: .running, hasTaskId: true, visible: true, appActive: false))
        XCTAssertFalse(LiveTaskCardRules.shouldPoll(phase: .running, hasTaskId: false, visible: true, appActive: true))
        for phase in [LiveTaskCardPhase.planReady, .notStarted, .finished] {
            XCTAssertFalse(LiveTaskCardRules.shouldPoll(phase: phase, hasTaskId: true, visible: true, appActive: true))
        }
        XCTAssertEqual(LiveTaskCardRules.pollInterval, 4)
    }

    // MARK: Line

    private func entry(_ message: String, stage: String?, model: Bool = false) -> PlanningEntry {
        PlanningEntry(id: UUID().uuidString, at: Date(), message: message, warning: false,
                      stage: stage, isModelCall: model)
    }

    func testPlanningLineIsTheLatestMilestoneNotAModelCall() {
        let (cardState, _) = state(task("planning"))
        let line = LiveTaskCardRules.line(
            phase: .planning, state: cardState,
            planningEntries: [entry("Outlining the plan", stage: "outline"),
                              entry("Outlined · gpt · 2s", stage: "llm", model: true)],
            stepRows: [], latestActivity: nil)
        XCTAssertEqual(line, "Outlining the plan")
    }

    func testRunningPlannedTaskLineIsTheStepSummary() {
        let (cardState, _) = state(task("running", steps: ["Fetched"]))
        let rows = PlanSteps.rows(statuses: [
            ["name": "Gather", "status": "completed"],
            ["name": "Draft", "status": "in_progress"],
            ["name": "Polish", "status": "pending"]
        ], runEnded: false)
        XCTAssertEqual(
            LiveTaskCardRules.line(phase: .running, state: cardState, planningEntries: [],
                                   stepRows: rows, latestActivity: "Fetched"),
            "Step 2 of 3 · Draft")
        // No plan: the latest activity, never the bare run label.
        XCTAssertEqual(
            LiveTaskCardRules.line(phase: .running, state: cardState, planningEntries: [],
                                   stepRows: [], latestActivity: "Fetched\nmore"),
            "Fetched")
    }

    func testRetryOrFailureMakesThePlanningLineBold() {
        XCTAssertTrue(LiveTaskCardRules.lineIsEmphasised(
            phase: .planning, track: PlanningTrack(phase: .steps, retrying: true, failed: false)))
        XCTAssertFalse(LiveTaskCardRules.lineIsEmphasised(
            phase: .planning, track: PlanningTrack(phase: .steps, retrying: false, failed: false)))
        XCTAssertFalse(LiveTaskCardRules.lineIsEmphasised(
            phase: .running, track: PlanningTrack(phase: .steps, retrying: true, failed: false)))
    }

    // MARK: Planning track

    func testPlanningSegmentsFillUpToTheCurrentPhase() {
        XCTAssertEqual(LiveTaskCardRules.planningSegments(PlanningTrack(phase: nil, retrying: false, failed: false)),
                       [.current, .ahead, .ahead, .ahead])
        XCTAssertEqual(LiveTaskCardRules.planningSegments(PlanningTrack(phase: .steps, retrying: true, failed: false)),
                       [.finished, .finished, .current, .ahead])
        XCTAssertEqual(LiveTaskCardRules.planningSegments(PlanningTrack(phase: .check, retrying: false, failed: false)),
                       [.finished, .finished, .finished, .current])
    }

    // MARK: Step bar

    private func rows(_ count: Int, done: Int) -> [PlanStepRow] {
        (0..<count).map {
            PlanStepRow(number: $0 + 1, name: "S\($0)", mark: $0 < done ? .done : .pending, note: nil, stepId: nil)
        }
    }

    func testStepBarIsSegmentedUpToTwelveStepsThenContinuous() {
        XCTAssertNil(LiveTaskStepBar.layout([]))
        XCTAssertEqual(LiveTaskStepBar.layout(rows(3, done: 1)), .segments([.done, .pending, .pending]))
        if case .segments(let marks) = LiveTaskStepBar.layout(rows(12, done: 0)) {
            XCTAssertEqual(marks.count, 12)
        } else { XCTFail("12 steps stay segmented") }
        XCTAssertEqual(LiveTaskStepBar.layout(rows(13, done: 13)), .continuous(1))
        XCTAssertEqual(LiveTaskStepBar.layout(rows(20, done: 5)), .continuous(0.25))
    }

    // MARK: Plan payload

    func testPlanPayloadReadsIdStatusAndStepNames() {
        let payload: [String: Any] = ["plan": [
            "plan_id": "plan_9", "status": "draft",
            "plan_graph": ["steps": [
                ["id": "s1", "task": "Gather sources"],
                ["id": "s2", "metadata": ["description": "Draft the brief"]],
                ["id": "s3"]
            ]]
        ]]
        XCTAssertEqual(LiveTaskPlan.parse(payload),
                       LiveTaskPlan(planId: "plan_9", status: "draft",
                                    steps: ["Gather sources", "Draft the brief", "Step s3"]))
        XCTAssertNil(LiveTaskPlan.parse(["plan": [:]]))
    }

    // MARK: Copy path

    func testCopyPathPrefersTheHostPath() {
        XCTAssertEqual(LiveTaskCardRules.copyPath(absolutePath: "/Users/me/out/a.md", relativePath: "outputs/a.md"),
                       "/Users/me/out/a.md")
        XCTAssertEqual(LiveTaskCardRules.copyPath(absolutePath: "out/a.md", relativePath: "outputs/a.md"),
                       "outputs/a.md")
        XCTAssertNil(LiveTaskCardRules.copyPath(absolutePath: nil, relativePath: "  "))
    }

    // MARK: Dot

    func testDotToneFollowsTheStatus() {
        XCTAssertEqual(LiveTaskCardRules.dotTone(.queued), .notStarted)
        XCTAssertEqual(LiveTaskCardRules.dotTone(.running), .running)
        XCTAssertEqual(LiveTaskCardRules.dotTone(.partial), .partial)
        XCTAssertEqual(LiveTaskCardRules.dotTone(.failed), .failed)
    }

    // Task 35eb15: the chat's copy of a deliverable (`session_output`) was
    // asked of the task endpoint and read "Task output not found".
    func testSessionCopyOfATaskOutputIsReadFromTheSession() {
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: "session_output", sourceTaskId: nil, cardTaskId: "task_1", sessionId: "s1"),
            .session("s1"))
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: "chat_output", sourceTaskId: nil, cardTaskId: "task_1", sessionId: "s1"),
            .session("s1"))
    }

    func testTaskOutputIsReadFromTheTask() {
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: "task_output", sourceTaskId: "task_2", cardTaskId: "task_1", sessionId: "s1"),
            .task("task_2"))
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: "task_output", sourceTaskId: nil, cardTaskId: "task_1", sessionId: "s1"),
            .task("task_1"))
    }

    func testSourcelessBlockPrefersTheSessionThenTheTask() {
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: nil, sourceTaskId: nil, cardTaskId: "task_1", sessionId: "s1"),
            .session("s1"))
        XCTAssertEqual(
            LiveTaskCardRules.outputHome(sourceType: nil, sourceTaskId: nil, cardTaskId: "task_1", sessionId: nil),
            .task("task_1"))
        XCTAssertNil(
            LiveTaskCardRules.outputHome(sourceType: "session_output", sourceTaskId: nil, cardTaskId: "task_1", sessionId: nil))
    }

    // MARK: The task's stored status (task_ee4d571626814eb889945c968172385d)

    func testStoreRecordReadsTheCurrentNestedShape() {
        let object: [String: Any] = ["task": [
            "manifest": ["title": "Brief", "chat_session_id": "sess_1"],
            "state": ["status": "ready", "active_root_execution_id": "exec_9"],
            "refs": [:]
        ]]
        XCTAssertEqual(TaskStoreRecord.parse(object),
                       TaskStoreRecord(status: "ready", activeRootExecutionId: "exec_9", chatSessionId: "sess_1"))
    }

    func testStoreRecordStillReadsTheFlatShape() {
        let object: [String: Any] = ["task": [
            "status": "running", "active_root_execution_id": "exec_1", "chat_session_id": "sess_2"]]
        XCTAssertEqual(TaskStoreRecord.parse(object),
                       TaskStoreRecord(status: "running", activeRootExecutionId: "exec_1", chatSessionId: "sess_2"))
        XCTAssertNil(TaskStoreRecord.parse(["task": ["state": [:]]]))
        XCTAssertNil(TaskStoreRecord.parse([:]))
    }

    /// The reported card: the chat's last message is `draft` ("Plan ready for
    /// review") but the plan was approved, so the task is `ready`.
    func testADraftMessageWithAReadyStoreIsQueuedWithRunAndNoTimerOrReview() {
        var message = task("draft")
        message.summary = "Plan ready for review"
        let reconciled = LiveTaskCardRules.reconciled(message, storeStatus: "ready")
        XCTAssertEqual(reconciled.status, "draft", "the card's own store-wins rule shows it")
        XCTAssertEqual(reconciled.storeStatus, "ready")

        let (state, phase) = state(reconciled)
        XCTAssertEqual(state.status, .queued)
        XCTAssertEqual(phase, .notStarted, "not .planReady: no Review / Approve & Run")
        XCTAssertTrue(state.startable, "Run chip")
        XCTAssertFalse(state.showsTimer)
        XCTAssertFalse(state.ticks)
        XCTAssertFalse(state.canCancel)
        XCTAssertNil(state.reason, "the draft summary described a state the task left")
        XCTAssertEqual(state.tagText, "Not started")
    }

    func testAPlanningMessageWithAPendingStoreIsQueued() {
        let (state, phase) = state(LiveTaskCardRules.reconciled(task("planning"), storeStatus: "pending"))
        XCTAssertEqual(state.status, .queued)
        XCTAssertEqual(phase, .notStarted)
        XCTAssertFalse(state.startable)
    }

    func testATerminalStoreStatusReplacesAStaleRunningMessage() {
        var message = task("running")
        message.summary = "Working on step 3"
        let reconciled = LiveTaskCardRules.reconciled(message, storeStatus: "completed")
        XCTAssertEqual(reconciled.status, "completed")
        XCTAssertNil(reconciled.summary)
        let (state, phase) = state(reconciled)
        XCTAssertTrue(state.terminal)
        XCTAssertEqual(phase, .finished)
        XCTAssertFalse(state.canCancel)
    }

    func testAMatchingStoreStatusKeepsTheMessage() {
        var message = task("running")
        message.summary = "Working on step 3"
        let reconciled = LiveTaskCardRules.reconciled(message, storeStatus: "running")
        XCTAssertEqual(reconciled.status, "running")
        XCTAssertEqual(reconciled.summary, "Working on step 3")
    }
}
