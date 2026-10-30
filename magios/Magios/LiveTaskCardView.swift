import SwiftUI
import UIKit

/// The chat's live task card: one card for a task's whole life, from planning
/// through plan review to the run's end. Designed for a phone (44 pt targets,
/// no hover affordance, Reduce Motion respected), reading only
/// `LiveTaskCardState` + `LiveTaskCardRules`.
///
/// Design §2 of `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
struct LiveTaskCardView: View {
    let messageId: String
    let task: TaskStatusModel
    /// The chat session, for session-owned output links.
    let sessionId: String?
    @ObservedObject var theme: ThemeManager
    /// Inspect: the task detail entry (the run when known).
    let onInspect: () -> Void
    /// Review / Plan graph: the task's plan.
    let onReviewPlan: (String) -> Void

    @StateObject private var loader = LiveTaskCardLoader()
    @ObservedObject private var usageStore = TaskUsageStore.shared
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    @State private var expanded = false
    @State private var visible = false
    @State private var confirmCancel = false
    @State private var busy: BusyAction?
    @State private var toast: String?
    @State private var toastToken = 0
    @State private var openedArtifact: ArtifactRef?
    @State private var sharePayload: SharePayload?

    private enum BusyAction: Equatable { case cancel, run, approveAndRun }

    /// The chat's model, reconciled with the task's stored status the card
    /// fetched (the chat posts no message when a plan is approved).
    private var current: TaskStatusModel {
        loader.storeStatus.map { LiveTaskCardRules.reconciled(task, storeStatus: $0) } ?? task
    }
    private var input: LiveTaskCardInput { LiveTaskCardRules.input(for: current, messageId: messageId) }
    private var state: LiveTaskCardState { LiveTaskCardState(input) }
    private var phase: LiveTaskCardPhase {
        LiveTaskCardRules.phase(state, effectiveStatus: LiveTaskCardRules.effectiveStatus(input))
    }
    private var track: PlanningTrack { PlanProgress.currentPhase(loader.planningEntries) }
    private var planSteps: [String] { loader.plan?.steps ?? [] }
    private var activityLines: [String] { Array(task.steps.suffix(4)) }
    private var hasActivity: Bool { phase == .running && !activityLines.isEmpty }
    private var reasonOverflows: Bool { LiveTaskCardRules.reasonOverflows(state.reason) }

    private var expandable: Bool {
        state.isExpandable(
            activityHasContent: hasActivity,
            planStepCount: planSteps.count,
            reasonOverflows: reasonOverflows,
            expanded: expanded,
            liveActivityAvailable: true
        ) || (phase == .running && task.activeExecutionIdForControls != nil)
    }

    private var pollKey: String {
        "\(state.taskId ?? "")|\(phase)|\(visible)|\(scenePhase == .active)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                header
                metaRow
            }
            if let line = LiveTaskCardRules.line(
                phase: phase, state: state, planningEntries: loader.planningEntries,
                stepRows: loader.stepRows, latestActivity: task.steps.last
            ) {
                lineView(line)
            }
            if phase == .planning { planningTrack }
            if phase == .running, let layout = LiveTaskStepBar.layout(loader.stepRows) {
                stepBar(layout)
            }
            usageLine
            if state.outputsInBody { deliverables(limit: expanded ? nil : LiveTaskCardState.outputPreviewLimit) }
            if expanded { details.transition(.opacity) }
        }
        .padding(.horizontal, 12)
        .padding(.top, 10)
        .padding(.bottom, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        )
        .overlay(alignment: .bottom) { toastView }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("live-task-card-\(state.id)")
        .onAppear { visible = true; syncLoader() }
        .onDisappear { visible = false; syncLoader() }
        .onChange(of: pollKey) { _, _ in syncLoader() }
        .onChange(of: phase) { old, new in
            // A plan arriving while the reader looks at the chat gets a light tap.
            if new == .planReady, old != .planReady, visible, scenePhase == .active {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
            }
        }
        .confirmationDialog("Cancel this task?", isPresented: $confirmCancel, titleVisibility: .visible) {
            Button("Cancel task", role: .destructive) { Task { await cancel() } }
            Button("Keep running", role: .cancel) {}
        } message: {
            Text("The run stops and the task is marked cancelled.")
        }
        .sheet(item: $openedArtifact) { ArtifactViewer(artifact: $0) }
        .sheet(item: $sharePayload) { ShareSheet(items: $0.items) }
    }

    // MARK: Header

    /// The title row. Every task action sits in one cluster beside the title
    /// (`LiveTaskCardRules.actions`); when the cluster and a readable title do
    /// not fit on one row, the whole cluster moves to one row directly under
    /// the title, wrapping onto a second row only when it must.
    private var header: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: 6) {
                statusDot
                titleText.frame(minWidth: Self.titleMinWidth, idealWidth: Self.titleMinWidth, maxWidth: .infinity, alignment: .leading)
                // Chips never compress: if they don't fit whole, the cluster moves down.
                HStack(spacing: 6) { actionButtons }.fixedSize()
                chevron
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .center, spacing: 6) {
                    statusDot
                    titleText.frame(maxWidth: .infinity, alignment: .leading)
                    chevron
                }
                FlowRow(spacing: 6) { actionButtons }
                    .padding(.leading, 18)
            }
        }
    }

    /// The plan preview's step-number column.
    private static let stepNumberWidth: CGFloat = 18

    /// The chevron's visible box; its tap target reaches 44 pt by an outset
    /// content shape, so the header row is only as tall as a chip.
    private static let headerControlSize: CGFloat = 24

    /// The narrowest the title may get before the actions leave its row.
    private static let titleMinWidth: CGFloat = 110

    private var actions: [LiveTaskCardAction] {
        LiveTaskCardRules.actions(
            phase: phase,
            hasTaskId: state.taskId != nil,
            startable: state.startable,
            canCancel: state.canCancel,
            showsPlan: state.showsPlanPreview(planStepCount: planSteps.count)
        )
    }

    private var statusDot: some View {
        StatusDot(tone: LiveTaskCardRules.dotTone(state.status), size: 9)
            .frame(width: 12)
    }

    /// A planned task (`TaskPlanMarker`): the message or stored status, or
    /// the plan the card loaded.
    private var isPlanned: Bool {
        TaskPlanMarker.hasPlan(messageStatus: task.status, storeStatus: loader.storeStatus, plan: loader.plan)
    }

    /// The short id when the title is only `Task · <short id>`.
    private var idStandInShortId: String? {
        guard let short = TaskTitleRules.shortTaskId(state.taskId), state.title == "Task · \(short)" else { return nil }
        return short
    }

    private var titleText: some View {
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            if isPlanned { TaskPlanGlyph(size: 11) }
            if let taskId = state.taskId, let short = idStandInShortId {
                // An id standing in for a title (web `cn-live-task__id`): the
                // short form is for scanning; a tap copies the whole id.
                Text("Task ·")
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(theme.textColor)
                CopyOnTap(value: taskId, kind: .taskId, onCopied: { showToast($0, announce: false) }) { copied in
                    CopiedSwap(copied: copied, font: .themed(12, weight: .semibold)) {
                        Text(short).font(.themedMono(13)).foregroundColor(theme.textColor)
                    }
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .overlay(
                        RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                            .strokeBorder(theme.controlBorderColor, style: StrokeStyle(lineWidth: 1, dash: [3, 2])))
                    .contentShape(Rectangle().inset(by: -10))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Task id \(short)")
                .accessibilityIdentifier("live-task-id")
            } else {
                Text(state.title)
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(theme.textColor)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { if expandable { toggleExpanded() } }
    }

    @ViewBuilder
    private var chevron: some View {
        if expandable {
            Button(action: toggleExpanded) {
                Image(systemName: "chevron.down")
                    .font(.system(size: 12, weight: .bold))
                    .foregroundColor(theme.secondaryTextColor)
                    .rotationEffect(.degrees(expanded ? 180 : 0))
                    .frame(width: Self.headerControlSize, height: Self.headerControlSize)
                    // A 44 pt target without making the header row 44 pt tall.
                    .contentShape(Rectangle().inset(by: -10))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(expanded ? "Collapse task" : "Expand task")
        }
    }

    @ViewBuilder
    private var actionButtons: some View {
        ForEach(actions, id: \.self) { action in
            actionButton(action)
        }
    }

    @ViewBuilder
    private func actionButton(_ action: LiveTaskCardAction) -> some View {
        let taskId = state.taskId ?? ""
        switch action {
        case .inspect:
            Button(action: onInspect) { Text("Inspect") }
                .buttonStyle(HeaderChipStyle())
                .accessibilityLabel("Inspect task")
                .accessibilityIdentifier("live-task-inspect")
        case .run:
            Button { Task { await run(taskId) } } label: {
                if busy == .run { Text("Starting…") } else { Label("Run", systemImage: "play.fill") }
            }
            .buttonStyle(HeaderChipStyle(prominent: true))
            .disabled(busy != nil)
            .accessibilityLabel("Run task")
            .accessibilityIdentifier("live-task-run")
        case .approveAndRun:
            Button { Task { await approveAndRun(taskId) } } label: {
                if busy == .approveAndRun { Text("Starting…") } else { Text("Approve & Run") }
            }
            .buttonStyle(HeaderChipStyle(prominent: true))
            .disabled(busy != nil)
            .accessibilityIdentifier("live-task-approve-run")
        case .review:
            Button { onReviewPlan(taskId) } label: { Text("Review") }
                .buttonStyle(HeaderChipStyle())
                .accessibilityLabel("Review plan")
                .accessibilityIdentifier("live-task-review")
        case .planGraph:
            Button { onReviewPlan(taskId) } label: { Text("Plan graph") }
                .buttonStyle(HeaderChipStyle())
                .accessibilityIdentifier("live-task-plan-graph")
        case .cancel:
            Button { confirmCancel = true } label: {
                if busy == .cancel { ProgressView().controlSize(.mini) } else { Text("Cancel") }
            }
            .buttonStyle(HeaderChipStyle())
            .disabled(busy != nil)
            .accessibilityLabel("Cancel task")
            .accessibilityIdentifier("live-task-cancel")
        }
    }

    private var metaRow: some View {
        HStack(spacing: 6) {
            if let tag = state.tagText {
                Text(tag)
                    .font(.themed(11, weight: .semibold))
                    .foregroundColor(theme.secondaryAccentColor)
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).fill(theme.secondaryAccentColor.opacity(0.12)))
            }
            Text(state.headline)
                .font(.themed(12, weight: .medium))
                .foregroundColor(theme.secondaryTextColor)
            if state.showsTimer {
                if state.ticks {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        elapsedText(state.elapsedText(now: context.date))
                    }
                } else {
                    elapsedText(state.elapsedText(now: Date()))
                }
            }
            if state.needsYou {
                Text("Needs you")
                    .font(.themed(11, weight: .semibold))
                    .foregroundColor(theme.warningColor)
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, 18)
    }

    /// The task's model usage so far — `3 calls · 12.3k tok · $0.04`, planning
    /// and run together — spoken with its Planning / Run split. Absent until a
    /// call reported usage.
    @ViewBuilder
    private var usageLine: some View {
        if let taskId = state.taskId, let total = usageStore.total(for: taskId),
           let line = TaskUsageFormat.taskLine(total.usage) {
            Text(line)
                .font(.themedMono(11))
                .monospacedDigit()
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(1)
                .padding(.leading, 18)
                .accessibilityLabel(TaskUsageFormat.taskAccessibilityLabel(total))
                .accessibilityIdentifier("live-task-usage")
        }
    }

    @ViewBuilder
    private func elapsedText(_ text: String?) -> some View {
        if let text {
            Text("· \(text)")
                .font(.themedMono(12))
                .monospacedDigit()
                .foregroundColor(theme.secondaryTextColor)
                .accessibilityLabel("Elapsed \(text)")
        }
    }

    private func lineView(_ line: String) -> some View {
        let emphasised = LiveTaskCardRules.lineIsEmphasised(phase: phase, track: track)
        return Text(Self.inlineMarkdown(line))
            .font(.themed(13, weight: emphasised ? .bold : .regular))
            .foregroundColor(phase == .finished ? theme.textColor : theme.secondaryTextColor)
            .lineLimit(expanded ? nil : (phase == .finished ? 3 : 1))
            .truncationMode(.tail)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.leading, 18)
            .accessibilityIdentifier("live-task-line")
    }

    // MARK: Planning track

    private var planningTrack: some View {
        let segments = LiveTaskCardRules.planningSegments(track)
        return VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                ForEach(Array(PlanningPhase.allCases.enumerated()), id: \.offset) { index, _ in
                    PlanningSegment(
                        state: segments[index],
                        failed: track.failed && segments[index] == .current,
                        theme: theme
                    )
                }
            }
            HStack(spacing: 4) {
                ForEach(Array(PlanningPhase.allCases.enumerated()), id: \.offset) { index, phase in
                    Text(phase.title)
                        .font(.themed(10, weight: segments[index] == .current ? .semibold : .regular))
                        .foregroundColor(segments[index] == .current ? theme.textColor : theme.secondaryTextColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .lineLimit(1)
                }
            }
        }
        .padding(.leading, 18)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Planning: \(track.phase?.title ?? PlanningPhase.understand.title)\(track.retrying ? ", retrying" : "")")
    }

    // MARK: Step bar

    @ViewBuilder
    private func stepBar(_ layout: LiveTaskStepBar.Layout) -> some View {
        Group {
            switch layout {
            case .segments(let marks):
                HStack(spacing: 3) {
                    ForEach(Array(marks.enumerated()), id: \.offset) { _, mark in
                        Capsule().fill(color(for: mark)).frame(height: 5)
                    }
                }
            case .continuous(let fraction):
                GeometryReader { proxy in
                    ZStack(alignment: .leading) {
                        Capsule().fill(theme.controlBorderColor)
                        Capsule().fill(theme.statusRunningColor)
                            .frame(width: max(5, proxy.size.width * fraction))
                    }
                }
                .frame(height: 5)
            }
        }
        .padding(.leading, 18)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(PlanSteps.summary(loader.stepRows))
    }

    private func color(for mark: PlanStepMark) -> Color {
        switch mark {
        case .done: return theme.successColor
        case .failed: return theme.dangerColor
        // A running task's colour is the theme's status-running (web
        // `--status-running`), never the accent, which stays for controls.
        case .inProgress: return theme.statusRunningColor
        case .skipped: return theme.secondaryTextColor.opacity(0.45)
        case .pending, .notReached: return theme.controlBorderColor
        }
    }

    // MARK: Details (expanded)

    @ViewBuilder
    private var details: some View {
        VStack(alignment: .leading, spacing: 10) {
            if state.showsPlanPreview(planStepCount: planSteps.count) {
                VStack(alignment: .leading, spacing: 6) {
                    // Plan graph sits in the title row's action cluster.
                    Text("Plan · \(planSteps.count) step\(planSteps.count == 1 ? "" : "s")")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(theme.textColor)
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(planSteps.enumerated()), id: \.offset) { index, step in
                            if index > 0 { StepHairline(leadingInset: Self.stepNumberWidth + 6) }
                            HStack(alignment: .firstTextBaseline, spacing: 6) {
                                Text("\(index + 1).")
                                    .font(.themedMono(Lively.stepMetaSize))
                                    .foregroundColor(theme.secondaryTextColor)
                                    .frame(width: Self.stepNumberWidth, alignment: .trailing)
                                Text(step)
                                    .font(.themed(Lively.stepTitleSize))
                                    .foregroundColor(theme.textColor)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .padding(.vertical, Lively.stepRowPadding)
                        }
                    }
                }
            }
            if hasActivity {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(activityLines.enumerated()), id: \.offset) { index, line in
                        if index > 0 { StepHairline() }
                        Text(line)
                            .font(.themed(Lively.stepTitleSize))
                            .foregroundColor(theme.secondaryTextColor)
                            .lineLimit(2)
                            .padding(.vertical, Lively.stepRowPadding)
                    }
                }
            }
            if !state.outputsInBody, !task.outputFiles.isEmpty {
                deliverables(limit: nil)
            }
            if phase == .running, let executionId = task.activeExecutionIdForControls {
                ExecutionControlsView(executionId: executionId, refreshToken: task.executionControlRefreshToken)
                    .id(executionId)
            }
        }
        .padding(.leading, 18)
    }

    // MARK: Deliverables

    @ViewBuilder
    private func deliverables(limit: Int?) -> some View {
        let files = limit.map { Array(task.outputFiles.prefix($0)) } ?? task.outputFiles
        let hidden = state.hiddenOutputCount(expanded: expanded)
        VStack(alignment: .leading, spacing: 8) {
            if let first = files.first, let ref = artifactReference(first) {
                LiveTaskDeliverablePreview(artifact: ref, theme: theme) { openedArtifact = ref }
            }
            ForEach(Array(files.enumerated()), id: \.offset) { _, block in
                deliverableRow(block)
            }
            if hidden > 0, !expanded {
                Button { toggleExpanded() } label: { Text("+\(hidden) more") }
                    .buttonStyle(ChipStyle())
            }
        }
    }

    private func deliverableRow(_ block: ContentBlock) -> some View {
        let ref = artifactReference(block)
        let name = ref?.filename ?? block.displayName ?? block.label ?? block.filename ?? "File"
        let meta = [ref?.kind.label, block.size.map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file) }]
            .compactMap { $0 }.joined(separator: " · ")
        let copyPath = LiveTaskCardRules.copyPath(absolutePath: block.absolutePath, relativePath: block.relativePath)
        return HStack(spacing: 6) {
            Image(systemName: ref?.kind.systemIcon ?? "doc")
                .font(.system(size: 13))
                .foregroundColor(theme.accentColor)
                .frame(width: 18)
            CopyOnTap(value: copyPath, kind: .path, onCopied: { showToast($0, announce: false) }) { copied in
                VStack(alignment: .leading, spacing: 1) {
                    CopiedSwap(copied: copied, font: .themed(12, weight: .semibold)) {
                        Text(name)
                            .font(.themed(12, weight: .medium))
                            .foregroundColor(theme.textColor)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    if !meta.isEmpty {
                        Text(meta).font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                    }
                }
                .frame(maxWidth: .infinity, minHeight: Lively.tapMin, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(name)
            if let ref {
                Button { openedArtifact = ref } label: { Text("Open") }
                    .buttonStyle(ChipStyle())
                Button { share(ref) } label: { Image(systemName: "square.and.arrow.up") }
                    .buttonStyle(ChipStyle())
                    .accessibilityLabel("Share \(name)")
            }
        }
    }

    private func artifactReference(_ block: ContentBlock) -> ArtifactRef? {
        let name = block.displayName ?? block.label ?? block.filename
        if let url = block.url, !url.isEmpty {
            return .direct(url: url, mime: block.mimeType, name: name)
        }
        guard let relativePath = block.relativePath, !relativePath.isEmpty else { return nil }
        switch LiveTaskCardRules.outputHome(
            sourceType: block.source?.type, sourceTaskId: block.source?.taskId,
            cardTaskId: state.taskId, sessionId: sessionId
        ) {
        case .task(let taskId):
            return .taskOutput(taskId: taskId, relativePath: relativePath, mime: block.mimeType)
        case .session(let sessionId):
            return .sessionOutput(sessionId: sessionId, relativePath: relativePath, mime: block.mimeType)
        case nil:
            return nil
        }
    }

    /// Download the authenticated bytes to a temp file, then share the file.
    private func share(_ ref: ArtifactRef) {
        guard let url = ref.url else { return }
        if !ref.isMagicianOwned {
            sharePayload = SharePayload(items: [url])
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        showToast("Preparing…")
        URLSession.shared.downloadTask(with: request) { tempURL, _, _ in
            guard let tempURL else {
                DispatchQueue.main.async { showToast("Could not download the file") }
                return
            }
            let dest = FileManager.default.temporaryDirectory.appendingPathComponent(ref.filename)
            try? FileManager.default.removeItem(at: dest)
            try? FileManager.default.moveItem(at: tempURL, to: dest)
            DispatchQueue.main.async { sharePayload = SharePayload(items: [dest]) }
        }.resume()
    }

    // MARK: Toast

    @ViewBuilder
    private var toastView: some View {
        if let toast {
            Text(toast)
                .font(.themed(12, weight: .semibold))
                .foregroundColor(theme.onAccentColor)
                .padding(.horizontal, 12).padding(.vertical, 6)
                .background(Capsule().fill(theme.accentFillColor))
                .padding(.bottom, 8)
                .transition(.opacity)
                .accessibilityAddTraits(.updatesFrequently)
        }
    }

    /// `announce: false` when the caller already announced (a copy says
    /// "Copied" itself).
    private func showToast(_ message: String, seconds: Double = 1.4, announce: Bool = true) {
        toastToken &+= 1
        let token = toastToken
        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.15)) { toast = message }
        if announce { UIAccessibility.post(notification: .announcement, argument: message) }
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
            guard token == toastToken else { return }
            withAnimation(reduceMotion ? nil : .easeIn(duration: 0.2)) { toast = nil }
        }
    }

    // MARK: Behaviour

    private func toggleExpanded() {
        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { expanded.toggle() }
    }

    private func syncLoader() {
        guard let taskId = state.taskId else { loader.stop(); return }
        let poll = LiveTaskCardRules.shouldPoll(
            phase: phase, hasTaskId: true, visible: visible, appActive: scenePhase == .active)
        if poll { loader.startPolling(taskId: taskId) } else { loader.stop() }
        if visible, state.wantsPlan { loader.loadPlanOnce(taskId: taskId) }
        // While the card reads non-terminal, check the task's stored status on
        // appear (the poll re-checks it each tick).
        if visible, !state.terminal { loader.refreshStoreStatus(taskId: taskId) }
        // The poll keeps usage current while planning or running; otherwise the
        // stored figure is read once per phase — and once more on settle, for
        // the final snapshot.
        if visible, !poll {
            usageStore.loadSnapshot(taskId: taskId, phase: LiveTaskCardRules.usagePhase(state))
        }
    }

    private func cancel() async {
        guard let taskId = state.taskId else { return }
        busy = .cancel
        let error = await LiveTaskCardClient.cancel(
            taskId: taskId, activeExecutionId: task.activeExecutionIdForControls)
        busy = nil
        showToast(error ?? "Cancelling…", seconds: error == nil ? 1.4 : 3)
    }

    private func run(_ taskId: String) async {
        busy = .run
        let error = await LiveTaskCardClient.execute(taskId: taskId)
        busy = nil
        showToast(error ?? "Run started", seconds: error == nil ? 1.4 : 3)
    }

    private func approveAndRun(_ taskId: String) async {
        busy = .approveAndRun
        defer { busy = nil }
        var planId = loader.plan?.planId
        if planId == nil { planId = await loader.fetchPlan(taskId: taskId)?.planId }
        guard let planId else {
            showToast("The plan could not be loaded", seconds: 3)
            return
        }
        let outcome = await PlanApprovalActions.approveAndRun(
            PlanApprovalRequest(correlationId: planId, taskId: taskId))
        showToast(outcome.message, seconds: outcome == .approvedAndStarted ? 1.8 : 3.5)
    }

    static func inlineMarkdown(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        )) ?? AttributedString(text)
    }
}

// MARK: - Planning segment

private struct PlanningSegment: View {
    let state: LiveTaskCardRules.SegmentState
    let failed: Bool
    @ObservedObject var theme: ThemeManager
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pulse = false

    var body: some View {
        let fill: Color = {
            switch state {
            // Planning is the task running: status-running, not the accent.
            case .finished: return theme.statusRunningColor
            case .current: return failed ? theme.warningColor : theme.statusRunningColor
            case .ahead: return theme.controlBorderColor
            }
        }()
        let moving = state == .current && !failed && !reduceMotion
        Capsule()
            .fill(fill)
            .frame(height: 5)
            .opacity(moving && pulse ? 0.4 : 1)
            .animation(moving ? .easeInOut(duration: 1.1).repeatForever(autoreverses: true) : .default, value: pulse)
            .onAppear { pulse = moving }
            .onChange(of: moving) { _, now in pulse = now }
    }
}

// MARK: - Deliverable preview

/// The first deliverable, previewed in the card by default: an image inline,
/// the start of a text/markdown/JSON file; other kinds show their row only.
private struct LiveTaskDeliverablePreview: View {
    let artifact: ArtifactRef
    @ObservedObject var theme: ThemeManager
    let onOpen: () -> Void
    @State private var snippet: String?

    var body: some View {
        switch artifact.kind {
        case .image:
            if let url = artifact.url {
                Button(action: onOpen) {
                    AuthAsyncImage(urlString: url.absoluteString, maxHeight: 180)
                        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Open \(artifact.filename)")
            }
        case .text(let kind):
            Button(action: onOpen) {
                Text(snippet ?? " ")
                    .font(kind == .markdown ? .themed(12) : .themedMono(11))
                    .foregroundColor(theme.textColor)
                    .lineLimit(6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                    .background(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(theme.surfaceColor)
                    )
                    .redacted(reason: snippet == nil ? .placeholder : [])
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Preview of \(artifact.filename). Open")
            .task(id: artifact.resolvedURL) { snippet = await LiveTaskCardClient.textSnippet(artifact.url) }
        default:
            EmptyView()
        }
    }
}

// MARK: - Loader (execution panel poll + plan)

/// Feeds the card's planning track and step bar from `/execution-panel`
/// (polled every 4 s only while the host says so) and loads `/plan` once.
@MainActor
final class LiveTaskCardLoader: ObservableObject {
    @Published private(set) var planningEntries: [PlanningEntry] = []
    @Published private(set) var stepRows: [PlanStepRow] = []
    @Published private(set) var plan: LiveTaskPlan?
    /// The task's stored status (`/v3/tasks/{id}`), when fetched.
    @Published private(set) var storeStatus: String?
    private var storeFetchedAt: Date?
    private var storeInFlight = false

    private var pollTask: Task<Void, Never>?
    private var pollingTaskId: String?
    private var planRequestedFor: String?

    func startPolling(taskId: String) {
        guard pollingTaskId != taskId || pollTask == nil else { return }
        stop()
        pollingTaskId = taskId
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                if let panel = await LiveTaskCardClient.json("/tasks/\(LiveTaskCardClient.segment(taskId))/execution-panel") {
                    guard !Task.isCancelled else { return }
                    self?.apply(panel: panel)
                }
                self?.refreshStoreStatus(taskId: taskId)
                try? await Task.sleep(nanoseconds: UInt64(LiveTaskCardRules.pollInterval * 1_000_000_000))
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
        pollingTaskId = nil
    }

    /// Fetch the task's stored status, at most once per poll interval.
    func refreshStoreStatus(taskId: String) {
        if storeInFlight { return }
        if let last = storeFetchedAt, Date().timeIntervalSince(last) < LiveTaskCardRules.pollInterval - 0.5 { return }
        storeInFlight = true
        storeFetchedAt = Date()
        Task { [weak self] in
            let payload = await LiveTaskCardClient.json("/tasks/\(LiveTaskCardClient.segment(taskId))")
            guard let self else { return }
            self.storeInFlight = false
            if let record = payload.flatMap(TaskStoreRecord.parse), record.status != self.storeStatus {
                self.storeStatus = record.status
            }
        }
    }

    func loadPlanOnce(taskId: String) {
        guard planRequestedFor != taskId else { return }
        planRequestedFor = taskId
        Task { _ = await fetchPlan(taskId: taskId) }
    }

    @discardableResult
    func fetchPlan(taskId: String) async -> LiveTaskPlan? {
        guard let payload = await LiveTaskCardClient.json("/tasks/\(LiveTaskCardClient.segment(taskId))/plan"),
              let parsed = LiveTaskPlan.parse(payload) else { return nil }
        plan = parsed
        return parsed
    }

    private func apply(panel: [String: Any]) {
        let entries = PlanProgress.entries(fromPanel: panel)
        if entries != planningEntries { planningEntries = entries }
        let rows = PlanSteps.rows(fromPanel: panel, runEnded: false)
        if rows != stepRows { stepRows = rows }
        if let pollingTaskId { TaskUsageStore.shared.apply(panel: panel, taskId: pollingTaskId) }
    }

    deinit { pollTask?.cancel() }
}

// MARK: - Client

/// The card's few authenticated calls, on the same base URL and bearer as the
/// rest of the app (`MagicianAccess`).
enum LiveTaskCardClient {
    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? id
    }

    static func json(_ v3Path: String) async -> [String: Any]? {
        let expected = TaskUsageStore.connectionKey()
        let origin = MagicianAccess.baseURL, headers = MagicianAccess.headers()
        guard let url = URL(string: "\(origin.absoluteString)/api/magician/v3" + v3Path) else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              expected == TaskUsageStore.connectionKey(), origin == MagicianAccess.baseURL, headers == MagicianAccess.headers(),
              let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    /// `POST /v3/tasks/{id}/execute`. Nil on success, else what went wrong.
    static func execute(taskId: String) async -> String? {
        await send("POST", "/api/magician/v3/tasks/\(segment(taskId))/execute", body: [:], fallback: "The run did not start")
    }

    /// Stop the active run (`POST /v2/executions/{id}/cancel`, as the run
    /// controls do); with no run yet (planning), mark the task cancelled.
    @MainActor
    static func cancel(taskId: String, activeExecutionId: String?) async -> String? {
        if let executionId = activeExecutionId {
            let error = await send("POST", "/api/magician/v2/executions/\(segment(executionId))/cancel",
                                   body: [:], fallback: "The task could not be cancelled")
            ExecutionControlCoordinator.shared.invalidate(executionId)
            return error
        }
        return await send("PUT", "/api/magician/v3/tasks/\(segment(taskId))/status",
                          body: ["status": "cancelled"], fallback: "The task could not be cancelled")
    }

    /// The first few KB of a text file, for the card's preview.
    static func textSnippet(_ url: URL?) async -> String? {
        guard let url else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        request.setValue("bytes=0-8191", forHTTPHeaderField: "Range")
        MagicianAccess.authorize(&request)
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return nil }
        let text = String(decoding: data.prefix(8192), as: UTF8.self)
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func send(_ method: String, _ path: String, body: [String: Any], fallback: String) async -> String? {
        guard let url = URL(string: MagicianAccess.baseURL.absoluteString + path) else { return fallback }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 20
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        MagicianAccess.authorize(&request)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if (200..<300).contains(status) { return nil }
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            let message = (object?["error"] ?? object?["message"] ?? object?["detail"]) as? String
            return message ?? "\(fallback) (HTTP \(status))"
        } catch {
            return error.localizedDescription
        }
    }
}

/// The header's action chips: `ChipStyle`'s look, but the 44 pt target is an
/// outset content shape rather than a 44 pt frame, so the title row stays as
/// tall as a chip.
private struct HeaderChipStyle: ButtonStyle {
    @ObservedObject private var theme = ThemeManager.shared
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.themed(ChipStyle.fontSize, weight: .semibold))
            .foregroundStyle(prominent ? theme.onAccentColor : theme.textColor)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: ChipStyle.cornerRadius, style: .continuous)
                    .fill(prominent
                          ? (configuration.isPressed ? theme.accentFillPressedColor : theme.accentFillColor)
                          : theme.controlColor)
            )
            .overlay(
                RoundedRectangle(cornerRadius: ChipStyle.cornerRadius, style: .continuous)
                    .strokeBorder(prominent ? Color.clear : theme.controlBorderColor, lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.7 : 1)
            .contentShape(Rectangle().inset(by: -10))
    }
}
