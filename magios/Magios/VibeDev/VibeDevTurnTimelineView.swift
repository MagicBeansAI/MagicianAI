import SwiftUI

/// One turn's timeline: what the agent said, its reasoning (collapsed), one
/// line per tool step with its outcome, errors, and the end of the turn.
struct VibeDevTurnTimelineView: View {
    let timeline: VibeDevTimeline
    @ObservedObject private var theme = ThemeManager.shared
    @State private var openReasoning: Set<Int> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            if timeline.truncated {
                Text("Earlier steps are not shown.")
                    .font(.themed(11))
                    .foregroundStyle(theme.secondaryTextColor)
            }
            if timeline.cards.isEmpty {
                Text("Nothing recorded for this turn yet.")
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
            }
            ForEach(timeline.cards) { card in
                cardRow(card)
            }
        }
    }

    @ViewBuilder
    private func cardRow(_ card: VibeDevTimelineCard) -> some View {
        switch card.kind {
        case .message:
            line(icon: "●", iconColor: theme.accentColor) {
                Text(card.text ?? card.title)
                    .font(.themed(12.5))
                    .foregroundStyle(theme.textColor)
                    .textSelection(.enabled)
            }
        case .reasoning:
            let open = openReasoning.contains(card.index)
            Button {
                if open { openReasoning.remove(card.index) } else { openReasoning.insert(card.index) }
            } label: {
                line(icon: "…", iconColor: theme.secondaryTextColor) {
                    Text(open ? (card.text ?? card.title) : card.title)
                        .font(.themed(12.5).italic())
                        .foregroundStyle(theme.secondaryTextColor)
                        .lineLimit(open ? nil : 1)
                        .multilineTextAlignment(.leading)
                }
                .frame(minHeight: Lively.tapMin)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Reasoning")
            .accessibilityValue(open ? (card.text ?? card.title) : card.title)
            .accessibilityHint(open ? "Collapses" : "Expands")
        case .action:
            let (icon, color) = actionMark(card.status)
            line(icon: icon, iconColor: color) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.title)
                        .font(.themedMono(11.5))
                        .foregroundStyle(theme.secondaryTextColor)
                        .vibeDevLineLimit(2)
                    if card.status == "error", let text = card.text, !text.isEmpty {
                        Text(text)
                            .font(.themedMono(11))
                            .foregroundStyle(theme.dangerColor)
                            .vibeDevLineLimit(3)
                    }
                }
            }
            .accessibilityLabel("\(card.title), \(card.status ?? "")")
        case .error:
            line(icon: "!", iconColor: theme.dangerColor) {
                Text(card.text ?? card.title)
                    .font(.themed(12.5))
                    .foregroundStyle(theme.dangerColor)
            }
        case .completed:
            line(icon: "✓", iconColor: theme.successColor) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.title)
                        .font(.themed(12.5, weight: .semibold))
                        .foregroundStyle(theme.textColor)
                    if let text = card.text, !text.isEmpty, text != card.title {
                        Text(text)
                            .font(.themed(12.5))
                            .foregroundStyle(theme.textColor)
                            .textSelection(.enabled)
                    }
                }
            }
        case .notice:
            line(icon: "i", iconColor: theme.secondaryTextColor) {
                Text(card.text ?? card.title)
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
            }
        }
    }

    private func actionMark(_ status: String?) -> (String, Color) {
        switch status {
        case "ok": return ("✓", theme.successColor)
        case "error": return ("✗", theme.dangerColor)
        default: return ("▸", theme.statusRunningColor)
        }
    }

    private func line<Content: View>(icon: String, iconColor: Color, @ViewBuilder content: () -> Content) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(icon)
                .font(.themedMono(11, weight: .semibold))
                .foregroundStyle(iconColor)
                .frame(width: 14, alignment: .center)
                .accessibilityHidden(true)
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
