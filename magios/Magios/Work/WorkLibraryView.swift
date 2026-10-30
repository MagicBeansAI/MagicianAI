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
            VStack(alignment: .leading, spacing: 0) {
                Text("Opens your connected web console. Sign in there if asked; Back to Work returns to this item.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true).padding(.bottom, 8)
                if let error { Text(error).font(.themed(13)).foregroundColor(theme.dangerColor).padding(.bottom, 8) }
                if liveSurface {
                    WorkRowDivider()
                    Button {
                        guard let client, client.isCurrentConnection else { error = "Connection changed. Reopen this work view."; return }
                        guard let url = origin.workURL(base: client.baseURL) else { error = "This connection cannot open the web console."; return }
                        openURL(url) { accepted in if !accepted { error = "No browser is available to open this tool." } }
                    } label: {
                        WorkRowLabel(title: "Live agent surface in web console", accessory: .external)
                    }.buttonStyle(.plain)
                }
                ForEach(destinations) { destination in
                    let title = destination == .apps && !origin.appInstallation.isEmpty ? "Originating App" : destination.title
                    let scope = !destination.scoped && !(destination == .apps && !origin.appInstallation.isEmpty) ? "Workspace-wide" : nil
                    WorkRowDivider()
                    Button {
                        guard let client, client.isCurrentConnection else { error = "Connection changed. Reopen this work view."; return }
                        guard let url = origin.url(base: client.baseURL, destination: destination) else { error = "This connection cannot open the web console."; return }
                        openURL(url) { accepted in if !accepted { error = "No browser is available to open this tool." } }
                    } label: {
                        WorkRowLabel(title: title, value: scope, accessory: .external)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(title).accessibilityValue(scope ?? "")
                }
            }
        }
        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "safari"))
        .tint(theme.accentColor)
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
                case .goals: WorkGoalsView()
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
                    else if ![.crew, .goals, .programs, .operations].contains(target.kind) && !(target.kind == .quality && target.recordID.isEmpty && target.agentID == nil) { navigationPath = [target] }
                }
            }
            .sheet(isPresented: $creatingMember) { WorkCrewCreateView { navigationPath = [] } }
            // The switcher names the view; the bar stays "Work" like the
            // tasks and Needs you views it sits beside.
            .navigationTitle("Work").navigationBarTitleDisplayMode(.inline)
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
            return match && statusMatch && (kind == "All" || member.kind.caseInsensitiveCompare(kind) == .orderedSame)
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
    private func statusTone(_ member: WorkCrewMember) -> Color {
        member.disabled ? theme.secondaryTextColor : Color.workStatusTone(member.status == "idle" ? "active" : member.status)
    }
    private func chips(_ member: WorkCrewMember) -> [WorkChipSpec] {
        var chips = [WorkChipSpec(title: member.kind.isEmpty ? "Member" : member.kind.capitalized, tone: theme.secondaryTextColor),
                     WorkChipSpec(title: member.disabled ? "Disabled" : member.status, tone: statusTone(member))]
        if member.isPrimary { chips.append(WorkChipSpec(title: "Primary", systemImage: "star.fill", tone: theme.accentColor)) }
        return chips
    }
    private var summary: String {
        "\(allMembers.count) members · \(allMembers.filter { ["running", "triggered"].contains($0.status) }.count) active · \(allMembers.filter(\.disabled).count) disabled"
    }
    private func filterPicker(_ title: String, selection: Binding<String>, options: [String], identifier: String) -> some View {
        WorkFormRow(title) {
            Picker(title, selection: selection) { ForEach(options, id: \.self) { Text($0).tag($0) } }
                .labelsHidden().pickerStyle(.menu).font(.themed(15))
                .accessibilityLabel(title).accessibilityIdentifier(identifier)
        }
    }
    var body: some View {
        WorkPage {
            WorkSearchField("Find crew", text: $search)
            WorkGroupCard("View and filters") {
                filterPicker("Crew view", selection: $view, options: ["Members", "Leaderboard", "Hierarchy", "Pipeline roles"], identifier: "work-crew-view")
                if view != "Pipeline roles" {
                    WorkRowDivider()
                    filterPicker("Status", selection: $status, options: ["All", "Active", "Attention", "Paused", "Disabled"], identifier: "work-crew-status")
                    WorkRowDivider()
                    filterPicker("Kind", selection: $kind, options: ["All", "Personal", "Worker"], identifier: "work-crew-kind")
                }
            }
            WorkResourceStatus(resource: resource)
            if view == "Leaderboard" || status == "Attention" { WorkResourceStatus(resource: health) }
            if view == "Pipeline roles" {
                let roles = (resource.value?["system_agents"].array ?? []).filter { search.isEmpty || $0.formatted.localizedCaseInsensitiveContains(search) }
                if resource.value != nil {
                    WorkGroupCard("Pipeline roles", footer: "Read-only runtime pipeline roles.") {
                        if resource.value?["system_agents"].array?.isEmpty != false { WorkInfoRow(title: "No pipeline descriptors were reported.") }
                        ForEach(Array(roles.enumerated()), id: \.element.formatted) { index, role in
                            if index > 0 { WorkRowDivider() }
                            WorkItemLabel(title: role["name"].text ?? role["agent_id"].display, subtitle: role["agent_id"].display,
                                          detail: role["description"].text, systemImage: "gearshape.2", accessory: .none)
                                .accessibilityElement(children: .combine)
                        }
                    }
                }
            } else if resource.value != nil {
                VStack(alignment: .leading, spacing: 8) {
                    WorkSectionHeader(view == "Members" ? "Members" : view)
                    Text(summary).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        .padding(.horizontal, WorkMetrics.cardInset)
                    if members.isEmpty {
                        WorkGroupCard { WorkEmptyState(title: search.isEmpty ? "No crew members match these filters." : "No matching crew members.", systemImage: "person.2.slash") }
                    }
                    ForEach(Array(members.enumerated()), id: \.element.id) { index, member in
                        VStack(alignment: .leading, spacing: 0) {
                            NavigationLink { WorkCrewDetail(agentID: member.id) } label: {
                                WorkItemLabel(title: (view == "Leaderboard" ? "\(index + 1). " : "") + member.name,
                                              detail: view == "Members" ? member.description : nil,
                                              systemImage: member.isPrimary ? "star.fill" : "person.crop.circle",
                                              chips: chips(member))
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("work-crew-\(member.id)")
                            if view == "Leaderboard" { WorkRowDivider(); WorkCrewHealthSummary(value: healthFor(member.id)) }
                            else if view == "Hierarchy" { WorkRowDivider(); WorkCrewDelegation(member: member, members: allMembers) }
                        }
                        .padding(.horizontal, WorkMetrics.cardInset)
                        .workCardSurface()
                    }
                }
            }
            WorkGroupCard {
                WorkButtonRow(title: "Add crew member", systemImage: "person.badge.plus") { creating = true }
                    .accessibilityIdentifier("work-add-member")
            }
        }
        .task {
            resource.load("/api/magician/v2/agents"); health.load("/api/magician/v2/agents/health")
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(30)) } catch { return }
                if scenePhase == .active { resource.refresh(); health.refresh() }
            }
        }.refreshable { resource.refresh(); health.refresh() }
        .toolbar {
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
        if value == .null { WorkNote("Health and activity have not been reported.") }
        else {
            let state = value["overall"]["inputs"]["state"].display
            let rolling = value["rolling_7d"]
            WorkRowLabel(title: "Health", systemImage: "heart.text.square",
                         value: "\(value["overall"]["score"].display) · \(state.replacingOccurrences(of: "_", with: " "))", accessory: .none)
                .accessibilityElement(children: .combine)
            WorkRowDivider()
            WorkRowLabel(title: "Last 7 days", systemImage: "chart.bar",
                         value: "\(rolling["calls"].display) calls · $\(rolling["spend_usd"].display)",
                         accessory: .none, subtitle: "Success \(rolling["success_rate"].display)")
                .accessibilityElement(children: .combine)
            WorkRowDivider()
            DisclosureGroup("Health, coverage and history") { WorkRecordView(value: value) }
                .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "list.bullet.rectangle"))
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
        WorkInfoRow(title: "Delegation depth", systemImage: "arrow.triangle.branch", value: member.definition["coordination"]?["max_delegation_depth"].display ?? "—")
        WorkRowDivider()
        if parents.isEmpty { WorkInfoRow(title: "No configured parent", systemImage: "arrow.up.left") }
        ForEach(parents) { parent in
            WorkNavigationRow(title: "From: " + parent.name, systemImage: "arrow.up.left") { WorkCrewDetail(agentID: parent.id) }
        }
        WorkRowDivider()
        if targets.isEmpty { WorkInfoRow(title: "No delegation targets", systemImage: "arrow.down.right") }
        ForEach(targets, id: \.self) { id in
            if id == "*" { WorkInfoRow(title: "Can delegate to any member", systemImage: "arrow.down.right") }
            else if let target = members.first(where: { $0.id == id }) {
                WorkNavigationRow(title: "To: " + target.name, systemImage: "arrow.down.right") { WorkCrewDetail(agentID: id) }
                if let cycle = workDelegationCycle(origin: member.id, target: id, members: members) { WorkNote("Delegation cycle: " + cycle.joined(separator: " → "), tone: theme.warningColor) }
            } else { WorkNote("Missing target: " + id, tone: theme.warningColor) }
        }
    }
}

struct WorkProgramsList: View {
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let rows = resource.value?["programs"].array {
                WorkGroupCard(WorkPlaybookCopy.title(rows.count)) {
                    if rows.isEmpty { WorkEmptyState(title: WorkPlaybookCopy.empty, systemImage: "doc.text") }
                    ForEach(Array(rows.enumerated()), id: \.element.formatted) { index, row in
                        if let name = row["name"].text {
                            if index > 0 { WorkRowDivider() }
                            NavigationLink { WorkProgramDetail(name: name) } label: {
                                WorkItemLabel(title: row["title"].text ?? name, subtitle: name, systemImage: "doc.text")
                            }.buttonStyle(.plain).accessibilityIdentifier("work-program-\(name)")
                        }
                    }
                }
            }
            WorkProgramMembers(program: "")
        }
        .task { resource.load("/api/magician/v2/programs") }
        .refreshable { resource.refresh() }
        .toolbar { Button("Refresh playbooks", systemImage: "arrow.clockwise") { resource.refresh() } }
    }
}

/// The version an owner edit was opened from: its hash is the save's base,
/// and its content decides whether the draft has unsaved changes.
private struct WorkProgramEdit: Equatable {
    let name: String
    let title: String
    let base: String
    let original: String
    let scopeID: UUID
}

struct WorkProgramDetail: View {
    let name: String
    @StateObject private var resource = WorkResource()
    @State private var restore: String?
    @State private var edit: WorkProgramEdit?
    @State private var draft = ""
    @State private var saveError: String?
    @State private var conflict = false
    /// What a confirmed discard does next: leave edit mode, or reload the
    /// latest version after a conflict.
    @State private var discard: DiscardIntent?
    @ScaledMetric(relativeTo: .body) private var editorHeight: CGFloat = 360
    @ObservedObject private var theme = ThemeManager.shared
    private enum DiscardIntent { case cancel, reload }
    private var path: String { "/api/magician/v2/programs/\(WorkAPIClient.segment(name))" }
    private var hasChanges: Bool { edit.map { draft != $0.original } ?? false }
    private var canSave: Bool { edit != nil && hasChanges && !conflict && !resource.busy && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var body: some View {
        WorkPage {
                WorkResourceStatus(resource: resource)
                if let value = resource.value {
                    if let edit {
                        WorkHeaderCard(value["title"].text ?? name, subtitle: name)
                        editor(edit)
                    } else {
                        WorkHeaderCard(value["title"].text ?? name, subtitle: name)
                        VStack(alignment: .leading, spacing: 8) {
                            WorkSectionHeader("Document")
                            VStack(alignment: .leading, spacing: 10) {
                                Markdown(value["content"].text ?? "").markdownTextStyle { ForegroundColor(theme.textColor) }.textSelection(.enabled)
                                if value["sha256"].text?.isEmpty != false {
                                    Text("Editing needs a newer Magician runtime.").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                }
                            }
                            .padding(WorkMetrics.cardInset)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .workCardSurface()
                        }
                    }
                    WorkProgramMembers(program: name)
                    WorkGroupCard("Document history", footer: edit == nil ? nil : "Finish editing to restore a saved version.") {
                        if value["history"].array?.isEmpty != false { WorkInfoRow(title: "No saved versions.", systemImage: "clock.arrow.circlepath") }
                        ForEach(Array((value["history"].array ?? []).enumerated()), id: \.element.formatted) { index, version in
                            if let snapshot = version["snapshot"].text {
                                if index > 0 { WorkRowDivider() }
                                HStack(spacing: 12) {
                                    WorkRowLabel(title: version["modified"].text ?? snapshot, systemImage: "clock.arrow.circlepath", accessory: .none, subtitle: snapshot)
                                        .accessibilityElement(children: .combine)
                                    Button("Restore") { restore = snapshot }
                                        .font(.themed(15, weight: .semibold)).foregroundColor(theme.accentColor)
                                        .frame(minHeight: 44).buttonStyle(.borderless)
                                        .disabled(resource.busy || edit != nil)
                                        .opacity(resource.busy || edit != nil ? 0.45 : 1)
                                }
                            }
                        }
                    }
                    WorkGroupCard("Web console") { WorkRelatedTools(origin: WorkWebContext(section: .programs, program: name), destinations: [.history, .memory, .evals, .events]) }
                }
        }
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle("Playbook").navigationBarTitleDisplayMode(.inline)
        // An open draft is left only through Save or Cancel, so a stray back
        // swipe cannot drop unsaved edits.
        .navigationBarBackButtonHidden(edit != nil)
        .toolbar {
            if edit == nil, let value = resource.value {
                Button("Edit", systemImage: "pencil") { startEdit(value) }
                    .disabled(resource.busy || value["sha256"].text?.isEmpty != false)
                    .accessibilityIdentifier("work-program-edit")
            }
        }
        .task(id: name) { resource.load(path) }.refreshable { if edit == nil { resource.refresh() } }
        .onChange(of: resource.scopeID) { _, _ in restore = nil; stopEditing() }
        .confirmationDialog("Discard changes", isPresented: Binding(get: { discard != nil }, set: { if !$0 { discard = nil } }), titleVisibility: .visible) {
            if let intent = discard {
                Button("Discard changes", role: .destructive) {
                    discard = nil; stopEditing()
                    if intent == .reload { resource.refresh() }
                }
            }
            Button("Keep editing", role: .cancel) { discard = nil }
        } message: { Text("Discard your unsaved changes to \(edit?.title ?? name)?") }
        .confirmationDialog("Restore this playbook version?", isPresented: Binding(get: { restore != nil }, set: { if !$0 { restore = nil } }), titleVisibility: .visible) {
            if let snapshot = restore {
                Button("Restore version") {
                    restore = nil
                    Task { if await resource.perform(path + "/revert", body: .object(["snapshot": .string(snapshot)])) != nil { resource.refresh() } }
                }
            }
            Button("Keep current version", role: .cancel) { restore = nil }
        } message: { Text("The existing playbook provider restores this saved source document.") }
    }

    @ViewBuilder
    private func editor(_ edit: WorkProgramEdit) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            WorkSectionHeader("Markdown source")
            VStack(alignment: .leading, spacing: 12) {
                TextEditor(text: $draft)
                    .font(.themedMono(.callout))
                    .autocorrectionDisabled().textInputAutocapitalization(.never)
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(minHeight: editorHeight)
                    .background(theme.controlColor, in: RoundedRectangle(cornerRadius: WorkMetrics.fieldRadius, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: WorkMetrics.fieldRadius, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
                    .disabled(resource.busy)
                    .accessibilityLabel("Markdown for \(edit.name)")
                    .accessibilityIdentifier("work-program-editor")
                if let saveError {
                    VStack(alignment: .leading, spacing: 8) {
                        Label(saveError, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(14)).foregroundColor(theme.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                        if conflict {
                            Button("Reload latest version") { if hasChanges { discard = .reload } else { stopEditing(); resource.refresh() } }
                                .font(.themed(15, weight: .semibold)).foregroundColor(theme.accentColor).frame(minHeight: 44).disabled(resource.busy)
                                .accessibilityIdentifier("work-program-reload")
                        }
                    }
                    .padding(12).frame(maxWidth: .infinity, alignment: .leading)
                    .background((conflict ? theme.warningColor : theme.dangerColor).opacity(0.12), in: RoundedRectangle(cornerRadius: WorkMetrics.panelRadius))
                    .overlay(RoundedRectangle(cornerRadius: WorkMetrics.panelRadius).stroke((conflict ? theme.warningColor : theme.dangerColor).opacity(0.5)))
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier("work-program-save-error")
                }
                HStack(spacing: 10) {
                    Button("Cancel") { if hasChanges { discard = .cancel } else { stopEditing() } }
                        .font(.themed(15, weight: .semibold)).foregroundColor(theme.accentColor)
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .background(theme.accentColor.opacity(0.12), in: RoundedRectangle(cornerRadius: WorkMetrics.panelRadius))
                        .buttonStyle(.plain).disabled(resource.busy)
                        .accessibilityIdentifier("work-program-cancel")
                    Button { Task { await save(edit) } } label: {
                        Text(resource.busy ? "Saving…" : "Save").font(.themed(15, weight: .semibold))
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .background(theme.accentFillColor, in: RoundedRectangle(cornerRadius: WorkMetrics.panelRadius))
                            .foregroundColor(theme.onAccentColor)
                    }
                    .buttonStyle(.plain).disabled(!canSave).opacity(canSave || resource.busy ? 1 : 0.5)
                    .accessibilityIdentifier("work-program-save")
                }
            }
            .padding(WorkMetrics.cardInset)
            .frame(maxWidth: .infinity, alignment: .leading)
            .workCardSurface()
            Text("Saving keeps the current version in Document history, so you can restore it.")
                .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, WorkMetrics.cardInset)
        }
    }

    private func startEdit(_ value: WorkJSON) {
        guard edit == nil, !resource.busy, let sha = value["sha256"].text, !sha.isEmpty else { return }
        let content = value["content"].text ?? ""
        draft = content; saveError = nil; conflict = false
        edit = WorkProgramEdit(name: value["name"].text ?? name, title: value["title"].text ?? name, base: sha, original: content, scopeID: resource.scopeID)
    }

    private func stopEditing() {
        edit = nil; draft = ""; saveError = nil; conflict = false; discard = nil
    }

    private func save(_ session: WorkProgramEdit) async {
        guard edit == session, resource.scopeID == session.scopeID, canSave else { return }
        let content = draft
        saveError = nil; conflict = false
        guard let result = await resource.call({ try await $0.saveProgram(name: session.name, content: content, baseSHA256: session.base) }),
              edit == session else { return }
        switch result {
        case .success:
            stopEditing(); resource.refresh()
        case .failure(let error):
            if case .changed(let message, _) = error as? WorkProgramSaveError {
                conflict = true
                saveError = message + " Your edits are still in the editor; copy anything you need, then reload the latest version."
            } else { saveError = error.localizedDescription }
        }
    }
}

/// Direction references come from definitions; mutable progress comes from the
/// runtime projection and remains separate from the Markdown source document.
struct WorkProgramMembers: View {
    let program: String
    @StateObject private var crew = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private var records: [WorkJSON] { (crew.value?["agents"].array ?? []).filter { $0["definition"].isPersonalCrew && $0["definition"]["harness"].object != nil } }
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            WorkSectionHeader(program.isEmpty ? "Runtime direction and progress" : "Responsible crew and progress")
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: crew)
                if crew.value != nil && records.isEmpty {
                    WorkGroupCard { WorkInfoRow(title: "No playbook-enabled crew in this workspace.", systemImage: "person.2.slash") }
                }
                ForEach(records, id: \.formatted) { record in WorkProgramProgress(record: record, program: program) }
            }
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
    /// One text, so VoiceOver and tests read "Current Phase: Verify migration".
    private func fact(_ title: String, _ value: String) -> some View {
        (Text(title + ": ").font(.themed(14, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            + Text(value).font(.themed(15)).foregroundColor(theme.textColor))
            .fixedSize(horizontal: false, vertical: true)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, minHeight: WorkMetrics.rowMinHeight, alignment: .leading)
    }
    var body: some View {
        Group {
            if associated || overview.loading || overview.error != nil {
                VStack(alignment: .leading, spacing: 0) {
                    NavigationLink { WorkCrewDetail(agentID: agentID) } label: {
                        WorkItemLabel(title: definition["name"].text ?? agentID, subtitle: "Definition version: " + record["version"].display,
                                      systemImage: definition["is_primary"].flag == true ? "star.fill" : "person.crop.circle")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(definition["name"].text ?? agentID)
                    ForEach(references.filter { program.isEmpty || $0["path"].text == program }, id: \.formatted) { reference in
                        WorkRowDivider()
                        WorkInfoRow(title: reference["path"].display, systemImage: "doc.text",
                                    value: [reference["section"].text, reference["focus"].text.map { "focus: " + $0 }].compactMap { $0 }.joined(separator: " · "))
                    }
                    WorkResourceStatus(resource: overview)
                    if let value = overview.value {
                        WorkRowDivider()
                        if matching && active.object?.isEmpty == false {
                            fact("Current source", active["program"]["relative_path"].display)
                            ForEach(["current_phase", "current_step", "updated_at", "update_reason"], id: \.self) { key in
                                if active[key] != .null {
                                    WorkRowDivider()
                                    fact(WorkRecordView.label(key), active[key].display)
                                }
                            }
                            if active["open_loops"].array?.isEmpty == false {
                                WorkRowDivider()
                                DisclosureGroup("Open loops") { WorkRecordView(value: active["open_loops"]) }
                                    .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "circle.dashed", value: WorkRecordView.summary(active["open_loops"])))
                            }
                            if active["last_run_summary"] != .null {
                                WorkRowDivider()
                                DisclosureGroup("Latest cycle summary") { WorkRecordView(value: active["last_run_summary"]) }
                                    .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "text.alignleft"))
                            }
                            ForEach(Array((active["update_history"].array ?? []).suffix(5).reversed()), id: \.formatted) { update in
                                WorkRowDivider()
                                WorkNote(update["reason"].display, tone: theme.textColor)
                                if let taskID = update["task_id"].text {
                                    WorkNavigationRow(title: "Task and execution evidence", systemImage: "doc.text.magnifyingglass") { TaskDetailScreen(route: .init(taskId: taskID, executionId: update["execution_id"].text)) }
                                }
                            }
                        } else { WorkInfoRow(title: "No current runtime progress for this document.", systemImage: "hourglass") }
                        let metrics = (value["evaluation"]["program_metrics"]["documents"].array ?? []).filter { program.isEmpty || $0["relative_path"].text == program }
                        if !metrics.isEmpty {
                            WorkRowDivider()
                            DisclosureGroup("Evaluation metrics") { WorkRecordView(value: .array(metrics)) }
                                .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "chart.bar"))
                        }
                    }
                    WorkRowDivider()
                    WorkNavigationRow(title: "Focus health and operating controls", systemImage: "gauge.with.dots.needle.33percent") { WorkOperatorView(agentID: agentID) }
                }
                .padding(.horizontal, WorkMetrics.cardInset)
                .frame(maxWidth: .infinity, alignment: .leading)
                .workCardSurface()
            }
        }.foregroundColor(theme.textColor).task(id: agentID) { overview.load("/api/magician/v2/agents/" + WorkAPIClient.segment(agentID) + "/harness-overview") }
    }
}
