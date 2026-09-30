//  MonitorDetailView.swift
//  Recurring Monitors (Phase 5, iOS) — monitor mode alongside the native task
//  detail (§9.3.2), mirroring the web `MonitorDetailPanel`: the §5.3 four-tab
//  layout (Latest / Updates / Runs / Settings) plus run-now / pause / resume /
//  edit / delete. Execution-level inspection deliberately stays on the SHARED
//  task detail (DeepWorkPanel) via "Open task view" — no second task-detail
//  implementation. A deep-linked update id lands on Updates with that record
//  highlighted and scrolled into view.

import SwiftUI

struct MonitorDetailView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var themeManager = ThemeManager.shared
    @StateObject private var vm: MonitorDetailViewModel

    /// Route into the SHARED task detail (the presenting view owns it).
    let onOpenTask: (String) -> Void
    /// Row-level refresh callbacks after mutations.
    let onChanged: () -> Void
    let onDeleted: () -> Void

    enum Section: String, CaseIterable, Identifiable {
        case latest, updates, runs, settings
        var id: String { rawValue }
        var title: String { rawValue.capitalized }
    }

    @State private var section: Section
    @State private var confirmingDelete = false
    @State private var showEditor = false

    init(taskID: String,
         highlightUpdateID: String? = nil,
         client: Monitors.APIClient? = nil,
         onOpenTask: @escaping (String) -> Void = { _ in },
         onChanged: @escaping () -> Void = {},
         onDeleted: @escaping () -> Void = {}) {
        _vm = StateObject(wrappedValue: MonitorDetailViewModel(
            taskID: taskID, highlightUpdateID: highlightUpdateID, client: client))
        self.onOpenTask = onOpenTask
        self.onChanged = onChanged
        self.onDeleted = onDeleted
        // A deep-linked update opens straight onto Updates.
        _section = State(initialValue: highlightUpdateID == nil ? .latest : .updates)
    }

    var body: some View {
        NavigationStack {
            content
                .background(themeManager.backgroundColor.ignoresSafeArea())
                .navigationTitle(vm.detail?.title ?? "Monitor")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Done") { dismiss() }
                    }
                }
        }
        .task { await vm.load() }
        .onChange(of: vm.wasDeleted) { _, deleted in
            if deleted {
                onDeleted()
                dismiss()
            }
        }
        .alert("Monitor action failed", isPresented: Binding(
            get: { vm.actionErrorMessage != nil },
            set: { if !$0 { vm.actionErrorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { vm.actionErrorMessage = nil }
        } message: {
            Text(vm.actionErrorMessage ?? "The monitor could not be updated.")
        }
        .confirmationDialog(
            "Delete this monitor?",
            isPresented: $confirmingDelete,
            titleVisibility: .visible
        ) {
            Button("Delete monitor", role: .destructive) {
                Task {
                    if await vm.delete() { onChanged() }
                }
            }
            Button("Keep monitor", role: .cancel) {}
        } message: {
            Text("The monitor stops running and disappears from the list. Its task record is archived, not destroyed.")
        }
        .sheet(isPresented: $showEditor) {
            if let detail = vm.detail {
                MonitorComposerView(
                    mode: .edit(taskID: detail.taskID),
                    initialForm: MonitorForm(detail: detail)
                ) {
                    Task {
                        await vm.load()
                        onChanged()
                    }
                }
            }
        }
    }

    @ViewBuilder private var content: some View {
        if vm.isLoading && vm.detail == nil {
            VStack { Spacer(); ProgressView("Loading monitor…"); Spacer() }
                .frame(maxWidth: .infinity)
        } else if let error = vm.loadErrorMessage, vm.detail == nil {
            VStack(spacing: 10) {
                Spacer()
                Image(systemName: "wifi.exclamationmark")
                    .font(.system(size: 30)).foregroundColor(themeManager.warningColor)
                Text(error)
                    .font(.themed(13)).foregroundColor(themeManager.secondaryTextColor)
                    .multilineTextAlignment(.center).padding(.horizontal, 24)
                Button("Retry") { Task { await vm.load() } }
                    .buttonStyle(.borderedProminent)
                Spacer()
            }
            .frame(maxWidth: .infinity)
        } else if let detail = vm.detail {
            loaded(detail)
        }
    }

    private func loaded(_ detail: Monitors.DetailV1) -> some View {
        VStack(spacing: 0) {
            header(detail)
            actionsRow
            if let notice = vm.actionNoticeMessage {
                Label(notice, systemImage: "checkmark.circle.fill")
                    .font(.themed(12, weight: .semibold))
                    .foregroundColor(themeManager.successColor)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 5)
                    .background(themeManager.successColor.opacity(0.12))
            }
            Picker("Section", selection: $section) {
                ForEach(Section.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal)
            .padding(.vertical, 8)
            Divider().overlay(themeManager.secondaryTextColor.opacity(0.15))
            sectionBody(detail)
        }
    }

    private func header(_ detail: Monitors.DetailV1) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                stateBadge(paused: vm.isPaused)
                pill(vm.cadenceSummary, tint: themeManager.infoColor)
                pill("rev \(detail.monitorRevision)", tint: themeManager.secondaryTextColor)
                Spacer(minLength: 0)
            }
            Text(detail.spec.objective)
                .font(.themed(13))
                .foregroundColor(themeManager.secondaryTextColor)
                .lineLimit(3)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal)
        .padding(.top, 10)
    }

    private var actionsRow: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                actionButton("Run now", systemImage: "play.fill", action: .run) {
                    Task {
                        if await vm.runNow() { onChanged() }
                    }
                }
                if vm.isPaused {
                    actionButton("Resume", systemImage: "playpause.fill", action: .resume) {
                        Task {
                            if await vm.resume() { onChanged() }
                        }
                    }
                } else {
                    actionButton("Pause", systemImage: "pause.fill", action: .pause) {
                        Task {
                            if await vm.pause() { onChanged() }
                        }
                    }
                }
                actionButton("Edit", systemImage: "pencil", action: nil) {
                    showEditor = true
                }
                actionButton("Delete", systemImage: "trash", action: .delete,
                             tintOverride: themeManager.dangerColor) {
                    confirmingDelete = true
                }
            }
            .padding(.horizontal)
        }
        .padding(.vertical, 8)
    }

    @ViewBuilder private func sectionBody(_ detail: Monitors.DetailV1) -> some View {
        switch section {
        case .latest: latestSection
        case .updates: updatesSection
        case .runs: runsSection
        case .settings: settingsSection(detail)
        }
    }

    // MARK: - Latest

    @ViewBuilder private var latestSection: some View {
        if let latest = vm.latestUpdate {
            ScrollView {
                updateCard(latest, highlighted: false)
                    .padding(.horizontal)
                    .padding(.vertical, 10)
            }
        } else {
            sectionEmpty("No updates yet",
                         message: "The first accepted run records a baseline; material changes appear here.")
        }
    }

    // MARK: - Updates

    @ViewBuilder private var updatesSection: some View {
        if vm.updates.isEmpty {
            sectionEmpty("No update history",
                         message: "Updates are the durable notification ledger — material changes, baselines, and every-run receipts.")
        } else {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 10) {
                        ForEach(vm.updates) { update in
                            updateCard(update,
                                       highlighted: update.updateID == vm.highlightUpdateID)
                                .id(update.updateID)
                        }
                    }
                    .padding(.horizontal)
                    .padding(.vertical, 10)
                }
                .onAppear {
                    if let target = vm.highlightUpdateID {
                        proxy.scrollTo(target, anchor: .center)
                    }
                }
            }
        }
    }

    private func updateCard(_ update: Monitors.UpdateDetailV1, highlighted: Bool) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Text(update.headline)
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                Spacer(minLength: 8)
                pill(Monitors.runStatusLabel(update.status.rawValue),
                     tint: statusTint(update.status.rawValue))
            }
            Text(update.summary)
                .font(.themed(13))
                .foregroundColor(themeManager.secondaryTextColor)
            ForEach(update.findings.indices, id: \.self) { index in
                findingRow(update.findings[index])
            }
            HStack(spacing: 6) {
                if let time = relativeTime(update.occurredAt) {
                    Text(time)
                }
                if !update.notification.emitted {
                    Text("· Not notified")
                }
                Spacer(minLength: 0)
            }
            .font(.themed(11))
            .foregroundColor(themeManager.secondaryTextColor)
            if Monitors.isMaterialUpdate(update) {
                // Plan §10: every MATERIAL update offers the four actions.
                feedbackActionsRow(update)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(themeManager.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(highlighted ? themeManager.accentColor : themeManager.cardBorderColor,
                        lineWidth: highlighted ? 2 : 1)
        }
        .accessibilityIdentifier("monitor-update-\(update.updateID)")
    }

    // MARK: - Feedback (Phase 6, plan §10)

    /// Useful / Not relevant verdict chips + Edit / Pause on one material
    /// update card. Verdict chips carry the stored verdict as a selected
    /// state (optimistic while the POST is in flight; the view model rolls
    /// back on error into the shared action alert).
    private func feedbackActionsRow(_ update: Monitors.UpdateDetailV1) -> some View {
        let verdict = vm.verdict(for: update.updateID)
        let inFlight = vm.isFeedbackInFlight(update.updateID)
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                verdictChip("Useful", systemImage: "hand.thumbsup",
                            active: verdict == .useful,
                            activeTint: themeManager.successColor,
                            inFlight: inFlight,
                            axLabel: "Mark this update as useful",
                            axHint: "Records that this update was worth seeing.",
                            identifier: "monitor-feedback-useful-\(update.updateID)") {
                    Task { await vm.submitFeedback(updateID: update.updateID, verdict: .useful) }
                }
                verdictChip("Not relevant", systemImage: "hand.thumbsdown",
                            active: verdict == .notRelevant,
                            activeTint: themeManager.warningColor,
                            inFlight: inFlight,
                            axLabel: "Mark this update as not relevant",
                            axHint: "Records this update as a false positive for this monitor.",
                            identifier: "monitor-feedback-not-relevant-\(update.updateID)") {
                    Task {
                        await vm.submitFeedback(updateID: update.updateID, verdict: .notRelevant)
                    }
                }
                updateActionChip("Edit monitor", systemImage: "pencil",
                                 axHint: "Opens the monitor editor.",
                                 identifier: "monitor-update-edit-\(update.updateID)") {
                    showEditor = true
                }
                if vm.detail?.schedule != nil && !vm.isPaused {
                    updateActionChip("Pause monitor", systemImage: "pause.fill",
                                     axHint: "Pauses the monitor's schedule.",
                                     identifier: "monitor-update-pause-\(update.updateID)") {
                        Task { if await vm.pause() { onChanged() } }
                    }
                }
            }
        }
    }

    private func verdictChip(
        _ label: String, systemImage: String, active: Bool, activeTint: Color,
        inFlight: Bool, axLabel: String, axHint: String, identifier: String,
        action: @escaping () -> Void
    ) -> some View {
        let tint = active ? activeTint : themeManager.secondaryTextColor
        return Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: active ? "\(systemImage).fill" : systemImage)
                    .font(.system(size: 10, weight: .semibold))
                Text(label)
            }
            .font(.themed(11, weight: .semibold))
            .padding(.horizontal, 9).padding(.vertical, 5)
            .background(tint.opacity(active ? 0.15 : 0.08))
            .foregroundColor(tint)
            .clipShape(Capsule())
            .overlay { Capsule().stroke(tint.opacity(active ? 0.4 : 0.2), lineWidth: 1) }
        }
        .buttonStyle(.plain)
        .disabled(inFlight)
        .accessibilityLabel(axLabel)
        .accessibilityHint(axHint)
        .accessibilityAddTraits(active ? .isSelected : [])
        .accessibilityIdentifier(identifier)
    }

    private func updateActionChip(
        _ label: String, systemImage: String, axHint: String, identifier: String,
        action: @escaping () -> Void
    ) -> some View {
        let tint = themeManager.accentColor
        return Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: systemImage)
                    .font(.system(size: 10, weight: .semibold))
                Text(label)
            }
            .font(.themed(11, weight: .semibold))
            .padding(.horizontal, 9).padding(.vertical, 5)
            .background(tint.opacity(0.1))
            .foregroundColor(tint)
            .clipShape(Capsule())
            .overlay { Capsule().stroke(tint.opacity(0.25), lineWidth: 1) }
        }
        .buttonStyle(.plain)
        .disabled(vm.busyAction != nil)
        .accessibilityLabel(label)
        .accessibilityHint(axHint)
        .accessibilityIdentifier(identifier)
    }

    private func findingRow(_ finding: Monitors.FindingV1) -> some View {
        HStack(alignment: .top, spacing: 6) {
            pill(finding.classification.rawValue, tint: classificationTint(finding.classification))
            VStack(alignment: .leading, spacing: 2) {
                Text(finding.title)
                    .font(.themed(12, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                Text(finding.summary)
                    .font(.themed(11))
                    .foregroundColor(themeManager.secondaryTextColor)
                    .lineLimit(3)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(themeManager.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    // MARK: - Runs

    @ViewBuilder private var runsSection: some View {
        if vm.runs.isEmpty {
            sectionEmpty("No accepted runs yet",
                         message: "Every accepted run — including unchanged and degraded scans — appears here.")
        } else {
            ScrollView {
                LazyVStack(spacing: 10) {
                    ForEach(vm.runs, id: \.executionID) { run in
                        runCard(run)
                    }
                }
                .padding(.horizontal)
                .padding(.vertical, 10)
            }
        }
    }

    private func runCard(_ run: Monitors.RunResultV1) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                pill(Monitors.runStatusLabel(run.status.rawValue),
                     tint: statusTint(run.status.rawValue))
                if !run.completeScan {
                    pill("partial scan", tint: themeManager.warningColor)
                }
                Spacer(minLength: 0)
                if let time = relativeTime(run.completedAt) {
                    Text(time)
                        .font(.themed(11))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
            }
            Text("Scanned \(run.counts.scanned) · \(run.counts.new) new · \(run.counts.updated) updated · \(run.counts.possiblyRemoved) possibly removed")
                .font(.themed(12))
                .foregroundColor(themeManager.secondaryTextColor)
            ForEach(run.sourceOutcomes.indices, id: \.self) { index in
                sourceOutcomeRow(run.sourceOutcomes[index])
            }
            if let problem = run.accessProblem {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: "lock.trianglebadge.exclamationmark")
                    Text(problem.message)
                }
                .font(.themed(12))
                .foregroundColor(themeManager.warningColor)
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(themeManager.warningColor.opacity(0.12))
                .clipShape(RoundedRectangle(cornerRadius: 10))
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
    }

    private func sourceOutcomeRow(_ outcome: Monitors.SourceOutcomeV1) -> some View {
        HStack(spacing: 6) {
            Image(systemName: outcome.status == .ok ? "checkmark.circle" : "exclamationmark.circle")
                .font(.system(size: 11))
                .foregroundColor(outcome.status == .ok
                    ? themeManager.successColor : themeManager.warningColor)
            Text(outcome.source)
                .font(.themed(11))
                .foregroundColor(themeManager.secondaryTextColor)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 4)
            Text(outcome.status == .ok
                 ? "\(outcome.itemsScanned) items"
                 : outcome.status.rawValue.replacingOccurrences(of: "_", with: " "))
                .font(.themed(11, weight: .medium))
                .foregroundColor(outcome.status == .ok
                    ? themeManager.secondaryTextColor : themeManager.warningColor)
        }
    }

    // MARK: - Settings

    private func settingsSection(_ detail: Monitors.DetailV1) -> some View {
        List {
            SwiftUI.Section("Contract") {
                settingsRow("Objective", detail.spec.objective)
                if !detail.spec.sources.urls.isEmpty {
                    settingsRow("URLs", detail.spec.sources.urls.joined(separator: "\n"))
                }
                if !detail.spec.sources.domains.isEmpty {
                    settingsRow("Domains", detail.spec.sources.domains.joined(separator: "\n"))
                }
                if !detail.spec.querySeeds.isEmpty {
                    settingsRow("Search phrases", detail.spec.querySeeds.joined(separator: "\n"))
                }
                if !detail.spec.sources.authenticatedSources.isEmpty {
                    settingsRow("Signed-in sources",
                                detail.spec.sources.authenticatedSources.joined(separator: "\n"))
                }
                if !detail.spec.includeRules.isEmpty {
                    settingsRow("Include", detail.spec.includeRules.joined(separator: "\n"))
                }
                if !detail.spec.excludeRules.isEmpty {
                    settingsRow("Exclude", detail.spec.excludeRules.joined(separator: "\n"))
                }
                settingsRow("Match mode", Monitors.matchModeLabel(detail.spec.matchMode))
                settingsRow("Notify", Monitors.notificationPolicyLabel(detail.spec.notificationPolicy))
                settingsRow("Notify on first baseline",
                            detail.spec.notifyInitialBaseline ? "Yes" : "No")
            }
            SwiftUI.Section("Schedule") {
                settingsRow("Cadence", vm.cadenceSummary)
                settingsRow("Revision", "\(detail.monitorRevision)")
                settingsRow("Fired", "\(detail.state.scheduleFireCount) times")
            }
            SwiftUI.Section("Task") {
                // Execution-level inspection lives on the SHARED task detail.
                Button {
                    onOpenTask(detail.taskID)
                } label: {
                    Label("Open task view", systemImage: "arrow.up.right.square")
                }
                .accessibilityIdentifier("monitor-open-task")
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
    }

    private func settingsRow(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label)
                .font(.themed(11, weight: .semibold))
                .foregroundColor(themeManager.secondaryTextColor)
            Text(value)
                .font(.themed(13))
                .foregroundColor(themeManager.textColor)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Small pieces

    private func sectionEmpty(_ title: String, message: String) -> some View {
        VStack(spacing: 8) {
            Spacer()
            Image(systemName: "tray")
                .font(.system(size: 28))
                .foregroundColor(themeManager.secondaryTextColor.opacity(0.5))
            Text(title)
                .font(.themed(14, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            Text(message)
                .font(.themed(12))
                .foregroundColor(themeManager.secondaryTextColor)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 28)
            Spacer()
        }
        .frame(maxWidth: .infinity)
    }

    private func actionButton(
        _ label: String, systemImage: String,
        action: MonitorDetailViewModel.MonitorAction?,
        tintOverride: Color? = nil,
        run: @escaping () -> Void
    ) -> some View {
        let tint = tintOverride ?? themeManager.accentColor
        let busy = action != nil && vm.busyAction == action
        return Button(action: run) {
            HStack(spacing: 5) {
                if busy { ProgressView().controlSize(.small).tint(tint) }
                else { Image(systemName: systemImage).font(.system(size: 11, weight: .semibold)) }
                Text(busy ? "Working…" : label)
            }
            .font(.themed(12, weight: .bold))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(tint.opacity(0.12))
            .foregroundColor(tint)
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .overlay {
                RoundedRectangle(cornerRadius: 7).stroke(tint.opacity(0.25), lineWidth: 1)
            }
        }
        .buttonStyle(.plain)
        .disabled(vm.busyAction != nil)
        .accessibilityIdentifier("monitor-action-\(label.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }

    private func stateBadge(paused: Bool) -> some View {
        let tint = paused ? themeManager.warningColor : themeManager.successColor
        return Text(paused ? "Paused" : "Active")
            .font(.themed(11, weight: .bold))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.15))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    private func pill(_ label: String, tint: Color) -> some View {
        Text(label)
            .font(.themed(11, weight: .medium))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.12))
            .foregroundColor(tint)
            .clipShape(Capsule())
    }

    private func statusTint(_ status: String) -> Color {
        switch status {
        case "changed": return themeManager.infoColor
        case "unchanged": return themeManager.successColor
        case "degraded": return themeManager.warningColor
        case "failed": return themeManager.dangerColor
        default: return themeManager.secondaryTextColor
        }
    }

    private func classificationTint(_ classification: Monitors.FindingClassification) -> Color {
        switch classification {
        case .new: return themeManager.infoColor
        case .updated: return themeManager.accentColor
        case .unchanged: return themeManager.secondaryTextColor
        case .possiblyRemoved: return themeManager.warningColor
        }
    }

    private func relativeTime(_ iso: String) -> String? {
        guard let date = TaskV3.parseISO(iso) else { return nil }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: Date())
    }
}
