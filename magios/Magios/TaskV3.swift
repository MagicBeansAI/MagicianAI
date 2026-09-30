import SwiftUI

/// A tag on a task (web parity: `{ id, name, color }`).
struct TaskTag: Codable, Equatable, Hashable {
    let id: String
    let name: String
    let color: String?
}

/// A pending HITL clarification on a task. Tolerant of the exact key.
struct TaskPendingQuestion: Codable, Equatable {
    let question: String?
    let text: String?
    let prompt: String?
    var display: String? { question ?? text ?? prompt }
}

/// Safe task-level actions exposed by a card swipe. Execution-level controls
/// (Pause / Resume / Steer) intentionally stay out of this enum because their
/// availability comes from the authoritative execution-control endpoint.
enum TaskCardSwipeAction: String, Equatable {
    case markComplete
    case markNotDone
    case reset
    case cancel
    case delete
}

/// Visible task-card actions, kept as deterministic model data so the mobile
/// presentation cannot drift from the web task state matrix.
enum TaskCardVisibleAction: String, Equatable, Identifiable {
    case viewPlan
    case answerQuestion
    case reviewPlan
    case runPlan
    case preplan
    case runNow
    case viewExecution
    case viewQuestion
    case reset
    /// Opens the task detail on the Output tab's Result card.
    case viewResult
    /// Publishes the task page to Notes (existing publish flow/eligibility).
    case publishToNotes

    var id: String { rawValue }
}

/// One row from `GET /api/magician/v3/tasks` (and `/tasks/internal`) — the flat
/// `TaskListItemV3` wire shape. Only the fields the list/filter/card need are
/// decoded; Swift ignores the rest (plan/schedule/enum fields we don't render yet).
struct TaskV3: Codable, Identifiable, Equatable {
    let id: String
    let title: String
    let description: String
    let status: String
    let agentId: String
    let uiThreadId: String
    let priority: String?
    let dueDate: String?
    let tags: [TaskTag]
    let dependsOn: [String]
    let schedule: JSONValue?
    let isBlocked: Bool
    let currentStepTitle: String?
    let currentSubstepTitle: String?
    let completionSummary: String?
    /// Outcome word(s) recorded when the latest run settled (web parity).
    let completionOutcome: String?
    /// Artifacts the latest run named as its result. Absent = empty.
    let completionArtifactNames: [String]
    /// Internal-lane recurring schedule, when the payload carries one.
    let recurringSchedule: JSONValue?
    let hasPlan: Bool
    let planStatus: String?
    let latestPlanId: String?
    let pendingQuestion: TaskPendingQuestion?
    let chatSessionId: String?
    let synthesisPending: Bool
    /// Set when output synthesis exhausted its retries — enables a "retry" pill.
    let synthesisFailedExecutionId: String?
    /// Origin markers for the internal-lane lifecycle badge (web parity).
    let lifecycle: String?
    /// Recurring Monitors Phase 7 — mirror of the server-owned
    /// `monitor_revision` on the list row (omitted from the wire while 0).
    /// `> 0` = the task IS a monitor; 0 = eligible for explicit conversion.
    let monitorRevision: Int
    let createdBy: String?
    let activeRootExecutionId: String?
    let latestRootExecutionId: String?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id, title, description, status, tags, priority, lifecycle, schedule
        case planStatus = "plan_status"
        case latestPlanId = "latest_plan_id"
        case pendingQuestion = "pending_question"
        case agentId = "agent_id"
        case uiThreadId = "ui_thread_id"
        case dueDate = "due_date"
        case dependsOn = "depends_on"
        case isBlocked = "is_blocked"
        case currentStepTitle = "current_step_title"
        case currentSubstepTitle = "current_substep_title"
        case completionSummary = "completion_summary"
        case completionOutcome = "completion_outcome"
        case completionArtifactNames = "completion_artifact_names"
        case recurringSchedule = "recurring_schedule"
        case hasPlan = "has_plan"
        case chatSessionId = "chat_session_id"
        case synthesisPending = "synthesis_pending"
        case synthesisFailedExecutionId = "synthesis_failed_execution_id"
        case createdBy = "created_by"
        case activeRootExecutionId = "active_root_execution_id"
        case latestRootExecutionId = "latest_root_execution_id"
        case monitorRevision = "monitor_revision"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    // Some optional/defaulted fields may be absent on older records.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        description = (try? c.decode(String.self, forKey: .description)) ?? ""
        status = try c.decode(String.self, forKey: .status)
        agentId = (try? c.decode(String.self, forKey: .agentId)) ?? ""
        uiThreadId = (try? c.decode(String.self, forKey: .uiThreadId)) ?? "general"
        priority = try? c.decodeIfPresent(String.self, forKey: .priority)
        dueDate = try? c.decodeIfPresent(String.self, forKey: .dueDate)
        tags = (try? c.decode([TaskTag].self, forKey: .tags)) ?? []
        dependsOn = (try? c.decode([String].self, forKey: .dependsOn)) ?? []
        schedule = try? c.decodeIfPresent(JSONValue.self, forKey: .schedule)
        isBlocked = (try? c.decode(Bool.self, forKey: .isBlocked)) ?? false
        currentStepTitle = try? c.decodeIfPresent(String.self, forKey: .currentStepTitle)
        currentSubstepTitle = try? c.decodeIfPresent(String.self, forKey: .currentSubstepTitle)
        completionSummary = try? c.decodeIfPresent(String.self, forKey: .completionSummary)
        completionOutcome = try? c.decodeIfPresent(String.self, forKey: .completionOutcome)
        let artifactNames: [String?] = (try? c.decodeIfPresent([String?].self, forKey: .completionArtifactNames)) ?? []
        completionArtifactNames = artifactNames
            .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        recurringSchedule = try? c.decodeIfPresent(JSONValue.self, forKey: .recurringSchedule)
        hasPlan = (try? c.decode(Bool.self, forKey: .hasPlan)) ?? false
        planStatus = try? c.decodeIfPresent(String.self, forKey: .planStatus)
        latestPlanId = try? c.decodeIfPresent(String.self, forKey: .latestPlanId)
        pendingQuestion = try? c.decodeIfPresent(TaskPendingQuestion.self, forKey: .pendingQuestion)
        chatSessionId = try? c.decodeIfPresent(String.self, forKey: .chatSessionId)
        synthesisPending = (try? c.decode(Bool.self, forKey: .synthesisPending)) ?? false
        synthesisFailedExecutionId = try? c.decodeIfPresent(String.self, forKey: .synthesisFailedExecutionId)
        lifecycle = try? c.decodeIfPresent(String.self, forKey: .lifecycle)
        monitorRevision = (try? c.decode(Int.self, forKey: .monitorRevision)) ?? 0
        createdBy = try? c.decodeIfPresent(String.self, forKey: .createdBy)
        activeRootExecutionId = try? c.decodeIfPresent(String.self, forKey: .activeRootExecutionId)
        latestRootExecutionId = try? c.decodeIfPresent(String.self, forKey: .latestRootExecutionId)
        createdAt = (try? c.decode(String.self, forKey: .createdAt)) ?? ""
        updatedAt = (try? c.decode(String.self, forKey: .updatedAt)) ?? ""
    }

    // MARK: - Display helpers (web parity)

    /// The backend-maintained active root is the only safe execution-control target.
    /// `latestRootExecutionId` is retained for inspection/history only.
    var activeExecutionIdForControls: String? {
        guard ["queued", "running", "planning", "paused", "executing"]
            .contains(status.lowercased()) else { return nil }
        guard let executionId = activeRootExecutionId?
            .trimmingCharacters(in: .whitespacesAndNewlines),
              !executionId.isEmpty else { return nil }
        return executionId
    }

    /// Manual completion must not clear the backend's active-root pointer while
    /// an execution lifecycle can still be running or resumable.
    var canMarkCompleteManually: Bool {
        !["queued", "running", "planning", "paused", "executing"]
            .contains(status.lowercased())
    }

    var statusLabel: String {
        switch status {
        case "pending": return "Pending"
        case "planning": return "Planning"
        case "ready": return "Ready"
        case "running": return "Running"
        case "paused": return "Paused"
        case "completed": return "Completed"
        case "failed": return "Failed"
        case "cancelled": return "Cancelled"
        case "deferred": return "Deferred"
        default: return status.capitalized
        }
    }

    var statusColor: Color {
        let theme = ThemeManager.shared
        switch status {
        case "running", "planning": return theme.infoColor
        case "completed": return theme.successColor
        case "failed", "cancelled": return theme.dangerColor
        case "paused", "deferred": return theme.warningColor
        default: return theme.secondaryTextColor // pending / ready
        }
    }

    /// The activity line shown under the title: the live step for a running task,
    /// else the completion summary, else the description.
    var activityLine: String? {
        if status == "running" || status == "planning" {
            return currentSubstepTitle ?? currentStepTitle ?? "Working…"
        }
        if status == "completed", let s = completionSummary, !s.isEmpty { return s }
        return description.isEmpty ? nil : description
    }

    var priorityLabel: String? {
        switch priority {
        case "p1": return "P1"
        case "p2": return "P2"
        case "p3": return "P3"
        case "p4": return "P4"
        default: return nil
        }
    }

    /// A HITL clarification is waiting on the user.
    var needsAnswer: Bool { pendingQuestion != nil }

    /// A drafted plan is waiting for approve/reject.
    var planAwaitsReview: Bool {
        hasPlan && latestPlanId != nil && (planStatus == "draft" || planStatus == "eliciting" || planStatus == nil)
    }

    /// Mirrors the Notes projection boundary enforced by the backend and used
    /// by web task actions. A live snapshot must never offer a control whose
    /// request is guaranteed to be rejected.
    var canPublishToNotes: Bool {
        TaskNotePublishEligibility.allows(status: status)
    }

    /// Web `hasFinalResult`: artifacts named, a summary written, or a
    /// completed outcome that does not read as a failure.
    var hasResult: Bool {
        TaskResultRule.hasResult(
            status: status,
            completionSummary: completionSummary,
            completionOutcome: completionOutcome,
            artifactNames: completionArtifactNames
        )
    }

    /// The task has run at least once, so "Reset to Ready" has something to
    /// reset (web: `task.executionId`).
    var hasExecution: Bool {
        [latestRootExecutionId, activeRootExecutionId].contains { id in
            id?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false
        }
    }

    /// Inline Resume/Stop controls render on the card for this task. When they
    /// do, a separate "View Execution" button would only open the same detail
    /// a card tap opens, so it is dropped (Reset stays).
    var showsInlineExecutionControls: Bool {
        ["running", "planning", "paused"].contains(status) && activeExecutionIdForControls != nil
    }

    /// The per-status action set, primary first. Plan-status rows win, as on
    /// the web card.
    private var stateCardActions: [TaskCardVisibleAction] {
        if planStatus == "planning" { return [.viewPlan] }
        if planStatus == "eliciting" { return [needsAnswer ? .answerQuestion : .viewPlan] }
        if planStatus == "draft" { return [.reviewPlan] }
        if planStatus == "approved" && status == "ready" { return [.runPlan] }
        switch status.lowercased() {
        case "pending": return [.preplan, .runNow]
        case "ready": return [.runNow]
        case "paused":
            if needsAnswer { return [.viewQuestion, .reset] }
            return showsInlineExecutionControls ? [.reset] : [.viewExecution, .reset]
        case "failed", "cancelled", "canceled":
            var actions: [TaskCardVisibleAction] = []
            if hasExecution { actions.append(.reset) }
            if canPublishToNotes { actions.append(.publishToNotes) }
            return actions
        case "completed":
            var actions: [TaskCardVisibleAction] = []
            if hasResult { actions.append(.viewResult) }
            if canPublishToNotes { actions.append(.publishToNotes) }
            return actions
        default: return []
        }
    }

    /// The card's primary (filled) action — the first of the per-status set.
    var primaryCardAction: TaskCardVisibleAction? { stateCardActions.first }

    /// Every visible card action, primary first. A task with a result shows
    /// exactly one Result button: primary on completed cards, otherwise right
    /// after the primary action.
    var visibleCardActions: [TaskCardVisibleAction] {
        var actions = stateCardActions
        if hasResult && !actions.contains(.viewResult) {
            actions.insert(.viewResult, at: actions.isEmpty ? 0 : 1)
        }
        return actions
    }

    // MARK: - Recurrence (web parity)

    /// Cron schedule, a `recurring`/`app_recurring` tag, or an internal
    /// recurring schedule on the payload.
    var isRecurring: Bool {
        TaskRecurrence.isRecurring(
            cron: scheduleCron,
            tags: tags,
            hasRecurringSchedule: recurringSchedule.map { $0 != .null } ?? false
        )
    }

    /// Human cadence for the ↻ chip ("Daily at 09:00"), or "Recurring task"
    /// when only a tag marks it.
    var recurrenceDescription: String { TaskRecurrence.description(cron: scheduleCron) }

    /// The backend keeps the schedule as an externally-tagged JSON value.
    /// These accessors mirror the web store's tolerant schedule decoder while
    /// preserving unknown fields for forward compatibility.
    var scheduleCron: String? {
        guard case .object(let root) = schedule,
              case .object(let kind)? = root["kind"],
              case .object(let cron)? = kind["Cron"],
              case .string(let expression)? = cron["expression"] else { return nil }
        return expression
    }

    var scheduleTimezone: String? {
        guard case .object(let root) = schedule else { return nil }
        if case .string(let timezone)? = root["timezone"] { return timezone }
        if case .object(let kind)? = root["kind"],
           case .object(let cron)? = kind["Cron"],
           case .string(let timezone)? = cron["timezone"] { return timezone }
        return nil
    }

    var scheduleRetentionMaxRecords: Int? {
        scheduleRetentionNumber("max_records")
    }

    var scheduleRetentionMaxDays: Int? {
        scheduleRetentionNumber("max_age_days")
    }

    private func scheduleRetentionNumber(_ key: String) -> Int? {
        guard case .object(let root) = schedule,
              case .object(let retention)? = root["execution_history_retention"],
              case .number(let value)? = retention[key] else { return nil }
        return Int(value)
    }

    // MARK: - Internal-lane badges (web parity)

    enum LifecycleKind: String { case persistent, internalTask, chat, debug }

    /// Origin badge for the internal lane: `__system__` created_by → debug,
    /// a chat_session_id → chat-spawned, else plain internal; non-internal → persistent.
    /// Mirrors the web `lifecycleBadge` (incl. the legacy wire-value aliases).
    var lifecycleBadge: (label: String, kind: LifecycleKind) {
        let isInternal = lifecycle == "internal"
            || lifecycle == "ephemeral_owned_by_chat"
            || lifecycle == "internal_debug"
        if !isInternal { return ("persistent", .persistent) }
        if createdBy == "__system__" { return ("debug", .debug) }
        if let c = chatSessionId, !c.isEmpty { return ("chat", .chat) }
        return ("internal", .internalTask)
    }

    /// Output synthesis exhausted its retries (a retry pill should show).
    var synthesisFailed: Bool { synthesisFailedExecutionId?.trimmingCharacters(in: .whitespaces).isEmpty == false }

    // MARK: - Recurring Monitors Phase 7 (explicit conversion)

    /// Client-side mirror of the backend convert eligibility gate:
    /// persistent lifecycle + not already a monitor. Archived tasks never
    /// appear on this list, and the server re-checks (409
    /// `monitor_already_exists` / `task_not_eligible_for_monitor`) on
    /// anything the mirror wrongly admits. A missing schedule is fine —
    /// the converted monitor is run-on-demand.
    var canConvertToMonitor: Bool {
        lifecycleBadge.kind == .persistent && monitorRevision == 0
    }

    /// Human summary of the schedule a conversion KEEPS (mirrors
    /// `monitors_api::cadence_summary` for the cron case; other kinds show
    /// as unscheduled here like the web store's tolerant decoder).
    var keptScheduleSummary: String {
        guard let cron = scheduleCron, !cron.isEmpty else { return "unscheduled" }
        if let timezone = scheduleTimezone, !timezone.isEmpty {
            return "Cron \(cron) (\(timezone))"
        }
        return "Cron \(cron)"
    }

    /// Leading/right swipe advances or restores a task without guessing at
    /// execution capabilities. Run remains the visible card button.
    var leadingCardSwipeActions: [TaskCardSwipeAction] {
        switch status.lowercased() {
        case "completed": return [.markNotDone]
        // Reset is a visible primary action for these states. Keeping a second
        // copy behind the swipe rail makes the gesture harder to learn and was
        // the same duplication removed from Today follow-ups.
        case "failed", "cancelled", "paused": return []
        default: return canMarkCompleteManually ? [.markComplete] : []
        }
    }

    /// Trailing/left swipe keeps destructive task-level operations together.
    /// Both actions are confirmed by the view before mutation.
    var trailingCardSwipeActions: [TaskCardSwipeAction] {
        if ["completed", "failed", "cancelled"].contains(status.lowercased()) {
            return [.delete]
        }
        return [.cancel, .delete]
    }

    /// The `updated_at` ISO string parsed to a Date (tolerant of fractional seconds).
    var updatedAtDate: Date? { TaskV3.parseISO(updatedAt) }

    static func parseISO(_ s: String) -> Date? {
        guard !s.isEmpty else { return nil }
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: s) { return d }
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: s)
    }
}

/// `{ "tasks": [ … ] }` — the list response for both lanes.
struct TaskListResponse: Codable {
    let tasks: [TaskV3]
    /// Present when the server paged the listing (`limit`/`offset` requested
    /// and honoured). Absent from a legacy binary or an unpaged request —
    /// callers treat that as "the whole pool arrived".
    let pagination: TaskListPagination?
    /// Every filter lane's total over the whole scoped pool, counted BEFORE
    /// the lane filter, keyed by the lane's wire name. Zeros are reported.
    ///
    /// Absent is a real state, not six zeros: the server reports counts only
    /// on the paged branch and only when the request carried the reader's
    /// local `today=`, since two of the lanes are date lanes it cannot
    /// compute without one. A caller that invented a `0` here would be
    /// publishing a number nobody counted.
    let counts: [String: Int]?
}

struct TaskListPagination: Codable {
    let total: Int
    let limit: Int
    let offset: Int
    let hasMore: Bool

    enum CodingKeys: String, CodingKey {
        case total, limit, offset
        case hasMore = "has_more"
    }
}
