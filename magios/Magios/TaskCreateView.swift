import SwiftUI

/// New-task form (POST /v3/tasks). Mirrors the web create form's fields: title,
/// description, agent (required), thread, priority, due date, tags.
struct TaskCreateView: View {
    @ObservedObject var vm: TasksViewModel
    var lockedThreadID: String? = nil
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    @State private var title = ""
    @State private var taskDescription = ""
    @State private var agentId = ""
    @State private var threadId = "general"
    @State private var priority = ""          // "" = none
    @State private var hasDueDate = false
    @State private var dueDate = Date()
    @State private var tagsText = ""
    @State private var repeatMode = "none"    // none | daily | weekdays | weekly
    @State private var customCron = ""
    @State private var scheduleTimezone = TimeZone.current.identifier
    private var invalidSchedule: Bool { repeatMode != "none" && (TimeZone(identifier: scheduleTimezone.trimmingCharacters(in: .whitespacesAndNewlines)) == nil || (repeatMode == "custom" && customCron.split(whereSeparator: \.isWhitespace).count != 5)) }
    @State private var outputMode = "accumulate"   // accumulate | overwrite
    @State private var dependsOnText = ""     // comma-separated task ids
    @State private var selectedReferenceIDs: [String] = []
    @State private var referenceQuery = ""
    @State private var referencesExpanded = false
    @State private var retentionMaxRecords = ""    // schedule execution-history retention
    @State private var retentionMaxDays = ""

    var body: some View {
        NavigationView {
            Form {
                if let error = vm.actionErrorMessage {
                    ThemedWorkTaskSection { Text(error).foregroundColor(theme.dangerColor).accessibilityIdentifier("task-create-error") }
                }
                ThemedWorkTaskSection("Task") {
                    TextField("Title", text: $title)
                    TextField("Description", text: $taskDescription, axis: .vertical)
                        .lineLimit(2...5)
                }
                ThemedWorkTaskSection {
                    DisclosureGroup("Reference completed work (\(selectedReferenceIDs.count))", isExpanded: $referencesExpanded) {
                        ForEach(selectedReferenceIDs, id: \.self) { id in Button("Remove reference: " + (vm.completedReferences.first(where: { $0.id == id })?.title ?? id)) { selectedReferenceIDs.removeAll { $0 == id } }.frame(minHeight: 44) }
                        Text("Selected tasks supply their context and outputs. You can still add other dependencies below.").foregroundColor(theme.secondaryTextColor)
                        TextField("Search loaded completed tasks", text: $referenceQuery)
                        ForEach(vm.completedReferences.filter { referenceQuery.isEmpty || ($0.title + " " + $0.id).localizedCaseInsensitiveContains(referenceQuery) }) { task in
                            Button {
                                if selectedReferenceIDs.contains(task.id) { selectedReferenceIDs.removeAll { $0 == task.id } }
                                else { selectedReferenceIDs.append(task.id) }
                            } label: {
                                HStack { VStack(alignment: .leading) { Text(task.title); Text(task.id).font(.caption).foregroundColor(theme.secondaryTextColor) }; Spacer(); Image(systemName: selectedReferenceIDs.contains(task.id) ? "checkmark.circle.fill" : "circle") }
                            }.buttonStyle(.plain).frame(minHeight: 44)
                        }
                        if vm.referencesLoading { ProgressView("Loading completed work") }
                        else if let error = vm.referencesError { Text(error).foregroundColor(theme.dangerColor); Button("Retry references") { vm.loadReferences() } }
                        else if vm.completedReferences.isEmpty { Text("No completed tasks available.").foregroundColor(theme.secondaryTextColor) }
                        if vm.referencesHasMore { Button("Load more completed tasks") { vm.loadReferences(append: true) }.disabled(vm.referencesLoading) }
                    }.onChange(of: referencesExpanded) { _, expanded in if expanded { vm.loadReferences() } }
                }
                ThemedWorkTaskSection("Assignment") {
                    Picker("Agent", selection: $agentId) {
                        Text("Choose personal crew").tag("")
                        ForEach(vm.agents) { Text($0.name).tag($0.id) }
                    }
                    if vm.agentsLoading { ProgressView("Loading crew") }
                    else if let error = vm.agentsError { Text(error).foregroundColor(theme.dangerColor); Button("Retry crew", action: vm.reloadAgents) }
                    else if vm.agents.isEmpty { Text("Add or enable a Personal member in Work’s Crew view.").foregroundColor(theme.secondaryTextColor) }
                    TextField("Thread", text: $threadId)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .disabled(lockedThreadID != nil)
                }
                ThemedWorkTaskSection("Details") {
                    Picker("Priority", selection: $priority) {
                        Text("None").tag("")
                        Text("P1 · Urgent").tag("p1")
                        Text("P2 · High").tag("p2")
                        Text("P3 · Medium").tag("p3")
                        Text("P4 · Low").tag("p4")
                    }
                    Toggle("Set due date", isOn: $hasDueDate)
                    if hasDueDate {
                        DatePicker("Due", selection: $dueDate, displayedComponents: .date)
                    }
                    TextField("Tags (comma-separated)", text: $tagsText)
                        .autocorrectionDisabled()
                    Picker("Output mode", selection: $outputMode) {
                        Text("Accumulate").tag("accumulate")
                        Text("Overwrite").tag("overwrite")
                    }
                    TextField("Depends on (task ids, comma-separated)", text: $dependsOnText)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
                ThemedWorkTaskSection("Schedule") {
                    Picker("Repeat", selection: $repeatMode) {
                        Text("None").tag("none")
                        Text("Daily (9am)").tag("daily")
                        Text("Weekdays (9am)").tag("weekdays")
                        Text("Weekly (Mon 9am)").tag("weekly")
                        Text("Custom cron").tag("custom")
                    }
                    if repeatMode == "custom" { TextField("Cron · five fields", text: $customCron).autocorrectionDisabled().textInputAutocapitalization(.never) }
                    if repeatMode != "none" {
                        TextField("Timezone", text: $scheduleTimezone).autocorrectionDisabled().textInputAutocapitalization(.never)
                        if invalidSchedule { Text("Use a five-field cron and a valid timezone such as Asia/Kolkata.").foregroundColor(theme.dangerColor) }
                        // Execution-history retention for the repeating schedule (web parity).
                        TextField("Keep last N runs (optional)", text: $retentionMaxRecords)
                            .keyboardType(.numberPad)
                        TextField("Keep runs for N days (optional)", text: $retentionMaxDays)
                            .keyboardType(.numberPad)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(theme.backgroundColor)
            .foregroundColor(theme.textColor)
            .font(.themed(15))
            .tint(theme.accentColor)
            .navigationTitle("New Task")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { create() }
                        .disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || vm.mutatingTaskID != nil || invalidSchedule || vm.agentsLoading || !vm.agents.contains(where: { $0.id == agentId }))
                }
            }
            .onAppear { vm.actionErrorMessage = nil; if let lockedThreadID { threadId = lockedThreadID }; vm.reloadAgents(); if !vm.agentsLoading { chooseAvailableOwner() } }
            .onChange(of: vm.agentsLoading) { _, loading in if !loading { chooseAvailableOwner() } }
            .onChange(of: vm.agents) { _, _ in if !vm.agentsLoading { chooseAvailableOwner() } }
        }
    }

    private func create() {
        guard !vm.agentsLoading, vm.agents.contains(where: { $0.id == agentId }) else { return }
        let tags = tagsText.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let deps = dependsOnText.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let due: String? = hasDueDate ? isoString(dueDate) : nil
        vm.createTask(
            title: title.trimmingCharacters(in: .whitespaces),
            description: TaskReferenceComposer.description(taskDescription, ids: selectedReferenceIDs),
            agentId: agentId,
            threadId: lockedThreadID ?? threadId,
            priority: priority.isEmpty ? nil : priority,
            dueDate: due,
            tagNames: tags,
            outputMode: outputMode,
            dependsOn: TaskReferenceComposer.dependencies(deps, references: selectedReferenceIDs),
            schedule: scheduleDict()
        ) { dismiss() }
    }

    /// Map the Repeat preset to the cron schedule shape the web sends
    /// (`{ kind: { Cron: { expression, timezone } }, timezone }`), plus optional
    /// execution-history retention (`execution_history_retention.{max_records,max_age_days}`).
    private func chooseAvailableOwner() {
        if !vm.agents.contains(where: { $0.id == agentId }) { agentId = vm.agents.first(where: \.isPrimary)?.id ?? vm.agents.first?.id ?? "" }
    }
    private func scheduleDict() -> [String: Any]? {
        let cron: String
        switch repeatMode {
        case "daily": cron = "0 9 * * *"
        case "weekdays": cron = "0 9 * * 1-5"
        case "weekly": cron = "0 9 * * 1"
        case "custom": cron = customCron.trimmingCharacters(in: .whitespacesAndNewlines)
        default: return nil
        }
        let tz = scheduleTimezone.trimmingCharacters(in: .whitespacesAndNewlines)
        var schedule: [String: Any] = ["kind": ["Cron": ["expression": cron, "timezone": tz]], "timezone": tz]
        var retention: [String: Any] = [:]
        if let n = Int(retentionMaxRecords.trimmingCharacters(in: .whitespaces)), n > 0 { retention["max_records"] = n }
        if let d = Int(retentionMaxDays.trimmingCharacters(in: .whitespaces)), d > 0 { retention["max_age_days"] = d }
        if !retention.isEmpty { schedule["execution_history_retention"] = retention }
        return schedule
    }

    private func isoString(_ d: Date) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: d)
    }
}

/// Native Form rows use the installed palette, including optional editors.
struct ThemedWorkTaskSection<Content: View>: View {
    private let title: String?
    private let content: Content
    @ObservedObject private var theme = ThemeManager.shared
    init(_ title: String? = nil, @ViewBuilder content: () -> Content) { self.title = title; self.content = content() }
    @ViewBuilder var body: some View {
        if let title { Section(title) { content }.listRowBackground(theme.cardColor) }
        else { Section { content }.listRowBackground(theme.cardColor) }
    }
}

struct TaskThreadPicker: View {
    @Binding var selected: String
    @Binding var choices: [TaskThreadChoice]
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkResourceStatus(resource: resource)
        Picker("Destination thread", selection: $selected) {
            Text("Choose a loaded thread").tag("")
            ForEach(choices) { Text($0.name.isEmpty ? $0.id : $0.name).tag($0.id) }
        }
        Button("Refresh threads") { resource.refresh() }
        if !resource.loading && resource.error == nil && choices.isEmpty { Text("No active threads available.").foregroundColor(theme.secondaryTextColor) }
        Text("The destination is checked again before the move.").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            .onAppear { resource.load("/api/magician/v2/ui-threads") }
            .onChange(of: resource.value) { _, value in
                choices = (value?["threads"].array ?? []).compactMap { row -> TaskThreadChoice? in
                    guard let object = row.object else { return nil }
                    let values = object.mapValues { value -> Any in switch value { case .string(let value): return value; case .bool(let value): return value; default: return NSNull() } }
                    return TaskThreadChoice(values)
                }.filter { !$0.archived }
                if !choices.contains(where: { $0.id == selected }) { selected = "" }
            }
    }
}
