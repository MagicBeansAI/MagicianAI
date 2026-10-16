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
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let runtime = resource.value {
                    WorkCard {
                        Text("Company loop cadence").font(.themed(18, weight: .semibold))
                        Text(runtime["enabled"].flag == true ? "Running" : "Paused").font(.themed(16))
                        Text("Controls the shared company loop. Each crew member and each task retain their own controls.")
                            .font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        Button(runtime["enabled"].flag == true ? "Pause company loop" : "Resume company loop") { changeEnabled = runtime["enabled"].flag != true }
                            .frame(minHeight: 44).disabled(resource.busy)
                        DisclosureGroup("Runtime status") { WorkRecordView(value: runtime) }
                    }
                }
                WorkCard {
                    Text("Manual company sequence").font(.themed(18, weight: .semibold))
                    Text("Dispatch the existing CMO, CRO, CPO, CTO and CEO focus areas in order. This does not wait for their runs to finish.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    Button(sequenceRunning ? "Dispatching…" : "Run company sequence") { confirmSequence = true }.frame(minHeight: 44).disabled(sequenceRunning || resource.busy || resource.value == nil)
                    ForEach(sequenceResults, id: \.formatted) { WorkRecordView(value: $0) }
                }
                WorkCard {
                    NavigationLink("Anomalies and repair evidence") { WorkEndpointView(title: "Anomalies", path: "/api/magician/v2/harness/anomalies") }.frame(minHeight: 44)
                    NavigationLink("Crew and individual cycles") { WorkCrewList().navigationTitle("Crew") }.frame(minHeight: 44)
                    NavigationLink("Programs") { WorkProgramsList().navigationTitle("Programs") }.frame(minHeight: 44)
                    WorkRelatedTools(origin: WorkWebContext(section: .operations))
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor)
        .task { resource.load("/api/magician/v2/harness/runtime") }.refreshable { resource.refresh() }
        .onChange(of: resource.scopeID) { _, _ in changeEnabled = nil; confirmSequence = false; sequenceID = UUID(); sequenceRunning = false; sequenceResults = [] }
        .onDisappear { sequenceID = UUID() }
        .confirmationDialog("Dispatch the five company focus areas?", isPresented: $confirmSequence, titleVisibility: .visible) {
            Button("Dispatch reviewed sequence") { Task { await dispatchSequence() } }
            Button("Cancel", role: .cancel) {}
        }
        .confirmationDialog("Change company loop cadence?", isPresented: Binding(get: { changeEnabled != nil }, set: { if !$0 { changeEnabled = nil } }), titleVisibility: .visible) {
            if let enabled = changeEnabled {
                Button(enabled ? "Resume company loop" : "Pause company loop") {
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
