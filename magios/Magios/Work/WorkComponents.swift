import SwiftUI

/// Compact human-readable values first; nested records expand on demand and
/// remain selectable. No field is silently discarded from an operator record.
struct WorkRecordView: View {
    let value: WorkJSON
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        switch value {
        case .object(let fields):
            VStack(alignment: .leading, spacing: 10) {
                ForEach(fields.keys.sorted(), id: \.self) { key in
                    if let child = fields[key] {
                        if child.object != nil || child.array != nil {
                            DisclosureGroup(WorkRecordView.label(key)) { WorkRecordView(value: child).padding(.top, 8) }
                                .font(.themed(14)).tint(theme.accentColor)
                        } else {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(Self.label(key)).font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                                Text(child.display).font(.themed(14)).foregroundColor(theme.textColor).textSelection(.enabled)
                            }
                        }
                    }
                }
            }
        case .array(let rows):
            VStack(alignment: .leading, spacing: 12) {
                if rows.isEmpty { Text("None").foregroundColor(theme.secondaryTextColor) }
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    WorkRecordView(value: row)
                    Divider().overlay(theme.cardBorderColor)
                }
            }
        default:
            Text(value.display).font(.themed(14)).foregroundColor(theme.textColor).textSelection(.enabled)
        }
    }
    static func label(_ key: String) -> String { key.replacingOccurrences(of: "_", with: " ").capitalized }
}

struct WorkCard<Content: View>: View {
    @ViewBuilder let content: Content
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        VStack(alignment: .leading, spacing: 12) { content }
            .frame(maxWidth: .infinity, alignment: .leading).padding(16)
            .background(theme.cardColor, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(theme.cardBorderColor, lineWidth: 1))
    }
}

struct WorkResourceStatus: View {
    @ObservedObject var resource: WorkResource
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        if resource.loading && resource.value == nil { ProgressView("Loading…").padding() }
        if let error = resource.error {
            WorkCard {
                Text(error).font(.themed(14)).foregroundColor(theme.dangerColor)
                Button("Retry") { resource.refresh() }.frame(minHeight: 44)
            }
        }
        if let notice = resource.notice { Text(notice).font(.themed(12)).foregroundColor(theme.successColor) }
    }
}

struct WorkEndpointView: View {
    let title: String
    let path: String
    @StateObject private var resource = WorkResource()
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                WorkResourceStatus(resource: resource)
                if let value = resource.value { WorkCard { WorkRecordView(value: value) } }
            }.padding(16)
        }
        .background(theme.backgroundColor).navigationTitle(title).navigationBarTitleDisplayMode(.inline)
        .task(id: path) { resource.load(path) }
        .refreshable { resource.refresh() }
        .toolbar { Button("Refresh", systemImage: "arrow.clockwise") { resource.refresh() } }
    }
}
