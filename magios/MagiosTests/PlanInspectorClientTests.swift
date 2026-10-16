import XCTest
@testable import Magician

/// Records every request; answers from a script keyed by method + path.
final class StubPlanTransport: PlanInspectorTransport {
    var responses: [String: PlanInspectorResponse] = [:]
    var errors: [String: Error] = [:]
    private(set) var requests: [PlanInspectorRequest] = []

    func on(_ method: String, _ path: String, status: Int = 200, _ body: Any = [String: Any]()) {
        responses["\(method) \(path)"] = PlanInspectorResponse(status: status, body: PlanJSON(body))
    }

    func send(_ request: PlanInspectorRequest) async throws -> PlanInspectorResponse {
        requests.append(request)
        let key = "\(request.method) \(request.path)"
        if let error = errors[key] { throw error }
        return responses[key] ?? PlanInspectorResponse(status: 404, body: PlanJSON(["error": "unscripted \(key)"]))
    }

    var calls: [String] { requests.map { "\($0.method) \($0.path)" } }
}

/// Request building and response reading for the Plan Inspector endpoints
/// (web `ExecutionPlanInspector.svelte`, `taskStore.ts` approve/reject).
final class PlanInspectorClientTests: XCTestCase {
    private let base = "/api/magician/v3/tasks/task_1/plan"
    private var transport: StubPlanTransport!
    private var client: PlanInspectorClient!

    override func setUp() {
        transport = StubPlanTransport()
        client = PlanInspectorClient(taskId: "task_1", transport: transport)
    }

    private static let graphJSON: [String: Any] = [
        "steps": [["id": "a", "task": "A", "tool": NSNull(), "parameters": [:], "expected_outputs": [],
                   "confidence": 0.9, "metadata": [:]]],
        "edges": [], "unresolved_inputs": [], "confidence": 0.9,
        "provenance": ["strategy": "S", "generator": NSNull(), "notes": NSNull()]
    ]

    func testEncodesTheTaskIdAsAPathSegment() {
        let odd = PlanInspectorClient(taskId: "a b/c", transport: transport)
        XCTAssertEqual(odd.planPath, "/api/magician/v3/tasks/a%20b%2Fc/plan")
    }

    func testLoadPlanReadsIdStatusGraphAndPendingQuestions() async throws {
        transport.on("GET", base, ["plan": [
            "plan_id": "plan_9", "status": "draft", "plan_graph": Self.graphJSON,
            "pending_questions": [["id": "q1", "question_text": "Who?", "urgency": 0.5,
                                   "options": [["value": "me", "label": "Me"], ["label": "no value"]]]]
        ]])
        let record = try await client.loadPlan()
        XCTAssertEqual(record?.planId, "plan_9")
        XCTAssertEqual(record?.status, .draft)
        XCTAssertEqual(record?.graph?.steps.map(\.id), ["a"])
        XCTAssertEqual(record?.pendingQuestions.first?.options.map(\.value), ["me"])
        XCTAssertEqual(transport.calls, ["GET \(base)"])
    }

    func testAMissingPlanIsNilNotAnError() async throws {
        transport.on("GET", base, status: 404, ["error": "task_plan_not_found:task_1"])
        let record = try await client.loadPlan()
        XCTAssertNil(record)
    }

    func testServerErrorsCarryTheServersMessage() async {
        transport.on("GET", base, status: 500, ["message": "store offline"])
        do {
            _ = try await client.loadPlan()
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual(error.localizedDescription, "store offline")
        }
    }

    func testSavePutsThePlanAndABlankLabelAsNull() async throws {
        let graph = PlanGraph(json: PlanJSON(Self.graphJSON))!
        let request = client.savePlanRequest(graph, label: "  ")
        XCTAssertEqual(request.method, "PUT")
        XCTAssertEqual(request.path, base)
        XCTAssertEqual(request.body?["label"], .null)
        XCTAssertEqual(request.body?["plan"]?["steps"]?.arrayValue?.first?["id"], .string("a"))
        XCTAssertEqual(client.savePlanRequest(graph, label: " v2 ").body?["label"], .string("v2"))

        transport.on("PUT", base, ["plan": ["plan_id": "p", "status": "draft", "plan_graph": Self.graphJSON]])
        let saved = try await client.savePlan(graph, label: nil)
        XCTAssertEqual(saved.planId, "p")
    }

    func testVersionsAskForTwentyAndParse() async throws {
        transport.on("GET", "\(base)/versions", ["versions": [
            ["epoch_ms": 1_700_000_000_000, "label": NSNull(), "step_count": 3, "confidence": 0.5],
            ["epoch_ms": 1_600_000_000_000, "label": "Before", "step_count": 1]
        ], "has_more": true])
        let page = try await client.versions()
        XCTAssertEqual(transport.requests.first?.query, [URLQueryItem(name: "limit", value: "20")])
        XCTAssertEqual(page.versions.map(\.epochMs), [1_700_000_000_000, 1_600_000_000_000])
        XCTAssertEqual(page.versions.map(\.title), ["Saved version", "Before"])
        XCTAssertTrue(page.hasMore)
    }

    func testVersionPreviewAndRestorePaths() async throws {
        transport.on("GET", "\(base)/versions/42", ["version": ["epoch_ms": 42, "plan_graph": Self.graphJSON]])
        transport.on("POST", "\(base)/versions/42/restore", ["plan": ["plan_id": "p", "status": "draft"]])
        let preview = try await client.version(42)
        XCTAssertEqual(preview?.steps.count, 1)
        _ = try await client.restore(42)
        XCTAssertEqual(transport.calls, ["GET \(base)/versions/42", "POST \(base)/versions/42/restore"])
    }

    func testAnsweringAQuestionPostsTheCanonicalHitlRespond() async throws {
        let request = client.answerRequest(questionId: "q 1", text: "Jordan")
        XCTAssertEqual(request.path, "/api/magician/v2/hitl/q%201/respond")
        XCTAssertEqual(request.body, .object([
            "source": .string("clarification"),
            "value": .object(["type": .string("text"), "value": .string("Jordan")]),
            "channel": .string("ios"),
            "task_id": .string("task_1")
        ]))
    }

    func testAnAlreadyAnsweredQuestionCountsAsDone() async throws {
        transport.on("POST", "/api/magician/v2/hitl/q1/respond", status: 409, ["reason": "already_resolved"])
        try await client.answer(questionId: "q1", text: "x")
        transport.on("POST", "/api/magician/v2/hitl/q2/respond", status: 400, ["message": "bad answer"])
        do {
            try await client.answer(questionId: "q2", text: "x")
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual(error.localizedDescription, "bad answer")
        }
    }

    func testDecisionsNameThePlanIdAndRunPostsExecute() {
        let approve = client.decisionRequest("approve", planId: "plan 9")
        XCTAssertEqual(approve.method, "POST")
        XCTAssertEqual(approve.path, "\(base)/approve")
        XCTAssertEqual(approve.query, [URLQueryItem(name: "plan_id", value: "plan 9")])
        XCTAssertEqual(client.decisionRequest("reject", planId: "p").path, "\(base)/reject")
        XCTAssertEqual(client.replanRequest().path, "\(base)/replan")
        XCTAssertEqual(client.executeRequest().path, "/api/magician/v3/tasks/task_1/execute")
        XCTAssertEqual(client.executionPanelRequest().path, "/api/magician/v3/tasks/task_1/execution-panel")
    }

    func testPaneEndpointsParse() async throws {
        transport.on("GET", "\(base)/analysis", ["query_analysis": NSNull()])
        transport.on("GET", "\(base)/slots", ["slot_graph_snapshot": [
            "execution_id": "e", "slots": [
                ["id": "t::who", "slot_type": "entity", "value": "Jo", "confidence": 0.9, "provenance": [["source": "UserReply"]]],
                ["id": "t::when", "slot_type": ["Custom": "x"], "value": NSNull(), "confidence": 0.1],
                ["id": "t::what", "slot_type": "Entity", "value": "Brief", "confidence": 0.5]
            ],
            "confidence_summary": ["overall": 0.5, "min_critical_slot": 0.1, "unresolved_slots": ["t::when"]],
            "slot_trigger_mappings": [], "confidence_boost_results": [:]
        ]])
        transport.on("GET", "\(base)/attempts", ["strategy_attempts": [
            ["strategy_type": "MCTS", "failure_reason": "timeout", "exploration_result": ["confidence": 0.3]]
        ]])
        transport.on("GET", "\(base)/clarifications", ["clarification_history": [
            ["question_id": "q", "question_text": "Q?", "stage": "waiting_user", "slot_updates": [["slot_id": "s"]]]
        ]])

        let analysis = try await client.analysis()
        XCTAssertNil(analysis)
        let slots = try await client.slots()
        XCTAssertEqual(slots?.groups.map(\.type), ["entity", "custom"])
        XCTAssertEqual(slots?.groups.first?.averageConfidence ?? 0, 0.7, accuracy: 0.0001)
        XCTAssertEqual(slots?.slots.first?.shortId, "who")
        XCTAssertEqual(slots?.unresolvedSlots, ["t::when"])
        XCTAssertNil(slots?.triggerMappings)
        XCTAssertNil(slots?.boostResults)
        let attempts = try await client.attempts()
        XCTAssertEqual(attempts.first?.title, "MCTS #1")
        XCTAssertEqual(attempts.first?.confidence, 0.3)
        let history = try await client.clarifications()
        XCTAssertEqual(history.first?.stageLabel, "Waiting User")
        XCTAssertEqual(history.first?.slotUpdates.count, 1)
    }

    func testManualResumeValidatesLikeTheWeb() throws {
        var form = PlanManualResume()
        XCTAssertEqual(try form.payload(), [:])
        form.updatedConfidence = "1.5"
        XCTAssertThrowsError(try form.payload()) { XCTAssertEqual($0 as? PlanManualResume.Invalid, .confidence) }
        form.updatedConfidence = "0.8"
        form.slotsJSON = #"{"slot_id":"a"}"#
        XCTAssertThrowsError(try form.payload()) { XCTAssertEqual($0 as? PlanManualResume.Invalid, .slotsNotArray) }
        form.slotsJSON = #"[{"slot_id":"a"}]"#
        XCTAssertThrowsError(try form.payload()) { XCTAssertEqual($0 as? PlanManualResume.Invalid, .slotMissingFields(1)) }
        form.slotsJSON = "[nope"
        XCTAssertThrowsError(try form.payload()) { XCTAssertEqual($0 as? PlanManualResume.Invalid, .slotsNotJSON) }
        form.slotsJSON = #"[{"slot_id":" a ","slot_type":"entity","value":{"n":1},"confidence":0.9}]"#
        form.handledQuestion = " q1 "
        let payload = try form.payload()
        XCTAssertEqual(payload["handled_question"], .string("q1"))
        XCTAssertEqual(payload["updated_confidence"], .number(0.8))
        XCTAssertEqual(payload["slots"], PlanJSON([["slot_id": "a", "slot_type": "entity", "value": ["n": 1], "confidence": 0.9]]))
        XCTAssertEqual(try client.resumeRequest(form).path, "\(base)/clarifications/resume")
    }
}
