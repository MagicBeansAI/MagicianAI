import SwiftUI

/// Shared style tokens for the lively chat surfaces (task card, transcript,
/// composer). Every theme reads the same radii; colours come from
/// `ThemeManager`, so the cards look right in every palette.
enum Lively {
    static let chipRadius: CGFloat = 6
    static let cardRadius: CGFloat = 14
    /// The composer card and everything stacked on it (docks, mention picker):
    /// 10 pt on every platform (web `--radius-composer: 10px`).
    static let composerRadius: CGFloat = 10
    static let bubbleRadius: CGFloat = 16
    /// Minimum hit area for anything tappable (Apple HIG).
    static let tapMin: CGFloat = 44
    /// Step lists (chat Steps panel, card, task detail, Plan Inspector): the
    /// step's title, its secondary lines, and each row's vertical padding.
    static let stepTitleSize: CGFloat = 12
    static let stepMetaSize: CGFloat = 10.5
    static let stepRowPadding: CGFloat = 6
}

/// The subtle 1 px rule between two rows of a step list, inset to the text
/// column (never under the leading status icon). Callers draw it between
/// rows only, never after the last.
struct StepHairline: View {
    var leadingInset: CGFloat = 0

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        Rectangle()
            .fill(theme.controlBorderColor.opacity(0.6))
            .frame(height: 1 / max(displayScale, 1))
            .padding(.leading, leadingInset)
            .accessibilityHidden(true)
    }
}

/// The five stable step tints (web `--tint-1…5`, lively order: rose, violet,
/// mint, butter, peach), mid-tone so a small dot reads on any card.
enum StepTint {
    static func color(slot: Int, isDark: Bool) -> Color {
        let light = ["#d9566b", "#7a5ce0", "#2f9e72", "#c99412", "#e0763a"]
        let dark = ["#ff9aa6", "#b8a6ff", "#7fd9b4", "#ffd36b", "#ffb07f"]
        let palette = isDark ? dark : light
        return Color(hex: palette[(max(1, min(5, slot))) - 1])
    }
}

/// The live Steps header's dot strip (web TurnStage trail): `+N` for earlier
/// settled steps, then one dot per latest settled step — tinted by its tool,
/// red when failed, amber when waiting.
struct StepDotStrip: View {
    let rows: [ActivityRow]
    @ObservedObject var theme: ThemeManager
    /// While the run is active the latest dot breathes (it replaced the
    /// header's spinner). Reduce Motion keeps it still.
    var breathesLatest = false

    var body: some View {
        let window = activityStageDots(rows)
        if !window.shown.isEmpty {
            HStack(spacing: 3) {
                if window.hidden > 0 {
                    Text("+\(window.hidden)")
                        .font(.themed(10, weight: .medium))
                        .monospacedDigit()
                        .foregroundColor(theme.secondaryTextColor)
                }
                ForEach(window.shown) { row in
                    if breathesLatest, row.id == window.shown.last?.id {
                        BreathingDot(color: Self.color(for: row, theme: theme))
                            .accessibilityIdentifier("activity-step-dot-breathing")
                    } else {
                        Circle()
                            .fill(Self.color(for: row, theme: theme))
                            .frame(width: 7, height: 7)
                    }
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(window.hidden > 0
                ? "Latest \(window.shown.count) steps, \(window.hidden) earlier"
                : "\(window.shown.count) steps settled")
            .accessibilityIdentifier("activity-step-dots")
        }
    }

    static func color(for row: ActivityRow, theme: ThemeManager) -> Color {
        switch row.status {
        case .failed: return theme.dangerColor
        case .waiting: return theme.warningColor
        default: return StepTint.color(slot: activityTintSlot(activityTintKey(row)), isDark: theme.isDark)
        }
    }
}

/// The live run's latest step dot: a gentle breath (opacity 0.45→1, scale
/// 1→1.25, ease-in-out, `ActivityStepsHeader.breathPeriod`). Reduce Motion
/// shows it still at full colour.
struct BreathingDot: View {
    let color: Color
    var size: CGFloat = 7
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(paused: reduceMotion)) { context in
            let phase = reduceMotion
                ? 1
                : ActivityStepsHeader.breathPhase(at: context.date.timeIntervalSinceReferenceDate)
            Circle()
                .fill(color)
                .frame(width: size, height: size)
                .scaleEffect(1 + 0.25 * phase)
                .opacity(0.45 + 0.55 * phase)
        }
        .frame(width: size, height: size)
    }
}

/// A thin indeterminate ring around the Steps caret while the run is active:
/// a 30% arc turning once per `ActivityStepsHeader.ringPeriod`. Reduce Motion
/// shows the arc still. Drawn as an overlay, so it never moves layout or
/// changes the hit area.
struct ChevronLoadingRing: View {
    let color: Color
    var diameter: CGFloat = 16
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        TimelineView(.animation(paused: reduceMotion)) { context in
            let turns = reduceMotion
                ? 0
                : ActivityStepsHeader.ringTurns(at: context.date.timeIntervalSinceReferenceDate)
            ZStack {
                Circle().stroke(color.opacity(0.18), lineWidth: 1.25)
                Circle()
                    .trim(from: 0, to: 0.3)
                    .stroke(color, style: StrokeStyle(lineWidth: 1.25, lineCap: .round))
                    .rotationEffect(.degrees(360 * turns - 90))
            }
            .frame(width: diameter, height: diameter)
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// A task/step status dot. Colours follow the web card's status language:
/// done green, failed red, partial and waiting amber (waiting pulses, partial
/// is solid), running the accent (breathing), not started the secondary
/// accent, cancelled muted. Reduce Motion stops every animation.
struct StatusDot: View {
    enum Tone: CaseIterable {
        case running, waiting, done, failed, partial, notStarted, cancelled
    }

    let tone: Tone
    var size: CGFloat = 8

    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase = false

    static func color(for tone: Tone, in theme: ThemeManager) -> Color {
        switch tone {
        case .running: return theme.statusRunningColor
        case .waiting, .partial: return theme.warningColor
        case .done: return theme.successColor
        case .failed: return theme.dangerColor
        case .notStarted: return theme.secondaryAccentColor
        case .cancelled: return theme.secondaryTextColor
        }
    }

    /// Only live tones move, and never under Reduce Motion.
    func animates(reduceMotion: Bool) -> Bool {
        guard !reduceMotion else { return false }
        return tone == .running || tone == .waiting
    }

    var body: some View {
        let moving = animates(reduceMotion: reduceMotion)
        Circle()
            .fill(Self.color(for: tone, in: theme))
            .frame(width: size, height: size)
            // Running breathes (scale + soft glow); waiting pulses (opacity).
            .scaleEffect(moving && tone == .running && phase ? 1.25 : 1)
            .opacity(moving && tone == .waiting && phase ? 0.45 : 1)
            .shadow(
                color: Self.color(for: tone, in: theme).opacity(moving && tone == .running && phase ? 0.6 : 0),
                radius: size / 2)
            .animation(
                moving
                    ? .easeInOut(duration: tone == .running ? 1.6 : 1.2).repeatForever(autoreverses: true)
                    : .default,
                value: phase)
            .onAppear { phase = moving }
            .onChange(of: moving) { _, now in phase = now }
            .accessibilityHidden(true)
    }
}

/// A compact chip button: 6 pt radius, 12 pt themed font, control fill, and
/// a 44 pt hit area even though the visible chip is smaller.
struct ChipStyle: ButtonStyle {
    static let cornerRadius: CGFloat = Lively.chipRadius
    static let fontSize: CGFloat = 12
    static let minHitSize: CGFloat = Lively.tapMin

    @ObservedObject private var theme = ThemeManager.shared
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.themed(Self.fontSize, weight: .semibold))
            .foregroundStyle(prominent ? theme.onAccentColor : theme.textColor)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
                    .fill(prominent
                          ? (configuration.isPressed ? theme.accentFillPressedColor : theme.accentFillColor)
                          : theme.controlColor)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Self.cornerRadius, style: .continuous)
                    .strokeBorder(prominent ? Color.clear : theme.controlBorderColor, lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.7 : 1)
            // Grow the tappable frame to the HIG minimum without growing the chip.
            .frame(minWidth: Self.minHitSize, minHeight: Self.minHitSize)
            .contentShape(Rectangle())
    }
}

extension ThemeManager {
    /// The wash stops for a theme: the palette's `--wash-hero` stops, or
    /// background → surface for themes without one (a wash is never flat).
    static func washHexStops(for theme: String) -> [String] {
        let palette = palette(for: theme)
        return palette.wash ?? [palette.background, palette.surface]
    }

    /// The active theme's background wash as a gradient.
    var washGradient: LinearGradient {
        LinearGradient(
            colors: Self.washHexStops(for: currentThemeName).map { Color(hex: $0) },
            startPoint: .topLeading,
            endPoint: .bottomTrailing)
    }
}
