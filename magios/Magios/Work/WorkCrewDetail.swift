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
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let member {
                    WorkCard {
                        Text(member.name).font(.themed(22, weight: .semibold))
                        Text(member.kind).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        if !member.description.isEmpty { Text(member.description).font(.themed(15)) }
                        HStack {
                            Text(member.disabled ? "Disabled" : member.status).font(.themed(12))
                            if member.isPrimary { Label("Primary", systemImage: "star.fill").font(.themed(12)).foregroundColor(theme.accentColor) }
                        }
                        if member.kind.lowercased() == "personal" { Menu("Actions") {
                            if member.kind.lowercased() == "personal" {
                                Button("Run once") { confirmAction = .trigger }
                                Button("Pause cycles") { confirmAction = .pause }
                                Button("Resume cycles") { confirmAction = .resume }
                            }
                            if member.kind.lowercased() == "personal" && !member.isPrimary { Button("Make primary") { confirmAction = .setPrimary } }
                        }.frame(minHeight: 44).disabled(resource.busy) }
                    }
                    WorkCard {
                        Text("Details and controls").font(.themed(17, weight: .semibold))
                        if let taskID = member.cycleTaskID {
                            NavigationLink("Inspect current cycle") {
                                TaskDetailScreen(route: TaskDetailRoute(taskId: taskID, executionId: member.executionID, inspectRun: true, titleHint: member.name + " cycle"))
                            }.frame(minHeight: 44).accessibilityIdentifier("work-crew-current-cycle")
                        } else { Text("No current cycle execution.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                        endpoint("Member health", suffix: "/health")
                        if member.kind.lowercased() == "personal" {
                            NavigationLink("Bootstrap task") { WorkCrewBootstrap(agentID: agentID) }.frame(minHeight: 44)
                        }
                        NavigationLink("Operator view") { WorkOperatorView(agentID: agentID) }.frame(minHeight: 44)
                        endpoint("Effective tools", suffix: "/effective-tools")
                        endpoint("Runtime tool cache", suffix: "/runtime-context-cache")
                        NavigationLink("Memory tiers") { WorkCrewMemoryView(agentID: agentID) }.frame(minHeight: 44)
                        NavigationLink("Rules and policies") { WorkCrewRulesView(agentID: agentID) }.frame(minHeight: 44)
                        endpoint("Memory health", suffix: "/memory/consolidation-health")
                        NavigationLink("User memory and sharing") { WorkCrewUserMemoryView(agentID: agentID) }.frame(minHeight: 44)
                        NavigationLink("Programs") { WorkProgramsList().navigationTitle("Programs") }.frame(minHeight: 44)
                        NavigationLink("Quality and review") { WorkQualityView(agentID: agentID).navigationTitle("Quality and review") }.frame(minHeight: 44)
                    }
                    WorkCard {
                        DisclosureGroup("Definition, rules and model pins") {
                            Button("Edit definition") {
                                draft = .object(member.definition); editor = draft.formatted; guided = true
                                editing = WorkCrewEditingSession(etag: member.etag, scopeID: resource.scopeID)
                            }.frame(minHeight: 44)
                            WorkRecordView(value: .object(member.definition)).padding(.top, 12)
                        }.font(.themed(15))
                    }
                    WorkCard { WorkRelatedTools(origin: WorkWebContext(section: .crew, agent: agentID), liveSurface: true) }
                }
            }.padding(16)
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
                            .background(theme.cardColor).accessibilityIdentifier("work-crew-definition-editor")
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
    private func endpoint(_ title: String, suffix: String) -> some View {
        NavigationLink(title) { WorkEndpointView(title: title, path: path + suffix) }.frame(minHeight: 44)
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
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            if let value = resource.value {
                WorkCard {
                    Text("Bootstrap task").font(.themed(18, weight: .semibold))
                    WorkRecordView(value: value)
                    NavigationLink("Open task and execution details") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }.frame(minHeight: 44)
                }
            }
        }.padding(16) }.background(theme.backgroundColor).foregroundColor(theme.textColor)
            .navigationTitle("Bootstrap").navigationBarTitleDisplayMode(.inline)
            .task(id: agentID) { resource.load("/api/magician/v3/tasks/" + WorkAPIClient.query(taskID)) }
            .refreshable { resource.refresh() }
    }
}

/// Guided fields remain on-device; the advanced document is the same draft.
struct WorkCrewFields: View {
    @Binding var value: WorkJSON
    var editingID: String? = nil
    @ObservedObject private var theme = ThemeManager.shared
    private func text(_ label: String, _ path: [String], multiline: Bool = false, list: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(label).font(.themed(13, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            TextField(label, text: Binding(get: {
                let item = value.at(path)
                return list ? (item.array ?? []).compactMap(\.text).joined(separator: ", ") : item.text ?? ""
            }, set: { input in
                value = value.setting(path, to: list ? .array(input.split(separator: ",").map { .string($0.trimmingCharacters(in: .whitespacesAndNewlines)) }.filter { !$0.display.isEmpty }) : .string(input))
            }), axis: multiline ? .vertical : .horizontal)
            .font(.themed(15)).lineLimit(multiline ? 3...8 : 1...1)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
            .padding(10).frame(minHeight: 44).background(theme.cardColor, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor))
            .disabled(path == ["agent_id"] && editingID != nil)
            .accessibilityIdentifier("work-crew-field-" + path.joined(separator: "-"))
        }
    }
    private func choice(_ label: String, _ path: [String], _ options: [String], defaultValue: String) -> some View {
        Picker(label, selection: Binding(get: { value.at(path).text ?? defaultValue }, set: { value = value.setting(path, to: .string($0))
            if path == ["kind"] && $0 == "Worker" { for key in ["harness", "autonomous_config", "user_memory_isolation", "readable_agents"] { value = value.setting([key], to: nil) } }
        })) {
            ForEach(options, id: \.self) { Text($0.replacingOccurrences(of: "_", with: " ")).tag($0) }
        }.font(.themed(14)).frame(minHeight: 44)
    }
    private func number(_ label: String, _ path: [String], _ range: ClosedRange<Int>, defaultValue: Int) -> some View {
        let current = Int(value.at(path).display) ?? defaultValue
        return Stepper("\(label): \(current)", value: Binding(get: { current }, set: { value = value.setting(path, to: .integer(Int64($0))) }), in: range).font(.themed(14)).frame(minHeight: 44)
    }
    private func optional(_ label: String, _ key: String, initial: WorkJSON) -> some View {
        Toggle(label, isOn: Binding(get: { value[key].object != nil }, set: { enabled in value = value.setting([key], to: enabled ? initial : nil) })).font(.themed(15)).frame(minHeight: 44)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            WorkCard {
                Text("Identity").font(.themed(18, weight: .semibold))
                text("Member ID (optional on creation)", ["agent_id"])
                text("Name", ["name"])
                text("Description", ["description"], multiline: true)
                text("Persona", ["persona"], multiline: true)
                choice("Kind", ["kind"], ["Worker", "Personal"], defaultValue: "Worker")
                choice("Trust", ["trust_level"], ["local", "reviewed", "builtin", "untrusted"], defaultValue: "local")
            }
            WorkCard {
                DisclosureGroup("Tools and delegation") {
                    VStack(spacing: 14) {
                        text("Tools (comma-separated)", ["tools"], list: true)
                        text("Excluded tools (comma-separated)", ["excluded_tools"], list: true)
                        text("Delegate to (IDs or *)", ["delegation_targets"], list: true)
                        number("Delegation depth", ["coordination", "max_delegation_depth"], 1...10, defaultValue: 3)
                    }.padding(.top, 12)
                }
            }
            if value["kind"].text == "Personal" {
                WorkCard {
                    Text("Programs and cycles").font(.themed(18, weight: .semibold))
                    optional("Use program harness", "harness", initial: .object([:]))
                    if value["harness"].object != nil { text("Program section", ["harness", "program_section"]) }
                    optional("Autonomous cycles", "autonomous_config", initial: .object(["schedule": .string("0 */4 * * *"), "focus_areas": .array([]), "max_tasks_per_cycle": .integer(3), "max_steps_per_plan": .integer(10)]))
                    if value["autonomous_config"].object != nil {
                        text("Cycle schedule (cron)", ["autonomous_config", "schedule"])
                        number("Tasks per cycle", ["autonomous_config", "max_tasks_per_cycle"], 1...10, defaultValue: 3)
                        number("Steps per plan", ["autonomous_config", "max_steps_per_plan"], 1...30, defaultValue: 10)
                        ForEach(Array((value.at(["autonomous_config", "focus_areas"]).array ?? []).enumerated()), id: \.offset) { index, _ in
                            WorkFocusFields(value: Binding(get: { { let areas = value.at(["autonomous_config", "focus_areas"]).array ?? []; return areas.indices.contains(index) ? areas[index] : .object([:]) }() }, set: { area in
                                var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                                guard areas.indices.contains(index) else { return }; areas[index] = area
                                value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                            }), remove: {
                                var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                                guard areas.indices.contains(index) else { return }; areas.remove(at: index)
                                value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                            })
                        }
                        Button("Add focus area") {
                            var areas = value.at(["autonomous_config", "focus_areas"]).array ?? []
                            areas.append(.object(["name": .string(""), "description": .string(""), "priority": .string("medium")]))
                            value = value.setting(["autonomous_config", "focus_areas"], to: .array(areas))
                        }.frame(minHeight: 44)
                    }
                }
                WorkCard {
                    DisclosureGroup("Memory boundaries") {
                        choice("User memory", ["user_memory_isolation"], ["shared", "fully_isolated"], defaultValue: "shared")
                        text("Readable members (comma-separated)", ["readable_agents"], list: true)
                    }
                }
            }
            WorkCrewModelFields(value: $value)
        }.foregroundColor(theme.textColor).tint(theme.accentColor)
    }
}

private struct WorkFocusFields: View {
    @Binding var value: WorkJSON
    let remove: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    private func field(_ label: String, _ key: String) -> some View {
        TextField(label, text: Binding(get: { value[key].text ?? "" }, set: { value = value.setting([key], to: .string($0)) }), axis: .vertical)
            .font(.themed(14)).textInputAutocapitalization(.never).padding(10).frame(minHeight: 44)
            .background(theme.backgroundColor, in: RoundedRectangle(cornerRadius: 8)).accessibilityLabel(label)
    }
    var body: some View {
        DisclosureGroup(value["name"].text?.isEmpty == false ? value["name"].display : "New focus area") {
            VStack(spacing: 10) {
                field("Focus name", "name"); field("Focus description", "description")
                Picker("Priority", selection: Binding(get: { value["priority"].text ?? "medium" }, set: { value = value.setting(["priority"], to: .string($0)) })) {
                    ForEach(["high", "medium", "low"], id: \.self) { Text($0).tag($0) }
                }.frame(minHeight: 44)
                field("Schedule override (cron)", "schedule"); field("Program document", "program")
                TextField("Scope (comma-separated)", text: Binding(get: { (value["scope"].array ?? []).compactMap(\.text).joined(separator: ", ") }, set: { input in value = value.setting(["scope"], to: .array(input.split(separator: ",").map { .string($0.trimmingCharacters(in: .whitespacesAndNewlines)) })) })).font(.themed(14)).padding(10).frame(minHeight: 44)
                Button("Remove focus area", role: .destructive, action: remove).frame(minHeight: 44)
            }.padding(.top, 12)
        }.font(.themed(15))
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
                    TextEditor(text: $editor).font(.themed(13)).scrollContentBackground(.hidden).background(theme.cardColor)
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
    @ObservedObject private var theme = ThemeManager.shared
    private let lanes = ["planning", "evaluation", "correction_extraction", "memory_consolidation"]
    private func selection(_ lane: String) -> String {
        let endpoint = value.at(["llm_routing", lane])
        if let profile = endpoint["profile"].text, !profile.isEmpty { return profile }
        return endpoint.object == nil ? "" : "direct:" + (endpoint["provider"].text ?? "") + "/" + (endpoint["model"].text ?? "")
    }
    var body: some View {
        WorkCard {
            DisclosureGroup("Models") {
                VStack(alignment: .leading, spacing: 12) {
                    Text("An unpinned member follows global routing. Direct and per-operation pins are preserved; the advanced definition can edit them.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    WorkResourceStatus(resource: profiles)
                    ForEach(lanes, id: \.self) { lane in
                        Picker(WorkRecordView.label(lane), selection: Binding(get: { selection(lane) }, set: { selected in
                            if selected.hasPrefix("direct:") { return }
                            value = value.setting(["llm_routing", lane], to: selected.isEmpty ? nil : .object(["profile": .string(selected), "provider": .string(""), "model": .string("")]))
                        })) {
                            Text("Default — global routing").tag("")
                            let saved = selection(lane)
                            if !saved.isEmpty { Text(saved).tag(saved) }
                            ForEach(profiles.value?["profiles"].array ?? [], id: \.formatted) { profile in
                                if profile["selectable"].flag == true, let name = profile["name"].text, name != saved { Text(name + " · " + profile["model"].display).tag(name) }
                            }
                        }.font(.themed(14)).frame(minHeight: 44)
                    }
                    WorkResourceStatus(resource: coding)
                    Picker("Coding engine model", selection: Binding(get: { value.at(["llm_routing", "coding_profile"]).text ?? "" }, set: { value = value.setting(["llm_routing", "coding_profile"], to: $0.isEmpty ? nil : .string($0)) })) {
                        Text("Default — global routing").tag("")
                        let saved = value.at(["llm_routing", "coding_profile"]).text ?? ""
                        if !saved.isEmpty { Text(saved).tag(saved) }
                        ForEach(coding.value?["profiles"].array ?? [], id: \.formatted) { profile in
                            if profile["selectable"].flag != false, let id = profile["id"].text, id != saved, id != "auto" { Text(profile["label"].text ?? id).tag(id) }
                        }
                    }.font(.themed(14)).frame(minHeight: 44)
                    if value.at(["llm_routing", "operations"]).object?.isEmpty == false {
                        DisclosureGroup("Per-operation pins") { WorkRecordView(value: value.at(["llm_routing", "operations"])) }
                    }
                    Button("Clear lane and coding pins") {
                        for key in lanes + ["coding_profile"] { value = value.setting(["llm_routing", key], to: nil) }
                        if value["llm_routing"].object?.isEmpty == true { value = value.setting(["llm_routing"], to: nil) }
                    }.frame(minHeight: 44)
                }.padding(.top, 12)
            }
        }.task { profiles.load("/api/magician/v2/llm/routing"); coding.load("/api/magician/v2/coding/profiles") }
    }
}

struct WorkCrewRulesView: View {
    let agentID: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private let sections = ["persona", "goals", "triggers", "constraints", "memory_tiers", "memory_consolidation", "prompt_pipeline", "circuit_breaker", "feedback_loops", "notification_rules", "retention", "llm_routing", "observation", "strategy", "state_machines", "delegation_targets", "coordination"]
    var body: some View {
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            if let definition = resource.value.flatMap({ try? WorkCrewMember($0) })?.definition {
                ForEach(sections, id: \.self) { key in
                    WorkCard {
                        Text(WorkRecordView.label(key)).font(.themed(17, weight: .semibold))
                        if let value = definition[key], value != .null { WorkRecordView(value: value) }
                        else { Text("Not configured").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                    }
                }
            }
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor)
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
                            WorkCard {
                                Text(WorkRecordView.label(name)).font(.themed(17, weight: .semibold))
                                Text(tier["description"].text ?? "").font(.themed(14))
                                Text("\(tier["scope"].display) · \(tier["shared"].flag == true ? "Shared" : "Private") · \(tier["has_data"].flag == true ? "Has data" : "Empty")").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                            }
                        }.buttonStyle(.plain).frame(minHeight: 44).accessibilityIdentifier("work-memory-tier-" + name)
                    }
                }
            }
            NavigationLink("Consolidation health") { WorkEndpointView(title: "Consolidation health", path: "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))/memory/consolidation-health") }.frame(minHeight: 44)
            NavigationLink("User memory and sharing") { WorkCrewUserMemoryView(agentID: agentID) }.frame(minHeight: 44)
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor)
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
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: summary)
            if !availableGoals.isEmpty {
                Picker("Goal context", selection: $goal) {
                    if !goal.isEmpty && !availableGoals.contains(goal) { Text(goal + " (not in current goals)").tag(goal) }
                    ForEach(availableGoals, id: \.self) { Text($0).tag($0) }
                }.font(.themed(14)).frame(minHeight: 44)
            }
            WorkResourceStatus(resource: resource)
            if let value = resource.value {
                WorkCard {
                    Text(WorkRecordView.label(tier)).font(.themed(20, weight: .semibold))
                    Text(value["tier"]["description"].display).font(.themed(14))
                    Text("Scope: \(value["tier"]["scope"].display) · \(value["shared"].flag == true ? "Shared" : "Private")").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    if let selected = value["goal_id"].text { Text("Goal: \(selected)").font(.themed(13)) }
                }
                WorkCard { Text("Persisted memory").font(.themed(17, weight: .semibold)); WorkRecordView(value: value["data"]) }
                WorkCard { Text("Sharing and consolidation").font(.themed(17, weight: .semibold)); WorkRecordView(value: value["shared_with_agents"]); WorkRecordView(value: value["consolidation_rules"]) }
            }
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor)
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
    private var isolated: Bool { member.value?["definition"]["kind"].text == "Personal" && (member.value?["definition"]["user_memory_isolation"].text ?? "shared") != "shared" }
    private var userTiers: [WorkJSON] { (resource.value?["tiers"].array ?? []).filter { $0["scope"].text == "user" } }
    private var hasData: Bool { userTiers.contains { $0["has_data"].flag == true } }
    @State private var confirm = false
    @ObservedObject private var theme = ThemeManager.shared
    private var base: String { "/api/magician/v2/agents/\(WorkAPIClient.segment(agentID))" }
    var body: some View {
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            WorkResourceStatus(resource: member)
            if resource.value != nil {
                WorkCard {
                    Text("User memory").font(.themed(18, weight: .semibold))
                    ForEach(userTiers, id: \.formatted) { tier in
                        if let name = tier["tier_name"].text { NavigationLink(WorkRecordView.label(name)) { WorkCrewTierView(agentID: agentID, tier: name, initialGoal: tier["goal_id"].text) }.frame(minHeight: 44) }
                        WorkRecordView(value: tier)
                    }
                    if userTiers.isEmpty { Text("No user memory tiers were reported.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                }
                if isolated {
                    Button("Merge into shared user memory") { confirm = true }.frame(minHeight: 44).disabled(resource.busy || !hasData)
                    if !hasData { Text("No local user memory is available to merge.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                }
            }
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor)
        .navigationTitle("User memory").navigationBarTitleDisplayMode(.inline)
        .task(id: agentID) { resource.load(base + "/memory"); member.load(base) }.refreshable { resource.refresh(); member.refresh() }
        .onChange(of: resource.scopeID) { _, _ in confirm = false }
        .confirmationDialog("Merge this member's local user memory into shared knowledge?", isPresented: $confirm, titleVisibility: .visible) {
            Button("Merge reviewed memory") { Task { if await resource.perform(base + "/consolidate-user-memory", body: nil) != nil { resource.refresh() } } }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Shared user memory can be read by other eligible members in this workspace.") }
    }
}
