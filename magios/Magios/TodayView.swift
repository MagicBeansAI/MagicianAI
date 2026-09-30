import SwiftUI
import UIKit
import MarkdownUI
import Combine

private struct AttentionVisibilityFramePreferenceKey: PreferenceKey {
    static var defaultValue: CGRect = .null
    static func reduce(value: inout CGRect, nextValue: () -> CGRect) { value = nextValue() }
}

private struct AttentionVerifiedVisibilityModifier: ViewModifier {
    let binding: AttentionDeliveryBinding?
    @Environment(\.scenePhase) private var scenePhase
    @State private var frame: CGRect = .null
    @State private var dwellTask: Task<Void, Never>?

    func body(content: Content) -> some View {
        content
            .background {
                GeometryReader { proxy in
                    Color.clear.preference(
                        key: AttentionVisibilityFramePreferenceKey.self,
                        value: proxy.frame(in: .global)
                    )
                }
            }
            .onPreferenceChange(AttentionVisibilityFramePreferenceKey.self) { next in
                frame = next
                reevaluate(frame: next, binding: binding, phase: scenePhase)
            }
            .onChange(of: binding?.identity) { _, _ in
                cancelDwell()
                reevaluate(frame: frame, binding: binding, phase: scenePhase)
            }
            .onChange(of: scenePhase) { _, phase in
                reevaluate(frame: frame, binding: binding, phase: phase)
            }
            .onAppear { reevaluate(frame: frame, binding: binding, phase: scenePhase) }
            .onDisappear { cancelDwell() }
    }

    private func reevaluate(
        frame: CGRect,
        binding: AttentionDeliveryBinding?,
        phase: ScenePhase
    ) {
        guard phase == .active,
              let binding,
              binding.expiresAt > Int64(Date().timeIntervalSince1970 * 1_000),
              visibleRatio(frame) >= 0.5 else {
            cancelDwell()
            return
        }
        guard AttentionImpressionLedger.shared.receipt(for: binding.identity) == nil,
              dwellTask == nil else { return }
        let startedAt = Date()
        dwellTask = Task { @MainActor in
            do {
                try await Task.sleep(for: .milliseconds(binding.minVisibleMS))
            } catch {
                return
            }
            guard !Task.isCancelled,
                  scenePhase == .active,
                  self.binding?.identity == binding.identity,
                  visibleRatio(self.frame) >= 0.5 else {
                dwellTask = nil
                return
            }
            let elapsed = max(
                binding.minVisibleMS,
                Int(Date().timeIntervalSince(startedAt) * 1_000)
            )
            await AttentionImpressionRecorder.shared.record(binding, visibleMS: elapsed)
            dwellTask = nil
        }
    }

    private func cancelDwell() {
        dwellTask?.cancel()
        dwellTask = nil
    }

    private func visibleRatio(_ frame: CGRect) -> CGFloat {
        guard !frame.isNull, !frame.isEmpty, frame.width > 0, frame.height > 0 else { return 0 }
        let window = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow)
        let viewport = window.map { $0.bounds.inset(by: $0.safeAreaInsets) }
            ?? UIScreen.main.bounds
        let intersection = frame.intersection(viewport)
        guard !intersection.isNull, !intersection.isEmpty else { return 0 }
        return (intersection.width * intersection.height) / (frame.width * frame.height)
    }
}

extension View {
    func verifiedAttentionVisibility(_ binding: AttentionDeliveryBinding?) -> some View {
        modifier(AttentionVerifiedVisibilityModifier(binding: binding))
    }
}

extension TaskStatusModel: Identifiable {
    var id: String { taskId }
}

/// Today — the "Morning Edition" (web parity with `MorningEdition.svelte`).
///
/// Page order: masthead · Realtime Wire · lead story (or slate clear) ·
/// Economics of Operations · Reading Room (Morning Brief deck | Broadsheet)
/// + hidden drawer · app widgets · § 3 briefings · § 4 completed
/// deliverables · § 5 chronicle & digest · footer. Every rich mobile flow —
/// follow-up detail/compose/writing style, resurfacing detail/actions,
/// briefing render, hide/undo — stays one tap away on its card.
struct TodayView: View {
    @StateObject private var viewModel = TodayViewModel()
    @StateObject private var followUpActions = ChannelFollowUpActionClient()
    @StateObject private var theme = ThemeManager.shared
    @State private var selectedItem: TodayItem?
    @State private var selectedTask: TaskStatusModel?
    @State private var selectedResurfacing: ResurfacingCard?
    @State private var selectedResurfacingActions: ResurfacingCard?
    @State private var selectedBriefing: TodayBriefing?
    @State private var selectedFollowUp: ChannelFollowUp?
    @State private var hiddenExpanded = false
    @State private var wireExpanded = false
    @State private var showingActivity = false
    @State private var activitySearch = ""
    @State private var showAllBriefings = false
    @State private var deliveredShown = TodayView.deliveredPageSize
    @State private var showUndo = false
    @AppStorage("todayBroadsheetTab") private var broadsheetTabRaw = TodayBroadsheetTab.forYou.rawValue
    /// Drives the greeting navigation title across hour boundaries.
    @State private var clock = Date()

    private static let deliveredPageSize = 6
    private static let minuteTicker = Timer.publish(every: 60, on: .main, in: .common).autoconnect()
    private static let briefingPreviewCount = 6

    private enum Anchor: Hashable {
        case top, lead, readingRoom, briefings, delivered, chronicle
    }

    var body: some View {
        NavigationView {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 28) {
                        TodayMasthead()
                            .id(Anchor.top)
                        if let error = viewModel.error {
                            errorBanner(error)
                        }

                        if viewModel.isLoading && viewModel.payload == nil {
                            loadingState
                        } else {
                            // The wire and the lead/slate row read as one status
                            // band, so they sit closer than the page's section gap.
                            VStack(alignment: .leading, spacing: 10) {
                                TodayRealtimeWireView(
                                    items: viewModel.wireItems,
                                    count24h: viewModel.wireEventCount24h,
                                    expanded: $wireExpanded,
                                    onOpen: openWire,
                                    onActivity: { showingActivity = true }
                                )
                                leadStory.id(Anchor.lead)
                            }
                            TodayOperationsCarousel(
                                pulse: viewModel.pulse,
                                errorMessage: viewModel.sectionErrors["pulse"],
                                onRetry: viewModel.fetch,
                                onTasks: { AppActions.shared.requestTask(nil) },
                                onOpenTask: { AppActions.shared.requestTask($0) }
                            )
                            readingRoom.id(Anchor.readingRoom)
                            PinnedAppsTodaySection()
                            AppNativeSlotPageRegion(
                                page: "/",
                                regions: ["primary", "secondary"],
                                accessibilityLabel: "Today app widgets"
                            )
                            briefingsSection.id(Anchor.briefings)
                            deliveredSection.id(Anchor.delivered)
                            chronicleSection.id(Anchor.chronicle)
                            footer
                        }
                    }
                    .padding(.horizontal, 16)
                    // Clear of the floating tab bar so the footer is readable.
                    .padding(.bottom, 96)
                }
                .background(theme.backgroundColor.ignoresSafeArea())
                .onReceive(AppActions.shared.$todayRequestID.dropFirst()) { _ in handleTodayRequest(proxy) }
            }
            .overlay(alignment: .bottom) { undoToast }
            // The masthead already says "Today's"; the bar carries the
            // greeting instead (the tab label stays "Today").
            .navigationTitle(TodayMorningEdition.greeting(for: clock))
            .onReceive(Self.minuteTicker) { clock = $0 }
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(theme.backgroundColor, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbarColorScheme(theme.colorScheme, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    HamburgerButton()
                }
                ToolbarItem(placement: .navigationBarTrailing) {
                    Button(action: viewModel.fetch) {
                        if viewModel.isLoading {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Image(systemName: "arrow.clockwise")
                                .font(.system(size: 15, weight: .semibold))
                                .foregroundColor(theme.accentColor)
                        }
                    }
                    .disabled(viewModel.isLoading)
                    .accessibilityLabel("Refresh Today")
                    .accessibilityIdentifier("today-refresh")
                }
            }
            .refreshable { viewModel.fetch() }
            .onAppear {
                viewModel.start()
#if DEBUG
                if ProcessInfo.processInfo.arguments.contains("--today-ui-test-open-followup") {
                    selectedFollowUp = viewModel.visibleMessageFollowUps.first
                }
                if ProcessInfo.processInfo.arguments.contains("--today-ui-test-expand-secondary") {
                    hiddenExpanded = true
                    wireExpanded = true
                }
#endif
            }
            .onDisappear { viewModel.stop() }
            .onChange(of: viewModel.lastHiddenItem?.id) { _, value in
                withAnimation { showUndo = value != nil }
                guard value != nil else { return }
                DispatchQueue.main.asyncAfter(deadline: .now() + 5) { withAnimation { showUndo = false } }
            }
            .sheet(item: $selectedItem, content: itemSheet)
            .sheet(item: $selectedTask, content: taskSheet)
            .sheet(item: $selectedResurfacing, content: resurfacingSheet)
            .sheet(item: $selectedResurfacingActions, content: resurfacingActionsSheet)
            .sheet(item: $selectedBriefing, content: briefingSheet)
            .sheet(item: $selectedFollowUp, content: followUpSheet)
            .sheet(isPresented: $showingActivity) {
                TodayActivitySheet(viewModel: viewModel, search: $activitySearch) { item in
                    showingActivity = false
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { openActivity(item) }
                }
            }
        }
    }

    /// `AppActions.requestToday(section:activity:)` — deep links, widgets and
    /// other surfaces land on the matching Morning Edition section.
    private func handleTodayRequest(_ proxy: ScrollViewProxy) {
        let actions = AppActions.shared
        if actions.todayShowActivity { showingActivity = true }
        if let section = actions.todayTargetSection {
            let anchor: Anchor
            switch section {
            case .needsYou:
                anchor = .lead
            case .followups, .worthALook:
                viewModel.readingRoomMode = .broadsheet
                broadsheetTabRaw = (section == .worthALook ? TodayBroadsheetTab.worth : .forYou).rawValue
                anchor = .readingRoom
            case .delivered:
                anchor = viewModel.items(for: .delivered).isEmpty ? .briefings : .delivered
            case .changed:
                anchor = viewModel.digest.bullets.isEmpty ? .top : .chronicle
            case .activeWork:
                anchor = .top
            }
            DispatchQueue.main.async {
                withAnimation(.easeInOut(duration: 0.25)) { proxy.scrollTo(anchor, anchor: .top) }
            }
        }
        actions.consumeTodayTarget()
    }

    private func itemSheet(_ item: TodayItem) -> some View {
        TodayItemDetailView(item: item, viewModel: viewModel)
    }

    private func taskSheet(_ task: TaskStatusModel) -> some View { DeepWorkPanel(task: task) }
    private func resurfacingSheet(_ card: ResurfacingCard) -> some View { ResurfacingDetailView(card: card, viewModel: viewModel) }
    private func resurfacingActionsSheet(_ card: ResurfacingCard) -> some View {
        ResurfacingActionsSheet(card: card, viewModel: viewModel) {
            selectedResurfacingActions = nil
            DispatchQueue.main.async { selectedResurfacing = card }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
    private func briefingSheet(_ briefing: TodayBriefing) -> some View { TodayBriefingDetailView(briefing: briefing, viewModel: viewModel) }

    private func followUpSheet(_ item: ChannelFollowUp) -> some View {
        ChannelFollowUpDetailView(
            item: item,
            client: followUpActions,
            onResolved: {
                withAnimation(.snappy(duration: 0.22)) {
                    viewModel.removeMessageFollowUp(id: item.id)
                }
                selectedFollowUp = nil
            },
            onResolutionRequested: { action, hint, reason in
                withAnimation(.snappy(duration: 0.22)) {
                    viewModel.resolveFollowUp(item, action: action, hint: hint, reason: reason)
                }
                selectedFollowUp = nil
            }
        )
    }

    // MARK: Lead story

    @ViewBuilder private var leadStory: some View {
        let urgent = viewModel.items(for: .needsYou)
        if let lead = urgent.first {
            let total = max(urgent.count, viewModel.coreCount(for: .needsYou))
            VStack(alignment: .leading, spacing: 10) {
                TodayKicker(text: "The Lead Story · Urgent Decision", color: theme.accentColor)
                Text(lead.title)
                    .font(TodayNewsprint.serif(27, weight: .bold))
                    .foregroundColor(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true)
                if let body = TodayMorningEdition.firstNonEmpty(lead.reason, lead.summary) {
                    Text(body)
                        .font(.themed(15))
                        .foregroundColor(theme.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 12) {
                    Button { open(lead) } label: {
                        Text("Take action now →")
                            .font(.themed(13, weight: .bold))
                            .padding(.horizontal, 14).frame(height: 38)
                            .foregroundColor(theme.onAccentColor)
                            .background(theme.accentColor)
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("today-lead-action")
                    if total > 1 {
                        Button { AppActions.shared.requestAttention() } label: {
                            Text("+\(total - 1) more urgent →")
                                .font(.themed(13, weight: .semibold))
                                .foregroundColor(theme.accentColor)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("today-lead-more")
                    }
                }
                .padding(.top, 2)
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.accentColor.opacity(0.06))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.accentColor.opacity(0.35)))
        } else if viewModel.payload != nil {
            (Text("☕  ")
                + Text("Slate is Clear").font(TodayNewsprint.serif(15, weight: .bold)).foregroundColor(theme.textColor)
                + Text(" · Nothing needs attention.").font(.themed(13)).foregroundColor(theme.secondaryTextColor))
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.successColor.opacity(0.10))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.successColor.opacity(0.3)))
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("today-slate-clear")
        }
    }

    // MARK: Reading Room

    private var readingRoom: some View {
        VStack(alignment: .leading, spacing: 14) {
            // One row: the title shrinks (to ~16pt) before the switch wraps.
            HStack(alignment: .center, spacing: 10) {
                Text("Reading Room")
                    .font(TodayNewsprint.serif(22, weight: .bold))
                    .foregroundColor(theme.textColor)
                    .lineLimit(1)
                    .minimumScaleFactor(0.72)
                    .accessibilityAddTraits(.isHeader)
                Spacer(minLength: 6)
                readingModeSwitcher.fixedSize()
            }
            switch viewModel.readingRoomMode {
            case .deck:
                TodayMorningBriefDeck(
                    cards: TodayMorningEdition.interleaveDeck(
                        followUps: viewModel.visibleMessageFollowUps,
                        worth: viewModel.visibleResurfacingCards
                    ),
                    isLoadingMore: viewModel.isDeckLoadingMore,
                    onNeedMore: { viewModel.loadMoreDeckCards() },
                    onAction: performDeckAction,
                    onOpen: openDeckCard,
                    onOpenBroadsheet: { withAnimation(.easeInOut(duration: 0.2)) { viewModel.readingRoomMode = .broadsheet } }
                )
                if let error = viewModel.sectionErrors["message_followups"] {
                    sectionError(error) { viewModel.loadRemainingMessageFollowUps() }
                }
                if let error = viewModel.sectionErrors[TodaySection.worthALook.rawValue] {
                    sectionError(error) { viewModel.loadRemainingResurfacing() }
                }
            case .broadsheet:
                broadsheet
            }
            hiddenSection
        }
    }

    private var readingModeSwitcher: some View {
        let total = viewModel.visibleMessageFollowUpTotal + viewModel.visibleResurfacingTotal
        return HStack(spacing: 2) {
            modeButton("🃏 Brief \(TodayMorningEdition.deckCountLabel(total))", mode: .deck)
            modeButton("📰 Broadsheet", mode: .broadsheet)
        }
        .padding(2)
        .background(theme.secondaryTextColor.opacity(0.08))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func modeButton(_ title: String, mode: TodayReadingMode) -> some View {
        let selected = viewModel.readingRoomMode == mode
        return Button {
            withAnimation(.easeInOut(duration: 0.2)) { viewModel.readingRoomMode = mode }
        } label: {
            Text(title)
                .font(.themed(12, weight: .semibold))
                .lineLimit(1)
                .padding(.horizontal, 9).padding(.vertical, 6)
                .foregroundColor(selected ? theme.textColor : theme.secondaryTextColor)
                .background(selected ? theme.cardColor : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(theme.cardBorderColor.opacity(selected ? 1 : 0)))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityIdentifier("today-reading-mode-\(mode.rawValue)")
    }

    /// Deck commits map onto the existing optimistic view-model calls
    /// (remove + rollback + receipts + attribution + tombstones).
    private func performDeckAction(_ card: TodayDeckCard, _ action: TodayDeckAction,
                                   completion: @escaping (Bool) -> Void) {
        switch card.source {
        case .followUp(let item):
            let verb: String
            switch action {
            case .useful: verb = "useful"
            case .acknowledge: verb = "acknowledge"
            case .dismiss: verb = "dismiss"
            case .primary: verb = "approve"
            }
            viewModel.resolveFollowUp(item, action: verb, completion: completion)
        case .worth(let worth):
            switch action {
            case .useful: viewModel.resolveResurfacing(worth, action: .open, completion: completion)
            case .acknowledge: viewModel.resolveResurfacing(worth, action: .acknowledge, completion: completion)
            case .dismiss: viewModel.resolveResurfacing(worth, action: .dismiss, completion: completion)
            case .primary:
                viewModel.resolveResurfacing(worth, action: .open, completion: completion)
                openWorthSource(worth)
            }
        }
    }

    private func openDeckCard(_ card: TodayDeckCard) {
        switch card.source {
        case .followUp(let item): selectedFollowUp = item
        case .worth(let worth): selectedResurfacing = worth
        }
    }

    /// ⚡ Open on a worth card: the linked source when there is one, otherwise
    /// the resurfacing detail (which fetches the source's current route/link).
    private func openWorthSource(_ card: ResurfacingCard) {
        if let raw = card.openURL, raw.hasPrefix("http"), let url = URL(string: raw) {
            UIApplication.shared.open(url)
        } else if let route = card.sourceRoute, route.hasPrefix("/tasks") {
            AppActions.shared.requestTask(Monitors.parseTasksRoute(route)?.taskID
                ?? URLComponents(string: route)?.queryItems?.first(where: { $0.name == "selected" })?.value)
        } else if let route = card.sourceRoute, route.hasPrefix("/t/") {
            AppActions.shared.requestThread(String(route.dropFirst(3)).removingPercentEncoding)
        } else {
            selectedResurfacing = card
        }
    }

    /// Broadsheet: its own For You / Worth a Look tabs over one server-paged
    /// column — a fixed-height, independently scrolling container and a
    /// `ServerPager`-style pager (5 per page). Core Today `followups` items
    /// sit at the top of For You page 1 only.
    private var broadsheet: some View {
        let tab = TodayBroadsheetTab(rawValue: broadsheetTabRaw) ?? .forYou
        let window = viewModel.broadsheetWindow(tab)
        let core = tab == .forYou && window.page == 1 ? viewModel.items(for: .followups) : []
        let loading = viewModel.broadsheetLoading.contains(tab)
        return VStack(alignment: .leading, spacing: 12) {
            broadsheetTabs(selected: tab)
            switch tab {
            case .forYou:
                let total = window.total + viewModel.coreCount(for: .followups)
                columnHeader("For You", count: "\(total) item\(total == 1 ? "" : "s")",
                             subtitle: "Messages and threads needing a decision or reply.")
            case .worth:
                columnHeader("Worth a look", count: "\(window.total) spark\(window.total == 1 ? "" : "s")",
                             subtitle: "Resurfaced memory, project notes, and relevant knowledge.")
            }
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: true) {
                    VStack(alignment: .leading, spacing: 11) {
                        Color.clear.frame(height: 0).id("broadsheet-top")
                        if !viewModel.isBroadsheetLoaded(tab) && loading {
                            ProgressView().tint(theme.accentColor)
                                .frame(maxWidth: .infinity, minHeight: 380)
                        } else if tab == .forYou {
                            let messages = viewModel.visibleBroadsheetFollowUps
                            if core.isEmpty && messages.isEmpty {
                                broadsheetEmpty("No dispatches waiting. Inbox is calm.")
                            } else {
                                ForEach(core) { item in todayRow(item) }
                                if viewModel.coreCount(for: .followups) > core.count && !core.isEmpty {
                                    loadMoreButton("Load more · \(core.count) of \(viewModel.coreCount(for: .followups))",
                                                   loadingKey: "lane:\(TodaySection.followups.rawValue)") {
                                        viewModel.loadRemaining(.followups)
                                    }
                                }
                                ForEach(messages) { item in messageFollowUpCard(item) }
                            }
                        } else {
                            let cards = viewModel.visibleBroadsheetWorth
                            if cards.isEmpty {
                                broadsheetEmpty("Nothing worth a look right now. Your library is resting.")
                            } else {
                                ForEach(cards) { card in resurfacingCard(card) }
                            }
                        }
                    }
                    .padding(10)
                }
                .frame(height: 460)
                .background(theme.surfaceColor.opacity(0.5))
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.cardBorderColor))
                .onChange(of: window.page) { _, _ in proxy.scrollTo("broadsheet-top", anchor: .top) }
                .onChange(of: broadsheetTabRaw) { _, _ in proxy.scrollTo("broadsheet-top", anchor: .top) }
            }
            broadsheetPager(tab, window: window, loading: loading)
            if let error = viewModel.sectionErrors["broadsheet:\(tab.rawValue)"] {
                sectionError(error) { viewModel.loadBroadsheetPage(tab, page: window.page) }
            }
            if tab == .forYou, let error = viewModel.sectionErrors[TodaySection.followups.rawValue] {
                sectionError(error) { viewModel.loadRemaining(.followups) }
            }
        }
        .onAppear { viewModel.ensureBroadsheetLoaded(tab) }
        .onChange(of: broadsheetTabRaw) { _, raw in
            viewModel.ensureBroadsheetLoaded(TodayBroadsheetTab(rawValue: raw) ?? .forYou)
        }
    }

    private func broadsheetTabs(selected: TodayBroadsheetTab) -> some View {
        HStack(spacing: 6) {
            ForEach(TodayBroadsheetTab.allCases) { option in
                let isSelected = option == selected
                let count = viewModel.broadsheetWindow(option).total
                    + (option == .forYou ? viewModel.coreCount(for: .followups) : 0)
                Button { broadsheetTabRaw = option.rawValue } label: {
                    HStack(spacing: 5) {
                        Text(option.title).font(.themed(12, weight: .semibold)).lineLimit(1)
                        if count > 0 {
                            Text("\(count)")
                                .font(.themedMono(10, weight: .bold))
                                .foregroundColor(isSelected ? theme.onAccentColor : theme.accentColor)
                                .padding(.horizontal, 5).padding(.vertical, 1)
                                .background(isSelected ? theme.onAccentColor.opacity(0.22) : theme.accentColor.opacity(0.12))
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                        }
                    }
                    .foregroundColor(isSelected ? theme.onAccentColor : theme.textColor)
                    .padding(.horizontal, 10).frame(height: 30)
                    .background(isSelected ? theme.accentColor : theme.cardColor)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor.opacity(isSelected ? 0 : 1)))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(isSelected ? .isSelected : [])
                .accessibilityIdentifier("today-broadsheet-tab-\(option.rawValue)")
            }
            Spacer(minLength: 0)
        }
    }

    private func broadsheetPager(_ tab: TodayBroadsheetTab, window: TodayPageWindow, loading: Bool) -> some View {
        HStack(spacing: 10) {
            Text(window.rangeLabel)
                .font(.themedMono(11))
                .foregroundColor(theme.secondaryTextColor)
                .accessibilityIdentifier("today-broadsheet-range")
            Spacer()
            if loading { ProgressView().controlSize(.small) }
            Button { viewModel.loadBroadsheetPage(tab, page: window.page - 1) } label: {
                Image(systemName: "chevron.left").font(.system(size: 13, weight: .semibold)).frame(width: 30, height: 30)
            }
            .disabled(!window.hasPrevious || loading)
            .accessibilityLabel("Previous page")
            .accessibilityIdentifier("today-broadsheet-prev")
            Text(window.pageLabel)
                .font(.themedMono(11, weight: .semibold))
                .foregroundColor(theme.textColor)
            Button { viewModel.loadBroadsheetPage(tab, page: window.page + 1) } label: {
                Image(systemName: "chevron.right").font(.system(size: 13, weight: .semibold)).frame(width: 30, height: 30)
            }
            .disabled(!window.hasNext || loading)
            .accessibilityLabel("Next page")
            .accessibilityIdentifier("today-broadsheet-next")
        }
        .foregroundColor(theme.accentColor)
    }

    private func broadsheetEmpty(_ text: String) -> some View {
        Text(text)
            .font(TodayNewsprint.serif(14, italic: true))
            .foregroundColor(theme.secondaryTextColor)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity, minHeight: 380)
    }

    private func columnHeader(_ title: String, count: String, subtitle: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text(title)
                    .font(TodayNewsprint.serif(20, weight: .bold))
                    .foregroundColor(theme.textColor)
                Spacer()
                TodayKicker(text: count, size: 10)
            }
            Text(subtitle)
                .font(TodayNewsprint.serif(13, italic: true))
                .foregroundColor(theme.secondaryTextColor)
            TodayHairline().padding(.top, 3)
        }
    }

    private func emptyColumn(_ text: String) -> some View {
        Text(text)
            .font(TodayNewsprint.serif(14, italic: true))
            .foregroundColor(theme.secondaryTextColor)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.cardBorderColor, style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
    }

    private func resurfacingCard(_ card: ResurfacingCard) -> some View {
        TodaySwipeActionCard(
            itemID: card.id,
            leadingActions: [
                TodaySwipeCardAction(
                    id: "mark-useful",
                    title: "Mark useful",
                    systemImage: "hand.thumbsup.fill",
                    color: theme.successColor,
                    run: {
                        withAnimation(.snappy(duration: 0.22)) {
                            viewModel.resolveResurfacing(card, action: .open)
                        }
                    }
                )
            ],
            trailingActions: [
                // Full-swipe default = Dismiss (negative). A short swipe also
                // reveals Acknowledge (neutral) — web parity with ResurfacingBand.
                TodaySwipeCardAction(
                    id: "dismiss",
                    title: "Dismiss",
                    systemImage: "xmark",
                    color: theme.dangerColor,
                    run: {
                        withAnimation(.snappy(duration: 0.22)) {
                            viewModel.resolveResurfacing(card, action: .dismiss)
                        }
                    }
                ),
                TodaySwipeCardAction(
                    id: "acknowledge",
                    title: "Acknowledge",
                    systemImage: "checkmark.circle",
                    color: theme.secondaryTextColor,
                    run: {
                        withAnimation(.snappy(duration: 0.22)) {
                            viewModel.resolveResurfacing(card, action: .acknowledge)
                        }
                    }
                )
            ]
        ) {
            VStack(alignment: .leading, spacing: 8) {
                VStack(alignment: .leading, spacing: 7) {
                    HStack(spacing: 6) {
                        TodayKicker(text: TodayDeckCard(source: .worth(card)).category, color: theme.discoveryColor, size: 9)
                        Spacer(minLength: 4)
                        Image(systemName: "chevron.right").font(.caption2).foregroundColor(theme.secondaryTextColor)
                    }
                    Text(TodayMorningEdition.firstNonEmpty(card.line, card.sourceTitle) ?? "Resurfaced Note")
                        .font(TodayNewsprint.serif(18, weight: .semibold)).foregroundColor(theme.textColor)
                        .lineLimit(3)
                    if !card.whyNow.isEmpty {
                        Text(card.whyNow).font(.themed(12, weight: .medium)).foregroundColor(theme.discoveryColor)
                    }
                    if !card.summary.isEmpty {
                        Text(card.summary).font(.themed(13)).foregroundColor(theme.secondaryTextColor).lineLimit(4)
                    }
                }
                .contentShape(Rectangle())
                .onTapGesture { if viewModel.actionItemID == nil { selectedResurfacing = card } }
                // One element: an identifier on a plain container is copied
                // onto every child, which made the card query ambiguous.
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("today-resurfacing-\(card.id)")
                .accessibilityAddTraits(.isButton)
                TodayHairline().padding(.vertical, 2)
                HStack(spacing: 6) {
                    Button { selectedResurfacingActions = card } label: {
                        Label("Actions", systemImage: "ellipsis.circle")
                    }
                    .buttonStyle(TodayCompactActionButtonStyle(tint: theme.accentColor))
                    .disabled(viewModel.actionItemID != nil)
                    .accessibilityIdentifier("today-resurfacing-actions-\(card.id)")
                    // Resurfacing has no snooze; dismissal reasons are the
                    // resurfacing vocabulary only.
                    Menu {
                        ForEach(ResurfacingDismissOption.all) { option in
                            Button(option.label, role: option.code == nil ? nil : .destructive) {
                                withAnimation(.snappy(duration: 0.22)) {
                                    viewModel.resolveResurfacing(card, action: .dismiss, reason: option.code)
                                }
                            }
                        }
                    } label: {
                        Label("Dismiss", systemImage: "chevron.down")
                    }
                    .buttonStyle(TodayCompactActionButtonStyle(tint: theme.dangerColor))
                    .accessibilityIdentifier("today-resurfacing-dismiss-reason-\(card.id)")
                }
            }
            .todayRowSurface(theme)
        }
        .opacity(viewModel.actionItemID == card.id ? 0.55 : 1)
        .verifiedAttentionVisibility(card.deliveryBinding)
    }

    private func messageFollowUpCard(_ item: ChannelFollowUp) -> some View {
        // Web parity (ChannelFollowUpActions): a long/full swipe dismisses
        // (trailing, one gesture, no reason) or marks Useful (leading); the
        // labeled Useful / Acknowledge / Dismiss▾ buttons live inline below.
        TodaySwipeActionCard(
            itemID: item.id,
            leadingActions: [
                TodaySwipeCardAction(
                    id: "useful",
                    title: "Useful",
                    systemImage: "hand.thumbsup.fill",
                    color: theme.successColor,
                    run: { resolveFollowUp(item, action: "useful") }
                )
            ],
            trailingActions: [
                TodaySwipeCardAction(
                    id: "dismiss",
                    title: "Dismiss",
                    systemImage: "xmark",
                    color: theme.dangerColor,
                    run: { resolveFollowUp(item, action: "dismiss") }
                )
            ]
        ) {
            VStack(alignment: .leading, spacing: 10) {
                VStack(alignment: .leading, spacing: 7) {
                    HStack {
                        TodayKicker(text: dispatchKicker(item), color: theme.accentColor, size: 9)
                        Spacer(minLength: 4)
                        if let received = item.receivedAt {
                            Text(TodayViewModel.relativeTime(received)).font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                    Text(TodayMorningEdition.firstNonEmpty(item.subject) ?? "Untitled Message")
                        .font(TodayNewsprint.serif(18, weight: .semibold)).foregroundColor(theme.textColor)
                        .lineLimit(3)
                    if let summary = TodayMorningEdition.firstNonEmpty(item.summary, item.reason) {
                        Text(summary).font(.themed(13)).foregroundColor(theme.secondaryTextColor).lineLimit(4)
                    }
                    if let action = item.actionSummary {
                        Text(action).font(.themed(11, weight: .medium)).foregroundColor(theme.warningColor).lineLimit(3)
                    }
                    HStack(spacing: 8) {
                        Label("Review actions", systemImage: "slider.horizontal.3")
                            .font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                        if item.openURL != nil {
                            Label("Open available", systemImage: "arrow.up.right")
                                .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                        }
                    }
                }
                .contentShape(Rectangle())
                .onTapGesture { selectedFollowUp = item }
                .accessibilityAddTraits(.isButton)
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        if let action = item.availableActions.first {
                            Button { selectedFollowUp = item } label: {
                                Label(action.label, systemImage: action.systemImage)
                            }
                            .buttonStyle(TodayCompactActionButtonStyle(tint: theme.accentColor))
                        }
                        Button { resolveFollowUp(item, action: "useful") } label: {
                            Label("Useful", systemImage: "hand.thumbsup.fill")
                        }
                        .buttonStyle(TodayCompactActionButtonStyle(tint: theme.successColor))
                        .accessibilityIdentifier("today-message-useful-\(item.id)")
                        if item.canAcknowledge {
                            Button { resolveFollowUp(item, action: "acknowledge") } label: {
                                Label("Acknowledge", systemImage: "checkmark.circle")
                            }
                            .buttonStyle(TodayCompactActionButtonStyle(tint: theme.secondaryTextColor))
                            .accessibilityIdentifier("today-message-acknowledge-\(item.id)")
                        }
                        // Split "Dismiss" (one-click, no reason) with a caret menu for
                        // the optional reason — parity with the web ▾ split button.
                        Button { resolveFollowUp(item, action: "dismiss") } label: {
                            Label("Dismiss", systemImage: "xmark")
                        }
                        .buttonStyle(TodayCompactActionButtonStyle(tint: theme.dangerColor))
                        .accessibilityIdentifier("today-message-dismiss-\(item.id)")
                        Menu {
                            ForEach(ChannelFollowUpDismissOption.all) { option in
                                Button(option.label, role: option.code == nil ? nil : .destructive) {
                                    resolveFollowUp(item, action: "dismiss", reason: option.code)
                                }
                            }
                        } label: {
                            Label("Reason", systemImage: "chevron.down")
                        }
                        .buttonStyle(TodayCompactActionButtonStyle(tint: theme.dangerColor))
                        .accessibilityIdentifier("today-message-dismiss-reason-\(item.id)")
                        // A channel follow-up snooze carries no duration on the
                        // wire: it hides the card from Today (no time picker).
                        Button { resolveFollowUp(item, action: "snooze") } label: {
                            Label(Self.followUpSnoozeLabel, systemImage: "clock")
                        }
                        .buttonStyle(TodayCompactActionButtonStyle(tint: theme.discoveryColor))
                        .accessibilityIdentifier("today-message-snooze-\(item.id)")
                    }
                }
            }
            .todayRowSurface(theme)
            .accessibilityIdentifier("today-message-followup-\(item.id)")
        }
        .opacity(viewModel.actionItemID == item.id ? 0.55 : 1)
        .verifiedAttentionVisibility(item.deliveryBinding)
    }

    static let followUpSnoozeLabel = "Snooze — hide from Today"

    private func dispatchKicker(_ item: ChannelFollowUp) -> String {
        let provider = item.provider.trimmingCharacters(in: .whitespacesAndNewlines)
        var parts = ["DISPATCH", provider.isEmpty || provider == "unknown" ? "CORRESPONDENCE" : provider]
        if let sender = TodayMorningEdition.firstNonEmpty(item.sender) { parts.append(sender) }
        return parts.joined(separator: " · ")
    }

    private func todayRow(_ item: TodayItem) -> some View {
        TodaySwipeActionCard(
            itemID: item.id,
            leadingActions: [
                TodaySwipeCardAction(
                    id: "open",
                    title: "Open",
                    systemImage: "arrow.up.right",
                    color: theme.accentColor,
                    run: { open(item) }
                )
            ],
            trailingActions: [
                TodaySwipeCardAction(
                    id: "dismiss",
                    title: "Dismiss",
                    systemImage: "xmark",
                    color: theme.dangerColor,
                    run: { dismissTodayItem(item) }
                )
            ]
        ) {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(alignment: .firstTextBaseline) {
                        TodayKicker(text: "Follow-up · \(sourceLabel(item.sourceKind))",
                                    color: item.status == "failed" ? theme.dangerColor : theme.accentColor, size: 9)
                        Spacer(minLength: 8)
                        Text(TodayViewModel.relativeTime(item.updatedAt))
                            .font(.themed(10)).foregroundColor(theme.secondaryTextColor).fixedSize()
                    }
                    Text(item.title)
                        .font(TodayNewsprint.serif(18, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(3)
                    if !item.reason.isEmpty {
                        Text(item.reason).font(.themed(12, weight: .medium)).foregroundColor(theme.warningColor).lineLimit(3)
                    }
                    if let summary = item.summary, !summary.isEmpty {
                        Text(summary).font(.themed(13)).foregroundColor(theme.secondaryTextColor).lineLimit(4)
                    }
                    ForEach(item.learnedItems.prefix(2)) { learned in
                        HStack(alignment: .top, spacing: 5) {
                            Image(systemName: "sparkles").font(.system(size: 9)).foregroundColor(theme.discoveryColor).padding(.top, 3)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(learned.title).font(.themed(11, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(2)
                                if let summary = learned.summary { Text(summary).font(.themed(10)).foregroundColor(theme.secondaryTextColor).lineLimit(2) }
                            }
                        }
                    }
                    let chips = ([item.agentID].compactMap { $0 } + item.spaceIDs.prefix(2).map(Self.titleCase))
                    if !chips.isEmpty {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 5) { ForEach(chips, id: \.self) { chip($0) } }
                        }
                    }
                }
                .contentShape(Rectangle())
                .onTapGesture { if viewModel.actionItemID == nil { open(item) } }
                // One element: an identifier on a plain container is copied
                // onto every child, which made the card query ambiguous.
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("today-item-\(item.id)")
                .accessibilityAddTraits(.isButton)
                TodayHairline().padding(.vertical, 9)
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(item.executableTodayActions.prefix(2)) { action in
                            Button { perform(action, for: item) } label: {
                                Label(viewModel.actionItemID == item.id ? "Working…" : action.label,
                                      systemImage: action.systemImage)
                            }
                            .buttonStyle(TodayCompactActionButtonStyle(tint: theme.accentColor))
                            .disabled(viewModel.actionItemID != nil)
                        }
                        // Core Today follow-ups snooze through
                        // `/today/items/{id}/visibility` with `snooze_minutes`,
                        // so they keep the time picker.
                        Menu {
                            Button("Until tonight") { snooze(item, .tonight) }
                            Button("Tomorrow morning") { snooze(item, .tomorrowMorning) }
                            Button("Next week") { snooze(item, .nextWeek) }
                        } label: {
                            Label("Snooze", systemImage: "clock")
                        }
                        .buttonStyle(TodayCompactActionButtonStyle(tint: theme.discoveryColor))
                        .disabled(viewModel.actionItemID != nil)
                        .accessibilityIdentifier("today-snooze-menu-\(item.id)")
                    }
                }
            }
            .todayRowSurface(theme)
            .opacity(viewModel.actionItemID == item.id ? 0.55 : 1)
            .contextMenu {
                Button { open(item) } label: { Label("Open", systemImage: "arrow.up.right") }
                ForEach(item.executableTodayActions) { action in
                    Button { perform(action, for: item) } label: {
                        Label(action.label, systemImage: action.systemImage)
                    }
                }
                Menu("Snooze") {
                    Button("Until tonight") { snooze(item, .tonight) }
                    Button("Tomorrow morning") { snooze(item, .tomorrowMorning) }
                    Button("Next week") { snooze(item, .nextWeek) }
                }
                Button(role: .destructive) { dismissTodayItem(item) } label: {
                    Label("Dismiss", systemImage: "xmark")
                }
            }
        }
    }

    @ViewBuilder private var hiddenSection: some View {
        if !viewModel.hiddenItems.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Button {
                    withAnimation(.easeInOut(duration: 0.18)) { hiddenExpanded.toggle() }
                } label: {
                    HStack {
                        Text("\(viewModel.hiddenItems.count) hidden · \(hiddenExpanded ? "Hide" : "Show")")
                        Spacer()
                        Image(systemName: hiddenExpanded ? "chevron.down" : "chevron.right")
                    }
                    .font(.themed(13, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("today-hidden-toggle")

                if hiddenExpanded {
                    Text("Dismissed or snoozed cards can be restored here.")
                        .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                    ForEach(viewModel.hiddenItems) { item in
                        HStack(alignment: .top, spacing: 10) {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(item.record.snapshot?.title ?? "Hidden Today item")
                                    .font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
                                    .accessibilityIdentifier("today-hidden-title-\(item.id)")
                                Text(Self.titleCase(item.hiddenKind))
                                    .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                                if item.hiddenKind == "snoozed" {
                                    Text("Snoozed \(TodayViewModel.futureDistance(item.record.snoozedUntil))")
                                        .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                                } else if let dismissed = item.record.dismissedAt {
                                    Text("Dismissed \(TodayViewModel.relativeTime(dismissed))")
                                        .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                                }
                                if let reason = item.record.snapshot?.summary ?? item.record.snapshot?.reason, !reason.isEmpty {
                                    Text(reason).font(.themed(11)).foregroundColor(theme.secondaryTextColor).lineLimit(2)
                                }
                            }
                            Spacer()
                            Button("Restore") { viewModel.restore(item) }
                                .font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                        }
                        .padding(.vertical, 5)
                    }
                }
            }
            .padding(12)
            .background(theme.surfaceColor.opacity(0.55))
            .cornerRadius(10)
        }
    }

    // MARK: § 3 Special Reports & Briefings

    private var briefingsSection: some View {
        let shown = showAllBriefings ? viewModel.briefings : Array(viewModel.briefings.prefix(Self.briefingPreviewCount))
        return VStack(alignment: .leading, spacing: 12) {
            TodaySectionBanner(marker: "§ 3", title: "Special Reports & Briefings",
                               subtitle: "Curated research dossiers, project briefs, and executive syntheses.") {
                Button(viewModel.actionItemID == "briefings" ? "Loading…" : "View all →") {
                    showAllBriefings = true
                    viewModel.loadAllBriefings()
                }
                .font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                .fixedSize()
                .accessibilityIdentifier("today-briefings-view-all")
            }
            if shown.isEmpty {
                emptyColumn("No special reports published today. Press room is clear.")
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(alignment: .top, spacing: 12) {
                        ForEach(shown) { briefing in briefingCard(briefing) }
                    }
                }
            }
            if let error = viewModel.sectionErrors["briefings"] { sectionError(error) { viewModel.loadAllBriefings() } }
        }
    }

    private func briefingCard(_ briefing: TodayBriefing) -> some View {
        Button { selectedBriefing = briefing } label: {
            VStack(alignment: .leading, spacing: 8) {
                TodayKicker(text: "Special Edition", color: theme.accentColor, size: 9)
                let meta = briefingMeta(briefing)
                if !meta.isEmpty {
                    Text(meta).font(.themed(10)).foregroundColor(theme.secondaryTextColor).lineLimit(1)
                }
                Text(briefing.surface.title)
                    .font(TodayNewsprint.serif(18, weight: .semibold)).foregroundColor(theme.textColor)
                    .lineLimit(3)
                    .multilineTextAlignment(.leading)
                if let summary = TodayMorningEdition.firstNonEmpty(briefing.surface.summary, briefing.sourceOutputSummary, briefing.taskTitle) {
                    Text(summary).font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                        .lineLimit(3).multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
                Text("Read report →").font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
            }
            .padding(14)
            .frame(width: 250, height: 190, alignment: .topLeading)
            .background(theme.cardColor)
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.cardBorderColor))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("today-briefing-\(briefing.id)")
    }

    /// `By {agent} · Task {id} · {2h ago}` — only the parts present.
    private func briefingMeta(_ briefing: TodayBriefing) -> String {
        var parts: [String] = []
        if let agent = TodayMorningEdition.firstNonEmpty(briefing.sourceAgentID) { parts.append("By \(agent)") }
        if let task = TodayMorningEdition.firstNonEmpty(briefing.surface.taskID) { parts.append("Task \(task)") }
        if let published = TodayMorningEdition.epochMilliseconds(fromISO: briefing.surface.publishedAt) {
            parts.append(TodayViewModel.relativeTime(published))
        }
        return parts.joined(separator: " · ")
    }

    // MARK: § 4 Completed Deliverables

    @ViewBuilder private var deliveredSection: some View {
        let items = viewModel.items(for: .delivered)
        let total = max(items.count, viewModel.coreCount(for: .delivered))
        if !items.isEmpty {
            VStack(alignment: .leading, spacing: 12) {
                TodaySectionBanner(marker: "§ 4", title: "Completed Deliverables",
                                   subtitle: "Official records and signed-off deliverables ready for review.") {
                    // Bare count at the trailing edge; the spoken label keeps the noun.
                    TodayKicker(text: "\(total)", size: 11).fixedSize()
                        .accessibilityLabel("\(total) deliverable\(total == 1 ? "" : "s")")
                }
                ForEach(items.prefix(deliveredShown)) { item in deliveredCard(item) }
                if items.count > deliveredShown || total > items.count {
                    loadMoreButton("Show more · \(min(deliveredShown, items.count)) of \(total)",
                                   loadingKey: "lane:\(TodaySection.delivered.rawValue)") {
                        if items.count <= deliveredShown { viewModel.loadRemaining(.delivered) }
                        deliveredShown += Self.deliveredPageSize
                    }
                }
                if let error = viewModel.sectionErrors[TodaySection.delivered.rawValue] {
                    sectionError(error) { viewModel.loadRemaining(.delivered) }
                }
            }
        }
    }

    private func deliveredCard(_ item: TodayItem) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack {
                TodayKicker(text: "Filed · \(item.sourceKind.replacingOccurrences(of: "_", with: " "))", size: 9)
                Spacer(minLength: 6)
                Text(TodayViewModel.relativeTime(item.updatedAt > 0 ? item.updatedAt : item.createdAt))
                    .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
            }
            Text(item.title)
                .font(TodayNewsprint.serif(19, weight: .semibold)).foregroundColor(theme.textColor)
                .fixedSize(horizontal: false, vertical: true)
            if let prose = TodayMorningEdition.firstNonEmpty(item.summary, item.reason) {
                Text(prose).font(.themed(13)).foregroundColor(theme.secondaryTextColor).lineLimit(5)
            }
            TodayHairline()
            HStack(spacing: 12) {
                Label("RESOLVED", systemImage: "checkmark")
                    .font(.themedMono(10, weight: .bold))
                    .foregroundColor(theme.successColor)
                    .padding(.horizontal, 7).padding(.vertical, 3)
                    .overlay(RoundedRectangle(cornerRadius: 4).stroke(theme.successColor, lineWidth: 1.5))
                Spacer()
                Button { open(item) } label: {
                    Text("Inspect →").font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("today-delivered-inspect-\(item.id)")
                Button { dismissTodayItem(item) } label: {
                    Text("Acknowledge").font(.themed(12, weight: .semibold)).foregroundColor(theme.secondaryTextColor)
                }
                .buttonStyle(.plain)
                .disabled(viewModel.pendingCardMutationKeys.contains("today:\(item.id)"))
                .accessibilityIdentifier("today-delivered-acknowledge-\(item.id)")
            }
        }
        .padding(14)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.cardBorderColor))
    }

    // MARK: § 5 The Chronicle & Digest

    @ViewBuilder private var chronicleSection: some View {
        let digest = viewModel.digest
        if !digest.bullets.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                TodaySectionBanner(marker: "§ 5", title: "The Chronicle & Digest",
                                   subtitle: "Automated ledger of state changes, memory saves, and task updates across your spaces.") {
                    Button("↻ Refresh digest") { viewModel.loadDigest(offset: 0) }
                        .font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                        .fixedSize()
                        .disabled(viewModel.isDigestLoading)
                        .accessibilityIdentifier("today-digest-refresh")
                }
                ForEach(digest.bullets) { bullet in
                    Button { openDigest(bullet) } label: {
                        HStack(alignment: .firstTextBaseline, spacing: 9) {
                            Text("▪").font(.themed(13)).foregroundColor(theme.accentColor)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(bullet.text).font(.themed(14)).foregroundColor(theme.textColor)
                                    .multilineTextAlignment(.leading)
                                    .fixedSize(horizontal: false, vertical: true)
                                Text(bullet.sourceKind.replacingOccurrences(of: "_", with: " "))
                                    .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                            }
                            Spacer(minLength: 4)
                            Text("→").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                        }
                        .padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("today-digest-\(bullet.id)")
                }
                let pageSize = TodayViewModel.digestPageSize
                if digest.total > digest.bullets.count || viewModel.digestOffset > 0 {
                    HStack {
                        Button("← Newer") { viewModel.loadDigest(offset: max(0, viewModel.digestOffset - pageSize)) }
                            .disabled(viewModel.digestOffset == 0 || viewModel.isDigestLoading)
                        Spacer()
                        Text("\(viewModel.digestOffset + 1)–\(min(viewModel.digestOffset + digest.bullets.count, digest.total)) of \(digest.total)")
                            .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                        Spacer()
                        Button("Older →") { viewModel.loadDigest(offset: viewModel.digestOffset + pageSize) }
                            .disabled(viewModel.digestOffset + digest.bullets.count >= digest.total || viewModel.isDigestLoading)
                    }
                    .font(.themed(11, weight: .semibold)).foregroundColor(theme.accentColor)
                }
                if let error = viewModel.sectionErrors["digest"] {
                    sectionError(error) { viewModel.loadDigest(offset: viewModel.digestOffset) }
                }
            }
        }
    }

    private var footer: some View {
        VStack(spacing: 10) {
            TodayHairline()
            Text("TODAY'S · MORNING EDITION")
                .font(TodayNewsprint.serif(11, weight: .medium))
                .tracking(11 * 0.18)
                .foregroundColor(theme.secondaryTextColor)
                .frame(maxWidth: .infinity)
        }
        .padding(.top, 4)
        .accessibilityIdentifier("today-footer")
    }

    private var loadingState: some View {
        VStack(spacing: 12) {
            ProgressView().tint(theme.accentColor)
            Text("Printing the morning edition…").font(TodayNewsprint.serif(15, italic: true)).foregroundColor(theme.secondaryTextColor)
        }
        .frame(maxWidth: .infinity).padding(.vertical, 70)
    }

    @ViewBuilder private var undoToast: some View {
        if showUndo, viewModel.lastHiddenItem != nil {
            HStack(spacing: 12) {
                Text(viewModel.lastHiddenItem?.hiddenKind == "snoozed" ? "Snoozed" : "Hidden")
                    .font(.themed(13, weight: .semibold)).foregroundColor(theme.backgroundColor)
                Spacer()
                Button("Undo") { viewModel.undoLastHidden(); showUndo = false }
                    .font(.themed(13, weight: .bold)).foregroundColor(theme.backgroundColor)
                Button { showUndo = false } label: { Image(systemName: "xmark").foregroundColor(theme.backgroundColor.opacity(0.8)) }
            }
            .padding(.horizontal, 14).frame(height: 48)
            .background(theme.textColor.opacity(0.94)).cornerRadius(13)
            .padding(.horizontal, 16).padding(.bottom, 8)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }

    // MARK: Routing

    private func openWire(_ item: TodayWireItem) {
        if let taskID = item.taskID {
            AppActions.shared.requestTask(taskID)
        } else if let threadID = item.threadID {
            AppActions.shared.requestThread(threadID)
        }
    }

    private func open(_ item: TodayItem) {
        viewModel.markSeen(item)
        if item.section == TodaySection.needsYou.rawValue || (item.sourceURL ?? "").hasPrefix("/attention") {
            AppActions.shared.requestAttention(itemID: item.attentionItemID)
            return
        }
        // Monitor updates open the monitor detail at the EXACT update record
        // (metadata.monitor_task_id/update_id) — before the generic task
        // routing below, which would land on the plain task view.
        if let monitor = item.monitorDeepLinkTarget {
            AppActions.shared.requestMonitor(taskID: monitor.taskID,
                                             updateID: monitor.updateID)
            return
        }
        switch item.todayCardRoutingPreference {
        case .task(let taskID):
            AppActions.shared.requestTask(taskID)
            return
        case .nativeDetail:
            // Meeting action items are durable Today records, not meeting
            // links. A stale/missing internal meeting thread must not make a
            // card tap fall through to an external Google Meet URL.
            selectedItem = item
            return
        case .standard:
            break
        }
        if let route = item.sourceURL, route.hasPrefix("/"), routeInternal(route, item: item) {
            return
        } else if item.section == TodaySection.delivered.rawValue,
                  let briefing = viewModel.briefings.first(where: { $0.surface.taskID == item.taskID || $0.surface.surfaceID == item.sourceID }) {
            selectedBriefing = briefing
        } else if let raw = item.sourceURL, raw.hasPrefix("http"), let url = URL(string: raw) {
            UIApplication.shared.open(url)
        } else {
            selectedItem = item
        }
    }

    private func perform(_ action: TodayAction, for item: TodayItem) {
        viewModel.performTodayAction(action, for: item) { taskID in
            AppActions.shared.requestTask(taskID)
        }
    }

    private func routeInternal(_ route: String, item: TodayItem) -> Bool {
        // The canonical monitors route (`/tasks?type=monitors&selected=…
        // [&update=…]`) opens monitor mode at the exact update, never the
        // generic task view.
        if let monitor = Monitors.parseTasksRoute(route) {
            AppActions.shared.requestMonitor(taskID: monitor.taskID,
                                             updateID: monitor.updateID)
            return true
        }
        if route.hasPrefix("/tasks") { AppActions.shared.requestTask(item.taskID ?? item.sourceID); return true }
        if route.hasPrefix("/t/") {
            let id = item.threadID ?? String(route.dropFirst(3)).removingPercentEncoding
            AppActions.shared.requestThread(id); return true
        }
        if route.hasPrefix("/briefing") {
            if let briefing = viewModel.briefings.first(where: { $0.surface.surfaceID == item.sourceID || $0.surface.taskID == item.taskID }) { selectedBriefing = briefing }
            else { selectedItem = item }
            return true
        }
        if route.hasPrefix("/feed") { activitySearch = item.sourceID; showingActivity = true; return true }
        return false
    }

    private func openActivity(_ item: TodayActivityItem) {
        if ["data_delivery", "routine_result"].contains(item.itemType),
           let briefing = viewModel.briefings.first(where: { $0.id == item.briefingSurfaceID || $0.surface.taskID == item.taskID }) {
            selectedBriefing = briefing
        } else if let taskID = item.taskID {
            AppActions.shared.requestTask(taskID)
        } else if item.itemType == "agent_message", let threadID = item.threadID {
            AppActions.shared.requestThread(threadID)
        } else {
            selectedItem = TodayItem(id: item.id, section: TodaySection.changed.rawValue, priority: 0,
                                     title: item.displayTitle, summary: item.summary, reason: "Activity",
                                     sourceKind: item.itemType, sourceID: item.id, sourceURL: nil,
                                     spaceIDs: [], threadID: item.threadID, taskID: item.taskID,
                                     agentID: item.agentID, status: item.status,
                                     createdAt: item.createdAt, updatedAt: item.updatedAt)
        }
    }

    private func snooze(_ item: TodayItem, _ option: TodayViewModel.SnoozeOption) {
        withAnimation(.snappy(duration: 0.22)) {
            viewModel.hide(item, action: "snooze", snoozeMinutes: TodayViewModel.snoozeMinutes(for: option))
        }
    }

    private func dismissTodayItem(_ item: TodayItem) {
        withAnimation(.snappy(duration: 0.22)) { viewModel.hide(item, action: "dismiss") }
    }

    /// Resolve a message follow-up through the shared channel-assist annotation
    /// endpoints (useful / acknowledge / dismiss[+reason] / snooze) — web parity
    /// with ChannelFollowUpActions. Removal is optimistic inside the view model.
    private func resolveFollowUp(_ item: ChannelFollowUp, action: String, reason: String? = nil) {
        withAnimation(.snappy(duration: 0.22)) {
            viewModel.resolveFollowUp(item, action: action, reason: reason)
        }
    }

    private func openDigest(_ bullet: TodayDigestBullet) {
        let source = bullet.sourceURL?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if source.hasPrefix("/attention") {
            AppActions.shared.requestAttention()
        } else if source.hasPrefix("http"), let url = URL(string: source) {
            UIApplication.shared.open(url)
        } else if source.hasPrefix("/tasks") {
            AppActions.shared.requestTask(URLComponents(string: source)?.queryItems?.first(where: { $0.name == "selected" })?.value)
        } else if source.hasPrefix("/t/") {
            AppActions.shared.requestThread(String(source.dropFirst(3)).removingPercentEncoding)
        } else {
            activitySearch = bullet.sourceID.isEmpty ? bullet.text : bullet.sourceID
            showingActivity = true
        }
    }

    // MARK: Chrome

    private func loadMoreButton(_ title: String, loadingKey: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 7) {
                if viewModel.actionItemID == loadingKey { ProgressView().controlSize(.small) }
                Text(viewModel.actionItemID == loadingKey ? "Loading…" : title)
                    .font(.themed(12, weight: .semibold))
            }
            .foregroundColor(theme.accentColor).frame(maxWidth: .infinity).padding(.vertical, 8)
        }
        .buttonStyle(.plain).disabled(viewModel.actionItemID != nil)
    }

    private func errorBanner(_ message: String) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundColor(theme.warningColor)
            Text(message).font(.themed(12)).foregroundColor(theme.textColor).lineLimit(4)
            Spacer()
        }
        .padding(11).background(theme.warningColor.opacity(0.12)).cornerRadius(11)
        .accessibilityIdentifier("today-error-banner")
    }

    private func sectionError(_ message: String, retry: @escaping () -> Void) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundColor(theme.warningColor)
            Text(message).font(.themed(11)).foregroundColor(theme.secondaryTextColor).lineLimit(3)
            Spacer()
            Button("Retry", action: retry).font(.themed(11, weight: .semibold)).foregroundColor(theme.accentColor)
        }
        .padding(9).background(theme.warningColor.opacity(0.10)).cornerRadius(9)
    }

    private func chip(_ text: String, tint: Color? = nil) -> some View {
        Text(text).font(.themed(9, weight: .semibold)).lineLimit(1)
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background((tint ?? theme.secondaryTextColor).opacity(0.1))
            .foregroundColor(tint ?? theme.secondaryTextColor).clipShape(Capsule())
    }

    private func sourceLabel(_ source: String) -> String {
        switch source { case "published_surface": return "Briefing"; case "routine_result": return "Routine"; case "memory_learning_digest", "memory_learning": return "Memory"; case "agent_message": return "Thread"; default: return Self.titleCase(source) }
    }

    fileprivate static func titleCase(_ value: String) -> String {
        value.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ").capitalized
    }

    fileprivate static func shortDate(_ value: String) -> String {
        let input = ISO8601DateFormatter()
        guard let date = input.date(from: value) else { return value }
        let output = DateFormatter(); output.dateStyle = .medium; output.timeStyle = .none
        return output.string(from: date)
    }
}

/// The durable Activity history (search, filters, remove, clear all) — the
/// former Today "Activity" section, now opened from the Realtime Wire drawer.
private struct TodayActivitySheet: View {
    @ObservedObject var viewModel: TodayViewModel
    @Binding var search: String
    let onOpen: (TodayActivityItem) -> Void

    @State private var filter = TodayActivityFilter.all
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Durable learnings, outcomes, failures, and deliveries.")
                        .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                    HStack {
                        TextField("Search title, summary, IDs, metadata, or actions", text: $search)
                            .font(.themed(14)).padding(10).background(theme.surfaceColor).cornerRadius(10)
                            .accessibilityIdentifier("today-activity-search")
                        if !viewModel.activityItems.isEmpty {
                            Menu {
                                Button("Clear all activity", role: .destructive) { viewModel.clearActivity() }
                            } label: { Image(systemName: "ellipsis.circle").font(.title3).foregroundColor(theme.accentColor) }
                        }
                    }
                    Picker("Activity filter", selection: $filter) {
                        ForEach(TodayActivityFilter.allCases) { Text("\($0.title) \(count($0))").tag($0) }
                    }
                    .pickerStyle(.segmented)

                    if filtered.isEmpty {
                        Text("No durable activity items match the current filter.")
                            .font(.themed(13)).foregroundColor(theme.secondaryTextColor).padding(.vertical, 8)
                    } else {
                        ForEach(filtered) { item in row(item) }
                    }
                    if let error = viewModel.sectionErrors["activity"] {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(11)).foregroundColor(theme.warningColor)
                    }
                }
                .padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Activity")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
        }
    }

    private func row(_ item: TodayActivityItem) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: icon(item))
                .foregroundColor(statusColor(item.status)).frame(width: 24)
            VStack(alignment: .leading, spacing: 4) {
                Text(item.displayTitle).font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor).lineLimit(3)
                if let summary = item.summary {
                    Text(summary).font(.themed(12)).foregroundColor(theme.secondaryTextColor).lineLimit(3)
                }
                HStack {
                    chip(TodayView.titleCase(item.itemType), tint: nil)
                    chip(TodayView.titleCase(item.status), tint: statusColor(item.status))
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 8) {
                Text(TodayViewModel.relativeTime(item.updatedAt)).font(.themed(9)).foregroundColor(theme.secondaryTextColor)
                Menu {
                    Button("View") { onOpen(item) }
                    Button("Remove", role: .destructive) { viewModel.removeActivity(item) }
                } label: { Image(systemName: "ellipsis").foregroundColor(theme.secondaryTextColor).frame(width: 28, height: 24) }
            }
        }
        .padding(.vertical, 5)
        .contentShape(Rectangle())
        .onTapGesture { onOpen(item) }
        .contextMenu {
            Button(role: .destructive) { viewModel.removeActivity(item) } label: { Label("Remove", systemImage: "trash") }
        }
    }

    private var filtered: [TodayActivityItem] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return viewModel.activityItems.filter { item in
            guard matches(item, filter) else { return false }
            let metadata = String(describing: item.metadata)
            let actions = item.actions.map { "\($0.id) \($0.label) \($0.actionType ?? "") \($0.payload)" }.joined(separator: " ")
            return query.isEmpty || "\(item.id) \(item.taskID ?? "") \(item.threadID ?? "") \(item.agentID ?? "") \(item.title) \(item.summary ?? "") \(item.itemType) \(item.status) \(metadata) \(actions)".lowercased().contains(query)
        }
    }

    private func matches(_ item: TodayActivityItem, _ filter: TodayActivityFilter) -> Bool {
        switch filter {
        case .all: return true
        case .learnings: return item.itemType == "agent_learning"
        case .outcomes: return item.itemType == "task"
        case .failed: return item.status == "failed"
        case .deliveries: return ["data_delivery", "routine_result"].contains(item.itemType)
        }
    }

    private func count(_ filter: TodayActivityFilter) -> Int {
        viewModel.activityItems.filter { matches($0, filter) }.count
    }

    private func chip(_ text: String, tint: Color?) -> some View {
        Text(text).font(.themed(9, weight: .semibold)).lineLimit(1)
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background((tint ?? theme.secondaryTextColor).opacity(0.1))
            .foregroundColor(tint ?? theme.secondaryTextColor).clipShape(Capsule())
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "done": return theme.successColor
        case "failed": return theme.dangerColor
        case "running": return theme.infoColor
        case "needs_action": return theme.warningColor
        default: return theme.secondaryTextColor
        }
    }

    private func icon(_ item: TodayActivityItem) -> String {
        if item.status == "failed" { return "xmark.circle.fill" }
        switch item.itemType { case "agent_learning": return "sparkles"; case "data_delivery", "routine_result": return "tray.full.fill"; default: return "checkmark.circle.fill" }
    }
}

private struct TodaySwipeCardAction: Identifiable {
    let id: String
    let title: String
    let systemImage: String
    let color: Color
    let run: () -> Void
}

private struct TodaySwipeCardWidthKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

private struct TodaySwipeActionCard<Content: View>: View {
    private static var actionWidth: CGFloat { 72 }

    let itemID: String
    let leadingActions: [TodaySwipeCardAction]
    let trailingActions: [TodaySwipeCardAction]
    @ViewBuilder let content: Content

    @State private var restingOffset: CGFloat = 0
    @State private var dragOffset: CGFloat = 0
    @State private var horizontalDragActive = false
    @State private var suppressContentTap = false
    @State private var tapSuppressionGeneration = 0
    @State private var cardWidth: CGFloat = 0
    @ObservedObject private var theme = ThemeManager.shared

    private var leadingRevealWidth: CGFloat {
        Self.actionWidth * CGFloat(leadingActions.count)
    }

    private var trailingRevealWidth: CGFloat {
        Self.actionWidth * CGFloat(trailingActions.count)
    }

    private var rawOffset: CGFloat { restingOffset + dragOffset }

    /// Full-swipe commit threshold: swipe past ~half the card (or well past the rail).
    private var trailingCommitThreshold: CGFloat { max(trailingRevealWidth + 96, cardWidth * 0.5) }
    private var leadingCommitThreshold: CGFloat { max(leadingRevealWidth + 96, cardWidth * 0.5) }

    /// The card follows the finger, but past the revealed rail it rubber-bands (55 %
    /// resistance, capped near the card edge) so a long swipe *visibly travels* — the
    /// primary action's colour then fills the widening gap (see `fullSwipePanel`).
    private var visibleOffset: CGFloat {
        let raw = rawOffset
        let cap = cardWidth > 0 ? cardWidth * 0.92 : max(trailingRevealWidth, leadingRevealWidth) + 160
        if raw < -trailingRevealWidth {
            return max(-cap, -trailingRevealWidth - ((-trailingRevealWidth) - raw) * 0.55)
        }
        if raw > leadingRevealWidth {
            return min(cap, leadingRevealWidth + (raw - leadingRevealWidth) * 0.55)
        }
        return raw
    }

    /// Armed = a release will commit the default action. Keyed off the real finger
    /// travel (not the rubber-banded card), so the brightening cue matches the finger.
    private var trailingArmed: Bool { rawOffset <= -trailingCommitThreshold }
    private var leadingArmed: Bool { rawOffset >= leadingCommitThreshold }

    private var actionSignature: String {
        (leadingActions.map(\.id) + ["|"] + trailingActions.map(\.id)).joined(separator: ",")
    }

    var body: some View {
        ZStack(alignment: .trailing) {
            HStack(spacing: 0) {
                ForEach(leadingActions) { action in
                    actionButton(
                        action.title,
                        systemImage: action.systemImage,
                        color: action.color,
                        identifier: "today-swipe-action-\(action.id)-\(itemID)"
                    ) {
                        close()
                        action.run()
                    }
                    .accessibilityHidden(restingOffset <= 0)
                    .allowsHitTesting(restingOffset > 0)
                }
                Spacer(minLength: 0)
                ForEach(trailingActions) { action in
                    actionButton(
                        action.title,
                        systemImage: action.systemImage,
                        color: action.color,
                        identifier: "today-swipe-\(action.id)-\(itemID)"
                    ) {
                        close()
                        action.run()
                    }
                    .accessibilityHidden(restingOffset >= 0)
                    .allowsHitTesting(restingOffset < 0)
                }
            }

            // Full-swipe panels: past the rail, the default (first) action's colour
            // fills the widening gap and brightens once armed — the Mail-style cue that
            // releasing commits. Shown only during a full swipe (past the rail).
            if let primary = trailingActions.first, visibleOffset < -trailingRevealWidth {
                fullSwipePanel(primary, armed: trailingArmed, width: -visibleOffset, trailing: true)
            }
            if let primary = leadingActions.first, visibleOffset > leadingRevealWidth {
                fullSwipePanel(primary, armed: leadingArmed, width: visibleOffset, trailing: false)
            }

            content
                // The close-overlay must attach BEFORE `offset` so it travels
                // with the visible card: applied after, its hit frame stays over
                // the revealed rail and swallows the action buttons' taps.
                .overlay {
                    if restingOffset != 0 {
                        Color.clear
                            .contentShape(Rectangle())
                            .onTapGesture { close() }
                    }
                }
                .offset(x: visibleOffset)
                // Once a horizontal drag wins, the nested card Button must
                // not receive the same touch-up and open the detail sheet.
                .allowsHitTesting(!suppressContentTap)
        }
        .clipShape(RoundedRectangle(cornerRadius: 13))
        .background(
            GeometryReader { geo in
                Color.clear.preference(key: TodaySwipeCardWidthKey.self, value: geo.size.width)
            }
        )
        .onPreferenceChange(TodaySwipeCardWidthKey.self) { cardWidth = $0 }
        .contentShape(Rectangle())
        // Horizontal-only so a vertical scroll that starts on a card still
        // scrolls the page (UIKit pan on iOS 18+, SwiftUI drag on iOS 17).
        .horizontalSwipe(
            fallback: swipeGesture,
            onChanged: { railDragChanged($0) },
            onEnded: { railDragEnded(translation: $0, predicted: $1) },
            onCancelled: { railDragCancelled() }
        )
        .onChange(of: actionSignature) { _, _ in close() }
    }

    private var swipeGesture: some Gesture {
        DragGesture(minimumDistance: 16)
            .onChanged { value in
                guard abs(value.translation.width) > abs(value.translation.height) else { return }
                railDragChanged(value.translation)
            }
            .onEnded { value in
                let handledHorizontalDrag = horizontalDragActive
                    || abs(value.translation.width) > abs(value.translation.height)
                guard handledHorizontalDrag else {
                    dragOffset = 0
                    horizontalDragActive = false
                    releaseContentTapSuppression()
                    return
                }
                railDragEnded(translation: value.translation, predicted: value.predictedEndTranslation)
            }
    }

    private func railDragChanged(_ translation: CGSize) {
        horizontalDragActive = true
        suppressContentTap = true
        dragOffset = translation.width
    }

    private func railDragEnded(translation: CGSize, predicted: CGSize) {
        dragOffset = 0
        horizontalDragActive = false
        let projected = restingOffset + predicted.width
        let raw = restingOffset + translation.width
        // Long/fast swipe past the commit threshold → fire the edge's default
        // (first) action in one gesture. Commits when the finger passed the
        // threshold (armed) OR a fast flick projects past it.
        if trailingRevealWidth > 0, let primary = trailingActions.first,
           raw <= -trailingCommitThreshold || projected < -trailingCommitThreshold {
            withAnimation(.snappy(duration: 0.22)) { restingOffset = 0 }
            primary.run()
        } else if leadingRevealWidth > 0, let primary = leadingActions.first,
                  raw >= leadingCommitThreshold || projected > leadingCommitThreshold {
            withAnimation(.snappy(duration: 0.22)) { restingOffset = 0 }
            primary.run()
        } else {
            withAnimation(.snappy(duration: 0.22)) {
                if trailingRevealWidth > 0, projected < -(trailingRevealWidth * 0.35) {
                    restingOffset = -trailingRevealWidth
                } else if leadingRevealWidth > 0, projected > leadingRevealWidth * 0.35 {
                    restingOffset = leadingRevealWidth
                } else {
                    restingOffset = 0
                }
            }
        }
        releaseContentTapSuppression()
    }

    private func railDragCancelled() {
        dragOffset = 0
        horizontalDragActive = false
        releaseContentTapSuppression()
    }

    private func actionButton(
        _ title: String,
        systemImage: String,
        color: Color,
        identifier: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 5) {
                Image(systemName: systemImage).font(.system(size: 15, weight: .semibold))
                Text(title)
                    .font(.themed(10, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
            }
            .foregroundColor(theme.contrastingTextColor(for: color))
            .frame(width: Self.actionWidth)
            .frame(maxHeight: .infinity)
            .background(color)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(identifier)
    }

    /// The expanding coloured panel shown during a full swipe: the primary action's
    /// colour fills the revealed gap (`width`), icon anchored near the card edge, and
    /// brightens + grows the icon once `armed`. Visual only (no hit-testing).
    private func fullSwipePanel(_ action: TodaySwipeCardAction, armed: Bool, width: CGFloat, trailing: Bool) -> some View {
        HStack(spacing: 0) {
            if trailing { Spacer(minLength: 0) }
            VStack(spacing: 5) {
                Image(systemName: action.systemImage).font(.system(size: armed ? 18 : 15, weight: .semibold))
                Text(action.title).font(.themed(10, weight: .semibold)).multilineTextAlignment(.center).lineLimit(2)
            }
            .foregroundColor(theme.contrastingTextColor(for: action.color))
            .frame(width: max(Self.actionWidth, width), alignment: trailing ? .leading : .trailing)
            .frame(maxHeight: .infinity)
            .background(action.color.opacity(armed ? 1.0 : 0.85))
            if !trailing { Spacer(minLength: 0) }
        }
        .allowsHitTesting(false)
        .animation(.snappy(duration: 0.14), value: armed)
    }

    private func close() {
        tapSuppressionGeneration += 1
        suppressContentTap = false
        dragOffset = 0
        horizontalDragActive = false
        withAnimation(.snappy(duration: 0.18)) { restingOffset = 0 }
    }

    private func releaseContentTapSuppression() {
        tapSuppressionGeneration += 1
        let generation = tapSuppressionGeneration
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            guard tapSuppressionGeneration == generation else { return }
            suppressContentTap = false
        }
    }
}

private enum TodayActivityFilter: String, CaseIterable, Identifiable {
    case all, learnings, outcomes, failed, deliveries
    var id: String { rawValue }
    var title: String { rawValue.capitalized }
}

private struct TodayPrimaryButtonStyle: ButtonStyle {
    @ObservedObject var theme: ThemeManager
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.themed(12, weight: .semibold)).foregroundColor(theme.onAccentColor)
            .padding(.horizontal, 12).frame(height: 32).background(theme.accentColor.opacity(configuration.isPressed ? 0.7 : 1)).cornerRadius(9)
    }
}

private struct TodayCompactActionButtonStyle: ButtonStyle {
    let tint: Color
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.themed(11, weight: .semibold))
            .padding(.horizontal, 8)
            .frame(height: 27)
            .foregroundColor(tint)
            .background(tint.opacity(configuration.isPressed ? 0.16 : 0.08))
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .stroke(tint.opacity(0.24), lineWidth: 0.75)
            }
            .opacity(isEnabled ? 1 : 0.45)
    }
}

private extension View {
    func todaySurface(_ theme: ThemeManager) -> some View {
        self.padding(14).background(theme.surfaceColor.opacity(0.82)).cornerRadius(17)
            .overlay(RoundedRectangle(cornerRadius: 17).stroke(theme.cardBorderColor))
    }
    func todayRowSurface(_ theme: ThemeManager) -> some View {
        self.padding(12).background(theme.cardColor).cornerRadius(13)
            .overlay(RoundedRectangle(cornerRadius: 13).stroke(theme.cardBorderColor))
    }
}

private extension JSONValue {
    var displayText: String {
        guard let data = try? JSONEncoder().encode(self),
              let object = try? JSONSerialization.jsonObject(with: data),
              let pretty = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]),
              let text = String(data: pretty, encoding: .utf8) else { return String(describing: self) }
        return text
    }
}

enum TodayItemCardRoutingPreference: Equatable {
    case task(String)
    case nativeDetail
    case standard
}

extension TodayItem {
    var executableTodayActions: [TodayAction] {
        actions.filter {
            $0.actionType == "today_source_action" && $0.executionEndpoint != nil
        }
    }

    var isMeetingAction: Bool {
        sourceKind == "meeting_action"
            || metadata.objectValue?["followup_kind"]?.stringValue == "meeting_action_item"
    }

    var linkedTodayTaskID: String? {
        if let taskID, !taskID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return taskID
        }
        return nil
    }

    var todayCardRoutingPreference: TodayItemCardRoutingPreference {
        // A visible meeting action has no current linked task: the server
        // suppresses it when that task exists. Its metadata may retain a stale
        // linked_task_id after task deletion, so it must still open natively.
        if isMeetingAction { return .nativeDetail }
        if let linkedTodayTaskID { return .task(linkedTodayTaskID) }
        return .standard
    }

    var externalSourceURL: URL? {
        guard !isMeetingAction,
              let sourceURL,
              sourceURL.hasPrefix("http") else { return nil }
        return URL(string: sourceURL)
    }

    var todayDetailMarkdownSource: String? {
        let metadataRecord = metadata.objectValue
        if let complete = metadataRecord?["detail_markdown"]?.stringValue?
            .trimmingCharacters(in: .whitespacesAndNewlines),
           !complete.isEmpty {
            return complete
        }
        let candidates = [
            metadataRecord?["meeting_summary"]?.stringValue,
            metadataRecord?["description"]?.stringValue,
            metadataRecord?["action_item"]?.stringValue,
            summary
        ]
        var seen = Set<String>()
        let sections = candidates.compactMap { candidate -> String? in
            guard let value = candidate?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !value.isEmpty,
                  seen.insert(value).inserted else { return nil }
            return value
        }
        return sections.isEmpty ? nil : sections.joined(separator: "\n\n")
    }
}

enum TodayDescriptionPresentation {
    static func isExpandable(_ content: String) -> Bool {
        content.count > 240 || content.filter(\.isNewline).count > 5
    }
}

private struct TodayExpandableMarkdown: View {
    let content: String
    @StateObject private var theme = ThemeManager.shared
    @State private var expanded = false

    private var canCollapse: Bool {
        TodayDescriptionPresentation.isExpandable(content)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Markdown(content)
                .font(.themed(15))
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(canCollapse && !expanded ? 8 : nil)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .accessibilityIdentifier("today-detail-description")

            if canCollapse {
                Button {
                    withAnimation(.easeInOut(duration: 0.18)) { expanded.toggle() }
                } label: {
                    Label(expanded ? "Show less" : "Show full description",
                          systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.themed(12, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundColor(theme.accentColor)
                .accessibilityIdentifier("today-detail-description-toggle")
            }
        }
    }
}

private struct TodayItemDetailView: View {
    let item: TodayItem
    @ObservedObject var viewModel: TodayViewModel
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Label(item.title, systemImage: "sparkles")
                        .font(.themed(22, weight: .bold)).foregroundColor(theme.textColor)
                    if !item.reason.isEmpty { Text(item.reason).font(.themed(15, weight: .semibold)).foregroundColor(theme.accentColor) }
                    if let description = item.todayDetailMarkdownSource {
                        VStack(alignment: .leading, spacing: 7) {
                            Text("Description")
                                .font(.themed(12, weight: .bold))
                                .foregroundColor(theme.secondaryTextColor)
                            TodayExpandableMarkdown(content: description)
                        }
                    }
                    Divider()
                    LabeledContent("Source", value: item.sourceKind.replacingOccurrences(of: "_", with: " ").capitalized)
                    LabeledContent("Status", value: item.status.replacingOccurrences(of: "_", with: " ").capitalized)
                    LabeledContent("Updated", value: TodayViewModel.localDateTime(item.updatedAt))
                    if let agent = item.agentID { LabeledContent("Agent", value: agent) }
                    if !item.spaceIDs.isEmpty { LabeledContent("Spaces", value: item.spaceIDs.joined(separator: ", ")) }
                    if !item.learnedItems.isEmpty {
                        Divider()
                        Text("Learned").font(.themed(16, weight: .bold)).foregroundColor(theme.textColor)
                        ForEach(item.learnedItems) { learned in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(learned.title).font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
                                if let summary = learned.summary { Text(summary).font(.themed(12)).foregroundColor(theme.secondaryTextColor) }
                                if let updated = learned.updatedAt { Text(TodayViewModel.localDateTime(updated)).font(.themed(10)).foregroundColor(theme.secondaryTextColor) }
                            }.padding(10).background(theme.elevatedColor).cornerRadius(10)
                        }
                    }
                    if !item.executableTodayActions.isEmpty {
                        Divider()
                        Text("Available actions").font(.themed(16, weight: .bold)).foregroundColor(theme.textColor)
                        ForEach(item.executableTodayActions) { action in
                            Button {
                                viewModel.performTodayAction(action, for: item) { taskID in
                                    dismiss()
                                    AppActions.shared.requestTask(taskID)
                                }
                            } label: {
                                HStack {
                                    Label(action.label, systemImage: action.systemImage)
                                    Spacer()
                                    if viewModel.actionItemID == item.id { ProgressView() }
                                    else { Image(systemName: "chevron.right") }
                                }
                                .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .tint(theme.accentColor)
                            .foregroundColor(theme.onAccentColor)
                            .disabled(viewModel.actionItemID != nil)
                        }
                    }
                    if item.section == TodaySection.followups.rawValue {
                        Divider()
                        Text("Manage follow-up")
                            .font(.themed(16, weight: .bold))
                            .foregroundColor(theme.textColor)
                        Menu {
                            Button("Until tonight") { snooze(.tonight) }
                            Button("Tomorrow morning") { snooze(.tomorrowMorning) }
                            Button("Next week") { snooze(.nextWeek) }
                        } label: {
                            Label("Snooze follow-up", systemImage: "clock")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                        .tint(theme.discoveryColor)
                        .disabled(viewModel.actionItemID != nil)
                        .accessibilityIdentifier("today-detail-snooze")
                    }
                    if item.section == TodaySection.needsYou.rawValue {
                        Button { AppActions.shared.requestAttention(itemID: item.attentionItemID); dismiss() } label: {
                            Label("Review in Attention", systemImage: "bell.badge.fill").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent).tint(theme.accentColor)
                        .foregroundColor(theme.onAccentColor)
                    }
                    if let url = item.externalSourceURL {
                        Button { UIApplication.shared.open(url) } label: {
                            Label("Open source", systemImage: "arrow.up.right.square").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered).tint(theme.accentColor)
                    }
                }
                .padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Today")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
        }
    }

    private func snooze(_ option: TodayViewModel.SnoozeOption) {
        withAnimation(.snappy(duration: 0.22)) {
            viewModel.hide(item, action: "snooze", snoozeMinutes: TodayViewModel.snoozeMinutes(for: option))
        }
        dismiss()
    }
}

private func resolvedResurfacingCapabilities(
    card: ResurfacingCard,
    detail: ResurfacingDetail?
) -> [ResurfacingActionCapability] {
    let source = (detail?.actions.isEmpty == false ? detail?.actions : card.actions) ?? card.actions
    if source.isEmpty {
        return [
            ResurfacingActionCapability(
                kind: .viewDetails,
                label: "Refresh details",
                requiresInput: false,
                sideEffect: "none"
            ),
            ResurfacingActionCapability(
                kind: .showOriginal,
                label: "Show original",
                requiresInput: false,
                sideEffect: "none"
            )
        ]
    }
    return source
}

private struct ResurfacingDetailView: View {
    let card: ResurfacingCard
    @ObservedObject var viewModel: TodayViewModel
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var inputAction: ResurfacingActionCapability?

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 15) {
                    Label("Worth a look", systemImage: "sparkles")
                        .font(.themed(12, weight: .semibold)).foregroundColor(theme.discoveryColor)
                    Text(card.line.isEmpty ? card.sourceTitle : card.line)
                        .font(.themed(23, weight: .bold)).foregroundColor(theme.textColor)
                    if !card.whyNow.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Why now").font(.themed(11, weight: .bold)).foregroundColor(theme.secondaryTextColor)
                            Text(card.whyNow).font(.themed(15, weight: .semibold)).foregroundColor(theme.discoveryColor)
                        }
                    }
                    if !card.summary.isEmpty { Text(card.summary).font(.themed(15)).foregroundColor(theme.secondaryTextColor) }
                    if let detail, detail.hasNewer || detail.sourceUpdated || card.sourceUpdated {
                        Label("The source has newer content. Refresh details before acting.", systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(12, weight: .semibold)).foregroundColor(theme.warningColor)
                            .padding(10).background(theme.warningColor.opacity(0.12)).cornerRadius(10)
                    }
                    Divider()
                    LabeledContent("Source", value: card.sourceTitle)
                    LabeledContent("Type", value: card.sourceKind.replacingOccurrences(of: "_", with: " ").capitalized)
                    if let detail { LabeledContent("Availability", value: detail.status.replacingOccurrences(of: "_", with: " ").capitalized) }
                    if viewModel.actionItemID == "resurfacing:\(card.id)" { ProgressView("Loading current details…") }
                    briefView
                    if let original = detail?.original {
                        Divider()
                        Text("Original").font(.themed(17, weight: .bold)).foregroundColor(theme.textColor)
                        Text(original.displayText).font(.themedMono(.caption)).foregroundColor(theme.secondaryTextColor)
                            .textSelection(.enabled).padding(10).background(theme.elevatedColor).cornerRadius(10)
                    }
                    if let result = viewModel.resurfacingActionResults[card.id]?.result {
                        Divider()
                        Text("Action result").font(.themed(17, weight: .bold)).foregroundColor(theme.textColor)
                        Text(result.displayText).font(.themed(12)).foregroundColor(theme.secondaryTextColor).textSelection(.enabled)
                        if let route = result.objectValue?["route"]?.stringValue {
                            Button { openRoute(route) } label: { Label("Open result", systemImage: "arrow.up.right.square") }
                                .buttonStyle(.bordered).tint(theme.accentColor)
                        }
                    }
                    Divider()
                    Text("Actions").font(.themed(17, weight: .bold)).foregroundColor(theme.textColor)
                    ForEach(capabilities) { capability in
                        Button { handle(capability) } label: {
                            HStack {
                                Image(systemName: capability.kind.systemImage)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(capability.label).font(.themed(14, weight: .semibold))
                                    if capability.kind == recommendation?.kind, let recommendation {
                                        Text("Recommended · \(recommendation.rationale)").font(.themed(10)).opacity(0.8).lineLimit(2)
                                    }
                                }
                                Spacer()
                                if viewModel.actionItemID == "resurfacing-action:\(card.id):\(capability.kind.rawValue)" { ProgressView().controlSize(.small) }
                                else { Image(systemName: "chevron.right").font(.caption2) }
                            }.frame(maxWidth: .infinity).padding(11)
                        }
                        .buttonStyle(.bordered).tint(capability.kind == recommendation?.kind ? theme.discoveryColor : theme.accentColor)
                        .disabled(viewModel.actionItemID != nil)
                    }
                    if let error = viewModel.sectionErrors["resurfacing:\(card.id)"] {
                        Text(error).font(.themed(12)).foregroundColor(theme.dangerColor)
                    }
                    if let error = viewModel.sectionErrors.first(where: { $0.key.hasPrefix("resurfacing-action:\(card.id):") })?.value {
                        Text(error).font(.themed(12)).foregroundColor(theme.dangerColor)
                        Button("Refresh source details") { viewModel.fetchResurfacingDetail(card) }.font(.themed(12, weight: .semibold))
                    }
                }
                .padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Worth a look")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .onAppear {
                let detailKey = "resurfacing:\(card.id)"
                if detail?.original == nil, viewModel.actionItemID != detailKey {
                    viewModel.fetchResurfacingDetail(card)
                }
            }
            .sheet(item: $inputAction) { ResurfacingActionInputView(card: card, capability: $0, viewModel: viewModel) }
        }
    }

    private var detail: ResurfacingDetail? { viewModel.resurfacingDetails[card.id] }
    private var brief: ResurfacingBrief? { detail?.brief ?? card.brief }
    private var recommendation: ResurfacingRecommendation? { detail?.recommendedAction ?? card.recommendedAction }
    private var capabilities: [ResurfacingActionCapability] {
        resolvedResurfacingCapabilities(card: card, detail: detail)
    }

    @ViewBuilder private var briefView: some View {
        if let brief {
            Divider()
            Text("Structured brief").font(.themed(17, weight: .bold)).foregroundColor(theme.textColor)
            ForEach(brief.keyFacts, id: \.self) { fact in Label(fact, systemImage: "checkmark.circle").font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
            ForEach(Array(brief.changes.enumerated()), id: \.offset) { _, change in
                VStack(alignment: .leading, spacing: 3) {
                    Text(change.aspect.isEmpty ? "Changed" : change.aspect).font(.themed(12, weight: .bold)).foregroundColor(theme.textColor)
                    if let before = change.before { Text("Before: \(before)").font(.themed(11)).foregroundColor(theme.secondaryTextColor) }
                    if let after = change.after { Text("After: \(after)").font(.themed(11, weight: .semibold)).foregroundColor(theme.discoveryColor) }
                    if let effective = change.effectiveText { Text(effective).font(.themed(11)).foregroundColor(theme.secondaryTextColor) }
                }.padding(9).background(theme.elevatedColor).cornerRadius(9)
            }
            ForEach(brief.temporalFacts) { fact in
                Label(fact.atMS.map(TodayViewModel.localDateTime) ?? fact.text, systemImage: "calendar")
                    .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            }
            if !brief.missingDetails.isEmpty {
                Text("Missing details: \(brief.missingDetails.joined(separator: ", "))")
                    .font(.themed(11)).foregroundColor(theme.warningColor)
            }
        }
    }

    private func handle(_ capability: ResurfacingActionCapability) {
        switch capability.kind {
        case .viewDetails: viewModel.fetchResurfacingDetail(card)
        case .showOriginal: viewModel.fetchResurfacingDetail(card, original: true)
        case .openSource:
            if let raw = detail?.openURL, let url = URL(string: raw) { UIApplication.shared.open(url) }
            else if let route = detail?.sourceRoute { openRoute(route) }
            else { viewModel.fetchResurfacingDetail(card) }
        case .createTask, .createReminder, .share, .saveToMemory: inputAction = capability
        default: viewModel.performResurfacingAction(card, kind: capability.kind)
        }
    }

    private func openRoute(_ route: String) {
        if route.hasPrefix("http"), let url = URL(string: route) { UIApplication.shared.open(url); return }
        let source = detail?.source?.objectValue
        if route.hasPrefix("/tasks") { AppActions.shared.requestTask(source?["task_id"]?.stringValue); dismiss(); return }
        if route.hasPrefix("/t/") { AppActions.shared.requestThread(source?["thread_id"]?.stringValue ?? String(route.dropFirst(3))); dismiss(); return }
        if route.hasPrefix("/briefing") { AppActions.shared.requestToday(section: .delivered); dismiss(); return }
        if route.hasPrefix("/feed") || route.hasPrefix("/memory") { AppActions.shared.requestToday(section: .changed, activity: true); dismiss() }
    }
}

private struct ResurfacingActionsSheet: View {
    let card: ResurfacingCard
    @ObservedObject var viewModel: TodayViewModel
    let onOpenDetail: () -> Void

    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var inputAction: ResurfacingActionCapability?

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 5) {
                        Label("Worth a look", systemImage: "sparkles")
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(theme.discoveryColor)
                        Text(card.line.isEmpty ? card.sourceTitle : card.line)
                            .font(.themed(20, weight: .bold))
                            .foregroundColor(theme.textColor)
                            .fixedSize(horizontal: false, vertical: true)
                        if !card.whyNow.isEmpty {
                            Text(card.whyNow)
                                .font(.themed(12, weight: .medium))
                                .foregroundColor(theme.discoveryColor)
                        }
                    }

                    actionSection
                    feedbackSection

                    if let error = viewModel.sectionErrors["resurfacing:\(card.id)"] {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(12))
                            .foregroundColor(theme.dangerColor)
                    }
                    if let error = viewModel.sectionErrors.first(where: {
                        $0.key.hasPrefix("resurfacing-action:\(card.id):")
                    })?.value {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(12))
                            .foregroundColor(theme.dangerColor)
                    }
                }
                .padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Worth a look actions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear {
                if detail == nil { viewModel.fetchResurfacingDetail(card) }
            }
            .sheet(item: $inputAction) {
                ResurfacingActionInputView(card: card, capability: $0, viewModel: viewModel)
            }
        }
    }

    private var detail: ResurfacingDetail? { viewModel.resurfacingDetails[card.id] }
    private var recommendation: ResurfacingRecommendation? {
        detail?.recommendedAction ?? card.recommendedAction
    }
    private var capabilities: [ResurfacingActionCapability] {
        resolvedResurfacingCapabilities(card: card, detail: detail)
            .filter { $0.kind != .viewDetails }
    }

    private var actionSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text("ACTIONS")
                .font(.themed(10, weight: .bold))
                .tracking(1.1)
                .foregroundColor(theme.secondaryTextColor)

            actionRow(
                label: "Open full details",
                systemImage: "doc.text.magnifyingglass",
                tint: theme.accentColor,
                identifier: "worth-actions-open-detail-\(card.id)",
                action: onOpenDetail
            )

            if viewModel.actionItemID == "resurfacing:\(card.id)" {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading current actions…")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                }
                .padding(.vertical, 4)
            }

            ForEach(capabilities) { capability in
                actionRow(
                    label: capability.label,
                    detail: capability.kind == recommendation?.kind ? recommendation?.rationale : nil,
                    systemImage: capability.kind.systemImage,
                    tint: capability.kind == recommendation?.kind ? theme.discoveryColor : theme.accentColor,
                    identifier: "worth-actions-capability-\(capability.kind.rawValue)-\(card.id)"
                ) {
                    perform(capability)
                }
            }
        }
    }

    private var feedbackSection: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text("FEEDBACK")
                .font(.themed(10, weight: .bold))
                .tracking(1.1)
                .foregroundColor(theme.secondaryTextColor)

            ForEach(ResurfacingFeedbackAction.allCases) { action in
                actionRow(
                    label: action.label,
                    systemImage: action.systemImage,
                    tint: feedbackTint(action),
                    identifier: "worth-actions-\(action.rawValue.replacingOccurrences(of: "open", with: "mark-useful"))-\(card.id)"
                ) {
                    withAnimation(.snappy(duration: 0.22)) {
                        viewModel.resolveResurfacing(card, action: action)
                    }
                    dismiss()
                }
            }
            // Resurfacing has no snooze; a dismissal may name one of the
            // resurfacing reasons.
            Menu {
                ForEach(ResurfacingDismissOption.all.filter { $0.code != nil }) { option in
                    Button(option.label, role: .destructive) {
                        withAnimation(.snappy(duration: 0.22)) {
                            viewModel.resolveResurfacing(card, action: .dismiss, reason: option.code)
                        }
                        dismiss()
                    }
                }
            } label: {
                HStack(spacing: 11) {
                    Image(systemName: "text.badge.xmark").font(.system(size: 15, weight: .semibold)).frame(width: 22)
                    Text("Dismiss with reason").font(.themed(14, weight: .semibold))
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.down").font(.caption2)
                }
                .foregroundColor(theme.dangerColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 13)
                .padding(.vertical, 11)
                .background(theme.elevatedColor)
                .clipShape(RoundedRectangle(cornerRadius: 11))
            }
            .disabled(viewModel.actionItemID != nil)
            .accessibilityIdentifier("worth-actions-dismiss-reason-\(card.id)")
        }
    }

    private func actionRow(
        label: String,
        detail: String? = nil,
        systemImage: String,
        tint: Color,
        identifier: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 11) {
                Image(systemName: systemImage)
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 2) {
                    Text(label).font(.themed(14, weight: .semibold))
                    if let detail, !detail.isEmpty {
                        Text("Recommended · \(detail)")
                            .font(.themed(10))
                            .lineLimit(2)
                            .opacity(0.78)
                    }
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.caption2)
            }
            .foregroundColor(tint)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 13)
            .padding(.vertical, 11)
            .background(theme.elevatedColor)
            .clipShape(RoundedRectangle(cornerRadius: 11))
        }
        .buttonStyle(.plain)
        .disabled(viewModel.actionItemID != nil)
        .opacity(viewModel.actionItemID == nil ? 1 : 0.55)
        .accessibilityIdentifier(identifier)
    }

    private func feedbackTint(_ action: ResurfacingFeedbackAction) -> Color {
        switch action {
        case .open: return theme.successColor
        case .acknowledge: return theme.accentColor
        case .dismiss: return theme.dangerColor
        }
    }

    private func perform(_ capability: ResurfacingActionCapability) {
        switch capability.kind {
        case .viewDetails:
            onOpenDetail()
        case .showOriginal:
            viewModel.fetchResurfacingDetail(card, original: true)
            onOpenDetail()
        case .openSource:
            if let raw = detail?.openURL, let url = URL(string: raw) {
                UIApplication.shared.open(url)
                dismiss()
            } else if let route = detail?.sourceRoute {
                openRoute(route)
            } else {
                onOpenDetail()
            }
        case .createTask, .createReminder, .share, .saveToMemory:
            inputAction = capability
        default:
            viewModel.performResurfacingAction(card, kind: capability.kind)
            onOpenDetail()
        }
    }

    private func openRoute(_ route: String) {
        if route.hasPrefix("http"), let url = URL(string: route) {
            UIApplication.shared.open(url)
            dismiss()
            return
        }
        let source = detail?.source?.objectValue
        if route.hasPrefix("/tasks") {
            AppActions.shared.requestTask(source?["task_id"]?.stringValue)
            dismiss()
        } else if route.hasPrefix("/t/") {
            AppActions.shared.requestThread(source?["thread_id"]?.stringValue ?? String(route.dropFirst(3)))
            dismiss()
        } else if route.hasPrefix("/briefing") {
            AppActions.shared.requestToday(section: .delivered)
            dismiss()
        } else if route.hasPrefix("/feed") || route.hasPrefix("/memory") {
            AppActions.shared.requestToday(section: .changed, activity: true)
            dismiss()
        } else {
            onOpenDetail()
        }
    }
}

private struct ResurfacingActionInputView: View {
    let card: ResurfacingCard
    let capability: ResurfacingActionCapability
    @ObservedObject var viewModel: TodayViewModel
    private let reminderPendingKey: String
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var instruction = ""
    @State private var recipient = ""
    @State private var channel = "email"
    @State private var fact = ""
    @State private var reminderDate = Date().addingTimeInterval(3_600)
    @State private var reminderTimeZoneIdentifier = TimeZone.current.identifier
    @State private var reminderReceiptID: String?
    @State private var reminderIdempotencyKey = UUID().uuidString
    @State private var isSubmittingReminder = false
    @State private var reminderCommitted = false
    @State private var reminderError: String?

    @MainActor
    init(card: ResurfacingCard, capability: ResurfacingActionCapability, viewModel: TodayViewModel) {
        self.card = card
        self.capability = capability
        _viewModel = ObservedObject(wrappedValue: viewModel)
        let key = AppleReminderPendingReceiptStore.operationKey(candidateID: card.id)
        reminderPendingKey = key
        if capability.kind == .createReminder,
           let pending = AppleReminderPendingReceiptStore.shared.receipt(for: key) {
            _title = State(initialValue: pending.title)
            _instruction = State(initialValue: pending.notes)
            _reminderDate = State(initialValue: pending.dueAt)
            _reminderTimeZoneIdentifier = State(initialValue: pending.timeZoneIdentifier)
            _reminderReceiptID = State(initialValue: pending.identifier)
            _reminderIdempotencyKey = State(initialValue: pending.idempotencyKey)
        }
    }

    var body: some View {
        NavigationView {
            Form {
                Section { Text(card.sourceTitle.isEmpty ? card.line : card.sourceTitle) }
                if [.createTask, .createReminder, .saveToMemory].contains(capability.kind) {
                    TextField("Title", text: $title)
                        .disabled(capability.kind == .createReminder && reminderReceiptID != nil)
                }
                if capability.kind == .createTask || capability.kind == .createReminder {
                    Section(capability.kind == .createReminder ? "Reminder note" : "Instruction") {
                        TextEditor(text: $instruction)
                            .frame(minHeight: 110)
                            .disabled(capability.kind == .createReminder && reminderReceiptID != nil)
                    }
                }
                if capability.kind == .createReminder {
                    Text("Creates a native reminder with an alert, then opens Apple Reminders.")
                        .font(.caption)
                        .foregroundColor(theme.secondaryTextColor)
                    DatePicker("Date and time", selection: $reminderDate, displayedComponents: [.date, .hourAndMinute])
                        .disabled(reminderReceiptID != nil)
                    LabeledContent("Timezone", value: reminderTimeZoneIdentifier)
                    if let reminderError {
                        Text(reminderError).font(.caption).foregroundColor(theme.dangerColor)
                    }
                }
                if capability.kind == .share {
                    TextField("Recipient", text: $recipient)
                    Picker("Channel", selection: $channel) {
                        Text("Email").tag("email"); Text("WhatsApp").tag("whatsapp")
                        Text("Telegram").tag("telegram"); Text("iMessage").tag("imessage")
                    }
                    Section("Draft instruction") { TextEditor(text: $instruction).frame(minHeight: 100) }
                }
                if capability.kind == .saveToMemory {
                    Section("Fact submitted for review") { TextEditor(text: $fact).frame(minHeight: 120) }
                }
            }
            .navigationTitle(capability.label)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                        .disabled(isSubmittingReminder)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button { submit() } label: {
                        if isSubmittingReminder {
                            ProgressView()
                        } else {
                            Text(reminderCommitted ? "Open Reminders" : "Continue")
                        }
                    }
                    .disabled(!valid || isSubmittingReminder)
                }
            }
            .onAppear {
                if reminderReceiptID == nil {
                    title = String((card.sourceTitle.isEmpty ? card.line : card.sourceTitle).prefix(200))
                    instruction = String(card.summary.prefix(4_000))
                }
                fact = String(card.summary.prefix(1_200))
            }
            .interactiveDismissDisabled(isSubmittingReminder)
        }
    }

    private var valid: Bool {
        switch capability.kind {
        case .createTask:
            return !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .createReminder:
            return !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !instruction.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && (reminderReceiptID != nil || reminderDate > Date())
        case .share: return !recipient.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .saveToMemory: return !fact.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        default: return true
        }
    }

    private func submit() {
        var input: [String: Any] = [:]
        switch capability.kind {
        case .createTask: input = ["title": title, "instruction": instruction]
        case .createReminder:
            submitAppleReminder()
            return
        case .share: input = ["recipient": recipient, "channel": channel, "instruction": instruction]
        case .saveToMemory: input = ["title": title, "fact": fact]
        default: break
        }
        viewModel.performResurfacingAction(card, kind: capability.kind, input: input)
        dismiss()
    }

    private func submitAppleReminder() {
        isSubmittingReminder = true
        reminderError = nil
        Task {
            do {
                if reminderCommitted {
                    let opened = await AppleReminderService.shared.openReminders()
                    isSubmittingReminder = false
                    if opened {
                        dismiss()
                    } else {
                        reminderError = "The Apple Reminder exists, but iOS could not open Reminders. Open it from your Home Screen or try again."
                    }
                    return
                }
                if reminderReceiptID == nil {
                    let timeZone = TimeZone(identifier: reminderTimeZoneIdentifier) ?? .current
                    let created = try await AppleReminderService.shared.create(
                        title: title,
                        notes: instruction,
                        dueAt: reminderDate,
                        timeZone: timeZone
                    )
                    reminderReceiptID = created.identifier
                    AppleReminderPendingReceiptStore.shared.save(
                        PendingAppleReminderReceipt(
                            idempotencyKey: reminderIdempotencyKey,
                            identifier: created.identifier,
                            title: title,
                            notes: instruction,
                            dueAt: reminderDate,
                            timeZoneIdentifier: reminderTimeZoneIdentifier,
                            createdAt: Date()
                        ),
                        for: reminderPendingKey
                    )
                }
                guard let reminderReceiptID else {
                    throw AppleReminderServiceError.noWritableList
                }
                let input: [String: Any] = [
                    "title": title,
                    "instruction": instruction,
                    "at": ISO8601DateFormatter().string(from: reminderDate),
                    "timezone": reminderTimeZoneIdentifier,
                    "delivery": "client_apple_eventkit",
                    "external_id": reminderReceiptID
                ]
                viewModel.performResurfacingAction(
                    card,
                    kind: .createReminder,
                    input: input,
                    idempotencyKey: reminderIdempotencyKey
                ) { result in
                    switch result {
                    case .success:
                        AppleReminderPendingReceiptStore.shared.remove(for: reminderPendingKey)
                        reminderCommitted = true
                        Task {
                            let opened = await AppleReminderService.shared.openReminders()
                            isSubmittingReminder = false
                            if opened {
                                dismiss()
                            } else {
                                reminderError = "The Apple Reminder was created, but iOS could not open Reminders. Open it from your Home Screen or tap Open Reminders to retry."
                            }
                        }
                    case .failure(let error):
                        isSubmittingReminder = false
                        reminderError = "The Apple Reminder was created, but Magican could not update this card: \(error.localizedDescription). Tap Continue to retry without creating another reminder."
                    }
                }
            } catch {
                isSubmittingReminder = false
                reminderError = error.localizedDescription
            }
        }
    }
}

struct ChannelFollowUpDetailView: View {
    let item: ChannelFollowUp
    @ObservedObject var client: ChannelFollowUpActionClient
    let onResolved: () -> Void
    let onResolutionRequested: (_ action: String, _ hint: String?, _ reason: String?) -> Void
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var hint = ""
    @State private var showingHint = false
    @State private var composeAction: ChannelActionDescriptor?
    @State private var composeText = ""
    @State private var composeID: String?
    @State private var composeHint = ""
    @State private var directConfirmation: ChannelActionDescriptor?
    @State private var showingWritingStyle = false

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    HStack {
                        badge(item.label ?? "follow up", theme.warningColor)
                        badge(item.provider, theme.accentColor)
                        badge(item.lane == "envoy" ? "Magican" : "You", theme.discoveryColor)
                    }
                    Text(item.subject ?? "(no subject)").font(.themed(23, weight: .bold)).foregroundColor(theme.textColor)
                    if let sender = item.sender { Text(sender).font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
                    if let received = item.receivedAt { LabeledContent("Received", value: TodayViewModel.localDateTime(received)) }
                    if !item.accountAlias.isEmpty { LabeledContent("Account", value: item.accountEmail ?? item.accountAlias) }
                    if let summary = item.summary { Text(summary).font(.themed(15)).foregroundColor(theme.secondaryTextColor) }
                    if let reason = item.reason { Label(reason, systemImage: "lightbulb").font(.themed(12)).foregroundColor(theme.warningColor) }
                    if let action = item.actionSummary {
                        VStack(alignment: .leading, spacing: 4) { Text("Proposed action").font(.themed(11, weight: .bold)); Text(action).font(.themed(13)) }
                            .foregroundColor(theme.textColor).padding(10).background(theme.elevatedColor).cornerRadius(10)
                    }
                    Divider()
                    if item.reviewRequired {
                        Label("A newer message made this recommendation stale. Review the evidence before re-opening it.",
                              systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(13, weight: .semibold)).foregroundColor(theme.warningColor)
                            .padding(10).background(theme.warningColor.opacity(0.12)).cornerRadius(10)
                        Button { resolve("review") } label: {
                            Label("Review & re-open", systemImage: "arrow.clockwise")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent).tint(theme.accentColor)
                        .foregroundColor(theme.onAccentColor)
                    } else if showingHint {
                        Text("Optional instruction for the agent").font(.themed(13, weight: .semibold)).foregroundColor(theme.textColor)
                        TextEditor(text: $hint).frame(minHeight: 100).padding(6).background(theme.elevatedColor).cornerRadius(10)
                        HStack {
                            Button("Cancel") { showingHint = false }.buttonStyle(.bordered)
                            Button("Create follow-up task") { resolve("approve", hint: hint) }
                                .buttonStyle(.borderedProminent).tint(theme.accentColor)
                                .foregroundColor(theme.onAccentColor)
                        }
                    } else {
                        Button { showingHint = true } label: { Label("Do it", systemImage: "bolt.fill").frame(maxWidth: .infinity) }
                            .buttonStyle(.borderedProminent).tint(theme.accentColor)
                            .foregroundColor(theme.onAccentColor)
                    }
                    // Web parity (ChannelFollowUpActions): Useful (positive) + Acknowledge
                    // (neutral — labelled "Acknowledge", not "Acknowledged") + Snooze +
                    // Dismiss (one-click, with an optional-reason menu).
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack {
                            Button { resolve("useful") } label: { Label("Useful", systemImage: "hand.thumbsup.fill") }
                                .buttonStyle(.bordered).tint(theme.successColor)
                                .accessibilityIdentifier("channel-detail-useful")
                            if item.canAcknowledge {
                                Button("Acknowledge") { resolve("acknowledge") }
                                    .buttonStyle(.bordered)
                                    .accessibilityIdentifier("channel-detail-acknowledge")
                            }
                            // No duration on the wire for channel follow-ups: a
                            // snooze hides the card from Today (no time picker).
                            Button { resolve("snooze") } label: { Label(TodayView.followUpSnoozeLabel, systemImage: "clock") }
                                .buttonStyle(.bordered).tint(theme.discoveryColor)
                                .accessibilityIdentifier("channel-detail-snooze")
                            Menu("Dismiss") {
                                ForEach(ChannelFollowUpDismissOption.all) { option in
                                    Button(option.label, role: option.code == nil ? nil : .destructive) {
                                        resolve("dismiss", reason: option.code)
                                    }
                                }
                            }.buttonStyle(.bordered).tint(theme.dangerColor)
                            .accessibilityIdentifier("channel-detail-dismiss")
                        }
                    }

                    if !item.availableActions.isEmpty {
                        Divider()
                        Text("Channel actions")
                            .font(.themed(16, weight: .bold)).foregroundColor(theme.textColor)
                        ForEach(item.availableActions) { descriptor in
                            Button { begin(descriptor) } label: {
                                HStack {
                                    Label(descriptor.label, systemImage: descriptor.systemImage)
                                    Spacer()
                                    if client.busyKey?.contains(":\(descriptor.id)") == true { ProgressView() }
                                    else { Image(systemName: "chevron.right") }
                                }
                                .frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.borderedProminent)
                            .tint(theme.accentColor)
                            .foregroundColor(theme.onAccentColor)
                            .disabled(client.busyKey != nil)
                            .accessibilityIdentifier("channel-action-\(descriptor.id)")
                        }
                    }

                    if let descriptor = composeAction {
                        VStack(alignment: .leading, spacing: 10) {
                            HStack {
                                Text(descriptor.label).font(.themed(16, weight: .bold))
                                Spacer()
                                Button("Cancel") { closeComposer() }.buttonStyle(.plain)
                            }
                            if client.busyKey?.hasPrefix("compose:") == true && composeID == nil {
                                ProgressView("Drafting locally…")
                            }
                            TextEditor(text: $composeText)
                                .frame(minHeight: 150).padding(7)
                                .background(theme.elevatedColor).cornerRadius(10)
                                .accessibilityIdentifier("channel-action-compose-text")
                            TextField("Optional redraft instruction", text: $composeHint, axis: .vertical)
                                .textFieldStyle(.roundedBorder)
                            HStack {
                                Button { draft(descriptor) } label: {
                                    Label("Redraft", systemImage: "arrow.clockwise")
                                }
                                .buttonStyle(.bordered)
                                Spacer()
                                Button { send(descriptor) } label: {
                                    Label("Send", systemImage: "paperplane.fill")
                                }
                                .buttonStyle(.borderedProminent).tint(theme.accentColor)
                                .foregroundColor(theme.onAccentColor)
                                .disabled(composeText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || client.busyKey != nil)
                            }
                        }
                        .padding(12).background(theme.surfaceColor).cornerRadius(12)
                    }

                    if let error = client.error {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(12)).foregroundColor(theme.dangerColor)
                            .accessibilityIdentifier("channel-action-error")
                    }
                    // Web parity ("Show message" split button): Show message + Open thread
                    // + Writing style (the ▾ menu's secondary options).
                    HStack {
                        Button { Task { await client.fetchMessage(for: item) } } label: { Label("Show message", systemImage: "envelope.open") }
                            .buttonStyle(.bordered)
                        if let raw = item.openURL, let url = URL(string: raw) {
                            Button { UIApplication.shared.open(url) } label: { Label("Open thread", systemImage: "arrow.up.right.square") }.buttonStyle(.bordered)
                        }
                        Button { showingWritingStyle = true } label: { Label("Writing style", systemImage: "textformat") }
                            .buttonStyle(.bordered)
                            .accessibilityIdentifier("channel-detail-writing-style")
                    }
                    if client.busyKey == "message:\(item.id)" { ProgressView("Fetching message…") }
                    if let message = client.messages[item.id] { messageEvidence(message) }
                }.padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Follow-up")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .confirmationDialog(directConfirmation?.label ?? "Confirm action",
                                isPresented: Binding(
                                    get: { directConfirmation != nil },
                                    set: { if !$0 { directConfirmation = nil } }
                                ), titleVisibility: .visible) {
                if let descriptor = directConfirmation {
                    Button(descriptor.label) { commitDirect(descriptor) }
                }
                Button("Cancel", role: .cancel) { directConfirmation = nil }
            }
            .sheet(isPresented: $showingWritingStyle) {
                ChannelWritingStyleView(item: item, client: client)
            }
        }
    }

    private func badge(_ value: String, _ color: Color) -> some View {
        Text(value.replacingOccurrences(of: "_", with: " ").capitalized).font(.themed(9, weight: .bold))
            .padding(.horizontal, 7).padding(.vertical, 4).foregroundColor(color).background(color.opacity(0.1)).clipShape(Capsule())
    }

    @ViewBuilder private func messageEvidence(_ message: ChannelMessageView) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if message.hasNewer { Label("A newer message arrived after the summarized message.", systemImage: "exclamationmark.triangle.fill").font(.themed(12, weight: .semibold)).foregroundColor(theme.warningColor) }
            if let summary = message.summary { Text("Summary").font(.themed(10, weight: .bold)); Text(summary).font(.themed(13)).foregroundColor(theme.secondaryTextColor) }
            if message.evidenceMessages.count > 1 { Text("EVIDENCE BATCH").font(.themed(10, weight: .bold)).foregroundColor(theme.secondaryTextColor) }
            ForEach(message.evidenceMessages) { evidence in
                VStack(alignment: .leading, spacing: 4) {
                    Text(evidence.subject ?? "Message").font(.themed(12, weight: .semibold)).foregroundColor(theme.textColor)
                    Text(evidence.body ?? "Body unavailable for this evidence message.").font(.themedMono(.caption)).foregroundColor(theme.secondaryTextColor).textSelection(.enabled)
                }.padding(9).background(theme.elevatedColor).cornerRadius(9)
            }
            if message.evidenceMessages.isEmpty { Text(message.body ?? "Message unavailable or suppressed.").font(.themedMono(.caption)).foregroundColor(theme.secondaryTextColor).textSelection(.enabled) }
        }.padding(11).background(theme.surfaceColor).cornerRadius(11)
    }

    private func begin(_ descriptor: ChannelActionDescriptor) {
        client.clearError()
        if descriptor.needsCompose {
            composeAction = descriptor
            composeText = ""
            composeID = nil
            composeHint = ""
            draft(descriptor)
        } else if descriptor.confirm {
            directConfirmation = descriptor
        } else {
            commitDirect(descriptor)
        }
    }

    private func draft(_ descriptor: ChannelActionDescriptor) {
        Task {
            guard let draft = await client.compose(descriptor, for: item, hint: composeHint) else { return }
            composeID = draft.composeID
            composeText = draft.text
        }
    }

    private func send(_ descriptor: ChannelActionDescriptor) {
        Task {
            if await client.commit(descriptor, for: item, body: composeText, composeID: composeID) {
                finish()
            }
        }
    }

    private func commitDirect(_ descriptor: ChannelActionDescriptor) {
        directConfirmation = nil
        Task {
            if await client.commit(descriptor, for: item) { finish() }
        }
    }

    private func closeComposer() {
        composeAction = nil
        composeText = ""
        composeID = nil
        composeHint = ""
        client.clearError()
    }

    private func resolve(_ action: String, hint: String? = nil, reason: String? = nil) {
        client.clearError()
        onResolutionRequested(action, hint, reason)
        dismiss()
    }

    private func finish() {
        onResolved()
        dismiss()
    }
}

/// Web parity: the ChannelFollowUpActions "Writing style" modal — lists the
/// learned exact statements for the sender/domain and lets the owner learn a new
/// one (optionally promoting it) or promote/dismiss a candidate.
private struct ChannelWritingStyleView: View {
    let item: ChannelFollowUp
    @ObservedObject var client: ChannelFollowUpActionClient
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss

    @State private var preferences: [ChannelWritingPreference] = []
    @State private var loading = false
    @State private var statement = ""
    @State private var scope = "sender"
    @State private var promoteImmediately = false

    var body: some View {
        NavigationView {
            Form {
                Section {
                    Text("Exact statements used for \(item.sender ?? item.subject ?? "this conversation").")
                        .font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                }
                Section("Learned statements") {
                    if loading {
                        ProgressView("Loading preferences…")
                    } else if preferences.isEmpty {
                        Text("No learned writing preferences yet.")
                            .font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                    } else {
                        ForEach(preferences) { preference in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(preference.statement).font(.themed(14, weight: .semibold)).foregroundColor(theme.textColor)
                                Text("\(preference.scopeKind) · \(preference.status) · evidence \(preference.evidenceCount)")
                                    .font(.themed(10)).foregroundColor(theme.secondaryTextColor)
                                HStack(spacing: 10) {
                                    if preference.status == "candidate" {
                                        Button("Promote") { update(preference, action: "promote") }
                                            .font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                                    }
                                    Button("Dismiss", role: .destructive) { update(preference, action: "dismiss") }
                                        .font(.themed(12, weight: .semibold))
                                }
                            }
                            .padding(.vertical, 2)
                        }
                    }
                }
                Section("Learn an exact statement") {
                    TextEditor(text: $statement).frame(minHeight: 90)
                    Picker("Apply to", selection: $scope) {
                        Text("This sender").tag("sender")
                        Text("This sender's domain").tag("domain")
                    }
                    Toggle("Promote immediately", isOn: $promoteImmediately)
                    Button("Learn preference") { learn() }
                        .disabled(statement.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || client.busyKey != nil)
                        .accessibilityIdentifier("channel-writing-learn")
                }
                if let error = client.error {
                    Section {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .font(.themed(12)).foregroundColor(theme.dangerColor)
                    }
                }
            }
            .navigationTitle("Writing preferences")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .onAppear { reload() }
        }
    }

    private func reload() {
        loading = true
        Task {
            preferences = await client.fetchWritingPreferences(for: item)
            loading = false
        }
    }

    private func learn() {
        Task {
            if await client.learnWritingPreference(for: item, scope: scope,
                                                   statement: statement, promote: promoteImmediately) {
                statement = ""
                preferences = await client.fetchWritingPreferences(for: item)
            }
        }
    }

    private func update(_ preference: ChannelWritingPreference, action: String) {
        Task {
            if await client.updateWritingPreference(id: preference.id, action: action) {
                preferences = await client.fetchWritingPreferences(for: item)
            }
        }
    }
}

private struct TodayBriefingDetailView: View {
    let briefing: TodayBriefing
    @ObservedObject var viewModel: TodayViewModel
    @StateObject private var theme = ThemeManager.shared
    @Environment(\.dismiss) private var dismiss
    @State private var task: TaskStatusModel?

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Image(systemName: "doc.text.image.fill")
                        .font(.title).foregroundColor(theme.accentColor)
                    Text(briefing.surface.title)
                        .font(.themed(24, weight: .bold)).foregroundColor(theme.textColor)
                    if let summary = briefing.surface.summary ?? briefing.taskTitle {
                        Text(summary).font(.themed(15)).foregroundColor(theme.secondaryTextColor)
                    }
                    if viewModel.actionItemID == "briefing:\(briefing.id)" { ProgressView("Rendering briefing…") }
                    if let render = viewModel.briefingRenders[briefing.id] {
                        if let document = render.muijDocument {
                            MuijDocumentView(document: document)
                        } else if let text = render.textContent, !text.isEmpty {
                            Markdown(text)
                                .markdownTextStyle { ForegroundColor(theme.textColor) }
                                .textSelection(.enabled)
                                .padding(12).background(theme.elevatedColor).cornerRadius(11)
                        } else if let json = render.jsonContent {
                            MuijJSONContentView(value: json)
                        } else if let unavailable = render.unavailableReason {
                            Text(unavailable).font(.themed(12)).foregroundColor(theme.warningColor)
                        } else if let summary = briefing.sourceOutputSummary, !summary.isEmpty {
                            Markdown(summary).markdownTextStyle { ForegroundColor(theme.textColor) }.textSelection(.enabled)
                        } else {
                            ContentUnavailableView("No rendered content", systemImage: "doc.text", description: Text("This briefing did not publish a displayable artifact."))
                        }
                    }
                    Divider()
                    LabeledContent("Published", value: TodayView.shortDate(briefing.surface.publishedAt))
                    if let title = briefing.taskTitle { LabeledContent("Task", value: title) }
                    if let agent = briefing.sourceAgentID { LabeledContent("Agent", value: agent) }
                    if let kind = briefing.renderKind { LabeledContent("Render", value: kind.replacingOccurrences(of: "_", with: " ").capitalized) }
                    if let taskID = briefing.surface.taskID {
                        Button {
                            task = TaskStatusModel(taskId: taskID, title: briefing.taskTitle ?? briefing.surface.title,
                                                   status: "completed", steps: [])
                        } label: {
                            Label("Inspect delivery", systemImage: "arrow.up.right.square").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent).tint(theme.accentColor)
                        .foregroundColor(theme.onAccentColor)
                    }
                }
                .padding()
            }
            .background(theme.backgroundColor.ignoresSafeArea())
            .navigationTitle("Briefing")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .sheet(item: $task) { DeepWorkPanel(task: $0) }
            .onAppear { viewModel.fetchBriefingRender(briefing) }
        }
    }
}
