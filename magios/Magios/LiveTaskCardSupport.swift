import Foundation

/// Pure rules the chat's live task card view reads on top of
/// `LiveTaskCardState`: which phase the card is in, its one line, the planning
/// track's segments, the running step bar, the plan preview payload, when to
/// poll and what a file-name tap copies. No SwiftUI, no networking, no clock.
///
/// Design §2 of `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`;
/// web reference `ui/unified-ui/src/lib/magician/chat/next/LiveTaskCard.svelte`.

/// The card's body, one at a time, in the order a task lives them.
enum LiveTaskCardPhase: Equatable {
    /// Working out a plan: the four-segment phase track.
    case planning
    /// A drafted plan waits for review: Review · Approve & Run.
    case planReady
    /// No run yet (`ready`, `pending`, `rejected`): the quiet tag, Run when approved.
    case notStarted
    /// A run in progress (or paused / waiting on the reader).
    case running
    /// Completed, partial, failed or cancelled.
    case finished
}

enum LiveTaskCardRules {
    /// The effective raw status: the card message's, unless the task store
    /// knows better (store wins over a message stuck at planning/draft).
    static func effectiveStatus(_ input: LiveTaskCardInput) -> String? {
        LiveTaskCardState.effectiveStatus(message: input.status, store: input.storeStatus)
    }

    static func phase(_ state: LiveTaskCardState, effectiveStatus: String?) -> LiveTaskCardPhase {
        if state.terminal { return .finished }
        if state.notStarted { return .notStarted }
        switch effectiveStatus {
        case "draft": return .planReady
        case "planning", "created": return .planning
        default: return .running
        }
    }

    /// `/execution-panel` is polled every 4 s only while the card is on screen,
    /// the app is active, and the task is planning or running (the planning
    /// track and the step bar are the only things it feeds).
    static let pollInterval: TimeInterval = 4

    static func shouldPoll(phase: LiveTaskCardPhase, hasTaskId: Bool, visible: Bool, appActive: Bool) -> Bool {
        guard hasTaskId, visible, appActive else { return false }
        return phase == .planning || phase == .running
    }

    /// The usage snapshot's phase bucket (web `usagePhase`): a task's stored
    /// usage is re-read when planning ends and when the run ends.
    static func usagePhase(_ state: LiveTaskCardState) -> String {
        if state.terminal { return "done" }
        if state.notStarted || state.status == .waiting { return "planned" }
        return "running"
    }

    /// The one line under the header. Planning: the latest planning milestone.
    /// Running a planned task: `Step 3 of 6 · <step>`. Otherwise the reason,
    /// else the latest activity line (never the bare run label: the headline
    /// already says the task is running).
    static func line(
        phase: LiveTaskCardPhase,
        state: LiveTaskCardState,
        planningEntries: [PlanningEntry],
        stepRows: [PlanStepRow],
        latestActivity: String?
    ) -> String? {
        switch phase {
        case .planning:
            if let milestone = planningEntries.last(where: { !$0.isModelCall && !$0.message.isEmpty }) {
                return milestone.message
            }
        case .running:
            if !stepRows.isEmpty { return PlanSteps.summary(stepRows) }
        case .planReady, .notStarted, .finished:
            break
        }
        return firstLine(state.reason) ?? (state.terminal ? nil : firstLine(latestActivity))
    }

    /// The planning track's line reads bold on a retry (a retry is not a failure).
    static func lineIsEmphasised(phase: LiveTaskCardPhase, track: PlanningTrack) -> Bool {
        phase == .planning && (track.retrying || track.failed)
    }

    /// Each of the four planning segments: finished, current or ahead.
    enum SegmentState: Equatable { case finished, current, ahead }

    static func planningSegments(_ track: PlanningTrack) -> [SegmentState] {
        PlanningPhase.allCases.map { phase in
            guard let current = track.phase else {
                // Nothing reached yet: the first segment is what is happening.
                return phase == .understand ? .current : .ahead
            }
            if phase.rawValue < current.rawValue { return .finished }
            return phase == current ? .current : .ahead
        }
    }

    /// What a tap on a deliverable's name copies: its host path, else the
    /// path the task wrote it at.
    static func copyPath(absolutePath: String?, relativePath: String?) -> String? {
        if let absolute = TaskOutputClassification.absolutePath(absolutePath) { return absolute }
        let relative = relativePath?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return relative.isEmpty ? nil : relative
    }

    enum OutputHome: Equatable { case task(String), session(String) }

    /// Where a chat output file is served from. The chat copies a task's
    /// deliverables into its session (`source.type` `session_output`, a
    /// `task_output_…` name), so only a block that says `task_output` is read
    /// from the task's outputs; a session copy asked of the task endpoint is
    /// "Task output not found". Without a session, a sourceless block falls
    /// back to the card's task.
    static func outputHome(
        sourceType: String?, sourceTaskId: String?, cardTaskId: String?, sessionId: String?
    ) -> OutputHome? {
        if sourceType == "task_output", let taskId = sourceTaskId ?? cardTaskId { return .task(taskId) }
        if let sessionId, !sessionId.isEmpty { return .session(sessionId) }
        if sourceType == nil, let cardTaskId { return .task(cardTaskId) }
        return nil
    }

    /// The card's one run (the chat holds one status message per task): its
    /// status, ids, first and latest update times and output count. With no
    /// run the card would have no clock and no outputs.
    static func runs(for task: TaskStatusModel, messageId: String) -> [LiveTaskRunInput] {
        let times = [task.startedAtMs, task.updatedAtMs].compactMap { $0 }
        return [LiveTaskRunInput(
            id: task.executionId ?? messageId,
            status: task.status,
            executionId: task.executionId,
            sessionId: task.uiThreadId,
            timestamps: times,
            outputCount: task.outputFiles.count
        )]
    }

    /// The card's input from the chat's status model.
    static func input(for task: TaskStatusModel, messageId: String) -> LiveTaskCardInput {
        LiveTaskCardInput(
            task: task,
            messageId: messageId,
            storeStatus: task.storeStatus,
            createdAt: task.startedAtMs,
            completionKind: task.completionKind,
            runs: runs(for: task, messageId: messageId)
        )
    }

    static func dotTone(_ status: LiveTaskStatus) -> StatusDot.Tone {
        switch status {
        case .queued: return .notStarted
        case .running: return .running
        case .waiting: return .waiting
        case .completed: return .done
        case .partial: return .partial
        case .failed: return .failed
        case .cancelled: return .cancelled
        }
    }

    /// A long reason is clamped while collapsed; expanding shows it whole.
    static func reasonOverflows(_ reason: String?) -> Bool {
        guard let reason else { return false }
        return reason.count > 140 || reason.contains("\n")
    }

    private static func firstLine(_ text: String?) -> String? {
        let line = text?
            .components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty }
        return line?.isEmpty == false ? line : nil
    }
}

// MARK: - Action cluster

/// One task action on the card. Every action the card offers sits in one
/// cluster beside the title (or, when that row is too narrow, in one row
/// directly under it) — never split across the card.
enum LiveTaskCardAction: Equatable, CaseIterable {
    /// The task detail entry. Always offered, always first.
    case inspect
    /// Start a task that has not started and is startable.
    case run
    /// Approve a drafted plan and start the run (the plan-ready primary).
    case approveAndRun
    /// Open a drafted plan for review.
    case review
    /// Open the plan graph of a task whose plan the card already knows.
    case planGraph
    /// Stop a live task. Always last, beside the expand chevron.
    case cancel
}

extension LiveTaskCardRules {
    /// The card's actions in display (and VoiceOver) order: Inspect first,
    /// then the state's primary action, then its secondary ones, Cancel last.
    ///
    /// - plan ready: Approve & Run, Review (Review already opens the plan,
    ///   so Plan graph is not offered twice)
    /// - not started and startable: Run
    /// - a known plan outside plan review: Plan graph
    /// - a live task: Cancel
    static func actions(
        phase: LiveTaskCardPhase,
        hasTaskId: Bool,
        startable: Bool,
        canCancel: Bool,
        showsPlan: Bool
    ) -> [LiveTaskCardAction] {
        var actions: [LiveTaskCardAction] = [.inspect]
        guard hasTaskId else { return actions }
        switch phase {
        case .planReady:
            actions += [.approveAndRun, .review]
        case .notStarted where startable:
            actions.append(.run)
        default:
            break
        }
        if showsPlan, phase != .planReady { actions.append(.planGraph) }
        if canCancel { actions.append(.cancel) }
        return actions
    }
}

// MARK: - The task's stored status

/// What `GET /api/magician/v3/tasks/{id}` says about a task. The record is
/// `{task: {manifest: {...}, state: {status, active_root_execution_id, ...},
/// refs: {...}}}` (`artifact_v2::models::TaskManifest` / `TaskState`); an
/// older flat `{task: {status, ...}}` still reads.
struct TaskStoreRecord: Equatable {
    let status: String
    let activeRootExecutionId: String?
    let chatSessionId: String?

    static func parse(_ object: [String: Any]) -> TaskStoreRecord? {
        guard let task = object["task"] as? [String: Any] else { return nil }
        let state = task["state"] as? [String: Any]
        let manifest = task["manifest"] as? [String: Any]
        guard let status = text(task["status"]) ?? text(state?["status"]) else { return nil }
        return TaskStoreRecord(
            status: status,
            activeRootExecutionId: text(task["active_root_execution_id"])
                ?? text(state?["active_root_execution_id"]),
            chatSessionId: text(task["chat_session_id"]) ?? text(manifest?["chat_session_id"])
                ?? text(state?["chat_session_id"]))
    }

    private static func text(_ value: Any?) -> String? {
        guard let raw = value as? String else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

extension LiveTaskCardRules {
    /// A chat task model brought in line with the task's stored status. The
    /// chat posts no message for every transition (approving a plan posts
    /// none), so a card's latest message can say `draft` long after the task
    /// went `ready`.
    ///
    /// - A not-started store status (`ready`, `pending`, `rejected`) behind a
    ///   `planning` / `draft` message is recorded as `storeStatus` only: the
    ///   card's own store-wins rule (web `liveTask.ts::effectiveStatus`)
    ///   shows it and drops the message's summary.
    /// - Any other store status that differs replaces the message's (as the
    ///   chat's authoritative refresh always has): the store is fresher, and
    ///   the message's summary, which described the state the task left, is
    ///   dropped.
    static func reconciled(_ task: TaskStatusModel, storeStatus: String) -> TaskStatusModel {
        var model = task
        model.storeStatus = storeStatus
        let message = task.status.lowercased()
        let store = storeStatus.lowercased()
        let behind = message.isEmpty || message == "planning" || message == "draft"
        if behind && LiveTaskCardState.isNotStarted(store) { return model }
        if store != message {
            model.status = storeStatus
            model.summary = nil
        }
        return model
    }
}

// MARK: - Running step bar

enum LiveTaskStepBar {
    /// Beyond this many plan steps the bar is one continuous fill.
    static let maxSegments = 12

    enum Layout: Equatable {
        /// One segment per plan step, in plan order.
        case segments([PlanStepMark])
        /// Share of steps settled (done or skipped), 0…1.
        case continuous(Double)
    }

    static func layout(_ rows: [PlanStepRow]) -> Layout? {
        guard !rows.isEmpty else { return nil }
        if rows.count <= maxSegments { return .segments(rows.map(\.mark)) }
        let settled = rows.filter { $0.mark == .done || $0.mark == .skipped }.count
        return .continuous(Double(settled) / Double(rows.count))
    }
}

// MARK: - Plan preview

/// What the card reads from `GET /v3/tasks/{id}/plan` (web
/// `applyTaskPlanPayload` + `extractPlanStepsFromPlanGraph`): the plan id (the
/// plan-approval ask's correlation id), its status and its steps' names.
struct LiveTaskPlan: Equatable {
    let planId: String?
    let status: String?
    let steps: [String]

    /// `{plan: {...}}` or the plan object itself.
    static func parse(_ payload: [String: Any]) -> LiveTaskPlan? {
        let plan = payload["plan"] as? [String: Any] ?? payload
        let graph = plan["plan_graph"] as? [String: Any] ?? [:]
        let rawSteps = graph["steps"] as? [[String: Any]] ?? []
        let steps = rawSteps.map { step -> String in
            let metadata = step["metadata"] as? [String: Any] ?? [:]
            return text(step["task"]) ?? text(metadata["description"])
                ?? "Step \(text(step["id"]) ?? "")".trimmingCharacters(in: .whitespaces)
        }
        let planId = text(plan["plan_id"])
        let status = text(plan["status"])
        guard planId != nil || status != nil || !steps.isEmpty else { return nil }
        return LiveTaskPlan(planId: planId, status: status, steps: steps)
    }

    private static func text(_ value: Any?) -> String? {
        guard let raw = value as? String else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
