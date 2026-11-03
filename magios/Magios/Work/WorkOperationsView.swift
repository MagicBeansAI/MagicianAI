import SwiftUI

struct WorkOperationsView: View {
    @StateObject private var resource = WorkResource()
    @State private var changeEnabled: Bool?
    @State private var confirmSequence = false
    @State private var sequenceRunning = false
    @State private var sequenceID = UUID()
    @State private var sequenceResults: [WorkJSON] = []
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let runtime = resource.value {
                let running = runtime["enabled"].flag == true
                WorkHeaderCard("Team routines schedule",
                               detail: "Controls the shared team routines. Each crew member and each task retain their own controls.",
                               chips: [WorkChipSpec(title: running ? "Running" : "Paused", systemImage: running ? "play.fill" : "pause.fill", tone: running ? theme.successColor : theme.warningColor)]) {
                    WorkButtonRow(title: running ? "Pause team routines" : "Resume team routines", systemImage: running ? "pause.circle" : "play.circle") { changeEnabled = !running }
                        .disabled(resource.busy)
                    WorkRowDivider()
                    DisclosureGroup("Runtime status") { WorkRecordView(value: runtime) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "info.circle"))
                }
            }
            WorkGroupCard("Run team routines now", footer: "Dispatch each crew member's team routines (their focus areas) in order. This does not wait for their runs to finish.") {
                WorkButtonRow(title: sequenceRunning ? "Dispatching…" : "Start team sequence", systemImage: "forward.end.alt") { confirmSequence = true }
                    .disabled(sequenceRunning || resource.busy || resource.value == nil)
                ForEach(Array(sequenceResults.enumerated()), id: \.offset) { _, result in
                    WorkRowDivider()
                    WorkRecordView(value: result)
                }
            }
            WorkGroupCard("Inspect") {
                WorkNavigationRow(title: "Anomalies and repair evidence", systemImage: "exclamationmark.triangle") { WorkEndpointView(title: "Anomalies", path: "/api/magician/v2/harness/anomalies") }
                WorkRowDivider()
                WorkNavigationRow(title: "Crew and individual cycles", systemImage: "person.2") { WorkCrewList().navigationTitle("Crew").navigationBarTitleDisplayMode(.inline) }
                WorkRowDivider()
                WorkNavigationRow(title: "Playbooks", systemImage: "doc.text") { WorkProgramsList().navigationTitle("Playbooks").navigationBarTitleDisplayMode(.inline) }
            }
            WorkGroupCard("Web console") { WorkRelatedTools(origin: WorkWebContext(section: .operations)) }
        }
        .toolbar { Button("Refresh team routines", systemImage: "arrow.clockwise") { resource.refresh() } }
        .task { resource.load("/api/magician/v2/harness/runtime") }.refreshable { resource.refresh() }
        .onChange(of: resource.scopeID) { _, _ in changeEnabled = nil; confirmSequence = false; sequenceID = UUID(); sequenceRunning = false; sequenceResults = [] }
        .onDisappear { sequenceID = UUID() }
        .confirmationDialog("Dispatch the team routines?", isPresented: $confirmSequence, titleVisibility: .visible) {
            Button("Dispatch reviewed sequence") { Task { await dispatchSequence() } }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog("Change the team routines schedule?", isPresented: Binding(get: { changeEnabled != nil }, set: { if !$0 { changeEnabled = nil } }), titleVisibility: .visible) {
            if let enabled = changeEnabled {
                Button(enabled ? "Resume team routines" : "Pause team routines") {
                    changeEnabled = nil
                    Task {
                        if let result = await resource.perform("/api/magician/v2/harness/runtime", method: "PUT", body: .object(["enabled": .bool(enabled)])) {
                            resource.refresh()
                            if let warnings = result["warnings"].array, !warnings.isEmpty { resource.error = warnings.map(\.display).joined(separator: "; ") }
                        }
                    }
                }
            }
            Button("Cancel", role: .cancel) { changeEnabled = nil }
        } message: { Text("The server applies the existing loop policy and reports cleanup warnings.") }
    }
    private func dispatchSequence() async {
        let ticket = UUID(); sequenceID = ticket; sequenceRunning = true; sequenceResults = []
        let scope = resource.scopeID
        // The live roster, read once: a routine retired since this screen opened must not run.
        let agents = await resource.perform("/api/magician/v2/agents", method: "GET", body: nil, successNotice: nil)
        let targets = agents.map(Self.teamRoutineTargets) ?? []
        if agents != nil, targets.isEmpty, sequenceID == ticket {
            resource.error = "No team routines are configured: no harness crew member has a focus area."
        }
        for target in targets {
            guard sequenceID == ticket, resource.scopeID == scope else { break }
            let result = await resource.perform("/api/magician/v2/agents/" + target.agent + "/trigger", body: .object(["goal_id": .string(target.goal), "trigger": .string("manual.harness.company_loop")]))
            guard sequenceID == ticket, resource.scopeID == scope else { break }
            sequenceResults.append(.object(["target": .string(target.agent + " · " + target.label), "result": result ?? .object(["status": .string("failed"), "reason": .string(resource.error ?? "Dispatch failed")])]))
        }
        if sequenceID == ticket { sequenceRunning = false }
    }

    /// One stage per focus area of each harness agent, read from the live
    /// definitions: a hard-coded list kept triggering routines retired months
    /// ago. Agents in id order; focus areas whose names slug alike are one
    /// server goal, so one stage.
    static func teamRoutineTargets(_ agents: WorkJSON) -> [(agent: String, goal: String, label: String)] {
        var seen = Set<String>()
        return (agents["agents"].array ?? agents.array ?? [])
            .map { $0["definition"] }
            .filter { $0["harness"] != .null }
            .compactMap { definition in definition["agent_id"].text.map { ($0, definition) } }
            .sorted { $0.0 < $1.0 }
            .flatMap { agent, definition in
                (definition["autonomous_config"]["focus_areas"].array ?? []).compactMap { area -> (agent: String, goal: String, label: String)? in
                    guard let name = area["name"].text else { return nil }
                    let goal = "harness:\(agent):\(harnessGoalSlug(name))"
                    guard seen.insert(goal).inserted else { return nil }
                    return (agent, goal, name)
                }
            }
    }

    /// The backend's `slugify_name`: ASCII letters and digits kept, every other
    /// run one `-`, trimmed, `unnamed` when empty, cut to 64 at a hyphen.
    static func harnessGoalSlug(_ name: String) -> String {
        let mapped = String(name.lowercased().unicodeScalars.map { scalar -> Character in
            let value = scalar.value
            return (97...122).contains(value) || (48...57).contains(value) ? Character(scalar) : "-"
        })
        let collapsed = mapped.split(separator: "-").joined(separator: "-")
        if collapsed.isEmpty { return "unnamed" }
        guard collapsed.count > 64 else { return collapsed }
        let head = String(collapsed.prefix(64))
        if let cut = head.lastIndex(of: "-") { return String(head[..<cut]) }
        return head
    }

}
