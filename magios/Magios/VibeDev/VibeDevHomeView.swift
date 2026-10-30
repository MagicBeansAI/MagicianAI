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
    var codingRouteID: String? = nil
    var engine: String? = nil
    var mode: VibeDevTurnMode = .build
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

/// The mobile run history and its navigation stack, shared composer and sheets.
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
        .buttonBorderShape(.roundedRectangle(radius: 8))
        .overlay(alignment: .bottom) {
            VibeDevUndoToast(center: context.undo)
                .padding(.bottom, 74)
                .animation(.easeOut(duration: 0.22), value: context.undo.toast?.id)
        }
        .sheet(item: $navigator.sheet) { sheet in
            sheetContent(sheet)
                .presentationDragIndicator(.visible)
                .presentationBackground(theme.cardColor)
                .buttonBorderShape(.roundedRectangle(radius: 8))
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
                switch outcome {
                case .run(let id): if seed.followUp == nil { navigator.open(.run(id, .turns)) }
                case .crew(let id): navigator.open(.project(id))
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

/// Cross-project run history, newest activity first.
struct VibeDevNowScreen: View {

    let close: () -> Void
    @StateObject private var model: VibeDevNowModel
    @EnvironmentObject private var navigator: VibeDevNavigator
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage(CodingRunsActivity.enabledKey) private var showWorkActivity = true

    init(context: VibeDevContext, close: @escaping () -> Void) {
        self.close = close
        _model = StateObject(wrappedValue: VibeDevNowModel(context: context))
    }

    var body: some View {
        list
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle("VibeDev")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button(action: close) { toolbarIcon("xmark") }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Close VibeDev")
            }
            .roundedSquareToolbarBackground()
            ToolbarItem(placement: .topBarTrailing) {
                HStack(spacing: 0) {
                    Menu {
                        if model.visibleProjects.isEmpty { Text("No projects yet") }
                        ForEach(model.visibleProjects) { project in
                            Button(project.name) { navigator.open(.project(project.projectId)) }
                        }
                    } label: { toolbarIcon("folder") }
                    .accessibilityLabel("Projects")
                    Button { navigator.present(.newProject) } label: { toolbarIcon("plus") }
                        .accessibilityLabel("New project")
                    Menu {
                        Toggle("Show Magician at work", isOn: Binding(
                            get: { showWorkActivity },
                            set: { CodingRunsActivity.shared.setEnabled($0) }
                        ))
                        .accessibilityHint("Live Activity on the Lock Screen and Dynamic Island")
                    } label: { toolbarIcon("ellipsis") }
                    .accessibilityLabel("More VibeDev options")
                }.buttonStyle(.plain)
            }
            .roundedSquareToolbarBackground()
        }
        .safeAreaInset(edge: .bottom) {
            Button { navigator.present(.newRun(VibeDevNewRunSeed())) } label: {
                Label("New run", systemImage: "plus")
            }
            .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true))
            .padding(.horizontal, 16).padding(.vertical, 6).background(theme.backgroundColor)
        }
        .task { await model.refresh() }
        .onAppear { model.startLive() }
        .onDisappear { model.stopLive() }
        .onChange(of: navigator.revision) { _, _ in Task { await model.refresh() } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await model.refresh() } }
        }
    }

    private func toolbarIcon(_ symbol: String) -> some View {
        Image(systemName: symbol).font(.system(size: 16, weight: .medium))
            .foregroundStyle(theme.textColor).frame(width: 34, height: 34)
            .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(theme.controlBorderColor, lineWidth: 1))
            .padding(.horizontal, 2)
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
            historyRows
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
    private var historyRows: some View {
        if model.history.isEmpty, model.loaded {
            row { VibeDevEmpty(text: "No runs yet. Start a new run, or tap + to add a project.") }
        }
        ForEach(model.history) { run in
            row {
                VibeDevLiveRunRow(run: run, stopping: model.isStopping(run),
                    actions: model.rowActions(for: run), onAction: { perform($0, on: run) },
                    onOpen: { navigator.open(.run(run.runId, .turns)) })
                    .contentShape(Rectangle())
                    .onTapGesture { navigator.open(.run(run.runId, .turns)) }
                    .contextMenu {
                        Button("Open project") { navigator.open(.project(run.projectId)) }
                    }
            }
        }
        if model.nextCursor != nil {
            row {
                Button { Task { await model.loadMore() } } label: {
                    if model.loadingMore { ProgressView() } else { Text("Load older runs") }
                }
                .buttonStyle(VibeDevButtonStyle())
                .disabled(model.loadingMore)
                .frame(maxWidth: .infinity)
            }
        }
        if let error = model.errorMessage, !model.history.isEmpty {
            row { Text(error).font(.themed(12)).foregroundStyle(theme.dangerColor) }
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

    private var locationLabel: String? {
        run.workLocation.map { $0 == "original" ? "Original folder" : $0 == "trusted" ? "Isolated branch" : "Isolated workspace" }
    }

    private var repoLine: String {
        [run.repoPath.map { URL(fileURLWithPath: $0).lastPathComponent }, run.lane.map { "\($0) lane" }]
            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    private var subtitle: String {
        [run.projectName, locationLabel, repoLine]
            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
    }

    var body: some View {
        HStack(spacing: 10) {
            VibeDevStateDot(run: stopping ? .stopped : run.state, partlyDone: !stopping && run.isPartlyDone)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(run.displayTitle)
                        .font(.themedDisplay(14, weight: .semibold))
                        .foregroundStyle(theme.textColor)
                        .vibeDevLineLimit(1)
                    if let count = run.needsYouCount, count > 0 {
                        Text("\(count) needs you")
                            .font(.themed(10, weight: .semibold))
                            .foregroundStyle(theme.warningColor)
                            .padding(.horizontal, 5).padding(.vertical, 3)
                            .background(theme.warningColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 6))
                    }
                    if !stopping && run.isPartlyDone {
                        Text(run.stateLabel)
                            .font(.themed(11, weight: .bold))
                            .foregroundStyle(theme.warningColor)
                            .fixedSize()
                    }
                }
                HStack(spacing: 6) {
                    Text(run.projectName)
                        .font(.themed(12))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if let locationLabel {
                        VibeDevTag(text: locationLabel).fixedSize()
                    }
                }
                if !repoLine.isEmpty {
                    Text(repoLine)
                        .font(.themed(11))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(1)
                }
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
                .vibeDevControlOutline()
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
        .accessibilityIdentifier("vibedev-run-\(run.runId)")
        .accessibilityLabel("\(run.displayTitle), \(stopping ? "Stopping" : run.stateLabel)")
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

/// A themed segment switch for run detail tabs.
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
                        .frame(maxWidth: .infinity, minHeight: 32)
                        .background(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(selected ? theme.cardColor : theme.cardColor.opacity(0))
                                .shadow(color: theme.textColor.opacity(selected ? 0.12 : 0), radius: 1, y: 1)
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .vibeDevHitTarget()
                .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
            }
        }
        .padding(.horizontal, 3)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(theme.softBackgroundColor))
    }
}
