import SwiftUI
import UIKit

// Morning Edition building blocks: the newsprint type ramp, masthead,
// Realtime Wire, Economics of Operations ledger and the section banners.
// `TodayView` composes them; the deck lives in `TodayMorningBriefDeck.swift`.

/// Newsreader, the Morning Edition serif. The bundled `Newsreader.ttf` is a
/// variable font (wght 200–800, opsz 6–72) whose named instances do not map
/// onto `Font.weight`, so weight and optical size are pinned through the
/// variation axes directly. Falls back to the system serif if the font is
/// ever missing from the bundle.
enum TodayNewsprint {
    private static let weightAxis = NSNumber(value: 0x7767_6874) // 'wght'
    private static let opticalSizeAxis = NSNumber(value: 0x6F70_737A) // 'opsz'
    private static var cache: [String: UIFont] = [:]

    /// `italic` slants the roman: the bundle ships no Newsreader italic, and
    /// SwiftUI's `.italic()` cannot synthesize one for a variable UIFont.
    static func serif(_ size: CGFloat, weight: Font.Weight = .regular, italic: Bool = false) -> Font {
        Font(uiSerif(size, weight: weight, italic: italic))
    }

    static func uiSerif(_ size: CGFloat, weight: Font.Weight = .regular, italic: Bool = false) -> UIFont {
        let axisWeight = axisValue(for: weight)
        let key = "\(size):\(axisWeight):\(italic)"
        if let cached = cache[key] { return cached }
        let variation: [NSNumber: NSNumber] = [
            weightAxis: NSNumber(value: axisWeight),
            opticalSizeAxis: NSNumber(value: Double(min(72, max(6, size))))
        ]
        var attributes: [UIFontDescriptor.AttributeName: Any] = [
            .family: "Newsreader",
            UIFontDescriptor.AttributeName(rawValue: kCTFontVariationAttribute as String): variation
        ]
        if italic {
            attributes[.matrix] = CGAffineTransform(a: 1, b: 0, c: 0.2, d: 1, tx: 0, ty: 0)
        }
        let descriptor = UIFontDescriptor(fontAttributes: attributes)
        var font = UIFont(descriptor: descriptor, size: size)
        if font.familyName != "Newsreader" {
            let system = UIFont.systemFont(ofSize: size, weight: uiWeight(for: weight))
            var fallback = system.fontDescriptor.withDesign(.serif) ?? system.fontDescriptor
            if italic { fallback = fallback.withSymbolicTraits(.traitItalic) ?? fallback }
            font = UIFont(descriptor: fallback, size: size)
        }
        cache[key] = font
        return font
    }

    private static func axisValue(for weight: Font.Weight) -> Double {
        switch weight {
        case .ultraLight, .thin: return 200
        case .light: return 300
        case .medium: return 500
        case .semibold: return 600
        case .bold: return 700
        case .heavy, .black: return 800
        default: return 400
        }
    }

    private static func uiWeight(for weight: Font.Weight) -> UIFont.Weight {
        switch weight {
        case .ultraLight, .thin: return .thin
        case .light: return .light
        case .medium: return .medium
        case .semibold: return .semibold
        case .bold: return .bold
        case .heavy, .black: return .heavy
        default: return .regular
        }
    }
}

/// A 1pt rule in the theme's hairline colour.
struct TodayHairline: View {
    @ObservedObject private var theme = ThemeManager.shared
    var body: some View {
        Rectangle().fill(theme.cardBorderColor).frame(height: 1)
    }
}

/// Mono caps kicker / dateline.
struct TodayKicker: View {
    let text: String
    var color: Color?
    var size: CGFloat = 10
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(text.uppercased())
            .font(.themedMono(size, weight: .semibold))
            .tracking(size * 0.12)
            .foregroundColor(color ?? theme.secondaryTextColor)
            .lineLimit(1)
    }
}

/// `§ 3  Special Reports & Briefings` — serif section banner with an italic
/// deck line and an optional trailing control.
struct TodaySectionBanner<Trailing: View>: View {
    let marker: String?
    let title: String
    let subtitle: String
    @ViewBuilder var trailing: Trailing
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                if let marker {
                    Text(marker)
                        .font(TodayNewsprint.serif(22, weight: .bold))
                        .foregroundColor(theme.accentColor)
                }
                // One line, shrinking to fit beside the marker and trailing
                // control rather than wrapping.
                Text(title)
                    .font(TodayNewsprint.serif(22, weight: .bold))
                    .foregroundColor(theme.textColor)
                    .lineLimit(1)
                    .minimumScaleFactor(0.65)
                    .layoutPriority(1)
                Spacer(minLength: 6)
                trailing
            }
            Text(subtitle)
                .font(TodayNewsprint.serif(14, italic: true))
                .foregroundColor(theme.secondaryTextColor)
                .fixedSize(horizontal: false, vertical: true)
            TodayHairline()
        }
    }
}

extension TodaySectionBanner where Trailing == EmptyView {
    init(marker: String?, title: String, subtitle: String) {
        self.init(marker: marker, title: title, subtitle: subtitle) { EmptyView() }
    }
}

// MARK: - Masthead

/// Compact masthead: the heavy serif `Today's`, one row with the date
/// (left) and `VOL. · NO.` (right), then a double rule. The greeting lives in
/// the navigation title.
struct TodayMasthead: View {
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        // Re-evaluated every minute so a page left open across midnight
        // turns over its date and issue number.
        TimelineView(.everyMinute) { context in
            content(now: context.date)
        }
    }

    private func content(now: Date) -> some View {
        let volume = TodayMorningEdition.volumeLine(for: now)
        let date = TodayMorningEdition.mastheadDate(now).uppercased()
        return VStack(spacing: 8) {
            Text("Today's")
                .font(TodayNewsprint.serif(44, weight: .heavy))
                .tracking(-1.2)
                .foregroundColor(theme.textColor)
                .frame(maxWidth: .infinity)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("today-masthead-title")
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) {
                    metaText(date)
                    Spacer(minLength: 8)
                    metaText(volume)
                }
                // Never truncate the date: on a very narrow width the issue
                // line drops below it.
                VStack(alignment: .leading, spacing: 3) {
                    metaText(date)
                    metaText(volume)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("today-masthead-meta")
            VStack(spacing: 3) {
                Rectangle().fill(theme.textColor).frame(height: 1)
                Rectangle().fill(theme.textColor).frame(height: 1)
            }
        }
        .padding(.top, 6)
    }

    private func metaText(_ text: String) -> some View {
        Text(text)
            .font(TodayNewsprint.serif(11, weight: .medium))
            .tracking(11 * 0.14)
            .foregroundColor(theme.secondaryTextColor)
            .fixedSize()
    }
}

// MARK: - Realtime Wire

struct TodayRealtimeWireView: View {
    let items: [TodayWireItem]
    let count24h: Int
    @Binding var expanded: Bool
    let onOpen: (TodayWireItem) -> Void
    let onActivity: () -> Void

    @State private var filter = TodayWireFilter.all
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private static let tickerInterval: TimeInterval = 4.5
    private static let drawerLimit = 5

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(.easeInOut(duration: 0.2)) { expanded.toggle() }
            } label: {
                collapsedBar
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("today-wire-toggle")

            if expanded {
                TodayHairline()
                drawer
                    .padding(12)
                    .transition(.opacity)
            }
        }
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.cardBorderColor))
    }

    private var collapsedBar: some View {
        HStack(spacing: 10) {
            TodayLiveDot(color: theme.successColor, animated: !reduceMotion)
            ticker
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(TodayMorningEdition.compact24hCount(count24h))
                .font(.themedMono(10, weight: .bold))
                .foregroundColor(theme.secondaryTextColor)
                .padding(.horizontal, 6).padding(.vertical, 3)
                .background(theme.secondaryTextColor.opacity(0.1))
                .clipShape(Capsule())
                .fixedSize()
                .accessibilityIdentifier("today-wire-count")
            Text(expanded ? "Hide" : "Latest 5")
                .font(.themed(11, weight: .semibold))
                .foregroundColor(theme.accentColor)
                .fixedSize()
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 42)
        .contentShape(Rectangle())
    }

    @ViewBuilder private var ticker: some View {
        if items.isEmpty {
            Text("Awaiting incoming transmissions…")
                .font(.themed(12).italic())
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(1)
        } else {
            TimelineView(.periodic(from: .now, by: Self.tickerInterval)) { context in
                let pool = Array(items.prefix(Self.drawerLimit))
                let index = expanded ? 0
                    : Int(context.date.timeIntervalSinceReferenceDate / Self.tickerInterval) % pool.count
                let item = pool[index]
                tickerLine(item)
                    .id(item.id)
                    .transition(.opacity)
                    .animation(.easeInOut(duration: 0.35), value: item.id)
            }
        }
    }

    private func tickerLine(_ item: TodayWireItem) -> some View {
        (Text(item.kind.tag).font(.themedMono(9, weight: .bold)).foregroundColor(kindColor(item.kind))
            + Text("  ")
            + Text(item.title).font(.themed(12, weight: .bold)).foregroundColor(theme.textColor)
            + Text(" · \(item.summary)").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            + Text(" (\(TodayMorningEdition.wireTimeAgo(item.timestamp)))").font(.themed(11)).foregroundColor(theme.secondaryTextColor))
            .lineLimit(1)
            .truncationMode(.tail)
    }

    private var drawer: some View {
        VStack(alignment: .leading, spacing: 10) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(TodayWireFilter.allCases) { option in
                        let count = items.filter(option.matches).count
                        Button { filter = option } label: {
                            Text("\(option.title) \(count)")
                                .font(.themed(11, weight: .semibold))
                                .padding(.horizontal, 9).frame(height: 26)
                                .foregroundColor(filter == option ? theme.onAccentColor : theme.textColor)
                                .background(filter == option ? theme.accentColor : theme.cardColor)
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                                .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.cardBorderColor.opacity(filter == option ? 0 : 1)))
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(filter == option ? .isSelected : [])
                        .accessibilityIdentifier("today-wire-filter-\(option.rawValue)")
                    }
                }
            }
            let visible = Array(items.filter(filter.matches).prefix(Self.drawerLimit))
            if visible.isEmpty {
                Text("Awaiting incoming transmissions…")
                    .font(.themed(12).italic())
                    .foregroundColor(theme.secondaryTextColor)
                    .padding(.vertical, 6)
            } else {
                ForEach(visible) { item in
                    // Rows without a task/thread target stay readable (no
                    // disabled dimming); tapping them is a no-op.
                    Button { onOpen(item) } label: { drawerRow(item) }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(item.taskID == nil && item.threadID == nil ? [] : .isLink)
                    if item.id != visible.last?.id { TodayHairline().opacity(0.6) }
                }
            }
            Button(action: onActivity) {
                HStack(spacing: 6) {
                    Image(systemName: "clock.arrow.circlepath").font(.system(size: 12, weight: .semibold))
                    Text("Activity").font(.themed(12, weight: .semibold))
                    Spacer()
                    Image(systemName: "chevron.right").font(.caption2)
                }
                .foregroundColor(theme.accentColor)
                .padding(.horizontal, 10).frame(height: 34)
                .background(theme.accentColor.opacity(0.08))
                .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("today-wire-activity")
        }
    }

    private func drawerRow(_ item: TodayWireItem) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(item.kind.tag)
                    .font(.themedMono(9, weight: .bold))
                    .foregroundColor(kindColor(item.kind))
                Text(item.badge.uppercased())
                    .font(.themedMono(9))
                    .foregroundColor(theme.secondaryTextColor)
                    .lineLimit(1)
                if item.severity == .error {
                    Image(systemName: "exclamationmark.circle.fill").font(.system(size: 10)).foregroundColor(theme.dangerColor)
                } else if item.severity == .success {
                    Image(systemName: "checkmark.circle.fill").font(.system(size: 10)).foregroundColor(theme.successColor)
                }
                Spacer(minLength: 4)
                Text(TodayMorningEdition.wireTimeAgo(item.timestamp))
                    .font(.themed(10))
                    .foregroundColor(theme.secondaryTextColor)
            }
            Text(item.title)
                .font(.themed(13, weight: .semibold))
                .foregroundColor(theme.textColor)
                .lineLimit(2)
            Text(item.summary)
                .font(.themed(12))
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private func kindColor(_ kind: TodayWireItem.Kind) -> Color {
        switch kind {
        case .event: return theme.infoColor
        case .insight: return theme.discoveryColor
        case .activity: return theme.accentColor
        }
    }
}

/// The wire's pulsing live indicator. The pulse is an SF Symbol effect,
/// rendered by Core Animation: a SwiftUI `repeatForever` state animation
/// re-lays-out the whole lazy Today stack every frame (and spins when UI
/// tests disable UIKit animations). Static under Reduce Motion / UI tests.
struct TodayLiveDot: View {
    let color: Color
    let animated: Bool
    private static let isUITest = ProcessInfo.processInfo.arguments.contains("--ui-test")

    var body: some View {
        Image(systemName: "circle.fill")
            .font(.system(size: 8))
            .foregroundColor(color)
            .symbolEffect(.pulse, options: .repeating, isActive: animated && !Self.isUITest)
            .frame(width: 14, height: 14)
            .accessibilityHidden(true)
    }
}

// MARK: - Operations carousel

/// The Operations carousel card (spec: operations-carousel-spec.md). A
/// generic list of slides — each a title plus collapsed and expanded content —
/// under one header whose title cross-fades per slide, one shared collapse
/// chevron (collapsed by default), fixed collapsed/expanded heights so the
/// page never jumps, tappable dots, and an 8 s auto-advance that pauses 15 s
/// after a manual swipe or dot tap (none under Reduce Motion).
///
/// Slide 1 "Economics of Operations": spend. Slide 2 "State of Operations":
/// task buckets, pie, agents and the one-line recent task list.
struct TodayOperationsCarousel: View {
    let pulse: TodayPulse?
    let errorMessage: String?
    let onRetry: () -> Void
    let onTasks: () -> Void
    let onOpenTask: (String) -> Void

    @State private var expanded = false
    @State private var index = 0
    @State private var selectedHour: Int?
    @State private var lastAdvance = Date()
    @State private var pausedUntil: Date?
    @State private var autoAdvancing = false
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static let collapsedHeight: CGFloat = 62
    static let expandedHeight: CGFloat = 206
    /// One-line list rows; lists show 3.5 so the cut-off row signals scroll.
    static let listRowHeight: CGFloat = 27
    static let listHeight: CGFloat = listRowHeight * 3.5
    private static let ticker = Timer.publish(every: 1, on: .main, in: .common).autoconnect()
    private static let isUITest = ProcessInfo.processInfo.arguments.contains("--ui-test")

    private struct Slide {
        let id: String
        let title: String
        let collapsed: AnyView
        let expanded: AnyView
    }

    private func slides(_ pulse: TodayPulse) -> [Slide] {
        [
            Slide(id: "economics", title: "Economics of Operations",
                  collapsed: AnyView(compactSpend(pulse)), expanded: AnyView(fullSpend(pulse))),
            Slide(id: "state", title: "State of Operations",
                  collapsed: AnyView(compactState(pulse)), expanded: AnyView(fullState(pulse))),
            Slide(id: "crew", title: "State of the Crew",
                  collapsed: AnyView(compactCrew(pulse)), expanded: AnyView(fullCrew(pulse)))
        ]
    }

    private var titles: [String] { ["Economics of Operations", "State of Operations", "State of the Crew"] }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if let pulse {
                let deck = slides(pulse)
                TabView(selection: $index) {
                    ForEach(Array(deck.enumerated()), id: \.element.id) { offset, slide in
                        Group { expanded ? slide.expanded : slide.collapsed }
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                            .tag(offset)
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
                .frame(height: expanded ? Self.expandedHeight : Self.collapsedHeight)
                .accessibilityElement(children: .contain)
                .accessibilityLabel(TodayCarouselAutoAdvance.dotLabel(index: index, count: deck.count, title: deck[index].title))
                .accessibilityIdentifier("today-operations-carousel")
                dots(deck)
            } else if errorMessage == nil {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Tallying the ledger…").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
                }
                .padding(.vertical, 8)
            }
            if let errorMessage {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundColor(theme.warningColor)
                    Text(errorMessage).font(.themed(11)).foregroundColor(theme.secondaryTextColor).lineLimit(3)
                    Spacer()
                    Button("Retry", action: onRetry).font(.themed(11, weight: .semibold)).foregroundColor(theme.accentColor)
                }
                .padding(9).background(theme.warningColor.opacity(0.10)).cornerRadius(9)
            }
        }
        .padding(14)
        .background(theme.cardColor)
        .overlay(RoundedRectangle(cornerRadius: 2).stroke(theme.cardBorderColor))
        .overlay(alignment: .top) { Rectangle().fill(theme.textColor).frame(height: 2) }
        .overlay(alignment: .bottom) { Rectangle().fill(theme.textColor).frame(height: 2) }
        .clipped()
        .onChange(of: index) { _, _ in
            // A change we did not make ourselves is a swipe or dot tap.
            if autoAdvancing { autoAdvancing = false } else { pausedUntil = TodayCarouselAutoAdvance.pauseUntil(afterInteractionAt: Date()) }
            selectedHour = nil
        }
        .onReceive(Self.ticker) { now in
            guard !Self.isUITest, pulse != nil,
                  TodayCarouselAutoAdvance.shouldAdvance(now: now, lastAdvance: lastAdvance, pausedUntil: pausedUntil,
                                                         reduceMotion: reduceMotion, slideCount: titles.count) else { return }
            lastAdvance = now
            autoAdvancing = true
            withAnimation(.easeInOut(duration: 0.35)) {
                index = TodayCarouselAutoAdvance.nextIndex(after: index, count: titles.count)
            }
        }
    }

    private var header: some View {
        Button {
            withAnimation(reduceMotion ? .easeInOut(duration: 0.2) : .snappy(duration: 0.28)) { expanded.toggle() }
        } label: {
            VStack(alignment: .leading, spacing: 5) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    ZStack(alignment: .leading) {
                        Text(titles[min(index, titles.count - 1)])
                            .font(TodayNewsprint.serif(22, weight: .bold))
                            .foregroundColor(theme.textColor)
                            .lineLimit(1)
                            .minimumScaleFactor(0.65)
                            .id(index)
                            .transition(.opacity)
                    }
                    .animation(.easeInOut(duration: 0.25), value: index)
                    Spacer(minLength: 6)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundColor(theme.accentColor)
                        .rotationEffect(.degrees(expanded ? 180 : 0))
                }
                TodayHairline()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityValue(expanded ? "Expanded" : "Collapsed")
        .accessibilityIdentifier("today-ledger-toggle")
    }

    private func dots(_ deck: [Slide]) -> some View {
        HStack(spacing: 6) {
            ForEach(Array(deck.enumerated()), id: \.element.id) { offset, slide in
                Button {
                    pausedUntil = TodayCarouselAutoAdvance.pauseUntil(afterInteractionAt: Date())
                    withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.3)) { index = offset }
                } label: {
                    Capsule()
                        .fill(offset == index ? theme.accentColor : theme.cardBorderColor)
                        .frame(width: offset == index ? 16 : 6, height: 6)
                        .padding(.vertical, 6)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(TodayCarouselAutoAdvance.dotLabel(index: offset, count: deck.count, title: slide.title))
                .accessibilityAddTraits(offset == index ? .isSelected : [])
                .accessibilityIdentifier("today-operations-dot-\(offset)")
            }
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Slide 1 — Economics of Operations

    private func compactSpend(_ pulse: TodayPulse) -> some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 0) {
                spendFigure(pulse, size: 34)
                Text("\(pulse.callsToday) \(pulse.callsToday == 1 ? "call" : "calls")")
                    .font(.themed(11))
                    .foregroundColor(theme.secondaryTextColor)
            }
            .fixedSize()
            Rectangle().fill(theme.cardBorderColor).frame(width: 1, height: 50)
            spendChart(pulse, plotHeight: 38)
        }
    }

    private func fullSpend(_ pulse: TodayPulse) -> some View {
        let tone = TodayMorningEdition.spendTone(today: pulse.spendToday, yesterday: pulse.spendYesterday)
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Circle().fill(theme.accentColor).frame(width: 6, height: 6)
                TodayKicker(text: providerKicker(pulse), color: theme.accentColor, size: 9)
            }
            HStack(alignment: .center, spacing: 12) {
                spendFigure(pulse, size: 38).fixedSize()
                Rectangle().fill(theme.cardBorderColor).frame(width: 1, height: 44)
                VStack(alignment: .leading, spacing: 4) {
                    Text(TodayMorningEdition.spendDeltaLine(today: pulse.spendToday, yesterday: pulse.spendYesterday))
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(toneColor(tone))
                    Text("\(pulse.callsToday) model calls today")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                }
            }
            spendChart(pulse, plotHeight: 30)
            HStack(spacing: 0) {
                statCell("Avg / call", TodayMorningEdition.averagePerCall(spend: pulse.spendToday, calls: pulse.callsToday))
                statCell("Peak hour", TodayMorningEdition.peakHour(pulse.hourlySpend))
                statCell("Coding runs", "\(pulse.codingRunsToday)")
                if pulse.evalCasesToday > 0 {
                    statCell("Evals", "\(pulse.evalPassesToday)/\(pulse.evalCasesToday)")
                } else {
                    statCell("Memories", "\(pulse.memoriesToday)")
                }
            }
            .accessibilityIdentifier("today-ledger-stats")
        }
    }

    private func statCell(_ label: String, _ value: String) -> some View {
        VStack(spacing: 1) {
            TodayKicker(text: label, size: 8)
            Text(value).font(TodayNewsprint.serif(15, weight: .bold)).foregroundColor(theme.textColor)
                .lineLimit(1).minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }

    private func spendFigure(_ pulse: TodayPulse, size: CGFloat) -> some View {
        let parts = TodayMorningEdition.spendFigureParts(pulse.spendToday)
        return HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text("$").font(TodayNewsprint.serif(size * 0.52, weight: .semibold))
            Text(parts.dollars).font(TodayNewsprint.serif(size, weight: .bold))
            Text(parts.cents).font(TodayNewsprint.serif(size * 0.5, weight: .semibold))
        }
        .foregroundColor(theme.textColor)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("LLM spend today \(TodayMorningEdition.formatSpend(pulse.spendToday))")
        .accessibilityIdentifier("today-ledger-spend")
    }

    private func providerKicker(_ pulse: TodayPulse) -> String {
        guard let top = pulse.topModel else { return "COMMERCIAL MODEL EXPENDITURES" }
        return "TOP PROVIDER: \(top.provider) / \(top.model) (\(TodayMorningEdition.providerSharePercent(top.share))%)"
    }

    private func spendChart(_ pulse: TodayPulse, plotHeight: CGFloat) -> some View {
        let currentHour = Calendar.current.component(.hour, from: Date())
        let spend = pulse.hourlySpend.count == 24 ? pulse.hourlySpend : Array(repeating: 0, count: 24)
        let calls = pulse.hourlyCalls.count == 24 ? pulse.hourlyCalls : Array(repeating: 0, count: 24)
        let maximum = max(spend.max() ?? 0, 0.001)
        return VStack(spacing: 5) {
            GeometryReader { geo in
                let slot = geo.size.width / 24
                let barWidth = max(2, slot * 0.62)
                let height = geo.size.height
                let top = { (value: Double) -> CGFloat in
                    height - (value > 0 ? max(3, CGFloat(value / maximum) * (height - 4)) : 1.5)
                }
                ZStack(alignment: .bottomLeading) {
                    if pulse.spendToday > 0 {
                        Path { path in
                            path.move(to: CGPoint(x: slot / 2, y: height))
                            for hour in 0..<24 { path.addLine(to: CGPoint(x: slot * (CGFloat(hour) + 0.5), y: top(spend[hour]))) }
                            path.addLine(to: CGPoint(x: slot * 23.5, y: height))
                            path.closeSubpath()
                        }
                        .fill(LinearGradient(colors: [theme.accentColor.opacity(0.28), theme.accentColor.opacity(0)],
                                             startPoint: .top, endPoint: .bottom))
                        Path { path in
                            for hour in 0..<24 {
                                let point = CGPoint(x: slot * (CGFloat(hour) + 0.5), y: top(spend[hour]))
                                if hour == 0 { path.move(to: point) } else { path.addLine(to: point) }
                            }
                        }
                        .stroke(theme.accentColor.opacity(0.7), style: StrokeStyle(lineWidth: 1.2, lineCap: .round, lineJoin: .round))
                    }
                    Rectangle().fill(theme.cardBorderColor).frame(height: 1)
                    ForEach(0..<24, id: \.self) { hour in
                        RoundedRectangle(cornerRadius: 1)
                            .fill(barColor(hour: hour, spend: spend[hour], currentHour: currentHour))
                            .frame(width: barWidth, height: height - top(spend[hour]))
                            .offset(x: slot * CGFloat(hour) + (slot - barWidth) / 2)
                    }
                }
                .contentShape(Rectangle())
                // Tap (not drag) picks a bar: a horizontal drag here pages
                // the carousel.
                .onTapGesture(coordinateSpace: .local) { location in
                    let hour = min(23, max(0, Int(location.x / max(slot, 1))))
                    selectedHour = selectedHour == hour ? nil : hour
                }
            }
            .frame(height: plotHeight)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Hourly spend over 24 hours")
            HStack {
                if let hour = selectedHour {
                    Text(TodayMorningEdition.hourDetail(hour: hour, spend: spend[hour], calls: calls[hour]))
                        .font(.themedMono(10, weight: .semibold))
                        .foregroundColor(theme.textColor)
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Spacer(minLength: 0)
                } else {
                    axisLabel("12a")
                    Spacer()
                    axisLabel("12p")
                    Spacer()
                    Text("NOW").font(.themedMono(9, weight: .bold)).foregroundColor(theme.accentColor)
                }
            }
            .accessibilityIdentifier("today-ledger-axis")
        }
    }

    private func axisLabel(_ text: String) -> some View {
        Text(text).font(.themedMono(9)).foregroundColor(theme.secondaryTextColor)
    }

    private func barColor(hour: Int, spend: Double, currentHour: Int) -> Color {
        if hour == selectedHour { return theme.textColor }
        if hour == currentHour { return theme.accentColor }
        return spend > 0 ? theme.accentColor.opacity(0.55) : theme.secondaryTextColor.opacity(0.22)
    }

    // MARK: Slide 2 — State of Operations

    private func compactState(_ pulse: TodayPulse) -> some View {
        HStack(alignment: .center, spacing: 0) {
            stateStat("Active", pulse.tasksInFlight, theme.warningColor)
            stateStat("Succeeded", pulse.tasksSucceeded, theme.successColor)
            stateStat("Failed", pulse.tasksFailed, theme.dangerColor)
            TodayTaskPie(succeeded: pulse.tasksSucceeded, failed: pulse.tasksFailed, inFlight: pulse.tasksInFlight)
                .frame(width: 44, height: 44)
        }
        .frame(maxHeight: .infinity)
    }

    private func stateStat(_ label: String, _ value: Int, _ color: Color) -> some View {
        VStack(spacing: 2) {
            Text("\(value)").font(TodayNewsprint.serif(28, weight: .bold)).foregroundColor(color)
            TodayKicker(text: label, size: 9)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }

    private func fullState(_ pulse: TodayPulse) -> some View {
        let shares = pulse.taskShares
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 16) {
                TodayTaskPie(succeeded: pulse.tasksSucceeded, failed: pulse.tasksFailed, inFlight: pulse.tasksInFlight)
                    .frame(width: 56, height: 56)
                VStack(alignment: .leading, spacing: 2) {
                    legendRow("Active", count: pulse.tasksInFlight, percent: shares.inFlight, color: theme.warningColor)
                    legendRow("Succeeded", count: pulse.tasksSucceeded, percent: shares.succeeded, color: theme.successColor)
                    legendRow("Failed", count: pulse.tasksFailed, percent: shares.failed, color: theme.dangerColor)
                }
            }
            HStack {
                if let agents = pulse.agents {
                    Text("Agents · Enabled \(agents.enabled) · Active \(agents.active) · Total \(agents.total)")
                        .font(.themed(11)).foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1).minimumScaleFactor(0.8)
                        .accessibilityIdentifier("today-ledger-agents")
                }
                Spacer(minLength: 6)
                Button(action: onTasks) {
                    Text("Tasks →").font(.themed(12, weight: .semibold)).foregroundColor(theme.accentColor)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("today-ledger-tasks")
            }
            TodayHairline()
            if pulse.recentTasks.isEmpty {
                Text("No tasks yet today.")
                    .font(TodayNewsprint.serif(13, italic: true))
                    .foregroundColor(theme.secondaryTextColor)
            } else {
                ScrollView(.vertical, showsIndicators: true) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(pulse.recentTasks) { task in
                            Button { onOpenTask(task.id) } label: { taskRow(task) }
                                .buttonStyle(.plain)
                                .accessibilityIdentifier("today-operations-task-\(task.id)")
                        }
                    }
                }
                .frame(height: Self.listHeight)
            }
        }
    }

    private func taskRow(_ task: TodayRecentTask) -> some View {
        HStack(spacing: 8) {
            Circle().fill(statusColor(task.status)).frame(width: 7, height: 7)
            Text(task.title)
                .font(.themed(12))
                .foregroundColor(theme.textColor)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 6)
            Text(TodayMorningEdition.shortRelativeTime(task.updatedAt))
                .font(.themedMono(10))
                .foregroundColor(theme.secondaryTextColor)
        }
        .frame(height: Self.listRowHeight)
        .contentShape(Rectangle())
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "completed": return theme.successColor
        case "failed": return theme.dangerColor
        case "running", "paused", "planning": return theme.warningColor
        default: return theme.secondaryTextColor
        }
    }

    // MARK: Slide 3 — State of the Crew

    @ViewBuilder private func crewBody(_ pulse: TodayPulse, expanded: Bool) -> some View {
        if let crew = pulse.crew {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 0) {
                    crewStat("Active", "\(crew.activeAgents)/\(crew.totalAgents)", theme.successColor)
                    crewStat("Cost 24h", TodayMorningEdition.formatPerCall(crew.costUSD), theme.textColor)
                    crewStat("Tasks 24h", "\(crew.tasksDone)", theme.textColor)
                    crewStat("Reliability", crew.reliabilityPercent.map { "\($0)%" } ?? "\u{2014}",
                             crew.reliabilityPercent.map(percentColor) ?? theme.secondaryTextColor)
                }
                if expanded { crewTable(crew) }
                if let error = pulse.crewError { crewError(error) }
            }
        } else if let error = pulse.crewError {
            crewError(error)
        } else {
            ProgressView().controlSize(.small).frame(maxWidth: .infinity)
        }
    }

    private func compactCrew(_ pulse: TodayPulse) -> some View {
        crewBody(pulse, expanded: false).frame(maxHeight: .infinity)
    }

    private func fullCrew(_ pulse: TodayPulse) -> some View {
        crewBody(pulse, expanded: true)
    }

    private func crewStat(_ label: String, _ value: String, _ color: Color) -> some View {
        VStack(spacing: 1) {
            Text(value).font(TodayNewsprint.serif(20, weight: .bold)).foregroundColor(color)
                .lineLimit(1).minimumScaleFactor(0.6)
            TodayKicker(text: label, size: 8)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private func crewTable(_ crew: TodayCrewSummary) -> some View {
        TodayHairline()
        if crew.members.isEmpty {
            Text("The crew is resting — no activity in the last 24 hours.")
                .font(TodayNewsprint.serif(13, italic: true))
                .foregroundColor(theme.secondaryTextColor)
        } else {
            crewRow(name: TodayKicker(text: "Name", size: 8), cells: ["Cost", "Tasks", "Succ.", "Rel."].map {
                AnyView(TodayKicker(text: $0, size: 8))
            })
            ScrollView(.vertical, showsIndicators: true) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(crew.members) { member in
                        crewRow(
                            name: HStack(spacing: 6) {
                                Circle()
                                    .fill(member.active ? theme.successColor : member.disabled ? theme.cardBorderColor : theme.secondaryTextColor.opacity(0.5))
                                    .frame(width: 7, height: 7)
                                Text(member.name).font(.themed(12)).foregroundColor(theme.textColor)
                                    .lineLimit(1).truncationMode(.tail)
                            },
                            cells: [
                                AnyView(mono(TodayMorningEdition.formatPerCall(member.costUSD), theme.textColor)),
                                AnyView(mono("\(member.tasksDone)", theme.textColor)),
                                AnyView(mono(member.successPercent.map { "\($0)%" } ?? "\u{2014}",
                                             member.successPercent.map(percentColor) ?? theme.secondaryTextColor)),
                                AnyView(mono(member.reliabilityPercent.map { "\($0)%" } ?? "\u{2014}",
                                             member.reliabilityPercent.map(percentColor) ?? theme.secondaryTextColor))
                            ]
                        )
                        .frame(height: Self.listRowHeight)
                        .accessibilityElement(children: .combine)
                        .accessibilityIdentifier("today-crew-\(member.id)")
                    }
                }
            }
            .frame(height: Self.listHeight)
        }
    }

    private func crewRow<Name: View>(name: Name, cells: [AnyView]) -> some View {
        HStack(spacing: 6) {
            name.frame(maxWidth: .infinity, alignment: .leading)
            ForEach(cells.indices, id: \.self) { index in
                cells[index].frame(width: index == 0 ? 58 : 40, alignment: .trailing)
            }
        }
    }

    private func mono(_ text: String, _ color: Color) -> some View {
        Text(text).font(.themedMono(10, weight: .semibold)).foregroundColor(color).lineLimit(1).minimumScaleFactor(0.7)
    }

    private func crewError(_ message: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 11)).foregroundColor(theme.warningColor)
            Text(message).font(.themed(11)).foregroundColor(theme.secondaryTextColor).lineLimit(2)
            Spacer(minLength: 4)
            Button("Retry", action: onRetry).font(.themed(11, weight: .semibold)).foregroundColor(theme.accentColor)
        }
    }

    /// ≥ 95 % success colour, 80–95 warning, below 80 danger.
    private func percentColor(_ percent: Int) -> Color {
        switch TodayMorningEdition.percentTone(percent) {
        case .good: return theme.successColor
        case .neutral: return theme.warningColor
        case .bad: return theme.dangerColor
        }
    }

    private func legendRow(_ label: String, count: Int, percent: Int, color: Color) -> some View {
        HStack(spacing: 6) {
            Text("●").font(.system(size: 10)).foregroundColor(color)
            Text("\(label):").font(.themed(12)).foregroundColor(theme.secondaryTextColor)
            Text("\(count)").font(.themed(12, weight: .bold)).foregroundColor(theme.textColor)
            Text("(\(percent)%)").font(.themed(11)).foregroundColor(theme.secondaryTextColor)
        }
        .accessibilityElement(children: .combine)
    }

    private func toneColor(_ tone: TodayMorningEdition.Tone) -> Color {
        switch tone {
        case .good: return theme.successColor
        case .bad: return theme.dangerColor
        case .neutral: return theme.secondaryTextColor
        }
    }
}

/// Solid (not donut) pie of task outcomes; a dashed neutral disc reading
/// IDLE when nothing was attempted.
struct TodayTaskPie: View {
    let succeeded: Int
    let failed: Int
    let inFlight: Int
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        let total = succeeded + failed + inFlight
        ZStack {
            if total == 0 {
                Circle()
                    .fill(theme.secondaryTextColor.opacity(0.08))
                    .overlay(Circle().stroke(theme.cardBorderColor, style: StrokeStyle(lineWidth: 1.5, dash: [3, 3])))
                Text("IDLE").font(.themedMono(10, weight: .bold)).foregroundColor(theme.secondaryTextColor)
            } else {
                Canvas { context, size in
                    let radius = min(size.width, size.height) / 2 - 1
                    let center = CGPoint(x: size.width / 2, y: size.height / 2)
                    var start = Angle.degrees(-90)
                    let slices: [(Int, Color)] = [(succeeded, theme.successColor), (failed, theme.dangerColor),
                                                  (inFlight, theme.warningColor)]
                    for (count, color) in slices where count > 0 {
                        let end = start + .degrees(360 * Double(count) / Double(total))
                        var path = Path()
                        path.move(to: center)
                        path.addArc(center: center, radius: radius, startAngle: start, endAngle: end, clockwise: false)
                        path.closeSubpath()
                        context.fill(path, with: .color(color))
                        context.stroke(path, with: .color(theme.cardColor), lineWidth: 1.5)
                        start = end
                    }
                }
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(total == 0 ? "No tasks attempted" : "\(succeeded) succeeded, \(failed) failed, \(inFlight) in flight")
        .accessibilityIdentifier("today-ledger-pie")
    }
}
