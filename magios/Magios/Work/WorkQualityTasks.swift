import SwiftUI

/// Server-paged recurring task inventory; no client-side first-page truncation.
struct WorkQualityTasks: View {
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @State private var goal = ""
    @ObservedObject private var theme = ThemeManager.shared
    private var path: String { "/api/magician/v2/relentless/work?limit=20&offset=\(page * 20)&goal_id=" + WorkAPIClient.query(goal) }
    var body: some View {
        ScrollView { LazyVStack(alignment: .leading, spacing: 12) {
            TextField("Goal ID (optional)", text: $goal).font(.themed(14)).padding(10).background(theme.controlColor)
            WorkResourceStatus(resource: resource)
            ForEach(resource.value?["targets"].array ?? [], id: \.formatted) { target in
                if let taskID = target["task_id"].text { WorkCard {
                    Text(target["title"].text ?? taskID).font(.themed(17, weight: .semibold))
                    WorkRecordView(value: target)
                    NavigationLink("Task and full controls") { TaskDetailScreen(route: .init(taskId: taskID)) }.frame(minHeight: 44)
                    NavigationLink("Review task quality policy") { WorkQualityComposer(taskID: taskID, agentID: target["agent_id"].text) }.frame(minHeight: 44)
                    ForEach(target["runs"].array ?? [], id: \.formatted) { run in
                        if let id = run["id"].text { NavigationLink("Run \(id)") { WorkQualityRunDetail(runID: id) }.frame(minHeight: 44) }
                    }
                } }
            }
            HStack { Button("Previous") { page -= 1 }.disabled(page == 0); Spacer(); Text("Page \(page + 1)"); Spacer(); Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true) }.frame(minHeight: 44)
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Recurring quality")
        .task(id: path) { resource.load(path) }.onChange(of: goal) { _, _ in page = 0 }.refreshable { resource.refresh() }
    }
}

struct WorkGuidanceProposals: View {
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView { LazyVStack(alignment: .leading, spacing: 12) {
            WorkResourceStatus(resource: resource)
            ForEach(resource.value?["proposals"].array ?? [], id: \.formatted) { proposal in
                WorkCard {
                    Text(proposal["guidance"]["title"].text ?? "Guidance proposal").font(.themed(17, weight: .semibold))
                    WorkRecordView(value: proposal)
                    Text("Choose fresh evaluation and canary cases before approving a policy.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    NavigationLink("Prepare qualification policy") { WorkPolicyEditor(kind: .learning, previous: prepared(proposal)) }.frame(minHeight: 44)
                    ForEach(proposal["training_run_ids"].array ?? [], id: \.formatted) { id in
                        if let id = id.text { NavigationLink("Training run \(id)") { WorkQualityRunDetail(runID: id) }.frame(minHeight: 44) }
                    }
                }
            }
            HStack { Button("Previous") { page -= 1 }.disabled(page == 0); Spacer(); Text("Page \(page + 1)"); Spacer(); Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true) }.frame(minHeight: 44)
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Learning proposals")
        .task(id: page) { resource.load("/api/magician/v2/relentless/guidance-proposals?limit=10&offset=\(page * 10)") }.refreshable { resource.refresh() }
    }
    private func prepared(_ proposal: WorkJSON) -> WorkJSON {
        var spec = WorkPolicyKind.learning.initialSpec.object ?? [:]
        spec["name"] = proposal["guidance"]["title"]; spec["candidate_id"] = proposal["candidate_id"]
        spec["agent_id"] = proposal["agent_id"]; spec["training_run_ids"] = proposal["training_run_ids"]
        return .object(["spec": .object(spec)])
    }
}
