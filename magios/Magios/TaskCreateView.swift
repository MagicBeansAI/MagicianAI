import SwiftUI

/// New-task form (POST /v3/tasks). Mirrors the web create form's fields: title,
/// description, agent (required), thread, priority, due date, tags.
struct TaskCreateView: View {
    @ObservedObject var vm: TasksViewModel
    @Environment(\.dismiss) private var dismiss

    @State private var title = ""
    @State private var taskDescription = ""
    @State private var agentId = "personal-assistant"
    @State private var threadId = "general"
    @State private var priority = ""          // "" = none
    @State private var hasDueDate = false
    @State private var dueDate = Date()
    @State private var tagsText = ""
    @State private var repeatMode = "none"    // none | daily | weekdays | weekly
    @State private var outputMode = "accumulate"   // accumulate | overwrite
    @State private var dependsOnText = ""     // comma-separated task ids
    @State private var retentionMaxRecords = ""    // schedule execution-history retention
    @State private var retentionMaxDays = ""

    var body: some View {
        NavigationView {
            Form {
                Section("Task") {
                    TextField("Title", text: $title)
                    TextField("Description", text: $taskDescription, axis: .vertical)
                        .lineLimit(2...5)
                }
                Section("Assignment") {
                    Picker("Agent", selection: $agentId) {
                        if !vm.agents.contains(where: { $0.id == agentId }) {
                            Text(agentId).tag(agentId)
                        }
                        ForEach(vm.agents) { Text($0.name).tag($0.id) }
                    }
                    TextField("Thread", text: $threadId)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }
                Section("Details") {
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
                Section("Schedule") {
                    Picker("Repeat", selection: $repeatMode) {
                        Text("None").tag("none")
                        Text("Daily (9am)").tag("daily")
                        Text("Weekdays (9am)").tag("weekdays")
                        Text("Weekly (Mon 9am)").tag("weekly")
                    }
                    if repeatMode != "none" {
                        // Execution-history retention for the repeating schedule (web parity).
                        TextField("Keep last N runs (optional)", text: $retentionMaxRecords)
                            .keyboardType(.numberPad)
                        TextField("Keep runs for N days (optional)", text: $retentionMaxDays)
                            .keyboardType(.numberPad)
                    }
                }
            }
            .navigationTitle("New Task")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { create() }
                        .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }

    private func create() {
        let tags = tagsText.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let deps = dependsOnText.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        let due: String? = hasDueDate ? isoString(dueDate) : nil
        vm.createTask(
            title: title.trimmingCharacters(in: .whitespaces),
            description: taskDescription,
            agentId: agentId,
            threadId: threadId,
            priority: priority.isEmpty ? nil : priority,
            dueDate: due,
            tagNames: tags,
            outputMode: outputMode,
            dependsOn: deps,
            schedule: scheduleDict()
        ) { dismiss() }
    }

    /// Map the Repeat preset to the cron schedule shape the web sends
    /// (`{ kind: { Cron: { expression, timezone } }, timezone }`), plus optional
    /// execution-history retention (`execution_history_retention.{max_records,max_age_days}`).
    private func scheduleDict() -> [String: Any]? {
        let cron: String
        switch repeatMode {
        case "daily": cron = "0 9 * * *"
        case "weekdays": cron = "0 9 * * 1-5"
        case "weekly": cron = "0 9 * * 1"
        default: return nil
        }
        let tz = TimeZone.current.identifier
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
