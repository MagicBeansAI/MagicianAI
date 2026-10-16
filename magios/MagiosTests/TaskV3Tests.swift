import XCTest
@testable import Magician

extension XCTestCase {
    /// Spin the runloop (draining async URL + main-queue completions) until the
    /// condition holds or we time out.
    func waitUntil(timeout: TimeInterval = 2,
                   file: StaticString = #filePath,
                   line: UInt = #line,
                   _ condition: @escaping () -> Bool) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.02))
        }
        XCTAssertTrue(condition(), "waitUntil condition not met within \(timeout)s", file: file, line: line)
    }
}

/// Decoding + display-helper coverage for the flat `TaskListItemV3` wire model.
final class TaskV3Tests: XCTestCase {
    private func decode(_ dict: [String: Any]) throws -> TaskV3 {
        try JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testExecutionControlTargetUsesOnlyActiveRootOnActiveTask() throws {
        let active = try decode([
            "id": "active", "title": "Active", "status": "running",
            "active_root_execution_id": "  exec-active  ",
            "latest_root_execution_id": "exec-history"
        ])
        let historicalOnly = try decode([
            "id": "history", "title": "History", "status": "running",
            "latest_root_execution_id": "exec-history"
        ])
        let terminal = try decode([
            "id": "terminal", "title": "Terminal", "status": "completed",
            "active_root_execution_id": "exec-stale",
            "latest_root_execution_id": "exec-history"
        ])

        XCTAssertEqual(active.activeExecutionIdForControls, "exec-active")
        XCTAssertNil(historicalOnly.activeExecutionIdForControls)
        XCTAssertNil(terminal.activeExecutionIdForControls)
    }

    func testManualCompletionIsBlockedForEveryActiveExecutionLifecycle() throws {
        for status in ["queued", "running", "planning", "paused", "executing"] {
            XCTAssertFalse(try decode([
                "id": status, "title": status, "status": status
            ]).canMarkCompleteManually)
        }
        XCTAssertTrue(try decode([
            "id": "pending", "title": "Pending", "status": "pending"
        ]).canMarkCompleteManually)
        XCTAssertTrue(try decode([
            "id": "failed", "title": "Failed", "status": "failed"
        ]).canMarkCompleteManually)
    }

    func testNotesPublicationEligibilityMatchesBackendTerminalBoundary() throws {
        for status in ["completed", "failed", "cancelled"] {
            XCTAssertTrue(try decode([
                "id": status, "title": status, "status": status
            ]).canPublishToNotes, "\(status) should be publishable")
        }
        for status in ["pending", "ready", "planning", "running", "paused", "executing", "queued", "canceled", " COMPLETED "] {
            XCTAssertFalse(try decode([
                "id": status, "title": status, "status": status
            ]).canPublishToNotes, "\(status) must not expose Publish to Notes")
        }
    }

    func testTaskCardSwipeActionsAreStateAwareAndKeepExecutionControlsOut() throws {
        let pending = try decode(["id": "pending", "title": "Pending", "status": "pending"])
        XCTAssertEqual(pending.leadingCardSwipeActions, [.markComplete])
        XCTAssertEqual(pending.trailingCardSwipeActions, [.cancel, .delete])

        for status in ["queued", "running", "planning", "executing"] {
            let task = try decode([
                "id": status, "title": status, "status": status,
                "active_root_execution_id": "exec-\(status)"
            ])
            XCTAssertTrue(task.leadingCardSwipeActions.isEmpty)
            XCTAssertEqual(task.trailingCardSwipeActions, [.cancel, .delete])
        }

        let paused = try decode([
            "id": "paused", "title": "Paused", "status": "paused",
            "active_root_execution_id": "exec-paused"
        ])
        let orphanedPause = try decode([
            "id": "orphaned", "title": "Paused", "status": "paused"
        ])
        XCTAssertTrue(paused.leadingCardSwipeActions.isEmpty)
        XCTAssertTrue(orphanedPause.leadingCardSwipeActions.isEmpty)

        for status in ["failed", "cancelled"] {
            let task = try decode(["id": status, "title": status, "status": status])
            XCTAssertTrue(task.leadingCardSwipeActions.isEmpty)
            XCTAssertEqual(task.trailingCardSwipeActions, [.delete])
        }

        let completed = try decode(["id": "completed", "title": "Done", "status": "completed"])
        XCTAssertEqual(completed.leadingCardSwipeActions, [.markNotDone])
        XCTAssertEqual(completed.trailingCardSwipeActions, [.delete])
    }

    func testVisibleCardActionsMatchWebStateMatrix() throws {
        XCTAssertEqual(try decode([
            "id": "planning", "title": "Planning", "status": "planning",
            "plan_status": "planning"
        ]).visibleCardActions, [.viewPlan])
        XCTAssertEqual(try decode([
            "id": "question", "title": "Question", "status": "paused",
            "plan_status": "eliciting", "pending_question": ["question": "Which?"]
        ]).visibleCardActions, [.answerQuestion])
        XCTAssertEqual(try decode([
            "id": "draft", "title": "Draft", "status": "ready", "plan_status": "draft"
        ]).visibleCardActions, [.reviewPlan])
        XCTAssertEqual(try decode([
            "id": "approved", "title": "Approved", "status": "ready", "plan_status": "approved"
        ]).visibleCardActions, [.runPlan])
        XCTAssertEqual(try decode([
            "id": "pending", "title": "Pending", "status": "pending"
        ]).visibleCardActions, [.preplan, .runNow])
        XCTAssertEqual(try decode([
            "id": "paused", "title": "Paused", "status": "paused"
        ]).visibleCardActions, [.viewExecution, .reset])
        // Reset needs an execution to reset; Publish to Notes rides along.
        XCTAssertEqual(try decode([
            "id": "failed", "title": "Failed", "status": "failed",
            "latest_root_execution_id": "exec-failed"
        ]).visibleCardActions, [.reset, .publishToNotes])
        XCTAssertEqual(try decode([
            "id": "failed-never-ran", "title": "Failed", "status": "failed"
        ]).visibleCardActions, [.publishToNotes])
        XCTAssertTrue(try decode([
            "id": "running", "title": "Running", "status": "running",
            "active_root_execution_id": "exec"
        ]).visibleCardActions.isEmpty)
    }

    func testDecodesCanonicalCronScheduleForMobileEditor() throws {
        let task = try decode([
            "id": "scheduled", "title": "Scheduled", "status": "ready",
            "schedule": [
                "kind": ["Cron": ["expression": "0 9 * * 1-5", "timezone": "Asia/Kolkata"]],
                "timezone": "Asia/Kolkata",
                "execution_history_retention": ["max_records": 8, "max_age_days": 30]
            ]
        ])

        XCTAssertEqual(task.scheduleCron, "0 9 * * 1-5")
        XCTAssertEqual(task.scheduleTimezone, "Asia/Kolkata")
        XCTAssertEqual(task.scheduleRetentionMaxRecords, 8)
        XCTAssertEqual(task.scheduleRetentionMaxDays, 30)
    }

    func testDecodesFullTask() throws {
        let t = try decode([
            "id": "task-1", "title": "Ship it", "description": "desc", "status": "running",
            "agent_id": "personal-assistant", "ui_thread_id": "vibedev",
            "priority": "p1", "due_date": "2026-07-13",
            "tags": [["id": "urgent", "name": "urgent", "color": "#ff0000"]],
            "depends_on": ["task-0"], "is_blocked": true,
            "current_step_title": "Step A", "current_substep_title": "Sub A",
            "completion_summary": "done", "has_plan": true,
            "plan_status": "approved", "latest_plan_id": "plan-9",
            "pending_question": ["question": "Which one?"],
            "active_root_execution_id": "exec-1", "latest_root_execution_id": "exec-2",
            "synthesis_pending": true,
            "created_at": "2026-07-13T00:00:00Z", "updated_at": "2026-07-13T01:00:00Z"
        ])
        XCTAssertEqual(t.id, "task-1")
        XCTAssertEqual(t.status, "running")
        XCTAssertEqual(t.uiThreadId, "vibedev")
        XCTAssertEqual(t.priority, "p1")
        XCTAssertEqual(t.dueDate, "2026-07-13")
        XCTAssertEqual(t.tags.first?.name, "urgent")
        XCTAssertEqual(t.tags.first?.color, "#ff0000")
        XCTAssertEqual(t.dependsOn, ["task-0"])
        XCTAssertTrue(t.isBlocked)
        XCTAssertEqual(t.currentSubstepTitle, "Sub A")
        XCTAssertEqual(t.latestPlanId, "plan-9")
        XCTAssertEqual(t.planStatus, "approved")
        XCTAssertEqual(t.activeRootExecutionId, "exec-1")
        XCTAssertEqual(t.latestRootExecutionId, "exec-2")
        XCTAssertTrue(t.synthesisPending)
        XCTAssertTrue(t.needsAnswer)
    }

    func testDecodesMinimalTaskWithDefaults() throws {
        let t = try decode(["id": "t2", "title": "Bare", "status": "pending"])
        XCTAssertEqual(t.description, "")
        XCTAssertEqual(t.uiThreadId, "general")
        XCTAssertTrue(t.tags.isEmpty)
        XCTAssertTrue(t.dependsOn.isEmpty)
        XCTAssertFalse(t.isBlocked)
        XCTAssertFalse(t.hasPlan)
        XCTAssertFalse(t.needsAnswer)
        XCTAssertNil(t.priority)
        XCTAssertNil(t.dueDate)
        XCTAssertNil(t.latestPlanId)
        XCTAssertNil(t.activeRootExecutionId)
    }

    func testStatusLabels() throws {
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "completed"]).statusLabel, "Completed")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "paused"]).statusLabel, "Paused")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "cancelled"]).statusLabel, "Cancelled")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "weird"]).statusLabel, "Weird")
    }

    func testActivityLine() throws {
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "running",
                                   "current_substep_title": "Doing X"]).activityLine, "Doing X")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "running",
                                   "current_step_title": "Step only"]).activityLine, "Step only")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "running"]).activityLine, "Working…")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "completed",
                                   "completion_summary": "All good"]).activityLine, "All good")
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "pending",
                                   "description": "the desc"]).activityLine, "the desc")
    }

    func testPriorityLabel() throws {
        XCTAssertEqual(try decode(["id": "a", "title": "t", "status": "pending", "priority": "p2"]).priorityLabel, "P2")
        XCTAssertNil(try decode(["id": "a", "title": "t", "status": "pending"]).priorityLabel)
    }

    func testNeedsAnswer() throws {
        XCTAssertTrue(try decode(["id": "a", "title": "t", "status": "paused",
                                  "pending_question": ["question": "?"]]).needsAnswer)
        XCTAssertFalse(try decode(["id": "a", "title": "t", "status": "paused"]).needsAnswer)
    }

    func testPlanAwaitsReview() throws {
        XCTAssertTrue(try decode(["id": "a", "title": "t", "status": "planning",
                                  "has_plan": true, "latest_plan_id": "p", "plan_status": "draft"]).planAwaitsReview)
        XCTAssertTrue(try decode(["id": "a", "title": "t", "status": "eliciting",
                                  "has_plan": true, "latest_plan_id": "p", "plan_status": "eliciting"]).planAwaitsReview)
        XCTAssertFalse(try decode(["id": "a", "title": "t", "status": "ready",
                                   "has_plan": true, "latest_plan_id": "p", "plan_status": "approved"]).planAwaitsReview)
        XCTAssertFalse(try decode(["id": "a", "title": "t", "status": "pending"]).planAwaitsReview)
    }

    func testListResponseDecode() throws {
        let data = jsonData(["tasks": [["id": "a", "title": "A", "status": "pending"],
                                       ["id": "b", "title": "B", "status": "running"]]])
        let list = try JSONDecoder().decode(TaskListResponse.self, from: data)
        XCTAssertEqual(list.tasks.map(\.id), ["a", "b"])
    }

    func testUnknownFieldsIgnored() throws {
        // Forward-compatible: extra backend fields don't break decoding.
        let t = try decode(["id": "a", "title": "t", "status": "pending",
                            "some_future_field": ["nested": 1], "sync_mode": "async"])
        XCTAssertEqual(t.id, "a")
    }
}
