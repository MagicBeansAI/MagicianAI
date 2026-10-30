import SwiftUI

/// The All work landing's Goals and Playbooks summary (the web's compact
/// `GoalsWorkspace` and `ProgramsWorkspace`). It reads the goal notes under
/// `WorkLanding.goalReadLimits`, not the Goals screen's full walk, and loads
/// on appearance; switching the Tasks, Monitors and System work lanes keeps it.
/// Only open goals are listed and counted; done and dropped ones are under the
/// Goals screen's Closed list.
@MainActor
final class WorkLandingModel: ObservableObject {
    @Published private(set) var goals: [WorkGoalNode] = []
    /// Open goals read; `closedCount` counts the done and dropped ones.
    @Published private(set) var goalCount = 0
    @Published private(set) var closedCount = 0
    /// Notes or folders were left unread, so `goalCount` is a floor.
    @Published private(set) var goalsPartial = false
    @Published private(set) var goalsLoading = true
    @Published private(set) var goalsError: String?
    @Published private(set) var playbooks: [WorkProgramSummary] = []
    @Published private(set) var playbookCount = 0
    @Published private(set) var playbooksLoading = true
    @Published private(set) var playbooksError: String?
    private(set) var lastLoad: Date?
    private var generation = 0
    private var tasks: [Task<Void, Never>] = []
    private let clientFactory: () throws -> WorkAPIClient

    init(clientFactory: @escaping () throws -> WorkAPIClient = { try .connected() }) { self.clientFactory = clientFactory }
    deinit { tasks.forEach { $0.cancel() } }

    /// Once per appearance: coming straight back (closing a task) keeps what is shown.
    func appear(now: Date = Date()) { if WorkLanding.needsLoad(lastLoad: lastLoad, now: now) { reload() } }

    /// A newer load, or a connection switch, discards an older answer.
    func reload() {
        tasks.forEach { $0.cancel() }; tasks = []
        generation += 1; lastLoad = Date()
        let ticket = generation
        goalsLoading = true; goalsError = nil; playbooksLoading = true; playbooksError = nil
        let client: WorkAPIClient
        do { client = try clientFactory() } catch {
            goalsError = error.localizedDescription; playbooksError = error.localizedDescription
            goalsLoading = false; playbooksLoading = false; lastLoad = nil
            return
        }
        tasks.append(Task { [weak self] in
            do {
                let spaces = try await WorkGoalReader.load(limits: WorkLanding.goalReadLimits) { path in try await client.status(path) }
                guard let self, self.current(ticket, client) else { return }
                let open = WorkLanding.openGoals(spaces)
                self.goals = WorkLanding.goals(open)
                self.goalCount = open.reduce(0) { $0 + $1.goals.count }
                self.closedCount = spaces.reduce(0) { $0 + $1.goals.count } - self.goalCount
                self.goalsPartial = spaces.contains { $0.notShown > 0 || $0.foldersNotRead > 0 }
                self.goalsLoading = false
            } catch {
                guard let self, self.current(ticket, client) else { return }
                self.goalsError = error.localizedDescription; self.goalsLoading = false
            }
        })
        tasks.append(Task { [weak self] in
            do {
                let programs = try await client.programs()
                guard let self, self.current(ticket, client) else { return }
                self.playbooks = WorkLanding.playbooks(programs)
                self.playbookCount = programs.count
                self.playbooksLoading = false
            } catch {
                guard let self, self.current(ticket, client) else { return }
                self.playbooksError = error.localizedDescription; self.playbooksLoading = false
            }
        })
    }

    private func current(_ ticket: Int, _ client: WorkAPIClient) -> Bool {
        ticket == generation && !Task.isCancelled && client.isCurrentConnection
    }
}

private struct WorkLandingNote: Identifiable {
    let path: String
    var id: String { path }
}

/// Two stacked, foldable sections under the Work switcher. The fold state is
/// remembered. It has no scroll of its own: the All work page is one scroll.
struct WorkLandingSummary: View {
    @ObservedObject var model: WorkLandingModel
    var onOpenSection: ((WorkSection) -> Void)? = nil
    @AppStorage("work.landing.goals.open") private var goalsOpen = true
    @AppStorage("work.landing.playbooks.open") private var playbooksOpen = true
    @AppStorage(WorkGoalList.storageKey) private var goalList = WorkGoalList.open
    @State private var openNote: WorkLandingNote?
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
                header(title: "Goals", systemImage: "target",
                       count: model.goalsLoading || model.goalsError != nil ? nil : WorkLanding.countLabel(model.goalCount, partial: model.goalsPartial),
                       open: $goalsOpen)
                if goalsOpen { goalsBody }
                WorkRowDivider()
                header(title: "Playbooks", systemImage: "doc.text",
                       count: model.playbooksLoading || model.playbooksError != nil ? nil : "\(model.playbookCount)",
                       open: $playbooksOpen)
                if playbooksOpen { playbooksBody }
        }
        .padding(.horizontal, 12).padding(.vertical, 2)
        .workCardSurface()
        .padding(.horizontal, WorkMetrics.screenPadding)
        .padding(.bottom, 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Goals and playbooks")
        .accessibilityIdentifier("work-landing-summary")
        .fullScreenCover(item: $openNote) { NotesBrowserView(initialPath: $0.path) }
    }

    private func header(title: String, systemImage: String, count: String?, open: Binding<Bool>) -> some View {
        Button { withAnimation(.snappy) { open.wrappedValue.toggle() } } label: {
            HStack(spacing: 8) {
                Image(systemName: systemImage).font(.themed(14, weight: .semibold)).foregroundColor(theme.accentColor)
                    .frame(width: 20).accessibilityHidden(true)
                Text(title).font(.themed(15, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(1)
                if let count { WorkChip(title: count, tone: theme.secondaryTextColor) }
                Spacer(minLength: 8)
                Image(systemName: "chevron.down").font(.themed(12, weight: .bold)).foregroundColor(theme.secondaryTextColor)
                    .rotationEffect(.degrees(open.wrappedValue ? 0 : -90)).accessibilityHidden(true)
            }
            .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(count.map { "\(title), \($0)" } ?? title)
        .accessibilityValue(open.wrappedValue ? "Expanded" : "Collapsed")
        .accessibilityHint(open.wrappedValue ? "Collapse" : "Expand")
        .accessibilityAddTraits(.isHeader)
        .accessibilityIdentifier("work-landing-\(title.lowercased())")
    }

    @ViewBuilder private var goalsBody: some View {
        if let error = model.goalsError {
            status(error, retry: true)
        } else if model.goalsLoading {
            status(WorkGoalCopy.loading)
        } else if model.goals.isEmpty && model.closedCount > 0 {
            Text(WorkGoalCopy.allClosed).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 6)
            more(WorkGoalCopy.seeClosed, section: .goals) { goalList = .closed }
        } else if model.goals.isEmpty {
            Button { AppActions.shared.requestThread(nil) } label: {
                Text("\(WorkGoalCopy.emptyLead) Ask Magican in chat to set one up →")
                    .font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 6)
            }
            .buttonStyle(.plain)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(model.goals, id: \.goal.path) { goalRow($0) }
            }
            .padding(.bottom, 2)
        }
        more("All goals", section: .goals) { goalList = .open }
    }

    private func goalRow(_ node: WorkGoalNode) -> some View {
        let goal = node.goal
        return Button { openNote = WorkLandingNote(path: goal.path) } label: {
            VStack(alignment: .leading, spacing: 5) {
                Text(goal.title).font(.themed(15, weight: .medium)).foregroundColor(theme.textColor)
                    .lineLimit(2).multilineTextAlignment(.leading)
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 6) { goalChips(node) }
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) { WorkGoalStatusChip(status: goal.status); if let owner = goal.owner { WorkChip(title: WorkGoalCopy.ownerLabel(owner), systemImage: "person", tone: theme.secondaryTextColor) } }
                        HStack(spacing: 6) { trailingGoalChips(node) }
                    }
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .workInsetPanel()
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens the goal note")
    }

    @ViewBuilder private func goalChips(_ node: WorkGoalNode) -> some View {
        WorkGoalStatusChip(status: node.goal.status)
        if let owner = node.goal.owner { WorkChip(title: WorkGoalCopy.ownerLabel(owner), systemImage: "person", tone: theme.secondaryTextColor) }
        trailingGoalChips(node)
    }

    @ViewBuilder private func trailingGoalChips(_ node: WorkGoalNode) -> some View {
        if !node.children.isEmpty { WorkChip(title: WorkGoalCopy.subGoals(of: node), tone: theme.accentColor) }
        if let target = node.goal.target { WorkChip(title: target, systemImage: "calendar", tone: theme.secondaryTextColor) }
    }

    @ViewBuilder private var playbooksBody: some View {
        if let error = model.playbooksError {
            status(error, retry: true)
        } else if model.playbooksLoading {
            status("Loading playbooks…")
        } else if model.playbooks.isEmpty {
            Text(WorkPlaybookCopy.empty).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 6)
        } else {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(model.playbooks.enumerated()), id: \.element.id) { index, playbook in
                    if index > 0 { WorkRowDivider() }
                    NavigationLink { WorkProgramDetail(name: playbook.name) } label: {
                        HStack(spacing: 8) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(playbook.title).font(.themed(15, weight: .medium)).foregroundColor(theme.textColor)
                                    .lineLimit(2).multilineTextAlignment(.leading)
                                Text(playbook.name).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                            }
                            Spacer(minLength: 8)
                            Image(systemName: "chevron.right").font(.themed(12, weight: .semibold))
                                .foregroundColor(theme.secondaryTextColor.opacity(0.75)).accessibilityHidden(true)
                        }
                        .padding(.vertical, 8)
                        .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("work-landing-playbook-\(playbook.name)")
                }
            }
        }
        more("All playbooks", section: .programs)
    }

    /// `before` runs first, e.g. to pick the Goals screen's Open or Closed list.
    @ViewBuilder private func more(_ title: String, section: WorkSection, before: (() -> Void)? = nil) -> some View {
        if let onOpenSection {
            Button { before?(); onOpenSection(section) } label: {
                Text("\(title) →").font(.themed(14, weight: .semibold)).foregroundColor(theme.accentColor)
                    .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(title)
        }
    }

    private func status(_ text: String, retry: Bool = false) -> some View {
        HStack(spacing: 8) {
            if !retry { ProgressView().controlSize(.small).tint(theme.accentColor) }
            Text(text).font(.themed(13)).foregroundColor(retry ? theme.dangerColor : theme.secondaryTextColor)
                .lineLimit(3).frame(maxWidth: .infinity, alignment: .leading)
            if retry {
                Button("Retry") { model.reload() }.font(.themed(13, weight: .semibold)).tint(theme.accentColor)
            }
        }
        .padding(.vertical, 6)
    }
}
