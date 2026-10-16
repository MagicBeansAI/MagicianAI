//  MonitorComposerView.swift
//  Recurring Monitors (Phase 5, iOS) — the native create/edit sheet (§9.3.3)
//  with the web composer's simple/advanced split and the SAME
//  review-before-activate step: simple fields (objective, URLs, cadence
//  preset, notification policy) up front; domains / search phrases / rules /
//  match mode / signed-in sources / baseline-notify behind one "More options"
//  disclosure. Submitting first shows the NORMALIZED contract and the exact
//  cadence; the POST/PATCH happens only from that review step. Validation
//  mirrors the backend's stable snake_case admission reasons (`MonitorForm`).

import SwiftUI

struct MonitorComposerView: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var themeManager = ThemeManager.shared
    @StateObject private var vm: MonitorComposerViewModel
    @State private var showAdvanced = false

    /// Called after a successful create/save (the presenter refreshes).
    let onComplete: () -> Void

    init(mode: MonitorComposerViewModel.Mode,
         initialForm: MonitorForm = MonitorForm(),
         client: Monitors.APIClient? = nil,
         onComplete: @escaping () -> Void = {}) {
        _vm = StateObject(wrappedValue: MonitorComposerViewModel(
            mode: mode, form: initialForm, client: client))
        self.onComplete = onComplete
    }

    private var isEdit: Bool {
        if case .edit = vm.mode { return true }
        return false
    }

    /// Phase 7 convert mode: the sheet authors a SPEC for an existing task;
    /// the task keeps its schedule, so the cadence editor is replaced by a
    /// read-only "keeps its current schedule" line.
    private var isConvert: Bool { vm.isConvert }

    private var keptCadence: String? {
        if case .convert(_, let cadence) = vm.mode { return cadence }
        return nil
    }

    private var navigationTitle: String {
        if isConvert { return "Convert to monitor" }
        return isEdit ? "Edit monitor" : "New monitor"
    }

    var body: some View {
        NavigationStack {
            Group {
                switch vm.step {
                case .form:
                    formStep
                case .review(let spec, let schedule):
                    reviewStep(spec: spec, schedule: schedule)
                }
            }
            .navigationTitle(navigationTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .interactiveDismissDisabled(vm.isSubmitting)
    }

    // MARK: - Step 1: the form

    private var formStep: some View {
        Form {
            Section("What to watch") {
                TextField("Title (optional)", text: $vm.form.title)
                    .accessibilityIdentifier("monitor-form-title")
                VStack(alignment: .leading, spacing: 4) {
                    Text("Objective")
                        .font(.themed(11, weight: .semibold))
                        .foregroundColor(themeManager.secondaryTextColor)
                    TextEditor(text: $vm.form.objective)
                        .frame(minHeight: 64)
                        .accessibilityIdentifier("monitor-form-objective")
                }
            }

            Section {
                VStack(alignment: .leading, spacing: 4) {
                    Text("URLs — one per line")
                        .font(.themed(11, weight: .semibold))
                        .foregroundColor(themeManager.secondaryTextColor)
                    TextEditor(text: $vm.form.urlsText)
                        .frame(minHeight: 64)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("monitor-form-urls")
                }
            } header: {
                Text("Sources")
            } footer: {
                Text("Add at least one URL here, or a domain/search phrase under More options.")
            }

            if isConvert {
                // Conversion never touches the task's schedule (the POST
                // carries no schedule key) — show what the task keeps.
                Section("Cadence") {
                    Text("Keeps its current schedule: \(keptCadence ?? "unscheduled")")
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                        .accessibilityIdentifier("monitor-form-kept-cadence")
                }
            } else {
                Section("Cadence") {
                    Picker("Check", selection: $vm.form.cadence) {
                        ForEach(MonitorForm.cadencePresets) { preset in
                            Text(preset.label).tag(preset.id)
                        }
                        Text("Custom cron…").tag("custom")
                        // Web parity: EDIT mode hides "On demand only" —
                        // selecting it would be a silent no-op (cadence "none"
                        // makes the PATCH omit `schedule`, so the existing
                        // schedule stays; the review step discloses this). The
                        // row stays visible while it IS the current selection
                        // (a monitor with an Interval/Once/OnEvent/no schedule
                        // opens the editor at "none").
                        if !isEdit || vm.form.cadence == "none" {
                            Text("On demand only").tag("none")
                        }
                    }
                    .accessibilityIdentifier("monitor-form-cadence")
                    if vm.form.cadence == "custom" {
                        TextField("Cron · 0 9 * * *", text: $vm.form.cronExpression)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                    if vm.form.cadence != "none" {
                        TextField("Timezone (optional, e.g. America/Los_Angeles)",
                                  text: $vm.form.timezone)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                }
            }

            Section("Notifications") {
                Picker("Notify", selection: $vm.form.notificationPolicy) {
                    ForEach(Monitors.NotificationPolicy.allCases, id: \.self) { policy in
                        Text(Monitors.notificationPolicyLabel(policy)).tag(policy)
                    }
                }
                .accessibilityIdentifier("monitor-form-policy")
            }

            Section {
                DisclosureGroup("More options", isExpanded: $showAdvanced) {
                    lineListField("Domains — one per line", text: $vm.form.domainsText,
                                  identifier: "monitor-form-domains")
                    lineListField("Search phrases — one per line", text: $vm.form.querySeedsText,
                                  identifier: "monitor-form-seeds")
                    lineListField("Include rules — one per line", text: $vm.form.includeRulesText,
                                  identifier: "monitor-form-include")
                    lineListField("Exclude rules — one per line", text: $vm.form.excludeRulesText,
                                  identifier: "monitor-form-exclude")
                    Picker("Match mode", selection: $vm.form.matchMode) {
                        ForEach(Monitors.MatchMode.allCases, id: \.self) { mode in
                            Text(Monitors.matchModeLabel(mode)).tag(mode)
                        }
                    }
                    lineListField("Signed-in sources — one per line",
                                  text: $vm.form.authenticatedText,
                                  identifier: "monitor-form-authenticated")
                    Toggle("Notify on the first baseline", isOn: $vm.form.notifyInitialBaseline)
                }
            }

            if let error = vm.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.warningColor)
                        .accessibilityIdentifier("monitor-form-error")
                }
            }

            Section {
                Button {
                    _ = vm.review()
                } label: {
                    Text("Review")
                        .font(.themed(14, weight: .bold))
                        .frame(maxWidth: .infinity)
                }
                .accessibilityIdentifier("monitor-form-review")
            }
        }
    }

    private func lineListField(_ label: String, text: Binding<String>,
                               identifier: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.themed(11, weight: .semibold))
                .foregroundColor(themeManager.secondaryTextColor)
            TextEditor(text: text)
                .frame(minHeight: 48)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .accessibilityIdentifier(identifier)
        }
    }

    // MARK: - Step 2: review-before-activate

    private func reviewStep(spec: Monitors.SpecV1,
                            schedule: Monitors.ScheduleWire?) -> some View {
        Form {
            Section {
                Text("This is the exact contract the monitor will run — normalized the way the server stores it.")
                    .font(.themed(12))
                    .foregroundColor(themeManager.secondaryTextColor)
            }

            Section("Contract") {
                reviewRow("Objective", spec.objective)
                if !spec.sources.urls.isEmpty {
                    reviewRow("URLs", spec.sources.urls.joined(separator: "\n"))
                }
                if !spec.sources.domains.isEmpty {
                    reviewRow("Domains", spec.sources.domains.joined(separator: "\n"))
                }
                if !spec.querySeeds.isEmpty {
                    reviewRow("Search phrases", spec.querySeeds.joined(separator: "\n"))
                }
                if !spec.sources.authenticatedSources.isEmpty {
                    reviewRow("Signed-in sources",
                              spec.sources.authenticatedSources.joined(separator: "\n"))
                }
                if !spec.includeRules.isEmpty {
                    reviewRow("Include", spec.includeRules.joined(separator: "\n"))
                }
                if !spec.excludeRules.isEmpty {
                    reviewRow("Exclude", spec.excludeRules.joined(separator: "\n"))
                }
                reviewRow("Match mode", Monitors.matchModeLabel(spec.matchMode))
                reviewRow("Notify", Monitors.notificationPolicyLabel(spec.notificationPolicy))
                reviewRow("Notify on first baseline", spec.notifyInitialBaseline ? "Yes" : "No")
            }

            Section("Exact cadence") {
                if isConvert {
                    // Conversion keeps the task's schedule verbatim.
                    reviewRow("Cadence", keptCadence ?? "unscheduled")
                        .accessibilityIdentifier("monitor-review-cadence")
                    Text((keptCadence ?? "unscheduled") == "unscheduled"
                         ? "The task has no schedule — the monitor runs only when you tap Run now."
                         : "The task keeps its existing schedule.")
                        .font(.themed(11))
                        .foregroundColor(themeManager.secondaryTextColor)
                } else {
                    // The SAME summary string the list rows and chat preview show.
                    reviewRow("Cadence", Monitors.cadenceSummary(schedule))
                        .accessibilityIdentifier("monitor-review-cadence")
                    if case .cron(let expression, let timezone)? = schedule?.kind {
                        reviewRow("Raw cron", expression)
                        if let timezone { reviewRow("Timezone", timezone) }
                    }
                    if schedule == nil, isEdit {
                        Text("No cadence change — the existing schedule stays as it is.")
                            .font(.themed(11))
                            .foregroundColor(themeManager.secondaryTextColor)
                    } else if schedule == nil {
                        Text("Runs only when you tap Run now.")
                            .font(.themed(11))
                            .foregroundColor(themeManager.secondaryTextColor)
                    }
                }
            }

            if let error = vm.errorMessage {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.warningColor)
                        .accessibilityIdentifier("monitor-review-error")
                }
            }

            Section {
                Button {
                    Task {
                        if await vm.activate() {
                            onComplete()
                            dismiss()
                        }
                    }
                } label: {
                    HStack(spacing: 6) {
                        if vm.isSubmitting { ProgressView().controlSize(.small) }
                        Text(vm.isSubmitting
                             ? "Working…"
                             : (isConvert
                                ? "Convert to monitor"
                                : (isEdit ? "Save changes" : "Create monitor")))
                            .font(.themed(14, weight: .bold))
                    }
                    .frame(maxWidth: .infinity)
                }
                .disabled(vm.isSubmitting)
                .accessibilityIdentifier("monitor-review-activate")

                Button("Back to editing") { vm.backToForm() }
                    .disabled(vm.isSubmitting)
                    .accessibilityIdentifier("monitor-review-back")
            }
        }
    }

    private func reviewRow(_ label: String, _ value: String) -> some View {
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
}
