import SwiftUI

// Shared chrome for the Observe deck: the command header, the KPI cards, and
// the card / section / banner primitives every deck view uses. Theme tokens
// only, so every theme (light and dark) renders it.

/// The standard deck card (card fill, soft border, 14pt corners).
struct ObserveCard<Content: View>: View {
    @ObservedObject private var theme = ThemeManager.shared
    var spacing: CGFloat = 12
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) { content() }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(theme.cardColor)
            .clipShape(RoundedRectangle(cornerRadius: 14))
            .overlay {
                RoundedRectangle(cornerRadius: 14)
                    .stroke(theme.cardBorderColor, lineWidth: 1)
            }
    }
}

/// Small uppercase section label ("LIVE", "UPCOMING", …) with optional trailing content.
struct ObserveSectionHeader<Trailing: View>: View {
    @ObservedObject private var theme = ThemeManager.shared
    let title: String
    var color: Color?
    @ViewBuilder var trailing: () -> Trailing

    var body: some View {
        HStack(spacing: 8) {
            Text(title.uppercased())
                .font(.caption.weight(.bold))
                .kerning(0.6)
                .foregroundColor(color ?? theme.secondaryTextColor)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
            trailing()
        }
    }
}

extension ObserveSectionHeader where Trailing == EmptyView {
    init(_ title: String, color: Color? = nil) {
        self.init(title: title, color: color) { EmptyView() }
    }
}

/// Tinted inline banner for errors / warnings.
struct ObserveBanner: View {
    let text: String
    let color: Color
    var retry: (() -> Void)?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(text)
                .font(.subheadline)
                .foregroundColor(color)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let retry {
                Button("Retry", action: retry)
                    .font(.caption.weight(.semibold))
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(12)
        .background(color.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }
}

/// Command header: one compact row above the KPI grid — live status dot +
/// status line on the left, Refresh on the right. (The screen title is the
/// navigation bar's "Observe"; no in-screen kicker/title duplicates it.)
struct ObserveCommandHeader: View {
    @ObservedObject private var theme = ThemeManager.shared
    let statusLine: String
    let live: Bool
    let refreshing: Bool
    let onRefresh: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            HStack(spacing: 7) {
                Circle()
                    .fill(live ? theme.dangerColor : theme.secondaryTextColor.opacity(0.45))
                    .frame(width: 8, height: 8)
                Text(statusLine)
                    .font(.subheadline.weight(.medium))
                    .foregroundColor(live ? theme.textColor : theme.secondaryTextColor)
                    .lineLimit(1)
                    .minimumScaleFactor(0.85)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("observe-status-line")
            Spacer(minLength: 0)
            Button(action: onRefresh) {
                ZStack {
                    if refreshing {
                        ProgressView().scaleEffect(0.75)
                    } else {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 14, weight: .semibold))
                    }
                }
                .frame(width: 32, height: 32)
                .foregroundColor(theme.textColor)
                .background(theme.cardColor)
                .clipShape(Circle())
                .overlay(Circle().stroke(theme.cardBorderColor, lineWidth: 1))
            }
            .buttonStyle(.plain)
            .disabled(refreshing)
            .accessibilityLabel(refreshing ? "Refreshing observation" : "Refresh observation")
            .accessibilityIdentifier("observe-refresh")
        }
    }
}

/// One KPI card. Tappable; the selected one carries the accent border + ring,
/// a light accent tint and a 3pt top accent bar.
struct ObserveKPICard: View {
    @ObservedObject private var theme = ThemeManager.shared
    let pane: ObservePane
    let metrics: ObserveDeckMetrics
    let selected: Bool
    let onTap: () -> Void

    private var tint: Color {
        switch pane {
        case .now: return metrics.anyActive ? theme.dangerColor : theme.successColor
        case .sources: return theme.infoColor
        case .audio: return theme.discoveryColor
        case .notes: return theme.warningColor
        }
    }

    var body: some View {
        Button(action: onTap) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    Image(systemName: pane.kpiIcon)
                        .font(.system(size: 10, weight: .bold))
                        .foregroundColor(tint)
                        .frame(width: 22, height: 22)
                        .background(tint.opacity(0.14))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                    Text(pane.kpiTitle.uppercased())
                        .font(.system(size: 10, weight: .bold))
                        .kerning(0.4)
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Spacer(minLength: 0)
                    if pane == .now && metrics.anyActive {
                        Text("LIVE")
                            .font(.system(size: 9, weight: .heavy))
                            .kerning(0.5)
                            .foregroundColor(.white)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(theme.dangerColor)
                            .clipShape(Capsule())
                    }
                }
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(metrics.metricText(pane))
                        .font(.themedMono(24, weight: .semibold))
                        .foregroundColor(theme.textColor)
                        .monospacedDigit()
                        .lineLimit(1)
                    Text(metrics.subText(pane))
                        .font(.caption2)
                        .foregroundColor(theme.secondaryTextColor)
                        .lineLimit(2)
                        .minimumScaleFactor(0.85)
                }
                Text(pane.kpiFooter)
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundColor(selected ? theme.accentColor : theme.secondaryTextColor.opacity(0.8))
                    .lineLimit(1)
            }
            .padding(.horizontal, 10)
            .padding(.top, 11)
            .padding(.bottom, 9)
            .frame(maxWidth: .infinity, minHeight: 96, alignment: .topLeading)
            .background(
                ZStack {
                    theme.cardColor
                    if selected { theme.accentColor.opacity(0.07) }
                }
            )
            .overlay(alignment: .top) {
                if selected {
                    Rectangle().fill(theme.accentColor).frame(height: 3)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .overlay {
                RoundedRectangle(cornerRadius: 12)
                    .stroke(selected ? theme.accentColor : theme.cardBorderColor, lineWidth: selected ? 1 : 1)
            }
            .overlay {
                if selected {
                    // The 1pt outer ring around the accent border.
                    RoundedRectangle(cornerRadius: 13)
                        .stroke(theme.accentColor.opacity(0.35), lineWidth: 1)
                        .padding(-2)
                }
            }
            .contentShape(RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(metrics.accessibilityLabel(pane, selected: selected))
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityIdentifier("observe-kpi-\(pane.rawValue)")
    }
}

/// The 2×2 KPI grid (the only view switcher).
struct ObserveKPIGrid: View {
    let metrics: ObserveDeckMetrics
    let selected: ObservePane
    let onSelect: (ObservePane) -> Void

    var body: some View {
        LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8)], spacing: 8) {
            ForEach(ObservePane.allCases) { pane in
                ObserveKPICard(pane: pane, metrics: metrics, selected: pane == selected) { onSelect(pane) }
            }
        }
        .padding(.horizontal, 2)
    }
}

/// Relative "5 min ago" / "in 2 h" wording used by the deck rows.
enum ObserveTime {
    static func relative(_ date: Date?, now: Date = Date()) -> String {
        guard let date else { return "never" }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .abbreviated
        return f.localizedString(for: date, relativeTo: now)
    }

    static func stamp(_ date: Date?) -> String {
        guard let date else { return "" }
        let f = DateFormatter()
        f.setLocalizedDateFormatFromTemplate("MMM d HH:mm")
        return f.string(from: date)
    }
}
