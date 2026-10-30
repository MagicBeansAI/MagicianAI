import SwiftUI

/// Operator decisions use the existing proposal and canonical HITL endpoints.
struct WorkOperatorView: View {
    let agentID: String
    @StateObject private var overview = WorkResource()
    @StateObject private var proposals = WorkResource()
    @StateObject private var requests = WorkResource()
    @State private var confirmation: WorkJSON?
    @ObservedObject private var theme = ThemeManager.shared
    private var rows: [WorkJSON] { (proposals.value?["proposals"].array ?? []).filter { $0["source"].text == "harness:" + agentID } }
    private var briefings: [WorkJSON] {
        (requests.value?["requests"].array ?? []).filter { $0["context"]["owner_agent_id"].text == agentID && ($0["request_type"].text ?? "").hasPrefix("harness.notify_owner.") }
    }
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: overview)
            if let value = overview.value {
                WorkGroupCard {
                    DisclosureGroup("Playbook, state and execution evidence") { WorkRecordView(value: value) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.text.magnifyingglass"))
                }
            }
            VStack(alignment: .leading, spacing: 12) {
                WorkSectionHeader("Structural proposals")
                WorkResourceStatus(resource: proposals)
                if proposals.value != nil && rows.isEmpty { WorkGroupCard { WorkInfoRow(title: "No structural proposals.", systemImage: "tray") } }
                ForEach(rows, id: \.formatted) { proposal in
                    let status = proposal["status"].text ?? "unknown"
                    WorkHeaderCard(proposal["title"].text ?? proposal["id"].display, subtitle: proposal["id"].text, detail: proposal["summary"].text,
                                   chips: [WorkChipSpec(title: WorkRecordView.label(status), tone: .workStatusTone(status))]) {
                        if proposal["status"].text == "pending" || (proposal["status"].text == "approved" && (Int(proposal["applied_version"].display) ?? 0) < 1) {
                            ForEach(["approve", "defer", "reject"], id: \.self) { decision in
                                WorkButtonRow(title: WorkRecordView.label(decision), systemImage: decision == "approve" ? "checkmark.circle" : decision == "defer" ? "clock" : "xmark.circle",
                                              role: decision == "reject" ? .destructive : nil) {
                                    confirmation = .object(["id": proposal["id"], "decision": .string(decision), "kind": .string("proposal")])
                                }.disabled(proposals.busy)
                                WorkRowDivider()
                            }
                        }
                        DisclosureGroup("Proposal record") { WorkRecordView(value: proposal) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                    }
                }
            }
            VStack(alignment: .leading, spacing: 12) {
                WorkSectionHeader("Owner briefings and history")
                WorkResourceStatus(resource: requests)
                if requests.value != nil && briefings.isEmpty { WorkGroupCard { WorkInfoRow(title: "No owner briefings.", systemImage: "tray") } }
                ForEach(briefings, id: \.formatted) { request in
                    let status = request["status"].text ?? "pending"
                    WorkHeaderCard(request["title"].text ?? WorkRecordView.label(request["request_type"].display), subtitle: request["id"].text,
                                   chips: [WorkChipSpec(title: WorkRecordView.label(status), tone: .workStatusTone(status))]) {
                        if request["status"].text == "pending" || request["status"] == .null {
                            let options = request["options"].array?.isEmpty == false ? request["options"].array! : [.object(["id": .string("acknowledge"), "label": .string("Acknowledge")])]
                            ForEach(options, id: \.formatted) { option in
                                if let id = option["id"].text {
                                    WorkButtonRow(title: option["label"].text ?? id, systemImage: "checkmark.circle") {
                                        confirmation = .object(["id": request["id"], "decision": .string(id), "kind": .string("request")])
                                    }.disabled(requests.busy)
                                    WorkRowDivider()
                                }
                            }
                        }
                        DisclosureGroup("Briefing record") { WorkRecordView(value: request) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                    }
                }
                if let history = requests.value?["history"] {
                    WorkGroupCard {
                        DisclosureGroup("Previous responses") { WorkRecordView(value: history) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "clock.arrow.circlepath", value: WorkRecordView.summary(history)))
                    }
                }
            }
        }
        .navigationTitle("Operator view").navigationBarTitleDisplayMode(.inline)
        .task { reload() }.refreshable { reload() }
        .onChange(of: requests.scopeID) { _, _ in confirmation = nil }.onChange(of: proposals.scopeID) { _, _ in confirmation = nil }
        .confirmationDialog("Apply this reviewed decision?", isPresented: Binding(get: { confirmation != nil }, set: { if !$0 { confirmation = nil } }), titleVisibility: .visible) {
            if let action = confirmation, let id = action["id"].text, let decision = action["decision"].text {
                Button(WorkRecordView.label(decision)) {
                    confirmation = nil
                    Task {
                        if action["kind"].text == "proposal" {
                            if let result = await proposals.perform("/api/magician/v2/proposals/" + WorkAPIClient.segment(id) + "/resolve", body: .object(["decision": .string(decision), "channel": .string("ios")])) {
                                reload(); if result["application_error"] != .null { proposals.error = result["application_error"].display }
                            }
                        } else {
                            if await requests.perform("/api/magician/v2/hitl/" + WorkAPIClient.segment(id) + "/respond", body: .object(["source": .string("user_request"), "value": .object(["type": .string("choice"), "selected_id": .string(decision)]), "channel": .string("ios")])) != nil { reload() }
                        }
                    }
                }
            }
            Button("Cancel", role: .cancel) { confirmation = nil }
        }
    }
    private func reload() {
        overview.load("/api/magician/v2/agents/" + WorkAPIClient.segment(agentID) + "/harness-overview")
        proposals.load("/api/magician/v2/proposals")
        requests.load("/api/magician/v2/user-requests?owner_agent_id=" + WorkAPIClient.query(agentID) + "&include_history=true&history_limit=40")
    }
}
