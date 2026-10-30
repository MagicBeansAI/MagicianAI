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
    /// The last catalog `counts`, kept while another lane or page loads.
    @State private var counts: WorkQualityCounts?
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String {
        var parts = URLComponents()
        parts.queryItems = [.init(name: "view", value: lane), .init(name: "limit", value: "20"), .init(name: "offset", value: String(page * 20)), .init(name: "q", value: search)]
        let owner = ownerOverride ?? agentID ?? ""
        parts.queryItems?.append(.init(name: "status", value: statusFilter))
        if !owner.isEmpty { parts.queryItems?.append(.init(name: "agent_id", value: owner)) }
        return qualityAPI + "/catalog?" + (parts.percentEncodedQuery ?? "")
    }
    private var owner: String { ownerOverride ?? agentID ?? "" }
    private var ownerName: String {
        guard !owner.isEmpty else { return "All members" }
        return crew.value?["agents"].array?.first { $0["definition"]["agent_id"].text == owner }?["definition"]["name"].text ?? owner
    }
    var body: some View {
        WorkPage {
                WorkSearchField("Search quality work", text: $search)
                Picker("Quality view", selection: $lane) {
                    Text(WorkQualityCounts.inboxLabel(counts)).tag("inbox"); Text(WorkQualityCounts.runsLabel(counts)).tag("runs")
                }.pickerStyle(.segmented)
                .accessibilityIdentifier("work-quality-lanes")
                WorkGroupCard {
                    WorkButtonRow(title: "New quality work", systemImage: "plus.circle") { compose = true }
                    WorkRowDivider()
                    DisclosureGroup("Filters") {
                        VStack(alignment: .leading, spacing: 0) {
                            WorkFormRow(lane == "runs" ? "Run status" : "Review type") {
                                Picker(lane == "runs" ? "Run status" : "Review type", selection: $statusFilter) {
                                    ForEach(lane == "runs" ? ["all", "active", "review", "attention", "complete", "stopped"] : ["all", "attention", "review", "brief"], id: \.self) { Text(WorkRecordView.label($0)).tag($0) }
                                }.labelsHidden().pickerStyle(.menu).font(.themed(15))
                            }
                            WorkRowDivider()
                            WorkFormRow("Crew member") {
                                Picker("Crew member", selection: Binding(get: { ownerOverride ?? agentID ?? "" }, set: { ownerOverride = $0 })) {
                                    Text("All members").tag("")
                                    let current = ownerOverride ?? agentID ?? ""
                                    if !current.isEmpty { Text(current).tag(current) }
                                    ForEach(crew.value?["agents"].array ?? [], id: \.formatted) { row in
                                        if let id = row["definition"]["agent_id"].text, id != current { Text(row["definition"]["name"].text ?? id).tag(id) }
                                    }
                                }.labelsHidden().pickerStyle(.menu).font(.themed(15))
                            }
                            WorkResourceStatus(resource: crew)
                        }
                    }
                    .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "line.3.horizontal.decrease.circle",
                                                              value: [statusFilter == "all" ? nil : WorkRecordView.label(statusFilter), owner.isEmpty ? nil : ownerName].compactMap { $0 }.joined(separator: " · ")))
                }
                if let taskID {
                    WorkGroupCard("This task") {
                        WorkNavigationRow(title: "Task quality context", systemImage: "checkmark.seal") { WorkEndpointView(title: "Task quality", path: qualityAPI + "/task-context/" + WorkAPIClient.segment(taskID)) }
                        WorkRowDivider()
                        WorkNavigationRow(title: "Open full task", systemImage: "arrow.up.forward.square") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }
                    }
                }
                WorkResourceStatus(resource: resource)
                if let items = resource.value?["items"].array {
                    VStack(alignment: .leading, spacing: 8) {
                        WorkGroupCard(lane == "runs" ? "Runs" : "Needs review") {
                            if items.isEmpty { WorkEmptyState(title: "No quality work in this view.", systemImage: "checkmark.seal") }
                            ForEach(Array(items.enumerated()), id: \.element.formatted) { index, entry in
                                let item = entry["item"]
                                if index > 0 { WorkRowDivider() }
                                if entry["kind"].text == "run", let id = item["id"].text {
                                    let status = item["status"].text ?? "unknown"
                                    NavigationLink { WorkQualityRunDetail(runID: id) } label: {
                                        WorkItemLabel(title: item["goal"].text ?? id,
                                                      subtitle: "Attempts: \(item["attempt_count"].display) of \(item["max_attempts"].display == "0" || item["max_attempts"].display == "—" ? "Unlimited" : item["max_attempts"].display)",
                                                      systemImage: "arrow.triangle.2.circlepath",
                                                      chips: [WorkChipSpec(title: item["label"].text ?? WorkRecordView.label(status), tone: .workStatusTone(status))])
                                    }.buttonStyle(.plain)
                                } else if entry["kind"].text == "proposal", let id = item["origin"]["request_id"].text {
                                    NavigationLink { WorkQualityProposalDetail(proposalID: id) } label: {
                                        WorkItemLabel(title: item["goal"].text ?? "Owner proposal",
                                                      subtitle: "Review proposed work · \(item["origin"]["agent_id"].display)",
                                                      systemImage: "lightbulb",
                                                      chips: [WorkChipSpec(title: "Proposal", tone: theme.warningColor)])
                                    }.buttonStyle(.plain)
                                } else { WorkRecordView(value: entry) }
                            }
                        }
                        WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true)
                    }
                }
                WorkGroupCard("Policies and learning") {
                    WorkNavigationRow(title: "Tasks and recurring quality", systemImage: "repeat") { WorkQualityTasks() }
                    WorkRowDivider()
                    WorkNavigationRow(title: "Capture policies", systemImage: "tray.and.arrow.down") { WorkPoliciesView(kind: .capture) }
                    WorkRowDivider()
                    WorkNavigationRow(title: "Learning policies", systemImage: "graduationcap") { WorkPoliciesView(kind: .learning) }
                    WorkRowDivider()
                    WorkNavigationRow(title: "Learning proposals", systemImage: "lightbulb") { WorkGuidanceProposals() }
                }
                WorkGroupCard("Web console") {
                    WorkRelatedTools(origin: WorkWebContext(section: .quality, agent: ownerOverride ?? agentID ?? "", task: taskID ?? ""), destinations: [.analytics, .history, .evals, .events, .budget])
                }
        }
        .toolbar { Button("Refresh quality", systemImage: "arrow.clockwise") { resource.refresh(); crew.refresh() } }
        .task { crew.load("/api/magician/v2/agents") }
        .task(id: path) { resource.load(path) }.refreshable { resource.refresh() }
        .onChange(of: resource.value) { _, value in if let next = WorkQualityCounts(catalog: value) { counts = next } }
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
        WorkPage {
            WorkResourceStatus(resource: resource)
            if let proposed = resource.value {
                WorkHeaderCard(proposed["proposal"]["goal"].text ?? "Proposed work",
                               subtitle: proposed["origin"]["agent_id"].text.map { "Proposed by " + $0 },
                               detail: proposed["reason"].text,
                               chips: [WorkChipSpec(title: "Proposal", tone: theme.warningColor)]) {
                    NavigationLink {
                        WorkQualityComposer(taskID: proposed["origin"]["task_id"].text, agentID: proposed["origin"]["agent_id"].text, proposed: proposed)
                    } label: {
                        WorkRowLabel(title: "Review and configure", systemImage: "slider.horizontal.3", tint: theme.accentColor)
                    }
                    .buttonStyle(.plain).accessibilityLabel("Review and configure")
                }
                WorkGroupCard("Proposal record") { WorkRecordView(value: proposed) }
            }
        }
        .navigationTitle("Proposal").navigationBarTitleDisplayMode(.inline)
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
        WorkPage {
                WorkResourceStatus(resource: resource)
                if let run = resource.value {
                    let status = run["status"].text ?? "unknown"
                    WorkHeaderCard(run["contract"]["goal"].text ?? runID, subtitle: runID, detail: run["reason"].text,
                                   chips: [WorkChipSpec(title: WorkRecordView.label(status), tone: .workStatusTone(status))]) {
                        if let taskID = run["admission"]["task_id"].text {
                            WorkNavigationRow(title: "Owning task", systemImage: "checklist") { TaskDetailScreen(route: TaskDetailRoute(taskId: taskID)) }
                            WorkRowDivider()
                        }
                        if !["accepted", "plateau", "budget_exhausted", "cancelled", "failed"].contains(status) {
                            WorkButtonRow(title: "Cancel quality run", systemImage: "xmark.circle", role: .destructive) { cancel = true }.disabled(resource.busy)
                        } else {
                            WorkButtonRow(title: "Reconcile available cost receipts", systemImage: "dollarsign.arrow.circlepath") { act("reconcile-costs") }.disabled(resource.busy)
                        }
                    }
                    WorkGroupCard("Budget and spend") {
                        WorkRecordView(value: .object(["spend": run["spend"], "budget": run["contract"]["budget"], "deadline": run["deadline_at"]]))
                    }
                    WorkGroupCard("Attempts and exact outputs") {
                        let attempts = run["attempts"].array ?? []
                        if attempts.isEmpty { WorkInfoRow(title: "No attempts yet.", systemImage: "hourglass") }
                        ForEach(Array(attempts.enumerated()), id: \.element.formatted) { index, attempt in
                            if index > 0 { WorkRowDivider() }
                            DisclosureGroup("Attempt \(attempt["number"].display)") {
                                VStack(alignment: .leading, spacing: 0) {
                                    if let hash = attempt["artifact"]["hash"].text {
                                        WorkNavigationRow(title: "Inspect artifact and evidence", systemImage: "doc.text.magnifyingglass") {
                                            WorkQualityArtifact(runID: runID, hash: hash, onReviewed: { reviewedHash = $0 })
                                        }
                                        WorkRowDivider()
                                    }
                                    WorkRecordView(value: attempt)
                                }
                                .workInsetPanel()
                            }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "number.circle", value: attempt["artifact"]["hash"].text))
                        }
                        if run["status"].text == "accepted", run["routing"]["route"].text == "human_review", run["owner_approved"] == .null, run["handoff"] == .null, let hash = run["routing"]["artifact_hash"].text {
                            WorkRowDivider()
                            WorkNote("Inspect the accepted artifact before approving its exact bytes.")
                            WorkRowDivider()
                            WorkButtonRow(title: "Approve exact draft", systemImage: "checkmark.circle") { review(hash, approved: true) }.disabled(resource.busy || reviewedHash != hash)
                            WorkRowDivider()
                            WorkButtonRow(title: "Decline handoff", systemImage: "xmark.circle", role: .destructive) { review(hash, approved: false) }.disabled(resource.busy)
                        }
                        if run["status"].text == "waiting_owner", let attentionID = run["attention_id"].text {
                            WorkRowDivider()
                            WorkButtonRow(title: "Open owner question", systemImage: "questionmark.bubble") { AppActions.shared.requestAttention(itemID: attentionID) }
                            WorkNote("Answer the canonical request before resuming this run.")
                            WorkRowDivider()
                            WorkButtonRow(title: "Resume after answer", systemImage: "play.circle") { act("resume", body: .object(["attention_id": .string(attentionID)])) }.disabled(resource.busy)
                        }
                    }
                    WorkGroupCard("Reports and evidence") {
                        WorkNavigationRow(title: "Run report", systemImage: "doc.richtext") { WorkEndpointView(title: "Run report", path: path + "/report") }
                        if run["handoff"]["kind"].text == "capture", let hash = run["handoff"]["artifact_hash"].text, let taskID = run["admission"]["task_id"].text {
                            WorkRowDivider()
                            WorkNavigationRow(title: "Bind exact local capture", systemImage: "link") { WorkCaptureBinder(runID: runID, artifactHash: hash, taskID: taskID) }
                        }
                        WorkRowDivider()
                        WorkNavigationRow(title: "Observed lessons", systemImage: "lightbulb") { WorkEndpointView(title: "Observed lessons", path: path + "/lessons") }
                        WorkRowDivider()
                        DisclosureGroup("Contract, decisions and retained work") { WorkRecordView(value: run) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                    }
                }
        }
        .navigationTitle("Quality run").navigationBarTitleDisplayMode(.inline)
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
        WorkPage {
                WorkHeaderCard("Artifact", subtitle: hash)
                WorkResourceStatus(resource: resource)
                WorkResourceStatus(resource: runResource)
                if let files = resource.value?["files"].object {
                    ForEach(files.keys.sorted(), id: \.self) { name in
                        VStack(alignment: .leading, spacing: 8) {
                            WorkSectionHeader(name)
                            Text(files[name]?.text ?? "").font(.themed(15)).foregroundColor(theme.textColor).textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                                .padding(WorkMetrics.cardInset)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .workCardSurface()
                        }
                    }
                }
                if let binaries = resource.value?["binary_files"].object, !binaries.isEmpty {
                    WorkGroupCard("Binary artifacts") {
                        ForEach(Array(binaries.keys.sorted().enumerated()), id: \.element) { index, name in
                            if index > 0 { WorkRowDivider() }
                            WorkNavigationRow(title: name, systemImage: "doc.zipper", value: inspectedBinaries.contains(name) ? "Reviewed" : nil) {
                                WorkBinaryArtifact(path: path, name: name, onReviewed: { inspectedBinaries.insert(name) })
                            }
                            WorkRecordView(value: binaries[name] ?? .null).padding(.leading, 36)
                        }
                    }
                }
                WorkGroupCard("Review") {
                    if let previousHash {
                        WorkNavigationRow(title: "Compare previous attempt", systemImage: "rectangle.split.2x1") { WorkQualityComparison(runID: runID, currentHash: hash, previousHash: previousHash) }
                        WorkRowDivider()
                    }
                    WorkNavigationRow(title: "Sealed evidence", systemImage: "lock.doc") { WorkEndpointView(title: "Artifact evidence", path: path + "/evidence") }
                    if let files = resource.value?["files"].object,
                       (!files.isEmpty || !(resource.value?["binary_files"].object?.isEmpty ?? true)) && Set((resource.value?["binary_files"].object ?? [:]).keys).isSubset(of: inspectedBinaries) {
                        WorkRowDivider()
                        WorkButtonRow(title: "Mark these exact files reviewed", systemImage: "checkmark.circle") { onReviewed?(hash) }
                    }
                }
        }
        .navigationTitle("Artifact").navigationBarTitleDisplayMode(.inline)
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
    private func fileText(_ text: String) -> some View {
        Text(text).font(.themed(15)).foregroundColor(theme.textColor).textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
            .padding(12).frame(maxWidth: .infinity, alignment: .leading)
            .workInsetPanel()
    }
    var body: some View {
        WorkPage {
                WorkHeaderCard("Compare exact attempts", subtitle: previousHash + " → " + currentHash)
                WorkResourceStatus(resource: current); WorkResourceStatus(resource: previous)
                if current.value != nil && previous.value != nil {
                    ForEach(files, id: \.self) { name in
                        let old = previous.value?["files"][name].text, new = current.value?["files"][name].text
                        let change = old == new ? "Unchanged" : old == nil ? "Added" : new == nil ? "Removed" : "Changed"
                        WorkGroupCard(name) {
                            WorkInfoRow(title: "Change", systemImage: "doc.on.doc", value: change)
                            WorkRowDivider()
                            DisclosureGroup("Previous attempt · " + previousHash) { fileText(old ?? "File absent from this attempt") }
                                .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "clock.arrow.circlepath"))
                            WorkRowDivider()
                            DisclosureGroup("Current attempt · " + currentHash) { fileText(new ?? "File absent from this attempt") }
                                .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.text"))
                        }
                    }
                    binaryFiles(previous, hash: previousHash, title: "Previous binary artifacts")
                    binaryFiles(current, hash: currentHash, title: "Current binary artifacts")
                }
        }
        .navigationTitle("Compare attempts").navigationBarTitleDisplayMode(.inline)
        .task { current.load(base + WorkAPIClient.segment(currentHash)); previous.load(base + WorkAPIClient.segment(previousHash)) }
        .refreshable { current.refresh(); previous.refresh() }
    }
    @ViewBuilder private func binaryFiles(_ resource: WorkResource, hash: String, title: String) -> some View {
        if let files = resource.value?["binary_files"].object, !files.isEmpty {
            WorkGroupCard(title) {
                ForEach(Array(files.keys.sorted().enumerated()), id: \.element) { index, name in
                    if index > 0 { WorkRowDivider() }
                    WorkNavigationRow(title: name, systemImage: "doc.zipper") { WorkBinaryArtifact(path: base + WorkAPIClient.segment(hash), name: name) }
                    WorkRecordView(value: files[name] ?? .null).padding(.leading, 36)
                }
            }
        }
    }
}
