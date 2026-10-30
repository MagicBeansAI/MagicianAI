import SwiftUI

/// One run: header (state, project, engine, cost, branch), Turns / Changes,
/// and the actions its state allows in the bottom bar.
struct VibeDevRunView: View {
    @StateObject private var model: VibeDevRunModel
    @EnvironmentObject private var navigator: VibeDevNavigator
    @ObservedObject private var theme = ThemeManager.shared
    @State private var confirm: Confirm?

    enum Confirm: Identifiable {
        case merge, pullRequest, discard
        var id: Self { self }
    }

    init(runID: String, tab: VibeDevRunTab, context: VibeDevContext) {
        _model = StateObject(wrappedValue: VibeDevRunModel(runID: runID, tab: tab, context: context))
    }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                header
                VibeDevStaleNote(lastUpdated: model.lastUpdated, failed: model.refreshFailed)
                if let error = model.errorMessage, model.turns == nil {
                    VibeDevEmpty(text: error)
                }
                if model.turns != nil {
                    VibeDevSegmented(options: [
                        (VibeDevRunTab.turns, "Turns · \(model.turns?.turns.count ?? 0)"),
                        (VibeDevRunTab.changes, model.changes.map { "Changes · \($0.files.count)" } ?? "Changes"),
                    ], selection: Binding(get: { model.tab }, set: { model.selectTab($0) }))
                    switch model.tab {
                    case .turns: turns
                    case .changes: changes
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 4)
            .padding(.bottom, 24)
        }
        .background(theme.backgroundColor.ignoresSafeArea())
        .refreshable { await model.refresh() }
        .overlay { if !model.loaded { ProgressView().tint(theme.accentColor) } }
        .navigationTitle(VibeDevFormat.short(model.title, 30))
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) { VibeDevBackButton() }
                .roundedSquareToolbarBackground()
            ToolbarItem(placement: .topBarTrailing) {
                // Follow: the run as its own Live Activity until it ends. A
                // crew lane is followed through its crew.
                if let turns = model.turns, turns.crewId == nil, !model.state.isEnded {
                    WorkFollowButton(
                        seed: FollowedItemActivity.runSeed(
                            runID: turns.runId, projectID: turns.projectId, title: model.title,
                            projectName: turns.projectName, state: model.state.rawValue),
                        iconOnly: true)
                        .vibeDevControlOutline()
                }
            }
            .roundedSquareToolbarBackground()
            ToolbarItem(placement: .topBarTrailing) {
                if model.bar == .ended(trusted: true) {
                    Menu {
                        Button(role: .destructive) { confirm = .discard } label: {
                            Label("Discard branch", systemImage: "trash")
                        }
                    } label: {
                        Image(systemName: "ellipsis")
                            .frame(width: 28, height: 34)
                    }
                    .vibeDevControlOutline()
                    .vibeDevHitTarget()
                    .accessibilityLabel("More")
                }
            }
            .roundedSquareToolbarBackground()
        }
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 0) {
                ScrollView {
                    TaskDetailAskSection(taskId: model.liveTaskID, panelRows: [],
                        vibeDevTaskIds: model.askTaskIDs, chatSessionId: model.project?.chatSessionId, onAnswered: {
                        Task { await model.refresh() }
                    })
                }.fixedSize(horizontal: false, vertical: true).frame(maxHeight: 260)
                bottomBar
                if let turns = model.turns, model.state.isEnded {
                    VibeDevNewRunSheet(context: model.context,
                        seed: VibeDevNewRunSeed(projectID: turns.projectId,
                            followUp: (runID: turns.runId, parentTaskID: turns.latestTurnId),
                            codingRouteID: turns.turns.last(where: { $0.codingRouteId != nil })?.codingRouteId,
                            engine: turns.turns.last(where: { $0.engine != nil })?.engine,
                            mode: turns.turns.last?.mode ?? .build),
                        embedded: true, showPendingAsks: false, workLocation: turns.workLocation ?? (turns.trusted ? "trusted" : "sandboxed")) { _ in
                            navigator.changed()
                            Task { await model.refresh() }
                        }
                }
            }
        }
        .confirmationDialog(confirmTitle, isPresented: Binding(get: { confirm != nil }, set: { if !$0 { confirm = nil } }),
                            titleVisibility: .visible, presenting: confirm) { choice in
            switch choice {
            case .merge: Button("Merge") { model.merge() }
            case .pullRequest: Button("Open pull request") { model.openPullRequest() }
            case .discard: Button("Discard", role: .destructive) { model.discard() }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task { await model.refresh() }
        .onAppear { model.startLive() }
        .onDisappear { model.stopLive() }
        .onChange(of: navigator.revision) { _, _ in Task { await model.refresh() } }
    }

    private var confirmTitle: String {
        switch confirm {
        case .merge: return "Merge this branch into \(model.project?.baseBranch ?? "its base branch")?"
        case .pullRequest: return "Push the branch and open a pull request?"
        case .discard: return "Delete this run's branch and clone?"
        case nil: return ""
        }
    }

    // MARK: Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                VibeDevStateDot(run: model.stopping ? .stopped : model.state,
                                partlyDone: !model.stopping && model.isPartlyDone)
                Text(model.stopping ? "Stopping…"
                     : model.isPartlyDone ? TaskCompletion.partlyDoneLabel : model.state.label)
                    .font(.themed(11, weight: .bold))
                    .foregroundStyle(!model.stopping && model.isPartlyDone
                                     ? theme.warningColor : theme.secondaryTextColor)
                if let lane = model.turns?.lane { VibeDevTag(text: "\(lane) lane") }
                if let step = model.live?.step, model.state.isLive {
                    Text("· \(step)")
                        .font(.themed(11))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(1)
                }
            }
            Text(model.title)
                .font(.themedDisplay(17, weight: .semibold))
                .foregroundStyle(theme.textColor)
                .vibeDevLineLimit(3)
            if !model.stopping, let open = model.partlyDoneOpenItems.flatMap(TaskCompletion.openLine) {
                Text(open)
                    .font(.themed(12))
                    .foregroundStyle(theme.warningColor)
                    .vibeDevLineLimit(2)
            }
            if let turns = model.turns {
                VibeDevKeyValues(rows: headerRows(turns))
            }
        }
        .vibeDevCard()
    }

    private func headerRows(_ turns: VibeDevRunTurns) -> [(String, String)] {
        var rows: [(String, String)] = [("Project", turns.projectName)]
        if !turns.engine.isEmpty { rows.append(("Engine", turns.engine)) }
        if let cost = turns.totalCost { rows.append(("Cost", VibeDevFormat.usd(cost))) }
        if let location = turns.workLocation { rows.append(("Work location", location == "original" ? "Original folder" : location == "trusted" ? "Isolated branch" : "Isolated workspace")) }
        if turns.trusted { rows.append(("Branch", turns.branch ?? "—")) }
        return rows
    }

    // MARK: Turns

    @ViewBuilder
    private var turns: some View {
        if let detail = model.turns {
            ForEach(Array(detail.turns.enumerated()), id: \.element.taskId) { index, turn in
                turnCard(turn, index: index)
            }
        }
    }

    private func turnCard(_ turn: VibeDevRunTurn, index: Int) -> some View {
        let open = model.expanded.contains(turn.taskId)
        return VStack(alignment: .leading, spacing: 0) {
            Button { withAnimation(.easeInOut(duration: 0.18)) { model.toggle(turn) } } label: {
                HStack(alignment: .top, spacing: 10) {
                    VibeDevStateDot(run: turn.state, partlyDone: turn.isPartlyDone)
                        .padding(.top, 4)
                    VStack(alignment: .leading, spacing: 3) {
                        (Text(index == 0 ? "" : "Follow-up · ").bold() + Text(turn.prompt))
                            .font(.themed(13))
                            .foregroundStyle(theme.textColor)
                            .vibeDevLineLimit(2)
                            .multilineTextAlignment(.leading)
                        Text(turnMeta(turn))
                            .font(.themed(11))
                            .foregroundStyle(theme.secondaryTextColor)
                            .vibeDevLineLimit(1)
                        if turn.isPartlyDone, let open = TaskCompletion.openLine(turn.openItems) {
                            Text(open)
                                .font(.themed(11))
                                .foregroundStyle(theme.warningColor)
                                .vibeDevLineLimit(1)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(theme.secondaryTextColor)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 11)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(index == 0 ? "" : "Follow-up, ")\(turn.prompt)")
            .accessibilityValue(turnMeta(turn))
            .accessibilityHint(open ? "Hides the timeline" : "Shows the timeline")
            if open {
                Rectangle().fill(theme.cardBorderColor).frame(height: 1)
                Group {
                    if let timeline = model.timelines[turn.taskId] {
                        VibeDevTurnTimelineView(timeline: timeline)
                        if let summary = turn.summary, turn.state.isEnded {
                            Text(summary)
                                .font(.themed(12.5))
                                .foregroundStyle(theme.textColor)
                                .padding(.top, 4)
                        }
                    } else if let error = model.timelineErrors[turn.taskId] {
                        Text(error).font(.themed(12)).foregroundStyle(theme.dangerColor)
                    } else {
                        ProgressView().tint(theme.accentColor).frame(maxWidth: .infinity)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 8)
                .padding(.bottom, 12)
            }
        }
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
            .strokeBorder(theme.cardBorderColor, lineWidth: 1))
    }

    private func turnMeta(_ turn: VibeDevRunTurn) -> String {
        var parts: [String] = []
        if turn.mode == .discuss { parts.append("Discuss") }
        let engine = VibeDevFormat.engine(turn.engine)
        if !engine.isEmpty { parts.append(engine) }
        parts.append(turn.stateLabel)
        let duration = turn.duration
        if !duration.isEmpty { parts.append(duration) }
        if let cost = turn.costUsd { parts.append(VibeDevFormat.usd(cost)) }
        if let files = turn.filesChanged { parts.append(files == 1 ? "1 file" : "\(files) files") }
        return parts.joined(separator: " · ")
    }

    // MARK: Changes

    @ViewBuilder
    private var changes: some View {
        if let detail = model.turns, detail.turns.count > 1 {
            VibeDevPills(options: [VibeDevPillOption(value: "", label: "Whole run")] +
                         detail.turns.enumerated().map { index, turn in
                             VibeDevPillOption(value: turn.taskId, label: index == 0 ? "First turn" : "Follow-up \(index)")
                         },
                         selection: Binding(get: { model.changesTurn ?? "" },
                                            set: { model.filterChanges(turn: $0.isEmpty ? nil : $0) }))
        }
        if let changes = model.changes {
            if changes.source == "proposals" && model.state == .needsYou {
                Text("Waiting for your approval before these are applied.")
                    .font(.themed(12))
                    .foregroundStyle(theme.warningColor)
            }
            if changes.files.isEmpty {
                VibeDevEmpty(text: "No changes yet.")
            }
            VStack(spacing: 0) {
                ForEach(Array(changes.files.enumerated()), id: \.element.path) { index, file in
                    Button {
                        navigator.open(.diff(VibeDevDiffTarget(runID: model.rootID, turn: model.changesTurn,
                                                               files: changes.files, index: index)))
                    } label: {
                        HStack(spacing: 8) {
                            Text(file.path)
                                .font(.themedMono(12.5, weight: .medium))
                                .foregroundStyle(theme.textColor)
                                .vibeDevLineLimit(1)
                                .truncationMode(.head)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text("+\(file.additions)").foregroundStyle(theme.successColor)
                            Text("−\(file.deletions)").foregroundStyle(theme.dangerColor)
                        }
                        .font(.themedMono(12, weight: .semibold))
                        .padding(.vertical, 11)
                        .padding(.horizontal, 2)
                        .frame(minHeight: Lively.tapMin)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(file.path), \(file.additions) added, \(file.deletions) removed")
                    Rectangle().fill(theme.cardBorderColor).frame(height: 1)
                }
            }
        } else if let error = model.changesError {
            VibeDevEmpty(text: error)
        } else if model.loadingChanges {
            ProgressView().tint(theme.accentColor).frame(maxWidth: .infinity).padding(.top, 20)
        }
    }

    // MARK: Bottom bar

    @ViewBuilder
    private var bottomBar: some View {
        switch model.bar {
        case .live:
            VibeDevBottomBar {
                Button("Steer") {
                    navigator.present(.steer(liveTaskID: model.liveTaskID, name: model.name))
                }
                .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                Button("Stop") { model.stop() }
                    .buttonStyle(VibeDevButtonStyle(style: .danger, expand: true, large: true))
            }
        case .needsYou:
            if model.live?.approvalId?.isEmpty == false {
                VibeDevBottomBar {
                    Button("Reject") { model.decide(apply: false) }
                        .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                    Button("Approve") { model.decide(apply: true) }
                        .buttonStyle(VibeDevButtonStyle(style: .primary, expand: true, large: true))
                }.disabled(model.busy)
            }
        case .ended(let trusted):
            if trusted {
                VibeDevBottomBar {
                    Button("Open PR") { confirm = .pullRequest }
                        .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                    Button("Merge") { confirm = .merge }
                        .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                }
                .disabled(model.busy)
            }
        case .none:
            EmptyView()
        }
    }
}
