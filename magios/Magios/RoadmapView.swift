import SwiftUI

struct RoadmapFeature: Identifiable, Equatable {
    var id: String { name }
    let name: String
    let phase: String
}

/// Pending-only by construction. Shipped capabilities belong in How to Use and
/// release notes, not in a roadmap decorated with stale completion marks.
enum RoadmapCatalog {
    static let pending = [
        RoadmapFeature(name: "Screenshot + dictation Shortcut / Action Button flow", phase: "Shortcuts"),
        RoadmapFeature(name: "Inline visual confirmations in Siri", phase: "Siri"),
        RoadmapFeature(name: "Agent-assisted negotiation from messaging apps", phase: "Future experiences"),
        RoadmapFeature(name: "Camera or sketch handoff to VibeDev", phase: "Future experiences"),
        RoadmapFeature(name: "Screen-broadcast App Copilot guidance", phase: "Future experiences")
    ]
}

struct RoadmapView: View {
    let features = RoadmapCatalog.pending

    @StateObject private var themeManager = ThemeManager.shared

    var body: some View {
        // Pushed from Settings — no own NavigationView (the Settings stack provides it).
        List {
            Section(header: headerView) {
                ForEach(features) { feature in
                    HStack(spacing: 16) {
                        Image(systemName: "circle")
                            .foregroundColor(themeManager.secondaryTextColor)
                            .font(.title3)

                        VStack(alignment: .leading, spacing: 4) {
                            Text(feature.name)
                                .font(themeManager.font(16))
                                .foregroundColor(themeManager.textColor)

                            Text(feature.phase)
                                .font(themeManager.font(12))
                                .foregroundColor(themeManager.secondaryTextColor)
                        }
                    }
                    .padding(.vertical, 4)
                    .listRowBackground(themeManager.surfaceColor)
                }
            }
        }
        .listStyle(InsetGroupedListStyle())
        .scrollContentBackground(.hidden)
        .background(themeManager.backgroundColor.ignoresSafeArea())
        .navigationTitle("Features & Roadmap")
        .navigationBarTitleDisplayMode(.inline)
    }
    
    var headerView: some View {
        VStack(spacing: 12) {
            Image(systemName: "wand.and.stars")
                .font(.system(size: 40))
                .foregroundStyle(themeManager.accentColor)
            Text("Coming to Magican")
                .font(themeManager.font(17, weight: .semibold))
                .foregroundColor(themeManager.textColor)
                .textCase(nil)
            Text("Only features that have not shipped yet")
                .font(themeManager.font(13))
                .foregroundColor(themeManager.secondaryTextColor)
                .textCase(nil)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
    }
}

#Preview {
    RoadmapView()
}
