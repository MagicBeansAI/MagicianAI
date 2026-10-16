import SwiftUI

/// "Planner needs you" above the composer (web `components/PlannerDock.svelte`):
/// the chat's tasks whose plan waits on the reader — questions to answer
/// (eliciting) or a draft to review — collapsed to one line ("N items across
/// M tasks"), expanded to the selected task's questions (Reply focuses the
/// composer on that question) and its plan actions: Open/Review plan,
/// Approve, Approve & Run, Replan, Reject.

// MARK: - Model

struct PlannerDockEntry: Identifiable, Equatable {
    let taskId: String
    var title: String
    var planId: String?
    var status: PlanStatus?
    var questions: [PlanQuestion]

    var id: String { taskId }
}

enum PlannerDockModel {
    /// Tasks worth a plan read: still open, and not already running a plan.
    static func candidateTaskIds(_ tasks: [TaskStatusModel], limit: Int = 6) -> [TaskStatusModel] {
        var seen = Set<String>()
        let open = tasks.reversed().filter { task in
            guard !task.taskId.isEmpty, !task.isTerminal else { return false }
            let status = (task.storeStatus ?? task.status).lowercased()
            return !["running", "in_progress", "executing", "queued", "cancelled", "canceled"].contains(status)
                && seen.insert(task.taskId).inserted
        }
        return Array(open.prefix(limit))
    }

    /// The entries the dock shows (web `plannerDockTasks`): eliciting with a
    /// question, or a draft; eliciting first, then the input order (the
    /// loader reads the newest task first).
    static func dockEntries(_ entries: [PlannerDockEntry]) -> [PlannerDockEntry] {
        entries.enumerated()
            .filter { ($0.element.status == .eliciting && !$0.element.questions.isEmpty) || $0.element.status == .draft }
            .sorted { lhs, rhs in
                let l = rank(lhs.element), r = rank(rhs.element)
                return l != r ? l < r : lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    private static func rank(_ entry: PlannerDockEntry) -> Int { entry.status == .eliciting ? 0 : 1 }

    /// Pending questions plus drafts to review.
    static func attentionCount(_ entries: [PlannerDockEntry]) -> Int {
        entries.reduce(0) { $0 + ($1.status == .eliciting ? $1.questions.count : 0) }
            + entries.filter { $0.status == .draft }.count
    }

    /// "3 items across 2 tasks" / "1 item".
    static func summary(_ entries: [PlannerDockEntry]) -> String {
        let count = attentionCount(entries)
        let items = "\(count) \(count == 1 ? "item" : "items")"
        return entries.count > 1 ? "\(items) across \(entries.count) tasks" : items
    }

    static func headline(_ entry: PlannerDockEntry) -> String {
        switch entry.status {
        case .eliciting: return "Waiting for clarification"
        case .draft: return "Plan ready for review"
        default: return "Task plan"
        }
    }

    static func detail(_ entry: PlannerDockEntry) -> String {
        switch entry.status {
        case .eliciting:
            return entry.questions.count > 1
                ? "The planner is waiting on \(entry.questions.count) answers before it can finalize the plan."
                : (entry.questions.first?.text ?? "The planner needs one more answer.")
        case .draft: return "The plan is ready for review, editing, approval, or replanning."
        default: return entry.title
        }
    }

    /// A plan reply whose question left the planner is stale.
    static func isStale(_ reply: PlanReplyTarget, entries: [PlannerDockEntry]) -> Bool {
        guard let entry = entries.first(where: { $0.taskId == reply.taskId }) else { return false }
        return !(entry.status == .eliciting && entry.questions.contains { $0.id == reply.questionId })
    }
}

enum PlannerDockAction: String { case open, approve, approveAndRun, replan, reject }

// MARK: - Loader

/// Reads each candidate task's `/plan` (the Plan Inspector's client).
@MainActor
final class PlannerDockLoader: ObservableObject {
    @Published private(set) var entries: [PlannerDockEntry] = []
    @Published private(set) var busy: Set<String> = []
    @Published var notice: String?
    private var transport: PlanInspectorTransport

    init(transport: PlanInspectorTransport = LivePlanInspectorTransport()) {
        self.transport = transport
    }

    #if DEBUG
    /// Fixtures and render tests.
    func setEntries(_ entries: [PlannerDockEntry]) { self.entries = entries }
    #endif

    var dockEntries: [PlannerDockEntry] { PlannerDockModel.dockEntries(entries) }

    func refresh(tasks: [TaskStatusModel]) async {
        let candidates = PlannerDockModel.candidateTaskIds(tasks)
        guard !candidates.isEmpty else {
            if !entries.isEmpty { entries = [] }
            return
        }
        var fresh: [PlannerDockEntry] = []
        for task in candidates {
            let client = PlanInspectorClient(taskId: task.taskId, transport: transport)
            guard let record = try? await client.loadPlan() else {
                // Keep what we knew when one read fails; drop it when the plan is gone.
                if let known = entries.first(where: { $0.taskId == task.taskId }) { fresh.append(known) }
                continue
            }
            fresh.append(PlannerDockEntry(
                taskId: task.taskId, title: task.title, planId: record.planId,
                status: record.status, questions: record.pendingQuestions))
        }
        guard !Task.isCancelled else { return }
        if fresh != entries { entries = fresh }
    }

    func isBusy(_ taskId: String, _ action: PlannerDockAction) -> Bool { busy.contains("\(taskId):\(action.rawValue)") }

    /// Approve / Approve & Run / Replan / Reject. Returns the outcome line.
    func perform(_ action: PlannerDockAction, on entry: PlannerDockEntry) async {
        let key = "\(entry.taskId):\(action.rawValue)"
        guard !busy.contains(key) else { return }
        let client = PlanInspectorClient(taskId: entry.taskId, transport: transport)
        busy.insert(key)
        defer { busy.remove(key) }
        switch action {
        case .open:
            return
        case .replan:
            do { try await client.replan(); notice = "Planning restarted." } catch { notice = error.localizedDescription }
        case .approve, .reject, .approveAndRun:
            guard let planId = entry.planId else { notice = "The plan could not be loaded."; return }
            let outcome: PlanApprovalOutcome
            switch action {
            case .reject:
                do { try await client.reject(planId: planId); outcome = .rejected } catch { outcome = .failed(error.localizedDescription) }
            case .approve:
                do { try await client.approve(planId: planId); outcome = .approved } catch { outcome = .failed(error.localizedDescription) }
            default:
                outcome = await PlanApprovalActions.approveAndRun(
                    taskId: entry.taskId,
                    approve: {
                        do { try await client.approve(planId: planId); return .ok } catch { return .failed(error.localizedDescription) }
                    },
                    startRun: { _ in try await client.startRun() })
            }
            notice = outcome.message
            if outcome.isApproved || outcome == .rejected {
                entries.removeAll { $0.taskId == entry.taskId }
            }
        }
    }
}

// MARK: - View

struct ComposerPlannerDock: View {
    @ObservedObject var loader: PlannerDockLoader
    var activeReply: PlanReplyTarget?
    var disabled = false
    var onReply: (PlannerDockEntry, PlanQuestion) -> Void
    var onOpenPlan: (PlannerDockEntry) -> Void
    /// Fixtures and render tests open it expanded.
    var startsExpanded = false

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var expanded = false
    @State private var selectedTaskId: String?

    private var entries: [PlannerDockEntry] { loader.dockEntries }
    private var selected: PlannerDockEntry? {
        if let reply = activeReply, let entry = entries.first(where: { $0.taskId == reply.taskId }) { return entry }
        return entries.first { $0.taskId == selectedTaskId } ?? entries.first
    }

    var body: some View {
        if let selected {
            VStack(spacing: 0) {
                header(selected)
                if expanded { details(selected) }
            }
            .background(theme.surfaceColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.composerRadius))
            .overlay(
                RoundedRectangle(cornerRadius: Lively.composerRadius)
                    .stroke(theme.secondaryTextColor.opacity(0.2), lineWidth: 1)
            )
            .padding(.horizontal, ComposerView.horizontalInset)
            .padding(.bottom, 6)
            .transition(reduceMotion ? .identity : .move(edge: .bottom).combined(with: .opacity))
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Planner attention")
            .accessibilityIdentifier("composer-planner-dock")
            .onAppear { if startsExpanded { expanded = true } }
        }
    }

    private func header(_ entry: PlannerDockEntry) -> some View {
        Button {
            withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded.toggle() }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "doc.text").font(.system(size: 12)).accessibilityHidden(true)
                Text("Planner needs you").font(.themed(12, weight: .semibold)).fixedSize()
                if entries.count == 1 {
                    Text(entry.title).font(.themed(12)).foregroundColor(theme.textColor)
                        .lineLimit(1).truncationMode(.tail)
                }
                Spacer(minLength: 6)
                Text(PlannerDockModel.summary(entries))
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
                    .fixedSize()
                Image(systemName: "chevron.down")
                    .font(.system(size: 10, weight: .semibold))
                    .rotationEffect(.degrees(expanded ? 180 : 0))
                    .foregroundColor(theme.secondaryTextColor)
                    .accessibilityHidden(true)
            }
            .foregroundColor(theme.textColor)
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity, minHeight: Lively.tapMin, alignment: .leading)
            .background(theme.softBackgroundColor.opacity(0.5))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityValue(expanded ? "Expanded" : "Collapsed")
        .accessibilityHint(expanded ? "Collapses the planner details" : "Shows the planner's questions and actions")
    }

    @ViewBuilder
    private func details(_ entry: PlannerDockEntry) -> some View {
        StepHairline()
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                if entries.count > 1 {
                    Menu {
                        ForEach(entries) { candidate in
                            Button(candidate.title) { selectedTaskId = candidate.taskId }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Text(entry.title).lineLimit(1)
                            Image(systemName: "chevron.up.chevron.down").font(.system(size: 9))
                        }
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(theme.textColor)
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                            .fill(theme.controlColor))
                        .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                            .strokeBorder(theme.controlBorderColor, lineWidth: 1))
                    }
                    .accessibilityLabel("Planner task needing attention: \(entry.title)")
                } else {
                    Text(entry.title).font(.themed(13, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(1)
                }
                Spacer(minLength: 4)
                Text(entry.status?.label ?? "Planning")
                    .font(.themed(10, weight: .semibold))
                    .foregroundColor(theme.warningColor)
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                        .fill(theme.warningColor.opacity(0.14)))
            }
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(PlannerDockModel.headline(entry)).font(.themed(12, weight: .semibold)).foregroundColor(theme.textColor)
                Text(PlannerDockModel.detail(entry)).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
            }
            if entry.status == .eliciting, !entry.questions.isEmpty {
                VStack(spacing: 0) {
                    ForEach(Array(entry.questions.enumerated()), id: \.element.id) { index, question in
                        if index > 0 { StepHairline() }
                        questionRow(entry, question, index: index)
                    }
                }
            }
            actions(entry)
            if let notice = loader.notice {
                Text(notice).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    private func questionRow(_ entry: PlannerDockEntry, _ question: PlanQuestion, index: Int) -> some View {
        let replying = activeReply?.taskId == entry.taskId && activeReply?.questionId == question.id
        return HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("QUESTION \(index + 1) OF \(entry.questions.count)")
                    .font(.themed(9, weight: .bold))
                    .foregroundColor(theme.secondaryTextColor)
                Text(question.text).font(.themed(12)).foregroundColor(replying ? theme.infoColor : theme.textColor)
                    .lineLimit(2)
            }
            Spacer(minLength: 4)
            Button(replying ? "Replying" : "Reply") {
                onReply(entry, question)
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded = false }
            }
            .buttonStyle(ChipStyle(prominent: replying))
            .disabled(disabled)
            .accessibilityLabel(replying ? "Replying to question \(index + 1)" : "Reply to question \(index + 1)")
        }
        .padding(.vertical, 4)
    }

    private func actions(_ entry: PlannerDockEntry) -> some View {
        ComposerAskFlow(spacing: 6) {
            Button(entry.status == .draft ? "Review plan" : "Open plan") { onOpenPlan(entry) }
                .buttonStyle(ChipStyle())
                .disabled(disabled)
            if entry.status == .draft {
                actionButton("Approve", .approve, entry)
                actionButton("Approve & Run", .approveAndRun, entry, prominent: true)
                actionButton("Replan", .replan, entry)
                actionButton("Reject", .reject, entry)
            }
        }
    }

    private func actionButton(_ label: String, _ action: PlannerDockAction, _ entry: PlannerDockEntry,
                              prominent: Bool = false) -> some View {
        Button {
            Task { await loader.perform(action, on: entry) }
        } label: {
            HStack(spacing: 4) {
                if loader.isBusy(entry.taskId, action) { ProgressView().controlSize(.mini) }
                Text(label)
            }
        }
        .buttonStyle(ChipStyle(prominent: prominent))
        .disabled(disabled || loader.isBusy(entry.taskId, action))
        .accessibilityIdentifier("planner-dock-\(action.rawValue)")
    }
}
