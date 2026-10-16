import SwiftUI

/// Whether a task is a planned task: one rule for every surface that shows
/// the plan glyph by a task title (chat live card, Tasks rows, task detail).
/// Web and Android draw the same three-node tree glyph.
enum TaskPlanMarker {
    /// Statuses that only a planned task passes through.
    static let planningStatuses: Set<String> = ["planning", "draft", "eliciting"]

    /// A task has a plan when the record says so, a plan status or plan id is
    /// present, plan steps are known, or any of `statuses` (the chat message
    /// or stored task status) is a planning status.
    static func hasPlan(
        hasPlanFlag: Bool = false,
        planStatus: String? = nil,
        latestPlanId: String? = nil,
        approvedPlanId: String? = nil,
        planStepCount: Int = 0,
        statuses: [String?] = []
    ) -> Bool {
        if hasPlanFlag || planStepCount > 0 { return true }
        if present(planStatus) || present(latestPlanId) || present(approvedPlanId) { return true }
        return statuses.contains { status in
            guard let raw = status?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() else { return false }
            return planningStatuses.contains(raw)
        }
    }

    /// A Tasks list row (`GET /v3/tasks`).
    static func hasPlan(_ task: TaskV3) -> Bool {
        hasPlan(hasPlanFlag: task.hasPlan, planStatus: task.planStatus,
                latestPlanId: task.latestPlanId, approvedPlanId: task.approvedPlanId,
                statuses: [task.status])
    }

    /// The task detail screen's snapshot (task overview + fetched plan); a
    /// plan body counts as plan steps.
    static func hasPlan(_ snapshot: TaskDetailSnapshot) -> Bool {
        hasPlan(hasPlanFlag: snapshot.taskHasPlan, planStatus: snapshot.planStatus,
                latestPlanId: snapshot.planId,
                planStepCount: present(snapshot.planMarkdown) ? 1 : 0,
                statuses: [snapshot.status])
    }

    /// The chat live task card: the message and store statuses, plus the plan
    /// the card loaded (`GET /v3/tasks/{id}/plan`), if any.
    static func hasPlan(messageStatus: String?, storeStatus: String?, plan: LiveTaskPlan?) -> Bool {
        hasPlan(planStatus: plan?.status, latestPlanId: plan?.planId,
                planStepCount: plan?.steps.count ?? 0,
                statuses: [messageStatus, storeStatus])
    }

    private static func present(_ value: String?) -> Bool {
        guard let value else { return false }
        return !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

/// The plan glyph drawn before a planned task's title, in the theme's
/// secondary accent. Place it in a `.firstTextBaseline` row with the title so
/// it sits on the title's first line.
struct TaskPlanGlyph: View {
    static let symbol = "point.3.filled.connected.trianglepath.dotted"

    var size: CGFloat = 11

    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Image(systemName: Self.symbol)
            .font(.system(size: size, weight: .semibold))
            .foregroundColor(theme.secondaryAccentColor)
            .accessibilityLabel("Planned task")
            .accessibilityIdentifier("task-plan-glyph")
    }
}
