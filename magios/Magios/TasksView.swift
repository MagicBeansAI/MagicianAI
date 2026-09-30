import SwiftUI

/// The Tasks workspace — web `/tasks` parity: regular Tasks use the preset and
/// tag filters, while Internal tasks use their own Any-status/agent controls.
/// Task cards open the existing DeepWorkPanel detail.
struct TasksView: View {
    @StateObject private var themeManager = ThemeManager.shared
    @StateObject private var vm = TasksViewModel()
    @StateObject private var monitorsVM = MonitorsListViewModel()
    @State private var selectedTask: TaskV3?
    /// Where the next task detail opens (the Result button's Output/Result
    /// focus). Cleared when the detail closes so ordinary opens stay default.
    @State private var detailFocus: TaskDetailFocus?
    @StateObject private var notePublisher = TaskNotePublishViewModel()
    @State private var selectedTaskActions: TaskV3?
    @State private var pendingDestructiveAction: PendingTaskDestructiveAction?
    @State private var showCreate = false
    @State private var showMonitorCompose = false
    @State private var selectedMonitor: Monitors.DeepLinkTarget?
    /// Phase 7: the eligible task being converted to a monitor (sheet item).
    @State private var convertTarget: TaskV3?
    @State private var resolvingTaskID: String?

    var body: some View {
        NavigationView {
            VStack(spacing: 0) {
                lanePicker
                if vm.lane != .monitors {
                    filterBar
                    statusLedger
                }
                Divider().overlay(themeManager.secondaryTextColor.opacity(0.15))
                content
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .safeAreaInset(edge: .top, spacing: 0) {
                if let notice = vm.actionNoticeMessage ?? notePublisher.successMessage {
                    Label(notice, systemImage: "checkmark.circle.fill")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.successColor)
                        .frame(maxWidth: .infinity)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 7)
                        .background(themeManager.successColor.opacity(0.12))
                        .accessibilityIdentifier("tasks-action-notice")
                }
            }
            // Publishing from a card sets a success message that never expires on
            // its own; clear it on the same 2.6 s cadence as the action notice.
            .onChange(of: notePublisher.successMessage) { _, message in
                guard let message else { return }
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.6) {
                    if notePublisher.successMessage == message { notePublisher.clearSuccess() }
                }
            }
            .navigationTitle("Tasks")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) { HamburgerButton() }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button {
                        if vm.lane == .monitors { showMonitorCompose = true }
                        else { showCreate = true }
                    } label: { Image(systemName: "plus") }
                        .tint(themeManager.accentColor)
                        .accessibilityIdentifier("tasks-create")
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button {
                        if vm.lane == .monitors { Task { await monitorsVM.reload() } }
                        else { vm.load() }
                    } label: { Image(systemName: "arrow.clockwise") }
                        .tint(themeManager.accentColor)
                }
            }
            .onAppear {
                vm.load()
                vm.connectRealtime()
                revealRequestedTask()
                revealRequestedMonitor()
            }
            .onReceive(AppActions.shared.$taskRequestID.dropFirst()) { _ in revealRequestedTask() }
            .onReceive(AppActions.shared.$monitorRequestID.dropFirst()) { _ in revealRequestedMonitor() }
            .onReceive(vm.$tasks.dropFirst()) { _ in revealRequestedTask() }
            .onReceive(vm.$internalTasks.dropFirst()) { _ in revealRequestedTask() }
            .onDisappear { vm.disconnectRealtime() }
            .sheet(isPresented: $showCreate) { TaskCreateView(vm: vm) }
            .sheet(isPresented: $showMonitorCompose) {
                MonitorComposerView(mode: .create) {
                    Task { await monitorsVM.refreshLoadedSpan() }
                }
            }
            .fullScreenCover(item: $selectedMonitor) { target in
                MonitorDetailView(
                    taskID: target.taskID,
                    highlightUpdateID: target.updateID,
                    onOpenTask: { taskID in
                        selectedMonitor = nil
                        openSharedTaskDetail(taskID)
                    },
                    onChanged: { Task { await monitorsVM.refreshLoadedSpan() } },
                    onDeleted: {
                        // The row goes at once, then the span is re-read: the
                        // reader keeps the pages they loaded, and the cursor
                        // comes back anchored on a monitor that still exists
                        // (a cursor minted from the deleted row would resolve
                        // to the end of the list and stop paging silently).
                        monitorsVM.remove(taskID: target.taskID)
                        Task { await monitorsVM.refreshLoadedSpan() }
                    }
                )
            }
            .sheet(item: $selectedTaskActions) { task in
                TaskCardActionsSheet(
                    task: task,
                    viewModel: vm,
                    openDetails: {
                        selectedTaskActions = nil
                        DispatchQueue.main.async { selectedTask = task }
                    },
                    requestConvert: {
                        selectedTaskActions = nil
                        DispatchQueue.main.async { convertTarget = task }
                    },
                    requestCancel: {
                        selectedTaskActions = nil
                        pendingDestructiveAction = .init(task: task, kind: .cancel)
                    },
                    requestDelete: {
                        selectedTaskActions = nil
                        pendingDestructiveAction = .init(task: task, kind: .delete)
                    }
                )
                .presentationDetents([.medium, .large])
                .presentationDragIndicator(.visible)
            }
            // Phase 7: convert an eligible task through the EXISTING monitor
            // composer in convert mode — prefilled (objective ← description,
            // title ← title), review-before-activate, POST
            // /monitors/{id}/convert. The task keeps its id/schedule/history
            // and then appears in the Monitors lane.
            .sheet(item: $convertTarget) { task in
                MonitorComposerView(
                    mode: .convert(taskID: task.id, keptCadence: task.keptScheduleSummary),
                    initialForm: MonitorForm.convertPrefill(
                        taskTitle: task.title, taskDescription: task.description)
                ) {
                    convertTarget = nil
                    vm.lane = .monitors
                    vm.load()
                    Task { await monitorsVM.refreshLoadedSpan() }
                }
            }
            .alert("Task action failed", isPresented: Binding(
                get: { vm.actionErrorMessage != nil },
                set: { if !$0 { vm.actionErrorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { vm.actionErrorMessage = nil }
            } message: {
                Text(vm.actionErrorMessage ?? "The task could not be updated.")
            }
            .alert("Publish to Notes failed", isPresented: Binding(
                get: { notePublisher.errorMessage != nil },
                set: { if !$0 { notePublisher.errorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { notePublisher.errorMessage = nil }
            } message: {
                Text(notePublisher.errorMessage ?? "The task page could not be published.")
            }
            .onChange(of: selectedTask?.id) { _, id in
                if id == nil { detailFocus = nil }
            }
            .searchable(text: $vm.searchQuery, placement: .navigationBarDrawer(displayMode: .automatic), prompt: "Search tasks")
            .fullScreenCover(item: $selectedTask) { task in
                DeepWorkPanel(task: task, focus: detailFocus, onAction: handleDetailAction)
                    .alert("Task action failed", isPresented: Binding(
                        get: { vm.actionErrorMessage != nil },
                        set: { if !$0 { vm.actionErrorMessage = nil } }
                    )) {
                        Button("OK", role: .cancel) { vm.actionErrorMessage = nil }
                    } message: {
                        Text(vm.actionErrorMessage ?? "The task could not be updated.")
                    }
            }
            .confirmationDialog(
                pendingDestructiveAction?.title ?? "Confirm task action",
                isPresented: Binding(
                    get: { pendingDestructiveAction != nil },
                    set: { if !$0 { pendingDestructiveAction = nil } }
                ),
                titleVisibility: .visible,
                presenting: pendingDestructiveAction
            ) { pending in
                if pending.kind == .delete && !pending.isInternal {
                    Button("Delete task and folder", role: .destructive) {
                        performDestructiveAction(pending, removeFiles: true)
                    }
                    Button("Delete task only", role: .destructive) {
                        performDestructiveAction(pending, removeFiles: false)
                    }
                } else {
                    Button(pending.confirmLabel, role: .destructive) {
                        performDestructiveAction(pending)
                    }
                }
                Button("Keep task", role: .cancel) { pendingDestructiveAction = nil }
            } message: { pending in
                Text(pending.message)
            }
        }
    }

    private func revealRequestedTask() {
        guard let target = AppActions.shared.taskTargetID else { return }
        guard resolvingTaskID != target else { return }
        resolvingTaskID = target
        // §9.3.4 fallback resolution: `magican://task/{id}` opens MONITOR mode
        // when the task is a monitor. The task list projection cannot tell
        // (monitor_spec lives on the manifest), so probe the monitor surface
        // through the SAME injectable client the Monitors lane uses (NOT an
        // ephemeral live client — tests can substitute a mock transport):
        // 200 → monitor detail; monitor_not_found/any failure → plain task.
        Task { @MainActor in
            guard AppActions.shared.taskTargetID == target else {
                resolvingTaskID = nil
                revealRequestedTask()
                return
            }
            if (try? await monitorsVM.client.detail(target)) != nil {
                resolvingTaskID = nil
                switch Monitors.finishTaskLink(
                    actions: AppActions.shared, target: target,
                    monitorProbeSucceeded: true, taskResolved: false) {
                case .superseded:
                    revealRequestedTask()
                default:
                    vm.lane = .monitors
                    selectedMonitor = Monitors.DeepLinkTarget(taskID: target)
                }
                return
            }
            vm.resolveNavigationTarget(for: target) { resolved in
                if resolvingTaskID == target { resolvingTaskID = nil }
                // Terminal either way: `finishTaskLink` consumes the pending
                // target exactly ONCE — including the probe-failed +
                // unresolved (offline) path, which previously left the
                // target set and re-probed on every `$tasks` publish
                // forever. Navigation degrades once; re-tapping retries.
                switch Monitors.finishTaskLink(
                    actions: AppActions.shared, target: target,
                    monitorProbeSucceeded: false, taskResolved: resolved != nil) {
                case .superseded:
                    revealRequestedTask()
                case .openTask:
                    if let resolved {
                        vm.prepareForNavigation(to: resolved)
                        selectedTask = resolved.task
                    }
                case .openMonitor, .degraded:
                    break
                }
            }
        }
    }

    /// A monitor deep link (Today `monitor_update` card, `magican://monitor/…`,
    /// or a canonical `/tasks?type=monitors&selected=…&update=…` route)
    /// landed: switch to the Monitors lane and open the detail at the exact
    /// update. This is the in-app resolution path push notifications ride
    /// once iOS push exists (§9.3.4 — no OS push today).
    private func revealRequestedMonitor() {
        guard let taskID = AppActions.shared.monitorTargetTaskID else { return }
        vm.lane = .monitors
        selectedMonitor = Monitors.DeepLinkTarget(
            taskID: taskID,
            updateID: AppActions.shared.monitorTargetUpdateID)
        AppActions.shared.consumeMonitorTarget()
    }

    /// Route from monitor Settings into the SHARED task detail (the one
    /// task-detail implementation — DeepWorkPanel).
    private func openSharedTaskDetail(_ taskID: String) {
        vm.resolveNavigationTarget(for: taskID) { resolved in
            guard let resolved else { return }
            selectedTask = resolved.task
        }
    }

    private var lanePicker: some View {
        Picker("Lane", selection: $vm.lane) {
            ForEach(TaskLane.allCases) { Text($0.title).tag($0) }
        }
        .pickerStyle(.segmented)
        .padding(.horizontal)
        .padding(.top, 8)
    }

    private var filterBar: some View {
        VStack(spacing: 8) {
            if vm.lane == .tasks {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(TaskFilter.allCases) { f in
                            // The badge counts the corpus, not the loaded pages —
                            // and is simply absent when the server reported no
                            // count, rather than showing a 0 nobody counted.
                            chip(f.title, badge: vm.laneCount(f),
                                 active: vm.selectedTag == nil && vm.filter == f) { vm.setFilter(f) }
                        }
                    }
                    .padding(.horizontal)
                }
            }
            controlsRow
            if vm.lane == .tasks && !vm.availableTags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(vm.availableTags, id: \.self) { tag in
                            chip("#\(tag)", active: vm.selectedTag == tag) { vm.toggleTag(tag) }
                        }
                    }
                    .padding(.horizontal)
                }
            }
        }
        .padding(.vertical, 8)
    }

    /// Sort control + (internal lane only) status/agent filter menus — web parity.
    private var controlsRow: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                sortMenu
                if vm.lane == .internalTasks {
                    internalStatusMenu
                    if !vm.availableInternalAgents.isEmpty { internalAgentMenu }
                }
            }
            .padding(.horizontal)
        }
    }

    private var sortMenu: some View {
        Menu {
            ForEach(TaskSortField.allCases) { field in
                Button { vm.setSort(field: field) } label: {
                    if vm.sortField == field {
                        Label(field.title, systemImage: vm.sortAscending ? "arrow.up" : "arrow.down")
                    } else {
                        Text(field.title)
                    }
                }
            }
        } label: {
            controlChipLabel(icon: "arrow.up.arrow.down",
                             text: "\(vm.sortField.title) \(vm.sortAscending ? "↑" : "↓")")
        }
    }

    private var internalStatusMenu: some View {
        Menu {
            Button("Any status") { vm.internalStatusFilter = nil }
            ForEach(["pending", "planning", "running", "completed", "failed", "paused"], id: \.self) { s in
                Button(s.capitalized) { vm.internalStatusFilter = s }
            }
        } label: {
            controlChipLabel(icon: "line.3.horizontal.decrease.circle",
                             text: vm.internalStatusFilter?.capitalized ?? "Status",
                             active: vm.internalStatusFilter != nil)
        }
    }

    private var internalAgentMenu: some View {
        Menu {
            Button("Any agent") { vm.internalAgentFilter = nil }
            ForEach(vm.availableInternalAgents, id: \.self) { a in
                Button(agentName(a)) { vm.internalAgentFilter = a }
            }
        } label: {
            controlChipLabel(icon: "person.crop.circle",
                             text: vm.internalAgentFilter.map(agentName) ?? "Agent",
                             active: vm.internalAgentFilter != nil)
        }
    }

    private func controlChipLabel(icon: String, text: String, active: Bool = false) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon).font(.system(size: 11))
            Text(text)
        }
        .font(.themed(13, weight: .medium))
        .padding(.horizontal, 12).padding(.vertical, 6)
        .background(active ? themeManager.accentColor : themeManager.surfaceColor)
        .foregroundColor(active ? themeManager.onAccentColor : themeManager.textColor)
        .clipShape(Capsule())
        .overlay(Capsule().stroke(themeManager.secondaryTextColor.opacity(active ? 0 : 0.2), lineWidth: 1))
    }

    @ViewBuilder private var content: some View {
        if vm.lane == .monitors {
            MonitorsLaneView(
                vm: monitorsVM,
                onSelect: { item in
                    selectedMonitor = Monitors.DeepLinkTarget(taskID: item.taskID)
                },
                onCreate: { showMonitorCompose = true }
            )
        } else if vm.visibleTasks.isEmpty {
            taskListPlaceholder
        } else {
            ScrollView {
                LazyVStack(spacing: 10) {
                    ForEach(vm.visibleTasks) { taskCard($0) }
                    loadMoreFooter
                }
                .padding(.horizontal)
                .padding(.vertical, 10)
            }
        }
    }

    /// Server-pagination footer (monitors idiom): shows how much of the pool
    /// is loaded and appends the next page. Hidden when everything is local
    /// (legacy binary → the whole pool arrives in one response).
    @ViewBuilder private var loadMoreFooter: some View {
        let hasMore = vm.lane == .internalTasks ? vm.internalHasMore : vm.tasksHasMore
        let total = vm.lane == .internalTasks ? vm.internalTotal : vm.tasksTotal
        let loaded = vm.lane == .internalTasks ? vm.internalTasks.count : vm.tasks.count
        if vm.lane != .monitors && hasMore {
            Button {
                vm.loadMore(lane: vm.lane)
            } label: {
                HStack(spacing: 6) {
                    if vm.activeLoadState == .loading { ProgressView().controlSize(.small) }
                    Text("Load more · \(loaded) of \(total)")
                        .font(.footnote.weight(.medium))
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 10)
            }
            .buttonStyle(.bordered)
            .accessibilityLabel("Load more tasks, \(loaded) of \(total) loaded")
        }
    }

    @ViewBuilder private var taskListPlaceholder: some View {
        switch vm.activeLoadState {
        case .idle, .loading:
            VStack(spacing: 8) {
                Spacer()
                ProgressView()
                    .tint(themeManager.accentColor)
                Text(vm.lane == .internalTasks ? "Loading internal tasks…" : "Loading tasks…")
                    .font(.themed(15)).foregroundColor(themeManager.secondaryTextColor)
                Spacer()
            }
            .frame(maxWidth: .infinity)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("tasks-loading-state")
        case .failed:
            VStack(spacing: 10) {
                Spacer()
                Image(systemName: "exclamationmark.arrow.triangle.2.circlepath")
                    .font(.system(size: 32))
                    .foregroundColor(themeManager.warningColor)
                Text(vm.lane == .internalTasks
                     ? "Internal tasks could not be loaded"
                     : "Tasks could not be loaded")
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                Button { vm.load() } label: {
                    Label("Try again", systemImage: "arrow.clockwise")
                        .font(.themed(13, weight: .semibold))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(themeManager.accentColor)
                        .foregroundColor(themeManager.onAccentColor)
                        .clipShape(RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
                Spacer()
            }
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier("tasks-load-failed-state")
        case .loaded:
            VStack(spacing: 8) {
                Spacer()
                Image(systemName: "checklist")
                    .font(.system(size: 34))
                    .foregroundColor(themeManager.secondaryTextColor.opacity(0.5))
                Text("No tasks here")
                    .font(.themed(15))
                    .foregroundColor(themeManager.secondaryTextColor)
                Spacer()
            }
            .frame(maxWidth: .infinity)
            .accessibilityIdentifier("tasks-empty-state")
        }
    }

    private func taskCard(_ t: TaskV3) -> some View {
        // A plain container with a tap gesture (not a Button) so card controls
        // remain independent and never become nested buttons.
        TaskSwipeActionCard(
            itemID: t.id,
            leadingActions: swipeActions(t.leadingCardSwipeActions, for: t),
            trailingActions: swipeActions(t.trailingCardSwipeActions, for: t)
        ) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .top, spacing: 8) {
                    Text(t.title.isEmpty ? "Untitled task" : t.title)
                        .font(.themed(16, weight: .semibold))
                        .foregroundColor(themeManager.textColor)
                        .lineLimit(2)
                    if vm.lane == .internalTasks && t.isRecurring {
                        // Internal rows mark recurrence inline after the title
                        // (web internal workspace ↻), not as a meta chip.
                        Text("↻")
                            .font(.themed(15, weight: .semibold))
                            .foregroundColor(themeManager.infoColor)
                            .accessibilityLabel("Recurring: \(t.recurrenceDescription)")
                            .accessibilityIdentifier("task-recurring-glyph-\(t.id)")
                    }
                    Spacer(minLength: 8)
                    if t.needsAnswer {
                        Text("Answer")
                            .font(.themed(11, weight: .bold))
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .background(themeManager.warningColor.opacity(0.16))
                            .foregroundColor(themeManager.warningColor)
                            .clipShape(Capsule())
                    }
                    statusBadge(t)
                }
                if let line = t.activityLine, !line.isEmpty {
                    Text(line)
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                metaRow(t)
                taskCardActionRow(t)
                originRow(t)
                if let executionId = runtimeExecutionId(t) {
                    ExecutionControlsView(
                        executionId: executionId,
                        refreshToken: "\(t.status)|\(t.updatedAt)|\(executionId)",
                        onChanged: vm.load
                    )
                        .id(executionId)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(themeManager.cardColor)
            .clipShape(RoundedRectangle(cornerRadius: 14))
            .overlay {
                RoundedRectangle(cornerRadius: 14)
                    .stroke(themeManager.cardBorderColor, lineWidth: 1)
            }
            .contentShape(Rectangle())
            .onTapGesture {
                selectedTask = t
            }
            .accessibilityIdentifier("task-card-\(t.id)")
        }
    }

    private func swipeActions(_ actions: [TaskCardSwipeAction], for task: TaskV3) -> [TaskSwipeCardAction] {
        actions.map { action in
            TaskSwipeCardAction(
                id: action.rawValue,
                title: action.title,
                systemImage: action.systemImage,
                color: swipeTint(action)
            ) {
                performSwipeAction(action, for: task)
            }
        }
    }

    private func performSwipeAction(_ action: TaskCardSwipeAction, for task: TaskV3) {
        switch action {
        case .markComplete: vm.setStatus(task, "completed")
        case .markNotDone, .reset: vm.setStatus(task, "ready")
        case .cancel:
            pendingDestructiveAction = .init(task: task, kind: .cancel)
        case .delete:
            pendingDestructiveAction = .init(task: task, kind: .delete)
        }
    }

    private func performDestructiveAction(
        _ pending: PendingTaskDestructiveAction,
        removeFiles: Bool = false
    ) {
        pendingDestructiveAction = nil
        switch pending.kind {
        case .cancel: vm.setStatus(pending.task, "cancelled")
        case .delete: vm.deleteTask(pending.task, removeFiles: removeFiles)
        }
    }

    private func handleDetailAction(_ task: TaskV3, _ action: TaskDetailAction) {
        switch action {
        case .run: vm.execute(task)
        case .plan: vm.preplan(task)
        case .reset, .markNotDone: vm.setStatus(task, "ready")
        case .markComplete: vm.setStatus(task, "completed")
        case .cancelTask: vm.setStatus(task, "cancelled")
        case .approvePlan: vm.approvePlan(task)
        case .rejectPlan: vm.rejectPlan(task)
        case .replan: vm.replan(task)
        case .retrySynthesis: vm.retrySynthesis(task)
        case .delete: vm.deleteTask(task)
        }
    }

    private func metaRow(_ t: TaskV3) -> some View {
        // The ↻ chip already says "recurring", so a plain recurring tag pill
        // beside it would say it twice. Internal rows carry the glyph instead.
        let showsRecurringChip = t.isRecurring && vm.lane != .internalTasks
        let tags = t.isRecurring ? t.tags.filter { !TaskRecurrence.isRecurringTag($0) } : t.tags
        return HStack(spacing: 6) {
            if showsRecurringChip { recurringChip(t) }
            ForEach(tags, id: \.name) { tag in
                tagPill(tag)
            }
            if let p = t.priorityLabel { metaPill(p, tint: themeManager.dangerColor) }
            if let d = t.dueDate, !d.isEmpty { metaPill(d, tint: themeManager.accentColor) }
            Spacer(minLength: 0)
        }
    }

    /// Web ↻ Recurring chip: the cadence is secondary text, never the raw cron.
    private func recurringChip(_ t: TaskV3) -> some View {
        HStack(spacing: 4) {
            Text("↻ Recurring").font(.themed(11, weight: .semibold))
            if t.scheduleCron != nil {
                Text(t.recurrenceDescription)
                    .font(.themed(11))
                    .opacity(0.8)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(themeManager.infoColor.opacity(0.12))
        .foregroundColor(themeManager.infoColor)
        .clipShape(Capsule())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Recurring: \(t.recurrenceDescription)")
        .accessibilityIdentifier("task-recurring-chip-\(t.id)")
    }

    /// Origin line (web parity): agent · #thread · relative updated-time, plus the
    /// internal-lane lifecycle + synthesis badges.
    private func originRow(_ t: TaskV3) -> some View {
        HStack(spacing: 6) {
            if !t.agentId.isEmpty {
                Label(agentName(t.agentId), systemImage: "person.crop.circle")
                    .labelStyle(.titleAndIcon)
                    .lineLimit(1)
            }
            Text("#\(t.uiThreadId)").lineLimit(1)
            if let rel = relativeUpdated(t) { Text("· \(rel)") }
            Spacer(minLength: 0)
            if vm.lane == .internalTasks { lifecyclePill(t) }
            if t.synthesisPending {
                synthPill("synthesizing…", tint: themeManager.accentColor, action: nil)
            } else if t.synthesisFailed {
                synthPill(vm.retryingSynthesisId == t.id ? "retrying…" : "synth failed · retry",
                          tint: themeManager.dangerColor) { vm.retrySynthesis(t) }
            }
        }
        .font(.themed(11))
        .foregroundColor(themeManager.secondaryTextColor)
    }

    /// Map an agent id to its display name when known (falls back to the id).
    private func agentName(_ id: String) -> String {
        vm.agents.first(where: { $0.id == id })?.name ?? id
    }

    private func relativeUpdated(_ t: TaskV3) -> String? {
        guard let date = t.updatedAtDate else { return nil }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: Date())
    }

    private func lifecyclePill(_ t: TaskV3) -> some View {
        let badge = t.lifecycleBadge
        let tint: Color = {
            switch badge.kind {
            case .persistent: return themeManager.secondaryTextColor
            case .internalTask: return themeManager.accentColor
            case .chat: return themeManager.infoColor
            case .debug: return themeManager.warningColor
            }
        }()
        return Text(badge.label)
            .font(.themed(10, weight: .semibold))
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(tint.opacity(0.14))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    @ViewBuilder private func synthPill(_ label: String, tint: Color, action: (() -> Void)?) -> some View {
        if let action = action {
            Button(action: action) { synthPillLabel(label, tint: tint) }
                .buttonStyle(.plain)
                .disabled(vm.retryingSynthesisId != nil)
        } else {
            synthPillLabel(label, tint: tint)
        }
    }

    private func synthPillLabel(_ label: String, tint: Color) -> some View {
        Text(label)
            .font(.themed(10, weight: .semibold))
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(tint.opacity(0.14))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    // MARK: - Actions

    private func taskCardActionRow(_ t: TaskV3) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                taskStateActions(t)
                actionButton(
                    "Actions",
                    systemImage: "ellipsis.circle",
                    taskID: t.id,
                    identifier: "actions",
                    tint: themeManager.secondaryTextColor,
                    filled: false
                ) { selectedTaskActions = t }
            }
        }
        .contentShape(Rectangle())
    }

    @ViewBuilder private func taskStateActions(_ t: TaskV3) -> some View {
        let primary = t.primaryCardAction
        ForEach(t.visibleCardActions) { action in
            // The primary action is filled; everything else is secondary. A
            // paused card's Reset stays quiet even when it is the only button.
            let filled = action == primary && !(action == .reset && t.status == "paused")
            switch action {
            case .viewPlan:
                detailsButton("View Plan", task: t, filled: filled)
            case .answerQuestion:
                detailsButton("Answer Question", task: t, filled: filled)
            case .reviewPlan:
                detailsButton("Review Plan", task: t, filled: filled)
            case .runPlan:
                actionButton("Run Plan", systemImage: "play.fill", taskID: t.id,
                             identifier: "run-plan", tint: themeManager.accentColor, filled: filled) {
                    vm.execute(t)
                }
            case .preplan:
                actionButton("PrePlan", systemImage: "sparkles", taskID: t.id,
                             identifier: "preplan", tint: themeManager.accentColor, filled: filled) {
                    vm.preplan(t)
                }
            case .runNow:
                actionButton("Run Now", systemImage: "play.fill", taskID: t.id,
                             identifier: "run-now", tint: themeManager.accentColor, filled: filled) {
                    vm.execute(t)
                }
            case .viewExecution:
                detailsButton("View Execution", task: t, filled: filled)
            case .viewQuestion:
                detailsButton("View Question", task: t, filled: filled)
            case .reset:
                actionButton("Reset to Ready", systemImage: "arrow.uturn.backward", taskID: t.id,
                             identifier: "reset", tint: themeManager.accentColor, filled: filled) {
                    vm.setStatus(t, "ready")
                }
            case .viewResult:
                actionButton("Result", systemImage: "doc.text.magnifyingglass", taskID: t.id,
                             identifier: "result", tint: themeManager.successColor, filled: filled) {
                    openResult(t)
                }
            case .publishToNotes:
                publishButton(t, filled: filled)
            }
        }
    }

    private func detailsButton(_ label: String, task: TaskV3, filled: Bool = true) -> some View {
        actionButton(label, systemImage: "arrow.up.right.square", taskID: task.id,
                     identifier: "details", tint: themeManager.accentColor, filled: filled) {
            selectedTask = task
        }
    }

    /// Open the detail straight on the Output tab's Result card.
    private func openResult(_ task: TaskV3) {
        detailFocus = .result
        selectedTask = task
    }

    private func publishButton(_ task: TaskV3, filled: Bool) -> some View {
        let publishing = notePublisher.publishingTaskID == task.id
        return actionButton(publishing ? "Publishing…" : "Publish to Notes",
                            systemImage: "note.text.badge.plus", taskID: task.id,
                            identifier: "publish-notes", tint: themeManager.accentColor,
                            filled: filled) {
            Task { await notePublisher.publish(taskID: task.id) }
        }
        .disabled(notePublisher.publishingTaskID != nil)
    }

    private func runtimeExecutionId(_ task: TaskV3) -> String? {
        guard ["running", "planning", "paused"].contains(task.status) else { return nil }
        return task.activeExecutionIdForControls
    }

    private func actionButton(
        _ label: String,
        systemImage: String,
        taskID: String,
        identifier: String,
        tint: Color,
        filled: Bool = true,
        action: @escaping () -> Void
    ) -> some View {
        let busy = vm.mutatingTaskID == taskID
        return Button(action: action) {
            HStack(spacing: 5) {
                if busy {
                    ProgressView().controlSize(.small)
                        .tint(filled ? themeManager.contrastingTextColor(for: tint) : tint)
                }
                else { Image(systemName: systemImage).font(.system(size: 11, weight: .semibold)) }
                Text(busy ? "Working…" : label)
            }
            .font(.themed(12, weight: .bold))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(filled ? tint : tint.opacity(0.12))
            .foregroundColor(filled ? themeManager.contrastingTextColor(for: tint) : tint)
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .overlay {
                if !filled {
                    RoundedRectangle(cornerRadius: 7).stroke(tint.opacity(0.25), lineWidth: 1)
                }
            }
        }
        .buttonStyle(.plain)
        .disabled(vm.mutatingTaskID != nil)
        .accessibilityIdentifier("task-action-\(identifier)-\(taskID)")
    }

    /// Read-only per-status counts for the active lane (web parity ledger). Only
    /// non-zero groups render.
    private var statusLedger: some View {
        let groups: [(String, [String])] = [
            ("Paused", ["paused"]), ("Pending", ["pending"]), ("Planning", ["planning"]),
            ("Ready", ["ready"]), ("Running", ["running"]), ("Failed", ["failed", "cancelled"])
        ]
        let present = groups.compactMap { g -> (String, Int)? in
            let n = g.1.reduce(0) { $0 + vm.count(status: $1) }
            return n > 0 ? (g.0, n) : nil
        }
        return Group {
            if !present.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(present, id: \.0) { item in
                            HStack(spacing: 4) {
                                Text(item.0).font(.themed(11))
                                Text("\(item.1)").font(.themed(11, weight: .bold))
                            }
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .background(themeManager.surfaceColor)
                            .foregroundColor(themeManager.secondaryTextColor)
                            .clipShape(Capsule())
                        }
                    }
                    .padding(.horizontal)
                }
                .padding(.bottom, 8)
            }
        }
    }

    // MARK: - Small pieces

    /// `badge` renders only when the caller has a real number. `nil` means the
    /// count is unknown and the chip stays bare — never a stand-in 0.
    private func chip(_ label: String, badge: Int? = nil, active: Bool,
                      action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Text(label)
                if let badge {
                    Text("\(badge)")
                        .font(.themed(11, weight: .semibold))
                        .monospacedDigit()
                        .opacity(active ? 0.85 : 0.6)
                }
            }
            .font(.themed(13, weight: .medium))
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(active ? themeManager.accentColor : themeManager.surfaceColor)
            .foregroundColor(active ? themeManager.onAccentColor : themeManager.textColor)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(themeManager.secondaryTextColor.opacity(active ? 0 : 0.2), lineWidth: 1))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(badge.map { "\(label), \($0) tasks" } ?? label)
    }

    private func statusBadge(_ t: TaskV3) -> some View {
        Text(t.statusLabel)
            .font(.themed(11, weight: .bold))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(t.statusColor.opacity(0.15))
            .foregroundColor(t.statusColor)
            .clipShape(Capsule())
    }

    private func tagPill(_ tag: TaskTag) -> some View {
        Text(tag.name)
            .font(.themed(11, weight: .medium))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background((tagColor(tag)).opacity(0.15))
            .foregroundColor(tagColor(tag))
            .clipShape(Capsule())
    }

    private func metaPill(_ label: String, tint: Color) -> some View {
        Text(label)
            .font(.themed(11, weight: .medium))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.12))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    private func tagColor(_ tag: TaskTag) -> Color {
        if let hex = tag.color, !hex.isEmpty { return Color(hex: hex) }
        return themeManager.accentColor
    }

    private func swipeTint(_ action: TaskCardSwipeAction) -> Color {
        switch action {
        case .markComplete: return themeManager.successColor
        case .markNotDone: return themeManager.infoColor
        case .reset: return themeManager.discoveryColor
        case .cancel: return themeManager.warningColor
        case .delete: return themeManager.dangerColor
        }
    }
}

/// Mobile counterpart to the web task-card overflow and inline editors. The
/// card keeps frequent state transitions visible; this sheet owns the denser
/// metadata, plan, schedule, and destructive actions that need room and clear
/// labels on a phone.
private struct TaskCardActionsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @StateObject private var themeManager = ThemeManager.shared
    @StateObject private var notePublisher = TaskNotePublishViewModel()
    @ObservedObject var viewModel: TasksViewModel

    let task: TaskV3
    let openDetails: () -> Void
    /// Phase 7: open the convert-to-monitor composer (shown only for
    /// eligible tasks — persistent, not already a monitor).
    let requestConvert: () -> Void
    let requestCancel: () -> Void
    let requestDelete: () -> Void

    @State private var descriptionText: String
    @State private var customDueDate: Date
    @State private var priority: String
    @State private var tagText = ""
    @State private var cron: String
    @State private var timezone: String
    @State private var maxRecords: String
    @State private var maxDays: String

    init(
        task: TaskV3,
        viewModel: TasksViewModel,
        openDetails: @escaping () -> Void,
        requestConvert: @escaping () -> Void,
        requestCancel: @escaping () -> Void,
        requestDelete: @escaping () -> Void
    ) {
        self.task = task
        self.viewModel = viewModel
        self.openDetails = openDetails
        self.requestConvert = requestConvert
        self.requestCancel = requestCancel
        self.requestDelete = requestDelete
        _descriptionText = State(initialValue: task.description)
        _customDueDate = State(initialValue: Self.parseDate(task.dueDate) ?? Date())
        _priority = State(initialValue: task.priority ?? "")
        _cron = State(initialValue: task.scheduleCron ?? "")
        _timezone = State(initialValue: task.scheduleTimezone ?? TimeZone.current.identifier)
        _maxRecords = State(initialValue: task.scheduleRetentionMaxRecords.map(String.init) ?? "")
        _maxDays = State(initialValue: task.scheduleRetentionMaxDays.map(String.init) ?? "")
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(task.title.isEmpty ? "Untitled task" : task.title)
                            .font(.themed(17, weight: .bold))
                            .foregroundColor(themeManager.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                        Text(task.statusLabel)
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(task.statusColor)
                    }
                    Button(action: openDetails) {
                        Label("Open full task", systemImage: "arrow.up.right.square")
                    }
                    quickActions
                    if task.canPublishToNotes {
                        Button {
                            Task { await notePublisher.publish(taskID: task.id) }
                        } label: {
                            if notePublisher.publishingTaskID == task.id {
                                HStack(spacing: 8) {
                                    ProgressView().controlSize(.small)
                                    Text("Publishing…")
                                }
                            } else {
                                Label("Publish to Notes", systemImage: "note.text.badge.plus")
                            }
                        }
                        .disabled(notePublisher.publishingTaskID != nil)
                        .accessibilityIdentifier("task-actions-publish-notes")
                    }
                    if let message = notePublisher.successMessage {
                        Label(message, systemImage: "checkmark.circle.fill")
                            .font(.themed(13, weight: .semibold))
                            .foregroundColor(themeManager.successColor)
                            .accessibilityIdentifier("task-actions-publish-notes-success")
                    }
                    if task.canConvertToMonitor {
                        // Phase 7: explicit, user-driven conversion only —
                        // never inferred from the task's title or prose.
                        Button(action: requestConvert) {
                            Label("Convert to monitor", systemImage: "binoculars")
                        }
                        .accessibilityIdentifier("task-actions-convert-monitor")
                    }
                }

                if task.hasPlan {
                    Section("Plan") {
                        if task.planAwaitsReview {
                            Button { viewModel.approvePlan(task) } label: {
                                Label("Approve plan", systemImage: "checkmark.seal")
                            }
                            Button(role: .destructive) { viewModel.rejectPlan(task) } label: {
                                Label("Reject plan", systemImage: "xmark.seal")
                            }
                        }
                        Button { viewModel.replan(task) } label: {
                            Label("Replan", systemImage: "arrow.triangle.2.circlepath")
                        }
                    }
                }

                Section("Description") {
                    TextEditor(text: $descriptionText)
                        .frame(minHeight: 96)
                        .accessibilityIdentifier("task-actions-description")
                    Button("Save description") {
                        viewModel.updateTask(task, fields: ["description": descriptionText])
                    }
                    .disabled(descriptionText == task.description || viewModel.mutatingTaskID != nil)
                }

                Section("Priority") {
                    Picker("Priority", selection: $priority) {
                        Text("None").tag("")
                        Text("P1 · Urgent").tag("p1")
                        Text("P2 · High").tag("p2")
                        Text("P3 · Medium").tag("p3")
                        Text("P4 · Low").tag("p4")
                    }
                    .onChange(of: priority) { _, value in
                        viewModel.updateTask(task, fields: ["priority": value.isEmpty ? nil : value])
                    }
                }

                Section("Due date") {
                    HStack(spacing: 8) {
                        shortcutButton("Today", days: 0)
                        shortcutButton("Tomorrow", days: 1)
                        shortcutButton("Next week", days: 7)
                    }
                    DatePicker("Custom", selection: $customDueDate, displayedComponents: .date)
                    Button("Set custom date") {
                        viewModel.updateTask(task, fields: ["due_date": Self.formatDate(customDueDate)])
                    }
                    if task.dueDate != nil {
                        Button("Remove due date", role: .destructive) {
                            viewModel.updateTask(task, fields: ["due_date": nil])
                        }
                    }
                }

                Section("Tags") {
                    ForEach(task.tags, id: \.name) { tag in
                        HStack {
                            Label(tag.name, systemImage: "tag")
                            Spacer()
                            Button(role: .destructive) { viewModel.removeTag(task, name: tag.name) } label: {
                                Image(systemName: "minus.circle")
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Remove \(tag.name)")
                        }
                    }
                    HStack {
                        TextField("Add a tag", text: $tagText)
                        Button("Add") {
                            viewModel.addTag(task, name: tagText)
                            tagText = ""
                        }
                        .disabled(tagText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }

                Section("Schedule") {
                    TextField("Cron · 0 9 * * *", text: $cron)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Timezone", text: $timezone)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Maximum saved runs (optional)", text: $maxRecords)
                        .keyboardType(.numberPad)
                    TextField("Maximum history days (optional)", text: $maxDays)
                        .keyboardType(.numberPad)
                    Button(task.scheduleCron != nil && cron.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                           ? "Remove schedule" : "Save schedule") {
                        viewModel.updateSchedule(
                            task,
                            cron: cron,
                            timezone: timezone,
                            maxRecords: maxRecords,
                            maxDays: maxDays
                        )
                    }
                    .disabled(task.scheduleCron == nil && cron.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                Section("Task management") {
                    if task.status != "completed" && task.status != "cancelled" {
                        Button(role: .destructive, action: requestCancel) {
                            Label("Cancel task", systemImage: "xmark.circle")
                        }
                    }
                    Button(role: .destructive, action: requestDelete) {
                        Label("Delete…", systemImage: "trash")
                    }
                }
            }
            .disabled(viewModel.mutatingTaskID != nil || notePublisher.publishingTaskID != nil)
            .navigationTitle("Task actions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .alert(notePublisher.errorMessage == nil ? "Task action failed" : "Publish to Notes failed", isPresented: Binding(
                get: { actionErrorMessage != nil },
                set: {
                    if !$0 {
                        viewModel.actionErrorMessage = nil
                        notePublisher.errorMessage = nil
                    }
                }
            )) {
                Button("OK", role: .cancel) {
                    viewModel.actionErrorMessage = nil
                    notePublisher.errorMessage = nil
                }
            } message: {
                Text(actionErrorMessage ?? "The task action could not be completed.")
            }
        }
    }

    private var actionErrorMessage: String? {
        notePublisher.errorMessage ?? viewModel.actionErrorMessage
    }

    @ViewBuilder private var quickActions: some View {
        switch task.status.lowercased() {
        case "pending":
            Button { viewModel.preplan(task) } label: { Label("PrePlan", systemImage: "sparkles") }
            Button { viewModel.execute(task) } label: { Label("Run Now", systemImage: "play.fill") }
        case "ready":
            Button { viewModel.execute(task) } label: {
                Label(task.planStatus == "approved" ? "Run Plan" : "Run Now", systemImage: "play.fill")
            }
        case "paused", "failed", "cancelled", "canceled":
            Button { viewModel.setStatus(task, "ready") } label: {
                Label("Reset to Ready", systemImage: "arrow.uturn.backward")
            }
        case "running", "planning", "executing", "queued":
            if task.activeExecutionIdForControls != nil {
                Button(role: .destructive) { viewModel.cancelExecution(task) } label: {
                    Label("Stop execution", systemImage: "stop.fill")
                }
            }
        case "completed":
            Button { viewModel.setStatus(task, "ready") } label: {
                Label("Mark not done", systemImage: "arrow.uturn.backward")
            }
        default:
            EmptyView()
        }
        if task.canMarkCompleteManually && task.status != "completed" {
            Button { viewModel.setStatus(task, "completed") } label: {
                Label("Mark complete", systemImage: "checkmark.circle")
            }
        }
    }

    private func shortcutButton(_ label: String, days: Int) -> some View {
        Button(label) {
            viewModel.updateTask(task, fields: ["due_date": viewModel.isoDate(offset: days)])
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
    }

    private static func parseDate(_ value: String?) -> Date? {
        guard let value else { return nil }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter.date(from: value)
    }

    private static func formatDate(_ value: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter.string(from: value)
    }
}

private extension TaskCardSwipeAction {
    var title: String {
        switch self {
        case .markComplete: return "Complete"
        case .markNotDone: return "Not done"
        case .reset: return "Reset"
        case .cancel: return "Cancel"
        case .delete: return "Delete"
        }
    }

    var systemImage: String {
        switch self {
        case .markComplete: return "checkmark.circle.fill"
        case .markNotDone: return "arrow.uturn.backward.circle.fill"
        case .reset: return "arrow.counterclockwise.circle.fill"
        case .cancel: return "xmark.circle.fill"
        case .delete: return "trash.fill"
        }
    }

}

private struct PendingTaskDestructiveAction: Identifiable {
    enum Kind: String { case cancel, delete }

    let task: TaskV3
    let kind: Kind

    var id: String { "\(kind.rawValue):\(task.id)" }
    var isInternal: Bool {
        ["internal", "ephemeral_owned_by_chat", "internal_debug"].contains(task.lifecycle ?? "")
    }
    var title: String { kind == .cancel ? "Cancel this task?" : "Delete this task?" }
    var confirmLabel: String { kind == .cancel ? "Cancel task" : "Delete task" }
    var message: String {
        let name = task.title.isEmpty ? "This task" : "\"\(task.title)\""
        switch kind {
        case .cancel: return "\(name) will stop and move to Cancelled."
        case .delete: return "\(name) and its saved task state will be removed."
        }
    }
}

private struct TaskSwipeCardAction: Identifiable {
    let id: String
    let title: String
    let systemImage: String
    let color: Color
    let run: () -> Void
}

/// ScrollView cards do not receive SwiftUI's List-only `.swipeActions`, so the
/// Tasks surface uses the same mobile rail interaction as Today while retaining
/// its card layout and nested execution controls.
private struct SwipeCardWidthKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

private struct TaskSwipeActionCard<Content: View>: View {
    private static var actionWidth: CGFloat { 76 }

    let itemID: String
    let leadingActions: [TaskSwipeCardAction]
    let trailingActions: [TaskSwipeCardAction]
    @ViewBuilder let content: Content

    @State private var restingOffset: CGFloat = 0
    @State private var dragOffset: CGFloat = 0
    @State private var horizontalDragActive = false
    @State private var suppressContentTap = false
    @State private var tapSuppressionGeneration = 0
    @State private var cardWidth: CGFloat = 0
    @ObservedObject private var theme = ThemeManager.shared

    private var leadingRevealWidth: CGFloat {
        Self.actionWidth * CGFloat(leadingActions.count)
    }

    private var trailingRevealWidth: CGFloat {
        Self.actionWidth * CGFloat(trailingActions.count)
    }

    private var rawOffset: CGFloat { restingOffset + dragOffset }

    /// Full-swipe commit threshold: swipe past ~half the card (or well past the rail).
    private var trailingCommitThreshold: CGFloat { max(trailingRevealWidth + 96, cardWidth * 0.5) }
    private var leadingCommitThreshold: CGFloat { max(leadingRevealWidth + 96, cardWidth * 0.5) }

    /// The card follows the finger, but past the revealed rail it rubber-bands (55 %
    /// resistance, capped near the card edge) so a long swipe *visibly travels* — the
    /// primary action's colour then fills the widening gap (see `fullSwipePanel`).
    private var visibleOffset: CGFloat {
        let raw = rawOffset
        let cap = cardWidth > 0 ? cardWidth * 0.92 : max(trailingRevealWidth, leadingRevealWidth) + 160
        if raw < -trailingRevealWidth {
            return max(-cap, -trailingRevealWidth - ((-trailingRevealWidth) - raw) * 0.55)
        }
        if raw > leadingRevealWidth {
            return min(cap, leadingRevealWidth + (raw - leadingRevealWidth) * 0.55)
        }
        return raw
    }

    /// Armed = a release will commit the default action. Keyed off the real finger
    /// travel (not the rubber-banded card), so the brightening cue matches the finger.
    private var trailingArmed: Bool { rawOffset <= -trailingCommitThreshold }
    private var leadingArmed: Bool { rawOffset >= leadingCommitThreshold }

    private var actionSignature: String {
        (leadingActions.map(\.id) + ["|"] + trailingActions.map(\.id)).joined(separator: ",")
    }

    var body: some View {
        ZStack(alignment: .trailing) {
            HStack(spacing: 0) {
                ForEach(leadingActions) { action in
                    actionButton(action) {
                        close()
                        action.run()
                    }
                    .accessibilityHidden(restingOffset <= 0)
                    .allowsHitTesting(restingOffset > 0)
                }
                Spacer(minLength: 0)
                ForEach(trailingActions) { action in
                    actionButton(action) {
                        close()
                        action.run()
                    }
                    .accessibilityHidden(restingOffset >= 0)
                    .allowsHitTesting(restingOffset < 0)
                }
            }

            // Full-swipe panels: past the rail, the default (first) action's colour
            // fills the widening gap and brightens once armed — the Mail-style cue that
            // releasing commits. Shown only during a full swipe (past the rail).
            if let primary = trailingActions.first, visibleOffset < -trailingRevealWidth {
                fullSwipePanel(primary, armed: trailingArmed, width: -visibleOffset, trailing: true)
            }
            if let primary = leadingActions.first, visibleOffset > leadingRevealWidth {
                fullSwipePanel(primary, armed: leadingArmed, width: visibleOffset, trailing: false)
            }

            content
                // Move the close-overlay with the visible card. Applying it
                // after `offset` leaves its layout frame over the revealed rail
                // and makes apparently hittable action buttons swallow taps.
                .overlay {
                    if restingOffset != 0 {
                        Color.clear
                            .contentShape(Rectangle())
                            .onTapGesture { close() }
                    }
                }
                .offset(x: visibleOffset)
                .allowsHitTesting(!suppressContentTap)
        }
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .background(
            GeometryReader { geo in
                Color.clear.preference(key: SwipeCardWidthKey.self, value: geo.size.width)
            }
        )
        .onPreferenceChange(SwipeCardWidthKey.self) { cardWidth = $0 }
        .contentShape(Rectangle())
        .simultaneousGesture(swipeGesture)
        .onChange(of: actionSignature) { _, _ in close() }
    }

    private var swipeGesture: some Gesture {
        DragGesture(minimumDistance: 16)
            .onChanged { value in
                guard abs(value.translation.width) > abs(value.translation.height) else { return }
                horizontalDragActive = true
                suppressContentTap = true
                dragOffset = value.translation.width
            }
            .onEnded { value in
                let handledHorizontalDrag = horizontalDragActive
                    || abs(value.translation.width) > abs(value.translation.height)
                dragOffset = 0
                horizontalDragActive = false
                guard handledHorizontalDrag else {
                    releaseContentTapSuppression()
                    return
                }
                let projected = restingOffset + value.predictedEndTranslation.width
                let raw = restingOffset + value.translation.width
                // Long/fast swipe past the commit threshold → fire the edge's default
                // (first) action in one gesture. Commits when the finger passed the
                // threshold (armed) OR a fast flick projects past it. Destructive actions
                // self-guard (their handler opens a confirmation), so this stays safe.
                if trailingRevealWidth > 0, let primary = trailingActions.first,
                   raw <= -trailingCommitThreshold || projected < -trailingCommitThreshold {
                    withAnimation(.snappy(duration: 0.22)) { restingOffset = 0 }
                    primary.run()
                } else if leadingRevealWidth > 0, let primary = leadingActions.first,
                          raw >= leadingCommitThreshold || projected > leadingCommitThreshold {
                    withAnimation(.snappy(duration: 0.22)) { restingOffset = 0 }
                    primary.run()
                } else {
                    withAnimation(.snappy(duration: 0.22)) {
                        if trailingRevealWidth > 0, projected < -(trailingRevealWidth * 0.35) {
                            restingOffset = -trailingRevealWidth
                        } else if leadingRevealWidth > 0, projected > leadingRevealWidth * 0.35 {
                            restingOffset = leadingRevealWidth
                        } else {
                            restingOffset = 0
                        }
                    }
                }
                releaseContentTapSuppression()
            }
    }

    private func actionButton(_ action: TaskSwipeCardAction, run: @escaping () -> Void) -> some View {
        Button(action: run) {
            VStack(spacing: 5) {
                Image(systemName: action.systemImage).font(.system(size: 15, weight: .semibold))
                Text(action.title).font(.themed(10, weight: .semibold)).lineLimit(1)
            }
            .foregroundColor(theme.contrastingTextColor(for: action.color))
            .frame(width: Self.actionWidth)
            .frame(maxHeight: .infinity)
            .background(action.color)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("task-swipe-\(action.id)-\(itemID)")
    }

    /// The expanding coloured panel shown during a full swipe: the primary action's
    /// colour fills the revealed gap (`width`), icon anchored near the card edge, and
    /// brightens + grows the icon once `armed`. Visual only (no hit-testing).
    private func fullSwipePanel(_ action: TaskSwipeCardAction, armed: Bool, width: CGFloat, trailing: Bool) -> some View {
        HStack(spacing: 0) {
            if trailing { Spacer(minLength: 0) }
            VStack(spacing: 5) {
                Image(systemName: action.systemImage).font(.system(size: armed ? 18 : 15, weight: .semibold))
                Text(action.title).font(.themed(10, weight: .semibold)).lineLimit(1)
            }
            .foregroundColor(theme.contrastingTextColor(for: action.color))
            .frame(width: max(Self.actionWidth, width), alignment: trailing ? .leading : .trailing)
            .frame(maxHeight: .infinity)
            .background(action.color.opacity(armed ? 1.0 : 0.85))
            if !trailing { Spacer(minLength: 0) }
        }
        .allowsHitTesting(false)
        .animation(.snappy(duration: 0.14), value: armed)
    }

    private func close() {
        tapSuppressionGeneration += 1
        suppressContentTap = false
        dragOffset = 0
        horizontalDragActive = false
        withAnimation(.snappy(duration: 0.18)) { restingOffset = 0 }
    }

    private func releaseContentTapSuppression() {
        tapSuppressionGeneration += 1
        let generation = tapSuppressionGeneration
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            guard tapSuppressionGeneration == generation else { return }
            suppressContentTap = false
        }
    }
}
