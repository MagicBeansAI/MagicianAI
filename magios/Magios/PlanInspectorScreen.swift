import SwiftUI

/// The Plan Inspector (design §7 of
/// `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`): a
/// full screen over a task's plan — Graph (default) · Steps · History, with
/// Analysis, Slots, Attempts and Clarifications behind More. Port of the web
/// `ExecutionPlanInspector.svelte` / `PlanGraphView.svelte`.
///
/// Opened full screen (own stack, Close) by `planInspectorHost` /
/// `planInspectorDestination`), or presented with `PlanInspectorSheetHost`
/// where no stack hosts it.
struct PlanInspectorScreen: View {
    @StateObject private var model: PlanInspectorModel
    /// Set when presented modally: a Close button.
    var onClose: (() -> Void)? = nil

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var selectedStep: PlanStepSelection?
    @State private var pane: PlanInspectorPane?
    @State private var askLabel = false
    @State private var saveLabel = ""
    @State private var confirmDiscard = false
    @State private var confirmReject = false
    @State private var confirmReplan = false
    @State private var showDocument = false
    @State private var started = false
    @State private var visible = false

    init(route: PlanInspectorRoute, onClose: (() -> Void)? = nil) {
        _model = StateObject(wrappedValue: PlanInspectorModel(route: route))
        self.onClose = onClose
    }

    init(model: PlanInspectorModel, onClose: (() -> Void)? = nil) {
        _model = StateObject(wrappedValue: model)
        self.onClose = onClose
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Picker("View", selection: $model.tab) {
                ForEach(PlanInspectorTab.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .accessibilityIdentifier("plan-inspector-tabs")
            banners
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .background(theme.washGradient.ignoresSafeArea())
        // Same chrome as chat: the theme's wash under a theme-coloured bar.
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .safeAreaInset(edge: .bottom) { if model.showsApprovalBar && model.preview == nil { approvalBar } }
        // System bordered buttons default to capsules; Magician's are rounded squares.
        .buttonBorderShape(.roundedRectangle(radius: 10))
        .navigationTitle(model.isEditing ? "Editing plan" : "Plan")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(model.isEditing)
        .toolbar { toolbar }
        .sheet(item: $selectedStep) { PlanStepDetailSheet(model: model, stepId: $0.id) }
        .navigationDestination(item: $pane) { PlanInspectorPaneView(model: model, pane: $0) }
        .sheet(isPresented: $showDocument) { PlanDocumentSheet(markdown: model.route.planDocument ?? "") }
        .alert("Save plan", isPresented: $askLabel) {
            TextField("Label (optional)", text: $saveLabel)
            Button("Cancel", role: .cancel) {}
            Button("Save") { Task { if await model.save(label: saveLabel) { saveLabel = "" } } }
        } message: {
            Text("Saving keeps the current plan as a restorable version, then writes your changes.")
        }
        .confirmationDialog("Discard your changes?", isPresented: $confirmDiscard, titleVisibility: .visible) {
            Button("Discard changes", role: .destructive) { model.discardEdits() }
            Button("Keep editing", role: .cancel) {}
        } message: { Text("Your edits to this plan have not been saved.") }
        .confirmationDialog("Reject this plan?", isPresented: $confirmReject, titleVisibility: .visible) {
            Button("Reject plan", role: .destructive) { Task { await model.decide(.reject) } }
        } message: { Text("A rejected plan will not run.") }
        .confirmationDialog("Plan this task again?", isPresented: $confirmReplan, titleVisibility: .visible) {
            Button("Replan", role: .destructive) { Task { await model.replan() } }
        } message: { Text("The planner starts over; the current plan stays in History.") }
        .interactiveDismissDisabled(model.isEditing)
        .task {
            guard !started else { return }
            started = true
            async let plan: Void = model.load()
            async let history: Void = model.loadVersions()
            _ = await (plan, history)
            applyLaunchState()
            if visible { model.startLive() }
        }
        .onAppear { visible = true; if started { model.startLive() } }
        .onDisappear { visible = false; model.stopLive() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active, visible { model.startLive() } else if phase != .active { model.stopLive() }
        }
        .onChange(of: model.notice) { _, notice in
            guard notice != nil else { return }
            Task {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                if model.notice == notice { model.notice = nil }
            }
        }
    }

    // MARK: Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let title = model.route.title, !title.isEmpty {
                Text(title).font(.themed(15, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(2)
            }
            if let summary = model.shownGraph?.summary {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        if let status = model.status { PlanTag(text: status.label, color: statusColor(status)) }
                        PlanTag(text: "\(summary.steps) step\(summary.steps == 1 ? "" : "s")", color: theme.infoColor)
                        PlanTag(text: "\(summary.edges) edge\(summary.edges == 1 ? "" : "s")")
                        PlanTag(text: "\(summary.unresolved) unresolved",
                                color: summary.unresolved > 0 ? theme.warningColor : theme.successColor)
                        PlanTag(text: "Confidence \(PlanStyle.percent(summary.confidence))", mono: true)
                        if let strategy = summary.strategy { PlanTag(text: strategy) }
                    }
                }
                .accessibilityElement(children: .combine)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private func statusColor(_ status: PlanStatus) -> Color {
        switch status {
        case .approved: return theme.successColor
        case .draft, .eliciting: return theme.warningColor
        case .planning: return theme.statusRunningColor
        case .rejected, .failed: return theme.dangerColor
        }
    }

    @ViewBuilder
    private var banners: some View {
        if let error = model.errorMessage, model.record != nil {
            banner(error, color: theme.dangerColor, icon: "exclamationmark.triangle.fill")
        }
        if let notice = model.notice {
            banner(notice, color: theme.successColor, icon: "checkmark.circle.fill")
                .transition(reduceMotion ? .identity : .opacity)
        }
        if model.isEditing {
            banner(model.isDirty ? "Unsaved changes — tap Save to keep them." : "Editing. Tap a step to change it.",
                   color: theme.accentColor, icon: "pencil")
        }
    }

    private func banner(_ text: String, color: Color, icon: String) -> some View {
        Label(text, systemImage: icon)
            .font(.themed(12, weight: .medium))
            .foregroundColor(color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12).padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: Lively.chipRadius + 2, style: .continuous).fill(color.opacity(0.1)))
            .padding(.horizontal, 16).padding(.bottom, 6)
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        if model.tab == .history {
            PlanHistoryView(model: model)
        } else if let graph = model.shownGraph, !graph.steps.isEmpty || model.isEditing {
            switch model.tab {
            case .graph:
                VStack(spacing: 0) {
                    PlanGraphCanvas(graph: graph, marks: model.runMarks) { selectedStep = PlanStepSelection(id: $0) }
                    legend
                }
            default:
                PlanStepsView(
                    graph: graph, marks: model.runMarks, editing: model.isEditing,
                    onSelect: { selectedStep = PlanStepSelection(id: $0) },
                    onMove: { source, destination in model.editDraft { $0.moveSteps(fromOffsets: source, toOffset: destination) } },
                    onDelete: { offsets in model.editDraft { $0.removeSteps(atOffsets: offsets) } },
                    onAdd: { if let id = model.addStep() { selectedStep = PlanStepSelection(id: id) } })
            }
        } else if !model.hasLoaded || (model.isLoading && model.record == nil) {
            ProgressView("Loading plan…").frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = model.errorMessage, model.record == nil {
            unavailable(title: "Plan unavailable", icon: "exclamationmark.triangle", detail: error, retry: true)
        } else {
            unavailable(title: "No graph-backed plan yet", icon: "point.3.connected.trianglepath.dotted",
                        detail: emptyDetail, retry: true)
        }
    }

    private var emptyDetail: String {
        switch model.status {
        case .planning?: return "The planner is still working. This fills in when the plan is drafted."
        case .eliciting?: return "The planner is waiting on answers — see Clarifications."
        case .failed?: return model.record?.error ?? "Planning failed."
        default: return "This task has no plan graph to inspect."
        }
    }

    private func unavailable(title: String, icon: String, detail: String, retry: Bool) -> some View {
        ContentUnavailableView {
            Label(title, systemImage: icon)
        } description: {
            Text(detail)
        } actions: {
            if !model.pendingQuestions.isEmpty {
                Button("Answer \(model.pendingQuestions.count) question\(model.pendingQuestions.count == 1 ? "" : "s")") {
                    pane = .clarifications
                }
                .buttonStyle(ChipStyle(prominent: true))
            }
            if retry { Button("Refresh") { Task { await model.refresh() } }.buttonStyle(ChipStyle()) }
        }
    }

    private var legend: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 10) {
                legendDot(theme.successColor, "≥70%")
                legendDot(theme.warningColor, "40–70%")
                legendDot(theme.dangerColor, "<40%")
                legendBorder(dashed: true, "No tool")
                Text("▶\u{FE0E} Start").font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                legendBorder(dashed: false, "Delegated", color: theme.secondaryAccentColor)
            }
            .padding(.horizontal, 16).padding(.vertical, 8)
        }
        .background(theme.surfaceColor.opacity(0.6))
        .accessibilityHidden(true)
    }

    private func legendDot(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 4) {
            RoundedRectangle(cornerRadius: 3).fill(color.opacity(0.5)).frame(width: 12, height: 12)
            Text(text).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
        }
    }

    private func legendBorder(dashed: Bool, _ text: String, color: Color? = nil) -> some View {
        HStack(spacing: 4) {
            RoundedRectangle(cornerRadius: 3)
                .strokeBorder(color ?? theme.secondaryTextColor, style: StrokeStyle(lineWidth: 1.5, dash: dashed ? [3, 2] : []))
                .frame(width: 14, height: 12)
            Text(text).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
        }
    }

    // MARK: Approval

    private var approvalBar: some View {
        let busy = model.deciding != nil
        return HStack(spacing: 8) {
            Button { confirmReject = true } label: {
                Text("Reject").frame(maxWidth: .infinity, minHeight: Lively.tapMin)
            }
            .buttonStyle(.bordered).tint(theme.dangerColor)
            Button { Task { await model.decide(.approve) } } label: {
                Text(model.deciding == .approve ? "Approving…" : "Approve").frame(maxWidth: .infinity, minHeight: Lively.tapMin)
            }
            .buttonStyle(.bordered).tint(theme.accentColor)
            Button { Task { await model.decide(.approveAndRun) } } label: {
                Text(model.deciding == .approveAndRun ? "Starting…" : "Approve & Run")
                    .frame(maxWidth: .infinity, minHeight: Lively.tapMin)
            }
            .buttonStyle(.borderedProminent).tint(theme.accentFillColor)
            .accessibilityIdentifier("plan-approve-and-run")
        }
        .font(.themed(14, weight: .semibold))
        .lineLimit(1)
        .minimumScaleFactor(0.8)
        .disabled(busy)
        .padding(.horizontal, 16).padding(.vertical, 8)
        .background(.bar)
    }

    // MARK: Toolbar

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        if model.isEditing {
            ToolbarItem(placement: .topBarLeading) {
                Button("Done") { if model.isDirty { confirmDiscard = true } else { model.discardEdits() } }
                    .accessibilityHint("Leaves edit mode")
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button(model.isSaving ? "Saving…" : "Save") { askLabel = true }
                    .fontWeight(.semibold)
                    .disabled(model.isSaving || !model.isDirty)
                    .accessibilityIdentifier("plan-inspector-save")
            }
        } else {
            if let onClose {
                ToolbarItem(placement: .topBarLeading) { Button("Close", action: onClose) }
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button { Task { await model.refresh() } } label: {
                    Image(systemName: "arrow.clockwise").frame(minWidth: Lively.tapMin, minHeight: Lively.tapMin)
                }
                .disabled(model.isLoading)
                .accessibilityLabel("Refresh")
                if model.canEdit && model.tab != .history {
                    Button("Edit") { model.beginEditing() }
                        .accessibilityIdentifier("plan-inspector-edit")
                }
                moreMenu
            }
        }
    }

    private var moreMenu: some View {
        Menu {
            Section {
                ForEach(PlanInspectorPane.allCases) { item in
                    Button { pane = item } label: {
                        if item == .clarifications, !model.pendingQuestions.isEmpty {
                            Label("\(item.title) (\(model.pendingQuestions.count))", systemImage: item.icon)
                        } else {
                            Label(item.title, systemImage: item.icon)
                        }
                    }
                }
            }
            if let document = model.route.planDocument, !document.isEmpty {
                Button { showDocument = true } label: { Label("Plan document", systemImage: "doc.text") }
            }
            if model.canReplan {
                Section {
                    Button(role: .destructive) { confirmReplan = true } label: {
                        Label("Replan", systemImage: "arrow.triangle.2.circlepath")
                    }
                }
            }
        } label: {
            Image(systemName: "ellipsis.circle").frame(minWidth: Lively.tapMin, minHeight: Lively.tapMin)
        }
        .accessibilityLabel("More")
        .accessibilityIdentifier("plan-inspector-more")
    }

    // MARK: DEBUG launch state

    private func applyLaunchState() {
#if DEBUG
        guard PlanInspectorFixtures.ids.contains(model.route.taskId), let state = PlanInspectorFixtures.launchState else { return }
        switch state {
        case "steps": model.tab = .steps
        case "history": model.tab = .history
        case "history-preview":
            model.tab = .history
            if let first = model.versions.first { Task { await model.previewVersion(first) } }
        case "steps-edit":
            model.tab = .steps
            model.beginEditing()
        case "step-sheet":
            model.tab = .steps
            model.beginEditing()
            selectedStep = PlanStepSelection(id: "analyze")
        case "step-sheet-read": selectedStep = PlanStepSelection(id: "analyze")
        case "clarifications": pane = .clarifications
        case "slots": pane = .slots
        case "attempts": pane = .attempts
        case "analysis": pane = .analysis
        default: break
        }
#endif
    }
}

/// The step a sheet is open for.
struct PlanStepSelection: Identifiable, Equatable {
    let id: String
}

/// The plan's markdown document, kept as a secondary view of the plan.
struct PlanDocumentSheet: View {
    let markdown: String
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(LocalizedStringKey(markdown))
                    .font(.themed(15))
                    .foregroundColor(theme.textColor)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            .background(theme.washGradient.ignoresSafeArea())
            .navigationTitle("Plan document")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

// MARK: - Presentation

/// Open the Plan Inspector from anywhere under a host that registered one
/// (`planInspectorHost`).
struct OpenPlanInspectorAction {
    let handler: (PlanInspectorRoute) -> Void
    func callAsFunction(_ route: PlanInspectorRoute) { handler(route) }
}

private struct OpenPlanInspectorKey: EnvironmentKey {
    static let defaultValue: OpenPlanInspectorAction? = nil
}

extension EnvironmentValues {
    /// Set by `planInspectorHost`; nil where no `NavigationStack` hosts the inspector.
    var openPlanInspector: OpenPlanInspectorAction? {
        get { self[OpenPlanInspectorKey.self] }
        set { self[OpenPlanInspectorKey.self] = newValue }
    }
}

extension View {
    /// Make this view's `NavigationStack` able to push the Plan Inspector:
    /// descendants call `@Environment(\.openPlanInspector)`, `route` drives
    /// the push. Apply it to content inside the stack (not in a lazy container).
    func planInspectorHost(_ route: Binding<PlanInspectorRoute?>) -> some View {
        environment(\.openPlanInspector, OpenPlanInspectorAction { route.wrappedValue = $0 })
            .planInspectorDestination(route)
    }

    /// Open the Plan Inspector for whichever route is set as its own full
    /// screen (own stack, Close), not a push: a pushed screen sits under the
    /// tab bar, which covered the Reject · Approve · Approve & Run bar.
    func planInspectorDestination(_ route: Binding<PlanInspectorRoute?>) -> some View {
        fullScreenCover(item: route) { PlanInspectorSheetHost(route: $0) }
    }
}

/// The inspector presented modally, for a caller with no `NavigationStack`:
/// the same screen in its own stack with Close.
struct PlanInspectorSheetHost: View {
    let route: PlanInspectorRoute
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            PlanInspectorScreen(route: route, onClose: { dismiss() })
        }
    }
}
