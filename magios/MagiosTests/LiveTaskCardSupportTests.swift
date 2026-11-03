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

    // MARK: Created / updated times

    func testStoreRecordKeepsTheTaskCreatedAndLatestUpdatedTimes() {
        let object: [String: Any] = ["task": [
            "manifest": ["title": "Brief", "created_at": "2026-10-31T08:58:12.123456789+00:00"],
            "state": [
                "status": "running",
                "updated_at": "2026-10-31T09:03:05+00:00",
                "last_progress_at": "2026-10-31T09:01:40.5Z"
            ],
            "refs": [:]
        ]]
        let record = TaskStoreRecord.parse(object)
        XCTAssertEqual(record?.createdAt?.timeIntervalSince1970 ?? 0, 1_793_437_092.123, accuracy: 0.01,
                       "a nanosecond RFC3339 time reads")
        XCTAssertEqual(record?.updatedAt?.timeIntervalSince1970 ?? 0, 1_793_437_300.5, accuracy: 0.01,
                       "last_progress_at over a later heartbeat-moved updated_at")

        let noProgress = TaskStoreRecord.parse(["task": ["state": [
            "status": "planning", "updated_at": "2026-10-31T09:03:05+00:00"]]])
        XCTAssertEqual(noProgress?.updatedAt?.timeIntervalSince1970 ?? 0, 1_793_437_385, accuracy: 0.01,
                       "no last_progress_at: updated_at")
    }

    func testStoreRecordWithoutTimesHasNone() {
        let record = TaskStoreRecord.parse(["task": ["state": ["status": "ready", "updated_at": "not a date"]]])
        XCTAssertEqual(record?.status, "ready")
        XCTAssertNil(record?.createdAt)
        XCTAssertNil(record?.updatedAt)
    }

    func testCardTimesFallBackToTheCardsOwnUpdates() {
        let (state, _) = state(task("running"))
        XCTAssertEqual(state.createdAt, Date(timeIntervalSince1970: 1_759_300_000))
        XCTAssertEqual(state.updatedAt, Date(timeIntervalSince1970: 1_759_300_065))
    }

    func testCardTimesPreferTheTaskRecordsCreatedTimeAndTheLatestUpdate() {
        let firstUpdate = Date(timeIntervalSince1970: 1_759_300_000)
        let latestUpdate = Date(timeIntervalSince1970: 1_759_300_065)
        let created = Date(timeIntervalSince1970: 1_759_299_000)
        let later = Date(timeIntervalSince1970: 1_759_300_500)
        let recorded = LiveTaskCardState(LiveTaskCardRules.input(
            for: task("running"), messageId: "m1", recordCreatedAt: created, recordUpdatedAt: later))
        XCTAssertEqual(recorded.createdAt, created, "the record's created time over the first card update")
        XCTAssertEqual(recorded.updatedAt, later, "a newer record update wins")
        XCTAssertEqual(recorded.startedAt, firstUpdate, "the elapsed clock still counts from the first card update")

        let staleRecord = LiveTaskCardState(LiveTaskCardRules.input(
            for: task("running"), messageId: "m1", recordUpdatedAt: Date(timeIntervalSince1970: 1_759_300_010)))
        XCTAssertEqual(staleRecord.createdAt, firstUpdate, "no record created time: the first card update")
        XCTAssertEqual(staleRecord.updatedAt, latestUpdate, "a newer card update wins")
    }

    func testAnUntimedCardHasNoTimes() {
        let model = TaskStatusModel(taskId: "task_1", title: "Write the brief", status: "running", steps: [])
        let state = LiveTaskCardState(LiveTaskCardRules.input(for: model, messageId: "m1"))
        XCTAssertNil(state.createdAt)
        XCTAssertNil(state.updatedAt)
    }

    // MARK: Card times format

    private var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }
    private let britishEnglish = Locale(identifier: "en_GB")
    private let usEnglish = Locale(identifier: "en_US")
    /// 2026-10-31 14:28 UTC, and two minutes later.
    private let at1428 = Date(timeIntervalSince1970: 1_793_456_880)
    private var now: Date { at1428.addingTimeInterval(120) }

    func testCardAbsoluteIsTheTimeAloneToday() {
        XCTAssertEqual(
            TaskTimelineFormatting.cardAbsolute(at1428, now: now, calendar: utc, locale: britishEnglish), "14:28")
    }

    func testCardAbsoluteAddsTheDateOnAnotherDayAndTheYearInAnotherYear() {
        let yesterday = TaskTimelineFormatting.cardAbsolute(
            Date(timeIntervalSince1970: 1_793_370_480), now: now, calendar: utc, locale: britishEnglish)
        XCTAssertTrue(yesterday.contains("30") && yesterday.contains("Oct") && yesterday.contains("14:28"), yesterday)
        XCTAssertFalse(yesterday.contains("2026"), yesterday)

        let lastYear = TaskTimelineFormatting.cardAbsolute(
            Date(timeIntervalSince1970: 1_767_104_880), now: now, calendar: utc, locale: britishEnglish)
        XCTAssertTrue(lastYear.contains("2025") && lastYear.contains("Dec") && lastYear.contains("14:28"), lastYear)
    }

    func testCachedFormattersStayPerLocaleAndTimeZone() {
        var ist = Calendar(identifier: .gregorian)
        ist.timeZone = TimeZone(identifier: "Asia/Kolkata")!
        XCTAssertEqual(
            TaskTimelineFormatting.cardAbsolute(at1428, now: now, calendar: utc, locale: britishEnglish), "14:28")
        XCTAssertEqual(
            TaskTimelineFormatting.cardAbsolute(at1428, now: now, calendar: ist, locale: britishEnglish), "19:58",
            "a cached UTC formatter is not reused for another time zone")
        XCTAssertEqual(
            TaskTimelineFormatting.cardAbsolute(at1428, now: now, calendar: utc, locale: britishEnglish), "14:28")
    }

    func testCardRelativeIsShortAndNowUnderAMinuteOrAheadOfTheDevice() {
        XCTAssertEqual(TaskTimelineFormatting.cardRelative(at1428, now: now, locale: usEnglish), "2 min. ago")
        XCTAssertEqual(TaskTimelineFormatting.cardRelative(now, now: now, locale: usEnglish), "now")
        XCTAssertEqual(
            TaskTimelineFormatting.cardRelative(now.addingTimeInterval(-59), now: now, locale: usEnglish), "now",
            "no second-level ages: a settled card refreshes every 30 s")
        XCTAssertEqual(
            TaskTimelineFormatting.cardRelative(now.addingTimeInterval(-60), now: now, locale: usEnglish), "1 min. ago")
        XCTAssertEqual(
            TaskTimelineFormatting.cardRelative(now.addingTimeInterval(5), now: now, locale: usEnglish), "now",
            "a server time ahead of the device clock")
    }

    func testCardTimePartsLeaveOutAnUnknownTime() {
        let both = TaskTimelineFormatting.cardTimeParts(
            started: at1428, updated: at1428, now: now, calendar: utc, locale: usEnglish)
        XCTAssertEqual(both.count, 2)
        XCTAssertTrue(both[0].hasPrefix("Created "), both[0])
        XCTAssertEqual(both[1], "Updated 2 min. ago")

        let updatedOnly = TaskTimelineFormatting.cardTimeParts(
            started: nil, updated: at1428, now: now, calendar: utc, locale: usEnglish)
        XCTAssertEqual(updatedOnly, ["Updated 2 min. ago"])

        let startedOnly = TaskTimelineFormatting.cardTimeParts(
            started: at1428, updated: nil, startWord: "Started", now: now, calendar: utc, locale: britishEnglish)
        XCTAssertEqual(startedOnly, ["Started 14:28"])

        XCTAssertEqual(TaskTimelineFormatting.cardTimeParts(started: nil, updated: nil, now: now), [])
    }
}
