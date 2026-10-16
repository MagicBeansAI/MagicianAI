#if DEBUG
import Foundation

/// Canned Plan Inspector responses for simulator screenshots and UI checks
/// (`--open-plan-inspector-fixture <id> [graph|steps|history|steps-edit|step-sheet|clarifications|slots]`).
/// DEBUG only. Ids: `fixture-plan-draft` (a draft with a pending question),
/// `fixture-plan-running` (approved, run in progress with live step marks).
enum PlanInspectorFixtures {
    static let ids = ["fixture-plan-draft", "fixture-plan-running"]

    static func transport(for taskId: String) -> PlanInspectorTransport? {
        guard taskId.hasPrefix("fixture-plan-") else { return nil }
        return FixtureTransport(running: taskId == "fixture-plan-running")
    }

    /// The launch argument's optional second value: the state to open in.
    static var launchState: String? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--open-plan-inspector-fixture"),
              arguments.indices.contains(index + 2), !arguments[index + 2].hasPrefix("-") else { return nil }
        return arguments[index + 2]
    }

    static func graphJSON() -> [String: Any] {
        func step(_ id: String, _ task: String, tool: String?, confidence: Double,
                  parameters: [String: Any] = [:], agent: String? = nil) -> [String: Any] {
            var object: [String: Any] = [
                "id": id, "task": task, "tool": tool ?? NSNull(), "parameters": parameters,
                "expected_outputs": ["\(id) result"], "confidence": confidence,
                "metadata": ["rationale": "Fixture step"], "success_criteria": "Done when \(id) has output"
            ]
            if let agent { object["providing_agent_id"] = agent }
            return object
        }
        return [
            "steps": [
                step("gather", "Gather the last quarter's sales exports", tool: "files__read", confidence: 0.92,
                     parameters: ["folder": "Reports/Q3", "limit": 50]),
                step("crm", "Pull pipeline changes from the CRM", tool: "crm__query", confidence: 0.74,
                     parameters: ["since": "2026-07-01"]),
                step("market", "Scan competitor pricing news", tool: nil, confidence: 0.38, agent: "web-researcher"),
                step("analyze", "Compare revenue against pipeline movement", tool: "python__run", confidence: 0.61,
                     parameters: ["notebook": "q3.ipynb", "charts": true, "seed": 7]),
                step("draft", "Draft the quarterly brief", tool: "docs__write", confidence: 0.81),
                step("review", "Check numbers and send for review", tool: "mail__send", confidence: 0.55)
            ],
            "edges": [
                ["from": "gather", "to": "analyze", "reason": "needs the exports"],
                ["from": "crm", "to": "analyze", "reason": "needs pipeline data"],
                ["from": "market", "to": "draft", "reason": "context for the narrative"],
                ["from": "analyze", "to": "draft", "reason": "charts and findings"],
                ["from": "draft", "to": "review", "reason": "the brief to check"]
            ],
            "unresolved_inputs": [["step_id": "review", "parameter": "recipient", "prompt": "Who reviews it?"]],
            "confidence": 0.72,
            "provenance": ["strategy": "AtomicComposition", "generator": "planner", "notes": NSNull()],
            "session_policy": ["mode": "fixture"]
        ]
    }

    private final class FixtureTransport: PlanInspectorTransport {
        let running: Bool
        var graph: [String: Any] = PlanInspectorFixtures.graphJSON()
        var status: String

        init(running: Bool) {
            self.running = running
            status = running ? "approved" : "draft"
        }

        func send(_ request: PlanInspectorRequest) async throws -> PlanInspectorResponse {
            try? await Task.sleep(nanoseconds: 150_000_000)
            let path = request.path
            func ok(_ object: Any) -> PlanInspectorResponse { PlanInspectorResponse(status: 200, body: PlanJSON(object)) }
            if path.hasSuffix("/execution-panel") {
                let marks = ["completed", "completed", "completed", "in_progress", "pending", "pending"]
                let steps = ["gather", "crm", "market", "analyze", "draft", "review"]
                return ok(["debug": ["selected_execution": [
                    "status": running ? "running" : "completed",
                    "step_statuses": running
                        ? steps.enumerated().map { ["number": $0.offset + 1, "name": $0.element, "status": marks[$0.offset], "step_id": $0.element] }
                        : []
                ]]])
            }
            if path.hasSuffix("/plan/versions") {
                let now = Int64(Date().timeIntervalSince1970 * 1000)
                return ok(["versions": [
                    ["epoch_ms": now - 3_600_000, "label": "Before adding market scan", "step_count": 5, "confidence": 0.68],
                    ["epoch_ms": now - 86_400_000, "label": NSNull(), "step_count": 4, "confidence": 0.6]
                ], "has_more": false])
            }
            if path.contains("/plan/versions/"), request.method == "GET" {
                var older = PlanInspectorFixtures.graphJSON()
                older["steps"] = (older["steps"] as? [[String: Any]] ?? []).filter { $0["id"] as? String != "market" }
                older["edges"] = (older["edges"] as? [[String: Any]] ?? []).filter { $0["from"] as? String != "market" }
                return ok(["version": ["epoch_ms": 1, "plan_graph": older]])
            }
            if path.hasSuffix("/plan/clarifications") {
                return ok(["clarification_history": [[
                    "question_id": "q-format", "question_text": "Should the brief be a doc or slides?",
                    "stage": "answered", "answered_at": Int64(Date().timeIntervalSince1970 * 1000) - 600_000,
                    "slot_updates": [["slot_id": "task::format", "slot_type": "modifier", "value": "doc",
                                      "confidence": 0.9, "received_at": 0]]
                ]]])
            }
            if path.hasSuffix("/plan/slots") {
                return ok(["slot_graph_snapshot": [
                    "execution_id": "planexec_fixture",
                    "slots": [
                        ["id": "task::period", "slot_type": "temporal", "value": "Q3 2026", "confidence": 0.95, "provenance": [["source": "LlmPrimary", "timestamp": 0]]],
                        ["id": "task::audience", "slot_type": "entity", "value": "Leadership team", "confidence": 0.7, "provenance": []],
                        ["id": "task::reviewer", "slot_type": "entity", "value": NSNull(), "confidence": 0.2, "provenance": []],
                        ["id": "task::format", "slot_type": "modifier", "value": "doc", "confidence": 0.9, "provenance": [["source": "UserReply", "timestamp": 0]]]
                    ],
                    "confidence_summary": ["overall": 0.69, "min_critical_slot": 0.2, "unresolved_slots": ["task::reviewer"]],
                    "clarified_task": ["goal": "Quarterly brief", "constraints": ["Q3 only"]]
                ]])
            }
            if path.hasSuffix("/plan/analysis") {
                return ok(["query_analysis": ["intent": "report", "complexity": "multi_step", "domains": ["sales", "finance"]]])
            }
            if path.hasSuffix("/plan/attempts") {
                return ok(["strategy_attempts": [
                    ["strategy_type": "AtomicComposition", "attempt_number": 1, "failure_reason": NSNull(),
                     "attempted_at": Int64(Date().timeIntervalSince1970 * 1000), "exploration_result": ["confidence": 0.72, "nodes": 14]]
                ]])
            }
            if path.hasSuffix("/plan"), request.method == "PUT" {
                if let plan = request.body?["plan"]?.foundation as? [String: Any] { graph = plan }
                return ok(["plan": record()])
            }
            if path.hasSuffix("/plan/approve") { status = "approved"; return ok(["plan": record()]) }
            if path.hasSuffix("/plan/reject") { status = "rejected"; return ok(["plan": record()]) }
            if path.hasSuffix("/execute") || path.hasSuffix("/respond") || path.hasSuffix("/resume") || path.hasSuffix("/restore") {
                return ok(["plan": record(), "workflow_resumed": true])
            }
            if path.hasSuffix("/plan") { return ok(["plan": record()]) }
            return PlanInspectorResponse(status: 404, body: PlanJSON(["error": "not_found"]))
        }

        private func record() -> [String: Any] {
            [
                "plan_id": "plan-fixture", "task_id": "fixture", "status": status, "plan_graph": graph,
                "pending_questions": status == "draft" ? [[
                    "id": "q-reviewer", "question_text": "Who should review the brief before it goes out?",
                    "options": [["value": "cfo", "label": "The CFO"], ["value": "team", "label": "My team"]],
                    "source_slot_id": "task::reviewer", "urgency": 0.8
                ]] : []
            ]
        }
    }
}
#endif
