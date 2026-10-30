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
        WorkPage {
            WorkSearchField("Find policies", text: $search)
            WorkGroupCard {
                NavigationLink { WorkPolicyEditor(kind: kind) } label: {
                    WorkRowLabel(title: "New \(kind == .capture ? "capture" : "qualification") policy", systemImage: "plus.circle", tint: theme.accentColor)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New \(kind == .capture ? "capture" : "qualification") policy")
            }
            WorkResourceStatus(resource: resource)
            if let rows = resource.value?[kind.rows].array {
                VStack(alignment: .leading, spacing: 8) {
                    WorkGroupCard(kind.title) {
                        if rows.isEmpty { WorkEmptyState(title: "No \(kind == .capture ? "capture" : "learning") policies yet.", systemImage: "doc.badge.gearshape") }
                        ForEach(Array(rows.enumerated()), id: \.element.formatted) { index, row in
                            let policy = kind == .capture ? row["state"]["campaign"] : row["policy"]
                            let status = row["status"].text ?? (policy["revoked_at"] == .null ? "Active capture policy" : "Revoked")
                            if index > 0 { WorkRowDivider() }
                            NavigationLink { WorkPolicyDetail(kind: kind, row: row) } label: {
                                WorkItemLabel(title: policy["spec"]["name"].text ?? policy["id"].display,
                                              subtitle: "Expires \(policy["spec"]["expires_at"].display)",
                                              systemImage: kind == .capture ? "tray.and.arrow.down" : "graduationcap",
                                              chips: [WorkChipSpec(title: WorkRecordView.label(status), tone: .workStatusTone(row["status"].text ?? (policy["revoked_at"] == .null ? "active" : "revoked")))])
                            }.buttonStyle(.plain)
                        }
                    }
                    WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true, disabled: resource.loading)
                }
            }
            if let identities = resource.value?["identities"] {
                WorkGroupCard {
                    DisclosureGroup("Counterparty identities") { WorkRecordView(value: identities) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "person.2", value: WorkRecordView.summary(identities)))
                }
            }
        }
        .navigationTitle(kind.title).navigationBarTitleDisplayMode(.inline)
        .onChange(of: search) { _, _ in page = 0 }
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
        WorkPage {
            WorkNote(kind == .capture ? "Captures are local records. Use exact task, contract and identity references, finite caps and an expiry." : "Choose a procedural candidate, fresh evaluation and canary cases, exact recurring tasks, finite budgets and an expiry.")
                .padding(.horizontal, WorkMetrics.cardInset)
            VStack(alignment: .leading, spacing: 8) {
                WorkSectionHeader("Policy JSON")
                TextEditor(text: $editor).font(.themedMono(13)).frame(minHeight: 360).scrollContentBackground(.hidden)
                    .autocorrectionDisabled().textInputAutocapitalization(.never)
                    .padding(8)
                    .background(theme.controlColor, in: RoundedRectangle(cornerRadius: WorkMetrics.cardRadius, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: WorkMetrics.cardRadius, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
                    .accessibilityIdentifier("work-policy-editor")
            }
            WorkResourceStatus(resource: resource)
            WorkGroupCard(footer: saved ? "Policy saved. Return to the policy list and refresh." : nil) {
                WorkButtonRow(title: "Review frozen policy", systemImage: "checkmark.shield") { Task { await review() } }.disabled(resource.busy || saved)
            }
            if let preview, editor == reviewedEditor {
                WorkGroupCard("Frozen preview") {
                    WorkRecordView(value: preview)
                    WorkRowDivider()
                    WorkButtonRow(title: "Approve exact policy", systemImage: "checkmark.seal") { Task { await save(preview) } }.disabled(resource.busy || saved)
                }
            }
        }
        .navigationTitle("Review policy").navigationBarTitleDisplayMode(.inline)
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
        WorkPage {
                WorkResourceStatus(resource: resource)
                if !resource.loading, resource.value != nil, currentRow == nil, resource.value?["pagination"]["has_more"].flag != true {
                    WorkGroupCard { WorkEmptyState(title: "Requested policy is not available.", systemImage: "questionmark.folder") }
                }
                if let currentRow {
                    let status = currentRow["status"].text ?? (policy["revoked_at"] == .null ? "active" : "revoked")
                    WorkHeaderCard(policy["spec"]["name"].text ?? kind.title, subtitle: policy["id"].text,
                                   detail: policy["spec"]["expires_at"].text.map { "Expires " + $0 },
                                   chips: [WorkChipSpec(title: WorkRecordView.label(status), tone: .workStatusTone(status))]) {
                        if kind == .capture {
                            WorkNavigationRow(title: "Review successor policy", systemImage: "square.and.pencil") { WorkPolicyEditor(kind: kind, previous: policy) }
                            WorkRowDivider()
                            WorkNavigationRow(title: "Captures and exact review", systemImage: "tray.full") { WorkCapturesView(campaignID: policy["id"].text ?? "") }
                            WorkRowDivider()
                        }
                        if kind == .learning && currentRow["status"].text == "paused" {
                            WorkButtonRow(title: "Resume exact qualification", systemImage: "play.circle") { confirm = .object(["action": .string("resume"), "version": policy["version_hash"]]) }
                                .disabled(resource.busy || currentRow["expired"].flag == true)
                            WorkRowDivider()
                        }
                        WorkButtonRow(title: "Revoke future \(kind == .capture ? "captures" : "guidance adoption")", systemImage: "nosign", role: .destructive) { confirm = .object(["action": .string("revoke"), "version": policy["version_hash"]]) }
                            .disabled(resource.busy || currentRow["status"].text == "revoked" || (kind == .capture && policy["revoked_at"] != .null))
                    }
                    let targets = (policy["spec"]["targets"].array ?? []).compactMap { $0["task_id"].text }
                    if !targets.isEmpty {
                        WorkGroupCard("Targets") {
                            ForEach(Array(targets.enumerated()), id: \.offset) { index, id in
                                if index > 0 { WorkRowDivider() }
                                WorkNavigationRow(title: "Task \(id)", systemImage: "checklist") { TaskDetailScreen(route: .init(taskId: id)) }
                            }
                        }
                    }
                    if kind == .learning {
                        WorkGroupCard("Runs and evidence") { WorkGuidanceRunLinks(row: currentRow, path: path) }
                    }
                    WorkGroupCard("Policy record") { WorkRecordView(value: currentRow) }
                }
        }
        .navigationTitle(policy["spec"]["name"].text ?? kind.title).navigationBarTitleDisplayMode(.inline)
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
    private var rows: [(title: String, icon: String, run: String, evidence: (String, String)?)] {
        trials.map { ("Trial \($0)", "flask", $0, row["observations"][$0]["evidence_hash"].text.map { ("Sealed trial evidence", $0) }) }
            + adopted.map { ("Adopted run \($0)", "checkmark.seal", $0, row["monitored"][$0]["evidence_hash"].text.map { ("Sealed monitoring evidence", $0) }) }
    }
    var body: some View {
        if rows.isEmpty { WorkInfoRow(title: "No trials or adopted runs yet.", systemImage: "hourglass") }
        ForEach(Array(rows.enumerated()), id: \.offset) { index, entry in
            if index > 0 { WorkRowDivider() }
            WorkNavigationRow(title: entry.title, systemImage: entry.icon) { WorkQualityRunDetail(runID: entry.run) }
            if let evidence = entry.evidence {
                WorkRowDivider()
                WorkNavigationRow(title: evidence.0, systemImage: "lock.doc") { WorkSealedEvidence(path: path + "/evidence/" + WorkAPIClient.segment(entry.run), expectedHash: evidence.1) }
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
        WorkPage {
            WorkGroupCard {
                WorkFormRow("Capture status") {
                    Picker("Capture status", selection: $status) { ForEach(["awaiting_review", "all", "captured", "rejected"], id: \.self) { Text(WorkRecordView.label($0)).tag($0) } }
                        .labelsHidden().pickerStyle(.menu).font(.themed(15))
                }
            }
            WorkResourceStatus(resource: resource)
            if resource.value != nil && resource.value?["captures"].array?.isEmpty != false {
                WorkGroupCard { WorkEmptyState(title: "No captures in this view.", systemImage: "tray") }
            }
            ForEach(resource.value?["captures"].array ?? [], id: \.formatted) { capture in
                let hash = capture["binding"]["artifact_hash"].text ?? ""
                let state = capture["status"].text ?? "unknown"
                WorkHeaderCard(capture["id"].text ?? "Capture", subtitle: capture["binding"]["identity_id"].text.map { "Identity " + $0 },
                               chips: [WorkChipSpec(title: WorkRecordView.label(state), tone: .workStatusTone(state))]) {
                    WorkNavigationRow(title: "Inspect bound artifact", systemImage: "doc.text.magnifyingglass", value: reviewed.contains(hash) ? "Reviewed" : nil) {
                        WorkQualityArtifact(runID: capture["binding"]["run_id"].text ?? "", hash: hash, onReviewed: { reviewed.insert($0) })
                    }
                    if state == "awaiting_review" {
                        WorkRowDivider()
                        WorkButtonRow(title: "Approve exact capture", systemImage: "checkmark.circle") { act(capture, approved: true) }.disabled(!reviewed.contains(hash) || resource.busy)
                        WorkRowDivider()
                        WorkButtonRow(title: "Reject capture", systemImage: "xmark.circle", role: .destructive) { act(capture, approved: false) }.disabled(resource.busy)
                    }
                    WorkRowDivider()
                    DisclosureGroup("Capture record") { WorkRecordView(value: capture) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                }
            }
            WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true)
        }
        .navigationTitle("Captures").navigationBarTitleDisplayMode(.inline)
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
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let evidence = resource.value {
                if evidence["sealed"].flag == true && evidence["content_hash"].text == expectedHash {
                    WorkHeaderCard("Sealed evidence", subtitle: expectedHash, chips: [WorkChipSpec(title: "Sealed", systemImage: "lock.fill", tone: theme.successColor)])
                    WorkGroupCard("Evidence") { WorkRecordView(value: evidence) }
                } else {
                    WorkGroupCard {
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Image(systemName: "exclamationmark.triangle.fill").foregroundColor(theme.warningColor).accessibilityHidden(true)
                            Text("Evidence changed or is not sealed. Refresh the recorded policy result.").font(.themed(15)).foregroundColor(theme.textColor)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .padding(.vertical, 12)
                    }
                }
            }
        }
        .navigationTitle("Sealed evidence").navigationBarTitleDisplayMode(.inline).task { resource.load(path) }
    }
}
