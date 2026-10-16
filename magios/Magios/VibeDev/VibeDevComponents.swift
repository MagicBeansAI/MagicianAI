import SwiftUI

// Small pieces every VibeDev screen uses. Colors come only from
// `ThemeManager.shared` tokens and sizes from `Lively`, so every web theme
// renders the same here as on the web and on Android.

/// A run, lane or crew state as a dot: going breathes in the running color,
/// waiting on the owner pulses amber, done green, failed red.
struct VibeDevStateDot: View {
    let tone: StatusDot.Tone
    var size: CGFloat = 9
    /// What VoiceOver says for the dot.
    let label: String

    init(run state: VibeDevRunState, size: CGFloat = 9) {
        self.size = size
        label = state.label
        switch state {
        case .working: tone = .running
        case .needsYou: tone = .waiting
        case .paused: tone = .partial
        case .done: tone = .done
        case .failed: tone = .failed
        case .stopped: tone = .cancelled
        }
    }

    init(lane status: VibeDevLaneStatus, size: CGFloat = 8) {
        self.size = size
        label = status.label
        switch status {
        case .pending: tone = .waiting
        case .running: tone = .running
        case .done: tone = .done
        case .failed: tone = .failed
        case .stopped: tone = .cancelled
        }
    }

    init(crew status: VibeDevCrewStatus, size: CGFloat = 9) {
        self.size = size
        label = status.label
        switch status {
        case .running, .ready: tone = .running
        case .proposed, .choosing, .conflict: tone = .waiting
        case .integrated: tone = .done
        case .failed: tone = .failed
        case .cancelled: tone = .cancelled
        }
    }

    var body: some View {
        StatusDot(tone: tone, size: size)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(label)
    }
}

/// A line limit that lifts at accessibility text sizes, so large type wraps
/// instead of clipping.
struct VibeDevLineLimit: ViewModifier {
    let lines: Int
    @Environment(\.dynamicTypeSize) private var size

    func body(content: Content) -> some View {
        content.lineLimit(size.isAccessibilitySize ? nil : lines)
    }
}

extension View {
    func vibeDevLineLimit(_ lines: Int) -> some View { modifier(VibeDevLineLimit(lines: lines)) }

    /// Grow the tappable area to the 44 pt minimum without growing the look.
    func vibeDevHitTarget() -> some View {
        frame(minWidth: Lively.tapMin, minHeight: Lively.tapMin).contentShape(Rectangle())
    }
}

/// A themed card surface.
struct VibeDevCard: ViewModifier {
    var highlight: Color?
    @ObservedObject private var theme = ThemeManager.shared

    func body(content: Content) -> some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(theme.cardColor)
            .clipShape(RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: Lively.cardRadius, style: .continuous)
                    .strokeBorder(highlight.map { $0.opacity(0.55) } ?? theme.cardBorderColor, lineWidth: 1)
            )
    }
}

extension View {
    func vibeDevCard(highlight: Color? = nil) -> some View { modifier(VibeDevCard(highlight: highlight)) }
}

/// "NEEDS YOU ··· 2"
struct VibeDevSectionLabel: View {
    let title: String
    var count: Int?
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack {
            Text(title.uppercased())
            Spacer()
            if let count { Text("\(count)") }
        }
        .font(.themed(11, weight: .bold))
        .tracking(0.8)
        .foregroundStyle(theme.secondaryTextColor)
        .padding(.top, 6)
        .accessibilityAddTraits(.isHeader)
    }
}

/// Pill buttons: primary is the accent fill, danger an outline in danger.
struct VibeDevButtonStyle: ButtonStyle {
    var style: VibeDevOptionStyle = .plain
    var expand = false
    var large = false
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        let foreground: Color = {
            switch style {
            case .primary: return theme.onAccentColor
            case .danger: return theme.dangerColor
            case .plain: return theme.textColor
            }
        }()
        let border: Color = {
            switch style {
            case .primary: return theme.accentFillColor
            case .danger: return theme.dangerColor.opacity(0.45)
            case .plain: return theme.controlBorderColor
            }
        }()
        return configuration.label
            .font(.themed(large ? 15 : 13, weight: .semibold))
            .vibeDevLineLimit(1)
            .minimumScaleFactor(0.85)
            .foregroundStyle(foreground)
            .padding(.horizontal, 13)
            .padding(.vertical, large ? 12 : 8)
            .frame(maxWidth: expand ? .infinity : nil, minHeight: large ? 44 : 34)
            .background(
                Capsule().fill(style == .primary
                               ? (configuration.isPressed ? theme.accentFillPressedColor : theme.accentFillColor)
                               : theme.cardColor.opacity(configuration.isPressed ? 0.6 : 0))
            )
            .overlay(Capsule().strokeBorder(border, lineWidth: 1))
            .opacity(isEnabled ? 1 : 0.5)
            .frame(minHeight: Lively.tapMin)
            .contentShape(Rectangle())
    }
}

/// A row of selectable pills (`aria-pressed` in the mock).
struct VibeDevPillOption<Value: Hashable>: Identifiable {
    let value: Value
    let label: String
    var id: Value { value }
}

struct VibeDevPills<Value: Hashable>: View {
    let options: [VibeDevPillOption<Value>]
    @Binding var selection: Value
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VibeDevFlow(spacing: 6) {
            ForEach(options) { option in
                let selected = option.value == selection
                Button { selection = option.value } label: {
                    Text(option.label)
                        .font(.themed(12.5, weight: .semibold))
                        .foregroundStyle(selected ? theme.accentColor : theme.secondaryTextColor)
                        .padding(.horizontal, 11)
                        .padding(.vertical, 6)
                        .background(Capsule().fill(selected ? theme.accentColor.opacity(0.1) : theme.cardColor.opacity(0)))
                        .overlay(Capsule().strokeBorder(selected ? theme.accentColor : theme.controlBorderColor, lineWidth: 1))
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .vibeDevHitTarget()
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
    }
}

/// Wraps its children onto as many lines as they need.
struct VibeDevFlow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0 && x + size.width > width { x = 0; y += rowHeight + spacing; rowHeight = 0 }
            x += size.width + spacing
            widest = max(widest, x - spacing)
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: proposal.width ?? widest, height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX && x + size.width > bounds.maxX { x = bounds.minX; y += rowHeight + spacing; rowHeight = 0 }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

/// SANDBOXED / TRUSTED
struct VibeDevTrustBadge: View {
    let trusted: Bool
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(trusted ? "TRUSTED" : "SANDBOXED")
            .font(.themed(10, weight: .bold))
            .tracking(0.4)
            .foregroundStyle(trusted ? theme.successColor : theme.secondaryTextColor)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .overlay(
                RoundedRectangle(cornerRadius: Lively.chipRadius)
                    .strokeBorder(trusted ? theme.successColor.opacity(0.5) : theme.cardBorderColor, lineWidth: 1)
            )
    }
}

/// An accent-tinted tag ("VibeDev crew", "API lane").
struct VibeDevTag: View {
    let text: String
    var icon: String?
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 3) {
            if let icon { Image(systemName: icon).font(.system(size: 9, weight: .bold)) }
            Text(text).font(.themed(10, weight: .bold))
        }
        .foregroundStyle(theme.accentColor)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(RoundedRectangle(cornerRadius: Lively.chipRadius).fill(theme.accentColor.opacity(0.14)))
    }
}

/// Label/value rows for header cards.
struct VibeDevKeyValues: View {
    let rows: [(String, String)]
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 3) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                GridRow {
                    Text(row.0)
                        .font(.themed(12))
                        .foregroundStyle(theme.secondaryTextColor)
                    Text(row.1)
                        .font(.themedMono(12))
                        .foregroundStyle(theme.textColor)
                        .textSelection(.enabled)
                }
            }
        }
    }
}

/// "Updated 3 min ago" — shown only while the newest read failed.
struct VibeDevStaleNote: View {
    let lastUpdated: Date?
    let failed: Bool
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        if failed, let lastUpdated {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                let minutes = Int(context.date.timeIntervalSince(lastUpdated) / 60)
                Label(minutes < 1 ? "Updated just now · offline" : "Updated \(minutes) min ago",
                      systemImage: "wifi.exclamationmark")
                    .font(.themed(11, weight: .semibold))
                    .foregroundStyle(theme.secondaryTextColor)
                    .frame(maxWidth: .infinity)
            }
        }
    }
}

/// An empty state line.
struct VibeDevEmpty: View {
    let text: String
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(text)
            .font(.themed(13))
            .foregroundStyle(theme.secondaryTextColor)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 18)
    }
}

/// The sticky bottom action bar, fading the screen under it.
struct VibeDevBottomBar<Content: View>: View {
    @ViewBuilder let content: Content
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.dynamicTypeSize) private var size

    var body: some View {
        // Side by side, or stacked when the text is accessibility-sized.
        let layout = size.isAccessibilitySize ? AnyLayout(VStackLayout(spacing: 8)) : AnyLayout(HStackLayout(spacing: 8))
        layout { content }
            .padding(.horizontal, 16)
            .padding(.top, 10)
            .padding(.bottom, 8)
            .background(
                LinearGradient(colors: [theme.backgroundColor.opacity(0), theme.backgroundColor, theme.backgroundColor],
                               startPoint: .top, endPoint: .bottom)
                    .ignoresSafeArea(edges: .bottom)
            )
    }
}

/// The dictation mic on a prompt field: tap to talk, tap again to drop the
/// words into the field.
struct VibeDevMicButton: View {
    @ObservedObject var dictation: DictationController
    @Binding var text: String
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Button(action: toggle) {
            Image(systemName: dictation.isRecording ? "stop.fill" : (dictation.isTranscribing ? "ellipsis" : "mic.fill"))
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(theme.onAccentColor)
                .frame(width: 34, height: 34)
                .background(Circle().fill(dictation.isRecording ? theme.dangerColor : theme.accentFillColor))
        }
        .buttonStyle(.plain)
        .frame(width: Lively.tapMin, height: Lively.tapMin)
        .contentShape(Rectangle())
        .accessibilityLabel(dictation.isRecording ? "Stop dictation" : "Dictate")
    }

    private func toggle() {
        if dictation.isRecording {
            dictation.finishLive { transcript in
                guard let transcript = transcript?.trimmingCharacters(in: .whitespacesAndNewlines),
                      !transcript.isEmpty else { return }
                let base = text.trimmingCharacters(in: .whitespacesAndNewlines)
                text = base.isEmpty ? transcript : base + " " + transcript
            }
        } else {
            dictation.startLive()
        }
    }
}

/// A prompt field with the mic in its corner; shows the live partial
/// transcript while recording.
struct VibeDevPromptField: View {
    let placeholder: String
    @Binding var text: String
    var minHeight: CGFloat = 92
    var singleLine = false
    @ObservedObject var dictation: DictationController
    @ObservedObject private var theme = ThemeManager.shared
    @FocusState private var focused: Bool

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Group {
                if singleLine {
                    TextField(placeholder, text: $text, axis: .vertical)
                        .lineLimit(1...3)
                        .padding(.vertical, 11)
                } else {
                    TextField(placeholder, text: $text, axis: .vertical)
                        .lineLimit(4...10)
                        .frame(minHeight: minHeight, alignment: .topLeading)
                        .padding(.vertical, 10)
                }
            }
            .font(.themed(15))
            .foregroundStyle(theme.textColor)
            .focused($focused)
            .padding(.leading, 12)
            .padding(.trailing, 48)
            .background(RoundedRectangle(cornerRadius: 12).fill(theme.surfaceColor))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(theme.controlBorderColor, lineWidth: 1))
            VibeDevMicButton(dictation: dictation, text: $text)
        }
        .overlay(alignment: .bottomLeading) {
            if dictation.isRecording {
                Text(dictation.partialTranscript.isEmpty ? "Listening…" : dictation.partialTranscript)
                    .font(.themed(12))
                    .foregroundStyle(theme.secondaryTextColor)
                    .vibeDevLineLimit(2)
                    .padding(.horizontal, 12)
                    .offset(y: 22)
            }
        }
        .padding(.bottom, dictation.isRecording ? 22 : 0)
        .onAppear { focused = true }
        .alert(item: $dictation.permissionAlert) { alert in
            Alert(title: Text(alert.title), message: Text(alert.message),
                  primaryButton: .default(Text("Open Settings")) { DictationController.openSettings() },
                  secondaryButton: .cancel())
        }
    }
}

/// A labelled sheet field ("PROJECT", "MODE").
struct VibeDevField<Content: View>: View {
    let label: String
    @ViewBuilder let content: Content
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(label.uppercased())
                .font(.themed(11, weight: .bold))
                .tracking(0.6)
                .foregroundStyle(theme.secondaryTextColor)
            content
        }
    }
}

/// A switch row with a title and a one-line explanation.
struct VibeDevToggleRow: View {
    let title: String
    var detail: String?
    @Binding var isOn: Bool
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.themed(14, weight: .bold)).foregroundStyle(theme.textColor)
                if let detail {
                    Text(detail).font(.themed(12)).foregroundStyle(theme.secondaryTextColor)
                }
            }
        }
        .tint(theme.accentFillColor)
    }
}
