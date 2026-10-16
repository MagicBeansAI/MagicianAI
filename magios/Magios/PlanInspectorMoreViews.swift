import SwiftUI

// MARK: - History

/// The latest 20 saved versions; tap one to preview it read-only, then
/// Restore (confirmed) — the plan is refetched and Steps shown.
struct PlanHistoryView: View {
    @ObservedObject var model: PlanInspectorModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var confirmRestore = false

    var body: some View {
        Group {
            if let preview = model.preview { previewView(preview) } else { list }
        }
        .task { if model.versions.isEmpty && !model.versionsLoading { await model.loadVersions() } }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Saved versions").font(.themed(15, weight: .semibold))
                    Spacer()
                    if model.versionsLoading { ProgressView() }
                    else if model.hasMoreVersions { Text("Latest 20 shown").font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
                }
                if let error = model.versionsError {
                    PlanMessage(text: error, isError: true)
                } else if model.versions.isEmpty && !model.versionsLoading {
                    PlanMessage(text: "No saved versions yet. Saving an edit keeps the previous plan here.")
                }
                ForEach(model.versions) { version in
                    Button { Task { await model.previewVersion(version) } } label: {
                        PlanCard {
                            HStack(alignment: .firstTextBaseline) {
                                Text(version.title).font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
                                Spacer()
                                Image(systemName: "chevron.right").font(.system(size: 12, weight: .semibold))
                                    .foregroundColor(theme.secondaryTextColor)
                            }
                            HStack(spacing: 6) {
                                PlanTag(text: version.date.formatted(date: .abbreviated, time: .shortened))
                                PlanTag(text: "\(version.stepCount) step\(version.stepCount == 1 ? "" : "s")")
                                if let confidence = version.confidence { PlanTag(text: PlanStyle.percent(confidence), mono: true) }
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("plan-version-\(version.epochMs)")
                }
            }
            .padding(16)
        }
    }

    private func previewView(_ version: PlanVersionSummary) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Button { model.clearPreview() } label: {
                    Label("Versions", systemImage: "chevron.left").frame(minHeight: Lively.tapMin)
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 2) {
                    Text(version.title).font(.themed(13, weight: .semibold)).lineLimit(1)
                    Text(version.date.formatted(date: .abbreviated, time: .shortened))
                        .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
            }
            .padding(.horizontal, 16)
            switch model.previewGraph {
            case .idle, .loading:
                ProgressView("Loading version…").frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed(let message):
                PlanMessage(text: message, isError: true).padding(16)
                Spacer()
            case .loaded(let graph):
                PlanStepsView(graph: graph, onSelect: { _ in })
                    .safeAreaInset(edge: .bottom) {
                        Button(role: .destructive) { confirmRestore = true } label: {
                            Text(model.isRestoring ? "Restoring…" : "Restore this version")
                                .font(.themed(15, weight: .semibold))
                                .frame(maxWidth: .infinity, minHeight: Lively.tapMin)
                        }
                        .buttonStyle(.borderedProminent).buttonBorderShape(.roundedRectangle(radius: 10))
                        .tint(theme.accentFillColor)
                        .disabled(model.isRestoring)
                        .padding(.horizontal, 16).padding(.vertical, 8)
                        .background(.bar)
                        .accessibilityIdentifier("plan-restore-version")
                    }
            }
        }
        .confirmationDialog("Restore historical plan version?", isPresented: $confirmRestore, titleVisibility: .visible) {
            Button("Restore", role: .destructive) { Task { await model.restorePreview() } }
        } message: {
            Text("This will replace the current task plan with this historical version.")
        }
    }
}

// MARK: - More panes

/// Analysis · Slots · Attempts · Clarifications, pushed from the More menu.
struct PlanInspectorPaneView: View {
    @ObservedObject var model: PlanInspectorModel
    let pane: PlanInspectorPane
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) { content }
                .padding(16)
        }
        .background(theme.washGradient.ignoresSafeArea())
        .navigationTitle(pane.title)
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await model.loadPane(pane) }
        .task { if model.paneState(pane) == .idle { await model.loadPane(pane) } }
    }

    @ViewBuilder
    private var content: some View {
        switch pane {
        case .analysis:
            loadable(model.analysis) { value in
                if let value { PlanCard { PlanJSONDisclosure(title: "Query analysis", value: value, expanded: true) } }
                else { PlanMessage(text: "No query analysis was persisted for this task plan.") }
            }
        case .slots:
            loadable(model.slots) { snapshot in
                if let snapshot { PlanSlotsView(snapshot: snapshot) }
                else { PlanMessage(text: "No slot graph snapshot was persisted for this task plan.") }
            }
        case .attempts:
            loadable(model.attempts) { attempts in
                if attempts.isEmpty { PlanMessage(text: "No strategy attempts were persisted for this task plan.") }
                ForEach(attempts) { attempt in attemptCard(attempt) }
            }
        case .clarifications:
            PlanClarificationsView(model: model)
        }
    }

    @ViewBuilder
    private func loadable<Value: Equatable, Content: View>(
        _ state: PlanLoadable<Value>, @ViewBuilder content: (Value) -> Content
    ) -> some View {
        switch state {
        case .idle, .loading: ProgressView().frame(maxWidth: .infinity, minHeight: 120)
        case .failed(let message): PlanMessage(text: message, isError: true)
        case .loaded(let value): content(value)
        }
    }

    private func attemptCard(_ attempt: PlanStrategyAttempt) -> some View {
        PlanCard {
            Text(attempt.title).font(.themed(14, weight: .semibold))
            HStack(spacing: 6) {
                PlanTag(text: "Confidence \(PlanStyle.percent(attempt.confidence))", mono: true)
                PlanTag(text: attempt.failureReason == nil ? "Completed" : "Failed",
                        color: attempt.failureReason == nil ? theme.successColor : theme.dangerColor)
                if let at = attempt.attemptedAt { PlanTag(text: at.formatted(date: .abbreviated, time: .shortened)) }
            }
            if let reason = attempt.failureReason { PlanMessage(text: reason, isError: true) }
            if let resources = attempt.resourcesUsed { PlanJSONDisclosure(title: "Resources used", value: resources) }
            if let result = attempt.explorationResult {
                PlanJSONDisclosure(title: "Exploration result", value: result, expanded: attempt.index == 0)
            }
        }
    }
}

/// Slots grouped by type (largest first) with each group's mean confidence.
struct PlanSlotsView: View {
    let snapshot: PlanSlotSnapshot
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                PlanTag(text: "\(snapshot.slots.count) slots")
                if let overall = snapshot.overallConfidence { PlanTag(text: "Overall \(PlanStyle.percent(overall))", mono: true) }
                if !snapshot.unresolvedSlots.isEmpty {
                    PlanTag(text: "\(snapshot.unresolvedSlots.count) unresolved", color: theme.warningColor)
                }
            }
            if snapshot.slots.isEmpty { PlanMessage(text: "No slot records were captured for this task plan.") }
            ForEach(snapshot.groups) { group in
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(group.type.capitalized).font(.themed(14, weight: .semibold))
                        Text("\(group.slots.count)").font(.themedMono(12)).foregroundColor(theme.secondaryTextColor)
                        Spacer()
                        PlanTag(text: "avg \(PlanStyle.percent(group.averageConfidence))",
                                color: slotColor(group.averageConfidence), mono: true)
                    }
                    ForEach(group.slots) { slot in
                        PlanCard {
                            HStack(alignment: .firstTextBaseline) {
                                Text(slot.shortId).font(.themedMono(13, weight: .semibold))
                                Spacer()
                                PlanTag(text: PlanStyle.percent(slot.confidence), color: slotColor(slot.confidence), mono: true)
                            }
                            Text(slot.value.isNull ? "No value yet" : slot.value.displayText)
                                .font(.themed(13))
                                .foregroundColor(slot.value.isNull ? theme.secondaryTextColor : theme.textColor)
                                .lineLimit(6)
                            if !slot.sources.isEmpty {
                                Text(slot.sources.joined(separator: " · ")).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                            }
                        }
                    }
                }
            }
            if let clarified = snapshot.clarifiedTask { PlanCard { PlanJSONDisclosure(title: "Clarified task", value: clarified) } }
            if let mappings = snapshot.triggerMappings { PlanCard { PlanJSONDisclosure(title: "Slot trigger mappings", value: mappings) } }
            if let boosts = snapshot.boostResults { PlanCard { PlanJSONDisclosure(title: "Confidence boost results", value: boosts) } }
            if let enrichment = snapshot.enrichmentSummary { PlanCard { PlanJSONDisclosure(title: "Enrichment summary", value: enrichment) } }
        }
    }

    /// Web `getConfidenceClass`: high ≥ 0.8, medium ≥ 0.5.
    private func slotColor(_ value: Double) -> Color {
        value >= 0.8 ? theme.successColor : value >= 0.5 ? theme.warningColor : theme.dangerColor
    }
}

/// Pending planning questions (option chips or typed answer), the answered
/// history, and Advanced → manual resume.
struct PlanClarificationsView: View {
    @ObservedObject var model: PlanInspectorModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var showAdvanced = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Pending questions").font(.themed(15, weight: .semibold))
                Spacer()
                PlanTag(text: "\(model.pendingQuestions.count) pending",
                        color: model.pendingQuestions.isEmpty ? theme.successColor : theme.warningColor)
            }
            if model.pendingQuestions.isEmpty {
                PlanMessage(text: "There are no pending planning questions right now.")
            }
            ForEach(model.pendingQuestions) { question in questionCard(question) }

            HStack {
                Text("Answered").font(.themed(15, weight: .semibold))
                Spacer()
                if let entries = model.clarifications.value { PlanTag(text: "\(entries.count) answered") }
            }
            .padding(.top, 8)
            switch model.clarifications {
            case .idle, .loading: ProgressView().frame(maxWidth: .infinity, minHeight: 60)
            case .failed(let message): PlanMessage(text: message, isError: true)
            case .loaded(let entries):
                if entries.isEmpty { PlanMessage(text: "No clarification history has been recorded yet.") }
                ForEach(entries) { entry in
                    PlanCard {
                        Text(entry.questionText).font(.themed(14, weight: .semibold))
                        HStack(spacing: 6) {
                            PlanTag(text: entry.stageLabel)
                            if let at = entry.answeredAt { PlanTag(text: at.formatted(date: .abbreviated, time: .shortened)) }
                            PlanTag(text: "\(entry.slotUpdates.count) slot update\(entry.slotUpdates.count == 1 ? "" : "s")")
                        }
                        if let slot = entry.sourceSlotId {
                            Text("Source slot: \(slot)").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        }
                        if !entry.slotUpdates.isEmpty { PlanJSONDisclosure(title: "Slot updates", value: .array(entry.slotUpdates)) }
                    }
                }
            }
            if model.canResume { advanced }
        }
    }

    private func questionCard(_ question: PlanQuestion) -> some View {
        let busy = model.answering.contains(question.id)
        let text = Binding(get: { model.answers[question.id] ?? "" }, set: { model.answers[question.id] = $0 })
        return PlanCard {
            Text(question.text).font(.themed(15, weight: .semibold))
            HStack(spacing: 6) {
                if let slot = question.sourceSlotId { PlanTag(text: slot, mono: true) }
                if let urgency = question.urgency { PlanTag(text: "Urgency \(PlanStyle.percent(urgency))", color: theme.warningColor) }
            }
            if !question.options.isEmpty {
                FlowRow(spacing: 8) {
                    ForEach(question.options) { option in
                        Button(option.label) { Task { await model.answer(question, with: option.value) } }
                            .buttonStyle(ChipStyle())
                            .disabled(busy)
                            .accessibilityHint(option.description ?? "Answers with \(option.label)")
                    }
                }
            }
            TextField("Type your answer…", text: text, axis: .vertical)
                .lineLimit(1...4)
                .textFieldStyle(.roundedBorder)
                .disabled(busy)
                .accessibilityIdentifier("plan-question-answer-\(question.id)")
            HStack {
                if let error = model.answerErrors[question.id] { Text(error).font(.themed(12)).foregroundColor(theme.dangerColor) }
                Spacer()
                Button(busy ? "Sending…" : "Submit") { Task { await model.answer(question) } }
                    .buttonStyle(ChipStyle(prominent: true))
                    .disabled(busy || text.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    private var advanced: some View {
        PlanCard {
            DisclosureGroup(isExpanded: $showAdvanced) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Resume planning when you already know the missing slot values, without answering through the question flow.")
                        .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    TextField("Handled question id (optional)", text: $model.resumeForm.handledQuestion)
                        .textFieldStyle(.roundedBorder).textInputAutocapitalization(.never).autocorrectionDisabled()
                    TextField("Updated confidence 0–1 (optional)", text: $model.resumeForm.updatedConfidence)
                        .textFieldStyle(.roundedBorder).keyboardType(.decimalPad)
                    TextField(#"Slots JSON, e.g. [{"slot_id":"contact","slot_type":"entity","value":"Jordan"}]"#,
                              text: $model.resumeForm.slotsJSON, axis: .vertical)
                        .lineLimit(3...8).font(.themedMono(12)).textFieldStyle(.roundedBorder)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    if let error = model.resumeError { Text(error).font(.themed(12)).foregroundColor(theme.dangerColor) }
                    Button(model.isResuming ? "Resuming…" : "Resume planning") { Task { await model.submitResume() } }
                        .buttonStyle(ChipStyle(prominent: true))
                        .disabled(model.isResuming)
                }
                .padding(.top, 6)
            } label: {
                HStack {
                    Text("Advanced · manual resume").font(.themed(14, weight: .semibold))
                    if let status = model.status { PlanTag(text: status.label, color: theme.warningColor) }
                }
                .frame(minHeight: Lively.tapMin)
            }
            .tint(theme.accentColor)
        }
    }
}

/// A muted (or error) line inside a pane.
struct PlanMessage: View {
    let text: String
    var isError = false
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(text)
            .font(.themed(13))
            .foregroundColor(isError ? theme.dangerColor : theme.secondaryTextColor)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
