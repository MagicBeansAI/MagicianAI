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

    /// `partlyDone`: a done run that completed with open requirements reads
    /// "Partly done" in the partial (warning) tone, not done green.
    init(run state: VibeDevRunState, partlyDone: Bool = false, size: CGFloat = 9) {
        self.size = size
        label = state == .done && partlyDone ? TaskCompletion.partlyDoneLabel : state.label
        switch state {
        case .working: tone = .running
        case .needsYou: tone = .waiting
        case .paused: tone = .partial
        case .done: tone = partlyDone ? .partial : .done
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

    /// Also boxes controls whose shared implementation supplies a plain label.
    func vibeDevControlOutline() -> some View { modifier(VibeDevControlOutline()) }
}

private struct VibeDevControlOutline: ViewModifier {
    @ObservedObject private var theme = ThemeManager.shared

    func body(content: Content) -> some View {
        content
            .padding(.horizontal, 8)
            .background(theme.controlColor, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(theme.controlBorderColor, lineWidth: 1))
    }
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

/// Rounded-square actions: primary is the accent fill, danger an outline in danger.
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
            .padding(.vertical, 8)
            .frame(maxWidth: expand ? .infinity : nil, minHeight: large ? 38 : 34)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous).fill(style == .primary
                               ? (configuration.isPressed ? theme.accentFillPressedColor : theme.accentFillColor)
                               : (configuration.isPressed ? theme.surfaceColor : theme.controlColor))
            )
            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(border, lineWidth: 1))
            .opacity(isEnabled ? 1 : 0.5)
            .frame(minHeight: Lively.tapMin)
            .contentShape(Rectangle())
    }
}

/// Match toolbar navigation to the same outline as VibeDev's other actions.
struct VibeDevBackButton: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Button { dismiss() } label: { Image(systemName: "chevron.left") }
            .buttonStyle(VibeDevButtonStyle())
            .accessibilityLabel("Back")
    }
}

/// A row of compact rounded-square choices.
struct VibeDevPillOption<Value: Hashable>: Identifiable {
    let value: Value
    let label: String
    /// A project pill marks a project that needs attention.
    var health: VibeDevProjectHealth = .ok
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
                    HStack(spacing: 4) {
                        Text(option.label)
                        VibeDevProjectHealthGlyph(health: option.health, size: 10)
                    }
                        .font(.themed(12.5, weight: .semibold))
                        .foregroundStyle(selected ? theme.accentColor : theme.secondaryTextColor)
                        .padding(.horizontal, 11)
                        .padding(.vertical, 6)
                        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(selected ? theme.accentColor.opacity(0.1) : theme.cardColor.opacity(0)))
                        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(selected ? theme.accentColor : theme.controlBorderColor, lineWidth: 1))
                        .contentShape(Rectangle())
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
    let location: VibeDevTrust
    private var trusted: Bool { location == .trusted }
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Text(location == .original ? "ORIGINAL FOLDER" : trusted ? "ISOLATED BRANCH" : "ISOLATED WORKSPACE")
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

/// Project health tones: error is danger, warning is warning.
enum VibeDevHealthTone {
    static func color(for level: VibeDevProjectHealth.Level, in theme: ThemeManager) -> Color {
        switch level {
        case .ok: return theme.secondaryTextColor
        case .warning: return theme.warningColor
        case .error: return theme.dangerColor
        }
    }

    static let accessibilityLabel = "Project needs attention"
}

/// "Project needs attention…" then each issue (bold title, message), for
/// the active project. An issue offering `git_init` gets its button, which
/// confirms, runs it, and re-reads the projects (`onChanged` lets the owning
/// screen re-read too). Renders nothing when the project is fine.
struct VibeDevProjectHealthBanner: View {
    let health: VibeDevProjectHealth
    var projectID: String? = nil
    var context: VibeDevContext? = nil
    var onChanged: (() -> Void)? = nil
    @ObservedObject private var theme = ThemeManager.shared
    @State private var confirming = false
    @State private var busy = false
    @State private var failure: String?

    var body: some View {
        if let title = health.title {
            let tint = VibeDevHealthTone.color(for: health.level, in: theme)
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(tint)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 6) {
                    Text(title)
                        .font(.themed(13, weight: .semibold))
                        .foregroundStyle(tint)
                    ForEach(Array(health.issues.enumerated()), id: \.offset) { _, issue in
                        issueView(issue)
                    }
                    if let failure {
                        Text(failure)
                            .font(.themed(12.5))
                            .foregroundStyle(theme.dangerColor)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(tint.opacity(0.1)))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(tint.opacity(0.4), lineWidth: 1))
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("vibedev-project-health")
            .alert(VibeDevGitInitCopy.title, isPresented: $confirming) {
                Button(VibeDevGitInitCopy.confirm) { runGitInit() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text(VibeDevGitInitCopy.message)
            }
        }
    }

    @ViewBuilder
    private func issueView(_ issue: VibeDevProjectHealthIssue) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            if let title = issue.title {
                Text(title)
                    .font(.themed(12.5, weight: .bold))
                    .foregroundStyle(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if issue.message != issue.title {
                Text(issue.message)
                    .font(.themed(12.5))
                    .foregroundStyle(theme.textColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let action = issue.gitInitAction, context != nil, projectID != nil {
                Button {
                    failure = nil
                    confirming = true
                } label: {
                    if busy {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small).tint(theme.accentColor)
                            Text("Working…")
                        }
                    } else {
                        Text(action.label)
                    }
                }
                .buttonStyle(VibeDevButtonStyle(style: .plain))
                .disabled(busy)
                .padding(.top, 4)
                .accessibilityIdentifier("vibedev-project-health-git-init")
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func runGitInit() {
        guard let context, let projectID, !busy else { return }
        busy = true
        Task { @MainActor in
            let message = await context.gitInit(projectID: projectID)
            busy = false
            failure = message
            if message == nil { onChanged?() }
        }
    }
}

/// The ⚠ beside a project name in lists and pickers.
struct VibeDevProjectHealthGlyph: View {
    let health: VibeDevProjectHealth
    var size: CGFloat = 12
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        if health.needsAttention {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: size, weight: .semibold))
                .foregroundStyle(VibeDevHealthTone.color(for: health.level, in: theme))
                .accessibilityLabel(VibeDevHealthTone.accessibilityLabel)
        }
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
    var compact = false
    @ObservedObject private var theme = ThemeManager.shared

    var body: some View {
        Button(action: toggle) {
            Image(systemName: dictation.isRecording ? "stop.fill" : (dictation.isTranscribing ? "ellipsis" : "mic.fill"))
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(theme.onAccentColor)
                .frame(width: compact ? 36 : 34, height: compact ? 36 : 34)
                .background(RoundedRectangle(cornerRadius: compact ? Lively.chipRadius : 8, style: .continuous).fill(dictation.isRecording ? theme.dangerColor : theme.accentFillColor))
        }
        .buttonStyle(.plain)
        .frame(width: compact ? 36 : Lively.tapMin, height: compact ? 36 : Lively.tapMin)
        .contentShape(Rectangle())
        .accessibilityLabel(dictation.isRecording ? "Stop dictation" : "Dictate")
        .disabled(dictation.isTranscribing)
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
                HStack(spacing: 8) {
                    Text(dictation.partialTranscript.isEmpty ? "Listening…" : dictation.partialTranscript)
                        .font(.themed(12)).foregroundStyle(theme.secondaryTextColor).vibeDevLineLimit(2)
                    VoiceActivityWaveform { dictation.level }
                }.padding(.horizontal, 12).offset(y: 28)
            }
        }
        .padding(.bottom, dictation.isRecording ? 28 : 0)
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
