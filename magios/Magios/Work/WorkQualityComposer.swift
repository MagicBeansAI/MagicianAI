import SwiftUI
import UniformTypeIdentifiers

/// The native form produces the existing Relentless contract. Preview is the
/// server's authority; a form edit invalidates the reviewed hash immediately.
struct WorkSourceReference: Identifiable, Equatable {
    let id: String
    let title: String
    let origin: String
    let content: String
    var source: String { "Reference: \(title)\nOrigin: \(origin)\n\n\(content)" }
}

struct WorkQualityDraft: Equatable {
    var goal = "", source = "", rubric = "", filename = "draft.md"
    var actor = "", critic = ""
    var maxAttempts = 5, minutes = 30, dollars = 40
    var references: [WorkSourceReference] = []
    func proposal() -> WorkJSON {
        var sources = references.reduce(into: [String: WorkJSON]()) { $0[$1.id] = .string($1.source) }
        if !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { sources["brief"] = .string(source) }
        return .object([
            "goal": .string(goal), "artifact_file": .string(filename), "maximum_artifact_bytes": .integer(65536),
            "human_review": .string("required"), "handoff_kind": .string("internal_draft"),
            "actor_profile": .string(actor), "critic_profiles": .array(Array(repeating: .string(critic), count: 3)),
            "sources": .object(sources),
            "criteria": .array([
                .object(["id": .string("readable_artifact"), "specification": .object(["kind": .string("check"), "assertions": .array([
                    .object(["kind": .string("utf8"), "file": .string(filename)]),
                    .object(["kind": .string("byte_length"), "file": .string(filename), "minimum": .integer(1), "maximum": .integer(65536)])
                ])]), "weight": .null, "must_pass": .bool(true), "pass_score": .null]),
                .object(["id": .string("grounding"), "specification": .object(["kind": .string("grounding"), "instructions": .string("Review all factual claims against the sealed source notes. Unsupported claims must be removed or corrected; uncertainty must remain visible.")]), "weight": .null, "must_pass": .bool(true), "pass_score": .integer(1)]),
                .object(["id": .string("quality"), "specification": .object(["kind": .string("rubric"), "instructions": .string(rubric), "anchors": .array([
                    .object(["score": .integer(0), "description": .string("Does not satisfy the requested outcome; major omissions or unusable output.")]),
                    .object(["score": .number(0.5), "description": .string("Useful draft with substantive gaps against the stated requirements.")]),
                    .object(["score": .integer(1), "description": .string("Fully satisfies the stated requirements; complete and ready for the intended local review.")])
                ])]), "weight": .integer(1), "must_pass": .bool(false), "pass_score": .null])
            ]),
            "score_at_least": .number(0.8),
            "review": .object(["panel_size": .integer(3), "score_quorum": .integer(3), "min_confidence": .number(0.8), "max_score_spread": .number(0.2), "noise_margin": .number(0.02), "max_retries_per_critic": .integer(1), "unknown_review": .string("revise_supported_defects")]),
            "budget": .object(["max_attempts": .integer(Int64(maxAttempts)), "max_wall_clock_secs": .integer(Int64(minutes * 60)), "max_cost_microusd": .integer(Int64(dollars * 1_000_000)), "plateau_attempts": .integer(3)])
        ])
    }
}

struct WorkQualityComposer: View {
    var taskID: String? = nil
    var agentID: String? = nil
    var proposed: WorkJSON? = nil
    @StateObject private var resource = WorkResource()
    @StateObject private var context = WorkResource()
    @StateObject private var crew = WorkResource()
    @State private var referencesPending = false
    @State private var useSourceTask = false
    @State private var draft = WorkQualityDraft()
    @State private var agent = ""
    @State private var thread = "general"
    @State private var advanced = ""
    @State private var captureTarget = ""
    @State private var preview: WorkJSON?
    @State private var reviewedSignature = ""
    @State private var runID = UUID().uuidString
    @State private var policyID = UUID().uuidString
    @State private var policyRuns = 1
    @State private var policyExpiry = Date().addingTimeInterval(7 * 86400)
    @State private var startedRun: String?
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    private var proposal: WorkJSON? {
        if let proposed { return proposed["proposal"] }
        if !advanced.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return try? WorkJSON.parseObject(advanced) }
        var value = draft.proposal().object ?? [:]
        if !captureTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { value["handoff_kind"] = .string("capture") }
        return .object(value)
    }
    private var signature: String { "\(proposal?.formatted ?? advanced)|\(agent)|\(thread)|\(context.value?.formatted ?? "")|\(captureTarget)|\(useSourceTask)" }
    private var profiles: [WorkJSON] { resource.value?["profiles"].array ?? [] }
    private var effectiveTaskID: String? { useSourceTask ? taskID : nil }
    private var canAttachProposal: Bool {
        guard let proposed else { return true }
        return proposed["proposal"]["native_harness"].object != nil &&
            context.value?["goal"] == proposed["proposal"]["goal"] && context.value?["agent_id"] == proposed["origin"]["agent_id"]
    }
    private var canReview: Bool {
        !referencesPending && proposal?.object != nil && !agent.isEmpty && !thread.isEmpty &&
        (effectiveTaskID?.isEmpty != false || (context.value?["task_id"].text == effectiveTaskID && context.value?["can_start"].flag == true && canAttachProposal)) &&
        (proposed != nil || !advanced.isEmpty || (!draft.goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !draft.rubric.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !draft.actor.isEmpty && !draft.critic.isEmpty))
    }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                WorkResourceStatus(resource: resource)
                if taskID?.isEmpty == false {
                    WorkCard {
                        Toggle("Use the existing task", isOn: $useSourceTask).disabled(resource.busy || !canAttachProposal)
                        Text(useSourceTask ? "Quality uses the reviewed task context and its owning member." : "Creates new work; the proposal origin remains attached as evidence.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    }
                    WorkResourceStatus(resource: context)
                    WorkCard {
                        Text(useSourceTask ? "Attached to task" : "Source task").font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                        Text(context.value?["title"].text ?? taskID ?? "").font(.themed(16))
                        if let reason = context.value?["reason"].text { Text(reason).font(.themed(13)).foregroundColor(theme.warningColor) }
                    }
                }
                if let proposed {
                    WorkCard { Text("Owner proposal").font(.themed(18, weight: .semibold)); WorkRecordView(value: proposed) }
                } else {
                    WorkCard {
                        field("Outcome", text: $draft.goal)
                        field("Source notes", text: $draft.source, multiline: true)
                        WorkReferencePicker(references: $draft.references, pending: $referencesPending, disabled: !advanced.isEmpty || resource.busy)
                        field("Quality requirements", text: $draft.rubric, multiline: true)
                        Picker("Actor", selection: $draft.actor) { profileOptions }
                        Picker("Critic", selection: $draft.critic) { profileOptions }
                        Stepper("Attempts: \(draft.maxAttempts)", value: $draft.maxAttempts, in: 1...100)
                        Stepper("Time limit: \(draft.minutes) minutes", value: $draft.minutes, in: 1...1440)
                        Stepper("Cost ceiling: $\(draft.dollars)", value: $draft.dollars, in: 1...10000)
                    }
                }
                WorkCard {
                    WorkResourceStatus(resource: crew)
                    Picker("Crew member", selection: $agent) {
                        Text("Choose a member").tag("")
                        if !agent.isEmpty && !(crew.value?["agents"].array ?? []).contains(where: { $0["definition"]["agent_id"].text == agent }) { Text(agent).tag(agent) }
                        ForEach(crew.value?["agents"].array ?? [], id: \.formatted) { row in
                            if let id = row["definition"]["agent_id"].text { Text(row["definition"]["name"].text ?? id).tag(id) }
                        }
                    }.disabled(useSourceTask)
                    field("Thread", text: $thread)
                    DisclosureGroup("Optional capture handoff") {
                        Text("Use an exact campaign, version and identity target from a reviewed capture policy. The contract must request a capture handoff.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        field("Capture target JSON", text: $captureTarget, multiline: true)
                    }
                    if proposed == nil {
                        DisclosureGroup("Advanced contract") {
                            Text("Review a complete contract to configure references, artifact checks, task harness limits and policies.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            Button("Edit complete contract") { advanced = proposal?.formatted ?? "" }.frame(minHeight: 44)
                            if !advanced.isEmpty { TextEditor(text: $advanced).font(.themed(12)).frame(minHeight: 240).scrollContentBackground(.hidden).background(theme.cardColor) }
                        }
                    }
                    Button("Review contract") { Task { await review() } }.frame(minHeight: 44).disabled(resource.busy || !canReview)
                    if let preview, signature == reviewedSignature {
                        WorkRecordView(value: preview)
                        Text("Outputs remain local. Approval does not send or submit anything.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        if effectiveTaskID?.isEmpty == false {
                            Stepper("Maximum recurring runs: \(policyRuns)", value: $policyRuns, in: 1...100)
                            DatePicker("Policy expires", selection: $policyExpiry, in: Date()..., displayedComponents: [.date, .hourAndMinute])
                            Button("Approve bounded recurring policy") { Task { await configurePolicy(enabled: true) } }.frame(minHeight: 44).disabled(resource.busy || context.busy)
                        }
                        Button("Start reviewed work") { Task { await start(preview) } }.frame(minHeight: 44)
                            .disabled(resource.busy || preview["budget_can_admit_attempt"].flag != true)
                    }
                }
                if useSourceTask && context.value?["policy"] != nil && context.value?["policy"] != .null {
                    WorkCard {
                        Text("Current recurring policy").font(.themed(17, weight: .semibold))
                        WorkRecordView(value: context.value?["policy"] ?? .null)
                        Button("Disable future quality runs", role: .destructive) { Task { await configurePolicy(enabled: false) } }.frame(minHeight: 44).disabled(context.busy)
                    }
                }
                if let startedRun { NavigationLink("Open started run") { WorkQualityRunDetail(runID: startedRun) }.frame(minHeight: 44) }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor).tint(theme.accentColor)
        .navigationTitle("Quality work").navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
        .task {
            agent = proposed?["origin"]["agent_id"].text ?? agentID ?? ""
            useSourceTask = proposed == nil && taskID?.isEmpty == false
            crew.load("/api/magician/v2/agents")
            if let taskID, !taskID.isEmpty { context.load("/api/magician/v2/relentless/task-context/" + WorkAPIClient.segment(taskID)) }
            resource.load("/api/magician/v2/relentless/profiles")
        }
        .onChange(of: context.value) { _, value in
            if useSourceTask, let id = value?["agent_id"].text { agent = id }
            if useSourceTask, let id = value?["ui_thread_id"].text { thread = id }
            if useSourceTask, captureTarget.isEmpty, let target = value?["policy"]["capture_handoff"], target != .null { captureTarget = target.formatted }
            if draft.goal.isEmpty { draft.goal = value?["goal"].text ?? "" }
        }
        .onChange(of: useSourceTask) { _, attached in
            if attached {
                agent = context.value?["agent_id"].text ?? agent
                thread = context.value?["ui_thread_id"].text ?? thread
                if captureTarget.isEmpty, let target = context.value?["policy"]["capture_handoff"], target != .null { captureTarget = target.formatted }
            } else if captureTarget == context.value?["policy"]["capture_handoff"].formatted {
                captureTarget = ""
            }
            preview = nil; reviewedSignature = ""
        }
        .onChange(of: crew.value) { _, value in
            if agent.isEmpty {
                let rows = value?["agents"].array ?? []
                agent = (rows.first { $0["definition"]["is_primary"].flag == true } ?? rows.first)?["definition"]["agent_id"].text ?? ""
            }
        }
        .onChange(of: resource.value) { _, profiles in
            if draft.actor.isEmpty { draft.actor = profiles?["actor_default"].text ?? "" }
            if draft.critic.isEmpty { draft.critic = profiles?["critic_default"].text ?? "" }
        }
        .onChange(of: signature) { _, _ in preview = nil; reviewedSignature = ""; runID = UUID().uuidString; policyID = UUID().uuidString }
        .onChange(of: policyRuns) { _, _ in policyID = UUID().uuidString }
        .onChange(of: policyExpiry) { _, _ in policyID = UUID().uuidString }
        .onChange(of: resource.scopeID) { _, _ in preview = nil; reviewedSignature = ""; startedRun = nil }
    }
    @ViewBuilder private var profileOptions: some View {
        Text("Choose profile").tag("")
        ForEach(profiles, id: \.formatted) { profile in
            if let name = profile["name"].text { Text(name + " · " + profile["family"].display).tag(name) }
        }
    }
    private func field(_ title: String, text: Binding<String>, multiline: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
            TextField(title, text: text, axis: multiline ? .vertical : .horizontal)
                .font(.themed(15)).lineLimit(multiline ? 3...8 : 1...1).padding(10)
                .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 9))
                .textInputAutocapitalization(.none)
        }
    }
    private func review() async {
        guard let proposal, canReview else { return }
        let original = signature
        if let result = await resource.perform("/api/magician/v2/relentless/preview", body: proposal), signature == original {
            preview = result; reviewedSignature = original
        }
    }
    private func configurePolicy(enabled: Bool) async {
        guard let taskID = effectiveTaskID, let contextHash = context.value?["context_hash"].text else { return }
        if enabled && !captureTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && (try? WorkJSON.parseObject(captureTarget)) == nil { resource.error = "Enter a valid capture target object."; return }
        if enabled && (preview == nil || signature != reviewedSignature || proposal == nil) { return }
        let body: WorkJSON = .object([
            "policy_id": .string(policyID), "expected_policy_id": context.value?["policy"]["id"] ?? .null,
            "expected_context_hash": .string(contextHash), "expected_contract_hash": preview?["contract_hash"] ?? .string(""),
            "proposal": enabled ? (proposal ?? .null) : .null, "max_runs": .integer(enabled ? Int64(policyRuns) : 0),
            "expires_at": .string(ISO8601DateFormatter().string(from: enabled ? policyExpiry : Date())), "capture_handoff": enabled ? ((try? WorkJSON.parseObject(captureTarget)) ?? .null) : .null
        ])
        if await context.perform("/api/magician/v2/relentless/task-policy/" + WorkAPIClient.segment(taskID), body: body) != nil {
            preview = nil; context.refresh(); policyID = UUID().uuidString
        }
    }
    private func start(_ preview: WorkJSON) async {
        guard signature == reviewedSignature, let proposal, let hash = preview["contract_hash"].text, canReview else { return }
        let pendingRun = effectiveTaskID == nil ? nil : context.value?["pending_run_id"].text
        var body: [String: WorkJSON] = ["run_id": .string(pendingRun ?? runID), "proposal": proposal, "agent_id": .string(agent), "ui_thread_id": .string(thread), "expected_contract_hash": .string(hash)]
        if let taskID = effectiveTaskID, !taskID.isEmpty, let contextHash = context.value?["context_hash"].text { body["existing_task"] = .object(["task_id": .string(taskID), "context_hash": .string(contextHash)]) }
        if !captureTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            guard let target = try? WorkJSON.parseObject(captureTarget) else { resource.error = "Enter a valid capture target object."; return }
            body["capture_handoff"] = target
        }
        if let proposed, let requestID = proposed["origin"]["request_id"].text { body["proposal_request_id"] = .string(requestID) }
        if let run = await resource.perform("/api/magician/v2/relentless/runs", body: .object(body)) { startedRun = run["id"].text; self.preview = nil }
    }
}

/// Search and file attachment freeze exact source text into the reviewed
/// contract. Search and reads use the same provider and connection snapshot.
private struct WorkReferencePicker: View {
    @Binding var references: [WorkSourceReference]
    @Binding var pending: Bool
    let disabled: Bool
    @StateObject private var resource = WorkResource()
    @State private var query = ""
    @State private var results: WorkJSON?
    @State private var importing = false
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("References · \(references.count) attached").font(.themed(16, weight: .semibold))
            Text("Their contents are frozen when you review the contract.").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            TextField("Find a reference", text: $query).textInputAutocapitalization(.never).padding(10).background(theme.controlColor, in: RoundedRectangle(cornerRadius: 8)).disabled(disabled || pending)
            Button("Search notes") { Task { await search() } }.frame(minHeight: 44).disabled(disabled || pending || query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || resource.value?["provider"].text?.isEmpty != false)
            Button("Attach text file") { importing = true }.frame(minHeight: 44).disabled(disabled || pending)
            WorkResourceStatus(resource: resource)
            if let results {
                ForEach(results["hits"].array ?? [], id: \.formatted) { hit in
                    Button(hit["title"].text ?? hit["relative_path"].display) { Task { await attach(hit) } }.frame(minHeight: 44).disabled(disabled || pending)
                    Text(hit["relative_path"].display).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
                if results["hits"].array?.isEmpty != false { Text("No matching references.").font(.themed(13)) }
                if results["more_available"].flag == true || results["scan_truncated"].flag == true { Text("More documents may be available. Narrow the search.").font(.themed(12)) }
            }
            ForEach(references) { reference in
                DisclosureGroup(reference.title) { Text(reference.origin).font(.themed(11)); Text(reference.content).font(.themed(13)).textSelection(.enabled) }
                Button("Remove " + reference.title, role: .destructive) { references.removeAll { $0.id == reference.id } }.frame(minHeight: 44).disabled(disabled || pending)
            }
        }.task { resource.load("/api/magician/v2/notes/tree") }
        .onChange(of: resource.scopeID) { _, _ in results = nil; query = ""; pending = false; references = [] }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.data]) { result in
            do {
                let url = try result.get()
                let accessed = url.startAccessingSecurityScopedResource(); defer { if accessed { url.stopAccessingSecurityScopedResource() } }
                let file = try FileHandle(forReadingFrom: url); defer { try? file.close() }
                let bytes = try file.read(upToCount: 1_048_577) ?? Data()
                guard bytes.count <= 1_048_576, let text = String(data: bytes, encoding: .utf8), !text.contains("\0") else { throw WorkAPIError.http(422, "Attach a UTF-8 text document up to 1 MB. Larger or binary sources use the complete contract.") }
                guard !disabled else { return }
                references.append(.init(id: "file:" + UUID().uuidString, title: url.lastPathComponent, origin: "Uploaded file: " + url.lastPathComponent, content: text))
            } catch { resource.error = error.localizedDescription }
        }
    }
    private func search() async {
        guard !disabled, !pending, let provider = resource.value?["provider"].text else { return }
        let requested = query.trimmingCharacters(in: .whitespacesAndNewlines)
        pending = true; defer { pending = false }
        if let response = await resource.perform("/api/magician/v2/notes/search", body: .object(["query": .string(requested), "provider": .string(provider), "limit": .integer(12)]), successNotice: nil), query.trimmingCharacters(in: .whitespacesAndNewlines) == requested { results = response }
    }
    private func attach(_ hit: WorkJSON) async {
        guard !disabled, !pending, let path = hit["relative_path"].text, let id = hit["source_ref"].text else { return }
        pending = true; defer { pending = false }
        if let note = await resource.perform("/api/magician/v2/notes/file?path=" + WorkAPIClient.query(path), method: "GET", body: nil) {
            guard !disabled, note["provider"] == hit["provider"], note["relative_path"].text == path, let content = note["markdown"].text else { resource.error = "The notes provider changed. Search and attach this reference again."; return }
            references.removeAll { $0.id == id }
            references.append(.init(id: id, title: note["title"].text ?? path, origin: id, content: content))
            results = nil; query = ""
        }
    }
}
