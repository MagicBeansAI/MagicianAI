import SwiftUI

/// Server-paged recurring task inventory; no client-side first-page truncation.
struct WorkQualityTasks: View {
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @State private var goal = ""
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { "/api/magician/v2/relentless/work?limit=20&offset=\(page * 20)&goal_id=" + WorkAPIClient.query(goal) }
    var body: some View {
        WorkPage {
            WorkGroupCard(footer: "Leave empty to list every recurring task.") {
                WorkFieldRow("Goal ID (optional)") {
                    TextField("Goal ID (optional)", text: $goal).font(.themed(15))
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .workFieldSurface()
                }
            }
            WorkResourceStatus(resource: resource)
            if resource.value != nil && resource.value?["targets"].array?.isEmpty != false {
                WorkGroupCard { WorkEmptyState(title: "No recurring quality tasks.", systemImage: "repeat") }
            }
            ForEach(resource.value?["targets"].array ?? [], id: \.formatted) { target in
                if let taskID = target["task_id"].text {
                    WorkHeaderCard(target["title"].text ?? taskID, subtitle: [taskID, target["agent_id"].text].compactMap { $0 }.joined(separator: " · ")) {
                        WorkNavigationRow(title: "Task and full controls", systemImage: "checklist") { TaskDetailScreen(route: .init(taskId: taskID)) }
                        WorkRowDivider()
                        WorkNavigationRow(title: "Review task quality policy", systemImage: "checkmark.seal") { WorkQualityComposer(taskID: taskID, agentID: target["agent_id"].text) }
                        ForEach(target["runs"].array ?? [], id: \.formatted) { run in
                            if let id = run["id"].text {
                                WorkRowDivider()
                                WorkNavigationRow(title: "Run \(id)", systemImage: "arrow.triangle.2.circlepath") { WorkQualityRunDetail(runID: id) }
                            }
                        }
                        WorkRowDivider()
                        DisclosureGroup("Policy and record") { WorkRecordView(value: target) }
                            .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                    }
                }
            }
            WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true)
        }
        .navigationTitle("Recurring quality").navigationBarTitleDisplayMode(.inline)
        .task(id: path) { resource.load(path) }.onChange(of: goal) { _, _ in page = 0 }.refreshable { resource.refresh() }
    }
}

struct WorkGuidanceProposals: View {
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkPage {
            WorkResourceStatus(resource: resource)
            if resource.value != nil && resource.value?["proposals"].array?.isEmpty != false {
                WorkGroupCard { WorkEmptyState(title: "No learning proposals.", systemImage: "lightbulb") }
            }
            ForEach(resource.value?["proposals"].array ?? [], id: \.formatted) { proposal in
                WorkHeaderCard(proposal["guidance"]["title"].text ?? "Guidance proposal",
                               subtitle: [proposal["agent_id"].text, proposal["candidate_id"].text].compactMap { $0 }.joined(separator: " · "),
                               detail: "Choose fresh evaluation and canary cases before approving a policy.") {
                    WorkNavigationRow(title: "Prepare qualification policy", systemImage: "checkmark.shield") { WorkPolicyEditor(kind: .learning, previous: prepared(proposal)) }
                    ForEach(proposal["training_run_ids"].array ?? [], id: \.formatted) { id in
                        if let id = id.text {
                            WorkRowDivider()
                            WorkNavigationRow(title: "Training run \(id)", systemImage: "arrow.triangle.2.circlepath") { WorkQualityRunDetail(runID: id) }
                        }
                    }
                    WorkRowDivider()
                    DisclosureGroup("Proposal record") { WorkRecordView(value: proposal) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                }
            }
            WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true)
        }
        .navigationTitle("Learning proposals").navigationBarTitleDisplayMode(.inline)
        .task(id: page) { resource.load("/api/magician/v2/relentless/guidance-proposals?limit=10&offset=\(page * 10)") }.refreshable { resource.refresh() }
    }
    private func prepared(_ proposal: WorkJSON) -> WorkJSON {
        var spec = WorkPolicyKind.learning.initialSpec.object ?? [:]
        spec["name"] = proposal["guidance"]["title"]; spec["candidate_id"] = proposal["candidate_id"]
        spec["agent_id"] = proposal["agent_id"]; spec["training_run_ids"] = proposal["training_run_ids"]
        return .object(["spec": .object(spec)])
    }
}
