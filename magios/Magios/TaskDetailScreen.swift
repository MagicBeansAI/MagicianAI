import Combine
import SwiftUI
import UIKit

/// The task detail screen (design §3): a pushed, full-screen page with one
/// scroll — a header card (verdict line, title, brief, id and plan chips),
/// then the Output, Run and Plan act cards in `TaskActs.actOrder`, one open,
/// then Technical details. Lifecycle actions live in the ⋯ menu. A compact
/// title bar takes over once the header scrolls away.
///
/// Every entry point pushes a `TaskDetailRoute`: the Tasks list
/// (`taskDetailDestination`), notifications and deep links (through the
/// Tasks list), Attention's Review plan, and the chat (`taskDetailHost` on
/// the chat's `NavigationStack` root; rows such as `ActivityTimelineView`'s
/// Run button push through `@Environment(\.openTaskDetail)`).
struct TaskDetailScreen: View {
    let route: TaskDetailRoute
    /// Called after a lifecycle action lands, so a host list can reload.
    var onChanged: (() -> Void)? = nil

    @StateObject private var model: TaskDetailScreenModel
    @StateObject private var notePublisher = TaskNotePublishViewModel()
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var openAct: ActId?
    @State private var appliedDefault = false
    @State private var showCompactTitle = false
    @State private var visible = false
    @State private var confirmDelete = false
    @State private var scrollTarget: String?
    /// The Plan Inspector pushed from the Plan act's Plan graph chip.
    @State private var planInspectorRoute: PlanInspectorRoute?

    init(route: TaskDetailRoute, onChanged: (() -> Void)? = nil, session: URLSession = .shared) {
        self.route = route
        self.onChanged = onChanged
        _model = StateObject(wrappedValue: TaskDetailScreenModel(route: route, session: session))
    }

    private static let scrollSpace = "task-detail-scroll"

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if let snapshot = model.snapshot {
                        content(snapshot)
                    } else if model.isLoading || model.errorMessage == nil {
                        loading
                    } else {
                        failure(model.errorMessage ?? "Task details are unavailable.")
                    }
                }
                .padding(.horizontal, 16)
                .padding(.top, 8)
                .padding(.bottom, 32)
            }
            .coordinateSpace(name: Self.scrollSpace)
            .onPreferenceChange(HeaderBottomKey.self) { bottom in
                let show = TaskDetailLayout.compactTitleVisible(headerBottom: bottom)
                guard show != showCompactTitle else { return }
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.18)) { showCompactTitle = show }
            }
            .onChange(of: scrollTarget) { _, target in
                guard let target else { return }
                withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.25)) {
                    proxy.scrollTo(target, anchor: .top)
                }
                scrollTarget = nil
            }
        }
        .refreshable { await model.refresh() }
        .background(theme.washGradient.ignoresSafeArea())
        // Same chrome as chat: the theme's wash under a theme-coloured bar.
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .planInspectorHost($planInspectorRoute)
        .navigationTitle("Task")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { compactTitle }
            ToolbarItem(placement: .topBarTrailing) { followButton }
            ToolbarItem(placement: .topBarTrailing) { actionsMenu }
        }
        .overlay(alignment: .bottom) { toast }
        .task { await model.refresh() }
        .onAppear {
            visible = true
            model.startRealtime()
        }
        .onDisappear {
            visible = false
            model.stopRealtime()
        }
        .onChange(of: scenePhase) { _, phase in
            // Backgrounding stops the socket and the poll; coming back
            // catches up once, then resumes both.
            if phase == .active, visible {
                model.startRealtime()
                Task { await model.refresh() }
            } else if phase == .background {
                model.stopRealtime()
            }
        }
        .task(id: pollKey) { await pollWhileLive() }
        .onReceive(model.$snapshot) { snapshot in
            guard !appliedDefault, let snapshot else { return }
            appliedDefault = true
            openAct = TaskDetailLayout.initialOpenAct(
                snapshot, focus: route.focus, inspectRun: route.inspectRun, now: Date())
        }
        .confirmationDialog("Delete this task?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete task", role: .destructive) {
                Task {
                    if await model.perform(.delete) {
                        onChanged?()
                        dismiss()
                    }
                }
            }
            Button("Keep task", role: .cancel) {}
        } message: {
            Text("This removes the task record and cannot be undone.")
        }
        .alert("Task action failed", isPresented: Binding(
            get: { model.actionError != nil || notePublisher.errorMessage != nil },
            set: { if !$0 { model.actionError = nil; notePublisher.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { model.actionError = nil; notePublisher.errorMessage = nil }
        } message: {
            Text(model.actionError ?? notePublisher.errorMessage ?? "The task could not be updated.")
        }
        .onChange(of: notePublisher.successMessage) { _, message in
            if let message { model.notice = message }
        }
    }

    // MARK: - Live

    private var pollKey: String {
        let snapshot = model.snapshot
        let active = visible && scenePhase == .active
        return "\(active)|\(snapshot?.status ?? route.statusHint ?? "")|\(snapshot?.planStatus ?? "")"
    }

    /// Poll `/execution-panel` every 4 s while visible, foregrounded and
    /// planning or running. The key changes with any of those, which cancels
    /// this loop and starts the right one.
    private func pollWhileLive() async {
        guard let snapshot = model.snapshot,
              TaskDetailLayout.shouldPoll(visible: visible && scenePhase == .active,
                                          status: snapshot.status, planStatus: snapshot.planStatus)
        else { return }
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: UInt64(TaskDetailLayout.pollInterval * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await model.pollPanel()
        }
    }

    // MARK: - Content

    @ViewBuilder
    private func content(_ snapshot: TaskDetailSnapshot) -> some View {
        let now = Date()
        header(snapshot, now: now)
            .background(
                GeometryReader { proxy in
                    Color.clear.preference(
                        key: HeaderBottomKey.self,
                        value: proxy.frame(in: .named(Self.scrollSpace)).maxY)
                }
            )
        // What this task waits on, answered here: the run's own asks and the
        // Attention feed's asks for this task, with the dock's controls.
        TaskDetailAskSection(
            taskId: route.taskId,
            panelRows: snapshot.askRows,
            onReviewPlan: { taskId in
                planInspectorRoute = PlanInspectorRoute(taskId: taskId, title: snapshot.title,
                                                        planDocument: snapshot.planMarkdown)
            },
            onAnswered: {
                Task {
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    await model.pollPanel()
                }
            })
        HStack(spacing: 16) {
            if !snapshot.isExecutionOnly {
                NavigationLink("Quality review") { WorkQualityView(taskID: route.taskId).navigationTitle("Quality and review") }
            }
            if let agentID = snapshot.agentId, !agentID.isEmpty {
                NavigationLink("Crew member") { WorkCrewDetail(agentID: agentID) }
            }
        }
        .font(.themed(13)).tint(theme.accentColor).frame(minHeight: 44)
        if !snapshot.unavailableSections.isEmpty {
            TaskDetailNotice(
                text: "Some \(snapshot.unavailableSections.joined(separator: ", ")) details are unavailable. Pull to retry.",
                tint: theme.warningColor, icon: "wifi.exclamationmark")
        }
        ForEach(TaskDetailLayout.actOrder(snapshot, now: now), id: \.self) { act in
            TaskDetailActCard(
                act: act,
                snapshot: snapshot,
                isOpen: openAct == act,
                taskId: route.taskId,
                model: model,
                onToggle: { toggle(act) },
                onNotice: { model.notice = $0 }
            )
            .id(anchor(act))
        }
        TaskDetailTechnicalCard(snapshot: snapshot, taskId: route.taskId, model: model,
                                onNotice: { model.notice = $0 })
    }

    private func anchor(_ act: ActId) -> String { "task-detail-act-\(act.rawValue)" }

    private func toggle(_ act: ActId) {
        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.2)) {
            openAct = openAct == act ? nil : act
        }
    }

    private func open(_ act: ActId) {
        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.2)) { openAct = act }
        scrollTarget = anchor(act)
    }

    // MARK: - Header card

    private func header(_ snapshot: TaskDetailSnapshot, now: Date) -> some View {
        let verdict = snapshot.displayVerdict(now: now)
        return VStack(alignment: .leading, spacing: 10) {
            TaskDetailVerdictLine(verdict: verdict, partlyDone: snapshot.isPartlyDone)
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                if TaskPlanMarker.hasPlan(snapshot) { TaskPlanGlyph(size: 15) }
                Text(snapshot.title.isEmpty ? "Untitled task" : snapshot.title)
                    .font(.themed(22, weight: .bold))
                    .foregroundColor(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
            }
            if let brief = Self.brief(snapshot.description) {
                Text(brief)
                    .font(.themed(15))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let recurrence = snapshot.recurrenceDescription {
                Label("Recurring · \(recurrence)", systemImage: "repeat")
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(theme.infoColor)
            }
            if let total = TaskDetailLayout.usageTotal(snapshot),
               let line = TaskUsageFormat.taskLine(total.usage) {
                // The task's model usage, planning and run together.
                Text(line)
                    .font(.themedMono(12))
                    .monospacedDigit()
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
                    .accessibilityLabel(TaskUsageFormat.taskAccessibilityLabel(total))
                    .accessibilityIdentifier("task-detail-usage")
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    idChip
                    if snapshot.hasPlan, snapshot.acts.contains(.plan) {
                        Button { open(.plan) } label: {
                            Label(planChipText(snapshot, now: now), systemImage: "list.bullet.clipboard")
                                .lineLimit(1)
                        }
                        .buttonStyle(ChipStyle())
                        .accessibilityIdentifier("task-detail-plan-chip")
                    }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .strokeBorder(theme.cardBorderColor, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("task-detail-header")
    }

    /// The short id (`TaskTitleRules.shortTaskId`); a tap copies the full id.
    private var idChip: some View {
        let short = TaskTitleRules.shortTaskId(route.taskId) ?? route.taskId
        return CopyOnTap(value: route.taskId, kind: .taskId, onCopied: { model.notice = $0 }) { copied in
            CopiedSwap(copied: copied, font: .themed(12, weight: .semibold)) {
                Label(short, systemImage: "number")
                    .font(.themedMono(12))
                    .lineLimit(1)
            }
        }
        .buttonStyle(ChipStyle())
        .accessibilityLabel("Task id \(short)")
        .accessibilityIdentifier("task-detail-id-chip")
    }

    private func planChipText(_ snapshot: TaskDetailSnapshot, now: Date) -> String {
        let summary = TaskDetailLayout.summary(for: .plan, snapshot, now: now)
        return summary.isEmpty ? "Plan" : "Plan · \(summary)"
    }

    /// The first paragraph of the description, Markdown markers stripped.
    static func brief(_ description: String) -> String? {
        let paragraph = description
            .components(separatedBy: "\n\n").first?
            .replacingOccurrences(of: "#", with: "")
            .replacingOccurrences(of: "**", with: "")
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return paragraph.isEmpty ? nil : paragraph
    }

    static func shortId(_ id: String) -> String {
        id.count > 14 ? String(id.prefix(12)) + "…" : id
    }

    // MARK: - Compact title

    private var compactTitle: some View {
        HStack(spacing: 6) {
            if let snapshot = model.snapshot {
                StatusDot(tone: TaskDetailVerdictLine.tone(for: snapshot.verdict(now: Date()).state,
                                                           partlyDone: snapshot.isPartlyDone), size: 7)
            }
            if let snapshot = model.snapshot, TaskPlanMarker.hasPlan(snapshot) { TaskPlanGlyph(size: 11) }
            Text(model.snapshot?.title ?? route.titleHint ?? "Task")
                .font(.themed(15, weight: .semibold))
                .foregroundColor(theme.textColor)
                .lineLimit(1)
            if let short = TaskTitleRules.shortTaskId(route.taskId) {
                CopyOnTap(value: route.taskId, kind: .taskId, onCopied: { model.notice = $0 }) { copied in
                    CopiedSwap(copied: copied, font: .themed(11, weight: .semibold)) {
                        Text(short)
                            .font(.themedMono(11))
                            .foregroundColor(theme.secondaryTextColor)
                    }
                    .contentShape(Rectangle().inset(by: -10))
                }
                .buttonStyle(.plain)
                .layoutPriority(1)
                .accessibilityLabel("Task id \(short)")
                .accessibilityIdentifier("task-detail-compact-id")
            }
        }
        .opacity(showCompactTitle ? 1 : 0)
        .accessibilityHidden(!showCompactTitle)
    }

    // MARK: - Follow

    /// Follow: the task as its own Live Activity until it ends. Offered while
    /// the task is not over, and not for an execution-only or app-managed one.
    @ViewBuilder
    private var followButton: some View {
        if let snapshot = model.snapshot, !snapshot.isExecutionOnly,
           !TaskWorkOwnership.isAppManaged(model.seed.taskId),
           !FollowedItemActivity.isEndedTaskStatus(snapshot.status) {
            WorkFollowButton(
                seed: FollowedItemActivity.taskSeed(taskID: route.taskId, title: snapshot.title),
                iconOnly: true)
        }
    }

    // MARK: - ⋯ menu

    @ViewBuilder
    private var actionsMenu: some View {
        if let snapshot = model.snapshot {
            TaskDetailActionsMenu(
                snapshot: snapshot,
                isAppManaged: TaskWorkOwnership.isAppManaged(model.seed.taskId),
                busy: model.busyAction != nil || notePublisher.publishingTaskID != nil,
                canPublish: !snapshot.isExecutionOnly && TaskNotePublishEligibility.allows(status: snapshot.status),
                perform: { action in
                    if action == .delete { confirmDelete = true; return }
                    Task {
                        if await model.perform(action) { onChanged?() }
                    }
                },
                publish: { Task { await notePublisher.publish(taskID: route.taskId) } },
                refresh: { Task { await model.refresh() } },
                onRunChanged: { Task { await model.refresh() } }
            )
            .id(snapshot.activeRootExecutionId ?? "no-run")
        } else {
            Image(systemName: "ellipsis.circle").foregroundColor(theme.secondaryTextColor)
        }
    }

    // MARK: - States

    @ViewBuilder
    private var toast: some View {
        if let notice = model.notice {
            Text(notice)
                .font(.themed(13, weight: .semibold))
                .foregroundColor(theme.onAccentColor)
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Capsule().fill(theme.accentFillColor))
                .padding(.bottom, 18)
                .transition(.opacity)
                .task(id: notice) {
                    try? await Task.sleep(nanoseconds: 2_400_000_000)
                    if model.notice == notice { model.notice = nil }
                }
                .accessibilityIdentifier("task-detail-toast")
        }
    }

    private var loading: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading the task…").font(.themed(14)).foregroundColor(theme.secondaryTextColor)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 100)
    }

    private func failure(_ message: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill").font(.largeTitle).foregroundColor(theme.warningColor)
            Text(message).font(.themed(14)).foregroundColor(theme.secondaryTextColor).multilineTextAlignment(.center)
            Button("Try again") { Task { await model.refresh() } }
                .buttonStyle(ChipStyle(prominent: true))
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 80)
    }
}

/// The header card's bottom edge in the scroll's coordinate space.
private struct HeaderBottomKey: PreferenceKey {
    static var defaultValue: CGFloat = .greatestFiniteMagnitude
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}

// MARK: - Verdict line

/// L0: the verdict with its status dot (web `TaskVerdictLine`).
struct TaskDetailVerdictLine: View {
    let verdict: Verdict
    /// Completed with open requirements: a finished verdict takes the
    /// partial (warning) tone instead of done.
    var partlyDone = false
    @ObservedObject private var theme = ThemeManager.shared

    static func tone(for state: VerdictState, partlyDone: Bool) -> StatusDot.Tone {
        state == .finished && partlyDone ? .partial : tone(for: state)
    }

    /// Verdict state → dot tone. Exhaustive, so a new state fails to build.
    static func tone(for state: VerdictState) -> StatusDot.Tone {
        switch state {
        case .running, .finalizing: return .running
        case .waiting: return .waiting
        case .failed: return .failed
        case .stalled, .paused: return .partial
        case .cancelled, .archived: return .cancelled
        case .queued: return .notStarted
        case .finished: return .done
        }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            StatusDot(tone: Self.tone(for: verdict.state, partlyDone: partlyDone), size: 9)
                .alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
            VStack(alignment: .leading, spacing: 2) {
                Text(verdict.headline)
                    .font(.themed(14, weight: .semibold))
                    .foregroundColor(StatusDot.color(for: Self.tone(for: verdict.state, partlyDone: partlyDone), in: theme))
                if !verdict.detail.isEmpty {
                    Text(verdict.detail)
                        .font(.themed(13))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("task-detail-verdict")
    }
}

// MARK: - ⋯ menu

/// The toolbar's ⋯ menu: live run controls (pause, resume, steer, stop) when
/// a run is active, then the task's lifecycle actions (`TaskDetailMenu`).
private struct TaskDetailActionsMenu: View {
    let snapshot: TaskDetailSnapshot
    let isAppManaged: Bool
    let busy: Bool
    let canPublish: Bool
    let perform: (TaskDetailAction) -> Void
    let publish: () -> Void
    let refresh: () -> Void
    let onRunChanged: () -> Void

    @StateObject private var runControls: RunControlsHolder
    @State private var showSteer = false
    @State private var confirmStop = false

    init(snapshot: TaskDetailSnapshot, isAppManaged: Bool = false, busy: Bool, canPublish: Bool,
         perform: @escaping (TaskDetailAction) -> Void, publish: @escaping () -> Void,
         refresh: @escaping () -> Void, onRunChanged: @escaping () -> Void) {
        self.snapshot = snapshot
        self.isAppManaged = isAppManaged
        self.busy = busy
        self.canPublish = canPublish
        self.perform = perform
        self.publish = publish
        self.refresh = refresh
        self.onRunChanged = onRunChanged
        _runControls = StateObject(wrappedValue: RunControlsHolder(
            executionId: snapshot.activeRootExecutionId, onChanged: onRunChanged))
    }

    var body: some View {
        Menu {
            if let state = runControls.model?.state, state.hasAvailableAction {
                Section("Run") {
                    if state.canPause { runButton("Pause run", "pause.fill", .pause) }
                    if state.canResume { runButton("Resume run", "play.fill", .resume) }
                    if state.canSteer {
                        Button { showSteer = true } label: {
                            Label("Steer run…", systemImage: "arrow.triangle.turn.up.right.diamond.fill")
                        }
                    }
                    if state.canCancel {
                        Button(role: .destructive) { confirmStop = true } label: {
                            Label("Stop run", systemImage: "stop.fill")
                        }
                    }
                }
            }
            Section {
                ForEach(TaskDetailMenu.actions(snapshot).filter { $0 != .delete && !isAppManaged }, id: \.self) { action in
                    Button { perform(action) } label: {
                        Label(TaskDetailMenu.title(action), systemImage: TaskDetailMenu.icon(action))
                    }
                }
            }
            Section {
                if canPublish {
                    Button(action: publish) { Label("Publish to Notes", systemImage: "note.text.badge.plus") }
                        .accessibilityIdentifier("task-detail-publish-notes")
                }
                Button(action: refresh) { Label("Refresh", systemImage: "arrow.clockwise") }
            }
            if !snapshot.isExecutionOnly && !isAppManaged { Section {
                Button(role: .destructive) { perform(.delete) } label: {
                    Label(TaskDetailMenu.title(.delete), systemImage: TaskDetailMenu.icon(.delete))
                }
            } }
        } label: {
            Image(systemName: "ellipsis.circle")
                .frame(minWidth: Lively.tapMin, minHeight: Lively.tapMin)
                .contentShape(Rectangle())
        }
        .disabled(busy)
        .accessibilityLabel("Task actions")
        .accessibilityIdentifier("task-detail-menu")
        .task { runControls.model?.load() }
        .sheet(isPresented: $showSteer) {
            ExecutionSteerSheet(
                isBusy: runControls.model?.busyAction != nil,
                onSend: { runControls.model?.perform(.steer, message: $0) ?? false })
        }
        .confirmationDialog("Stop this run?", isPresented: $confirmStop, titleVisibility: .visible) {
            Button("Stop run", role: .destructive) { _ = runControls.model?.perform(.cancel) }
            Button("Keep running", role: .cancel) {}
        } message: {
            Text("The active execution and its running children will be cancelled.")
        }
        .alert("Run control failed", isPresented: Binding(
            get: { runControls.model?.errorMessage != nil },
            set: { if !$0 { runControls.model?.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { runControls.model?.errorMessage = nil }
        } message: {
            Text(runControls.model?.errorMessage ?? "The run could not be updated.")
        }
    }

    private func runButton(_ title: String, _ icon: String, _ action: ExecutionControlAction) -> some View {
        Button { _ = runControls.model?.perform(action) } label: { Label(title, systemImage: icon) }
            .disabled(runControls.model?.busyAction != nil)
    }
}

/// Holds the active run's control model (if any) and republishes its changes
/// so the menu re-renders.
@MainActor
private final class RunControlsHolder: ObservableObject {
    let model: ExecutionControlViewModel?
    private var forward: Any?

    init(executionId: String?, onChanged: @escaping () -> Void) {
        model = executionId.map { ExecutionControlViewModel(executionId: $0, onChanged: onChanged) }
        forward = model?.objectWillChange.sink { [weak self] _ in self?.objectWillChange.send() }
    }
}

// MARK: - Navigation

/// Push a task detail from anywhere under a host that registered one
/// (`taskDetailHost`). Rows inside lazy lists use this instead of declaring
/// their own `navigationDestination`, which SwiftUI does not support there.
struct OpenTaskDetailAction {
    let handler: (TaskDetailRoute) -> Void
    func callAsFunction(_ route: TaskDetailRoute) { handler(route) }
}

private struct OpenTaskDetailKey: EnvironmentKey {
    static let defaultValue: OpenTaskDetailAction? = nil
}

extension EnvironmentValues {
    /// Set by `taskDetailHost`; nil where no `NavigationStack` hosts the screen.
    var openTaskDetail: OpenTaskDetailAction? {
        get { self[OpenTaskDetailKey.self] }
        set { self[OpenTaskDetailKey.self] = newValue }
    }
}

/// The screen presented modally, for a caller with no `NavigationStack`
/// host: the same screen with a Done button.
struct TaskDetailSheetHost: View {
    let route: TaskDetailRoute
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            TaskDetailScreen(route: route)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                }
        }
    }
}

extension View {
    /// Make this view's `NavigationStack` the task detail host: descendants
    /// push with `@Environment(\.openTaskDetail)`, and `route` drives the push.
    /// Apply it to the stack's root content.
    func taskDetailHost(_ route: Binding<TaskDetailRoute?>, onChanged: (() -> Void)? = nil) -> some View {
        environment(\.openTaskDetail, OpenTaskDetailAction { route.wrappedValue = $0 })
            .taskDetailDestination(route, onChanged: onChanged)
    }

    /// Push the task detail screen for whichever route is set.
    func taskDetailDestination(_ route: Binding<TaskDetailRoute?>, onChanged: (() -> Void)? = nil) -> some View {
        navigationDestination(item: route) { TaskDetailScreen(route: $0, onChanged: onChanged) }
    }

    /// Register the task detail screen for `NavigationLink(value: TaskDetailRoute)`
    /// anywhere inside this `NavigationStack` (rows in lazy lists included).
    func taskDetailDestinations(onChanged: (() -> Void)? = nil) -> some View {
        navigationDestination(for: TaskDetailRoute.self) { TaskDetailScreen(route: $0, onChanged: onChanged) }
    }
}
