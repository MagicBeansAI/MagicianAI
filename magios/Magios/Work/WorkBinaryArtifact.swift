import SwiftUI
import QuickLook

/// Preview only downloaded bytes from the frozen scoped connection. Quick Look
/// receives a local file, so another origin cannot inherit runtime credentials.
struct WorkBinaryArtifact: View {
    let path: String
    let name: String
    var onReviewed: (() -> Void)? = nil
    @StateObject private var resource = WorkResource()
    @State private var file: URL?
    @State private var preview: URL?
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        WorkPage {
            WorkHeaderCard(name)
            WorkResourceStatus(resource: resource)
            WorkGroupCard {
                if let file {
                    WorkButtonRow(title: "Preview exact file", systemImage: "eye") { preview = file }
                    WorkRowDivider()
                    ShareLink(item: file) {
                        WorkRowLabel(title: "Save or share exact file", systemImage: "square.and.arrow.up", accessory: .none, tint: theme.accentColor)
                    }
                    .buttonStyle(.plain).accessibilityLabel("Save or share exact file")
                    if let onReviewed {
                        WorkRowDivider()
                        WorkButtonRow(title: "I reviewed this file", systemImage: "checkmark.circle", action: onReviewed)
                    }
                } else {
                    WorkButtonRow(title: "Load exact file", systemImage: "arrow.down.circle") { Task { await load() } }.disabled(resource.busy || resource.loading)
                }
            }
        }
        .navigationTitle("Artifact file").navigationBarTitleDisplayMode(.inline).quickLookPreview($preview)
        .task { resource.load(path) }
        .onChange(of: resource.scopeID) { _, _ in file = nil; preview = nil }
    }
    private func load() async {
        guard let data = await resource.file(path + "/file?name=" + WorkAPIClient.query(name)) else { return }
        do {
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent("work-artifacts/" + UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let safeName = (name as NSString).lastPathComponent
            let target = folder.appendingPathComponent(safeName.isEmpty ? "artifact" : safeName)
            try data.write(to: target, options: .atomic); file = target
        } catch { resource.error = "Could not save the artifact for preview." }
    }
}
