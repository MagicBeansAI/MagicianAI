import SwiftUI

/// An accepted capture handoff is bound to an exact campaign target, never sent.
struct WorkCaptureBinder: View {
    let runID: String
    let artifactHash: String
    let taskID: String
    @StateObject private var resource = WorkResource()
    @State private var page = 0
    @State private var reviewed = false
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView { LazyVStack(alignment: .leading, spacing: 12) {
            Text("Bind this accepted artifact to a reviewed campaign. Captures are local records; nothing is sent.").font(.themed(14)).foregroundColor(theme.secondaryTextColor)
            NavigationLink("Inspect exact accepted artifact") { WorkQualityArtifact(runID: runID, hash: artifactHash, onReviewed: { if $0 == artifactHash { reviewed = true } }) }.frame(minHeight: 44)
            WorkResourceStatus(resource: resource)
            ForEach(resource.value?["campaigns"].array ?? [], id: \.formatted) { row in
                let policy = row["state"]["campaign"]
                WorkCard {
                    Text(policy["spec"]["name"].display).font(.themed(17, weight: .semibold))
                    WorkRecordView(value: policy)
                    ForEach((policy["spec"]["targets"].array ?? []).filter { $0["task_id"].text == taskID }, id: \.formatted) { target in
                        Button("Bind local capture · \(target["identity_id"].display)") {
                            Task {
                                if let id = policy["id"].text {
                                    _ = await resource.perform("/api/magician/v2/relentless/campaigns/" + WorkAPIClient.segment(id) + "/captures", body: .object(["expected_version": policy["version_hash"], "run_id": .string(runID), "artifact_hash": .string(artifactHash), "identity_id": target["identity_id"]]))
                                }
                            }
                        }.frame(minHeight: 44).disabled(!reviewed || resource.busy || policy["revoked_at"] != .null)
                    }
                }
            }
            HStack { Button("Previous") { page -= 1 }.disabled(page == 0); Spacer(); Text("Page \(page + 1)"); Spacer(); Button("Next") { page += 1 }.disabled(resource.value?["pagination"]["has_more"].flag != true) }.frame(minHeight: 44)
        }.padding(16) }.foregroundColor(theme.textColor).background(theme.backgroundColor).navigationTitle("Local capture")
        .task(id: page) { resource.load("/api/magician/v2/relentless/campaigns?limit=10&offset=\(page * 10)") }.onChange(of: resource.scopeID) { _, _ in reviewed = false }
    }
}
