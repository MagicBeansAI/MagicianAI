import XCTest
@testable import Magician

/// The Plan Inspector's state rules: who may edit, the unsaved-changes
/// guard, the approval bar, save/restore round trips, Approve & Run's order,
/// and live run marks.
@MainActor
final class PlanInspectorModelTests: XCTestCase {
    private let base = "/api/magician/v3/tasks/task_1/plan"
    private var transport: StubPlanTransport!

    override func setUp() async throws {
        transport = StubPlanTransport()
    }

    private static let graphJSON: [String: Any] = [
        "steps": [
            ["id": "a", "task": "A", "tool": "t", "parameters": [:], "expected_outputs": [], "confidence": 0.9, "metadata": [:]],
            ["id": "b", "task": "B", "tool": NSNull(), "parameters": [:], "expected_outputs": [], "confidence": 0.3, "metadata": [:]]
        ],
        "edges": [["from": "a", "to": "b", "reason": "r"]],
        "unresolved_inputs": [], "confidence": 0.6,
        "provenance": ["strategy": "S", "generator": NSNull(), "notes": NSNull()]
    ]

    private func model(status: String = "draft", planId: String = "plan_1") -> PlanInspectorModel {
        transport.on("GET", base, ["plan": ["plan_id": planId, "status": status, "plan_graph": Self.graphJSON]])
        return PlanInspectorModel(route: PlanInspectorRoute(taskId: "task_1"),
                                  client: PlanInspectorClient(taskId: "task_1", transport: transport))
    }

    func testEditingIsOpenForDraftElicitingAndApprovedOnly() {
        for status in [PlanStatus.draft, .eliciting, .approved] {
            XCTAssertTrue(PlanInspectorModel.canEdit(status: status, hasGraph: true), status.rawValue)
        }
        for status in [PlanStatus.planning, .rejected, .failed] {
            XCTAssertFalse(PlanInspectorModel.canEdit(status: status, hasGraph: true), status.rawValue)
        }
        XCTAssertFalse(PlanInspectorModel.canEdit(status: .draft, hasGraph: false))
        XCTAssertFalse(PlanInspectorModel.canEdit(status: nil, hasGraph: true))
    }

    func testApprovalBarOnlyForADraftWithAPlanIdOutsideEditMode() {
        XCTAssertTrue(PlanInspectorModel.showsApprovalBar(status: .draft, planId: "p", editing: false))
        XCTAssertFalse(PlanInspectorModel.showsApprovalBar(status: .draft, planId: "p", editing: true))
        XCTAssertFalse(PlanInspectorModel.showsApprovalBar(status: .draft, planId: nil, editing: false))
        XCTAssertFalse(PlanInspectorModel.showsApprovalBar(status: .approved, planId: "p", editing: false))
    }

    func testUnsavedGuardTracksRealChangesOnly() async {
        let model = model()
        await model.load()
        XCTAssertTrue(model.canEdit)
        model.beginEditing()
        XCTAssertTrue(model.isEditing)
        XCTAssertFalse(model.isDirty, "entering edit mode is not a change")
        model.addStep()
        XCTAssertTrue(model.isDirty)
        XCTAssertEqual(model.shownGraph?.steps.count, 3)
        XCTAssertEqual(model.graph?.steps.count, 2, "the saved plan is untouched")
        model.discardEdits()
        XCTAssertFalse(model.isEditing)
        XCTAssertFalse(model.isDirty)
        XCTAssertEqual(model.shownGraph?.steps.count, 2)
    }

    func testARejectedPlanCannotEnterEditMode() async {
        let model = model(status: "rejected")
        await model.load()
        model.beginEditing()
        XCTAssertFalse(model.isEditing)
    }

    func testSavePutsTheDraftLeavesEditModeAndReloads() async {
        let model = model()
        await model.load()
        model.beginEditing()
        XCTAssertTrue(model.setDependency("b", on: "a", false))
        transport.on("PUT", base, ["plan": ["plan_id": "plan_1", "status": "draft", "plan_graph": Self.graphJSON]])
        transport.on("GET", "\(base)/versions", ["versions": [], "has_more": false])
        let saved = await model.save(label: "trim b")
        XCTAssertTrue(saved)
        XCTAssertFalse(model.isEditing)
        let put = transport.requests.first { $0.method == "PUT" }
        XCTAssertEqual(put?.body?["label"], .string("trim b"))
        XCTAssertEqual(put?.body?["plan"]?["edges"], .array([]))
        XCTAssertEqual(model.notice, "Plan saved.")
    }

    func testAFailedSaveKeepsTheDraft() async {
        let model = model()
        await model.load()
        model.beginEditing()
        model.addStep()
        transport.on("PUT", base, status: 409, ["error": "plan_version_conflict"])
        let saved = await model.save(label: nil)
        XCTAssertFalse(saved)
        XCTAssertTrue(model.isEditing)
        XCTAssertTrue(model.isDirty)
        XCTAssertEqual(model.errorMessage, "plan_version_conflict")
    }

    func testRestoreRefetchesAndShowsSteps() async {
        let model = model()
        await model.load()
        transport.on("GET", "\(base)/versions/7", ["version": ["plan_graph": Self.graphJSON]])
        transport.on("POST", "\(base)/versions/7/restore", ["plan": ["plan_id": "plan_1", "status": "draft"]])
        transport.on("GET", "\(base)/versions", ["versions": [], "has_more": false])
        model.tab = .history
        await model.previewVersion(PlanVersionSummary(epochMs: 7, label: nil, stepCount: 2, confidence: nil))
        XCTAssertEqual(model.previewGraph.value?.steps.count, 2)
        let restored = await model.restorePreview()
        XCTAssertTrue(restored)
        XCTAssertEqual(model.tab, .steps)
        XCTAssertNil(model.preview)
        XCTAssertEqual(transport.calls.filter { $0 == "GET \(base)" }.count, 2)
    }

    func testApproveAndRunApprovesThenStartsTheRun() async {
        let model = model()
        await model.load()
        transport.on("POST", "\(base)/approve", ["ok": true])
        transport.on("POST", "/api/magician/v3/tasks/task_1/execute", status: 202, ["execution_id": "e"])
        transport.on("GET", "/api/magician/v3/tasks/task_1/execution-panel", ["debug": [:]])
        await model.decide(.approveAndRun)
        let posts = transport.requests.filter { $0.method == "POST" }
        XCTAssertEqual(posts.map(\.path), ["\(base)/approve", "/api/magician/v3/tasks/task_1/execute"])
        XCTAssertEqual(posts.first?.query, [URLQueryItem(name: "plan_id", value: "plan_1")])
        XCTAssertEqual(model.notice, PlanApprovalOutcome.approvedAndStarted.message)
        model.stopLive()
    }

    func testAFailedStartKeepsTheApprovalAndSaysSo() async {
        let model = model()
        await model.load()
        transport.on("POST", "\(base)/approve", ["ok": true])
        transport.on("POST", "/api/magician/v3/tasks/task_1/execute", status: 409, ["message": "Already running"])
        await model.decide(.approveAndRun)
        XCTAssertEqual(model.notice, PlanApprovalOutcome.approvedRunNotStarted("Already running").message)
    }

    func testAFailedApprovalDoesNotStartARun() async {
        let model = model()
        await model.load()
        transport.on("POST", "\(base)/approve", status: 409, ["error": "plan_version_conflict"])
        await model.decide(.approveAndRun)
        XCTAssertFalse(transport.calls.contains { $0.hasSuffix("/execute") })
        XCTAssertEqual(model.notice, "plan_version_conflict")
    }

    func testRunMarksMapByStepIdThenByPosition() async {
        let model = model(status: "approved")
        await model.load()
        model.apply(panel: ["debug": ["selected_execution": [
            "status": "running",
            "step_statuses": [
                ["number": 1, "name": "A", "status": "completed", "step_id": "a"],
                ["number": 2, "name": "B", "status": "in_progress", "step_id": "unknown-id"]
            ]
        ]]])
        XCTAssertEqual(model.runMarks, ["a": .done, "b": .inProgress])
        XCTAssertTrue(model.runActive)

        model.apply(panel: ["debug": ["selected_execution": [
            "status": "completed", "step_statuses": [["number": 1, "status": "completed", "step_id": "a"],
                                                     ["number": 2, "status": "pending", "step_id": "b"]]
        ]]])
        XCTAssertFalse(model.runActive)
        XCTAssertEqual(model.runMarks["b"], .notReached)
    }

    func testAnsweringAQuestionClearsItsTextAndReloads() async {
        transport.on("GET", base, ["plan": ["plan_id": "plan_1", "status": "eliciting", "plan_graph": Self.graphJSON,
                                            "pending_questions": [["id": "q1", "question_text": "Who?"]]]])
        let model = PlanInspectorModel(route: PlanInspectorRoute(taskId: "task_1"),
                                       client: PlanInspectorClient(taskId: "task_1", transport: transport))
        await model.load()
        XCTAssertTrue(model.canResume)
        transport.on("POST", "/api/magician/v2/hitl/q1/respond", ["accepted": true])
        transport.on("GET", "\(base)/clarifications", ["clarification_history": []])
        model.answers["q1"] = "  Jordan "
        await model.answer(model.pendingQuestions[0])
        let post = transport.requests.first { $0.method == "POST" }
        XCTAssertEqual(post?.body?["value"]?["value"], .string("Jordan"))
        XCTAssertNil(model.answers["q1"])
        XCTAssertTrue(model.answering.isEmpty)
        XCTAssertEqual(model.clarifications, .loaded([]))
    }
}
