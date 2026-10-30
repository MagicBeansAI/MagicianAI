import SwiftUI
import UIKit

/// What a deck card commit asks the page to do. `snooze` (follow-up) and
/// `open` (worth: mark useful + open the source) come from the Act menu.
enum TodayDeckAction: Equatable {
    case useful, acknowledge, dismiss, snooze, open
}

/// The Reading Room's "Morning Brief" — a Bumble-style swipe deck over the
/// interleaved channel follow-ups and worth cards (web `TodayTriageDeck`).
///
/// Drag the top card: right past 95pt = Useful, left = Dismiss; a flick
/// whose predicted end crosses the threshold also commits. The card claims
/// ONLY horizontal drags — a vertical drag that starts on it scrolls the page
/// and never triages; Seen (acknowledge) is the button. A plain tap (or the
/// footer's "Details ›") opens the existing rich detail sheet. ⚡ Act opens a
/// menu of the card's real actions (`TodayDeckCard.actEntries`): snooze and
/// Open commit like a swipe; the rest hand off to `onAct` / `onOpen` and
/// leave the card on top.
/// Triage is optimistic through the view model; a failed action reports back
/// through `onAction`'s completion and the card returns to the stack.
struct TodayMorningBriefDeck: View {
    let cards: [TodayDeckCard]
    let isLoadingMore: Bool
    var forYouLoading = false
    var worthLoading = false
    var followUpTotal = 0
    var worthTotal = 0
    var hasMoreForYou = false
    var hasMoreWorth = false
    var pageRevision = 0
    var loadErrors: [String: String] = [:]
    let onNeedMore: (TodayDeckTab) -> Void
    let onAction: (TodayDeckCard, TodayDeckAction, @escaping (Bool) -> Void) -> Void
    let onOpen: (TodayDeckCard) -> Void
    /// Act-menu entries that neither resolve the card nor open Details.
    let onAct: (TodayDeckCard, TodayDeckActEntry) -> Void
    let onOpenBroadsheet: () -> Void

    @State private var tab = TodayDeckTab.all
    /// Locally triaged card ids → whether the card was a For You card, so
    /// the complete state can still say how many were cleared per tab after
    /// the view model has dropped them.
    @State private var triaged: [String: Bool] = [:]
    @State private var drag = CGSize.zero
    /// Axis lock for the current drag: nil until it first moves, then
    /// horizontal (the card's) or vertical (the page's).
    @State private var dragIsHorizontal: Bool?
    /// True while a drag is live; resets even when the page scroll cancels
    /// the gesture (then `onEnded` never runs), so the lock and any partial
    /// offset are cleared instead of sticking.
    @GestureState private var dragging = false
    @State private var flying: TodayDeckAction?
    @State private var fading = false
    @ObservedObject private var theme = ThemeManager.shared
    @ObservedObject private var primaryAgent = PrimaryAgentSiriAdvertiser.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static let cardHeight: CGFloat = 300
    private static let flyDuration = 0.26

    private var remaining: [TodayDeckCard] { cards.filter { triaged[$0.id] == nil } }
    private var stack: [TodayDeckCard] { TodayMorningEdition.deckCards(remaining, in: tab) }

    private func triagedCount(in tab: TodayDeckTab) -> Int {
        triaged.values.filter { isForYou in
            switch tab {
            case .all: return true
            case .forYou: return isForYou
            case .worth: return !isForYou
            }
        }.count
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            tabs
            Text(initialLoading ? "Loading Reading Room…" : "\(stack.count) loaded · \(max(stack.count, total(in: tab))) total")
                .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
            stage
            if let top = stack.first {
                controls(top)
            }
        }
        .onChange(of: stack.count) { _, _ in requestMoreIfLow() }
        .onChange(of: tab) { _, _ in requestMoreIfLow() }
        .onChange(of: initialLoading) { _, loading in if !loading { requestMoreIfLow() } }
        .onChange(of: pageRevision) { _, _ in requestMoreIfLow() }
        .onChange(of: isLoadingMore) { _, loading in if !loading { requestMoreIfLow() } }
        .onAppear { requestMoreIfLow() }
    }

    private var initialLoading: Bool {
        (tab != .worth && forYouLoading) || (tab != .forYou && worthLoading)
    }
    private var hasMore: Bool {
        switch tab {
        case .all: return hasMoreForYou || hasMoreWorth
        case .forYou: return hasMoreForYou
        case .worth: return hasMoreWorth
        }
    }
    private var loadError: String? {
        if tab != .worth, let error = loadErrors["message_followups"] { return error }
        if tab != .forYou, let error = loadErrors["worth_a_look"] { return error }
        return nil
    }
    private func total(in tab: TodayDeckTab) -> Int {
        switch tab { case .all: return followUpTotal + worthTotal; case .forYou: return followUpTotal; case .worth: return worthTotal }
    }
    private func requestMoreIfLow() {
        if !initialLoading && !isLoadingMore && loadError == nil && hasMore && stack.count <= 4 { onNeedMore(tab) }
    }

    // MARK: Tabs

    private var tabs: some View {
        HStack(spacing: 6) {
            ForEach(TodayDeckTab.allCases) { option in
                let count = max(total(in: option), TodayMorningEdition.deckCards(remaining, in: option).count)
                Button {
                    guard tab != option else { return }
                    tab = option
                    drag = .zero
                } label: {
                    HStack(spacing: 5) {
                        Text(option.title).font(.themed(12, weight: .semibold)).lineLimit(1)
                        if count > 0 && !initialLoading {
                            Text("\(count)")
                                .font(.themedMono(10, weight: .bold))
                                .foregroundColor(tab == option ? theme.onAccentColor : theme.accentColor)
                                .padding(.horizontal, 5).padding(.vertical, 1)
                                .background(tab == option ? theme.onAccentColor.opacity(0.22) : theme.accentColor.opacity(0.12))
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                        }
                    }
                    .foregroundColor(tab == option ? theme.onAccentColor : theme.textColor)
                    .padding(.horizontal, 10).frame(height: 30)
                    .background(tab == option ? theme.accentFillColor : theme.cardColor)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor.opacity(tab == option ? 0 : 1)))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(tab == option ? .isSelected : [])
                .accessibilityIdentifier("today-deck-tab-\(option.rawValue)")
            }
            Spacer(minLength: 0)
            if isLoadingMore { ProgressView().controlSize(.small) }
        }
    }

    // MARK: Stage

    @ViewBuilder private var stage: some View {
        let current = stack
        if current.isEmpty && (initialLoading || isLoadingMore) {
            VStack(spacing: 12) {
                ProgressView().tint(theme.accentColor)
                Text(initialLoading ? "Loading Reading Room…" : "Loading more cards…").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            }.frame(maxWidth: .infinity).frame(height: Self.cardHeight + 24)
        } else if current.isEmpty && (hasMore || loadError != nil) {
            VStack(spacing: 12) {
                Text(loadError ?? "More cards are available.").font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                Button("Load more cards") { onNeedMore(tab) }.buttonStyle(.bordered)
            }.frame(maxWidth: .infinity).frame(height: Self.cardHeight + 24)
        } else if current.isEmpty { emptyState }
        else {
            ZStack(alignment: .top) {
                if current.count > 2 { preview(current[2], depth: 2) }
                if current.count > 1 { preview(current[1], depth: 1) }
                topCard(current[0])
            }
            .frame(height: Self.cardHeight + 24, alignment: .top)
            .frame(maxWidth: .infinity)
        }
    }

    private func preview(_ card: TodayDeckCard, depth: Int) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            TodayKicker(text: card.category, color: categoryColor(card), size: 9)
            Text(card.title)
                .font(TodayNewsprint.serif(17, weight: .semibold))
                .foregroundColor(theme.textColor.opacity(0.75))
                .lineLimit(2)
            Spacer(minLength: 0)
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(height: Self.cardHeight)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(theme.cardBorderColor))
        .scaleEffect(depth == 1 ? 0.96 : 0.92, anchor: .bottom)
        .offset(y: depth == 1 ? 12 : 24)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }

    private func topCard(_ card: TodayDeckCard) -> some View {
        let dx = Double(drag.width)
        return VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(card.category)
                    .font(.themedMono(9, weight: .bold))
                    .tracking(1)
                    .foregroundColor(categoryColor(card))
                    .padding(.horizontal, 7).padding(.vertical, 4)
                    .background(categoryColor(card).opacity(0.1))
                    .clipShape(Capsule())
                    .lineLimit(1)
                Spacer(minLength: 4)
                if let sender = card.sender {
                    Text(sender)
                        .font(.themed(11, weight: .medium))
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                }
            }
            if let dateline = card.dateline {
                Text(dateline).font(.themed(11)).foregroundColor(theme.secondaryTextColor)
            }
            Text(card.title)
                .font(TodayNewsprint.serif(23, weight: .semibold))
                .foregroundColor(theme.textColor)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)
            Text(card.summary)
                .font(.themed(14))
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(3)
            Spacer(minLength: 0)
            HStack(spacing: 8) {
                if case .followUp(let item) = card.source, let raw = item.openURL, let url = URL(string: raw) {
                    Button { UIApplication.shared.open(url) } label: {
                        Text("View thread in \(TodayMorningEdition.providerDisplayName(item.provider)) \u{2197}\u{FE0E}")
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(theme.accentColor)
                            .lineLimit(1)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("today-deck-open-thread")
                }
                Spacer(minLength: 4)
                // Discoverable twin of the plain card tap.
                Button { if flying == nil { onOpen(card) } } label: {
                    Text("Details \u{203A}")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(theme.secondaryTextColor)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("today-deck-details")
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(height: Self.cardHeight)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(theme.cardBorderColor))
        .overlay(alignment: .topLeading) {
            stamp("USEFUL", color: theme.successColor, angle: 14,
                  opacity: flying == .useful ? 1 : TodayMorningEdition.usefulStampOpacity(dx: dx))
                .padding(18)
        }
        .overlay(alignment: .topTrailing) {
            stamp("DISMISS", color: theme.dangerColor, angle: -14,
                  opacity: flying == .dismiss ? 1 : TodayMorningEdition.dismissStampOpacity(dx: dx))
                .padding(18)
        }
        .overlay(alignment: .top) {
            // Seen is button-only, so its stamp inks only on commit.
            stamp("ACKNOWLEDGED", color: theme.infoColor, angle: 0, opacity: flying == .acknowledge ? 1 : 0)
                .padding(.top, 54)
        }
        .shadow(color: Color.black.opacity(0.10), radius: 10, y: 4)
        .contentShape(RoundedRectangle(cornerRadius: 12))
        .offset(reduceMotion ? .zero : drag)
        .rotationEffect(.degrees(reduceMotion ? 0 : dx * 0.07))
        .opacity(fading ? 0 : 1)
        // Horizontal-only: on iOS 18+ a UIKit pan that won't begin for a
        // vertical drag, so the page keeps scrolling when a scroll starts on
        // the card; iOS 17 keeps the simultaneous SwiftUI drag.
        .horizontalSwipe(
            fallback: dragGesture(card),
            onChanged: { deckDragChanged($0) },
            onEnded: { deckDragEnded(card, translation: $0, predicted: $1) },
            onCancelled: { deckDragCancelled() }
        )
        .onChange(of: dragging) { _, live in
            guard !live else { return }
            DispatchQueue.main.async {
                dragIsHorizontal = nil
                if flying == nil, drag != .zero {
                    withAnimation(.spring(response: 0.3, dampingFraction: 0.72)) { drag = .zero }
                }
            }
        }
        .onTapGesture { if flying == nil { onOpen(card) } }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Opens the full card. Swipe right for useful, left to dismiss.")
        .accessibilityIdentifier("today-deck-card")
        .accessibilityValue(card.id)
        // Impressions are recorded for the TOP card only, once it has been
        // visible for the delivery's policy duration.
        .verifiedAttentionVisibility(card.deliveryBinding)
        .id(card.id)
    }

    private func stamp(_ text: String, color: Color, angle: Double, opacity: Double) -> some View {
        Text(text)
            .font(TodayNewsprint.serif(26, weight: .heavy))
            .tracking(1.5)
            .foregroundColor(color)
            .padding(.horizontal, 10).padding(.vertical, 3)
            .background(color.opacity(0.08))
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(color, lineWidth: 3))
            .rotationEffect(.degrees(angle))
            .opacity(opacity)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }

    private func dragGesture(_ card: TodayDeckCard) -> some Gesture {
        DragGesture(minimumDistance: 12)
            .updating($dragging) { _, live, _ in live = true }
            .onChanged { value in
                guard flying == nil else { return }
                if dragIsHorizontal == nil {
                    dragIsHorizontal = TodayMorningEdition.isHorizontalDeckDrag(value.translation)
                }
                guard dragIsHorizontal == true else { return }
                drag = CGSize(width: value.translation.width, height: 0)
            }
            .onEnded { value in
                let horizontal = dragIsHorizontal == true
                dragIsHorizontal = nil
                guard horizontal else { return }
                deckDragEnded(card, translation: value.translation, predicted: value.predictedEndTranslation)
            }
    }

    /// iOS 18+ pan: it only began because the drag is horizontal.
    private func deckDragChanged(_ translation: CGSize) {
        guard flying == nil else { return }
        dragIsHorizontal = true
        drag = CGSize(width: translation.width, height: 0)
    }

    private func deckDragEnded(_ card: TodayDeckCard, translation: CGSize, predicted: CGSize) {
        dragIsHorizontal = nil
        guard flying == nil else { return }
        if let swipe = TodayMorningEdition.deckRelease(translation: translation, predicted: predicted) {
            switch swipe {
            case .useful: commit(card, .useful)
            case .dismiss: commit(card, .dismiss)
            }
        } else {
            withAnimation(.spring(response: 0.3, dampingFraction: 0.72)) { drag = .zero }
        }
    }

    private func deckDragCancelled() {
        dragIsHorizontal = nil
        guard flying == nil else { return }
        withAnimation(.spring(response: 0.3, dampingFraction: 0.72)) { drag = .zero }
    }

    /// Stamp at full ink, fly the card out (or just fade under Reduce
    /// Motion), then mark it triaged locally and fire the action.
    private func commit(_ card: TodayDeckCard, _ action: TodayDeckAction) {
        guard flying == nil else { return }
        flying = action
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        let width = UIScreen.main.bounds.width * 1.2
        withAnimation(.easeIn(duration: Self.flyDuration)) {
            if reduceMotion {
                fading = true
            } else {
                switch action {
                case .useful, .open: drag = CGSize(width: width, height: drag.height * 0.5)
                case .dismiss: drag = CGSize(width: -width, height: drag.height * 0.5)
                case .acknowledge, .snooze: drag = CGSize(width: drag.width * 0.4, height: -450)
                }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.flyDuration) {
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) {
                triaged[card.id] = card.isForYou
                drag = .zero
                fading = false
                flying = nil
            }
            onAction(card, action) { accepted in
                // A rolled-back action puts the card back on the stack.
                if !accepted { triaged[card.id] = nil }
            }
        }
    }

    // MARK: Controls

    private func controls(_ card: TodayDeckCard) -> some View {
        HStack(spacing: 8) {
            deckButton("Dismiss", systemImage: "xmark", tint: theme.dangerColor, id: "dismiss") { commit(card, .dismiss) }
            deckButton("Seen", systemImage: "eye", tint: theme.infoColor, id: "seen") { commit(card, .acknowledge) }
            deckButton("Useful", systemImage: "checkmark", tint: theme.successColor, id: "useful") { commit(card, .useful) }
            Menu {
                TodayActMenuItems(entries: card.actEntries, agentName: agentName,
                                  identifierPrefix: "today-deck-act") { act(card, $0) }
            } label: {
                Label("Act", systemImage: "bolt.fill")
                    .font(.themed(12, weight: .bold))
                    .lineLimit(1)
                    .frame(maxWidth: .infinity).frame(height: 40)
                    .foregroundColor(theme.onAccentColor)
                    .background(theme.accentFillColor)
                    .clipShape(RoundedRectangle(cornerRadius: 9))
            }
            .menuOrder(.fixed)
            .buttonStyle(.plain)
            .accessibilityLabel("Act")
            .accessibilityHint("Shows this card's actions.")
            .accessibilityIdentifier("today-deck-act")
        }
        .disabled(flying != nil)
    }

    private var agentName: String { primaryAgent.preferredName ?? MagicianAccess.productName }

    /// Resolving entries fly the card out through `commit` (rollback on
    /// failure); Details and the rest keep the card on top of the stack.
    private func act(_ card: TodayDeckCard, _ entry: TodayDeckActEntry) {
        guard flying == nil else { return }
        if let action = entry.resolvingAction {
            commit(card, action)
        } else if entry == .details {
            onOpen(card)
        } else {
            onAct(card, entry)
        }
    }

    private func deckButton(_ title: String, systemImage: String, tint: Color, id: String,
                            action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.themed(12, weight: .semibold))
                .lineLimit(1)
                .frame(maxWidth: .infinity).frame(height: 40)
                .foregroundColor(tint)
                .background(tint.opacity(0.08))
                .clipShape(RoundedRectangle(cornerRadius: 9))
                .overlay(RoundedRectangle(cornerRadius: 9).stroke(tint.opacity(0.3)))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("today-deck-\(id)")
    }

    // MARK: Empty / complete

    private var emptyState: some View {
        let cleared = triagedCount(in: tab)
        return VStack(spacing: 10) {
            Text("☕").font(.system(size: 34))
            Text(cleared > 0 ? "All Dispatches Cleared" : "No Dispatches in This Stack")
                .font(TodayNewsprint.serif(22, weight: .bold))
                .foregroundColor(theme.textColor)
                .multilineTextAlignment(.center)
            Text(cleared > 0
                 ? "You've triaged all \(cleared) items in this stack."
                 : "There are currently no \(tab == .all ? "" : "\(tab.title) ")cards in today's deck.")
                .font(.themed(13))
                .foregroundColor(theme.secondaryTextColor)
                .multilineTextAlignment(.center)
            HStack(spacing: 8) {
                Button(action: onOpenBroadsheet) {
                    Text("📰 Open Broadsheet View").font(.themed(12, weight: .semibold))
                        .padding(.horizontal, 12).frame(height: 34)
                        .foregroundColor(theme.onAccentColor).background(theme.accentFillColor)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("today-deck-open-broadsheet")
                if cleared > 0 {
                    Button { triaged.removeAll() } label: {
                        Text("↻ Review Again").font(.themed(12, weight: .semibold))
                            .padding(.horizontal, 12).frame(height: 34)
                            .foregroundColor(theme.accentColor)
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.accentColor.opacity(0.4)))
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("today-deck-review-again")
                }
            }
            .padding(.top, 4)
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(theme.cardBorderColor, style: StrokeStyle(lineWidth: 1, dash: [4, 4])))
    }

    private func categoryColor(_ card: TodayDeckCard) -> Color {
        card.isForYou ? theme.accentColor : theme.discoveryColor
    }
}

/// The ⚡ Act menu's items, shared by the deck and the broadsheet rows so
/// both list the same entries with the same labels and identifiers
/// (`<prefix>-<entry id>`).
struct TodayActMenuItems: View {
    let entries: [TodayDeckActEntry]
    let agentName: String
    let identifierPrefix: String
    let onSelect: (TodayDeckActEntry) -> Void

    var body: some View {
        ForEach(entries) { entry in
            if entry == .details { Divider() }
            Button { onSelect(entry) } label: {
                Label(entry.label(agentName: agentName), systemImage: entry.systemImage)
            }
            .accessibilityIdentifier("\(identifierPrefix)-\(entry.id)")
        }
    }
}
