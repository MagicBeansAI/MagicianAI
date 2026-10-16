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
        VStack(alignment: .leading, spacing: 16) {
            Text(name).font(.themed(18, weight: .semibold)).textSelection(.enabled)
            WorkResourceStatus(resource: resource)
            if let file {
                Button("Preview exact file") { preview = file }.frame(minHeight: 44)
                ShareLink("Save or share exact file", item: file).frame(minHeight: 44)
                if let onReviewed { Button("I reviewed this file", action: onReviewed).frame(minHeight: 44) }
            } else {
                Button("Load exact file") { Task { await load() } }.disabled(resource.busy || resource.loading).frame(minHeight: 44)
            }
            Spacer()
        }.padding(16).foregroundColor(theme.textColor).background(theme.backgroundColor)
        .navigationTitle("Artifact file").quickLookPreview($preview)
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
