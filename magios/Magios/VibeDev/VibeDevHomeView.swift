import SwiftUI

/// A sheet any VibeDev screen can ask for.
struct VibeDevSheet: Identifiable {
    enum Kind {
        case newRun(VibeDevNewRunSeed)
        case newProject
        case steer(liveTaskID: String, name: String)
    }
    let id = UUID()
    let kind: Kind
}

/// What the New run sheet opens with.
struct VibeDevNewRunSeed {
    var projectID: String?
    /// A follow-up threads onto this run.
    var followUp: (runID: String, parentTaskID: String)?
    /// Open with the VibeDev crew switch on.
    var crew = false
}

/// The stack and the sheet, shared by every VibeDev screen.
@MainActor
final class VibeDevNavigator: ObservableObject {
    @Published var path: [VibeDevRoute] = []
    @Published var sheet: VibeDevSheet?
    /// Bumped after a write from a sheet so the visible screen re-reads.
    @Published private(set) var revision = 0

    func open(_ route: VibeDevRoute) { path.append(route) }
    func present(_ kind: VibeDevSheet.Kind) { sheet = VibeDevSheet(kind: kind) }
    func changed() { revision += 1 }
}

/// The VibeDev surface from the left menu: its own navigation stack over Now
/// and Projects, with the undo toast and the sheets above every screen.
struct VibeDevHomeView: View {
    @StateObject private var context = VibeDevContext()
    @StateObject private var navigator = VibeDevNavigator()
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack(path: $navigator.path) {
            VibeDevNowScreen(context: context, close: { dismiss() })
                .navigationDestination(for: VibeDevRoute.self) { route in
                    switch route {
                    case .project(let id):
                        VibeDevProjectView(projectID: id, context: context)
                    case .run(let id, let tab):
                        VibeDevRunView(runID: id, tab: tab, context: context)
                    case .diff(let target):
                        VibeDevDiffView(target: target, context: context)
                    }
                }
        }
        .environmentObject(navigator)
        .tint(theme.accentColor)
        .overlay(alignment: .bottom) {
            VibeDevUndoToast(center: context.undo)
                .padding(.bottom, 74)
                .animation(.easeOut(duration: 0.22), value: context.undo.toast?.id)
        }
        .sheet(item: $navigator.sheet) { sheet in
            sheetContent(sheet)
                .presentationDragIndicator(.visible)
                .presentationBackground(theme.cardColor)
        }
        .onDisappear { context.undo.flush() }
    }

    @ViewBuilder
    private func sheetContent(_ sheet: VibeDevSheet) -> some View {
        switch sheet.kind {
        case .newRun(let seed):
            VibeDevNewRunSheet(context: context, seed: seed) { outcome in
                navigator.sheet = nil
                navigator.changed()
                if case .run(let id) = outcome, seed.followUp == nil {
                    navigator.open(.run(id, .turns))
                }
            }
            .presentationDetents([.large])
        case .newProject:
            VibeDevNewProjectSheet(context: context) { project in
                navigator.sheet = nil
                navigator.changed()
                navigator.open(.project(project.projectId))
            }
            .presentationDetents([.large])
        case .steer(let liveTaskID, let name):
            VibeDevSteerSheet(context: context, liveTaskID: liveTaskID, name: name) {
                navigator.sheet = nil
                navigator.changed()
            }
            .presentationDetents([.height(250), .medium])
        }
    }
}

/// Now / Projects.
struct VibeDevNowScreen: View {
    enum Segment: String, CaseIterable { case now = "Now", projects = "Projects" }

    let close: () -> Void
    @StateObject private var model: VibeDevNowModel
    @EnvironmentObject private var navigator: VibeDevNavigator
    @ObservedObject private var theme = ThemeManager.shared
    @State private var segment: Segment = .now
    @Environment(\.scenePhase) private var scenePhase

    init(context: VibeDevContext, close: @escaping () -> Void) {
        self.close = close
        _model = StateObject(wrappedValue: VibeDevNowModel(context: context))
    }

    var body: some View {
        VStack(spacing: 0) {
            Text("VibeDev")
                .font(.themedDisplay(30, weight: .bold))
                .foregroundStyle(theme.textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
                .accessibilityAddTraits(.isHeader)
            VibeDevSegmented(options: Segment.allCases.map { ($0, $0.rawValue) }, selection: $segment)
                .padding(.horizontal, 16)
                .padding(.bottom, 6)
            list
        }
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button(action: close) { Image(systemName: "xmark") }
                    .accessibilityLabel("Close VibeDev")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button { navigator.present(.newProject) } label: { Image(systemName: "plus") }
                    .accessibilityLabel("New project")
            }
        }
        .safeAreaInset(edge: .bottom) {
            VibeDevBottomBar {
                Button("New run") {
                    navigator.present(.newRun(VibeDevNewRunSeed(projectID: model.visibleProjects.first?.projectId)))
                }
                .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                .disabled(model.visibleProjects.isEmpty)
            }
        }
        .task { await model.refresh() }
        .onAppear { model.startLive() }
        .onDisappear { model.stopLive() }
        .onChange(of: navigator.revision) { _, _ in Task { await model.refresh() } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await model.refresh() } }
        }
    }

    private func row<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .listRowInsets(EdgeInsets(top: 5, leading: 16, bottom: 5, trailing: 16))
            .listRowSeparator(.hidden)
            .listRowBackground(theme.backgroundColor)
    }

    private var list: some View {
        List {
            if let error = model.errorMessage, model.liveRuns.isEmpty, model.projects.isEmpty {
                row { VibeDevEmpty(text: error) }
            }
            row { VibeDevStaleNote(lastUpdated: model.lastUpdated, failed: model.refreshFailed) }
            switch segment {
            case .now: nowRows
            case .projects: projectRows
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(theme.backgroundColor)
        .refreshable { await model.refresh() }
        .overlay {
            if !model.loaded { ProgressView().tint(theme.accentColor) }
        }
    }

    @ViewBuilder
    private var nowRows: some View {
        let asks = model.needsYou
        row { VibeDevSectionLabel(title: "Needs you", count: asks.count) }
        if asks.isEmpty, model.loaded {
            row { VibeDevEmpty(text: "Nothing needs you right now.") }
        }
        ForEach(asks) { item in
            row {
                VibeDevAskCard(
                    item: item,
                    onOption: { option in model.perform(option, of: item, open: { navigator.open($0) }) },
                    onOpen: {
                        if let run = item.run { navigator.open(.run(run.runId, .changes)) }
                        else { navigator.open(.project(item.projectID)) }
                    },
                    onOpenRun: { navigator.open(.run($0, .turns)) })
            }
        }
        let crews = model.runningCrews
        if !crews.isEmpty {
            row { VibeDevSectionLabel(title: "VibeDev crews") }
            ForEach(crews) { entry in
                row {
                    VibeDevCrewCard(crew: entry.crew, projectName: entry.project.name,
                                    onOpenRun: { navigator.open(.run($0, .turns)) })
                        .contentShape(Rectangle())
                        .onTapGesture { navigator.open(.project(entry.project.projectId)) }
                }
            }
        }
        let working = model.working
        row { VibeDevSectionLabel(title: "Working", count: working.count) }
        if working.isEmpty, model.loaded {
            row { VibeDevEmpty(text: "No run is going.") }
        }
        ForEach(working) { run in
            row {
                VibeDevLiveRunRow(run: run, stopping: model.isStopping(run),
                                  actions: model.rowActions(for: run),
                                  onAction: { perform($0, on: run) },
                                  onOpen: { navigator.open(.run(run.runId, .turns)) })
                    .contentShape(Rectangle())
                    .onTapGesture { navigator.open(.run(run.runId, .turns)) }
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                ForEach(model.rowActions(for: run).reversed(), id: \.self) { action in
                    Button(action == .stop ? "Stop" : "Steer", role: action == .stop ? .destructive : nil) {
                        perform(action, on: run)
                    }
                    .tint(action == .stop ? theme.dangerColor : theme.accentFillColor)
                }
            }
        }
        if !working.isEmpty {
            row {
                Text("Swipe a run left to steer or stop")
                    .font(.themed(11))
                    .foregroundStyle(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity)
            }
        }
        let finished = model.justFinished
        if !finished.isEmpty {
            row { VibeDevSectionLabel(title: "Just finished") }
            ForEach(finished) { run in
                row {
                    VibeDevLiveRunRow(run: run, stopping: false, actions: [], onAction: { _ in },
                                      onOpen: { navigator.open(.run(run.runId, .changes)) })
                        .contentShape(Rectangle())
                        .onTapGesture { navigator.open(.run(run.runId, .changes)) }
                }
            }
        }
    }

    @ViewBuilder
    private var projectRows: some View {
        if model.visibleProjects.isEmpty, model.loaded {
            row { VibeDevEmpty(text: "No projects yet. Tap + to add one.") }
        }
        ForEach(model.visibleProjects) { project in
            row {
                Button { navigator.open(.project(project.projectId)) } label: {
                    VibeDevProjectCard(project: project, liveCount: model.liveCount(for: project.projectId))
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func perform(_ action: VibeDevRowAction, on run: VibeDevLiveRun) {
        switch action {
        case .steer: navigator.present(.steer(liveTaskID: run.liveTaskId, name: run.shortName))
        case .stop: model.stop(run)
        }
    }
}

/// One live run: state dot, title, lane · project · step, and age.
struct VibeDevLiveRunRow: View {
    let run: VibeDevLiveRun
    let stopping: Bool
    let actions: [VibeDevRowAction]
    let onAction: (VibeDevRowAction) -> Void
    var onOpen: () -> Void = {}
    @ObservedObject private var theme = ThemeManager.shared

    private var subtitle: String {
        var parts: [String] = []
        if let lane = run.lane { parts.append("\(lane) lane") }
        parts.append(run.projectName)
        if stopping { parts.append("Stopping…") }
        else if let line = run.state.isEnded ? (run.summary ?? run.state.label) : (run.step ?? run.latestTurnTitle) {
            parts.append(line)
        }
        return parts.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    var body: some View {
        HStack(spacing: 10) {
            VibeDevStateDot(run: stopping ? .stopped : run.state)
            VStack(alignment: .leading, spacing: 2) {
                Text(run.displayTitle)
                    .font(.themedDisplay(14, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .vibeDevLineLimit(1)
                Text(subtitle)
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
                    .vibeDevLineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(run.rowTime)
                .font(.themedMono(11, weight: .medium))
                .foregroundStyle(theme.secondaryTextColor)
            if !actions.isEmpty {
                Menu {
                    ForEach(actions, id: \.self) { action in
                        Button(role: action == .stop ? .destructive : nil) { onAction(action) } label: {
                            Label(action == .stop ? "Stop" : "Steer",
                                  systemImage: action == .stop ? "stop.circle" : "arrow.turn.down.right")
                        }
                    }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(theme.secondaryTextColor)
                        .frame(width: Lively.tapMin, height: Lively.tapMin)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Steer or stop")
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 11)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
            .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(run.displayTitle), \(stopping ? "Stopping" : run.state.label)")
        .accessibilityValue("\(subtitle), \(run.rowTime)")
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { onOpen() }
        .accessibilityAction(named: "Open") { onOpen() }
        .accessibilityActions {
            ForEach(actions, id: \.self) { action in
                Button(action == .stop ? "Stop" : "Steer") { onAction(action) }
            }
        }
    }
}

/// A project on the Projects segment.
struct VibeDevProjectCard: View {
    let project: VibeDevProject
    let liveCount: Int
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(project.name)
                    .font(.themedDisplay(15, weight: .semibold))
                    .foregroundStyle(theme.textColor)
                    .vibeDevLineLimit(1)
                Spacer()
                VibeDevTrustBadge(trusted: project.isTrusted)
            }
            HStack(spacing: 8) {
                Text(project.repoLabel)
                    .font(.themedMono(12))
                    .vibeDevLineLimit(1)
                    .truncationMode(.head)
                Text(liveCount > 0 ? "\(liveCount) running" : "idle")
                    .font(.themed(12))
            }
            .foregroundStyle(theme.secondaryTextColor)
        }
        .vibeDevCard()
        .contentShape(Rectangle())
    }
}

/// The theme's two-segment switch (Now / Projects, Turns / Changes).
struct VibeDevSegmented<Value: Hashable>: View {
    let options: [(Value, String)]
    @Binding var selection: Value
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                let selected = option.0 == selection
                Button { selection = option.0 } label: {
                    Text(option.1)
                        .font(.themed(13, weight: .semibold))
                        .foregroundStyle(selected ? theme.textColor : theme.secondaryTextColor)
                        .frame(maxWidth: .infinity, minHeight: Lively.tapMin - 6)
                        .padding(.vertical, 2)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(selected ? theme.cardColor : theme.cardColor.opacity(0))
                                .shadow(color: theme.textColor.opacity(selected ? 0.12 : 0), radius: 1, y: 1)
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
            }
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(theme.softBackgroundColor))
    }
}
