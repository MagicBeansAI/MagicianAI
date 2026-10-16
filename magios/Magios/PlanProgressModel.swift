import Foundation

/// Planning phases and plan-step marks, read from the `/execution-panel`
/// payload. Pure — no SwiftUI, no networking.
///
/// Sources: `planProgressFrom` in
/// `ui/unified-ui/src/lib/magician/tasks/taskPanelModel.ts` (planning entries),
/// `magician/src/magician_v2/progress_channel_seam/normalize.rs`
/// (`planning_milestone`: the stages and their wording),
/// `magician-learning/src/execution_panel/v3_adapter.rs`
/// (`plan_step_statuses_from`, `apply_reported_step_outcomes`) and
/// `docs/plans/2026-10-01-plan-step-progress.md`. The stage → phase table is
/// design §4 of `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.

// MARK: - Planning

/// The four-segment track a planning card shows.
enum PlanningPhase: Int, CaseIterable, Equatable {
    case understand, outline, steps, check

    var title: String {
        switch self {
        case .understand: return "Understand"
        case .outline: return "Outline"
        case .steps: return "Steps"
        case .check: return "Check"
        }
    }
}

/// One planning milestone or planning model call, oldest first.
struct PlanningEntry: Equatable, Identifiable {
    let id: String
    let at: Date
    let message: String
    /// Warning or error severity: shown bold (a retry is not a failure).
    let warning: Bool
    /// The planning stage (`query_analysis` … `plan_ready`, `plan_retry`,
    /// `failed`, `llm`), when it can be read.
    let stage: String?
    /// A planning LLM call: a compact secondary row, never moves the track.
    let isModelCall: Bool
    /// A model call's tokens and price, when the row carries them (optional
    /// on the wire; the message already reads `op · model · 8.5s · 9.1K tokens`).
    var tokens: Int? = nil
    var costUsd: Double? = nil
}

/// Where the planning track stands.
struct PlanningTrack: Equatable {
    /// The latest phase reached; nil before any phase-moving milestone.
    let phase: PlanningPhase?
    /// The latest move was a retry (back to Steps).
    let retrying: Bool
    /// Planning hit a problem after the latest move; the phase is kept.
    let failed: Bool
}

enum PlanProgress {
    /// `debug.planning_timeline` when the server sends it (even empty), else
    /// `debug.timeline` rows titled "Progress" (an older server); either way
    /// only `planexec_` rows, oldest first. Web `planProgressFrom`.
    static func entries(fromPanel json: [String: Any]) -> [PlanningEntry] {
        let debug = json["debug"] as? [String: Any] ?? [:]
        let dedicated = debug["planning_timeline"] as? [[String: Any]]
        let source = dedicated ?? (debug["timeline"] as? [[String: Any]] ?? [])
        return source
            .filter { row in
                (dedicated != nil || (row["title"] as? String) == "Progress")
                    && ((row["execution_id"] as? String) ?? "").hasPrefix("planexec_")
            }
            .compactMap { row -> PlanningEntry? in
                guard let id = row["id"] as? String, let at = instant(row["timestamp"]) else { return nil }
                let severity = (row["severity"] as? String)?.lowercased()
                let stage = stage(of: row)
                return PlanningEntry(
                    id: id,
                    at: at,
                    message: row["message"] as? String ?? "",
                    warning: severity == "warning" || severity == "error",
                    stage: stage,
                    isModelCall: stage == "llm",
                    tokens: (row["tokens"] as? NSNumber)?.intValue,
                    costUsd: (row["cost_usd"] as? NSNumber).map(\.doubleValue).flatMap { $0.isFinite && $0 >= 0 ? $0 : nil }
                )
            }
            .enumerated()
            .sorted { lhs, rhs in
                lhs.element.at == rhs.element.at ? lhs.offset < rhs.offset : lhs.element.at < rhs.element.at
            }
            .map(\.element)
    }

    /// The entry's planning stage: the `planning_stage` field; else the stage
    /// segment of a `planning:{exec}:{stage}:{ts}` id; else, last, the
    /// backend's fixed milestone wording (`normalize.rs::planning_milestone`).
    ///
    /// The last fallback exists because today's panel rows carry neither: the
    /// field is requested, and a row's id is the progress message's UUID, not
    /// its `planning:` entity key. Model calls are recognised by the ` · `
    /// separator only they use (`Analyzed the request · model · 8.5s`).
    static func stage(of entry: [String: Any]) -> String? {
        if let field = (entry["planning_stage"] as? String)?.trimmingCharacters(in: .whitespaces),
           !field.isEmpty {
            return field
        }
        if let id = entry["id"] as? String {
            let parts = id.split(separator: ":", omittingEmptySubsequences: false).map(String.init)
            if parts.count >= 4, parts[0] == "planning", !parts[parts.count - 2].isEmpty {
                return parts[parts.count - 2]
            }
        }
        return stage(fromMessage: entry["message"] as? String ?? "")
    }

    private static let milestoneWording: [(prefix: String, stage: String)] = [
        ("Understood the request", "query_analysis"),
        ("Picking out the details the task needs", "slot_extraction"),
        ("Restated the task", "clarified_task"),
        ("Outlining the plan", "outline"),
        ("Outlined ", "outline_done"),
        ("Working out the steps for", "expansion"),
        ("Checking the drafted plan", "validation"),
        ("Plan drafted and checked", "plan_ready"),
        ("The draft did not pass its checks", "plan_retry"),
        ("Planning hit a problem", "failed")
    ]

    private static func stage(fromMessage message: String) -> String? {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.contains(" · ") { return "llm" }
        return milestoneWording.first { text.hasPrefix($0.prefix) }?.stage
    }

    /// Design §4. `llm` and `failed` never move the track (nil).
    static func phase(for stage: String) -> PlanningPhase? {
        switch stage {
        case "query_analysis", "slot_extraction", "clarified_task": return .understand
        case "outline", "outline_done": return .outline
        case "expansion", "plan_retry": return .steps
        case "validation", "plan_ready": return .check
        default: return nil
        }
    }

    /// The latest phase reached. A `plan_retry` steps back to Steps and marks
    /// the retry; `failed` keeps the phase and marks it; a later move clears
    /// both (a durable retry resumed planning).
    static func currentPhase(_ entries: [PlanningEntry]) -> PlanningTrack {
        var phase: PlanningPhase?
        var retrying = false
        var failed = false
        for entry in entries {
            guard let stage = entry.stage, !entry.isModelCall else { continue }
            if stage == "failed" {
                failed = true
                continue
            }
            guard let moved = Self.phase(for: stage) else { continue }
            phase = moved
            retrying = stage == "plan_retry"
            failed = false
        }
        return PlanningTrack(phase: phase, retrying: retrying, failed: failed)
    }

    /// The folded line after planning: `N events · duration` (first to last).
    static func activitySummary(_ entries: [PlanningEntry]) -> String {
        guard let first = entries.first, let last = entries.last else { return "" }
        let count = "\(entries.count) \(entries.count == 1 ? "event" : "events")"
        guard entries.count > 1,
              let span = TaskVerdict.durationIfKnown(last.at.timeIntervalSince(first.at)) else { return count }
        return "\(count) · \(span)"
    }

    /// Epoch milliseconds (the wire's `i64`, as the web reads it), or ISO-8601.
    private static func instant(_ value: Any?) -> Date? {
        if let text = value as? String { return TaskV3.parseISO(text) }
        guard let number = value as? NSNumber, number.doubleValue.isFinite else { return nil }
        return Date(timeIntervalSince1970: number.doubleValue / 1_000)
    }
}

// MARK: - Plan steps

/// A plan step's mark in the run checklist and the step bar.
enum PlanStepMark: String, Equatable {
    case done, failed, inProgress, pending, skipped
    /// Still pending when the run ended.
    case notReached
}

struct PlanStepRow: Equatable, Identifiable {
    let number: Int
    let name: String
    let mark: PlanStepMark
    /// The agent's reason (a skipped, failed or done-another-way step), on any mark.
    let note: String?
    let stepId: String?
    /// Activity rows recorded while the run was on this step
    /// (`step_statuses[].activity_count`); nil when the run is not attributed.
    var activityCount: Int? = nil

    var id: String { stepId ?? "step-\(number)" }
}

enum PlanSteps {
    /// `debug.selected_execution.step_statuses` as rows in plan order (the
    /// order the payload lists them; never re-sorted by completion).
    static func rows(fromPanel json: [String: Any], runEnded: Bool) -> [PlanStepRow] {
        let debug = json["debug"] as? [String: Any] ?? [:]
        let selected = debug["selected_execution"] as? [String: Any] ?? [:]
        return rows(statuses: selected["step_statuses"] as? [[String: Any]] ?? [], runEnded: runEnded)
    }

    static func rows(statuses: [[String: Any]], runEnded: Bool) -> [PlanStepRow] {
        statuses.enumerated().map { offset, item in
            let number = integer(item["number"]) ?? offset + 1
            return PlanStepRow(
                number: number,
                name: text(item["name"]) ?? "Step \(number)",
                mark: mark(for: item["status"] as? String, runEnded: runEnded),
                note: text(item["note"]),
                stepId: text(item["step_id"]),
                activityCount: integer(item["activity_count"])
            )
        }
    }

    /// `completed | failed | in_progress | pending | skipped`. An unknown or
    /// cancelled status reads as pending; pending reads `notReached` once the
    /// run ended.
    static func mark(for status: String?, runEnded: Bool) -> PlanStepMark {
        switch status?.trimmingCharacters(in: .whitespaces).lowercased() {
        case "completed", "done": return .done
        case "failed": return .failed
        case "in_progress", "running": return .inProgress
        case "skipped": return .skipped
        default: return runEnded ? .notReached : .pending
        }
    }

    /// `Step 3 of 6 · <name>` while a step runs; `Step 2 of 3 failed · <name>`
    /// after a failure; else `4 of 6 done` (`· 1 skipped` when any were).
    static func summary(_ rows: [PlanStepRow]) -> String {
        guard !rows.isEmpty else { return "" }
        let total = rows.count
        if let index = rows.firstIndex(where: { $0.mark == .inProgress }) {
            return "Step \(index + 1) of \(total) · \(rows[index].name)"
        }
        if let index = rows.firstIndex(where: { $0.mark == .failed }) {
            return "Step \(index + 1) of \(total) failed · \(rows[index].name)"
        }
        let done = rows.filter { $0.mark == .done }.count
        let skipped = rows.filter { $0.mark == .skipped }.count
        return skipped > 0 ? "\(done) of \(total) done · \(skipped) skipped" : "\(done) of \(total) done"
    }

    private static func text(_ value: Any?) -> String? {
        guard let raw = value as? String else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private static func integer(_ value: Any?) -> Int? {
        if let value = value as? Int { return value }
        if let value = value as? NSNumber { return value.intValue }
        if let value = value as? String { return Int(value) }
        return nil
    }
}

// MARK: - Off-plan attribution

/// The run's activity grouped under the plan step the run was on when each
/// row was recorded, and the rows outside every step (before the first,
/// between steps, after the last, after a skip). The server decides the
/// attribution (`plan_step_id` / `off_plan` on each row); this only groups.
/// Web `planStepActivity.ts`, Android `PlanStepActivity.kt` — same cases.
struct PlanStepActivityGroups: Equatable {
    /// Whether any row carries an attribution; false leaves the act unchanged.
    let attributed: Bool
    /// Each plan step's rows, oldest first, by plan step id.
    let byStep: [String: [TaskDetailActivity]]
    /// The rows outside every plan step, oldest first.
    let offPlan: [TaskDetailActivity]

    static let empty = PlanStepActivityGroups(attributed: false, byStep: [:], offPlan: [])

    /// A step's row count; 0 for a step with none; nil when nothing is attributed.
    func count(for stepId: String?) -> Int? {
        guard attributed, let stepId else { return nil }
        return byStep[stepId]?.count ?? 0
    }
}

enum PlanStepActivity {
    static let offPlanExplanation = "Work the agent did between or beyond the planned steps"

    static func offPlanLabel(_ count: Int) -> String { "Outside the plan · \(count)" }

    /// Rows with no attribution (lifecycle rows) belong to neither group; the
    /// flat "All activity" timeline still shows every row.
    static func group(_ activity: [TaskDetailActivity]) -> PlanStepActivityGroups {
        let ordered = activity.enumerated().sorted { lhs, rhs in
            let left = lhs.element.timestamp ?? .distantPast
            let right = rhs.element.timestamp ?? .distantPast
            return left == right ? lhs.offset < rhs.offset : left < right
        }.map(\.element)
        var byStep: [String: [TaskDetailActivity]] = [:]
        var offPlan: [TaskDetailActivity] = []
        var attributed = false
        for row in ordered {
            if let stepId = row.planStepId?.trimmingCharacters(in: .whitespaces), !stepId.isEmpty {
                attributed = true
                byStep[stepId, default: []].append(row)
            } else if row.offPlan {
                attributed = true
                offPlan.append(row)
            }
        }
        return attributed ? PlanStepActivityGroups(attributed: true, byStep: byStep, offPlan: offPlan) : .empty
    }
}
