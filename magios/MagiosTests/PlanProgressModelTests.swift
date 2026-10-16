import XCTest
@testable import Magician

/// Planning phases and plan-step marks from the `/execution-panel` payload.
/// The planning-entry cases mirror `planProgressFrom` in
/// `ui/unified-ui/src/lib/magician/tasks/taskPanelModel.test.ts`; the stage →
/// phase table is design §4 of
/// `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`; the
/// step statuses are `v3_adapter.rs::plan_step_statuses_from` plus the phase-2
/// skip and note.
final class PlanProgressModelTests: XCTestCase {
    private func entry(
        _ id: String,
        at ms: Double,
        message: String,
        severity: String = "info",
        title: String = "Progress",
        execution: String? = "planexec_1",
        stage: String? = nil
    ) -> [String: Any] {
        var row: [String: Any] = [
            "id": id, "timestamp": ms, "severity": severity, "title": title, "message": message
        ]
        if let execution { row["execution_id"] = execution }
        if let stage { row["planning_stage"] = stage }
        return row
    }

    /// The web fixture: an older server's shared timeline.
    private func olderPanel() -> [String: Any] {
        ["debug": ["timeline": [
            entry("p2", at: 30_000, message: "Outlining the plan"),
            entry("p1", at: 20_000, message: "Understood the request"),
            entry("p3", at: 40_000, message: "Planning hit a problem: boom", severity: "warning"),
            entry("status", at: 10_000, message: "Execution entered planning", title: "Status changed to Planning"),
            entry("run-progress", at: 50_000, message: "Execution is running.", execution: "exec-run")
        ]]]
    }

    private func date(_ ms: Double) -> Date { Date(timeIntervalSince1970: ms / 1_000) }

    // MARK: - Entries (web planProgressFrom)

    func testListsThePlanningMilestonesOldestFirstFromTheSharedTimelineOnAnOlderServer() {
        let entries = PlanProgress.entries(fromPanel: olderPanel())
        XCTAssertEqual(entries.map(\.id), ["p1", "p2", "p3"])
        XCTAssertEqual(entries.map(\.at), [date(20_000), date(30_000), date(40_000)])
        XCTAssertEqual(entries.map(\.message),
                       ["Understood the request", "Outlining the plan", "Planning hit a problem: boom"])
        XCTAssertEqual(entries.map(\.warning), [false, false, true])
    }

    func testReadsTheDedicatedPlanningTimelineWhenTheServerSendsOne() {
        var panel = olderPanel()
        var debug = panel["debug"] as! [String: Any]
        debug["planning_timeline"] = [
            entry("llm1", at: 15_000, message: "Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens")
        ]
        panel["debug"] = debug
        let entries = PlanProgress.entries(fromPanel: panel)
        XCTAssertEqual(entries.map(\.id), ["llm1"])
        XCTAssertEqual(entries[0].message, "Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens")
        XCTAssertFalse(entries[0].warning)
        XCTAssertTrue(entries[0].isModelCall)
        XCTAssertEqual(PlanProgress.entries(fromPanel: [:]), [])
    }

    func testAnEmptyDedicatedTimelineIsTheAnswerNotAReasonToFallBack() {
        var panel = olderPanel()
        var debug = panel["debug"] as! [String: Any]
        debug["planning_timeline"] = [[String: Any]]()
        panel["debug"] = debug
        XCTAssertEqual(PlanProgress.entries(fromPanel: panel), [])
    }

    func testCountsAnErrorAsAWarning() {
        let panel: [String: Any] = ["debug": ["planning_timeline": [
            entry("e", at: 1_000, message: "Planning hit a problem: x", severity: "error")
        ]]]
        XCTAssertEqual(PlanProgress.entries(fromPanel: panel).map(\.warning), [true])
    }

    // MARK: - Stage

    func testReadsTheStageFieldWhenPresent() {
        XCTAssertEqual(PlanProgress.stage(of: entry("x", at: 1, message: "anything", stage: "outline")), "outline")
    }

    func testFallsBackToTheStageSegmentOfThePlanningId() {
        XCTAssertEqual(
            PlanProgress.stage(of: entry("planning:planexec_1:expansion:1760000000000", at: 1, message: "m")),
            "expansion"
        )
        XCTAssertEqual(
            PlanProgress.stage(of: entry("planning:planexec_1:plan_retry:1760000000000", at: 1, message: "m")),
            "plan_retry"
        )
        // Not a planning id: no stage from it.
        XCTAssertNil(PlanProgress.stage(of: entry("b9f1c3de-uuid", at: 1, message: "Execution entered planning")))
        XCTAssertNil(PlanProgress.stage(of: entry("status:a:b:c", at: 1, message: "m")))
    }

    func testFallsBackToTheBackendsFixedMilestoneWordingLast() {
        let cases: [(String, String)] = [
            ("Understood the request", "query_analysis"),
            ("Picking out the details the task needs", "slot_extraction"),
            ("Restated the task (2 objectives)", "clarified_task"),
            ("Outlining the plan", "outline"),
            ("Outlined 3 goals", "outline_done"),
            ("Working out the steps for 3 goals", "expansion"),
            ("Checking the drafted plan", "validation"),
            ("Plan drafted and checked", "plan_ready"),
            ("The draft did not pass its checks; revising (attempt 2)", "plan_retry"),
            ("Planning hit a problem: boom", "failed"),
            ("Outlined the plan · gpt · 12.0s", "llm"),
            ("Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens", "llm")
        ]
        for (message, stage) in cases {
            XCTAssertEqual(PlanProgress.stage(of: entry("uuid", at: 1, message: message)), stage, message)
        }
        XCTAssertNil(PlanProgress.stage(of: entry("uuid", at: 1, message: "Execution entered planning")))
    }

    func testTheStageFieldOutranksTheIdAndTheIdOutranksTheWording() {
        let fielded = entry("planning:planexec_1:outline:1", at: 1, message: "Plan drafted and checked", stage: "validation")
        XCTAssertEqual(PlanProgress.stage(of: fielded), "validation")
        let ided = entry("planning:planexec_1:outline:1", at: 1, message: "Plan drafted and checked")
        XCTAssertEqual(PlanProgress.stage(of: ided), "outline")
    }

    // MARK: - Stage → phase (design §4)

    func testMapsEveryStageToItsPhase() {
        let table: [String: PlanningPhase] = [
            "query_analysis": .understand, "slot_extraction": .understand, "clarified_task": .understand,
            "outline": .outline, "outline_done": .outline,
            "expansion": .steps, "plan_retry": .steps,
            "validation": .check, "plan_ready": .check
        ]
        for (stage, phase) in table {
            XCTAssertEqual(PlanProgress.phase(for: stage), phase, stage)
        }
        XCTAssertNil(PlanProgress.phase(for: "llm"))
        XCTAssertNil(PlanProgress.phase(for: "failed"))
        XCTAssertNil(PlanProgress.phase(for: "something_new"))
    }

    func testNamesThePhasesInTrackOrder() {
        XCTAssertEqual(PlanningPhase.allCases.map(\.title), ["Understand", "Outline", "Steps", "Check"])
    }

    // MARK: - Current phase

    private func entries(_ stages: [String]) -> [PlanningEntry] {
        stages.enumerated().map { index, stage in
            PlanningEntry(id: "e\(index)", at: date(Double(index) * 1_000), message: stage,
                          warning: false, stage: stage, isModelCall: stage == "llm")
        }
    }

    func testTheLatestPhaseReachedIsCurrent() {
        let track = PlanProgress.currentPhase(entries(["query_analysis", "outline", "outline_done", "expansion"]))
        XCTAssertEqual(track, PlanningTrack(phase: .steps, retrying: false, failed: false))
        XCTAssertEqual(PlanProgress.currentPhase([]), PlanningTrack(phase: nil, retrying: false, failed: false))
    }

    func testARetryStepsBackToSteps() {
        let track = PlanProgress.currentPhase(entries(["outline", "expansion", "validation", "plan_retry"]))
        XCTAssertEqual(track, PlanningTrack(phase: .steps, retrying: true, failed: false))
        // The revision moving on clears the retry.
        let moved = PlanProgress.currentPhase(entries(["validation", "plan_retry", "validation"]))
        XCTAssertEqual(moved, PlanningTrack(phase: .check, retrying: false, failed: false))
    }

    func testModelCallsNeverMoveTheTrack() {
        let track = PlanProgress.currentPhase(entries(["query_analysis", "outline", "llm", "llm"]))
        XCTAssertEqual(track, PlanningTrack(phase: .outline, retrying: false, failed: false))
        XCTAssertEqual(PlanProgress.currentPhase(entries(["llm"])).phase, nil)
    }

    func testFailedKeepsTheCurrentPhaseAndMarksIt() {
        let track = PlanProgress.currentPhase(entries(["query_analysis", "outline", "failed"]))
        XCTAssertEqual(track, PlanningTrack(phase: .outline, retrying: false, failed: true))
        // A durable retry that resumes planning clears the failure.
        let resumed = PlanProgress.currentPhase(entries(["outline", "failed", "llm", "expansion"]))
        XCTAssertEqual(resumed, PlanningTrack(phase: .steps, retrying: false, failed: false))
    }

    func testTheOlderServerTimelineDrivesTheTrackThroughTheWordingFallback() {
        let track = PlanProgress.currentPhase(PlanProgress.entries(fromPanel: olderPanel()))
        XCTAssertEqual(track, PlanningTrack(phase: .outline, retrying: false, failed: true))
    }

    func testSummarisesPlanningActivityAsCountAndDuration() {
        let rows = PlanProgress.entries(fromPanel: olderPanel())
        XCTAssertEqual(PlanProgress.activitySummary(rows), "3 events · 20s")
        XCTAssertEqual(PlanProgress.activitySummary(Array(rows.prefix(1))), "1 event")
        XCTAssertEqual(PlanProgress.activitySummary([]), "")
    }

    // MARK: - Plan steps

    private func panel(_ statuses: [[String: Any]]) -> [String: Any] {
        ["debug": ["selected_execution": ["step_statuses": statuses]]]
    }

    private func step(_ number: Int, _ name: String, _ status: String, note: String? = nil,
                      progress: String = "") -> [String: Any] {
        var row: [String: Any] = ["number": number, "name": name, "status": status,
                                  "progress": progress, "step_id": "s\(number)"]
        if let note { row["note"] = note }
        return row
    }

    func testMapsEachStepStatusToItsMark() {
        let rows = PlanSteps.rows(fromPanel: panel([
            step(1, "Look up the weather", "completed"),
            step(2, "Draft the brief", "failed"),
            step(3, "Check the facts", "in_progress"),
            step(4, "Send it", "pending"),
            step(5, "Archive", "skipped"),
            step(6, "Celebrate", "teleported")
        ]), runEnded: false)
        XCTAssertEqual(rows.map(\.mark), [.done, .failed, .inProgress, .pending, .skipped, .pending])
        XCTAssertEqual(rows.map(\.number), [1, 2, 3, 4, 5, 6])
        XCTAssertEqual(rows.map(\.name).first, "Look up the weather")
        XCTAssertEqual(rows.map(\.stepId).first, "s1")
    }

    func testReadsTheAgentsReasonFromNoteNotProgressOnAnyMark() {
        let rows = PlanSteps.rows(fromPanel: panel([
            step(1, "Search flights", "skipped", note: "  The user already booked one.  "),
            step(2, "Compare fares", "completed", note: "Used the cached fares instead."),
            step(3, "Pay", "failed", note: "Card declined."),
            step(4, "Email", "pending", progress: "50%")
        ]), runEnded: false)
        XCTAssertEqual(rows.map(\.note), ["The user already booked one.", "Used the cached fares instead.",
                                          "Card declined.", nil])
    }

    func testAPendingStepReadsNotReachedOnceTheRunEnded() {
        let statuses = [step(1, "A", "completed"), step(2, "B", "pending"), step(3, "C", "teleported")]
        XCTAssertEqual(PlanSteps.rows(fromPanel: panel(statuses), runEnded: true).map(\.mark),
                       [.done, .notReached, .notReached])
        XCTAssertEqual(PlanSteps.rows(fromPanel: panel(statuses), runEnded: false).map(\.mark),
                       [.done, .pending, .pending])
    }

    func testKeepsThePlansOrderWhenStepsFinishOutOfOrder() {
        let rows = PlanSteps.rows(fromPanel: panel([
            step(1, "A", "pending"), step(2, "B", "completed"), step(3, "C", "in_progress")
        ]), runEnded: false)
        XCTAssertEqual(rows.map(\.name), ["A", "B", "C"])
    }

    func testNamesAStepWithNoNameByItsPositionAndToleratesMissingFields() {
        let rows = PlanSteps.rows(fromPanel: panel([["status": "completed"], ["name": "  ", "number": 2]]),
                                  runEnded: false)
        XCTAssertEqual(rows.map(\.name), ["Step 1", "Step 2"])
        XCTAssertEqual(rows.map(\.mark), [.done, .pending])
        XCTAssertEqual(PlanSteps.rows(fromPanel: [:], runEnded: true), [])
    }

    func testSummarisesTheRunAgainstThePlan() {
        let live = PlanSteps.rows(fromPanel: panel([
            step(1, "A", "completed"), step(2, "B", "completed"), step(3, "Check the facts", "in_progress"),
            step(4, "D", "pending"), step(5, "E", "pending"), step(6, "F", "pending")
        ]), runEnded: false)
        XCTAssertEqual(PlanSteps.summary(live), "Step 3 of 6 · Check the facts")

        let done = (1...6).map { step($0, "S\($0)", "completed") }
        XCTAssertEqual(PlanSteps.summary(PlanSteps.rows(fromPanel: panel(done), runEnded: true)), "6 of 6 done")

        let failed = PlanSteps.rows(fromPanel: panel([
            step(1, "A", "completed"), step(2, "Pay", "failed"), step(3, "C", "pending")
        ]), runEnded: true)
        XCTAssertEqual(PlanSteps.summary(failed), "Step 2 of 3 failed · Pay")

        let skipped = PlanSteps.rows(fromPanel: panel([
            step(1, "A", "completed"), step(2, "B", "skipped"), step(3, "C", "completed")
        ]), runEnded: true)
        XCTAssertEqual(PlanSteps.summary(skipped), "2 of 3 done · 1 skipped")

        XCTAssertEqual(PlanSteps.summary([]), "")
    }

    // MARK: - Off-plan attribution (web planStepActivity.test.ts, Android PlanStepActivityTest)

    private func row(_ id: String, at seconds: Double, step: String?, offPlan: Bool = false) -> TaskDetailActivity {
        TaskDetailActivity(
            id: id, title: id, body: nil, status: "done", kind: "tool", agentId: nil,
            timestamp: Date(timeIntervalSince1970: seconds), eventType: "tool.succeeded",
            latencyMs: nil, model: nil, costUsd: nil, inputTokens: nil, outputTokens: nil,
            cacheReadTokens: nil, executionId: "exec-1", planStepId: step, offPlan: offPlan)
    }

    func testAnUnattributedRunIsLeftUnchanged() {
        let groups = PlanStepActivity.group([row("a", at: 1, step: nil), row("b", at: 2, step: nil)])
        XCTAssertFalse(groups.attributed)
        XCTAssertTrue(groups.byStep.isEmpty)
        XCTAssertTrue(groups.offPlan.isEmpty)
        XCTAssertFalse(PlanStepActivity.group([]).attributed)
    }

    func testGroupsRowsUnderTheirStepAndTheRestOutsideThePlanOldestFirst() {
        let groups = PlanStepActivity.group([
            row("after", at: 9, step: nil, offPlan: true),
            row("s2-call", at: 7, step: "s2"),
            row("s1-second", at: 3, step: "s1"),
            row("before", at: 1, step: nil, offPlan: true),
            row("s1-first", at: 2, step: "s1"),
            row("between", at: 5, step: nil, offPlan: true)
        ])
        XCTAssertTrue(groups.attributed)
        XCTAssertEqual(groups.byStep["s1"]?.map(\.id), ["s1-first", "s1-second"])
        XCTAssertEqual(groups.byStep["s2"]?.map(\.id), ["s2-call"])
        XCTAssertEqual(groups.offPlan.map(\.id), ["before", "between", "after"])
    }

    func testIgnoresRowsThatCarryNoAttributionInAnAttributedRun() {
        let groups = PlanStepActivity.group([row("lifecycle", at: 1, step: nil), row("s1", at: 2, step: "s1")])
        XCTAssertTrue(groups.attributed)
        XCTAssertTrue(groups.offPlan.isEmpty)
        XCTAssertEqual(groups.byStep["s1"]?.count, 1)
    }

    func testCountsAStepWithNoRowsAsZeroAndNothingWhenUnattributed() {
        let groups = PlanStepActivity.group([row("s1", at: 2, step: "s1")])
        XCTAssertEqual(groups.count(for: "s1"), 1)
        XCTAssertEqual(groups.count(for: "s3"), 0)
        XCTAssertNil(PlanStepActivity.group([]).count(for: "s1"))
    }

    func testLabelsTheOffPlanGroupWithItsCountAndExplanation() {
        XCTAssertEqual(PlanStepActivity.offPlanLabel(3), "Outside the plan · 3")
        XCTAssertEqual(PlanStepActivity.offPlanExplanation, "Work the agent did between or beyond the planned steps")
    }

    func testReadsEachStepsActivityCount() {
        let rows = PlanSteps.rows(fromPanel: panel([
            ["number": 1, "name": "A", "status": "completed", "step_id": "s1", "activity_count": 4],
            ["number": 2, "name": "B", "status": "pending", "step_id": "s2"]
        ]), runEnded: false)
        XCTAssertEqual(rows.map(\.activityCount), [4, nil])
    }
}
