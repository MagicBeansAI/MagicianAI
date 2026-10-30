import SwiftUI

struct WorkOperationsView: View {
    @StateObject private var resource = WorkResource()
    @State private var changeEnabled: Bool?
    @State private var confirmSequence = false
    @State private var sequenceRunning = false
    @State private var sequenceID = UUID()
    @State private var sequenceResults: [WorkJSON] = []
    private let targets = [("cmo", "content-calendar", "Content Calendar"), ("cro", "weekly-gtm-review", "Weekly GTM Review"), ("cpo", "backlog-grooming", "Backlog Grooming"), ("cto", "morning-engineering-standup", "Engineering Standup"), ("ceo", "morning-briefing", "Executive Briefing")]
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
            WorkGroupCard("Run team routines now", footer: "Dispatch the existing CMO, CRO, CPO, CTO and CEO focus areas in order. This does not wait for their runs to finish.") {
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
        .confirmationDialog("Dispatch the five company focus areas?", isPresented: $confirmSequence, titleVisibility: .visible) {
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
        for (agent, goal, label) in targets {
            guard sequenceID == ticket, resource.scopeID == scope else { break }
            let result = await resource.perform("/api/magician/v2/agents/" + agent + "/trigger", body: .object(["goal_id": .string("harness:" + agent + ":" + goal), "trigger": .string("manual.harness.company_loop")]))
            guard sequenceID == ticket, resource.scopeID == scope else { break }
            sequenceResults.append(.object(["target": .string(label), "result": result ?? .object(["status": .string("failed"), "reason": .string(resource.error ?? "Dispatch failed")])]))
        }
        if sequenceID == ticket { sequenceRunning = false }
    }

}
