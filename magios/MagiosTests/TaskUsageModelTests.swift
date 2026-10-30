import XCTest
@testable import Magician

/// Model usage per task and per turn. The ledger and planning cases mirror
/// `ui/unified-ui/src/lib/magician/chat/next/taskUsage.test.ts`; the run
/// reading follows `runCost` in `taskTimeline.ts`; the formatting follows
/// `formatUsd`, `formatTokenCount` and `llmUsageSuffix` on the web.
final class TaskUsageModelTests: XCTestCase {
    // MARK: Ledger (web taskUsage.test.ts)

    func testSumsCallsTokensAndCostPerTask() {
        var ledger = TaskUsageLedger()
        ledger.record(taskId: "task-1", callKey: "call-a", tokens: 1200, costUsd: 0.01)
        ledger.record(taskId: "task-1", callKey: "call-b", tokens: 800, costUsd: nil)
        ledger.record(taskId: "task-2", callKey: "call-c", tokens: 50, costUsd: 0.002)

        XCTAssertEqual(ledger.total(for: "task-1")?.usage, TaskUsage(calls: 2, tokens: 2000, costUsd: 0.01))
        XCTAssertEqual(ledger.total(for: "task-2")?.usage, TaskUsage(calls: 1, tokens: 50, costUsd: 0.002))
    }

    func testCountsAReplayedCallOnce() {
        var ledger = TaskUsageLedger()
        XCTAssertTrue(ledger.record(taskId: "task-1", callKey: "call-a", tokens: 1200, costUsd: 0.01))
        XCTAssertFalse(ledger.record(taskId: "task-1", callKey: "call-a", tokens: 1200, costUsd: 0.01))
        XCTAssertEqual(ledger.total(for: "task-1")?.usage.calls, 1)
    }

    func testReportsNoCostUntilACallIsPriced() {
        var ledger = TaskUsageLedger()
        ledger.record(taskId: "task-1", callKey: "call-a", tokens: 100, costUsd: nil)
        XCTAssertNil(ledger.total(for: "task-1")?.usage.costUsd)
    }

    func testNothingReportedIsNoUsageRatherThanZeros() {
        XCTAssertNil(TaskUsageLedger().total(for: "task-1"))
        XCTAssertNil(TaskUsageModel.total(live: nil, snapshot: TaskUsageSnapshot(run: nil, planning: nil)))
    }

    func testLateSubscriptionUnionsStoredLiveAndAuxiliaryCalls() {
        var ledger = TaskUsageLedger()
        let stored = ["a": TaskCallUsage(tokens: 1000, costUsd: 0.01, executionId: "run")]
        ledger.setSnapshot(TaskUsageSnapshot(run: TaskUsageModel.sumCalls(stored),
            planning: TaskUsage(calls: 2, tokens: 900, costUsd: 0.01), executionId: "run", calls: stored), for: "task")
        ledger.record(taskId: "task", callKey: "a", tokens: 1000, costUsd: nil, executionId: "run")
        ledger.record(taskId: "task", callKey: "b", tokens: 100, costUsd: nil, executionId: "run")
        let history = TaskRunEvidence.history(["columns": ["llm_call_id", "input_tokens", "output_tokens", "cost_usd"],
            "rows": [["a", 800, 200, 0.01], ["judge", 20, 10, 0.002]]], executionId: "run")
        ledger.setSnapshot(TaskUsageSnapshot(run: TaskUsageModel.sumCalls(history), planning: nil,
            executionId: "run", calls: history), for: "task")
        let total = ledger.total(for: "task")
        XCTAssertEqual(total?.run?.calls, 3)
        XCTAssertEqual(total?.run?.tokens, 1130)
        XCTAssertEqual(total?.planning?.calls, 2)
        XCTAssertEqual(total?.usage.calls, 5)
        XCTAssertEqual(total?.usage.tokens, 2030)
        XCTAssertEqual(total?.usage.costUsd ?? -1, 0.022, accuracy: 1e-12)
    }

    func testReplayedPriceOnlyCallIsEnrichedWithoutDuplicatingIt() {
        var ledger = TaskUsageLedger()
        ledger.record(taskId: "task", callKey: "a", tokens: nil, costUsd: 0.01)
        XCTAssertEqual(TaskUsageFormat.taskLine(ledger.total(for: "task")!.usage), "1 call · tokens unavailable · $0.010")
        ledger.record(taskId: "task", callKey: "a", tokens: 250, costUsd: nil)
        XCTAssertEqual(ledger.total(for: "task")?.usage, TaskUsage(calls: 1, tokens: 250, costUsd: 0.01))
    }

    func testPartialSnapshotsKeepHistoryButNewExecutionsResetIt() {
        var ledger = TaskUsageLedger()
        func snapshot(_ root: String, _ at: Double, _ ids: Set<String>) -> TaskUsageSnapshot {
            let calls = [root: TaskCallUsage(tokens: 100, costUsd: nil, executionId: root)]
            return TaskUsageSnapshot(run: TaskUsageModel.sumCalls(calls), planning: nil,
                executionId: root, calls: calls, activityIds: ids, updatedAt: at)
        }
        ledger.setSnapshot(snapshot("old", 10, ["a", "b"]), for: "task")
        ledger.setSnapshot(snapshot("old", 9, ["b"]), for: "task")
        XCTAssertEqual(ledger.total(for: "task")?.activityCount, 2)
        ledger.setSnapshot(snapshot("new", 20, ["c"]), for: "task")
        XCTAssertFalse(ledger.setSnapshot(snapshot("old", 30, ["a"]), for: "task"))
        XCTAssertFalse(ledger.record(taskId: "task", callKey: "late", tokens: 999, costUsd: nil, executionId: "old"))
        XCTAssertEqual(ledger.total(for: "task")?.executionId, "new")
        XCTAssertEqual(ledger.total(for: "task")?.usage.tokens, 100)
        XCTAssertEqual(ledger.total(for: "task")?.activityCount, 1)
    }

    func testWorkEvidenceRequiresMatchingRootAndActualWork() {
        let panel: [String: Any] = ["overview": ["execution_id": "run"], "run": ["activity_log": [
            ["created_at": 2000, "metadata": ["event_type": "tool.call.started"]],
            ["created_at": 9000, "metadata": ["event_type": "heartbeat"]],
            ["created_at": 10000, "metadata": ["event_type": "llm.succeeded", "provider_attempt_count": 0]]]]]
        XCTAssertEqual(TaskRunEvidence.progressAt(panel, executionId: "run"), Date(timeIntervalSince1970: 2))
        XCTAssertNil(TaskRunEvidence.progressAt(panel, executionId: "another-run"))
        XCTAssertFalse(TaskRunEvidence.isProgress("metrics.updated"))
        let event: [String: Any] = ["event_type": "AgentEvent", "data": ["event": ["event_type": "llm.first_token",
            "principal": "owner", "workspace": "default", "timestamp": 5000,
            "payload": ["execution_id": "child", "root_execution_id": "run"]]]]
        XCTAssertEqual(TaskRunEvidence.progressEvent(event, principal: "owner", workspace: "default")?.0, "run")
        XCTAssertNil(TaskRunEvidence.progressEvent(event, principal: "other", workspace: "default"))
    }

    // MARK: Snapshot from /execution-panel

    func testReadsPlanningUsageFromThePlanningTimeline() {
        let panel: [String: Any] = [
            "overview": ["task_id": "task-1", "status": "ready"],
            "run": [:],
            "debug": [
                "timeline": [],
                "planning_timeline": [
                    ["id": "a", "timestamp": 1, "severity": "info", "title": "Progress", "message": "Outlined", "cost_usd": 0.02, "tokens": 9000],
                    ["id": "b", "timestamp": 2, "severity": "info", "title": "Progress", "message": "Outlining the plan"],
                    ["id": "c", "timestamp": 3, "severity": "info", "title": "Progress", "message": "Worked out the steps", "cost_usd": 0.03, "tokens": 27000]
                ]
            ]
        ]
        let snapshot = TaskUsageModel.usageSnapshot(fromPanel: panel)
        XCTAssertNil(snapshot.run)
        XCTAssertEqual(snapshot.planning?.calls, 2)
        XCTAssertEqual(snapshot.planning?.tokens, 36000)
        XCTAssertEqual(snapshot.planning?.costUsd ?? -1, 0.05, accuracy: 1e-10)
    }

    /// A boot-time backfill writes one summary row per planning execution,
    /// `llm_calls` standing for the calls it sums.
    func testABackfilledPlanningRowCountsItsCalls() {
        let panel: [String: Any] = ["debug": ["planning_timeline": [
            ["id": "a", "message": "Planning model usage", "planning_stage": "llm", "llm_calls": 4, "tokens": 12000, "cost_usd": 0.04],
            ["id": "b", "message": "Outlined", "tokens": 1000, "cost_usd": 0.01]
        ]]]
        let planning = TaskUsageModel.usageSnapshot(fromPanel: panel).planning
        XCTAssertEqual(planning?.calls, 5)
        XCTAssertEqual(planning?.tokens, 13000)
    }

    func testAnOlderPayloadWithoutPlanningUsageHasNone() {
        let panel: [String: Any] = ["debug": ["planning_timeline": [["id": "a", "message": "Outlined"]]]]
        XCTAssertNil(TaskUsageModel.usageSnapshot(fromPanel: panel).planning)
    }

    func testRunUsageSumsTheActivityLogLikeRunCost() {
        let panel: [String: Any] = [
            "run": [
                "activity_log": [
                    ["id": "1", "metadata": ["event_type": "llm.requested"]],
                    ["id": "2", "metadata": ["event_type": "llm.succeeded", "input_tokens": 1000, "output_tokens": 200,
                                             "cache_read_tokens": 800, "cost_usd": 0.01]],
                    // Older snapshots priced task-run calls as `cost`.
                    ["id": "3", "metadata": ["event_type": "llm.succeeded", "output_tokens": 50, "cost": 0.002]],
                    // A price on a non-LLM row is not a model call's price.
                    ["id": "4", "metadata": ["event_type": "tool.call.finished", "cost_usd": 9]],
                    // Cache-only usage is a reported call with no in/out tokens.
                    ["id": "5", "metadata": ["event_type": "llm.succeeded", "cache_read_tokens": 10]]
                ],
                // Ignored while the full log is present.
                "recent_activity": [["id": "x", "metadata": ["event_type": "llm.succeeded", "input_tokens": 99999]]]
            ]
        ]
        let run = TaskUsageModel.usageSnapshot(fromPanel: panel).run
        XCTAssertEqual(run?.calls, 3)
        XCTAssertEqual(run?.tokens, 1250)
        XCTAssertEqual(run?.costUsd ?? -1, 0.012, accuracy: 1e-12)
    }

    func testRunUsageFallsBackToRecentActivityAndIsNilWhenNothingReported() {
        let recent: [String: Any] = ["run": ["activity_log": [], "recent_activity": [
            ["id": "1", "metadata": ["event_type": "llm.succeeded", "input_tokens": 10, "output_tokens": 5]]]]]
        XCTAssertEqual(TaskUsageModel.usageSnapshot(fromPanel: recent).run, TaskUsage(calls: 1, tokens: 15, costUsd: nil))

        let unbilled: [String: Any] = ["run": ["activity_log": [
            ["id": "1", "metadata": ["event_type": "llm.succeeded", "latency_ms": 900]]]]]
        XCTAssertNil(TaskUsageModel.usageSnapshot(fromPanel: unbilled).run)
        XCTAssertNil(TaskUsageModel.usageSnapshot(fromPanel: [:]).run)
    }

    func testABooleanIsNotATokenCount() {
        let panel: [String: Any] = ["run": ["activity_log": [
            ["id": "1", "metadata": ["event_type": "llm.succeeded", "input_tokens": true]]]]]
        XCTAssertNil(TaskUsageModel.usageSnapshot(fromPanel: panel).run)
    }

    // MARK: Formatting

    /// The cost rule shared with web `formatUsd` and magdroid
    /// `TaskUsageFormat.usd`: three decimals below a dollar, two from a dollar
    /// up, half-up, and a positive amount under half a thousandth reads
    /// `<$0.001`. The same cases on all three platforms.
    func testUsdMatchesWebFormatUsd() {
        let cases: [(Double?, String?)] = [
            (0, "$0.000"),
            (0.0001, "<$0.001"),
            (0.0004, "<$0.001"),
            (0.0005, "$0.001"),
            (0.00503936, "$0.005"),
            (0.04, "$0.040"),
            (0.999, "$0.999"),
            (0.9996, "$1.00"),
            (1, "$1.00"),
            (3.225331, "$3.23"),
            (12, "$12.00"),
            (nil, nil)
        ]
        for (amount, expected) in cases {
            XCTAssertEqual(TaskUsageFormat.usd(amount), expected, "\(String(describing: amount))")
        }
        XCTAssertNil(TaskUsageFormat.usd(-1))
        XCTAssertNil(TaskUsageFormat.usd(.nan))
    }

    func testSpokenUsdSaysLessThanATenthOfACent() {
        XCTAssertEqual(TaskUsageFormat.spokenUsd(0.0001), "less than a tenth of a cent")
        XCTAssertEqual(TaskUsageFormat.spokenUsd(3.225331), "$3.23")
        XCTAssertNil(TaskUsageFormat.spokenUsd(nil))
        let total = TaskUsageTotal(usage: TaskUsage(calls: 1, tokens: 10, costUsd: 0.0002), run: nil, planning: nil)
        XCTAssertEqual(TaskUsageFormat.taskAccessibilityLabel(total), "1 model call, 10 tokens, less than a tenth of a cent")
    }

    func testCompactTokens() {
        XCTAssertEqual(TaskUsageFormat.compactTokens(999), "999")
        XCTAssertEqual(TaskUsageFormat.compactTokens(1000), "1.0k")
        XCTAssertEqual(TaskUsageFormat.compactTokens(12_345), "12.3k")
        XCTAssertEqual(TaskUsageFormat.compactTokens(0), "0")
    }

    func testTokenCountMatchesWebFormatTokenCount() {
        XCTAssertEqual(TaskUsageFormat.tokenCount(0), "0")
        XCTAssertEqual(TaskUsageFormat.tokenCount(850), "850")
        XCTAssertEqual(TaskUsageFormat.tokenCount(9_100), "9.1k")
        XCTAssertEqual(TaskUsageFormat.tokenCount(42_400), "42k")
        XCTAssertEqual(TaskUsageFormat.tokenCount(1_234_567), "1.2M")
    }

    func testStepSuffixAndWithUsage() {
        XCTAssertEqual(TaskUsageFormat.stepSuffix(tokens: 9_100, costUsd: 0.0123), " · 9.1k tok · $0.012")
        XCTAssertEqual(TaskUsageFormat.stepSuffix(tokens: nil, costUsd: nil), "")
        XCTAssertEqual(TaskUsageFormat.stepSuffix(tokens: 0, costUsd: nil), "")
        XCTAssertEqual(TaskUsageFormat.withUsage("700ms (2.1s)", suffix: " · 9.1k tok"), "700ms (2.1s) · 9.1k tok")
        XCTAssertEqual(TaskUsageFormat.withUsage(nil, suffix: " · 9.1k tok · $0.01"), "9.1k tok · $0.01")
        XCTAssertNil(TaskUsageFormat.withUsage(nil, suffix: ""))
        XCTAssertEqual(TaskUsageFormat.withUsage("detail", suffix: ""), "detail")
    }

    func testTaskLineIsHiddenWithNoCalls() {
        XCTAssertEqual(TaskUsageFormat.taskLine(TaskUsage(calls: 3, tokens: 12_345, costUsd: 0.04)), "3 calls · 12.3k tok · $0.040")
        XCTAssertEqual(TaskUsageFormat.taskLine(TaskUsage(calls: 1, tokens: 50, costUsd: nil)), "1 call · 50 tok")
        XCTAssertNil(TaskUsageFormat.taskLine(TaskUsage(calls: 0, tokens: 0, costUsd: nil)))
    }

    func testTaskAccessibilityLabelSplitsPlanningAndRun() {
        let total = TaskUsageTotal(
            usage: TaskUsage(calls: 3, tokens: 1500, costUsd: 0.03),
            run: TaskUsage(calls: 2, tokens: 1000, costUsd: 0.02),
            planning: TaskUsage(calls: 1, tokens: 500, costUsd: 0.01))
        XCTAssertEqual(
            TaskUsageFormat.taskAccessibilityLabel(total),
            "3 model calls, 1500 tokens, $0.030. Planning: 1 call, $0.010. Run, including delegated work: 2 calls, $0.020")
    }

    func testPlanningSuffixSumsTokensAndPricedCalls() {
        let at = Date(timeIntervalSince1970: 0)
        let entries = [
            PlanningEntry(id: "a", at: at, message: "m", warning: false, stage: "llm", isModelCall: true, tokens: 9000, costUsd: 0.02),
            PlanningEntry(id: "b", at: at, message: "m", warning: false, stage: "outline", isModelCall: false),
            PlanningEntry(id: "c", at: at, message: "m", warning: false, stage: "llm", isModelCall: true, tokens: 27000, costUsd: 0.03)
        ]
        XCTAssertEqual(TaskUsageFormat.planningSuffix(entries), " · 36.0k tok · $0.050")
        XCTAssertEqual(TaskUsageFormat.planningSuffix([entries[1]]), "")
    }

    func testRunSummaryAppendsRunUsage() {
        XCTAssertEqual(TaskUsageFormat.runSummary("4 steps · 2m", run: TaskUsage(calls: 5, tokens: 9_100, costUsd: 0.03)),
                       "4 steps · 2m · 5 calls · 9.1k tok · $0.030")
        XCTAssertEqual(TaskUsageFormat.runSummary("4 steps", run: nil), "4 steps")
    }

    // MARK: Turn chip (web RequestActivityCard)

    private func llmSucceeded(_ trace: String, _ payload: [String: Any]) -> [String: Any] {
        var merged = payload
        merged["trace_id"] = trace
        return ["event_type": "AgentEvent", "data": ["event": ["event_type": "llm.succeeded", "payload": merged]]]
    }

    private func toolFinished(_ call: String, tool: String, _ payload: [String: Any]) -> [String: Any] {
        var merged = payload
        merged["call_id"] = call
        merged["tool_name"] = tool
        return ["event_type": "AgentEvent", "data": ["event": ["event_type": "tool.call.finished", "payload": merged]]]
    }

    func testTurnSumsItsOwnCallsAndPricedCost() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["input_tokens": 1000, "output_tokens": 200, "cost_usd": 0.01, "duration_ms": 900]))
        sut.ingest(llmSucceeded("t2", ["input_tokens": 500, "output_tokens": 100, "cost": 0.002, "latency_ms": 400]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.first?.durationMs, 900)
        XCTAssertEqual(rows.last?.durationMs, 400, "task-run calls report latency_ms")

        let summary = TurnUsage.summary(rows: rows) { _ in nil }
        XCTAssertEqual(summary?.calls, 2)
        XCTAssertEqual(summary?.tokens, 1800)
        XCTAssertEqual(summary?.costUsd ?? -1, 0.012, accuracy: 1e-12)
        XCTAssertEqual(summary.flatMap(TaskUsageFormat.turnLine), "2 calls · 1.8k tok · $0.012")
    }

    func testTurnWithoutUsageHasNoChip() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["duration_ms": 900]))
        XCTAssertNil(TurnUsage.summary(rows: sut.snapshot()) { _ in nil })
    }

    func testTokenAvailabilityFalseIsNotReported() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["input_tokens": 0, "output_tokens": 0, "usage_availability": ["tokens": false], "cost_usd": 0.01]))
        let summary = TurnUsage.summary(rows: sut.snapshot()) { _ in nil }
        XCTAssertEqual(summary?.calls, 1, "a priced call with no reported tokens still counts")
        XCTAssertEqual(summary?.tokens, 0)
    }

    func testOneCallDeliveredByTwoTransportsCountsOnce() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["llm_call_id": "call-1", "input_tokens": 100, "output_tokens": 20]))
        sut.ingest(["event_type": "LLMResponseReceived", "data": [
            "timestamp": 5, "capability": "reason", "llm_call_id": "call-1", "input_tokens": 100, "output_tokens": 20]])
        XCTAssertEqual(TurnUsage.summary(rows: sut.snapshot()) { _ in nil }?.calls, 1)
    }

    func testTurnIncludesTasksItStarted() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["input_tokens": 1000, "output_tokens": 0, "cost_usd": 0.01]))
        sut.ingest(toolFinished("c1", tool: "create_task", ["spawned_task_ids": ["task_a"]]))
        let legacyId = "task_" + String(repeating: "ab", count: 16)
        sut.ingest(toolFinished("c2", tool: "run_task", ["content_preview": "Started \(legacyId) ok"]))
        // Delegation is not a spawned task: its calls are already the turn's.
        sut.ingest(toolFinished("c3", tool: "delegate_to_agent", ["content_preview": "child \(legacyId)"]))
        let rows = sut.snapshot()
        XCTAssertEqual(TurnUsage.spawnedTaskIds(rows), ["task_a", legacyId])

        let usage: [String: TaskUsageTotal] = ["task_a": TaskUsageTotal(
            usage: TaskUsage(calls: 4, tokens: 4000, costUsd: 0.03), run: nil, planning: nil)]
        let summary = TurnUsage.summary(rows: rows) { usage[$0] }
        XCTAssertEqual(summary?.calls, 5)
        XCTAssertEqual(summary?.tokens, 5000)
        XCTAssertEqual(summary?.spawnedTasks, 2)
        XCTAssertEqual(summary.flatMap(TaskUsageFormat.turnLine), "5 calls · 5.0k tok · $0.040 · incl. 2 tasks")
        XCTAssertEqual(summary.flatMap(TaskUsageFormat.turnShortLine), "5.0k tok · $0.040")
    }

    func testStepRowCarriesItsCallUsage() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", ["input_tokens": 9000, "output_tokens": 100, "cost_usd": 0.0123, "duration_ms": 2100, "ttft_ms": 700]))
        let row = sut.snapshot().first
        XCTAssertEqual(row?.detail, "700ms (2.1s)")
        XCTAssertEqual(
            TaskUsageFormat.withUsage(row?.detail, suffix: TaskUsageFormat.stepSuffix(
                tokens: row?.callUsage?.stepTokens, costUsd: row?.callUsage?.costUsd)),
            "700ms (2.1s) · 9.1k tok · $0.012")
    }
}
