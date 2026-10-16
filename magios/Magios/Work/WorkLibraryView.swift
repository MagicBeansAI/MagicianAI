import SwiftUI

/// Specialist controls keep their established browser session and permissions.
struct WorkRelatedTools: View {
    let origin: WorkWebContext
    var destinations: [WorkSpecialist] = WorkSpecialist.allCases
    var liveSurface = false
    @State private var client: WorkAPIClient?
    @State private var error: String?
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.openURL) private var openURL
    var body: some View {
        DisclosureGroup("Related tools") {
            Text("Opens your connected web console. Sign in there if asked; Back to Work returns to this item.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            if let error { Text(error).font(.themed(13)).foregroundColor(theme.dangerColor) }
            if liveSurface {
                Button("Live agent surface in web console") {
                    guard let client, client.isCurrentConnection else { error = "Connection changed. Reopen this work view."; return }
                    guard let url = origin.workURL(base: client.baseURL) else { error = "This connection cannot open the web console."; return }
                    openURL(url) { accepted in if !accepted { error = "No browser is available to open this tool." } }
                }.frame(minHeight: 44)
            }
            ForEach(destinations) { destination in
                Button(destination == .apps && !origin.appInstallation.isEmpty ? "Originating App" : destination.title) {
                    guard let client, client.isCurrentConnection else { error = "Connection changed. Reopen this work view."; return }
                    guard let url = origin.url(base: client.baseURL, destination: destination) else { error = "This connection cannot open the web console."; return }
                    openURL(url) { accepted in if !accepted { error = "No browser is available to open this tool." } }
                }.frame(minHeight: 44)
                if !destination.scoped && !(destination == .apps && !origin.appInstallation.isEmpty) { Text("Workspace-wide").font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
            }
        }.font(.themed(15)).tint(theme.accentColor)
        .task { if client == nil { client = try? WorkAPIClient.connected() } }
    }
}
import MarkdownUI

struct WorkLibraryView: View {
    let section: WorkSection
    let onCreate: () -> Void
    var workNavigation: AnyView? = nil
    var initialTarget: WorkNavigationTarget? = nil
    @State private var navigationPath: [WorkNavigationTarget] = []
    @State private var creatingMember = false
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        NavigationStack(path: $navigationPath) {
            VStack(spacing: 0) {
                if let workNavigation { workNavigation }
                Group {
                switch section {
                case .crew: WorkCrewList()
                case .programs: WorkProgramsList()
                case .quality: WorkQualityView()
                case .operations: WorkOperationsView()
                default: EmptyView()
                }
            }
            }
            .navigationDestination(for: WorkNavigationTarget.self) { target in
                switch target.kind {
                case .member: WorkCrewDetail(agentID: target.recordID)
                case .editMember: WorkCrewDetail(agentID: target.recordID, openEditor: true)
                case .addMember: WorkCrewCreateView { navigationPath = [] }
                case .rules: WorkCrewRulesView(agentID: target.recordID)
                case .memory: WorkCrewTierView(agentID: target.recordID, tier: target.tier, initialGoal: target.goalID)
                case .program: WorkProgramDetail(name: target.recordID)
                case .quality: WorkQualityView(taskID: target.recordID.isEmpty ? nil : target.recordID, agentID: target.agentID)
                case .qualityRun: WorkQualityRunDetail(runID: target.recordID)
                case .qualityProposal: WorkQualityProposalDetail(proposalID: target.recordID)
                case .qualityCampaign: WorkPolicyDetail(kind: .capture, row: .null, recordID: target.recordID)
                default: EmptyView()
                }
            }
            .task(id: initialTarget) {
                if let target = initialTarget, target.section == section {
                    if target.kind == .addMember { creatingMember = true }
                    else if ![.crew, .programs, .operations].contains(target.kind) && !(target.kind == .quality && target.recordID.isEmpty && target.agentID == nil) { navigationPath = [target] }
                }
            }
            .sheet(isPresented: $creatingMember) { WorkCrewCreateView { navigationPath = [] } }
            .navigationTitle(section.title).navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { HamburgerButton() }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("New work", systemImage: "plus", action: onCreate).accessibilityIdentifier("work-new")
                }
            }
            .tint(theme.accentColor)
            .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        }
    }
}

struct WorkCrewList: View {
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var resource = WorkResource()
    @StateObject private var health = WorkResource()
    @State private var search = ""
    @State private var creating = false
    @State private var view = "Members"
    @State private var status = "All"
    @State private var kind = "All"
    @State private var reloadDefinitions = false
    @ObservedObject private var theme = ThemeManager.shared
    private var allMembers: [WorkCrewMember] {
        (resource.value?["agents"].array ?? resource.value?.array ?? []).compactMap { try? WorkCrewMember($0) }
    }
    private func healthFor(_ id: String) -> WorkJSON {
        health.value?["agents"].array?.first { $0["agent_id"].text == id } ?? .null
    }
    private var members: [WorkCrewMember] {
        let filtered = allMembers.filter { member in
            let match = search.isEmpty || "\(member.name) \(member.id) \(member.description)".localizedCaseInsensitiveContains(search)
            let statusMatch: Bool
            switch status {
            case "Active": statusMatch = ["running", "triggered"].contains(member.status)
            case "Attention": statusMatch = ["error", "partial"].contains(member.status) || healthFor(member.id)["overall"]["inputs"]["state"].text == "needs_attention"
            case "Paused": statusMatch = member.status == "paused"
            case "Disabled": statusMatch = member.disabled || member.status == "disabled"
            default: statusMatch = true
            }
            return match && statusMatch && (kind == "All" || member.kind == kind)
        }
        if view == "Leaderboard" {
            return filtered.sorted {
                let left = healthFor($0.id)["rolling_7d"], right = healthFor($1.id)["rolling_7d"]
                let lc = Int(left["calls"].display) ?? -1, rc = Int(right["calls"].display) ?? -1
                if lc != rc { return lc > rc }
                let ls = Double(left["spend_usd"].display) ?? -1, rs = Double(right["spend_usd"].display) ?? -1
                return ls == rs ? $0.name.localizedCompare($1.name) == .orderedAscending : ls > rs
            }
        }
        return filtered.sorted { $0.name.localizedCompare($1.name) == .orderedAscending }
    }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                Picker("Crew view", selection: $view) {
                    ForEach(["Members", "Leaderboard", "Hierarchy", "Pipeline roles"], id: \.self) { Text($0).tag($0) }
                }.pickerStyle(.menu).frame(minHeight: 44)
                if view != "Pipeline roles" {
                    HStack {
                        Picker("Status", selection: $status) { ForEach(["All", "Active", "Attention", "Paused", "Disabled"], id: \.self) { Text($0).tag($0) } }
                        Picker("Kind", selection: $kind) { ForEach(["All", "Personal", "Worker"], id: \.self) { Text($0).tag($0) } }
                    }.pickerStyle(.menu)
                }
                WorkResourceStatus(resource: resource)
                if view == "Leaderboard" || status == "Attention" { WorkResourceStatus(resource: health) }
                if resource.value != nil {
                    Text("\(allMembers.count) members · \(allMembers.filter { ["running", "triggered"].contains($0.status) }.count) active · \(allMembers.filter(\.disabled).count) disabled").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                }
                if view == "Pipeline roles" {
                    Text("Read-only runtime pipeline roles").font(.themed(14)).foregroundColor(theme.secondaryTextColor)
                    ForEach((resource.value?["system_agents"].array ?? []).filter { search.isEmpty || $0.formatted.localizedCaseInsensitiveContains(search) }, id: \.formatted) { role in
                        WorkCard {
                            Text(role["name"].text ?? role["agent_id"].display).font(.themed(17, weight: .semibold))
                            Text(role["agent_id"].display).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                            Text(role["description"].display).font(.themed(14))
                        }
                    }
                    if resource.value != nil && resource.value?["system_agents"].array?.isEmpty != false { Text("No pipeline descriptors were reported.").foregroundColor(theme.secondaryTextColor) }
                } else {
                    if resource.value != nil && members.isEmpty { Text(search.isEmpty ? "No crew members match these filters." : "No matching crew members.").font(.themed(14)).foregroundColor(theme.secondaryTextColor) }
                    ForEach(Array(members.enumerated()), id: \.element.id) { index, member in
                        WorkCard {
                            NavigationLink { WorkCrewDetail(agentID: member.id) } label: {
                                Label((view == "Leaderboard" ? "\(index + 1). " : "") + member.name, systemImage: member.isPrimary ? "star.fill" : "person.crop.circle")
                                    .font(.themed(17, weight: .semibold)).frame(minHeight: 44)
                            }.accessibilityIdentifier("work-crew-\(member.id)")
                            Text(member.kind + " · " + (member.disabled ? "Disabled" : member.status)).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                            if view == "Leaderboard" { WorkCrewHealthSummary(value: healthFor(member.id)) }
                            else if view == "Hierarchy" { WorkCrewDelegation(member: member, members: allMembers) }
                            else if !member.description.isEmpty { Text(member.description).font(.themed(14)).lineLimit(3) }
                        }
                    }
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor).searchable(text: $search, prompt: "Find crew")
        .task {
            resource.load("/api/magician/v2/agents"); health.load("/api/magician/v2/agents/health")
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                if scenePhase == .active { resource.refresh(); health.refresh() }
            }
        }.refreshable { resource.refresh(); health.refresh() }
        .toolbar {
            Button("Add member", systemImage: "person.badge.plus") { creating = true }.accessibilityIdentifier("work-add-member")
            Menu("Refresh crew", systemImage: "arrow.clockwise") {
                Button("Refresh status and health") { resource.refresh(); health.refresh() }
                Button("Reload definitions from disk") { reloadDefinitions = true }.disabled(resource.busy)
            }
        }
        .sheet(isPresented: $creating) { WorkCrewCreateView { resource.refresh() } }
        .onChange(of: resource.scopeID) { _, _ in reloadDefinitions = false }
        .confirmationDialog("Reload Crew definitions from disk?", isPresented: $reloadDefinitions, titleVisibility: .visible) {
            Button("Reload definitions") { Task { if await resource.perform("/api/magician/v2/agents/refresh-definitions") != nil { resource.refresh(); health.refresh() } } }
            Button("Cancel", role: .cancel) {}
        } message: { Text("Reloads the runtime definition cache after edits made outside the app.") }
    }
}

private struct WorkCrewHealthSummary: View {
    let value: WorkJSON
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        if value == .null { Text("Health and activity have not been reported.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
        else {
            Text("Health: \(value["overall"]["score"].display) · \(value["overall"]["inputs"]["state"].display)").font(.themed(14))
            let rolling = value["rolling_7d"]
            Text("7 days · \(rolling["calls"].display) calls · $\(rolling["spend_usd"].display) · success \(rolling["success_rate"].display)").font(.themed(13))
            DisclosureGroup("Health, coverage and history") { WorkRecordView(value: value) }.font(.themed(13))
        }
    }
}

private struct WorkCrewDelegation: View {
    let member: WorkCrewMember
    let members: [WorkCrewMember]
    @ObservedObject private var theme = ThemeManager.shared
    private var targets: [String] { member.definition["delegation_targets"]?.array?.compactMap(\.text) ?? [] }
    private var parents: [WorkCrewMember] { members.filter { $0.definition["delegation_targets"]?.array?.contains(.string(member.id)) == true || $0.definition["delegation_targets"]?.array?.contains(.string("*")) == true } }
    var body: some View {
        Text("Delegation depth: \(member.definition["coordination"]?["max_delegation_depth"].display ?? "—")").font(.themed(13))
        if parents.isEmpty { Text("No configured parent").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
        ForEach(parents) { parent in NavigationLink("From: " + parent.name) { WorkCrewDetail(agentID: parent.id) }.frame(minHeight: 44) }
        if targets.isEmpty { Text("No delegation targets").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
        ForEach(targets, id: \.self) { id in
            if id == "*" { Text("Can delegate to any member").font(.themed(13)) }
            else if let target = members.first(where: { $0.id == id }) {
                NavigationLink("To: " + target.name) { WorkCrewDetail(agentID: id) }.frame(minHeight: 44)
                if let cycle = workDelegationCycle(origin: member.id, target: id, members: members) { Text("Delegation cycle: " + cycle.joined(separator: " → ")).font(.themed(12)).foregroundColor(theme.warningColor) }
            } else { Text("Missing target: " + id).font(.themed(13)).foregroundColor(theme.warningColor) }
        }
    }
}

struct WorkProgramsList: View {
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView {
            LazyVStack(spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let rows = resource.value?["programs"].array {
                    if rows.isEmpty { Text("No scoped program documents yet.").foregroundColor(theme.secondaryTextColor) }
                    ForEach(rows, id: \.formatted) { row in
                        if let name = row["name"].text {
                            NavigationLink { WorkProgramDetail(name: name) } label: {
                                WorkCard {
                                    Text(row["title"].text ?? name).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor)
                                    Text(name).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                }
                            }.buttonStyle(.plain).accessibilityIdentifier("work-program-\(name)")
                        }
                    }
                }
                WorkProgramMembers(program: "")
            }.padding(16)
        }
        .background(theme.backgroundColor).task { resource.load("/api/magician/v2/programs") }
        .refreshable { resource.refresh() }
        .toolbar { Button("Refresh programs", systemImage: "arrow.clockwise") { resource.refresh() } }
    }
}

struct WorkProgramDetail: View {
    let name: String
    @StateObject private var resource = WorkResource()
    @State private var restore: String?
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { "/api/magician/v2/programs/\(WorkAPIClient.segment(name))" }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                WorkResourceStatus(resource: resource)
                if let value = resource.value {
                    WorkCard {
                        Text(value["title"].text ?? name).font(.themed(20, weight: .semibold)).foregroundColor(theme.textColor)
                        Markdown(value["content"].text ?? "").markdownTextStyle { ForegroundColor(theme.textColor) }.textSelection(.enabled)
                    }
                    WorkProgramMembers(program: name)
                    WorkCard { WorkRelatedTools(origin: WorkWebContext(section: .programs, program: name), destinations: [.history, .memory, .evals, .events]) }
                    WorkCard {
                        Text("History").font(.themed(17, weight: .semibold))
                        if value["history"].array?.isEmpty != false { Text("No saved versions.").foregroundColor(theme.secondaryTextColor) }
                        ForEach(value["history"].array ?? [], id: \.formatted) { version in
                            if let snapshot = version["snapshot"].text {
                                HStack {
                                    VStack(alignment: .leading) {
                                        Text(version["modified"].text ?? snapshot).font(.themed(13))
                                        Text(snapshot).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                                    }
                                    Spacer()
                                    Button("Restore") { restore = snapshot }.frame(minHeight: 44).disabled(resource.busy)
                                }
                            }
                        }
                    }
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor)
        .navigationTitle("Program").navigationBarTitleDisplayMode(.inline)
        .task(id: name) { resource.load(path) }.refreshable { resource.refresh() }
        .onChange(of: resource.scopeID) { _, _ in restore = nil }
        .confirmationDialog("Restore this program version?", isPresented: Binding(get: { restore != nil }, set: { if !$0 { restore = nil } }), titleVisibility: .visible) {
            if let snapshot = restore {
                Button("Restore version") {
                    restore = nil
                    Task { if await resource.perform(path + "/revert", body: .object(["snapshot": .string(snapshot)])) != nil { resource.refresh() } }
                }
            }
            Button("Keep current version", role: .cancel) { restore = nil }
        } message: { Text("The existing program provider restores this saved source document.") }
    }
}

/// Direction references come from definitions; mutable progress comes from the
/// runtime projection and remains separate from the Markdown source document.
struct WorkProgramMembers: View {
    let program: String
    @StateObject private var crew = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private var records: [WorkJSON] { (crew.value?["agents"].array ?? []).filter { $0["definition"]["kind"].text == "Personal" && $0["definition"]["harness"].object != nil } }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(program.isEmpty ? "Runtime direction and progress" : "Responsible crew and progress").font(.themed(18, weight: .semibold))
            WorkResourceStatus(resource: crew)
            if crew.value != nil && records.isEmpty { Text("No program-enabled crew in this workspace.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
            ForEach(records, id: \.formatted) { record in WorkProgramProgress(record: record, program: program) }
        }.task { crew.load("/api/magician/v2/agents") }
    }
}

private struct WorkProgramProgress: View {
    let record: WorkJSON
    let program: String
    @StateObject private var overview = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private var definition: WorkJSON { record["definition"] }
    private var agentID: String { definition["agent_id"].text ?? "" }
    private var references: [WorkJSON] {
        let main: WorkJSON = .object(["path": .string("program.md"), "section": definition["harness"]["program_section"]])
        return [main] + (definition["autonomous_config"]["focus_areas"].array ?? []).map { area in
            let path = area["program"].text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            return .object(["path": .string(path.isEmpty ? "program.md" : path), "focus": area["name"], "section": path.isEmpty ? definition["harness"]["program_section"] : .null])
        }
    }
    private var active: WorkJSON { overview.value?["evaluation"]["active_program_state"] ?? .null }
    private var associated: Bool { program.isEmpty || references.contains { $0["path"].text == program } || overview.value?["context"]["program"]["relative_path"].text == program }
    private var matching: Bool { program.isEmpty || active["program"]["relative_path"].text == program }
    var body: some View {
        Group {
            if associated || overview.loading || overview.error != nil {
                WorkCard {
                    NavigationLink(definition["name"].text ?? agentID) { WorkCrewDetail(agentID: agentID) }.font(.themed(17, weight: .semibold)).frame(minHeight: 44)
                    Text("Definition version: " + record["version"].display).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    ForEach(references.filter { program.isEmpty || $0["path"].text == program }, id: \.formatted) { reference in
                        Text(reference["path"].display + (reference["section"].text.map { " · " + $0 } ?? "") + (reference["focus"].text.map { " · focus: " + $0 } ?? "")).font(.themed(13))
                    }
                    WorkResourceStatus(resource: overview)
                    if let value = overview.value {
                        if matching && active.object?.isEmpty == false {
                            Text("Current source: " + active["program"]["relative_path"].display).font(.themed(13))
                            ForEach(["current_phase", "current_step", "updated_at", "update_reason"], id: \.self) { key in
                                if active[key] != .null { Text(WorkRecordView.label(key) + ": " + active[key].display).font(.themed(14)) }
                            }
                            if active["open_loops"].array?.isEmpty == false { DisclosureGroup("Open loops") { WorkRecordView(value: active["open_loops"]) } }
                            if active["last_run_summary"] != .null { DisclosureGroup("Latest cycle summary") { WorkRecordView(value: active["last_run_summary"]) } }
                            ForEach(Array((active["update_history"].array ?? []).suffix(5).reversed()), id: \.formatted) { update in
                                Text(update["reason"].display).font(.themed(13))
                                if let taskID = update["task_id"].text { NavigationLink("Task and execution evidence") { TaskDetailScreen(route: .init(taskId: taskID, executionId: update["execution_id"].text)) }.frame(minHeight: 44) }
                            }
                        } else { Text("No current runtime progress for this document.").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                        let metrics = (value["evaluation"]["program_metrics"]["documents"].array ?? []).filter { program.isEmpty || $0["relative_path"].text == program }
                        if !metrics.isEmpty { DisclosureGroup("Evaluation metrics") { WorkRecordView(value: .array(metrics)) } }
                    }
                    NavigationLink("Focus health and operating controls") { WorkOperatorView(agentID: agentID) }.frame(minHeight: 44)
                }
            }
        }.foregroundColor(theme.textColor).task(id: agentID) { overview.load("/api/magician/v2/agents/" + WorkAPIClient.segment(agentID) + "/harness-overview") }
    }
}
