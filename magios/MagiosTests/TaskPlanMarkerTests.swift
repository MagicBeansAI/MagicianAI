import XCTest
@testable import Magician

/// The plan-glyph rule: which tasks count as planned on every surface.
final class TaskPlanMarkerTests: XCTestCase {
    // MARK: - Rule

    func testNoPlanSignalIsNotPlanned() {
        XCTAssertFalse(TaskPlanMarker.hasPlan())
        XCTAssertFalse(TaskPlanMarker.hasPlan(planStatus: "  ", latestPlanId: "", approvedPlanId: nil,
                                              statuses: ["running", nil, "ready", "completed"]))
    }

    func testHasPlanFlag() {
        XCTAssertTrue(TaskPlanMarker.hasPlan(hasPlanFlag: true))
    }

    func testPlanStatus() {
        XCTAssertTrue(TaskPlanMarker.hasPlan(planStatus: "approved"))
    }

    func testLatestPlanId() {
        XCTAssertTrue(TaskPlanMarker.hasPlan(latestPlanId: "plan-1"))
    }

    func testApprovedPlanId() {
        XCTAssertTrue(TaskPlanMarker.hasPlan(approvedPlanId: "plan-1"))
    }

    func testPlanSteps() {
        XCTAssertTrue(TaskPlanMarker.hasPlan(planStepCount: 2))
    }

    func testPlanningStatuses() {
        for status in ["planning", "draft", "eliciting", " Draft "] {
            XCTAssertTrue(TaskPlanMarker.hasPlan(statuses: [nil, status]), status)
        }
    }

    // MARK: - Tasks list row

    private func task(_ extra: [String: Any]) throws -> TaskV3 {
        var dict: [String: Any] = ["id": "t1", "title": "T", "status": "ready"]
        dict.merge(extra) { $1 }
        return try JSONDecoder().decode(TaskV3.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testListRowFields() throws {
        XCTAssertFalse(TaskPlanMarker.hasPlan(try task([:])))
        XCTAssertTrue(TaskPlanMarker.hasPlan(try task(["has_plan": true])))
        XCTAssertTrue(TaskPlanMarker.hasPlan(try task(["plan_status": "draft"])))
        XCTAssertTrue(TaskPlanMarker.hasPlan(try task(["latest_plan_id": "p1"])))
        let approved = try task(["approved_plan_id": "p2"])
        XCTAssertEqual(approved.approvedPlanId, "p2")
        XCTAssertTrue(TaskPlanMarker.hasPlan(approved))
        XCTAssertTrue(TaskPlanMarker.hasPlan(try task(["status": "planning"])))
    }

    // MARK: - Chat live card

    func testLiveCard() {
        XCTAssertFalse(TaskPlanMarker.hasPlan(messageStatus: "running", storeStatus: "running", plan: nil))
        XCTAssertTrue(TaskPlanMarker.hasPlan(messageStatus: "draft", storeStatus: nil, plan: nil))
        XCTAssertTrue(TaskPlanMarker.hasPlan(messageStatus: "ready", storeStatus: "eliciting", plan: nil))
        XCTAssertTrue(TaskPlanMarker.hasPlan(
            messageStatus: "ready", storeStatus: "ready",
            plan: LiveTaskPlan(planId: nil, status: nil, steps: ["Search", "Write"])))
        XCTAssertTrue(TaskPlanMarker.hasPlan(
            messageStatus: "ready", storeStatus: "ready",
            plan: LiveTaskPlan(planId: "plan-1", status: nil, steps: [])))
    }

    // MARK: - Task detail

    private func snapshot(status: String, overview: [String: Any] = [:]) -> TaskDetailSnapshot {
        let seed = TaskDetailSeed(TaskStatusModel(taskId: "task-1", title: "T", status: status, steps: []))
        var fullOverview = overview
        fullOverview["status"] = status
        return TaskDetailSnapshot.parse(
            seed: seed, taskPayload: nil, panelPayload: ["overview": fullOverview],
            outputsPayload: nil, detailsPayload: nil, planPayload: nil)
    }

    func testTaskDetailSnapshot() {
        XCTAssertFalse(TaskPlanMarker.hasPlan(snapshot(status: "running")))
        XCTAssertTrue(TaskPlanMarker.hasPlan(snapshot(status: "running", overview: ["has_plan": true])))
        XCTAssertTrue(TaskPlanMarker.hasPlan(snapshot(status: "planning")))
    }
}
