import SwiftUI

struct AttentionView: View {
    // Owned by AppTabView so the tab badge can read the same live count.
    @ObservedObject var viewModel: AttentionViewModel
    @StateObject private var themeManager = ThemeManager.shared
    @State private var activeLane = "all"
    @State private var selected: AttentionItem?
    @Environment(\.scenePhase) private var scenePhase

    private let laneTitles: [String: String] = [
        "all": "All", "requests": "Requests", "approvals": "Approvals",
        "escalations": "Escalations", "failed": "Failed"
    ]

    private func tabCount(_ lane: String) -> Int {
        viewModel.total(lane)
    }

    /// First-ever feed load — no page metadata exists yet.
    private var awaitingFirstLoad: Bool {
        viewModel.pages == nil && viewModel.isLoading
    }

    var body: some View {
        NavigationView {
            VStack(spacing: 0) {
                if viewModel.showHistory {
                    historyList
                } else {
                laneTabs
                Divider()
                bulkActionBar

                if let error = viewModel.loadError {
                    HStack(spacing: 8) {
                        Text(error).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                        Spacer()
                        Button("Retry") { viewModel.fetchUserInitiated() }
                            .disabled(viewModel.isLoading)
                    }
                    .padding()
                    .accessibilityIdentifier("attention-load-error")
                }

                if awaitingFirstLoad {
                    ProgressView("Refreshing attention…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .foregroundColor(themeManager.secondaryTextColor)
                } else if viewModel.items(activeLane).isEmpty {
                    emptyLane
                } else {
                    List {
                        ForEach(viewModel.items(activeLane)) { item in
                            AttentionListRow(item: item, theme: themeManager)
                                .contentShape(Rectangle())
                                .onTapGesture { if item.isActionable { selected = item } }
                                .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                                    // Gate on the ITEM, not the lane: `all` renders the
                                    // same failed cards, and they dismiss the same way
                                    // wherever they appear (web parity — the web All list
                                    // offers Dismiss on failed rows too). "Failed" on the
                                    // wire is a STATUS, not an item_type — the feed has no
                                    // failed FeedItemType (failed rows are task items with
                                    // status "failed"; the web gates the same way), so an
                                    // itemType check matched nothing and killed swipe
                                    // everywhere. HITL rows keep no swipe: they are
                                    // answered, not dismissed.
                                    if item.status == "failed" {
                                        // No reasons here — a full/long swipe commits the
                                        // dismiss in one gesture (the default action).
                                        Button(role: .destructive) {
                                            withAnimation(.snappy(duration: 0.22)) { viewModel.dismiss(item) }
                                        } label: {
                                            Label("Dismiss", systemImage: "xmark.bin")
                                        }
                                    }
                                }
                                // Rows are cards on the page background (web parity:
                                // bg-card on bg-base) — without this the plain List paints
                                // the system row background, pure white in light mode.
                                .listRowBackground(themeManager.cardColor)
                        }
                        if viewModel.hasMore(activeLane) {
                            Button(action: { viewModel.loadMore(activeLane) }) {
                                HStack(spacing: 8) {
                                    if viewModel.isLoadingPage(activeLane) { ProgressView().controlSize(.small) }
                                    Text(viewModel.isLoadingPage(activeLane)
                                         ? "Loading…"
                                         : "Load more · \(viewModel.loadedCount(activeLane)) of \(viewModel.total(activeLane))")
                                }
                                .frame(maxWidth: .infinity)
                                .foregroundColor(themeManager.accentColor)
                            }
                            .disabled(viewModel.isLoadingPage(activeLane))
                            .accessibilityIdentifier("attention-load-more-\(activeLane)")
                            .listRowBackground(Color.clear)
                        }
                    }
                    .listStyle(.plain)
                    // Hide the List's own system background so the themed page
                    // background shows; the row tint above carries the surface.
                    .scrollContentBackground(.hidden)
                    .refreshable { await viewModel.refresh() }
                }
                }
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle(viewModel.showHistory ? "History" : "Attention")
            .navigationBarTitleDisplayMode(.inline)
            .navigationBarItems(
                leading: HStack(spacing: 12) {
                    HamburgerButton()
                    Button(action: {
                        viewModel.showHistory.toggle()
                        if viewModel.showHistory { viewModel.fetchHistory() }
                    }) {
                        Image(systemName: viewModel.showHistory ? "list.bullet" : "clock")
                            .foregroundColor(themeManager.accentColor)
                    }
                    .accessibilityIdentifier("attention-history-toggle")
                },
                trailing: Button(action: {
                    if viewModel.showHistory { viewModel.fetchHistory() }
                    else { viewModel.fetchUserInitiated() }
                }) {
                    // Spinner only for a refresh the USER asked for: background
                    // refetches (realtime events, tab entry) run silently so a
                    // busy system doesn't look stuck "refreshing" forever.
                    if (viewModel.isLoading && viewModel.isUserRefresh) || viewModel.historyLoading {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "arrow.clockwise").foregroundColor(themeManager.accentColor)
                    }
                }
                .disabled(viewModel.isLoading || viewModel.historyLoading)
                .accessibilityLabel(viewModel.showHistory ? "Refresh attention history" : "Refresh attention")
                .accessibilityIdentifier("attention-refresh")
            )
            .onChange(of: activeLane) { _, _ in viewModel.bulkNotice = nil }
            .onAppear { viewModel.fetch(); viewModel.connectRealtime() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { viewModel.fetch() }
            }
            .onDisappear { viewModel.disconnectRealtime() }
            .onReceive(AppActions.shared.$attentionRequestID.dropFirst()) { _ in
                revealRequestedItemIfAvailable()
            }
            .onReceive(viewModel.$lanes.dropFirst()) { _ in
                revealRequestedItemIfAvailable()
            }
            .sheet(item: $selected) { item in
                AttentionDetailModal(item: item, viewModel: viewModel)
            }
        }
    }

    private func revealRequestedItemIfAvailable() {
        guard let target = AppActions.shared.attentionTargetItemID else { return }
        for lane in AttentionViewModel.laneKeys {
            if let item = viewModel.items(lane).first(where: {
                $0.id == target || $0.correlationId == target || $0.metadata?.pauseStateId == target
            }) {
                activeLane = lane
                if item.isActionable { selected = item }
                AppActions.shared.consumeAttentionTarget()
                return
            }
        }
    }

    /// Lane tabs: UIKit-backed chips (`AttentionLaneTabs`) — square-rounded,
    /// theme-token colors, larger text, horizontally scrollable when five
    /// lanes + counts outgrow the width. See that file for why the touch
    /// layer is UIKit and not SwiftUI Buttons.
    private var laneTabs: some View {
        AttentionLaneTabs(
            tabs: AttentionViewModel.laneKeys.map {
                AttentionLaneTabs.TabModel(id: $0, title: laneTitles[$0] ?? $0, count: tabCount($0))
            },
            selection: $activeLane,
            palette: AttentionLaneTabs.Palette(
                accent: UIColor(themeManager.accentColor),
                secondaryText: UIColor(themeManager.secondaryTextColor),
                chipBackground: UIColor(themeManager.cardColor),
                fontName: themeManager.fontName,
                revision: themeManager.themeRevision))
    }

    /// Web parity: a lane-level "Approve all (N)" quick action for diff_approval items,
    /// plus the transient applied/failed notice.
    @ViewBuilder private var bulkActionBar: some View {
        let diffs = viewModel.diffApprovalItems(in: activeLane)
        if !diffs.isEmpty || viewModel.bulkNotice != nil || viewModel.mutationError != nil {
            VStack(spacing: 6) {
                if !diffs.isEmpty {
                    Button(action: { viewModel.approveAllDiffApprovals(in: activeLane) }) {
                        HStack(spacing: 6) {
                            if viewModel.approvingAllDiffs {
                                ProgressView().scaleEffect(0.8)
                                Text("Applying…")
                            } else {
                                Image(systemName: "checkmark.seal")
                                Text("Approve all (\(diffs.count))")
                            }
                        }
                        .font(.themed(14, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 10)
                        .background(themeManager.accentColor.opacity(0.15))
                        .foregroundColor(themeManager.accentColor)
                        .cornerRadius(10)
                    }
                    .buttonStyle(.plain)
                    .disabled(viewModel.approvingAllDiffs)
                    .accessibilityIdentifier("attention-approve-all")
                }
                if let notice = viewModel.bulkNotice {
                    Button(action: { viewModel.bulkNotice = nil }) {
                        HStack(spacing: 6) {
                            Image(systemName: "checkmark.circle.fill").foregroundColor(themeManager.successColor)
                            Text(notice).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                            Spacer()
                            Image(systemName: "xmark").font(.caption2).foregroundColor(themeManager.secondaryTextColor)
                        }
                    }
                    .buttonStyle(.plain)
                }
                if let error = viewModel.mutationError {
                    Button(action: { viewModel.mutationError = nil }) {
                        HStack(spacing: 6) {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundColor(themeManager.dangerColor)
                            Text(error).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                            Spacer()
                            Image(systemName: "xmark").font(.caption2).foregroundColor(themeManager.secondaryTextColor)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("attention-mutation-error")
                }
            }
            .padding(.horizontal)
            .padding(.top, 8)
        }
    }

    private var emptyLane: some View {
        VStack(spacing: 12) {
            Image(systemName: viewModel.loadError == nil ? "checkmark.circle" : "exclamationmark.circle")
                .font(.system(size: 36))
                .foregroundColor(viewModel.loadError == nil ? themeManager.successColor : themeManager.secondaryTextColor)
            Text(viewModel.loadError == nil ? "Nothing here" : "Attention unavailable")
                .font(.themed(17, weight: .semibold)).foregroundColor(themeManager.secondaryTextColor)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder private var historyList: some View {
        if viewModel.resolved.isEmpty {
            VStack(spacing: 12) {
                Image(systemName: viewModel.historyLoading ? "clock" : "clock.badge.checkmark")
                    .font(.system(size: 36)).foregroundColor(themeManager.secondaryTextColor.opacity(0.6))
                Text(viewModel.historyLoading ? "Loading history…" : "No resolved requests")
                    .font(.themed(15)).foregroundColor(themeManager.secondaryTextColor)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            List(viewModel.resolved) { row in
                VStack(alignment: .leading, spacing: 4) {
                    Text(row.prompt).font(.themed(15, weight: .medium))
                        .foregroundColor(themeManager.textColor).lineLimit(2)
                    HStack(spacing: 6) {
                        Text((row.decision ?? row.outcome).capitalized)
                            .font(.themed(11)).padding(.horizontal, 6).padding(.vertical, 2)
                            .background(themeManager.accentColor.opacity(0.12))
                            .foregroundColor(themeManager.accentColor).cornerRadius(4)
                        Spacer()
                        Text(relativeTime(row.resolvedAt))
                            .font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                    }
                }
                .padding(.vertical, 2)
                .listRowBackground(themeManager.cardColor)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .refreshable { viewModel.fetchHistory() }
        }
    }

    private func relativeTime(_ msOrS: Double) -> String {
        guard msOrS > 0 else { return "" }
        let seconds = msOrS > 1_000_000_000_000 ? msOrS / 1000 : msOrS   // ms → s
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: Date(timeIntervalSince1970: seconds), relativeTo: Date())
    }
}

struct AttentionListRow: View {
    let item: AttentionItem
    @ObservedObject var theme: ThemeManager

    /// Absolute date+time — a failure has to be placeable in the day it
    /// happened, which a relative "2h ago" only answers until it isn't.
    private static let stampFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMM d, HH:mm"
        return f
    }()

    private var timeStamp: String? {
        guard let ms = item.updatedAt, ms > 0 else { return nil }
        // Web parses this as millis; clamp defensively so a seconds-scale
        // value renders this decade, not 1970.
        let seconds = ms > 1e12 ? ms / 1000 : ms
        return Self.stampFormatter.string(from: Date(timeIntervalSince1970: seconds))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(item.title).font(.themed(17, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(2)
            if let s = item.summary, !s.isEmpty {
                Text(s).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(2)
            }
            HStack(spacing: 6) {
                if item.isActionable {
                    Text(item.inputType)
                        .font(.themed(11))
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(theme.accentColor.opacity(0.15))
                        .foregroundColor(theme.accentColor)
                        .cornerRadius(4)
                }
                if !item.status.isEmpty {
                    Text(item.status).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
                if let timeStamp {
                    Text(timeStamp).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                }
                Spacer()
                if item.isActionable {
                    Image(systemName: "chevron.right").font(.caption2).foregroundColor(theme.secondaryTextColor)
                }
            }
        }
        .padding(.vertical, 4)
    }
}

/// Attention lane tabs, UIKit-backed on purpose.
///
/// Every SwiftUI variant of this strip (Buttons, `.plain`, capsule chips,
/// invisible hit extensions) passed exhaustive simulator tap sweeps while the
/// owner's device registered taps only below the drawn chip; the segmented
/// Picker — UIKit's own touch handling — was the one control that worked on
/// that device. This panel keeps that UIKit touch stack (UIScrollView +
/// UIButtons) while returning to the app's chip visuals: theme tokens,
/// square-rounded corners (radius 8, matching cards), larger title text, and
/// horizontal scrolling when five lanes + counts outgrow the width.
/// `themeRevision` rides along in the palette so a theme flip re-renders the
/// UIKit children (the same hook the toolbar content uses).
private struct AttentionLaneTabs: UIViewRepresentable {
    struct TabModel: Equatable {
        let id: String
        let title: String
        let count: Int
    }

    struct Palette: Equatable {
        let accent: UIColor
        let secondaryText: UIColor
        let chipBackground: UIColor
        let fontName: String
        let revision: UInt
    }

    let tabs: [TabModel]
    @Binding var selection: String
    let palette: Palette

    func makeUIView(context: Context) -> UIScrollView {
        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.backgroundColor = .clear
        scroll.contentInsetAdjustmentBehavior = .never
        let stack = UIStackView()
        stack.axis = .horizontal
        stack.alignment = .center
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)
        // Self-sizing strip: pin the stack to the scroll view's FRAME edges
        // vertically (height = chips + insets — a contentLayoutGuide/frame mix
        // leaves the height circular and SwiftUI falls back to the proposed
        // size, i.e. half the screen) and to the CONTENT guide horizontally so
        // wide content overflows and scrolls.
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: scroll.topAnchor, constant: 8),
            stack.bottomAnchor.constraint(equalTo: scroll.bottomAnchor, constant: -8),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -16),
        ])
        for tab in tabs { stack.addArrangedSubview(makeButton(tab)) }
        context.coordinator.stack = stack
        restyle(stack)
        return scroll
    }

    /// A UIScrollView reports no intrinsic size, so SwiftUI falls back to the
    /// PROPOSED size — which the device's layout pass set to half the screen
    /// (the simulator happened to propose compact, hiding the bug). Answer
    /// proposals with the measured chip height instead, so the proposal
    /// cannot size the strip.
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UIScrollView, context: Context) -> CGSize? {
        guard let stack = context.coordinator.stack else { return nil }
        let fitting = stack.systemLayoutSizeFitting(UIView.layoutFittingCompressedSize)
        return CGSize(width: proposal.width ?? uiView.frame.width,
                      height: fitting.height + 16)   // + top/bottom strip insets
    }

    func updateUIView(_ scroll: UIScrollView, context: Context) {
        guard let stack = context.coordinator.stack else { return }
        if stack.arrangedSubviews.count != tabs.count {
            for button in stack.arrangedSubviews { button.removeFromSuperview() }
            for tab in tabs { stack.addArrangedSubview(makeButton(tab)) }
        }
        restyle(stack)
    }

    private func makeButton(_ tab: TabModel) -> UIButton {
        let button = UIButton(type: .system)
        button.accessibilityIdentifier = "attention-lane-\(tab.id)"
        button.setContentHuggingPriority(.required, for: .horizontal)
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
        button.addAction(UIAction { [selection = _selection] _ in
            selection.wrappedValue = tab.id
        }, for: .touchUpInside)
        return button
    }

    private func restyle(_ stack: UIStackView) {
        for (index, tab) in tabs.enumerated() {
            guard let button = stack.arrangedSubviews[index] as? UIButton else { continue }
            let selected = tab.id == selection

            var title = AttributedString(tab.title)
            title.font = Self.font(size: 15, weight: .semibold, family: palette.fontName)
            title.foregroundColor = selected ? palette.accent : palette.secondaryText
            if tab.count > 0 {
                var count = AttributedString(" \(tab.count)")
                count.font = Self.font(size: 12, weight: .bold, family: palette.fontName)
                count.foregroundColor = palette.accent
                title += count
            }

            var config = UIButton.Configuration.plain()
            config.attributedTitle = title
            config.contentInsets = NSDirectionalEdgeInsets(top: 9, leading: 12, bottom: 9, trailing: 12)
            config.background.backgroundColor = selected
                ? palette.accent.withAlphaComponent(0.15)
                : palette.chipBackground
            config.background.strokeColor = palette.accent.withAlphaComponent(selected ? 0.45 : 0.14)
            config.background.strokeWidth = 1
            config.background.cornerRadius = 8
            button.configuration = config
            button.isSelected = selected
            if selected {
                button.accessibilityTraits.insert(.selected)
            } else {
                button.accessibilityTraits.remove(.selected)
            }
        }
    }

    /// Theme fonts are custom families (Manrope by default); weights resolve
    /// through the descriptor so a family without a static face still renders.
    private static func font(size: CGFloat, weight: UIFont.Weight, family: String) -> UIFont {
        let base = UIFont(name: family, size: size) ?? .systemFont(ofSize: size)
        let descriptor = base.fontDescriptor.addingAttributes([
            .traits: [UIFontDescriptor.TraitKey.weight: weight]
        ])
        return UIFont(descriptor: descriptor, size: size)
    }

    final class Coordinator {
        var stack: UIStackView?
    }

    func makeCoordinator() -> Coordinator { Coordinator() }
}
