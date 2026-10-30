import SwiftUI

/// Work → More → Goals: the open goal trees and the Closed list from the notes
/// under `Workspaces/*/Goals/`, at parity with the web's `GoalsWorkspace`. A goal
/// is marked done or reopened here; other changes go through chat or its note.
@MainActor
final class WorkGoalsModel: ObservableObject {
    /// `tree` holds only open goals; done and dropped goals are listed in `closed`.
    struct Shown: Identifiable {
        let space: WorkGoalWorkspace
        let tree: WorkGoalTree
        let closed: [WorkGoal]
        var id: String { space.path }
    }
    @Published private(set) var workspaces: [Shown] = []
    @Published private(set) var loading = true
    @Published private(set) var error: String?
    /// The outcome of the last Mark done or Reopen; cleared by a refresh.
    @Published private(set) var notice: String?
    /// Goals whose Mark done or Reopen is being saved, by note path.
    @Published private(set) var saving: Set<String> = []
    /// Folded goal nodes; kept across a reload, like the web.
    @Published private(set) var collapse = WorkGoalCollapse()
    private(set) var started = false
    let limits: WorkGoalReadLimits
    private var generation = 0
    private var task: Task<Void, Never>?
    private let clientFactory: () throws -> WorkAPIClient

    init(limits: WorkGoalReadLimits = .standard, clientFactory: @escaping () throws -> WorkAPIClient = { try .connected() }) {
        self.limits = limits; self.clientFactory = clientFactory
    }
    deinit { task?.cancel() }

    var openCount: Int { workspaces.reduce(0) { $0 + $1.space.goals.count } - closedCount }
    var closedCount: Int { workspaces.reduce(0) { $0 + $1.closed.count } }
    var canFold: Bool { !WorkGoalCollapse.collapsible(workspaces.map(\.tree)).isEmpty }
    func toggle(_ path: String) { collapse.toggle(path) }
    func expandAll() { collapse.expandAll() }
    func collapseAll() { collapse.collapseAll(workspaces.map(\.tree)) }

    /// An explicit refresh also clears the last notice.
    func refresh() { notice = nil; reload() }

    /// Mark done moves the goal under Closed; Reopen brings it back as active. A save or a
    /// conflict reloads, since closing or reopening a goal can move the open goals under it.
    func setStatus(_ goal: WorkGoal, to status: WorkGoalStatusChange) {
        guard !saving.contains(goal.path) else { return }
        notice = nil
        let client: WorkAPIClient
        do { client = try clientFactory() } catch { notice = WorkGoalCopy.statusNotice(goal.title, status, .failed(error.localizedDescription)); return }
        saving.insert(goal.path)
        Task { [weak self] in
            let result = try? await WorkGoalWriter.updateStatus(goal, to: status) { path, method, body in
                try await client.status(path, method: method, body: body)
            }
            guard let self else { return }
            self.saving.remove(goal.path)
            guard let result, client.isCurrentConnection else { return }
            self.notice = WorkGoalCopy.statusNotice(goal.title, status, result)
            if case .failed = result { return }
            self.reload()
        }
    }

    /// A newer load, or a connection switch, discards an older answer.
    func reload() {
        started = true
        task?.cancel(); generation += 1
        let ticket = generation, limits = limits
        workspaces = []; error = nil; loading = true
        let client: WorkAPIClient
        do { client = try clientFactory() } catch { self.error = error.localizedDescription; loading = false; return }
        task = Task { [weak self] in
            do {
                let result = try await WorkGoalReader.load(limits: limits) { path in try await client.status(path) }
                guard let self, ticket == self.generation, !Task.isCancelled, client.isCurrentConnection else { return }
                self.workspaces = result.map { Shown(space: $0, tree: workOpenGoalTree(workBuildGoalTree($0.goals)), closed: workClosedGoals($0.goals)) }
                self.loading = false
            } catch {
                guard let self, ticket == self.generation, !Task.isCancelled, client.isCurrentConnection else { return }
                self.error = error.localizedDescription
                self.loading = false
            }
        }
    }
}

/// A note opened from a goal row.
private struct WorkGoalNoteLink: Identifiable {
    let path: String
    var id: String { path }
}

struct WorkGoalsView: View {
    @StateObject private var model = WorkGoalsModel()
    @State private var openNote: WorkGoalNoteLink?
    /// Open or Closed; the All work landing's links set it before opening this screen.
    @AppStorage(WorkGoalList.storageKey) private var list = WorkGoalList.open
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        WorkPage {
            WorkHeaderCard("Goals", detail: WorkGoalCopy.intro,
                           chips: model.loading || model.error != nil ? [] : [WorkChipSpec(title: WorkGoalCopy.openGoalCount(model.openCount), systemImage: "target", tone: theme.accentColor)])
            Picker("Open or closed goals", selection: $list) {
                Text("Open \(model.openCount)").tag(WorkGoalList.open)
                Text("Closed \(model.closedCount)").tag(WorkGoalList.closed)
            }
            .pickerStyle(.segmented)
            .accessibilityIdentifier("work-goals-list")
            if let notice = model.notice {
                WorkGroupCard { WorkNote(notice, tone: theme.textColor) }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("work-goals-notice")
            }
            if let error = model.error {
                WorkGroupCard {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Image(systemName: "exclamationmark.triangle.fill").font(.themed(15, weight: .semibold))
                            .foregroundColor(theme.dangerColor).accessibilityHidden(true)
                        Text(error).font(.themed(15)).foregroundColor(theme.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.vertical, 12)
                    WorkRowDivider()
                    WorkButtonRow(title: "Retry", systemImage: "arrow.clockwise") { model.refresh() }
                }
                .accessibilityIdentifier("work-goals-error")
            } else if model.loading {
                HStack(spacing: 10) {
                    ProgressView().tint(theme.accentColor)
                    Text(WorkGoalCopy.loading).font(.themed(15)).foregroundColor(theme.secondaryTextColor)
                }
                .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight)
                .accessibilityElement(children: .combine)
            } else if model.workspaces.isEmpty {
                WorkGroupCard {
                    WorkNote("\(WorkGoalCopy.emptyLead) \(WorkGoalCopy.emptyLink) \(WorkGoalCopy.emptyTail)")
                    WorkRowDivider()
                    WorkButtonRow(title: WorkGoalCopy.emptyLink, systemImage: "bubble.left.and.bubble.right", accessory: .chevron) {
                        AppActions.shared.requestThread(nil)
                    }
                }
                .accessibilityIdentifier("work-goals-empty")
            } else if list == .closed {
                let spaces = model.workspaces.filter(\.space.showsInClosedList)
                if spaces.isEmpty {
                    WorkGroupCard { WorkNote(WorkGoalCopy.noClosedGoalsAnywhere) }
                        .accessibilityIdentifier("work-goals-closed-empty")
                }
                ForEach(spaces) { shown in
                    WorkGoalClosedSection(shown: shown, saving: model.saving, onReopen: { model.setStatus($0, to: .active) }) { openNote = WorkGoalNoteLink(path: $0) }
                }
            } else {
                let spaces = model.workspaces.filter(\.space.showsInOpenList)
                if spaces.isEmpty {
                    WorkGroupCard {
                        WorkNote(WorkGoalCopy.noOpenGoalsAnywhere)
                        WorkRowDivider()
                        WorkButtonRow(title: WorkGoalCopy.seeClosed, systemImage: "checkmark.circle", accessory: .chevron) { list = .closed }
                    }
                    .accessibilityIdentifier("work-goals-open-empty")
                }
                ForEach(spaces) { shown in
                    WorkGoalWorkspaceSection(shown: shown, limits: model.limits, collapse: model.collapse, saving: model.saving, onToggle: model.toggle,
                                             onMarkDone: { model.setStatus($0, to: .done) }) { openNote = WorkGoalNoteLink(path: $0) }
                }
            }
        }
        .toolbar {
            Button("Refresh goals", systemImage: "arrow.clockwise") { model.refresh() }.disabled(model.loading)
            Menu("Goal tree", systemImage: "ellipsis.circle") {
                Button("Expand all", systemImage: "chevron.down") { withAnimation(.snappy) { model.expandAll() } }
                Button("Collapse all", systemImage: "chevron.up") { withAnimation(.snappy) { model.collapseAll() } }
            }
            .disabled(model.loading || list == .closed || !model.canFold)
            .accessibilityIdentifier("work-goals-tree-menu")
        }
        .task { if !model.started { model.reload() } }
        .refreshable { model.refresh() }
        .onChange(of: model.notice) { _, notice in
            if let notice { UIAccessibility.post(notification: .announcement, argument: notice) }
        }
        .fullScreenCover(item: $openNote) { NotesBrowserView(initialPath: $0.path) }
    }
}

/// A workspace's name and the notes or folders in it that could not be read.
private struct WorkGoalSpaceHeading: View {
    let space: WorkGoalWorkspace
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        Text(space.name).font(.themed(20, weight: .semibold)).foregroundColor(theme.textColor)
            .fixedSize(horizontal: false, vertical: true).accessibilityAddTraits(.isHeader)
        if !space.problems.isEmpty {
            DisclosureGroup(WorkGoalCopy.problems(space.problems.count)) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(space.problems.enumerated()), id: \.offset) { _, problem in
                        Text(problem).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                    }
                }
            }
            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "exclamationmark.triangle"))
            .tint(theme.accentColor)
        }
    }
}

private struct WorkGoalWorkspaceSection: View {
    let shown: WorkGoalsModel.Shown
    let limits: WorkGoalReadLimits
    let collapse: WorkGoalCollapse
    let saving: Set<String>
    let onToggle: (String) -> Void
    let onMarkDone: (WorkGoal) -> Void
    let onOpenNote: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        let space = shown.space, tree = shown.tree
        VStack(alignment: .leading, spacing: 10) {
            WorkGoalSpaceHeading(space: space)
            if space.notShown > 0 { muted(WorkGoalCopy.notShown(space.notShown, limits: limits)) }
            if space.foldersNotRead > 0 { muted(WorkGoalCopy.foldersNotRead(space.foldersNotRead, limits: limits)) }
            if !tree.roots.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(tree.roots, id: \.goal.path) { WorkGoalRow(node: $0, goals: tree.byKey, collapse: collapse, saving: saving, onToggle: onToggle, onMarkDone: onMarkDone, onOpenNote: onOpenNote) }
                }
            } else if tree.orphans.isEmpty {
                muted(shown.closed.isEmpty ? WorkGoalCopy.noGoals : WorkGoalCopy.noOpenGoals)
            }
            if !tree.orphans.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text(WorkGoalCopy.notLinked).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor)
                        .accessibilityAddTraits(.isHeader)
                    muted(WorkGoalCopy.notLinkedDetail)
                    // A goal lifted from under a closed one says so instead of why its tree is not linked.
                    ForEach(tree.orphans, id: \.goal.path) { orphan in
                        WorkGoalRow(node: orphan.node, goals: tree.byKey, note: orphan.node.closedParent == nil ? workGoalOrphanNote(orphan) : nil,
                                    collapse: collapse, saving: saving, onToggle: onToggle, onMarkDone: onMarkDone, onOpenNote: onOpenNote)
                    }
                }
                .padding(.top, 6)
                .accessibilityElement(children: .contain)
                .accessibilityLabel("\(space.name) goals not linked")
            }
        }
        .padding(.top, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(space.name) goals")
    }
    private func muted(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A workspace's closed goals, most recently closed first, each with Reopen.
private struct WorkGoalClosedSection: View {
    let shown: WorkGoalsModel.Shown
    let saving: Set<String>
    let onReopen: (WorkGoal) -> Void
    let onOpenNote: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            WorkGoalSpaceHeading(space: shown.space)
            if shown.closed.isEmpty {
                Text(WorkGoalCopy.noClosedGoals).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(shown.closed, id: \.path) { row($0) }
                }
            }
        }
        .padding(.top, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(shown.space.name) closed goals")
    }

    private func row(_ goal: WorkGoal) -> some View {
        let isSaving = saving.contains(goal.path)
        return VStack(alignment: .leading, spacing: 8) {
            Button { onOpenNote(goal.path) } label: {
                Text(goal.title).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true).multilineTextAlignment(.leading)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens the goal note")
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) { details(goal) }
                VStack(alignment: .leading, spacing: 4) { details(goal) }
            }
            .font(.themed(13))
            Button { onReopen(goal) } label: { Text(isSaving ? WorkGoalCopy.saving : WorkGoalCopy.reopen) }
                .buttonStyle(.plain).foregroundColor(theme.accentColor)
                .font(.themed(14, weight: .medium))
                .frame(minHeight: 32)
                .disabled(isSaving)
                .accessibilityLabel("Reopen \(goal.title)")
                .accessibilityIdentifier("work-goal-reopen-\(goal.name)")
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .workCardSurface()
        .accessibilityElement(children: .contain)
        .accessibilityLabel(goal.title)
    }

    @ViewBuilder private func details(_ goal: WorkGoal) -> some View {
        WorkGoalStatusChip(status: goal.status)
        if let parent = workGoalParentTitle(goal, in: shown.tree) {
            Text(WorkGoalCopy.under(parent)).foregroundColor(theme.secondaryTextColor).fixedSize(horizontal: false, vertical: true)
        }
        if let closed = goal.closed { Text(WorkGoalCopy.closedOn(closed)).foregroundColor(theme.secondaryTextColor) }
    }
}

/// One goal card and, nested under it, its sub-goals (8 levels; deeper ones are counted).
private struct WorkGoalRow: View {
    let node: WorkGoalNode
    let goals: [String: WorkGoal]
    var depth = 0
    /// Why this goal heads a "Not linked" tree.
    var note: String? = nil
    let collapse: WorkGoalCollapse
    /// Note paths whose Mark done is being saved.
    let saving: Set<String>
    let onToggle: (String) -> Void
    let onMarkDone: (WorkGoal) -> Void
    let onOpenNote: (String) -> Void
    @ObservedObject private var theme = ThemeManager.shared

    private var hasChildren: Bool { !node.children.isEmpty }
    private var isCollapsed: Bool { collapse.isCollapsed(node) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            card
            if hasChildren && !isCollapsed {
                if depth + 1 < WorkGoalCopy.maxDepth {
                    HStack(alignment: .top, spacing: 0) {
                        Rectangle().fill(theme.cardBorderColor).frame(width: 1).accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(node.children, id: \.goal.path) { child in
                                WorkGoalRow(node: child, goals: goals, depth: depth + 1, collapse: collapse, saving: saving, onToggle: onToggle, onMarkDone: onMarkDone, onOpenNote: onOpenNote)
                            }
                        }
                        .padding(.leading, 8)
                    }
                    .padding(.leading, 8)
                } else {
                    Text(WorkGoalCopy.deeper(node.children.count)).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true).padding(.leading, 16)
                }
            }
        }
    }

    private var goal: WorkGoal { node.goal }

    private var card: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 6) {
                if hasChildren { toggleButton }
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { title; titleChips }
                    VStack(alignment: .leading, spacing: 6) { title; HStack(spacing: 6) { titleChips } }
                }
            }
            if let note { secondary(note) }
            if let closedParent = node.closedParent { secondary(WorkGoalCopy.closedParent(closedParent)) }
            HStack(spacing: 6) {
                WorkGoalStatusChip(status: goal.status)
                WorkChip(title: goal.visibility.rawValue, tone: theme.secondaryTextColor)
                    .help(goal.visibility.meaning)
                    .accessibilityHint(goal.visibility.meaning)
            }
            if goal.owner != nil || goal.target != nil {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 12) { owner; target }
                    VStack(alignment: .leading, spacing: 4) { owner; target }
                }
                .font(.themed(13))
            }
            if !goal.peers.isEmpty { peers }
            if !goal.metrics.isEmpty { secondary("Metrics: \(goal.metrics.joined(separator: ", "))") }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 16) { links }
                VStack(alignment: .leading, spacing: 4) { links }
            }
            .font(.themed(14, weight: .medium))
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .workCardSurface()
        .accessibilityElement(children: .contain)
        .accessibilityLabel(goal.title)
    }

    private var title: some View {
        Text(goal.title).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
            .fixedSize(horizontal: false, vertical: true)
    }
    private var peerChip: some View { WorkChip(title: "Peer goal", tone: theme.textColor) }

    @ViewBuilder private var titleChips: some View {
        if goal.kind == .peer { peerChip }
        // Folded: the count stands in for the hidden sub-goals and unfolds them.
        if isCollapsed {
            Button { toggle() } label: { WorkChip(title: WorkGoalCopy.subGoals(of: node), tone: theme.accentColor) }
                .buttonStyle(.plain)
                .accessibilityLabel(WorkGoalCopy.expandLabel(goal.title))
        }
    }

    private var toggleButton: some View {
        Button { toggle() } label: {
            Image(systemName: "chevron.down")
                .font(.themed(12, weight: .bold))
                .foregroundColor(theme.secondaryTextColor)
                .rotationEffect(.degrees(isCollapsed ? -90 : 0))
                .frame(width: 26, height: 26)
                .background(theme.controlColor, in: RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
                .frame(minWidth: 32, minHeight: 32, alignment: .topLeading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(isCollapsed ? WorkGoalCopy.expandLabel(goal.title) : WorkGoalCopy.collapseLabel(goal.title))
        .accessibilityValue(isCollapsed ? "Collapsed" : "Expanded")
        .accessibilityIdentifier("work-goal-toggle-\(goal.name)")
    }

    private func toggle() { withAnimation(.snappy) { onToggle(goal.path) } }

    @ViewBuilder private var owner: some View {
        if goal.owner == "user" {
            Text("Owner: you").foregroundColor(theme.secondaryTextColor)
        } else if let agent = goal.owner {
            HStack(spacing: 4) {
                Text("Owner:").foregroundColor(theme.secondaryTextColor)
                NavigationLink { WorkCrewDetail(agentID: agent) } label: { Text(agent).underline() }
                    .buttonStyle(.plain).foregroundColor(theme.accentColor)
                    .accessibilityLabel(agent)
            }
        }
    }
    @ViewBuilder private var target: some View {
        if let target = goal.target { Text("Target \(target)").foregroundColor(theme.secondaryTextColor) }
    }

    private var peers: some View {
        let rows = goal.peers.map { (name: $0, goal: goals[workGoalKey($0)]) }
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) { peerItems(rows) }
            VStack(alignment: .leading, spacing: 4) { peerItems(rows) }
        }
        .font(.themed(13))
    }
    @ViewBuilder private func peerItems(_ rows: [(name: String, goal: WorkGoal?)]) -> some View {
        Text("Peers").foregroundColor(theme.secondaryTextColor)
        ForEach(Array(rows.enumerated()), id: \.offset) { _, peer in
            if let found = peer.goal {
                Button { onOpenNote(found.path) } label: { Text(found.title).underline() }
                    .buttonStyle(.plain).foregroundColor(theme.accentColor)
            } else {
                Text(peer.name).foregroundColor(theme.textColor)
            }
        }
    }

    @ViewBuilder private var links: some View {
        if let program = goal.program {
            NavigationLink { WorkProgramDetail(name: workGoalProgramName(program)) } label: { Text("Program: \(program) →") }
                .buttonStyle(.plain).foregroundColor(theme.accentColor)
                .frame(minHeight: 32)
        }
        Button { onOpenNote(goal.path) } label: { Text("Note →") }
            .buttonStyle(.plain).foregroundColor(theme.accentColor)
            .frame(minHeight: 32)
        let isSaving = saving.contains(goal.path)
        Button { onMarkDone(goal) } label: { Text(isSaving ? WorkGoalCopy.saving : WorkGoalCopy.markDone) }
            .buttonStyle(.plain).foregroundColor(theme.accentColor)
            .frame(minHeight: 32)
            .disabled(isSaving)
            .accessibilityLabel("Mark \(goal.title) done")
            .accessibilityIdentifier("work-goal-mark-done-\(goal.name)")
    }

    private func secondary(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A goal's status: active in the accent, blocked in danger, done in success,
/// dropped struck through. Shared by the Goals screen and the All work summary.
struct WorkGoalStatusChip: View {
    let status: String
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        let tone: Color
        switch status {
        case "active": tone = theme.accentColor
        case "blocked": tone = theme.dangerColor
        case "done": tone = theme.successColor
        case "dropped": tone = theme.secondaryTextColor
        default: tone = theme.textColor
        }
        return Text(status).strikethrough(status == "dropped")
            .font(.themed(12, weight: .semibold)).foregroundColor(tone)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tone.opacity(0.12), in: RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).strokeBorder(tone.opacity(0.35), lineWidth: 1))
    }
}
