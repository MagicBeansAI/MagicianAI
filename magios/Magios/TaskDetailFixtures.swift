#if DEBUG
import Foundation

/// Canned `/api/magician/v3` payloads for the task detail screen, one task
/// per state, for UI tests (`--tasks-ui-test-fixture`) and simulator
/// screenshots (`--open-task-detail-fixture <id>`). DEBUG only.
enum TaskDetailFixtures {
    static let states = [
        "fixture-detail-planning", "fixture-detail-approved", "fixture-detail-running",
        "fixture-detail-done", "fixture-detail-failed"
    ]

    static func isFixture(_ taskId: String) -> Bool { taskId.hasPrefix("fixture-detail-") }

    /// The value when `present`, else JSON null.
    private static func orNull(_ present: Bool, _ value: @autoclosure () -> Any) -> Any {
        present ? value() : NSNull()
    }

    private enum Shape { case planning, approved, running, done, failed, internalFailed }

    private static func shape(for taskId: String) -> Shape {
        switch taskId {
        case "fixture-detail-planning": return .planning
        case "fixture-detail-approved": return .approved
        case "fixture-detail-running", "fixture-active": return .running
        case "fixture-detail-failed": return .failed
        case "fixture-internal": return .internalFailed
        default: return .done
        }
    }

    static func payload(path: String, taskId: String, now: Date = Date()) -> [String: Any] {
        let shape = shape(for: taskId)
        let ms = { (secondsAgo: Double) -> Double in (now.timeIntervalSince1970 - secondsAgo) * 1_000 }
        let iso = { (secondsAgo: Double) -> String in
            ISO8601DateFormatter().string(from: now.addingTimeInterval(-secondsAgo))
        }
        let exec = shape == .internalFailed ? "exec-internal" : "exec-fixture"
        let title: String = {
            switch shape {
            case .planning: return "Compare three note-taking apps for a small team"
            case .approved: return "Draft the Q4 launch brief"
            case .running, .done: return "Build a concise launch brief for the mobile app"
            case .failed: return "Summarise last week's support tickets"
            case .internalFailed: return "Fixture internal task"
            }
        }()
        let status: String = {
            switch shape {
            case .planning: return "planning"
            case .approved: return "ready"
            case .running: return "running"
            case .done: return "completed"
            case .failed, .internalFailed: return "failed"
            }
        }()
        let planStatus: String? = {
            switch shape {
            case .planning: return "planning"
            case .internalFailed: return nil
            default: return "approved"
            }
        }()
        let stepNames = ["Gather the product facts", "Draft the brief", "Check the numbers", "Polish and export"]
        let stepStatuses: [[String: Any]] = {
            let marks: [String]
            switch shape {
            case .running: marks = ["completed", "in_progress", "pending", "pending"]
            case .done: marks = ["completed", "completed", "skipped", "completed"]
            case .failed: marks = ["completed", "failed", "pending", "pending"]
            default: return []
            }
            return zip(stepNames.indices, marks).map { index, mark in
                var row: [String: Any] = ["number": index, "name": stepNames[index], "status": mark,
                                          "step_id": "step-\(index + 1)"]
                if mark == "skipped" { row["note"] = "The figures were already checked in step 1." }
                if mark == "failed" { row["note"] = "The source spreadsheet could not be opened." }
                return row
            }
        }()
        let planning: [[String: Any]] = shape == .running || shape == .planning || shape == .done
            ? [
                ["id": "p1", "execution_id": "planexec_1", "timestamp": ms(95), "message": "Understood the request", "planning_stage": "query_analysis"],
                ["id": "p2", "execution_id": "planexec_1", "timestamp": ms(90), "message": "Analyzed the request · claude-sonnet · 4.2s · 3.1K tokens", "planning_stage": "llm", "tokens": 3100, "cost_usd": 0.004],
                ["id": "p3", "execution_id": "planexec_1", "timestamp": ms(70), "message": "Outlining the plan", "planning_stage": "outline"],
                ["id": "p4", "execution_id": "planexec_1", "timestamp": ms(40), "message": "Working out the steps for 4 parts", "planning_stage": "expansion"]
            ] + (shape == .planning ? [] : [
                ["id": "p5", "execution_id": "planexec_1", "timestamp": ms(20), "message": "Plan drafted and checked", "planning_stage": "plan_ready"]
            ])
            : []
        let ended = shape == .done || shape == .failed || shape == .internalFailed
        let selected: [String: Any] = shape == .planning || shape == .approved ? [:] : [
            "execution_id": exec, "status": shape == .running ? "running" : status,
            "started_at": ms(600), "ended_at": orNull(ended, ms(60)),
            "plan_id": "plan-fixture", "step_statuses": stepStatuses,
            "current_step": orNull(shape == .running, 1),
            "error_message": orNull(shape == .failed, "Step 2 failed: the source spreadsheet could not be opened.")
        ]
        let activity: [[String: Any]] = selected.isEmpty ? [] : [
            ["id": "a1", "title": "Read the product notes", "summary": "Collected three sources.", "status": "done",
             "item_type": "tool", "agent_id": "researcher", "created_at": ms(560)],
            ["id": "a2", "title": "Drafted the outline", "status": "done", "item_type": "agent_message",
             "agent_id": "researcher", "created_at": ms(420)],
            ["id": "a3", "title": shape == .running ? "Writing the brief" : "Wrote the brief",
             "status": shape == .running ? "running" : "done", "item_type": "agent_message",
             "agent_id": "researcher", "created_at": ms(200)]
        ]
        let panel: [String: Any] = [
            "overview": [
                "title": title, "description": "A one-page brief for the team: what ships, who it is for, and the three numbers that matter. Keep it short enough to read on a phone.",
                "status": status, "assigned_agent_id": "researcher", "has_plan": planStatus != nil,
                "execution_id": orNull(!selected.isEmpty, exec),
                "current_step": orNull(shape == .running, 1),
                "created_at": ms(3_600), "updated_at": ms(30)
            ],
            "run": [
                "summary": orNull(shape == .done, "The brief is written and exported."),
                "activity_log": activity, "pending_questions": [], "needs_attention": []
            ],
            "output": [
                "result": orNull(shape == .done, ["summary": "The launch brief is ready: **one page**, three numbers, two risks.", "outcome": "success"]),
                "recent_runs": selected.isEmpty ? [] : [[
                    "execution_id": exec, "status": selected["status"] ?? status,
                    "started_at": ms(600), "ended_at": orNull(ended, ms(60))
                ]],
                "deliveries": []
            ],
            "debug": [
                "selected_execution": orNull(!selected.isEmpty, selected),
                "planning_timeline": planning,
                "timeline": [], "observations": [], "shell_entries": []
            ]
        ]
        let deliverables: [[String: Any]] = shape == .done ? [
            ["output_id": "o1", "relative_path": "outputs/launch-brief-for-the-mobile-app-final.md", "media_type": "text/markdown",
             "role": "deliverable", "size_bytes": 4_812, "deliverable": true,
             "absolute_path": "/Users/me/MagicianNotes/tasks/fixture/outputs/launch-brief-for-the-mobile-app-final.md",
             "body_snippet": "# Launch brief\n\nThe mobile app ships on **14 October**. It is for small teams who plan in chat.\n\n- 3 numbers that matter\n- 2 risks"],
            ["output_id": "o2", "relative_path": "outputs/numbers.csv", "media_type": "text/csv",
             "role": "deliverable", "size_bytes": 912, "deliverable": true],
            ["output_id": "o3", "relative_path": "outputs/research-notes.md", "media_type": "text/markdown",
             "role": "working_note", "size_bytes": 7_340, "deliverable": false]
        ] : []
        let sidecars: [[String: Any]] = shape == .done ? [
            ["file_name": "continuation.json", "kind": "continuation",
             "absolute_path": "/Users/me/MagicianNotes/tasks/fixture/outputs/continuation.json"]
        ] : []
        let plan: [String: Any] = planStatus == nil ? [:] : ["plan": [
            "plan_id": "plan-fixture", "status": planStatus!, "updated_at": iso(shape == .planning ? 5 : 900),
            "pending_questions": [],
            "plan_graph": orNull(shape != .planning, ["steps": stepNames.enumerated().map { ["id": "s\($0.offset + 1)", "task": $0.element] }])
        ]]
        switch path {
        case _ where path.contains("/execution-panel"): return panel
        case _ where path.hasSuffix("/outputs"):
            return ["outputs": ["outputs": deliverables, "internal_files": sidecars]]
        case _ where path.hasSuffix("/details"):
            return ["executions": selected.isEmpty ? [] : [[
                "state": ["execution_id": exec, "status": selected["status"] ?? status, "agent_id": "researcher",
                          "started_at": iso(600), "completed_at": orNull(ended, iso(60))],
                "refs": ["output_refs": [], "child_output_refs": []], "artifacts": []
            ]]]
        case _ where path.hasSuffix("/plan"): return plan
        default:
            return ["task": [
                "manifest": ["title": title, "agent_id": "researcher",
                             "lifecycle": shape == .internalFailed ? "internal" : "persistent",
                             "created_by": shape == .internalFailed ? "__system__" : "me"],
                "state": ["status": status, "latest_root_execution_id": orNull(!selected.isEmpty, exec),
                          "active_root_execution_id": orNull(shape == .running, exec)]
            ]]
        }
    }
}
#endif
