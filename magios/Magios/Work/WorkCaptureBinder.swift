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
        WorkPage {
            WorkGroupCard(footer: "Bind this accepted artifact to a reviewed campaign. Captures are local records; nothing is sent.") {
                WorkNavigationRow(title: "Inspect exact accepted artifact", systemImage: "doc.text.magnifyingglass", value: reviewed ? "Reviewed" : nil) {
                    WorkQualityArtifact(runID: runID, hash: artifactHash, onReviewed: { if $0 == artifactHash { reviewed = true } })
                }
            }
            WorkResourceStatus(resource: resource)
            ForEach(resource.value?["campaigns"].array ?? [], id: \.formatted) { row in
                let policy = row["state"]["campaign"]
                WorkHeaderCard(policy["spec"]["name"].display, subtitle: policy["id"].text) {
                    ForEach((policy["spec"]["targets"].array ?? []).filter { $0["task_id"].text == taskID }, id: \.formatted) { target in
                        WorkButtonRow(title: "Bind local capture · \(target["identity_id"].display)", systemImage: "link") {
                            Task {
                                if let id = policy["id"].text {
                                    _ = await resource.perform("/api/magician/v2/relentless/campaigns/" + WorkAPIClient.segment(id) + "/captures", body: .object(["expected_version": policy["version_hash"], "run_id": .string(runID), "artifact_hash": .string(artifactHash), "identity_id": target["identity_id"]]))
                                }
                            }
                        }.disabled(!reviewed || resource.busy || policy["revoked_at"] != .null)
                        WorkRowDivider()
                    }
                    DisclosureGroup("Policy record") { WorkRecordView(value: policy) }
                        .disclosureGroupStyle(WorkDisclosureStyle(systemImage: "doc.badge.gearshape"))
                }
            }
            WorkPager(page: $page, hasMore: resource.value?["pagination"]["has_more"].flag == true)
        }
        .navigationTitle("Local capture").navigationBarTitleDisplayMode(.inline)
        .task(id: page) { resource.load("/api/magician/v2/relentless/campaigns?limit=10&offset=\(page * 10)") }.onChange(of: resource.scopeID) { _, _ in reviewed = false }
    }
}
