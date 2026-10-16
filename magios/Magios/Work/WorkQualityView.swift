import SwiftUI
import MarkdownUI

private let qualityAPI = "/api/magician/v2/relentless"

struct WorkQualityView: View {
    var taskID: String? = nil
    var agentID: String? = nil
    @StateObject private var resource = WorkResource()
    @StateObject private var crew = WorkResource()
    @State private var ownerOverride: String?
    @State private var statusFilter = "all"
    @State private var lane = "inbox"
    @State private var search = ""
    @State private var page = 0
    @State private var compose = false
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String {
        var parts = URLComponents()
        parts.queryItems = [.init(name: "view", value: lane), .init(name: "limit", value: "20"), .init(name: "offset", value: String(page * 20)), .init(name: "q", value: search)]
        let owner = ownerOverride ?? agentID ?? ""
        parts.queryItems?.append(.init(name: "status", value: statusFilter))
        if !owner.isEmpty { parts.queryItems?.append(.init(name: "agent_id", value: owner)) }
        return qualityAPI + "/catalog?" + (parts.percentEncodedQuery ?? "")
    }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                Picker("Quality view", selection: $lane) {
                    Text("Inbox").tag("inbox"); Text("Runs").tag("runs")
                }.pickerStyle(.segmented)
                DisclosureGroup("Filters") {
                    Picker(lane == "runs" ? "Run status" : "Review type", selection: $statusFilter) {
                        ForEach(lane == "runs" ? ["all", "active", "review", "attention", "complete", "stopped"] : ["all", "attention", "review", "brief"], id: \.self) { Text(WorkRecordView.label($0)).tag($0) }
                    }.pickerStyle(.menu)
                    Picker("Crew member", selection: Binding(get: { ownerOverride ?? agentID ?? "" }, set: { ownerOverride = $0 })) {
                        Text("All members").tag("")
                        let current = ownerOverride ?? agentID ?? ""
                        if !current.isEmpty { Text(current).tag(current) }
                        ForEach(crew.value?["agents"].array ?? [], id: \.formatted) { row in
                            if let id = row["definition"]["agent_id"].text, id != current { Text(row["definition"]["name"].text ?? id).tag(id) }
                        }
                    }.pickerStyle(.menu)
                    WorkResourceStatus(resource: crew)
                }.font(.themed(14))
                Button("New quality work", systemImage: "plus") { compose = true }.frame(minHeight: 44)
                if let taskID {
                    WorkCard {
                        NavigationLink("Task quality context") { WorkEndpointView(title: "Task quality", path: qualityAPI + "/task-context/" + WorkAPIClient.segment(taskID)) }.frame(minHeight: 44)
                        NavigationLink("Open full task") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }.frame(minHeight: 44)
                    }
                }
                WorkResourceStatus(resource: resource)
                if let items = resource.value?["items"].array {
                    if items.isEmpty { Text("No quality work in this view.").foregroundColor(theme.secondaryTextColor) }
                    ForEach(items, id: \.formatted) { entry in
                        let item = entry["item"]
                        if entry["kind"].text == "run", let id = item["id"].text {
                            NavigationLink { WorkQualityRunDetail(runID: id) } label: {
                                WorkCard {
                                    Text(item["goal"].text ?? id).font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
                                    Text(item["label"].text ?? item["status"].display).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                    Text("Attempts: \(item["attempt_count"].display) of \(item["max_attempts"].display)").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                }
                            }.buttonStyle(.plain)
                        } else if entry["kind"].text == "proposal", let id = item["origin"]["request_id"].text {
                            NavigationLink { WorkQualityProposalDetail(proposalID: id) } label: {
                                WorkCard {
                                    Text(item["goal"].text ?? "Owner proposal").font(.themed(16, weight: .semibold)).foregroundColor(theme.textColor)
                                    Text("Review proposed work · \(item["origin"]["agent_id"].display)").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                                }
                            }.buttonStyle(.plain)
                        } else { WorkCard { WorkRecordView(value: entry) } }
                    }
                    HStack {
                        Button("Previous") { page = max(0, page - 1) }.disabled(page == 0)
                        Spacer(); Text("Page \(page + 1)").font(.themed(12)); Spacer()
                        Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true)
                    }.frame(minHeight: 44)
                }
                WorkCard {
                    NavigationLink("Tasks and recurring quality") { WorkQualityTasks() }.frame(minHeight: 44)
                    NavigationLink("Capture policies") { WorkPoliciesView(kind: .capture) }.frame(minHeight: 44)
                    NavigationLink("Learning policies") { WorkPoliciesView(kind: .learning) }.frame(minHeight: 44)
                    NavigationLink("Learning proposals") { WorkGuidanceProposals() }.frame(minHeight: 44)
                    WorkRelatedTools(origin: WorkWebContext(section: .quality, agent: ownerOverride ?? agentID ?? "", task: taskID ?? ""), destinations: [.analytics, .history, .evals, .events, .budget])
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor)
        .searchable(text: $search, prompt: "Search quality work")
        .task { crew.load("/api/magician/v2/agents") }
        .task(id: path) { resource.load(path) }.refreshable { resource.refresh() }
        .onChange(of: lane) { _, _ in page = 0; statusFilter = "all" }
        .onChange(of: ownerOverride) { _, _ in page = 0 }
        .onChange(of: statusFilter) { _, _ in page = 0 }
        .onChange(of: search) { _, _ in page = 0 }
        .sheet(isPresented: $compose) { NavigationStack { WorkQualityComposer(taskID: taskID, agentID: (ownerOverride ?? agentID)?.isEmpty == false ? (ownerOverride ?? agentID) : nil) } }
    }
}

struct WorkQualityProposalDetail: View {
    let proposalID: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let proposed = resource.value {
                    WorkCard {
                        Text(proposed["proposal"]["goal"].text ?? "Proposed work").font(.themed(20, weight: .semibold))
                        WorkRecordView(value: proposed)
                        NavigationLink("Review and configure") {
                            WorkQualityComposer(taskID: proposed["origin"]["task_id"].text, agentID: proposed["origin"]["agent_id"].text, proposed: proposed)
                        }.frame(minHeight: 44)
                    }
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Proposal")
        .task { resource.load(qualityAPI + "/proposals/" + WorkAPIClient.segment(proposalID)) }
        .refreshable { resource.refresh() }
    }
}

struct WorkQualityRunDetail: View {
    let runID: String
    @StateObject private var resource = WorkResource()
    @State private var reviewedHash: String?
    @State private var cancel = false
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { qualityAPI + "/runs/" + WorkAPIClient.segment(runID) }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let run = resource.value {
                    WorkCard {
                        Text(run["contract"]["goal"].text ?? runID).font(.themed(21, weight: .semibold))
                        Text(WorkRecordView.label(run["status"].text ?? "unknown")).font(.themed(15, weight: .semibold))
                        if let reason = run["reason"].text { Text(reason).font(.themed(14)) }
                        WorkRecordView(value: .object(["spend": run["spend"], "budget": run["contract"]["budget"], "deadline": run["deadline_at"]]))
                        if let taskID = run["admission"]["task_id"].text {
                            NavigationLink("Owning task") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }.frame(minHeight: 44)
                        }
                        if !["accepted", "plateau", "budget_exhausted", "cancelled", "failed"].contains(run["status"].text ?? "") {
                            Button("Cancel quality run", role: .destructive) { cancel = true }.frame(minHeight: 44).disabled(resource.busy)
                        } else {
                            Button("Reconcile available cost receipts") { act("reconcile-costs") }.frame(minHeight: 44).disabled(resource.busy)
                        }
                    }
                    WorkCard {
                        Text("Attempts and exact outputs").font(.themed(17, weight: .semibold))
                        ForEach(run["attempts"].array ?? [], id: \.formatted) { attempt in
                            DisclosureGroup("Attempt \(attempt["number"].display)") {
                                WorkRecordView(value: attempt)
                                if let hash = attempt["artifact"]["hash"].text {
                                    NavigationLink("Inspect artifact and evidence") {
                                        WorkQualityArtifact(runID: runID, hash: hash, onReviewed: { reviewedHash = $0 })
                                    }.frame(minHeight: 44)
                                }
                            }.padding(.vertical, 4)
                        }
                        if run["status"].text == "accepted", run["routing"]["route"].text == "human_review", run["owner_approved"] == .null, run["handoff"] == .null, let hash = run["routing"]["artifact_hash"].text {
                            Text("Inspect the accepted artifact before approving its exact bytes.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            Button("Approve exact draft") { review(hash, approved: true) }.disabled(resource.busy || reviewedHash != hash).frame(minHeight: 44)
                            Button("Decline handoff") { review(hash, approved: false) }.disabled(resource.busy).frame(minHeight: 44)
                        }
                        if run["status"].text == "waiting_owner", let attentionID = run["attention_id"].text {
                            Button("Open owner question") { AppActions.shared.requestAttention(itemID: attentionID) }.frame(minHeight: 44)
                            Text("Answer the canonical request before resuming this run.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            Button("Resume after answer") { act("resume", body: .object(["attention_id": .string(attentionID)])) }.frame(minHeight: 44).disabled(resource.busy)
                        }
                    }
                    WorkCard {
                        NavigationLink("Run report") { WorkEndpointView(title: "Run report", path: path + "/report") }.frame(minHeight: 44)
                        if run["handoff"]["kind"].text == "capture", let hash = run["handoff"]["artifact_hash"].text, let taskID = run["admission"]["task_id"].text {
                            NavigationLink("Bind exact local capture") { WorkCaptureBinder(runID: runID, artifactHash: hash, taskID: taskID) }.frame(minHeight: 44)
                        }
                        NavigationLink("Observed lessons") { WorkEndpointView(title: "Observed lessons", path: path + "/lessons") }.frame(minHeight: 44)
                        DisclosureGroup("Contract, decisions and retained work") { WorkRecordView(value: run) }
                    }
                }
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Quality run")
        .task(id: runID) { resource.load(path) }.refreshable { resource.refresh() }
        .onChange(of: resource.scopeID) { _, _ in reviewedHash = nil; cancel = false }
        .toolbar { Button("Refresh run", systemImage: "arrow.clockwise") { resource.refresh() } }
        .confirmationDialog("Cancel this quality run?", isPresented: $cancel, titleVisibility: .visible) {
            Button("Cancel run", role: .destructive) { act("cancel") }
            Button("Keep running", role: .cancel) {}
        }
    }
    private func act(_ action: String, body: WorkJSON = .object([:])) {
        Task { if await resource.perform(path + "/" + action, body: body) != nil { resource.refresh() } }
    }
    private func review(_ hash: String, approved: Bool) { act("review", body: .object(["artifact_hash": .string(hash), "approved": .bool(approved)])) }
}

struct WorkQualityArtifact: View {
    let runID: String
    let hash: String
    var onReviewed: ((String) -> Void)? = nil
    @StateObject private var resource = WorkResource()
    @StateObject private var runResource = WorkResource()
    private var previousHash: String? {
        let attempts = runResource.value?["attempts"].array ?? []
        guard let index = attempts.firstIndex(where: { $0["artifact"]["hash"].text == hash }) else { return nil }
        return attempts.prefix(index).reversed().compactMap { $0["artifact"]["hash"].text }.first { $0 != hash }
    }
    @State private var inspectedBinaries = Set<String>()
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { qualityAPI + "/runs/" + WorkAPIClient.segment(runID) + "/artifacts/" + WorkAPIClient.segment(hash) }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(hash).font(.themed(11)).foregroundColor(theme.secondaryTextColor).textSelection(.enabled)
                WorkResourceStatus(resource: resource)
                WorkResourceStatus(resource: runResource)
                if let previousHash {
                    NavigationLink("Compare previous attempt") { WorkQualityComparison(runID: runID, currentHash: hash, previousHash: previousHash) }.frame(minHeight: 44)
                }
                if let files = resource.value?["files"].object {
                    ForEach(files.keys.sorted(), id: \.self) { name in
                        WorkCard {
                            Text(name).font(.themed(17, weight: .semibold))
                            Text(files[name]?.text ?? "").font(.themed(14)).textSelection(.enabled)
                        }
                    }
                    if (!files.isEmpty || !(resource.value?["binary_files"].object?.isEmpty ?? true)) && Set((resource.value?["binary_files"].object ?? [:]).keys).isSubset(of: inspectedBinaries) { Button("Mark these exact files reviewed") { onReviewed?(hash) }.frame(minHeight: 44) }
                }
                if let binaries = resource.value?["binary_files"].object, !binaries.isEmpty {
                    WorkCard {
                        Text("Binary artifacts").font(.themed(17, weight: .semibold))
                        ForEach(binaries.keys.sorted(), id: \.self) { name in
                            NavigationLink(name) { WorkBinaryArtifact(path: path, name: name, onReviewed: { inspectedBinaries.insert(name) }) }.frame(minHeight: 44)
                            WorkRecordView(value: binaries[name] ?? .null)
                        }
                    }
                }
                NavigationLink("Sealed evidence") { WorkEndpointView(title: "Artifact evidence", path: path + "/evidence") }.frame(minHeight: 44)
            }.padding(16)
        }
        .foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Artifact")
        .onChange(of: resource.scopeID) { _, _ in inspectedBinaries = [] }
        .task(id: path) { resource.load(path); runResource.load(qualityAPI + "/runs/" + WorkAPIClient.segment(runID)) }.refreshable { resource.refresh(); runResource.refresh() }
    }
}

struct WorkQualityComparison: View {
    let runID: String
    let currentHash: String
    let previousHash: String
    @StateObject private var current = WorkResource()
    @StateObject private var previous = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    private var base: String { qualityAPI + "/runs/" + WorkAPIClient.segment(runID) + "/artifacts/" }
    private var files: [String] { Set((current.value?["files"].object ?? [:]).keys).union((previous.value?["files"].object ?? [:]).keys).sorted() }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                Text("Compare exact attempts").font(.themed(20, weight: .semibold))
                WorkResourceStatus(resource: current); WorkResourceStatus(resource: previous)
                if current.value != nil && previous.value != nil {
                    ForEach(files, id: \.self) { name in
                        WorkCard {
                            Text(name).font(.themed(17, weight: .semibold))
                            let old = previous.value?["files"][name].text, new = current.value?["files"][name].text
                            Text(old == new ? "Unchanged" : old == nil ? "Added" : new == nil ? "Removed" : "Changed").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                            DisclosureGroup("Previous attempt · " + previousHash) { Text(old ?? "File absent from this attempt").font(.themed(14)).textSelection(.enabled) }
                            DisclosureGroup("Current attempt · " + currentHash) { Text(new ?? "File absent from this attempt").font(.themed(14)).textSelection(.enabled) }
                        }
                    }
                    binaryFiles(previous, hash: previousHash, title: "Previous binary artifacts")
                    binaryFiles(current, hash: currentHash, title: "Current binary artifacts")
                }
            }.padding(16)
        }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Compare attempts")
        .task { current.load(base + WorkAPIClient.segment(currentHash)); previous.load(base + WorkAPIClient.segment(previousHash)) }
        .refreshable { current.refresh(); previous.refresh() }
    }
    @ViewBuilder private func binaryFiles(_ resource: WorkResource, hash: String, title: String) -> some View {
        if let files = resource.value?["binary_files"].object, !files.isEmpty {
            WorkCard {
                Text(title).font(.themed(17, weight: .semibold))
                ForEach(files.keys.sorted(), id: \.self) { name in
                    NavigationLink(name) { WorkBinaryArtifact(path: base + WorkAPIClient.segment(hash), name: name) }.frame(minHeight: 44)
                    WorkRecordView(value: files[name] ?? .null)
                }
            }
        }
    }
}
