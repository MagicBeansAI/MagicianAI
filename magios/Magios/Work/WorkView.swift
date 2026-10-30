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
    /// A Goals link picks Open or Closed (`goals=closed`) before the screen opens.
    @AppStorage(WorkGoalList.storageKey) private var goalList = WorkGoalList.open
    @ObservedObject private var actions = AppActions.shared
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        ZStack {
                // Retain filters, scroll and navigation while reading Crew or
                // programs. Hidden content cannot receive taps or VoiceOver.
                TasksView(workCreateRequest: $createRequested, workNavigation: section == .all ? AnyView(navigation) : nil, threadContextID: threadContext,
                          onOpenWorkSection: { select($0) })
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
        if target.kind == .goals { goalList = target.closedGoals ? .closed : .open }
        if target.kind == .thread { threadContext = target.recordID; initialTarget = nil }
        else { threadContext = nil; initialTarget = target }
        connectionEpoch = UUID(); actions.consumeWorkTarget(target)
    }
    private func create() { initialTarget = nil; section = .all; createRequested = true }
    private static let moreSections: [WorkSection] = [.goals, .programs, .quality, .operations]
    private func select(_ item: WorkSection) {
        initialTarget = nil
        if item == .all { threadContext = nil }
        section = item
    }
    /// One segmented track under the title: the three everyday views, then
    /// More, which names the library view it is showing.
    private var navigation: some View {
        let moreActive = Self.moreSections.contains(section)
        return HStack(spacing: 2) {
            ForEach([WorkSection.all, .needsYou, .crew]) { item in
                let badge = item == .needsYou ? attention.badgeCount : 0
                Button { select(item) } label: { segment(item.title, selected: section == item, badge: badge) }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("work-view-\(item.rawValue)")
                    .accessibilityValue(badge > 0 ? "\(badge) waiting" : "")
                    .accessibilityAddTraits(section == item ? .isSelected : [])
                    .accessibilityHint(item.help.explanation)
                    .help(item.help.explanation)
            }
            Menu {
                ForEach(Self.moreSections) { item in
                    Button { select(item) } label: {
                        Label(item.title, systemImage: section == item ? "checkmark" : Self.icon(item))
                    }
                }
            } label: {
                segment(moreActive ? Self.shortTitle(section) : "More", selected: moreActive, menu: true)
            }
            .accessibilityLabel("More work views")
            .accessibilityValue(moreActive ? section.title : "")
            .accessibilityIdentifier("work-more")
            WorkHelpButton(topic: section.help, compact: true)
        }
        .padding(3)
        .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
        .padding(.horizontal, WorkMetrics.screenPadding)
        .padding(.top, 4).padding(.bottom, 8)
        .frame(maxWidth: .infinity)
        .background(theme.backgroundColor)
        .dynamicTypeSize(...DynamicTypeSize.accessibility2)
    }
    private func segment(_ title: String, selected: Bool, menu: Bool = false, badge: Int = 0) -> some View {
        HStack(spacing: 4) {
            // One line always; a narrow phone shrinks the title instead of wrapping.
            Text(title).lineLimit(1).minimumScaleFactor(0.5).multilineTextAlignment(.center)
            if badge > 0 {
                Text(badge > 99 ? "99+" : "\(badge)")
                    .font(.themed(11, weight: .bold)).foregroundColor(theme.dangerColor)
                    .padding(.horizontal, 5).padding(.vertical, 1)
                    .background(theme.dangerColor.opacity(0.14), in: RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous).strokeBorder(theme.dangerColor.opacity(0.4), lineWidth: 1))
                    .fixedSize()
                    .accessibilityHidden(true)
            }
            if menu && !typeSize.isAccessibilitySize { Image(systemName: "chevron.down").font(.themed(10, weight: .bold)).accessibilityHidden(true) }
        }
        .font(.themed(14, weight: selected ? .semibold : .medium))
        .foregroundColor(selected ? theme.accentColor : theme.secondaryTextColor)
        .padding(.horizontal, typeSize.isAccessibilitySize ? 2 : 6)
        .frame(maxWidth: .infinity, minHeight: 38)
        .background(selected ? theme.accentColor.opacity(0.14) : .clear, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
        .contentShape(Rectangle())
    }
    private static func shortTitle(_ item: WorkSection) -> String {
        switch item { case .quality: return "Quality"; case .operations: return "Routines"; default: return item.title }
    }
    private static func icon(_ item: WorkSection) -> String {
        switch item { case .goals: return "target"; case .programs: return "doc.text"; case .quality: return "checkmark.seal"; default: return "arrow.triangle.2.circlepath" }
    }
}
