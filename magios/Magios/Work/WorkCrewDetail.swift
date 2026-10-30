import SwiftUI

private struct WorkCrewEditingSession: Identifiable {
    let id = UUID()
    let etag: String
    let scopeID: UUID
}

struct WorkCrewDetail: View {
    let agentID: String
    var openEditor = false
    @State private var autoOpened = false
    @StateObject private var resource = WorkResource()
    @State private var editor = ""
    @State private var editing: WorkCrewEditingSession?
    @State private var editingHelp = false
    @State private var guided = true
    @State private var draft: WorkJSON = .object([:])
    @State private var confirmAction: WorkAPIClient.AgentAction?
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.scenePhase) private var scenePhase
    private var path: String { "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))" }
    private var member: WorkCrewMember? { resource.value.flatMap { try? WorkCrewMember($0) } }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: WorkMetrics.sectionSpacing) {
                WorkResourceStatus(resource: resource)
                if let member {
                    header(member)
                    WorkGroupCard("Health and runtime") {
                        if let taskID = member.cycleTaskID {
                            WorkNavigationRow(title: "Inspect current cycle", systemImage: "arrow.triangle.2.circlepath") {
                                TaskDetailScreen(route: TaskDetailRoute(taskId: taskID, executionId: member.executionID, inspectRun: true, titleHint: member.name + " cycle"))
                            }.accessibilityIdentifier("work-crew-current-cycle")
                        } else { WorkInfoRow(title: "No current cycle execution.", systemImage: "arrow.triangle.2.circlepath") }
                        WorkRowDivider()
                        endpoint("Member health", suffix: "/health", systemImage: "heart.text.square")
                        WorkRowDivider()
                        WorkNavigationRow(title: "Operator view", systemImage: "gauge.with.dots.needle.33percent") { WorkOperatorView(agentID: agentID) }
                        if member.kind.lowercased() == "personal" {
                            WorkRowDivider()
                            WorkNavigationRow(title: "Bootstrap task", systemImage: "flag") { WorkCrewBootstrap(agentID: agentID) }
                        }
                    }
                    WorkGroupCard("Memory") {
                        WorkNavigationRow(title: "Memory tiers", systemImage: "square.stack.3d.up") { WorkCrewMemoryView(agentID: agentID) }
                        WorkRowDivider()
                        endpoint("Memory health", suffix: "/memory/consolidation-health", systemImage: "waveform.path.ecg")
                        WorkRowDivider()
                        WorkNavigationRow(title: "User memory and sharing", systemImage: "person.2") { WorkCrewUserMemoryView(agentID: agentID) }
                    }
                    WorkGroupCard("Rules and tools") {
                        WorkNavigationRow(title: "Rules and policies", systemImage: "checklist") { WorkCrewRulesView(agentID: agentID) }
                        WorkRowDivider()
                        endpoint("Effective tools", suffix: "/effective-tools", systemImage: "wrench.and.screwdriver")
                        WorkRowDivider()
                        endpoint("Runtime tool cache", suffix: "/runtime-context-cache", systemImage: "internaldrive")
                    }
                    WorkGroupCard("Playbooks and quality") {
                        WorkNavigationRow(title: "Playbooks", systemImage: "doc.text") { WorkProgramsList().navigationTitle("Playbooks") }
                        WorkRowDivider()
                        WorkNavigationRow(title: "Quality checks", systemImage: "checkmark.seal") { WorkQualityView(agentID: agentID).navigationTitle("Quality checks") }
                    }
                    WorkGroupCard("Definition") {
                        DisclosureGroup("Definition, rules and model pins") {
                            VStack(alignment: .leading, spacing: 0) {
                                WorkRowDivider()
                                Button {
                                    draft = .object(member.definition); editor = draft.formatted; guided = true
                                    editing = WorkCrewEditingSession(etag: member.etag, scopeID: resource.scopeID)
                                } label: {
                                    WorkRowLabel(title: "Edit definition", systemImage: "pencil", accessory: .none, tint: theme.accentColor)
                                }.buttonStyle(.plain)
                                WorkRowDivider()
                                WorkRecordView(value: .object(member.definition)).padding(.top, 12)
                            }
                        }.disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                    }
                    WorkGroupCard("Web console") {
                        WorkRelatedTools(origin: WorkWebContext(section: .crew, agent: agentID), liveSurface: true)
                    }
                }
            }.padding(WorkMetrics.screenPadding)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor)
        .navigationTitle(member?.name ?? "Crew member").navigationBarTitleDisplayMode(.inline)
        .task(id: agentID) {
            resource.load(path)
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                if scenePhase == .active { resource.refresh() }
            }
        }.refreshable { resource.refresh() }
        .onChange(of: resource.value) { _, _ in
            if openEditor, !autoOpened, let member { draft = .object(member.definition); editor = draft.formatted; guided = true; editing = WorkCrewEditingSession(etag: member.etag, scopeID: resource.scopeID); autoOpened = true }
        }
        .onChange(of: resource.scopeID) { _, _ in confirmAction = nil; editing = nil; editor = ""; editingHelp = false }
        .toolbar { Button("Refresh member", systemImage: "arrow.clockwise") { resource.refresh() } }
        .confirmationDialog("Change this crew member?", isPresented: Binding(get: { confirmAction != nil }, set: { if !$0 { confirmAction = nil } }), titleVisibility: .visible) {
            if let action = confirmAction {
                Button(actionTitle(action)) {
                    confirmAction = nil
                    Task {
                        if await resource.perform(path + "/" + action.rawValue, body: action == .trigger ? .object([:]) : nil) != nil { resource.refresh() }
                    }
                }
            }
            Button("Cancel", role: .cancel) { confirmAction = nil }
        } message: { Text("This applies to \(member?.name ?? agentID) in the current connection.") }
        .sheet(item: $editing) { session in
            NavigationStack {
                VStack(alignment: .leading, spacing: 12) {
                    Button("Editing help", systemImage: "info.circle") { editingHelp = true }.font(.themed(13)).frame(minHeight: 44)
                    Picker("Editor", selection: Binding(get: { guided }, set: { next in
                        if next { do { draft = try WorkJSON.parseObject(editor); guided = true } catch { resource.error = "Enter a valid definition before returning to guided editing." } }
                        else { editor = draft.formatted; guided = false }
                    })) { Text("Guided").tag(true); Text("Advanced JSON").tag(false) }.pickerStyle(.segmented)
                    if guided {
                        ScrollView { WorkCrewFields(value: $draft, editingID: agentID) }
                    } else {
                        TextEditor(text: $editor).font(.themed(13)).scrollContentBackground(.hidden)
                            .accessibilityIdentifier("work-crew-definition-editor")
                            .padding(8).workCardSurface()
                    }
                    WorkResourceStatus(resource: resource)
                }.padding(16).background(theme.backgroundColor).foregroundColor(theme.textColor)
                .navigationTitle("Edit crew")
                .navigationBarTitleDisplayMode(.inline)
                .alert("Editing definition", isPresented: $editingHelp) {
                    Button("OK", role: .cancel) {}
                } message: {
                    Text("All fields are retained. Save checks the version you opened; a concurrent edit requires a refresh. Set a field to null to remove it.")
                }
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { editing = nil }.disabled(resource.busy) }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") { Task { await save(session) } }.disabled(resource.busy || session.etag.isEmpty)
                    }
                }
            }.tint(theme.accentColor)
        }
    }
    private func endpoint(_ title: String, suffix: String, systemImage: String) -> some View {
        WorkNavigationRow(title: title, systemImage: systemImage) { WorkEndpointView(title: title, path: path + suffix) }
    }
    private func kindLabel(_ kind: String) -> String {
        switch kind.lowercased() { case "personal": return "Personal"; case "worker": return "Worker"; default: return kind }
    }
    private func statusTone(_ member: WorkCrewMember) -> Color {
        if member.disabled { return theme.secondaryTextColor }
        switch member.status.lowercased() {
        case "active", "running", "triggered", "idle": return theme.successColor
        case "error", "failed", "partial": return theme.dangerColor
        case "paused": return theme.warningColor
        default: return theme.secondaryTextColor
        }
    }
    /// Name, kind and state, then the member actions as the card's one row.
    private func header(_ member: WorkCrewMember) -> some View {
        let chips = Group {
            WorkChip(title: kindLabel(member.kind), tone: theme.secondaryTextColor)
            WorkChip(title: member.disabled ? "Disabled" : member.status, tone: statusTone(member))
            if member.isPrimary { WorkChip(title: "Primary", systemImage: "star.fill", tone: theme.accentColor) }
        }
        return VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                Text(member.name).font(.themed(22, weight: .semibold)).fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
                if !member.description.isEmpty {
                    Text(member.description).font(.themed(15)).foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { chips }
                    VStack(alignment: .leading, spacing: 6) { chips }
                }
            }
            .padding(.vertical, WorkMetrics.cardInset)
            if member.kind.lowercased() == "personal" {
                WorkRowDivider()
                Menu {
                    Button("Run once") { confirmAction = .trigger }
                    Button("Pause cycles") { confirmAction = .pause }
                    Button("Resume cycles") { confirmAction = .resume }
                    if !member.isPrimary { Button("Make primary") { confirmAction = .setPrimary } }
                } label: {
                    WorkRowLabel(title: "Actions", systemImage: "bolt.circle", accessory: .menu)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Actions")
                .disabled(resource.busy)
                .opacity(resource.busy ? 0.5 : 1)
            }
        }
        .padding(.horizontal, WorkMetrics.cardInset)
        .frame(maxWidth: .infinity, alignment: .leading)
        .workCardSurface()
    }
    private func actionTitle(_ action: WorkAPIClient.AgentAction) -> String {
        switch action { case .pause: return "Pause cycles"; case .resume: return "Resume cycles"; case .trigger: return "Run once"; case .setPrimary: return "Make primary" }
    }
    private func save(_ session: WorkCrewEditingSession) async {
        do {
            guard let member, editing?.id == session.id, resource.scopeID == session.scopeID, !session.etag.isEmpty else { return }
            let patch = guided ? draft : try WorkJSON.parseObject(editor)
            if let message = patch.crewValidation(editingID: agentID) { resource.error = message; return }
            guard (patch["agent_id"].text ?? patch["id"].text) == agentID else { resource.error = "Keep the crew member identity unchanged."; return }
            if await resource.perform(path, method: "PATCH", body: patch.mergePatch(from: .object(member.definition)), etag: session.etag) != nil {
                editing = nil; resource.refresh()
            }
        } catch { resource.error = "Enter a valid definition object before saving." }
    }
}

struct WorkCrewBootstrap: View {
    let agentID: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private var taskID: String { "bootstrap__" + agentID }
    var body: some View {
        ScrollView { VStack(alignment: .leading, spacing: WorkMetrics.sectionSpacing) {
            WorkResourceStatus(resource: resource)
            if let value = resource.value {
                WorkHeaderCard(value["title"].text ?? "Bootstrap task", subtitle: taskID,
                               chips: value["status"].text.map { [WorkChipSpec(title: WorkRecordView.label($0), tone: .workStatusTone($0))] } ?? []) {
                    WorkNavigationRow(title: "Open task and execution details", systemImage: "arrow.right.circle") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }
                }
                WorkGroupCard("Task record") { WorkRecordView(value: value) }
            }
        }.padding(WorkMetrics.screenPadding) }.background(theme.backgroundColor).foregroundColor(theme.textColor)
            .navigationTitle("Bootstrap").navigationBarTitleDisplayMode(.inline)
            .task(id: agentID) { resource.load("/api/magician/v3/tasks/" + WorkAPIClient.query(taskID)) }
            .refreshable { resource.refresh() }
    }
}

/// Guided fields remain on-device; the advanced document is the same draft.
struct WorkCrewFields: View {
    @Binding var value: WorkJSON
    var editingID: String? = nil
    /// Program documents a focus area can choose as shared documents.
    @StateObject private var programs = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private func text(_ label: String, _ path: [String], multiline: Bool = false, list: Bool = false) -> some View {
        WorkFieldRow(label) {
            TextField(label, text: Binding(get: {
                let item = value.at(path)
                return list ? (item.array ?? []).compactMap(\.text).joined(separator: ", ") : item.text ?? ""
            }, set: { input in
                value = value.setting(path, to: list ? .array(input.split(separator: ",").map { .string($0.trimmingCharacters(in: .whitespacesAndNewlines)) }.filter { !$0.display.isEmpty }) : .string(input))
            }), axis: multiline ? .vertical : .horizontal)
            .font(.themed(15)).lineLimit(multiline ? 3...8 : 1...1)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .workFieldSurface()
            .disabled(path == ["agent_id"] && editingID != nil)
            .accessibilityIdentifier("work-crew-field-" + path.joined(separator: "-"))
        }
    }
    private func choice(_ label: String, _ path: [String], _ options: [String], defaultValue: String, labels: [String: String] = [:]) -> some View {
        // The server stores enum values lowercase (`personal`); a stored value
        // in another case still selects its option.
        WorkFormRow(label) {
            Picker(label, selection: Binding(get: { let current = value.at(path).text ?? defaultValue; return options.first { $0.caseInsensitiveCompare(current) == .orderedSame } ?? current }, set: { value = value.setting(path, to: .string($0))
                if path == ["kind"] && $0 == "worker" { for key in ["harness", "autonomous_config", "user_memory_isolation", "readable_agents"] { value = value.setting([key], to: nil) } }
            })) {
                ForEach(options, id: \.self) { Text(labels[$0] ?? Self.sentence($0)).tag($0) }
            }.labelsHidden().pickerStyle(.menu).font(.themed(15))
        }
    }
    private func number(_ label: String, _ path: [String], _ range: ClosedRange<Int>, defaultValue: Int) -> some View {
        let current = Int(value.at(path).display) ?? defaultValue
        return Stepper(value: Binding(get: { current }, set: { value = value.setting(path, to: .integer(Int64($0))) }), in: range) {
            HStack(spacing: 8) {
                Text(label).font(.themed(16)).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Text("\(current)").font(.themed(16, weight: .semibold)).monospacedDigit().foregroundColor(theme.secondaryTextColor)
            }
        }
        .padding(.vertical, 6).frame(minHeight: WorkMetrics.rowMinHeight)
    }
    private func optional(_ label: String, _ key: String, initial: WorkJSON) -> some View {
        Toggle(isOn: Binding(get: { value[key].object != nil }, set: { enabled in value = value.setting([key], to: enabled ? initial : nil) })) {
            Text(label).font(.themed(16)).fixedSize(horizontal: false, vertical: true)
        }
        .tint(theme.accentFillColor)
        .padding(.vertical, 6).frame(minHeight: WorkMetrics.rowMinHeight)
    }
    /// `fully_isolated` reads "Fully isolated"; the stored value is unchanged.
    static func sentence(_ raw: String) -> String {
        let words = raw.replacingOccurrences(of: "_", with: " ")
        return words.prefix(1).uppercased() + words.dropFirst()
    }
    private var focusAreas: [WorkJSON] { value.at(["autonomous_config", "focus_areas"]).array ?? [] }
    var body: some View {
        VStack(alignment: .leading, spacing: WorkMetrics.sectionSpacing) {
            WorkGroupCard("Identity") {
                text("Member ID (optional on creation)", ["agent_id"])
                text("Name", ["name"])
                text("Description", ["description"], multiline: true)
                text("Persona", ["persona"], multiline: true)
                WorkRowDivider()
                choice("Kind", ["kind"], ["worker", "personal"], defaultValue: "worker", labels: ["worker": "Worker", "personal": "Personal"])
                WorkRowDivider()
                choice("Trust", ["trust_level"], ["local", "reviewed", "builtin", "untrusted"], defaultValue: "local", labels: ["builtin": "Built-in"])
            }
            if value.isPersonalCrew {
                WorkGroupCard("Playbooks and cycles") {
                    optional("Use Autopilot", "harness", initial: .object([:]))
                    if value["harness"].object != nil { text("Playbook section", ["harness", "program_section"]) }
                    WorkRowDivider()
                    optional("Scheduled check-ins", "autonomous_config", initial: .object(["schedule": .string("0 */4 * * *"), "focus_areas": .array([]), "max_tasks_per_cycle": .integer(3), "max_steps_per_plan": .integer(10)]))
                    if value["autonomous_config"].object != nil {
                        text("Cycle schedule (cron)", ["autonomous_config", "schedule"])
                        WorkRowDivider()
                        number("Tasks per cycle", ["autonomous_config", "max_tasks_per_cycle"], 1...10, defaultValue: 3)
                        WorkRowDivider()
                        number("Steps per plan", ["autonomous_config", "max_steps_per_plan"], 1...30, defaultValue: 10)
                    }
                }
                if value["autonomous_config"].object != nil {
                    WorkGroupCard("Focus areas") {
                        ForEach(Array(focusAreas.enumerated()), id: \.offset) { index, _ in
                            WorkFocusFields(value: Binding(get: { { let areas = value.at(["autonomous_config", "focus_areas"]).array ?? []; return areas.indices.contains(index) ? areas[index] : .object([:]) }() }, set: { area in
                                var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                                guard areas.indices.contains(index) else { return }; areas[index] = area
                                value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                            }), programs: programs, remove: {
                                var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                                guard areas.indices.contains(index) else { return }; areas.remove(at: index)
                                value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                            })
                            WorkRowDivider()
                        }
                        Button {
                            var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                            areas.append(.object(["name": .string(""), "description": .string(""), "priority": .string("medium")]))
                            value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                        } label: {
                            WorkRowLabel(title: "Add focus area", systemImage: "plus.circle.fill", accessory: .none, tint: theme.accentColor)
                        }.buttonStyle(.plain)
                    }
                }
            }
            WorkGroupCard("Delegation, memory and models") {
                DisclosureGroup("Tools and delegation") {
                    VStack(alignment: .leading, spacing: 0) {
                        text("Tools (comma-separated)", ["tools"], list: true)
                        text("Excluded tools (comma-separated)", ["excluded_tools"], list: true)
                        text("Delegate to (IDs or *)", ["delegation_targets"], list: true)
                        WorkRowDivider()
                        number("Delegation depth", ["constraints", "coordination", "max_delegation_depth"], 1...10, defaultValue: 3)
                    }
                }.disclosureGroupStyle(WorkDisclosureStyle(systemImage: "person.line.dotted.person"))
                if value.isPersonalCrew {
                    WorkRowDivider()
                    DisclosureGroup("Memory boundaries") {
                        VStack(alignment: .leading, spacing: 0) {
                            choice("User memory", ["user_memory_isolation"], ["shared", "fully_isolated"], defaultValue: "shared")
                            text("Readable members (comma-separated)", ["readable_agents"], list: true)
                        }
                    }.disclosureGroupStyle(WorkDisclosureStyle(systemImage: "lock.shield"))
                }
                WorkRowDivider()
                WorkCrewModelFields(value: $value)
            }
        }
        .task(id: value.isPersonalCrew && value["autonomous_config"].object != nil) {
            if value.isPersonalCrew && value["autonomous_config"].object != nil && programs.path.isEmpty { programs.load("/api/magician/v2/programs") }
        }
        .foregroundColor(theme.textColor).tint(theme.accentColor)
    }
}

private struct WorkFocusFields: View {
    @Binding var value: WorkJSON
    @ObservedObject var programs: WorkResource
    let remove: () -> Void
    /// "Choose documents" with nothing checked stores `[]`, like "None";
    /// this keeps the picker on Choose until another mode is picked.
    @State private var choosing = false
    /// Checked names kept while another mode is picked, so switching back
    /// to Choose restores them.
    @State private var remembered: [String] = []
    /// Names stored when the editor opened; one that is not a program
    /// document stays listed (as not found) after it is unchecked.
    @State private var retained: [String]?
    @ObservedObject private var theme = ThemeManager.shared
    private func field(_ label: String, _ key: String) -> some View {
        WorkFieldRow(label) {
            TextField(label, text: Binding(get: { value[key].text ?? "" }, set: { value = value.setting([key], to: .string($0)) }), axis: .vertical)
                .font(.themed(15)).textInputAutocapitalization(.never).workFieldSurface().accessibilityLabel(label)
        }
    }
    private var priority: String { value["priority"].text ?? "medium" }
    private var sharedMode: WorkSharedProgramsMode { value.sharedProgramsMode(choosing: choosing) }
    private var documents: [WorkProgramSummary] {
        (programs.value?["programs"].array ?? []).compactMap { row in
            guard let name = row["name"].text, !name.isEmpty else { return nil }
            let title = row["title"].text ?? ""
            return WorkProgramSummary(name: name, title: title.isEmpty ? name : title)
        }
    }
    private var sharedDocuments: some View {
        VStack(alignment: .leading, spacing: 0) {
            WorkFormRow("Shared documents", exposesTitle: true) {
                Picker("Shared documents", selection: Binding(get: { sharedMode }, set: { next in
                    if sharedMode == .choose { remembered = value.sharedPrograms ?? [] }
                    choosing = next == .choose
                    value = value.settingSharedPrograms(next, selected: next == .choose ? remembered : [])
                })) {
                    ForEach(WorkSharedProgramsMode.allCases) { Text($0.title).tag($0) }
                }
                .labelsHidden().pickerStyle(.menu).font(.themed(15))
                .accessibilityIdentifier("work-focus-shared-mode")
            }
            Text(sharedMode.hint).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true).padding(.bottom, 8)
            if sharedMode == .choose {
                let stored = value.sharedPrograms ?? []
                let known = Set(documents.map(\.name))
                let others = ((retained ?? []) + stored).reduce(into: [String]()) { names, name in
                    if !known.contains(name) && !names.contains(name) { names.append(name) }
                }
                WorkResourceStatus(resource: programs)
                if programs.value != nil && documents.isEmpty && others.isEmpty {
                    Text("No playbook documents in this workspace.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        .padding(.vertical, 8)
                }
                ForEach(documents) { document in
                    WorkRowDivider()
                    sharedRow(name: document.name, title: document.title, selected: stored.contains(document.name), missing: false)
                }
                ForEach(others, id: \.self) { name in
                    WorkRowDivider()
                    sharedRow(name: name, title: name, selected: stored.contains(name), missing: programs.value != nil)
                }
            }
        }
        .onAppear { if retained == nil { retained = value.sharedPrograms ?? [] } }
    }
    private func sharedRow(name: String, title: String, selected: Bool, missing: Bool) -> some View {
        Button {
            choosing = true
            value = value.togglingSharedProgram(name, on: !selected)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Image(systemName: selected ? "checkmark.square.fill" : "square").font(.themed(18))
                    .foregroundColor(selected ? theme.accentColor : theme.secondaryTextColor)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.themed(15, weight: .medium)).foregroundColor(theme.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                    if missing { Text(name + " · not found").font(.themed(12)).foregroundColor(theme.warningColor) }
                    else if title != name { Text(name).font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, WorkMetrics.rowVerticalPadding)
            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityHint(missing ? "Not a playbook document in this workspace. Uncheck to remove it." : "")
        .accessibilityIdentifier("work-focus-shared-" + name)
    }
    var body: some View {
        DisclosureGroup(value["name"].text?.isEmpty == false ? value["name"].display : "New focus area") {
            VStack(alignment: .leading, spacing: 0) {
                field("Focus name", "name"); field("Focus description", "description")
                WorkRowDivider()
                WorkFormRow("Priority") {
                    Picker("Priority", selection: Binding(get: { priority }, set: { value = value.setting(["priority"], to: .string($0)) })) {
                        ForEach(["high", "medium", "low"], id: \.self) { Text(WorkCrewFields.sentence($0)).tag($0) }
                    }.labelsHidden().pickerStyle(.menu).font(.themed(15))
                }
                WorkRowDivider()
                field("Schedule override (cron)", "schedule"); field("Playbook document", "program")
                WorkFieldRow("Scope (comma-separated)") {
                    TextField("Scope (comma-separated)", text: Binding(get: { (value["scope"].array ?? []).compactMap(\.text).joined(separator: ", ") }, set: { input in value = value.setting(["scope"], to: .array(input.split(separator: ",").map { .string($0.trimmingCharacters(in: .whitespacesAndNewlines)) })) }))
                        .font(.themed(15)).workFieldSurface()
                }
                WorkRowDivider()
                sharedDocuments
                WorkRowDivider()
                Button(role: .destructive, action: remove) {
                    WorkRowLabel(title: "Remove focus area", systemImage: "trash", accessory: .none, tint: theme.dangerColor)
                }.buttonStyle(.plain)
            }
            .workInsetPanel()
        }
        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "scope", value: WorkCrewFields.sentence(priority)))
    }
}

struct WorkCrewCreateView: View {
    let onSaved: () -> Void
    @Environment(\.dismiss) private var dismiss
    @StateObject private var resource = WorkResource()
    @State private var draft = WorkJSON.newCrewDefinition
    @State private var advanced = false
    @State private var editor = ""
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        NavigationStack {
            VStack(spacing: 12) {
                Picker("Editor", selection: Binding(get: { advanced }, set: { next in
                    if next { editor = draft.formatted; advanced = true }
                    else { do { draft = try WorkJSON.parseObject(editor); advanced = false } catch { resource.error = "Enter a valid definition before returning to guided editing." } }
                })) { Text("Guided").tag(false); Text("Advanced JSON").tag(true) }.pickerStyle(.segmented)
                if advanced {
                    TextEditor(text: $editor).font(.themed(13)).scrollContentBackground(.hidden).padding(8).workCardSurface()
                } else { ScrollView { WorkCrewFields(value: $draft) } }
                WorkResourceStatus(resource: resource)
            }.padding(16).foregroundColor(theme.textColor).background(theme.backgroundColor)
            .navigationTitle("Add member").navigationBarTitleDisplayMode(.inline)
            .task { resource.load("/api/magician/v2/agents") }
            .onChange(of: resource.scopeID) { _, _ in draft = .newCrewDefinition; editor = ""; advanced = false }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(resource.busy) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { Task { await create() } }.disabled(resource.busy || resource.value == nil)
                }
            }
        }.tint(theme.accentColor)
    }
    private func create() async {
        do {
            let definition = advanced ? try WorkJSON.parseObject(editor) : draft
            if let message = definition.crewValidation() { resource.error = message; return }
            if await resource.perform("/api/magician/v2/agents", body: definition) != nil { onSaved(); dismiss() }
        } catch { resource.error = "Enter a valid definition before creating a member." }
    }
}

private struct WorkCrewModelFields: View {
    @Binding var value: WorkJSON
    @StateObject private var profiles = WorkResource()
    @StateObject private var coding = WorkResource()
    @State private var catalogModels: [ChatModelChoice] = []
    @State private var codingProfiles: [VibeDevCodingProfile] = []
    @ObservedObject private var theme = ThemeManager.shared
    private let lanes = ["planning", "evaluation", "correction_extraction", "memory_consolidation"]
    private func selection(_ lane: String) -> String {
        let endpoint = value.at(["llm_routing", lane])
        if let profile = endpoint["profile"].text, !profile.isEmpty { return profile }
        return endpoint.object == nil ? "" : "direct:" + (endpoint["provider"].text ?? "") + "/" + (endpoint["model"].text ?? "")
    }
    private func updateCatalog(_ payload: WorkJSON?) {
        guard let raw = payload?["models"], let data = raw.formatted.data(using: .utf8) else { catalogModels = []; return }
        catalogModels = (try? JSONDecoder().decode([ChatModelChoice].self, from: data)) ?? []
    }
    private func modelSelection(_ lane: String) -> ChatModelSelection? {
        for model in catalogModels {
            if let level = model.choices.first(where: { $0.value == selection(lane) })?.key
                ?? model.operationChoices?.first(where: { $0.value == selection(lane) })?.key {
                return ChatModelSelection(modelId: model.id, thinking: level)
            }
        }
        guard let row = profiles.value?["profiles"].array?.first(where: { $0["name"].text == selection(lane) }),
              let data = row["model_selection"].formatted.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(ChatModelSelection.self, from: data)
    }
    private func savedModelLabel(_ route: String) -> String {
        if let row = profiles.value?["profiles"].array?.first(where: { $0["name"].text == route }) {
            return row["model"].display + " · " + row["provider"].display
        }
        return route.hasPrefix("direct:") ? String(route.dropFirst(7)) + " (direct pin)" : "Unavailable model"
    }
    private func setRoute(_ lane: String, _ route: String) {
        guard !route.hasPrefix("direct:") else { return }
        value = value.setting(["llm_routing", lane], to: route.isEmpty ? nil : .object(["profile": .string(route), "provider": .string(""), "model": .string("")]))
    }
    private func assign(_ lane: String, model: ChatModelChoice, thinking: String) {
        if let route = (model.operationChoices ?? model.choices)[thinking] { setRoute(lane, route) }
    }
    private func catalogLane(_ lane: String) -> some View {
        let selected = modelSelection(lane)
        let model = catalogModels.first(where: { $0.id == selected?.modelId })
        return WorkFieldRow(WorkRecordView.label(lane)) { VStack(alignment: .leading, spacing: 10) {
            ModelCatalogPicker(title: WorkRecordView.label(lane), selected: model?.id ?? selection(lane),
                options: [VoiceSelectionOption(id: "", label: "Follow global routing")]
                    + (model == nil && !selection(lane).isEmpty ? [VoiceSelectionOption(id: selection(lane), label: savedModelLabel(selection(lane)), available: false)] : [])
                    + catalogModels.map { VoiceSelectionOption(id: $0.id, label: $0.label, detail: $0.providerDisplayName, available: $0.installed != false) }, showsLabel: false) { id in
                if id.isEmpty { setRoute(lane, "") }
                else if let next = catalogModels.first(where: { $0.id == id }) {
                    let level = selected?.thinking ?? "auto"
                    assign(lane, model: next, thinking: next.choices[level] == nil ? "auto" : level)
                }
            }
            if let model {
                ModelThinkingControl(control: model.thinking, level: selected?.thinking ?? "auto", theme: theme) { assign(lane, model: model, thinking: $0) }
            }
        } }
    }
    private var codingModels: [ChatModelChoice] { codingProfiles.compactMap(\.modelChoice) }
    private func updateCodingCatalog(_ payload: WorkJSON?) {
        guard let data = payload?.formatted.data(using: .utf8) else { codingProfiles = []; return }
        codingProfiles = (try? JSONDecoder().decode(VibeDevModelCatalog.self, from: data))?.rows ?? []
    }
    private func setCoding(_ route: String) {
        value = value.setting(["llm_routing", "coding_profile"], to: route.isEmpty ? nil : .string(route))
    }
    private func catalogCodingPicker() -> some View {
        let saved = value.at(["llm_routing", "coding_profile"]).text ?? ""
        let selected = codingProfiles.first(where: { $0.id == saved })?.modelSelection
        let model = codingModels.first(where: { $0.id == selected?.modelId })
        let modelEngines = Set(codingProfiles.filter { $0.modelChoice != nil }.compactMap(\.engine))
        let engines = codingProfiles.filter { !modelEngines.contains($0.engine ?? "") && $0.id != "auto" }
        return WorkFieldRow("Coding model or engine") { VStack(alignment: .leading, spacing: 10) {
            ModelCatalogPicker(title: "Coding model or engine", selected: model?.id ?? saved,
                options: [VoiceSelectionOption(id: "", label: "Follow global coding")]
                    + (!saved.isEmpty && model == nil && !engines.contains(where: { $0.id == saved }) ? [VoiceSelectionOption(id: saved, label: "Unavailable coding model · saved", available: false)] : [])
                    + codingModels.map { VoiceSelectionOption(id: $0.id, label: $0.label, detail: $0.providerDisplayName) }
                    + engines.map { VoiceSelectionOption(id: $0.id, label: $0.label, available: $0.selectable) }, showsLabel: false) { id in
                if let next = codingModels.first(where: { $0.id == id }) {
                    if let route = next.choices[selected?.thinking ?? "auto"] ?? next.choices["auto"] { setCoding(route) }
                } else { setCoding(id) }
            }
            if let model {
                ModelThinkingControl(control: model.thinking, level: selected?.thinking ?? "auto", theme: theme) {
                    if let route = model.choices[$0] { setCoding(route) }
                }
            }
        } }
    }
    var body: some View {
        DisclosureGroup("Models") {
            VStack(alignment: .leading, spacing: 0) {
                Text("An unpinned member follows global routing. Direct and per-operation pins are preserved; the advanced definition can edit them.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true).padding(.bottom, 2)
                WorkResourceStatus(resource: profiles)
                ForEach(lanes, id: \.self) { lane in
                    catalogLane(lane)
                }
                WorkResourceStatus(resource: coding)
                if !codingModels.isEmpty { catalogCodingPicker() } else {
                    WorkFieldRow("Coding engine model") {
                        Picker("Coding engine model", selection: Binding(get: { value.at(["llm_routing", "coding_profile"]).text ?? "" }, set: { value = value.setting(["llm_routing", "coding_profile"], to: $0.isEmpty ? nil : .string($0)) })) {
                            Text("Default — global routing").tag("")
                            let saved = value.at(["llm_routing", "coding_profile"]).text ?? ""
                            if !saved.isEmpty { Text(saved).tag(saved) }
                        }
                        .labelsHidden().pickerStyle(.menu).font(.themed(15))
                        .frame(maxWidth: .infinity, alignment: .leading).workFieldSurface()
                    }
                }
                VStack(alignment: .leading, spacing: 0) {
                    if value.at(["llm_routing", "operations"]).object?.isEmpty == false {
                        WorkRowDivider()
                        DisclosureGroup("Per-operation pins") { WorkRecordView(value: value.at(["llm_routing", "operations"])) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "pin"))
                    }
                    WorkRowDivider()
                    Button {
                        for key in lanes + ["coding_profile"] { value = value.setting(["llm_routing", key], to: nil) }
                        if value["llm_routing"].object?.isEmpty == true { value = value.setting(["llm_routing"], to: nil) }
                    } label: {
                        WorkRowLabel(title: "Clear lane and coding pins", systemImage: "xmark.circle", accessory: .none, tint: theme.accentColor)
                    }.buttonStyle(.plain)
                }
            }
        }
        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "cpu"))
        .task { profiles.load("/api/magician/v2/llm/routing"); coding.load("/api/magician/v2/coding/models") }
            .onChange(of: profiles.value, initial: true) { _, payload in updateCatalog(payload) }
            .onChange(of: coding.value, initial: true) { _, payload in updateCodingCatalog(payload) }
    }
}

struct WorkCrewRulesView: View {
    let agentID: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private let sections = ["persona", "goals", "triggers", "constraints", "memory_tiers", "memory_consolidation", "prompt_pipeline", "circuit_breaker", "feedback_loops", "notification_rules", "retention", "llm_routing", "observation", "strategy", "state_machines", "delegation_targets", "coordination"]
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let definition = resource.value.flatMap({ try? WorkCrewMember($0) })?.definition {
                let configured = sections.filter { definition[$0] != nil && definition[$0] != .null }
                ForEach(configured, id: \.self) { key in
                    WorkGroupCard(WorkRecordView.label(key)) { WorkRecordView(value: definition[key] ?? .null) }
                }
                let missing = sections.filter { !configured.contains($0) }
                if !missing.isEmpty {
                    WorkGroupCard("Not configured") {
                        ForEach(Array(missing.enumerated()), id: \.element) { index, key in
                            if index > 0 { WorkRowDivider() }
                            WorkInfoRow(title: WorkRecordView.label(key), value: "Not configured")
                        }
                    }
                }
            }
        }
        .navigationTitle("Rules and policies").navigationBarTitleDisplayMode(.inline)
        .task(id: agentID) { resource.load("/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))") }.refreshable { resource.refresh() }
    }
}

struct WorkCrewMemoryView: View {
    let agentID: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            if let value = resource.value {
                if value["tiers"].array?.isEmpty != false { Text("No declared memory tiers.").font(.themed(14)) }
                ForEach(value["tiers"].array ?? [], id: \.formatted) { tier in
                    if let name = tier["tier_name"].text {
                        NavigationLink { WorkCrewTierView(agentID: agentID, tier: name, initialGoal: tier["goal_id"].text ?? tier["available_goal_ids"].array?.first?.text) } label: {
                            HStack(spacing: 12) {
                                VStack(alignment: .leading, spacing: 6) {
                                    Text(WorkRecordView.label(name)).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor)
                                    if let description = tier["description"].text, !description.isEmpty { Text(description).font(.themed(14)).foregroundColor(theme.textColor) }
                                    Text("\(tier["scope"].display) · \(tier["shared"].flag == true ? "Shared" : "Private") · \(tier["has_data"].flag == true ? "Has data" : "Empty")").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                }
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right").font(.themed(13, weight: .semibold))
                                    .foregroundColor(theme.secondaryTextColor.opacity(0.75)).accessibilityHidden(true)
                            }
                            .padding(WorkMetrics.cardInset)
                            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
                            .workCardSurface().contentShape(Rectangle())
                        }.buttonStyle(.plain).accessibilityIdentifier("work-memory-tier-" + name)
                    }
                }
            }
            WorkGroupCard("More") {
                WorkNavigationRow(title: "Consolidation health", systemImage: "waveform.path.ecg") { WorkEndpointView(title: "Consolidation health", path: "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))/memory/consolidation-health") }
                WorkRowDivider()
                WorkNavigationRow(title: "User memory and sharing", systemImage: "person.2") { WorkCrewUserMemoryView(agentID: agentID) }
            }
        }.padding(WorkMetrics.screenPadding) }.foregroundColor(theme.textColor).background(theme.backgroundColor)
        .navigationTitle("Memory tiers").navigationBarTitleDisplayMode(.inline)
        .task(id: agentID) { resource.load("/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))/memory") }.refreshable { resource.refresh() }
    }
}

struct WorkCrewTierView: View {
    let agentID: String
    let tier: String
    var initialGoal: String? = nil
    @StateObject private var resource = WorkResource()
    @StateObject private var summary = WorkResource()
    @State private var goal = ""
    @ObservedObject private var theme = ThemeManager.shared
    private var base: String { "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))/memory" }
    private var availableGoals: [String] {
        let entry = summary.value?["tiers"].array?.first { $0["tier_name"].text == tier }
        return Array(Set((entry?["available_goal_ids"].array ?? resource.value?["available_goal_ids"].array ?? []).compactMap(\.text))).sorted()
    }
    private var path: String { base + "/" + WorkAPIClient.segment(tier) + (goal.isEmpty ? "" : "?goal_id=" + WorkAPIClient.query(goal)) }
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: summary)
            if !availableGoals.isEmpty {
                WorkGroupCard {
                    WorkFormRow("Goal context") {
                        Picker("Goal context", selection: $goal) {
                            if !goal.isEmpty && !availableGoals.contains(goal) { Text(goal + " (not in current goals)").tag(goal) }
                            ForEach(availableGoals, id: \.self) { Text($0).tag($0) }
                        }.labelsHidden().pickerStyle(.menu).font(.themed(15))
                    }
                }
            }
            WorkResourceStatus(resource: resource)
            if let value = resource.value {
                WorkHeaderCard(WorkRecordView.label(tier),
                               subtitle: value["goal_id"].text.map { "Goal: " + $0 },
                               detail: value["tier"]["description"].text,
                               chips: [WorkChipSpec(title: value["tier"]["scope"].display.capitalized, tone: theme.secondaryTextColor),
                                       WorkChipSpec(title: value["shared"].flag == true ? "Shared" : "Private", tone: value["shared"].flag == true ? theme.accentColor : theme.secondaryTextColor)])
                WorkGroupCard("Persisted memory") { WorkRecordView(value: value["data"]) }
                WorkGroupCard("Sharing and consolidation") {
                    WorkRecordView(value: .object(["shared_with_agents": value["shared_with_agents"], "consolidation_rules": value["consolidation_rules"]]))
                }
            }
        }
        .navigationTitle("Memory tier").navigationBarTitleDisplayMode(.inline)
        .onAppear { goal = initialGoal ?? ""; summary.load(base); resource.load(path) }
        .onChange(of: availableGoals) { _, values in if goal.isEmpty, let first = values.first { goal = first } }
        .onChange(of: goal) { _, _ in resource.load(path) }
        .refreshable { resource.refresh(); summary.refresh() }
    }
}

struct WorkCrewUserMemoryView: View {
    let agentID: String
    @StateObject private var resource = WorkResource()
    @StateObject private var member = WorkResource()
    private var isolated: Bool { member.value?["definition"].isPersonalCrew == true && (member.value?["definition"]["user_memory_isolation"].text ?? "shared") != "shared" }
    private var userTiers: [WorkJSON] { (resource.value?["tiers"].array ?? []).filter { $0["scope"].text == "user" } }
    private var hasData: Bool { userTiers.contains { $0["has_data"].flag == true } }
    @State private var confirm = false
    @ObservedObject private var theme = ThemeManager.shared
    private var base: String { "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))" }
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            WorkResourceStatus(resource: member)
            if resource.value != nil {
                WorkGroupCard("User memory") {
                    ForEach(Array(userTiers.enumerated()), id: \.element.formatted) { index, tier in
                        if index > 0 { WorkRowDivider() }
                        if let name = tier["tier_name"].text {
                            WorkNavigationRow(title: WorkRecordView.label(name), systemImage: "person.crop.circle",
                                              value: tier["has_data"].flag == true ? "Has data" : "Empty",
                                              subtitle: "\(tier["scope"].display) · \(tier["shared"].flag == true ? "Shared" : "Private")") {
                                WorkCrewTierView(agentID: agentID, tier: name, initialGoal: tier["goal_id"].text)
                            }
                        }
                    }
                    if userTiers.isEmpty { WorkInfoRow(title: "No user memory tiers were reported.", systemImage: "person.crop.circle.badge.questionmark") }
                }
                if isolated {
                    WorkGroupCard(footer: hasData ? nil : "No local user memory is available to merge.") {
                        Button { confirm = true } label: {
                            WorkRowLabel(title: "Merge into shared user memory", systemImage: "arrow.triangle.merge", accessory: .none, tint: theme.accentColor)
                        }
                        .buttonStyle(.plain).disabled(resource.busy || !hasData)
                        .opacity(resource.busy || !hasData ? 0.45 : 1)
                    }
                }
            }
        }
        .navigationTitle("User memory").navigationBarTitleDisplayMode(.inline)
        .task(id: agentID) { resource.load(base + "/memory"); member.load(base) }.refreshable { resource.refresh(); member.refresh() }
        .onChange(of: resource.scopeID) { _, _ in confirm = false }
        .confirmationDialog("Merge this member's local user memory into shared knowledge?", isPresented: $confirm, titleVisibility: .visible) {
            Button("Merge reviewed memory") { Task { if await resource.perform(base + "/consolidate-user-memory", body: nil) != nil { resource.refresh() } } }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Shared user memory can be read by other eligible members in this workspace.") }
    }
}
