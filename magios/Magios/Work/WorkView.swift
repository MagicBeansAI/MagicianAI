import SwiftUI

/// Work owns navigation only. The mature task/monitor and Attention views keep
/// their own models, inspectors, intent resolution and execution controls.
struct WorkView: View {
    @ObservedObject var attention: AttentionViewModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var section: WorkSection = .all
    @State private var createRequested = false
    @State private var connectionEpoch = UUID()
    @State private var initialTarget: WorkNavigationTarget?
    @State private var threadContext: String?
    @ObservedObject private var actions = AppActions.shared

    var body: some View {
        ZStack {
                // Retain filters, scroll and navigation while reading Crew or
                // programs. Hidden content cannot receive taps or VoiceOver.
                TasksView(workCreateRequest: $createRequested, workNavigation: section == .all ? AnyView(navigation) : nil, threadContextID: threadContext)
                    .id(connectionEpoch)
                    .opacity(section == .all ? 1 : 0)
                    .allowsHitTesting(section == .all)
                    .accessibilityHidden(section != .all)
                if section == .needsYou { AttentionView(viewModel: attention, workNavigation: AnyView(navigation)) }
                if ![.all, .needsYou].contains(section) {
                    WorkLibraryView(section: section, onCreate: create, workNavigation: AnyView(navigation), initialTarget: initialTarget).id(section).id(connectionEpoch)
                }
        }
        .background(theme.backgroundColor)
        .onReceive(actions.$taskRequestID.dropFirst()) { _ in section = .all; threadContext = actions.taskTargetThreadID }
        .onReceive(actions.$workRequestID.dropFirst()) { _ in applyTarget() }
        .onAppear { applyTarget() }
        .onReceive(actions.$monitorRequestID.dropFirst()) { _ in section = .all }
        .onReceive(NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)) { _ in
            section = .all; createRequested = false; connectionEpoch = UUID(); initialTarget = nil; threadContext = nil
        }
    }
    private func applyTarget() {
        guard let target = actions.workTarget else { return }
        section = target.section
        if target.kind == .thread { threadContext = target.recordID; initialTarget = nil }
        else { threadContext = nil; initialTarget = target }
        connectionEpoch = UUID(); actions.consumeWorkTarget(target)
    }
    private func create() { initialTarget = nil; section = .all; createRequested = true }
    private var navigation: some View {
        HStack(spacing: 4) {
            ForEach([WorkSection.all, .needsYou, .crew]) { item in
                Button { initialTarget = nil; if item == .all { threadContext = nil }; section = item } label: {
                    Text(item.title).font(.themed(12, weight: section == item ? .semibold : .regular))
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .foregroundColor(section == item ? theme.accentColor : theme.secondaryTextColor)
                        .background(section == item ? theme.accentColor.opacity(0.12) : .clear)
                        .clipShape(RoundedRectangle(cornerRadius: 9))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("work-view-\(item.rawValue)")
                .accessibilityAddTraits(section == item ? .isSelected : [])
            }
            Menu {
                ForEach([WorkSection.programs, .quality, .operations]) { item in
                    Button(item.title) { initialTarget = nil; section = item }
                }
            } label: {
                Image(systemName: "ellipsis").frame(width: 44, height: 44).foregroundColor(theme.accentColor)
            }
            .accessibilityLabel("More work views")
            .accessibilityIdentifier("work-more")
        }
        .padding(.horizontal, 8)
        .background(theme.surfaceColor)
    }
}
