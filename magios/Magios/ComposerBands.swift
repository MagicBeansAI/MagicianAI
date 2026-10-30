import SwiftUI

// MARK: - Pure rules

/// The queue row's one-line title: "N queued · <first message preview>"
/// (web `QueueInspector`, Android `QueueRow`). Whitespace and newlines fold to
/// single spaces so the preview always fits one line; the row ellipsizes it.
enum QueuedMessagesTitle {
    static let previewLimit = 120

    static func make(count: Int, firstText: String?) -> String {
        guard count > 0 else { return "" }
        return "\(count) queued · \(preview(firstText))"
    }

    static func preview(_ text: String?) -> String {
        let folded = (text ?? "")
            .components(separatedBy: .whitespacesAndNewlines)
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        guard !folded.isEmpty else { return "(attachments only)" }
        guard folded.count > previewLimit else { return folded }
        return String(folded.prefix(previewLimit - 1)) + "…"
    }
}

/// The dictation auto-send countdown (web `FloatingComposer` auto-send).
enum AutoSendCountdown {
    /// Seconds between a transcript landing and its send.
    static let seconds = 3

    /// The draining bar: 1 when the countdown starts, 0 at send. Clamped, and
    /// 0 for a degenerate total.
    static func fraction(deadline: Date, total: TimeInterval, now: Date) -> Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, deadline.timeIntervalSince(now) / total))
    }

    /// The stepped fraction Reduce Motion shows: one jump per whole second.
    static func steppedFraction(remaining: Int, total: Int = seconds) -> Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, Double(remaining) / Double(total)))
    }

    static func label(remaining: Int) -> String { "Sending in \(max(0, remaining))s…" }
}

/// Composer focus on one plan question (web `PlanReplyIntent`): while set, a
/// Plan-mode send answers that question (`plan_task_id` / `plan_question_id`)
/// instead of starting a new plan, and the mode returns to `returnMode`.
struct PlanReplyTarget: Equatable {
    let taskId: String
    let taskTitle: String
    let questionId: String
    let questionText: String
    var returnMode: ComposerMode = .ask
}

/// Agent presence beside the session title (web `next/presence.ts`).
/// Precedence: needs-you > working > thinking > idle.
enum ChatPresenceState: String, Equatable {
    case idle, thinking, working, needsYou

    static func derive(hasPendingAsk: Bool, streaming: Bool, toolsRunning: Bool, liveTurn: Bool) -> ChatPresenceState {
        if hasPendingAsk { return .needsYou }
        if streaming || toolsRunning { return .working }
        if liveTurn { return .thinking }
        return .idle
    }

    func label(assistant: String) -> String {
        switch self {
        case .idle: return "\(assistant) is idle"
        case .thinking: return "\(assistant) is thinking"
        case .working: return "\(assistant) is working"
        case .needsYou: return "\(assistant) needs you"
        }
    }
}

// MARK: - Band chrome

/// A flush band inside the composer card: full card width, a 14 pt text
/// inset, a tint (never an opaque fill that would hide the chat wash through
/// the card), and a hairline under it.
private struct ComposerBand<Content: View>: View {
    var tint: Color
    var rule: Color
    var minHeight: CGFloat = 30
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(spacing: 0) {
            content()
                .padding(.horizontal, 14)
                .frame(maxWidth: .infinity, minHeight: minHeight, alignment: .leading)
                .background(tint)
            Rectangle().fill(rule).frame(height: 0.5).accessibilityHidden(true)
        }
        // Always its content's height: a band never gives up lines to the
        // flexible views around it.
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// A small 6 pt rounded-square chip for a band: compact visibly, with a hit
/// area grown toward 44 pt without growing the band.
struct BandChipStyle: ButtonStyle {
    var tint: Color
    @ObservedObject private var theme = ThemeManager.shared

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.themed(12, weight: .semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 9)
            .padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                    .fill(theme.surfaceColor.opacity(configuration.isPressed ? 0.5 : 0.85))
            )
            .overlay(
                RoundedRectangle(cornerRadius: Lively.chipRadius, style: .continuous)
                    .strokeBorder(tint.opacity(0.35), lineWidth: 1)
            )
            .contentShape(Rectangle().inset(by: -10))
    }
}

// MARK: - Bands

/// Dictation auto-send: "Sending in 3s…" with a draining bar and Cancel.
struct AutoSendCountdownBand: View {
    let remaining: Int
    let deadline: Date?
    var onCancel: () -> Void
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ComposerBand(tint: theme.accentColor.opacity(0.12), rule: theme.accentColor.opacity(0.25)) {
            HStack(spacing: 8) {
                Image(systemName: "waveform").font(.system(size: 11, weight: .semibold))
                    .foregroundColor(theme.accentColor)
                    .accessibilityHidden(true)
                Text(AutoSendCountdown.label(remaining: remaining))
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(theme.textColor)
                    .lineLimit(1)
                Spacer(minLength: 8)
                Button("Cancel", action: onCancel)
                    .buttonStyle(BandChipStyle(tint: theme.accentColor))
                    .accessibilityHint("Keeps the dictated text without sending it")
                    .accessibilityIdentifier("chat-autosend-cancel")
            }
        }
        .overlay(alignment: .bottom) { drainBar.padding(.bottom, 0.5) }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-autosend-countdown")
    }

    @ViewBuilder
    private var drainBar: some View {
        let total = TimeInterval(AutoSendCountdown.seconds)
        if reduceMotion || deadline == nil {
            bar(AutoSendCountdown.steppedFraction(remaining: remaining))
        } else if let deadline {
            TimelineView(.animation(minimumInterval: 1 / 30)) { context in
                bar(AutoSendCountdown.fraction(deadline: deadline, total: total, now: context.date))
            }
        }
    }

    private func bar(_ fraction: Double) -> some View {
        Rectangle()
            .fill(theme.accentColor.opacity(0.7))
            .frame(height: 2)
            .scaleEffect(x: fraction, y: 1, anchor: .leading)
            .accessibilityHidden(true)
    }
}

/// A queue failure, in place: one band in the danger colour, dismissible.
struct ComposerErrorBand: View {
    let message: String
    var onDismiss: () -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        ComposerBand(tint: theme.dangerColor.opacity(0.1), rule: theme.dangerColor.opacity(0.3)) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.circle.fill").font(.system(size: 11))
                    .accessibilityHidden(true)
                Text(message).font(.themed(12)).lineLimit(2)
                Spacer(minLength: 4)
                Button(action: onDismiss) {
                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold))
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle().inset(by: -10))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss error")
            }
            .foregroundColor(theme.dangerColor)
            .padding(.vertical, 4)
        }
        .accessibilityIdentifier("chat-queue-error")
    }
}

/// Plan mode is on: the next send drafts a plan (web `.plan-banner`).
struct PlanModeBand: View {
    var body: some View {
        ComposerModeBand(title: "Plan mode", message: "I'll draft a plan first. Switch to Do for direct execution.")
            .accessibilityIdentifier("chat-plan-mode-band")
    }
}

/// Chat and VibeDev mode panels share theme tint, border, pulse and typography.
struct ComposerModeBand: View {
    let title: String
    let message: String
    var options: AnyView? = nil
    var maxLines = 3
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pulse = false

    var body: some View {
        ComposerBand(tint: theme.accentColor.opacity(0.08), rule: theme.accentColor.opacity(0.14)) {
            VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Circle()
                    .fill(theme.accentColor)
                    .frame(width: 6, height: 6)
                    .opacity(!reduceMotion && pulse ? 0.35 : 1)
                    .animation(reduceMotion ? nil : .easeInOut(duration: 1.1).repeatForever(autoreverses: true), value: pulse)
                    .onAppear { pulse = !reduceMotion }
                    .accessibilityHidden(true)
                (Text(title).bold() + Text(" — " + message))
                    .font(.themed(12))
                    .foregroundColor(theme.accentHoverColor)
                    .lineLimit(maxLines).truncationMode(.tail)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 5)
            if let options { options.padding(.vertical, 6) }
            }
        }
        .accessibilityElement(children: .contain)
    }
}

/// Replying into one plan question (web `PlanReplyBanner`). Stale when the
/// question left the planner while the reply was being written.
struct PlanReplyBanner: View {
    let target: PlanReplyTarget
    var stale = false
    var onCancel: () -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        let tone = stale ? theme.warningColor : theme.infoColor
        ComposerBand(tint: tone.opacity(0.12), rule: tone.opacity(0.32)) {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Image(systemName: "arrowshape.turn.up.left.fill").font(.system(size: 9))
                            .foregroundColor(tone)
                            .accessibilityHidden(true)
                        Text(stale ? "Planning question changed" : "Replying to Planner")
                            .foregroundColor(theme.secondaryTextColor)
                        Text(target.taskTitle).bold().foregroundColor(theme.textColor)
                            .lineLimit(1).truncationMode(.tail)
                    }
                    .font(.themed(11))
                    Text(target.questionText)
                        .font(.themed(12))
                        .foregroundColor(theme.textColor)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 4)
                Button(action: onCancel) {
                    Image(systemName: "xmark").font(.system(size: 11, weight: .bold))
                        .foregroundColor(theme.secondaryTextColor)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle().inset(by: -8))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Cancel planning reply")
            }
            .padding(.vertical, 8)
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityIdentifier("chat-plan-reply-banner")
    }
}

/// An archived conversation is read-only: this band takes the composer's
/// place (web `chat-readonly-banner`).
struct ArchivedSessionBand: View {
    var onReturn: () -> Void
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "archivebox").font(.system(size: 13))
                .foregroundColor(theme.secondaryTextColor)
                .accessibilityHidden(true)
            Text("Viewing archived conversation (read-only)")
                .font(.themed(13, weight: .medium))
                .foregroundColor(theme.textColor)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 6)
            Button("Return to active", action: onReturn)
                .buttonStyle(ChipStyle())
                .accessibilityIdentifier("chat-return-to-active")
        }
        .padding(.leading, 14)
        .padding(.trailing, 4)
        .background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: Lively.composerRadius))
        .overlay(
            RoundedRectangle(cornerRadius: Lively.composerRadius)
                .stroke(theme.secondaryTextColor.opacity(0.2), lineWidth: 1)
        )
        .padding(.horizontal, ComposerView.horizontalInset)
        .padding(.top, 2)
        .padding(.bottom, 6)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-archived-band")
    }
}

// MARK: - Presence dot

/// The small presence dot beside the session title: idle (online) green,
/// thinking accent (breathing), working the running colour (pulsing),
/// needs-you amber with a soft ring. Reduce Motion stops every animation.
struct ChatPresenceDot: View {
    let state: ChatPresenceState
    var size: CGFloat = 7
    @ObservedObject private var theme = ThemeManager.shared
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase = false

    static func color(for state: ChatPresenceState, in theme: ThemeManager) -> Color {
        switch state {
        case .idle: return theme.successColor
        case .thinking: return theme.accentColor
        case .working: return theme.statusRunningColor
        case .needsYou: return theme.warningColor
        }
    }

    private var moving: Bool { !reduceMotion && (state == .thinking || state == .working) }

    var body: some View {
        let color = Self.color(for: state, in: theme)
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .overlay(
                Circle().stroke(color.opacity(state == .needsYou ? 0.3 : 0), lineWidth: 3)
                    .frame(width: size + 3, height: size + 3)
            )
            .scaleEffect(moving && state == .working && phase ? 1.3 : 1)
            .opacity(moving && state == .thinking && phase ? 0.45 : 1)
            .animation(moving
                ? .easeInOut(duration: state == .working ? 0.8 : 1.4).repeatForever(autoreverses: true)
                : .default, value: phase)
            .onAppear { phase = moving }
            .onChange(of: moving) { _, now in phase = now }
            .accessibilityHidden(true)
    }
}

/// Reveals a mode panel downward inside its own slot, below the context row.
struct ComposerPanelReveal: ViewModifier {
    var fraction: CGFloat
    func body(content: Content) -> some View {
        content.clipShape(ComposerPanelRevealShape(fraction: fraction))
    }
}

private struct ComposerPanelRevealShape: Shape {
    var fraction: CGFloat
    var animatableData: CGFloat {
        get { fraction }
        set { fraction = newValue }
    }
    func path(in rect: CGRect) -> Path {
        Path(CGRect(x: rect.minX, y: rect.minY, width: rect.width, height: rect.height * min(1, max(0, fraction))))
    }
}
