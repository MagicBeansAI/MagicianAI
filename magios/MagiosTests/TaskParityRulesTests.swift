import XCTest
@testable import Magician

/// Web Tasks parity rules: has-result, recurrence, cron description, per-status
/// card actions, output scopes, delegation envelopes and history spans.
final class TaskParityRulesTests: XCTestCase {
    private func task(_ fields: [String: Any]) throws -> TaskV3 {
        var dict: [String: Any] = ["id": "t", "title": "T", "status": "pending"]
        fields.forEach { dict[$0.key] = $0.value }
        return try JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    // MARK: Decode

    func testDecodesCompletionOutcomeAndArtifactNamesLeniently() throws {
        let full = try task([
            "status": "completed", "completion_outcome": "success",
            "completion_artifact_names": ["report.md", " ", "notes.md"],
            "recurring_schedule": ["interval_seconds": 600]
        ])
        XCTAssertEqual(full.completionOutcome, "success")
        XCTAssertEqual(full.completionArtifactNames, ["report.md", "notes.md"])
        XCTAssertNotNil(full.recurringSchedule)

        let bare = try task([:])
        XCTAssertNil(bare.completionOutcome)
        XCTAssertEqual(bare.completionArtifactNames, [])
        XCTAssertNil(bare.recurringSchedule)

        let nulls = try task(["completion_outcome": NSNull(), "completion_artifact_names": NSNull()])
        XCTAssertNil(nulls.completionOutcome)
        XCTAssertEqual(nulls.completionArtifactNames, [])
    }

    // MARK: Has-result

    func testHasResultRule() {
        func has(_ status: String, summary: String? = nil, outcome: String? = nil, names: [String] = []) -> Bool {
            TaskResultRule.hasResult(status: status, completionSummary: summary,
                                     completionOutcome: outcome, artifactNames: names)
        }
        XCTAssertTrue(has("failed", names: ["report.md"]), "artifact names always count")
        XCTAssertTrue(has("running", summary: "Wrote the brief"), "a summary always counts")
        XCTAssertFalse(has("completed", summary: "   "), "blank summary is no result")
        XCTAssertTrue(has("completed", outcome: "success"))
        XCTAssertTrue(has("Completed", outcome: " Partial success "))
        for word in ["failed", "Cancelled", "canceled", "STOPPED", "tool_error"] {
            XCTAssertFalse(has("completed", outcome: word), "\(word) is not a result")
        }
        XCTAssertFalse(has("failed", outcome: "success"), "outcome only counts on completed tasks")
        XCTAssertFalse(has("completed", outcome: ""))
        XCTAssertFalse(has("completed"))
    }

    // MARK: Recurrence

    func testRecurringDetection() throws {
        XCTAssertTrue(try task([
            "schedule": ["kind": ["Cron": ["expression": "0 9 * * *"]]]
        ]).isRecurring)
        XCTAssertTrue(try task(["tags": [["id": "x", "name": "Recurring"]]]).isRecurring)
        XCTAssertTrue(try task(["tags": [["id": "app_recurring", "name": "Daily sweep"]]]).isRecurring)
        XCTAssertTrue(try task(["tags": [["id": "y", "name": "APP_RECURRING"]]]).isRecurring)
        XCTAssertTrue(try task(["recurring_schedule": ["interval_seconds": 300]]).isRecurring)
        XCTAssertFalse(try task(["recurring_schedule": NSNull()]).isRecurring)
        XCTAssertFalse(try task(["tags": [["id": "z", "name": "recurring-ish"]]]).isRecurring)
        XCTAssertFalse(try task([:]).isRecurring)

        XCTAssertEqual(try task([
            "schedule": ["kind": ["Cron": ["expression": "0 9 * * 1-5"]]]
        ]).recurrenceDescription, "Weekdays at 09:00")
        XCTAssertEqual(try task(["tags": [["id": "r", "name": "recurring"]]]).recurrenceDescription,
                       "Recurring task")
    }

    func testCronDescription() {
        let cases: [(String, String)] = [
            ("*/15 * * * *", "Every 15 minutes"),
            ("* * * * *", "Every minute"),
            ("*/1 * * * *", "Every minute"),
            ("0 * * * *", "Every hour"),
            ("30 * * * *", "Every hour at :30"),
            ("0 */2 * * *", "Every 2 hours"),
            ("15 */6 * * *", "Every 6 hours at :15"),
            ("0 9 * * *", "Daily at 09:00"),
            ("5 18 * * *", "Daily at 18:05"),
            ("0 9 * * 1-5", "Weekdays at 09:00"),
            ("0 9 * * MON-FRI", "Weekdays at 09:00"),
            ("30 8 * * 1", "Weekly on Monday at 08:30"),
            ("0 10 * * 0", "Weekly on Sunday at 10:00"),
            ("0 10 * * 7", "Weekly on Sunday at 10:00"),
            ("0 10 * * 1,3,5", "Weekly on Monday, Wednesday, Friday at 10:00"),
            ("0 7 * * sat", "Weekly on Saturday at 07:00"),
            ("0 6 1 * *", "Monthly on day 1 at 06:00"),
            ("0 6 15 * *", "Monthly on day 15 at 06:00"),
            // Unmodelled shapes fall back to the raw expression, trimmed.
            ("  0 6 1 1 *  ", "0 6 1 1 *"),
            ("0 9-17 * * *", "0 9-17 * * *"),
            ("0 0 9 * * *", "0 0 9 * * *"),
            ("@daily", "@daily"),
            ("", "")
        ]
        for (cron, expected) in cases {
            XCTAssertEqual(TaskCronDescription.describe(cron), expected, cron)
        }
    }

    // MARK: Card actions

    func testFailedCardActionsNeedAnExecutionForReset() throws {
        let ran = try task(["status": "failed", "latest_root_execution_id": "exec-1"])
        XCTAssertEqual(ran.visibleCardActions, [.reset, .publishToNotes])
        XCTAssertEqual(ran.primaryCardAction, .reset)

        let neverRan = try task(["status": "failed"])
        XCTAssertEqual(neverRan.visibleCardActions, [.publishToNotes])
        XCTAssertEqual(neverRan.primaryCardAction, .publishToNotes)

        let cancelled = try task(["status": "cancelled", "active_root_execution_id": "exec-2"])
        XCTAssertEqual(cancelled.visibleCardActions, [.reset, .publishToNotes])

        // `canceled` is resettable but the Notes boundary does not admit it.
        let canceled = try task(["status": "canceled", "latest_root_execution_id": "exec-3"])
        XCTAssertEqual(canceled.visibleCardActions, [.reset])
        XCTAssertEqual(try task(["status": "canceled"]).visibleCardActions, [])
    }

    func testCompletedCardActionsLeadWithResultWhenThereIsOne() throws {
        let withResult = try task(["status": "completed", "completion_summary": "Brief ready"])
        XCTAssertEqual(withResult.visibleCardActions, [.viewResult, .publishToNotes])
        XCTAssertEqual(withResult.primaryCardAction, .viewResult)

        let withoutResult = try task(["status": "completed", "completion_outcome": "failed"])
        XCTAssertEqual(withoutResult.visibleCardActions, [.publishToNotes])
        XCTAssertEqual(withoutResult.primaryCardAction, .publishToNotes)
    }

    func testResultRendersOnceAndStaysSecondaryOffCompletedCards() throws {
        let failedWithSummary = try task([
            "status": "failed", "latest_root_execution_id": "exec-1",
            "completion_summary": "Partial draft saved"
        ])
        XCTAssertEqual(failedWithSummary.visibleCardActions, [.reset, .viewResult, .publishToNotes])
        XCTAssertEqual(failedWithSummary.primaryCardAction, .reset)

        let runningWithEarlierResult = try task([
            "status": "running", "active_root_execution_id": "exec-live",
            "completion_artifact_names": ["previous.md"]
        ])
        XCTAssertEqual(runningWithEarlierResult.visibleCardActions, [.viewResult])
        XCTAssertNil(runningWithEarlierResult.primaryCardAction)

        for fields in [
            ["status": "completed", "completion_summary": "x"],
            ["status": "failed", "completion_artifact_names": ["a"]]
        ] as [[String: Any]] {
            let actions = try task(fields).visibleCardActions
            XCTAssertEqual(actions.filter { $0 == .viewResult }.count, 1)
        }
    }

    func testPausedCardDropsViewExecutionWhenInlineControlsShow() throws {
        let withControls = try task(["status": "paused", "active_root_execution_id": "exec-p"])
        XCTAssertTrue(withControls.showsInlineExecutionControls)
        XCTAssertEqual(withControls.visibleCardActions, [.reset])

        let orphaned = try task(["status": "paused"])
        XCTAssertEqual(orphaned.visibleCardActions, [.viewExecution, .reset])

        let asking = try task([
            "status": "paused", "active_root_execution_id": "exec-q",
            "pending_question": ["question": "Which?"]
        ])
        XCTAssertEqual(asking.visibleCardActions, [.viewQuestion, .reset])
    }

    // MARK: Detail entry + header

    private func snapshot(
        status: String = "completed",
        panel: [String: Any]? = nil,
        outputs: [String: Any]? = ["outputs": ["outputs": []]],
        seed: TaskDetailSeed? = nil
    ) -> TaskDetailSnapshot {
        let resolvedSeed = seed ?? TaskDetailSeed(TaskStatusModel(
            taskId: "task-1", title: "T", status: status, steps: []
        ))
        return TaskDetailSnapshot.parse(
            seed: resolvedSeed, taskPayload: nil, panelPayload: panel,
            outputsPayload: outputs, detailsPayload: nil, planPayload: nil
        )
    }

    func testResultFocusOpensOutputTabExplicitly() {
        let running = snapshot(status: "running", panel: [
            "overview": ["status": "running", "execution_id": "exec-1"],
            "debug": ["selected_execution": ["execution_id": "exec-1", "status": "running"]]
        ])
        XCTAssertEqual(running.defaultOpenTab(now: Date()), .run)
        XCTAssertEqual(running.openTab(focus: .result, now: Date()), .output,
                       "an explicit focus wins over the verdict default")
        XCTAssertEqual(running.openTab(focus: nil, now: Date()), .run)

        let noOutput = snapshot(status: "running", panel: [
            "overview": ["status": "running", "execution_id": "exec-1"]
        ], outputs: nil)
        XCTAssertFalse(noOutput.visibleTabs.contains(.output))
        XCTAssertEqual(noOutput.openTab(focus: .result, now: Date()),
                       noOutput.defaultOpenTab(now: Date()),
                       "a focus on a tab the task lacks falls back to the default")
    }

    func testHeaderOwnsResetSoTheMenuDoesNotDuplicateIt() {
        XCTAssertTrue(snapshot(status: "failed").headerOffersReset)
        XCTAssertTrue(snapshot(status: "canceled").headerOffersReset)
        XCTAssertFalse(snapshot(status: "completed").headerOffersReset)

        // Paused with a live execution: the header shows run controls, so
        // Reset belongs to the menu.
        let pausedLive = snapshot(status: "paused", panel: [
            "overview": ["status": "paused", "execution_id": "exec-p"]
        ], seed: TaskDetailSeed(TaskStatusModel(
            taskId: "task-1", title: "T", status: "paused", steps: [],
            activeRootExecutionId: "exec-p"
        )))
        XCTAssertNotNil(pausedLive.activeRootExecutionId)
        XCTAssertFalse(pausedLive.headerOffersReset)
    }

    func testSeedCarriesRecurrenceIntoDetailHeader() throws {
        let source = try task([
            "status": "ready", "schedule": ["kind": ["Cron": ["expression": "0 9 * * *"]]]
        ])
        let detail = snapshot(status: "ready", seed: TaskDetailSeed(source))
        XCTAssertEqual(detail.recurrenceDescription, "Daily at 09:00")
        XCTAssertNil(snapshot().recurrenceDescription)
    }

    // MARK: Output scopes

    func testOutputGroupingSeparatesDeliverablesFromRunEvidence() {
        let detail = snapshot(panel: [
            "overview": ["status": "completed", "execution_id": "exec-1"],
            "output": [
                "selected_execution_outputs": [["relative_path": "outputs/draft.md", "scope": "execution"]],
                "selected_child_outputs": [["relative_path": "outputs/child/notes.md"]],
                "selected_execution_artifacts": [
                    ["artifact_id": "a1", "artifact_type": "screenshot", "content_type": "image/png",
                     "produced_at": "2026-07-15T07:00:00Z", "relative_path": "evidence/shot.png"],
                    ["artifact_id": "a2", "artifact_type": "tool_result", "content_type": "application/json",
                     "produced_at": "2026-07-15T07:01:00Z", "display_name": "Search result"]
                ]
            ]
        ], outputs: ["outputs": ["outputs": [
            ["relative_path": "outputs/report.md", "media_type": "text/markdown"],
            ["relative_path": "outputs/draft.md"]
        ]]])

        let groups = detail.outputGroups
        XCTAssertEqual(groups.deliverables.map(\.relativePath), ["outputs/report.md", "outputs/draft.md"])
        XCTAssertEqual(groups.intermediates.map(\.scope), [.execution, .delegated, .artifact])
        XCTAssertEqual(groups.intermediates[0].files.map(\.relativePath), ["outputs/draft.md"],
                       "a promoted deliverable and its run file are two rows, as on the web")
        XCTAssertEqual(groups.intermediates[1].files.map(\.relativePath), ["outputs/child/notes.md"])
        XCTAssertEqual(groups.intermediates[2].files.map(\.relativePath), ["evidence/shot.png"])
        XCTAssertEqual(groups.intermediates[2].structured.map(\.name), ["Search result"])
        XCTAssertEqual(groups.intermediateCount, 4)
        XCTAssertEqual(TaskOutputScope.delegated.title, "Delegated outputs")
        XCTAssertEqual(TaskOutputScope.artifact.title, "Persisted artifacts")
        XCTAssertEqual(TaskOutputScope(wire: " Execution "), .execution)
        XCTAssertNil(TaskOutputScope(wire: "unknown"))
    }

    func testOlderPanelWithoutRunArraysAddsNoIntermediates() {
        let detail = snapshot(panel: [
            "overview": ["status": "completed", "execution_id": "exec-1"],
            "output": ["selected_execution_outputs": [["relative_path": "a.md"]]]
        ], outputs: ["outputs": ["outputs": [["relative_path": "report.md"]]]])
        XCTAssertEqual(detail.outputGroups.deliverables.count, 1)
        XCTAssertTrue(detail.outputGroups.intermediates.isEmpty)
    }

    // MARK: Delegations

    private func activity(_ id: String, execution: String?, at seconds: TimeInterval?) -> TaskDetailActivity {
        TaskDetailActivity(
            id: id, title: id, body: nil, status: "done", kind: "event", agentId: nil,
            timestamp: seconds.map { Date(timeIntervalSince1970: $0) }, eventType: nil,
            latencyMs: nil, model: nil, costUsd: nil, inputTokens: nil, outputTokens: nil,
            cacheReadTokens: nil, executionId: execution
        )
    }

    private func delegation(_ id: String, agent: String, status: String,
                            started: TimeInterval? = nil, completed: TimeInterval? = nil) -> TaskDelegationGroup {
        TaskDelegationGroup(
            executionId: id, agentId: agent, status: status, entryCount: 0, parentExecutionId: "root",
            startedAt: started.map { Date(timeIntervalSince1970: $0) },
            completedAt: completed.map { Date(timeIntervalSince1970: $0) }
        )
    }

    func testDelegationGroupingPlacesOneBlockPerChildAtItsFirstRow() {
        let rows = [
            activity("p1", execution: "root", at: 0),
            activity("c1", execution: "child-a", at: 10),
            activity("p2", execution: "root", at: 20),
            activity("c2", execution: "child-a", at: 30),
            activity("x1", execution: "child-unknown", at: 40),
            activity("n1", execution: nil, at: 50)
        ]
        let groups = [delegation("child-a", agent: "writer", status: "completed")]
        let segments = TaskDelegationTimeline.group(rows, delegations: groups)
        XCTAssertEqual(segments.map(\.id), ["p1", "delegation:child-a", "p2", "x1", "n1"])
        guard case .delegation(let group, let entries) = segments[1] else {
            return XCTFail("expected a delegation block")
        }
        XCTAssertEqual(group.agentId, "writer")
        XCTAssertEqual(entries.map(\.id), ["c1", "c2"])

        XCTAssertEqual(TaskDelegationTimeline.group(rows, delegations: []).map(\.id),
                       rows.map(\.id), "no delegations → plain rows")
        XCTAssertEqual(TaskDelegationTimeline.delegatedAgent(for: rows[1], delegations: groups), "writer")
        XCTAssertNil(TaskDelegationTimeline.delegatedAgent(for: rows[0], delegations: groups))
    }

    func testDelegationSpanSummary() {
        let clock: (Date?) -> String? = { date in
            date.map { "t\(Int($0.timeIntervalSince1970))" }
        }
        let done = TaskDelegationTimeline.span(
            entries: [activity("a", execution: "c", at: 0), activity("b", execution: "c", at: 300)],
            group: delegation("c", agent: "w", status: "completed"),
            clock: clock
        )
        XCTAssertEqual(done.summary, "t0 – t300 (5m)")
        XCTAssertEqual(done.duration, "5m")

        let recorded = TaskDelegationTimeline.span(
            entries: [activity("a", execution: "c", at: 100)],
            group: delegation("c", agent: "w", status: "failed", started: 60, completed: 90),
            clock: clock
        )
        XCTAssertEqual(recorded.summary, "t60 – t90 (30s)", "recorded bounds win over row times")

        let running = TaskDelegationTimeline.span(
            entries: [activity("a", execution: "c", at: 0), activity("b", execution: "c", at: 120)],
            group: delegation("c", agent: "w", status: "running"),
            clock: clock
        )
        XCTAssertEqual(running.summary, "started t0")
        XCTAssertNil(running.endClock)

        let untimed = TaskDelegationTimeline.span(
            entries: [activity("a", execution: "c", at: nil)],
            group: delegation("c", agent: "w", status: "completed"),
            clock: clock
        )
        XCTAssertNil(untimed.summary)
    }

    func testPanelParseCarriesDelegationsAndRowExecutionIds() {
        let detail = snapshot(status: "running", panel: [
            "overview": ["status": "running", "execution_id": "root"],
            "run": [
                "activity_log": [
                    ["id": "r1", "title": "Delegating", "status": "done", "created_at": 1_752_562_700_000,
                     "metadata": ["execution_id": "root"]],
                    ["id": "c1", "title": "Child work", "status": "done", "agent_id": "writer",
                     "created_at": 1_752_562_760_000, "metadata": ["execution_id": "child-1"]]
                ],
                "delegations": [[
                    "execution_id": "child-1", "agent_id": "writer", "status": "running",
                    "entry_count": 1, "parent_execution_id": "root",
                    "started_at": "2026-07-15T07:00:00Z"
                ]]
            ]
        ])
        XCTAssertEqual(detail.activity.map(\.executionId), ["root", "child-1"])
        XCTAssertEqual(detail.delegations.map(\.executionId), ["child-1"])
        XCTAssertEqual(detail.delegations.first?.status, "running")
        XCTAssertNotNil(detail.delegations.first?.startedAt)
        XCTAssertEqual(
            TaskDelegationTimeline.group(detail.activity, delegations: detail.delegations).map(\.id),
            ["r1", "delegation:child-1"]
        )
    }

    func testTimelineModeDefaultsToGrouped() {
        XCTAssertEqual(TaskTimelineMode(rawValue: "grouped"), .grouped)
        XCTAssertEqual(TaskTimelineMode(rawValue: "bogus") ?? .grouped, .grouped)
        XCTAssertEqual(TaskTimelineMode.allCases.map(\.title), ["Grouped", "Chronological"])
    }

    // MARK: History

    func testHistoryRunSpan() {
        let clock: (Date) -> String = { "t\(Int($0.timeIntervalSince1970))" }
        XCTAssertEqual(TaskHistoryFormatting.runSpan(
            startedAt: Date(timeIntervalSince1970: 0), endedAt: Date(timeIntervalSince1970: 300), clock: clock
        ), "t0 → t300 · 5m")
        XCTAssertEqual(TaskHistoryFormatting.runSpan(
            startedAt: Date(timeIntervalSince1970: 0), endedAt: nil, clock: clock
        ), "started t0")
        XCTAssertNil(TaskHistoryFormatting.runSpan(startedAt: nil, endedAt: nil, clock: clock))
    }
}
