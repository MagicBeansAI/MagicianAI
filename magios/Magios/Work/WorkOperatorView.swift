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
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: overview)
                if let value = overview.value { WorkCard { DisclosureGroup("Program, state and execution evidence") { WorkRecordView(value: value) } } }
                Text("Structural proposals").font(.themed(18, weight: .semibold))
                WorkResourceStatus(resource: proposals)
                ForEach(rows, id: \.formatted) { proposal in
                    WorkCard {
                        WorkRecordView(value: proposal)
                        if proposal["status"].text == "pending" || (proposal["status"].text == "approved" && (Int(proposal["applied_version"].display) ?? 0) < 1) {
                            ForEach(["approve", "defer", "reject"], id: \.self) { decision in
                                Button(WorkRecordView.label(decision)) { confirmation = .object(["id": proposal["id"], "decision": .string(decision), "kind": .string("proposal")]) }.frame(minHeight: 44).disabled(proposals.busy)
                            }
                        }
                    }
                }
                Text("Owner briefings and history").font(.themed(18, weight: .semibold))
                WorkResourceStatus(resource: requests)
                ForEach((requests.value?["requests"].array ?? []).filter { $0["context"]["owner_agent_id"].text == agentID && ($0["request_type"].text ?? "").hasPrefix("harness.notify_owner.") }, id: \.formatted) { request in
                    WorkCard {
                        WorkRecordView(value: request)
                        if request["status"].text == "pending" || request["status"] == .null {
                            let options = request["options"].array?.isEmpty == false ? request["options"].array! : [.object(["id": .string("acknowledge"), "label": .string("Acknowledge")])]
                            ForEach(options, id: \.formatted) { option in
                                if let id = option["id"].text { Button(option["label"].text ?? id) { confirmation = .object(["id": request["id"], "decision": .string(id), "kind": .string("request")]) }.frame(minHeight: 44).disabled(requests.busy) }
                            }
                        }
                    }
                }
                if let history = requests.value?["history"] { WorkCard { DisclosureGroup("Previous responses") { WorkRecordView(value: history) } } }
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Operator view")
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
