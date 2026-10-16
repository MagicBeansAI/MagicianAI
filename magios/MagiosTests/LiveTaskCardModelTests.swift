import XCTest
@testable import Magician

/// Port of `ui/unified-ui/src/lib/magician/chat/next/liveTask.test.ts` (every
/// case) and the state rules `LiveTaskCard.component.test.ts` asserts through
/// the DOM: the timer, Cancel, the live activity, the run list, the plan
/// preview and when the card is expandable at all.
final class LiveTaskCardModelTests: XCTestCase {
    private let t0: Double = 1_760_000_000_000

    /// A `task_status_update` message, as the web fixture builds it.
    private func message(
        _ id: String,
        _ status: String,
        executionId: String? = nil,
        createdAt: Double? = nil,
        summary: String? = nil,
        synthesisPending: Bool = false,
        completionKind: String? = nil,
        outputs: Int = 0
    ) -> LiveTaskCardInput {
        LiveTaskCardInput(
            messageId: id,
            taskId: "task-1",
            displayLabel: "Review release",
            status: status,
            summary: summary,
            synthesisPending: synthesisPending,
            completionKind: completionKind,
            executionId: executionId,
            createdAt: createdAt ?? t0,
            outputCount: outputs
        )
    }

    /// One run group: its latest update's status, ids and output blocks, and
    /// every update's time.
    private func group(_ executionId: String, _ updates: [LiveTaskCardInput]) -> LiveTaskRunInput {
        let latest = updates[updates.count - 1]
        return LiveTaskRunInput(
            id: "task:task-1::execution:\(executionId)",
            status: latest.status,
            executionId: latest.executionId,
            sessionId: "session-1",
            timestamps: updates.compactMap(\.createdAt),
            outputCount: latest.outputCount
        )
    }

    private func view(
        _ input: LiveTaskCardInput,
        _ runs: [LiveTaskRunInput] = [],
        store: String? = nil
    ) -> LiveTaskCardState {
        var input = input
        input.runs = runs
        input.storeStatus = store
        return LiveTaskCardState(input)
    }

    // MARK: - liveTask.test.ts

    func testReadsADraftPlanAsWaitingForReviewNotAsRunning() {
        let v = view(message("m1", "draft"))
        XCTAssertEqual(v.status, .waiting)
        XCTAssertEqual(v.verb, "Plan ready for review")
        XCTAssertEqual(v.headline, "Plan ready for review")
    }

    func testLetsTheTaskStoreWinWhenTheCardMessageLagsItAtDraftOrPlanning() {
        let lagging = view(message("m1", "draft", summary: "Plan ready for review."), store: "ready")
        XCTAssertEqual(lagging.status, .queued)
        XCTAssertEqual(lagging.verb, "Plan approved, not started")
        XCTAssertNil(lagging.reason)
        XCTAssertTrue(lagging.startable)

        XCTAssertEqual(view(message("m1", "planning"), store: "pending").status, .queued)
        // A message that has moved on (a run started) is not overridden.
        XCTAssertEqual(view(message("m2", "running"), store: "ready").status, .running)
    }

    func testReadsAnApprovedOrUnplannedTaskAsNotStartedNeverAsRunning() {
        let approved = view(message("m1", "ready"))
        XCTAssertEqual(approved.status, .queued)
        XCTAssertEqual(approved.verb, "Plan approved, not started")
        XCTAssertFalse(approved.terminal)
        XCTAssertTrue(approved.startable)

        let unplanned = view(message("m2", "pending"))
        XCTAssertEqual(unplanned.status, .queued)
        XCTAssertEqual(unplanned.verb, "Not started")
        XCTAssertFalse(unplanned.startable)

        let rejected = view(message("m3", "rejected"))
        XCTAssertEqual(rejected.status, .queued)
        XCTAssertEqual(rejected.verb, "Plan rejected")
    }

    func testTakesIdTitleAndTheSummaryReasonFromTheContent() {
        let v = view(message("m1", "running", summary: "Checking CI. Then tags."))
        XCTAssertEqual(v.id, "task-1")
        XCTAssertEqual(v.title, "Review release")
        XCTAssertEqual(v.status, .running)
        XCTAssertEqual(v.reason, "Checking CI. Then tags.")
        XCTAssertFalse(v.terminal)
    }

    func testNeverTitlesTheCardWithARawId() {
        var m = message("m1", "running")
        m.displayLabel = nil
        XCTAssertEqual(view(m).title, "Task · 1")
        // Live: the card read `task_2679cefd6f664bf7bd4a3b57fae4ce75`.
        m.taskId = "task_2679cefd6f664bf7bd4a3b57fae4ce75"
        XCTAssertEqual(view(m).title, "Task · 2679ce")
        // The reason is not a title, even when it is the only prose there is.
        m.summary = "Searching flights."
        XCTAssertEqual(view(m).title, "Task · 2679ce")
        m.taskId = nil
        XCTAssertEqual(view(m).title, "Task")
        XCTAssertEqual(view(m).id, "m1")
    }

    func testTitlesFromTheDisplayLabelThroughTheDrawersTitleRule() {
        var m = message("m1", "running")
        // A routed prompt passed as the label becomes its goal.
        m.displayLabel = "web-researcher / Do not delegate further. Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL…"
        XCTAssertEqual(view(m).title, "Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL")
        // An id-shaped label is not a label.
        m.displayLabel = "task-1"
        XCTAssertEqual(view(m).title, "Task · 1")
    }

    /// Web: `'progress' in view` is false. Swift has no such probe; the type
    /// simply declares no progress fraction, so this pins the step data that
    /// does exist instead: a run list, never a percentage.
    func testCarriesNoProgressFraction() {
        let v = view(message("m1", "running"))
        XCTAssertEqual(v.steps, [])
        XCTAssertFalse(Mirror(reflecting: v).children.contains { $0.label == "progress" })
    }

    func testListsOneStepPerRunWithItsStatus() {
        let done = message("r1", "completed", executionId: "e1")
        let running = message("r2", "running", executionId: "e2", summary: "Rerunning the flaky suite.")
        let v = view(running, [group("e1", [done]), group("e2", [running])])
        XCTAssertEqual(v.steps.map(\.status), [.done, .running])
        XCTAssertEqual(v.steps.map(\.label), ["Run 1", "Run 2"])
        // The running run's label is a real step, distinct from the reason.
        XCTAssertEqual(v.currentStep, "Run 2")
        XCTAssertEqual(v.reason, "Rerunning the flaky suite.")
    }

    func testDropsTheSummaryDerivedStepWhenTheReasonAlreadySaysIt() {
        let running = message("r2", "running", executionId: "e2", summary: "Rerunning the flaky suite. Almost there.")
        let v = view(running, [group("e2", [running])])
        XCTAssertEqual(v.reason, "Rerunning the flaky suite. Almost there.")
        XCTAssertNil(v.currentStep)
    }

    func testShowsTheReasonForEveryStateThatHasOne() {
        let failed = view(message("m", "failed", summary: "Auth expired for `gh` - re-login needed."))
        XCTAssertEqual(failed.status, .failed)
        // Kept verbatim (markdown, hyphens intact): the card renders it as markdown.
        XCTAssertEqual(failed.reason, "Auth expired for `gh` - re-login needed.")

        let waiting = view(message("m", "paused", summary: "Needs your approval to send the e-mail."))
        XCTAssertEqual(waiting.status, .waiting)
        XCTAssertEqual(waiting.reason, "Needs your approval to send the e-mail.")

        let done = view(message("m", "completed", summary: "Release **v1.2** tagged."))
        XCTAssertEqual(done.reason, "Release **v1.2** tagged.")

        XCTAssertNil(view(message("m", "completed", summary: "   ")).reason)
        XCTAssertNil(view(message("m", "completed")).reason)
    }

    func testMapsAPartialCompletionToPartialTerminalWithItsDetailAsTheReason() {
        let v = view(message("m", "completed", summary: "Tagged; changelog still open.", completionKind: "partial"))
        XCTAssertEqual(v.status, .partial)
        XCTAssertTrue(v.terminal)
        XCTAssertEqual(v.verb, "Partially completed")
        XCTAssertEqual(v.reason, "Tagged; changelog still open.")

        let disposition = view(message("m", "partial_success"))
        XCTAssertEqual(disposition.status, .partial)
        XCTAssertTrue(disposition.terminal)

        XCTAssertEqual(view(message("m", "completed", completionKind: "full")).status, .completed)
    }

    func testDoesNotReadAPartialCompletionKindOnAStillRunningTask() {
        let v = view(message("m", "running", completionKind: "partial"))
        XCTAssertEqual(v.status, .running)
        XCTAssertFalse(v.terminal)
    }

    func testFallsBackToTheRunningStepLabelThenTheStatusVerb() {
        let running = message("r1", "running", executionId: "e1")
        XCTAssertEqual(view(running, [group("e1", [running])]).currentStep, "Run")
        XCTAssertEqual(view(message("m1", "planning")).currentStep, "Planning")
    }

    func testDetectsTerminalStatuses() {
        let done = message("r1", "completed", executionId: "e1", createdAt: t0 + 5_000)
        let started = message("r0", "running", executionId: "e1", createdAt: t0 + 2_000)
        let v = view(done, [group("e1", [started, done])])
        XCTAssertTrue(v.terminal)
        XCTAssertEqual(v.status, .completed)
        XCTAssertNil(v.currentStep)
        XCTAssertEqual(v.startedAt, Date(timeIntervalSince1970: (t0 + 2_000) / 1_000))
        XCTAssertEqual(v.finishedAt, Date(timeIntervalSince1970: (t0 + 5_000) / 1_000))

        XCTAssertTrue(view(message("m", "failed")).terminal)
        XCTAssertEqual(view(message("m", "cancelled")).status, .cancelled)
    }

    func testKeepsAFinishedButSynthesizingTaskRunning() {
        let v = view(message("m", "running", synthesisPending: true))
        XCTAssertFalse(v.terminal)
        XCTAssertEqual(v.currentStep, "Preparing final result")
    }

    func testMapsAPausedTaskToWaitingOnTheUser() {
        let paused = message("r1", "paused", executionId: "e1")
        let v = view(paused, [group("e1", [paused])])
        XCTAssertEqual(v.status, .waiting)
        XCTAssertFalse(v.terminal)
        XCTAssertEqual(v.steps.first?.status, .waiting)
        XCTAssertTrue(v.needsYou)
    }

    func testNormalisesSecondResolutionTimestampsToMilliseconds() {
        let v = view(message("m", "running", createdAt: 1_700_000_000))
        XCTAssertEqual(v.startedAt, Date(timeIntervalSince1970: 1_700_000_000))
        XCTAssertNil(v.finishedAt)
    }

    func testCollectsPerRunOutputsAndTheLatestRunIds() {
        let first = message("r1", "completed", executionId: "e1")
        let second = message("r2", "completed", executionId: "e2", outputs: 1)
        let v = view(second, [group("e1", [first]), group("e2", [second])])
        XCTAssertEqual(v.outputs.count, 1)
        XCTAssertEqual(v.outputs[0].key, "task:task-1::execution:e2")
        XCTAssertEqual(v.outputs[0].sessionId, "session-1")
        XCTAssertNotNil(v.outputs[0].label)
        XCTAssertEqual(v.outputs[0].count, 1)
        XCTAssertEqual(v.taskId, "task-1")
        XCTAssertEqual(v.executionId, "e2")
    }

    func testHasNoOutputsAndNoExecutionIdForABareStatusMessage() {
        let v = view(message("m1", "running"))
        XCTAssertEqual(v.outputs, [])
        XCTAssertNil(v.executionId)
    }

    // MARK: - LiveTaskCard.component.test.ts (state, not DOM)

    func testTicksTheElapsedTimeWhileRunning() {
        let v = view(message("m", "running"))
        XCTAssertTrue(v.showsTimer)
        XCTAssertTrue(v.ticks)
        let now = Date(timeIntervalSince1970: (t0 + 5_000) / 1_000)
        XCTAssertEqual(v.elapsedText(now: now), "0:05")
        XCTAssertEqual(v.elapsedText(now: now.addingTimeInterval(2)), "0:07")
    }

    func testDoesNotTickOnceTheTaskIsTerminal() {
        let v = view(message("m", "completed"))
        XCTAssertFalse(v.ticks)
        // A finished task still shows how long it took.
        XCTAssertEqual(v.elapsedText(now: .distantFuture), "0:00")
    }

    func testFormatsElapsedAsMinutesSecondsThenHours() {
        XCTAssertEqual(LiveTaskCardState.formatElapsed(0), "0:00")
        XCTAssertEqual(LiveTaskCardState.formatElapsed(65), "1:05")
        XCTAssertEqual(LiveTaskCardState.formatElapsed(3_725), "1:02:05")
        XCTAssertEqual(LiveTaskCardState.formatElapsed(-4), "0:00")
    }

    func testShowsAnApprovedUnstartedTaskAsNotStartedNoTimerNoCancel() {
        let v = view(message("r1", "ready", executionId: "e1"))
        XCTAssertEqual(v.verb, "Plan approved, not started")
        XCTAssertEqual(v.tagText, "Not started")
        XCTAssertFalse(v.showsTimer)
        XCTAssertFalse(v.ticks)
        XCTAssertNil(v.elapsedText(now: .distantFuture))
        XCTAssertFalse(v.canCancel)
        XCTAssertFalse(v.showsLiveActivity)
    }

    func testTagsOnlyAnUnstartedTask() {
        XCTAssertNil(view(message("m", "running")).tagText)
        XCTAssertNil(view(message("m", "draft")).tagText)
        XCTAssertEqual(view(message("m", "pending")).tagText, "Not started")
    }

    func testOffersCancelOnlyWhileLive() {
        XCTAssertTrue(view(message("m", "running")).canCancel)
        XCTAssertTrue(view(message("m", "planning")).canCancel)
        XCTAssertFalse(view(message("m", "completed")).canCancel)
        XCTAssertFalse(view(message("m", "failed")).canCancel)
        XCTAssertFalse(view(message("m", "pending")).canCancel)
    }

    func testOffersTheLiveActivityOnlyWhileRunningWithARealTaskId() {
        XCTAssertTrue(view(message("m", "running")).showsLiveActivity)
        XCTAssertFalse(view(message("m", "completed")).showsLiveActivity)
        var noTask = message("m", "running")
        noTask.taskId = nil
        XCTAssertFalse(view(noTask).showsLiveActivity)
    }

    func testOffersTheRunAndPlanPreviewAndAsksForThePlanWhileNotStarted() {
        let ready = view(message("r1", "ready"))
        XCTAssertTrue(ready.startable)
        XCTAssertTrue(ready.wantsPlan)
        XCTAssertTrue(ready.showsPlanPreview(planStepCount: 2))
        XCTAssertFalse(ready.showsPlanPreview(planStepCount: 0))
        XCTAssertTrue(ready.isExpandable(activityHasContent: false, planStepCount: 2, reasonOverflows: false))

        // A drafted plan waiting for review previews too; a running task does not.
        XCTAssertTrue(view(message("m", "draft")).wantsPlan)
        XCTAssertFalse(view(message("m", "running")).wantsPlan)
        XCTAssertFalse(view(message("m", "running")).showsPlanPreview(planStepCount: 3))
    }

    func testShowsNoRunsListAndIsNotExpandableForAFinishedSingleRunTask() {
        let run = message("r1", "completed", executionId: "exec-1")
        let v = view(run, [group("exec-1", [run])])
        XCTAssertFalse(v.showsRuns(liveActivityAvailable: false))
        XCTAssertFalse(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
    }

    func testShowsTheRunsListForAFinishedTaskWhoseRunsEndedDifferently() {
        let failed = message("r1", "failed", executionId: "e1")
        let done = message("r2", "completed", executionId: "e2")
        let v = view(done, [group("e1", [failed]), group("e2", [done])])
        XCTAssertTrue(v.showsRuns(liveActivityAvailable: false))
        XCTAssertEqual(v.steps.map(\.status), [.failed, .done])
        XCTAssertTrue(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
    }

    func testShowsNoRunsListForAFinishedTaskWhoseRunsAllEndedTheSameWay() {
        let a = message("r1", "completed", executionId: "e1")
        let b = message("r2", "completed", executionId: "e2")
        let v = view(b, [group("e1", [a]), group("e2", [b])])
        XCTAssertFalse(v.showsRuns(liveActivityAvailable: false))
        XCTAssertFalse(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
    }

    func testShowsTheRunsListWhileRunningWhenThereIsNoActivitySlot() {
        let run = message("r1", "running", executionId: "e1")
        let v = view(run, [group("e1", [run])])
        XCTAssertTrue(v.showsRuns(liveActivityAvailable: false))
        // With the activity slot the steps stream there instead.
        XCTAssertFalse(v.showsRuns(liveActivityAvailable: true))
    }

    func testIsNotExpandableWhenTheOnlyExtraWouldBeCancel() {
        // A planning task: no run id yet, an empty live activity slot.
        let v = view(message("m", "planning"))
        XCTAssertTrue(v.canCancel)
        XCTAssertFalse(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
    }

    func testBecomesExpandableOnceTheLiveActivityRendersSomething() {
        let m = message("m", "running", executionId: "exec-2")
        let v = view(m, [group("exec-2", [m])])
        XCTAssertFalse(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
        XCTAssertTrue(v.isExpandable(activityHasContent: true, planStepCount: 0, reasonOverflows: false))
    }

    func testOffersMoreWhenTheReasonOverflowsItsClamp() {
        let v = view(message("m", "completed", summary: "A long reason."))
        XCTAssertFalse(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
        XCTAssertTrue(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: true))
    }

    func testKeepsAFinishedTasksOutputsInTheBodyAndARunningTasksInTheDetails() {
        let done = message("r1", "completed", executionId: "e1", outputs: 1)
        let finished = view(done, [group("e1", [done])])
        XCTAssertTrue(finished.outputsInBody)
        XCTAssertFalse(finished.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))

        let partial = message("r1", "running", executionId: "e1", outputs: 1)
        let running = view(partial, [group("e1", [partial])])
        XCTAssertFalse(running.outputsInBody)
        XCTAssertTrue(running.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false,
                                           liveActivityAvailable: true))
    }

    func testShowsTheFirstTwoOutputsAndCountsTheRestBehindMore() {
        let done = message("r1", "completed", executionId: "e1", outputs: 5)
        let v = view(done, [group("e1", [done])])
        XCTAssertEqual(v.hiddenOutputCount(expanded: false), 3)
        XCTAssertEqual(v.hiddenOutputCount(expanded: true), 0)
        XCTAssertTrue(v.isExpandable(activityHasContent: false, planStepCount: 0, reasonOverflows: false))
    }

    func testTheBareExpandabilityRuleIsAnyOfItsInputs() {
        XCTAssertFalse(LiveTaskCardState.isExpandable(
            hasSteps: false, hasRuns: false, hasOutputs: false,
            hasClampedReason: false, hasHiddenOutputs: false, hasPlanPreview: false))
        XCTAssertTrue(LiveTaskCardState.isExpandable(
            hasSteps: false, hasRuns: false, hasOutputs: false,
            hasClampedReason: false, hasHiddenOutputs: false, hasPlanPreview: true))
        XCTAssertTrue(LiveTaskCardState.isExpandable(
            hasSteps: true, hasRuns: false, hasOutputs: false,
            hasClampedReason: false, hasHiddenOutputs: false, hasPlanPreview: false))
    }

    // MARK: - iOS chat inputs

    func testBuildsFromTheChatTaskStatusModel() {
        let model = TaskStatusModel(
            taskId: "task-1", title: "Review release", status: "draft", steps: [],
            summary: "Plan ready for review.", executionId: "e1"
        )
        let lagging = LiveTaskCardState(LiveTaskCardInput(task: model, messageId: "m1", storeStatus: "ready"))
        XCTAssertEqual(lagging.status, .queued)
        XCTAssertNil(lagging.reason)
        XCTAssertEqual(lagging.title, "Review release")

        // The chat falls back to the raw id as the title; the card never shows it.
        let untitled = TaskStatusModel(
            taskId: "task_2679cefd6f664bf7bd4a3b57fae4ce75",
            title: "task_2679cefd6f664bf7bd4a3b57fae4ce75", status: "running", steps: []
        )
        XCTAssertEqual(LiveTaskCardState(LiveTaskCardInput(task: untitled, messageId: "m2")).title, "Task · 2679ce")
    }
}
