import SwiftUI

/// Advanced policy controls retain the web's frozen preview/version protocol.
/// The ordinary New work form stays independent of these optional settings.
enum WorkPolicyKind: String, Identifiable {
    case capture, learning
    var id: String { rawValue }
    var endpoint: String { self == .capture ? "campaigns" : "guidance-policies" }
    var rows: String { self == .capture ? "campaigns" : "policies" }
    var title: String { self == .capture ? "Capture policies" : "Learning policies" }
    var initialSpec: WorkJSON {
        let source = self == .capture ? """
        {"name":"","goal_id":null,"sender_identity":"","channel":"","message_kind":"","targets":[],"allowed_contract_hashes":[],"content_contract_hash":"","content_template":null,"maximum_acts":1,"maximum_acts_per_day":1,"maximum_acts_per_recipient":1,"expires_at":"","approval":{"source_document":"","always_review":true,"important_identity_ids":[],"allow_automatic_capture":false,"clean_approvals_before_automatic_capture":null,"audit_every":1}}
        """ : """
        {"name":"","candidate_id":"","agent_id":"","ui_thread_id":"","baseline_guidance":null,"training_run_ids":[],"evaluation_cases":[],"canary_cases":[],"evaluation_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"canary_requirements":{"metric":"weighted_quality","minimum_mean_gain":0,"maximum_case_regression":0,"minimum_candidate_acceptance_rate":1,"maximum_acceptance_regressions":0},"maximum_evaluation_cost_microusd":0,"maximum_canary_cost_microusd":0,"maximum_guidance_bytes":16384,"targets":[],"maximum_adopted_runs":1,"disable_after_regressions":1,"unknown_monitor_disables_adoption":true,"monitor_nonacceptance_is_regression":true,"expires_at":""}
        """
        return (try? WorkJSON.parseObject(source)) ?? .object([:])
    }
}

struct WorkPoliciesView: View {
    let kind: WorkPolicyKind
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @State private var search = ""
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { "/api/magician/v2/relentless/\(kind.endpoint)?limit=10&offset=\(page * 10)&q=\(WorkAPIClient.query(search))" }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                NavigationLink("New \(kind == .capture ? "capture" : "qualification") policy") { WorkPolicyEditor(kind: kind) }.frame(minHeight: 44)
                WorkResourceStatus(resource: resource)
                ForEach(resource.value?[kind.rows].array ?? [], id: \.formatted) { row in
                    let policy = kind == .capture ? row["state"]["campaign"] : row["policy"]
                    NavigationLink { WorkPolicyDetail(kind: kind, row: row) } label: {
                        WorkCard {
                            Text(policy["spec"]["name"].text ?? policy["id"].display).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor)
                            Text(row["status"].text ?? (policy["revoked_at"] == .null ? "Active capture policy" : "Revoked")).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            Text("Expires \(policy["spec"]["expires_at"].display)").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        }
                    }.buttonStyle(.plain)
                }
                if let identities = resource.value?["identities"] { WorkCard { DisclosureGroup("Counterparty identities") { WorkRecordView(value: identities) } } }
                HStack {
                    Button("Previous") { page -= 1 }.disabled(page == 0 || resource.loading)
                    Spacer(); Text("Page \(page + 1)").font(.themed(12)); Spacer()
                    Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true || resource.loading)
                }.frame(minHeight: 44)
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle(kind.title)
        .searchable(text: $search, prompt: "Find policies").onChange(of: search) { _, _ in page = 0 }
        .task(id: path) { resource.load(path) }.refreshable { resource.refresh() }
    }
}

struct WorkPolicyEditor: View {
    let kind: WorkPolicyKind
    var previous: WorkJSON? = nil
    @StateObject private var resource = WorkResource()
    @State private var editor = ""
    @State private var policyID = UUID().uuidString
    @State private var preview: WorkJSON?
    @State private var reviewedEditor = ""
    @State private var saved = false
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { "/api/magician/v2/relentless/" + kind.endpoint }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(kind == .capture ? "Captures are local records. Use exact task, contract and identity references, finite caps and an expiry." : "Choose a procedural candidate, fresh evaluation and canary cases, exact recurring tasks, finite budgets and an expiry.").font(.themed(14)).foregroundColor(theme.secondaryTextColor)
                TextEditor(text: $editor).font(.themed(13)).frame(minHeight: 360).scrollContentBackground(.hidden).background(theme.cardColor)
                    .accessibilityIdentifier("work-policy-editor")
                WorkResourceStatus(resource: resource)
                Button("Review frozen policy") { Task { await review() } }.disabled(resource.busy || saved).frame(minHeight: 44)
                if let preview, editor == reviewedEditor {
                    WorkCard { WorkRecordView(value: preview) }
                    Button("Approve exact policy") { Task { await save(preview) } }.disabled(resource.busy || saved).frame(minHeight: 44)
                }
                if saved { Text("Policy saved. Return to the policy list and refresh.").font(.themed(14)) }
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Review policy")
        .task {
            if editor.isEmpty {
                editor = (previous?["spec"] ?? kind.initialSpec).formatted
                if let id = previous?["id"].text { policyID = id }
            }
            resource.load(path + "?limit=1&offset=0")
        }
        .onChange(of: editor) { _, next in if next != reviewedEditor { preview = nil; reviewedEditor = "" } }
        .onChange(of: resource.scopeID) { _, _ in preview = nil; reviewedEditor = "" }
    }
    private func review() async {
        do {
            let original = editor, spec = try WorkJSON.parseObject(original)
            var body: [String: WorkJSON] = ["spec": spec]
            if kind == .learning { body["policy_id"] = .string(policyID) }
            if let result = await resource.perform(path + "/preview", body: .object(body)), editor == original {
                if kind == .capture { editor = result["spec"].formatted; reviewedEditor = editor; preview = result }
                else { reviewedEditor = original; preview = result }
            }
        } catch { resource.error = "Enter a valid policy object before review." }
    }
    private func save(_ preview: WorkJSON) async {
        guard editor == reviewedEditor, let spec = try? WorkJSON.parseObject(editor) else { return }
        let body: WorkJSON = kind == .capture ? .object(["spec": spec, "expected_version": previous?["version_hash"] ?? .null]) : .object(["spec": spec, "expected_version_hash": preview["policy"]["version_hash"]])
        if await resource.perform(path + "/" + WorkAPIClient.segment(policyID), body: body) != nil { saved = true }
    }
}

struct WorkPolicyDetail: View {
    let kind: WorkPolicyKind
    let row: WorkJSON
    var recordID: String? = nil
    @State private var page = 0
    @StateObject private var resource = WorkResource()
    @State private var confirm: WorkJSON?
    @ObservedObject private var theme = ThemeManager.shared
    private var policyID: String { recordID ?? (kind == .capture ? row["state"]["campaign"] : row["policy"])["id"].text ?? "" }
    private var currentRow: WorkJSON? {
        if kind == .learning { return resource.value }
        return resource.value?["campaigns"].array?.first { $0["state"]["campaign"]["id"].text == policyID }
    }
    private var policy: WorkJSON { kind == .capture ? currentRow?["state"]["campaign"] ?? .null : currentRow?["policy"] ?? .null }
    private var path: String { "/api/magician/v2/relentless/" + kind.endpoint + "/" + WorkAPIClient.segment(policyID) }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if !resource.loading, resource.value != nil, currentRow == nil, resource.value?["pagination"]["has_more"].flag != true {
                    Text("Requested policy is not available.").font(.themed(14)).foregroundColor(theme.secondaryTextColor)
                }
                if let currentRow {
                WorkCard { WorkRecordView(value: currentRow) }
                if kind == .capture {
                    NavigationLink("Review successor policy") { WorkPolicyEditor(kind: kind, previous: policy) }.frame(minHeight: 44)
                    NavigationLink("Captures and exact review") { WorkCapturesView(campaignID: policy["id"].text ?? "") }.frame(minHeight: 44)
                }
                Button("Revoke future \(kind == .capture ? "captures" : "guidance adoption")", role: .destructive) { confirm = .object(["action": .string("revoke"), "version": policy["version_hash"]]) }.disabled(resource.busy || currentRow["status"].text == "revoked" || (kind == .capture && policy["revoked_at"] != .null)).frame(minHeight: 44)
                if kind == .learning && currentRow["status"].text == "paused" { Button("Resume exact qualification") { confirm = .object(["action": .string("resume"), "version": policy["version_hash"]]) }.frame(minHeight: 44).disabled(resource.busy || currentRow["expired"].flag == true) }
                ForEach(policy["spec"]["targets"].array ?? [], id: \.formatted) { target in
                    if let id = target["task_id"].text { NavigationLink("Task \(id)") { TaskDetailScreen(route: .init(taskId: id)) }.frame(minHeight: 44) }
                }
                if kind == .learning {
                    WorkGuidanceRunLinks(row: currentRow, path: path)
                }
                }
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle(policy["spec"]["name"].text ?? kind.title)
        .task(id: "\(policyID)|\(page)") { resource.load(kind == .learning ? path : "/api/magician/v2/relentless/campaigns?limit=10&offset=\(page * 10)&q=" + WorkAPIClient.query(policyID)) }
        .onChange(of: resource.value) { _, _ in if kind == .capture, currentRow == nil, resource.value?["pagination"]["has_more"].flag == true { page += 1 } }
        .refreshable { resource.refresh() }
        .onChange(of: resource.scopeID) { _, _ in confirm = nil }
        .confirmationDialog("Apply this policy change?", isPresented: Binding(get: { confirm != nil }, set: { if !$0 { confirm = nil } }), titleVisibility: .visible) {
            if let change = confirm, let action = change["action"].text {
                Button(action == "revoke" ? "Revoke future use" : "Resume qualification") {
                    confirm = nil
                    Task {
                        let body: WorkJSON = kind == .capture ? .object(["expected_version": change["version"], "spec": .null]) : .object(["expected_version_hash": change["version"]])
                        if await resource.perform(kind == .capture ? path : path + "/" + action, body: body) != nil { resource.refresh() }
                    }
                }
            }
            Button("Cancel", role: .cancel) { confirm = nil }
        }
    }
}

/// Trials without a result and adopted runs remain inspectable; sealed evidence
/// is opened only against the hash recorded by the policy, including monitoring.
private struct WorkGuidanceRunLinks: View {
    let row: WorkJSON
    let path: String
    private var trials: [String] {
        Set((row["reservations"].object ?? [:]).values.compactMap { $0["run_id"].text })
            .union((row["observations"].object ?? [:]).keys).sorted()
    }
    private var adopted: [String] {
        Set((row["adoptions"].object ?? [:]).values.compactMap { $0["run_id"].text })
            .union((row["monitored"].object ?? [:]).keys).sorted()
    }
    var body: some View {
        ForEach(trials, id: \.self) { runID in
            NavigationLink("Trial \(runID)") { WorkQualityRunDetail(runID: runID) }.frame(minHeight: 44)
            if let hash = row["observations"][runID]["evidence_hash"].text {
                NavigationLink("Sealed trial evidence") { WorkSealedEvidence(path: path + "/evidence/" + WorkAPIClient.segment(runID), expectedHash: hash) }.frame(minHeight: 44)
            }
        }
        ForEach(adopted, id: \.self) { runID in
            NavigationLink("Adopted run \(runID)") { WorkQualityRunDetail(runID: runID) }.frame(minHeight: 44)
            if let hash = row["monitored"][runID]["evidence_hash"].text {
                NavigationLink("Sealed monitoring evidence") { WorkSealedEvidence(path: path + "/evidence/" + WorkAPIClient.segment(runID), expectedHash: hash) }.frame(minHeight: 44)
            }
        }
    }
}

struct WorkCapturesView: View {
    let campaignID: String
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @State private var status = "awaiting_review"
    @State private var reviewed = Set<String>()
    @ObservedObject private var theme = ThemeManager.shared
    private var root: String { "/api/magician/v2/relentless/campaigns/" + WorkAPIClient.segment(campaignID) + "/captures" }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                Picker("Capture status", selection: $status) { ForEach(["awaiting_review", "all", "captured", "rejected"], id: \.self) { Text(WorkRecordView.label($0)).tag($0) } }
                WorkResourceStatus(resource: resource)
                ForEach(resource.value?["captures"].array ?? [], id: \.formatted) { capture in
                    WorkCard {
                        WorkRecordView(value: capture)
                        let hash = capture["binding"]["artifact_hash"].text ?? ""
                        NavigationLink("Inspect bound artifact") { WorkQualityArtifact(runID: capture["binding"]["run_id"].text ?? "", hash: hash, onReviewed: { reviewed.insert($0) }) }.frame(minHeight: 44)
                        if capture["status"].text == "awaiting_review" {
                            Button("Approve exact capture") { act(capture, approved: true) }.disabled(!reviewed.contains(hash) || resource.busy).frame(minHeight: 44)
                            Button("Reject capture", role: .destructive) { act(capture, approved: false) }.disabled(resource.busy).frame(minHeight: 44)
                        }
                    }
                }
                HStack { Button("Previous") { page -= 1 }.disabled(page == 0); Spacer(); Text("Page \(page + 1)"); Spacer(); Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true) }.frame(minHeight: 44)
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Captures")
        .task(id: "\(page)|\(status)") { resource.load(root + "?limit=20&offset=\(page * 20)&status=\(status)") }
        .onChange(of: status) { _, _ in page = 0 }.onChange(of: resource.scopeID) { _, _ in reviewed = [] }
    }
    private func act(_ capture: WorkJSON, approved: Bool) {
        guard let id = capture["id"].text, let hash = capture["binding_hash"].text else { return }
        Task { if await resource.perform(root + "/" + WorkAPIClient.segment(id) + "/review", body: .object(["binding_hash": .string(hash), "approved": .bool(approved)])) != nil { resource.refresh() } }
    }
}

struct WorkSealedEvidence: View {
    let path: String
    let expectedHash: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView { VStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            if let evidence = resource.value {
                if evidence["sealed"].flag == true && evidence["content_hash"].text == expectedHash { WorkRecordView(value: evidence) }
                else { Text("Evidence changed or is not sealed. Refresh the recorded policy result.").foregroundColor(theme.warningColor) }
            }
        }.padding(16) }.background(theme.backgroundColor).foregroundColor(theme.textColor).navigationTitle("Sealed evidence").task { resource.load(path) }
    }
}
