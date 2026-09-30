import SwiftUI

/// Reports the real laid-out height of the key stack so the controller can size the
/// input view to it exactly — no manual guesswork, so nothing is clipped or leaves a
/// gap when the strip / aux row / rows change.
private struct KeyboardContentHeightKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

/// The keyboard's SwiftUI root: a top strip (normal predictions + the ✦ brand key)
/// over the key grid, with the agentic panels overlaid on demand.
struct KeyboardRootView: View {
    @ObservedObject var model: KeyboardModel
    /// The controller sets this to size the input view to the measured content.
    var onHeightChange: (CGFloat) -> Void = { _ in }

    var body: some View {
        ZStack(alignment: .top) {
            // The key stack drives the keyboard's height (measured below). Panels
            // overlay it without changing the height, so they never get clipped.
            VStack(spacing: KeyboardTheme.rowSpacing) {
                if let step = model.coachStep {
                    CoachBanner(model: model, step: step)
                }
                KeyboardStrip(model: model)
                if model.hasFullAccess, model.isSensitive {
                    SecureFieldNote()
                } else if model.aiSurfaceRevealed {
                    AIActionRow(model: model)
                }
                if model.input.layer == .emoji {
                    EmojiKeyboardView(model: model)
                } else {
                    KeyGridView(rows: model.rows, shift: model.input.shift, model: model)
                        .equatable()
                }
            }
            .padding(.horizontal, 4)
            .padding(.top, 6)
            .padding(.bottom, 4)
            .background(
                GeometryReader { geo in
                    Color.clear.preference(key: KeyboardContentHeightKey.self, value: geo.size.height)
                }
            )

            if showsWritePanel {
                WritePanel(model: model)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
            if showsAskPanel {
                AskPanel(model: model)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
            if showsActPanel {
                ActPanel(model: model)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .frame(maxWidth: .infinity)
        .background(KeyboardTheme.backdrop.ignoresSafeArea())
        .onPreferenceChange(KeyboardContentHeightKey.self) { onHeightChange($0) }
        .animation(.easeOut(duration: 0.18), value: showsWritePanel)
        .animation(.easeOut(duration: 0.18), value: showsAskPanel)
        .animation(.easeOut(duration: 0.18), value: showsActPanel)
        .animation(.easeOut(duration: 0.18), value: model.aiSurfaceRevealed)
    }

    private var showsActPanel: Bool {
        model.act != .idle
    }

    private var showsWritePanel: Bool {
        switch model.write {
        case .generating, .preview, .error: return true
        default: return false
        }
    }

    private var showsAskPanel: Bool {
        model.ask != .idle
    }
}

/// The top strip: local suggestions plus the persistent ✦ entry point for the
/// on-demand Magican action row.
struct KeyboardStrip: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        Group {
            if let original = model.revertOriginal {
                revertStrip(original)
            } else if case .undo = model.write {
                undoStrip
            } else {
                // Normal iOS feel: predictions while typing, brand wordmark at rest —
                // with the ✦ Magican brand key always available on the trailing edge.
                HStack(spacing: 6) {
                    Group {
                        if !model.suggestions.isEmpty {
                            suggestionStrip
                        } else {
                            brandedStrip
                        }
                    }
                    .frame(maxWidth: .infinity)
                    if !model.isSensitive {
                        BrandKey(model: model)
                    }
                }
            }
        }
        .frame(height: 34)
    }

    private func revertStrip(_ original: String) -> some View {
        Button {
            model.revertAutocorrect()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "arrow.uturn.backward").font(.system(size: 12))
                Text("Revert to “\(original)”").font(.system(size: 13, weight: .medium)).lineLimit(1)
                Spacer(minLength: 0)
            }
            .foregroundColor(KeyboardTheme.keyText.opacity(0.7))
            .padding(.horizontal, 10)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var undoStrip: some View {
        HStack(spacing: 10) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 13)).foregroundColor(KeyboardTheme.accent)
            Text("Rewritten")
                .font(.system(size: 13, weight: .medium)).foregroundColor(KeyboardTheme.keyText.opacity(0.7))
            Spacer()
            Button { model.undoWrite() } label: {
                Text("Undo").font(.system(size: 13, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
            }.buttonStyle(.plain)
            Button { model.dismissWrite() } label: {
                Image(systemName: "xmark").font(.system(size: 11)).foregroundColor(KeyboardTheme.keyText.opacity(0.4))
            }.buttonStyle(.plain)
        }
        .padding(.horizontal, 10)
    }

    private var suggestionStrip: some View {
        HStack(spacing: 0) {
            ForEach(Array(model.suggestions.enumerated()), id: \.offset) { index, suggestion in
                if index > 0 {
                    Divider().frame(height: 18).background(KeyboardTheme.keyText.opacity(0.12))
                }
                Button {
                    model.applySuggestion(suggestion)
                } label: {
                    Text(suggestion)
                        .font(.system(size: 15))
                        .foregroundColor(KeyboardTheme.keyText)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                        .frame(maxWidth: .infinity)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 6)
    }

    private var brandedStrip: some View {
        HStack(spacing: 6) {
            if !model.hasFullAccess {
                Text("Full Access off — typing only")
                    .font(.system(size: 12))
                    .foregroundColor(KeyboardTheme.keyText.opacity(0.4))
            } else {
                // Subtle discoverability hint for the on-demand agentic surface.
                Text("Hold space or tap ✦ for Magican")
                    .font(.system(size: 12))
                    .foregroundColor(KeyboardTheme.keyText.opacity(0.38))
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
    }
}

/// The always-available ✦ Magican brand key on the strip's trailing edge — taps open
/// (or close) the agentic action surface. The one persistent, discoverable AI entry.
struct BrandKey: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        Button {
            KeyboardHaptics.special()
            model.toggleAISurface()
        } label: {
            Image(systemName: model.aiSurfaceRevealed ? "xmark" : "sparkles")
                .font(.system(size: 15, weight: .bold))
                .foregroundColor(model.aiSurfaceRevealed ? .white : KeyboardTheme.accent)
                .frame(width: 34, height: 28)
                .background(
                    // Only a filled pill when open — at rest it's just the ✦ glyph,
                    // so the resting strip stays clean (no floating rounded box).
                    Group {
                        if model.aiSurfaceRevealed {
                            RoundedRectangle(cornerRadius: 14).fill(KeyboardTheme.accent)
                        }
                    }
                )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(model.aiSurfaceRevealed ? "Close Magican actions" : "Open Magican actions")
    }
}

/// The on-demand agentic surface: explicit Rewrite / Ask / Paste + skill chips,
/// revealed by the ✦ brand key or a stationary space long-press. Hidden by default
/// so the keyboard types like a normal iOS keyboard.
struct AIActionRow: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        HStack(spacing: 6) {
            if model.hasFullAccess {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        actionChip("Rewrite", symbol: "sparkles", fill: KeyboardTheme.accentSoft) {
                            Task { await model.runWrite(action: .rewrite, guidance: nil) }
                        }
                        actionChip("Ask", symbol: "questionmark.bubble", fill: KeyboardTheme.keyText.opacity(0.08)) {
                            Task { await model.beginAsk() }
                        }
                        if model.canPaste {
                            actionChip("Paste", symbol: "doc.on.clipboard", fill: KeyboardTheme.keyText.opacity(0.08)) {
                                model.pasteClipboard()
                            }
                        }
                        ForEach(model.skills) { skill in
                            skillChip(skill)
                        }
                    }
                    .padding(.horizontal, 6)
                }
            } else {
                Text("Turn on Full Access for Magican to write, ask, and run skills.")
                    .font(.system(size: 12))
                    .foregroundColor(KeyboardTheme.keyText.opacity(0.55))
                    .lineLimit(2).minimumScaleFactor(0.8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.leading, 8)
            }
            Button { model.hideAISurface() } label: {
                Image(systemName: "xmark").font(.system(size: 12, weight: .semibold))
                    .foregroundColor(KeyboardTheme.keyText.opacity(0.5))
                    .frame(width: 30, height: 30)
            }.buttonStyle(.plain)
        }
        .frame(height: 34)
    }

    private func actionChip(_ title: String, symbol: String, fill: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: symbol).font(.system(size: 11))
                Text(title).font(.system(size: 12, weight: .semibold)).lineLimit(1)
            }
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(Capsule().fill(fill))
            .foregroundColor(KeyboardTheme.keyText.opacity(0.85))
        }.buttonStyle(.plain)
    }

    private func skillChip(_ skill: KeyboardSkill) -> some View {
        Button { model.dispatch(skill) } label: {
            HStack(spacing: 5) {
                Image(systemName: skill.symbol).font(.system(size: 11))
                Text(skill.label).font(.system(size: 12, weight: .medium)).lineLimit(1)
            }
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(Capsule().fill(laneColor(skill.lane)))
            .foregroundColor(KeyboardTheme.keyText.opacity(0.85))
            .overlay(
                skill.lane == .act
                    ? Capsule().stroke(Color.orange.opacity(0.5), lineWidth: 1)
                    : nil
            )
        }.buttonStyle(.plain)
    }

    private func laneColor(_ lane: KeyboardSkill.Lane) -> Color {
        switch lane {
        case .write: return KeyboardTheme.accentSoft
        case .ask: return KeyboardTheme.keyText.opacity(0.08)
        case .act: return Color.orange.opacity(0.16)
        }
    }
}

/// Overlay panel for the Write lane's generating / preview / error states.
struct WritePanel: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        content
            .padding(14)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(KeyboardTheme.backdrop.opacity(0.98))
    }

    @ViewBuilder
    private var content: some View {
        switch model.write {
        case .generating:
            VStack(spacing: 10) {
                ProgressView().tint(KeyboardTheme.accent)
                Text(model.writeStage.isEmpty ? "Magican is writing…" : model.writeStage)
                    .font(.system(size: 14)).foregroundColor(KeyboardTheme.keyText.opacity(0.7))
                    .animation(.easeInOut(duration: 0.2), value: model.writeStage)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .preview(let draft, let plan):
            previewCard(draft: draft, plan: plan)
        case .error(let message):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle").foregroundColor(.orange).font(.system(size: 22))
                Text(message).font(.system(size: 13)).foregroundColor(KeyboardTheme.keyText.opacity(0.75)).multilineTextAlignment(.center)
                HStack(spacing: 18) {
                    Button("Dismiss") { model.dismissWrite() }
                        .font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.keyText.opacity(0.6))
                    Button("Try again") { Task { await model.regenerateWrite() } }
                        .font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        default:
            EmptyView()
        }
    }

    private func previewCard(draft: String, plan: KeyboardEditPlan) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Rewrite", systemImage: "sparkles")
                    .font(.system(size: 13, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
                Spacer()
                if !plan.isReplace {
                    Text("will append").font(.system(size: 11)).foregroundColor(KeyboardTheme.keyText.opacity(0.4))
                }
                Button { Task { await model.regenerateWrite() } } label: {
                    Label("Try another", systemImage: "arrow.triangle.2.circlepath")
                        .labelStyle(.titleAndIcon)
                        .font(.system(size: 12, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
                }.buttonStyle(.plain)
                Button { model.dismissWrite() } label: {
                    Image(systemName: "xmark.circle.fill").foregroundColor(KeyboardTheme.keyText.opacity(0.3))
                }.buttonStyle(.plain)
            }
            ScrollView {
                Text(draft)
                    .font(.system(size: 15))
                    .foregroundColor(KeyboardTheme.keyText)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack(spacing: 10) {
                Button { model.dismissWrite() } label: {
                    Text("Cancel").frame(maxWidth: .infinity)
                }.buttonStyle(.bordered)
                Button { model.acceptWrite(draft: draft, plan: plan) } label: {
                    Label(plan.isReplace ? "Replace" : "Insert", systemImage: "checkmark").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent).tint(KeyboardTheme.accent)
            }
        }
    }
}

/// Live onboarding banner — advances as the user performs each action in the
/// in-app playground.
struct CoachBanner: View {
    @ObservedObject var model: KeyboardModel
    let step: KeyboardCoachStep

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "sparkles").font(.system(size: 12, weight: .bold)).foregroundColor(.white)
            Text(step.title)
                .font(.system(size: 13, weight: .semibold)).foregroundColor(.white)
                .lineLimit(1).minimumScaleFactor(0.7)
            Spacer(minLength: 6)
            if step == .done {
                Button { model.finishCoach() } label: {
                    Text("Finish").font(.system(size: 12, weight: .bold))
                        .foregroundColor(KeyboardTheme.accent)
                        .padding(.horizontal, 10).padding(.vertical, 4)
                        .background(Capsule().fill(.white))
                }.buttonStyle(.plain)
            } else {
                Button { model.finishCoach() } label: {
                    Text("Skip").font(.system(size: 12)).foregroundColor(.white.opacity(0.75))
                }.buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 30)
        .background(
            LinearGradient(colors: [KeyboardTheme.accent, KeyboardTheme.accent.opacity(0.82)],
                           startPoint: .leading, endPoint: .trailing)
        )
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .padding(.horizontal, 2)
    }
}

/// Shown in place of the skill chips in a secure/OTP/number field — Magican captures
/// nothing here.
struct SecureFieldNote: View {
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "lock.fill").font(.system(size: 11))
            Text("Magican paused — secure field, nothing captured")
                .font(.system(size: 12))
        }
        .foregroundColor(KeyboardTheme.keyText.opacity(0.4))
        .frame(maxWidth: .infinity)
        .frame(height: 30)
    }
}

/// The Act lane: a verification card (Confirm-first), then Working / Done.
struct ActPanel: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        content
            .padding(14)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(KeyboardTheme.backdrop.opacity(0.98))
    }

    @ViewBuilder
    private var content: some View {
        switch model.act {
        case .idle:
            EmptyView()
        case .confirm(let skill, let goal):
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Label("Run: \(skill.label)", systemImage: skill.symbol)
                        .font(.system(size: 13, weight: .semibold)).foregroundColor(.orange)
                    Spacer()
                    Button { model.dismissAct() } label: {
                        Image(systemName: "xmark.circle.fill").foregroundColor(KeyboardTheme.keyText.opacity(0.3))
                    }.buttonStyle(.plain)
                }
                Text("Magican will run this task:")
                    .font(.system(size: 12)).foregroundColor(KeyboardTheme.keyText.opacity(0.5))
                ScrollView {
                    Text(goal).font(.system(size: 15)).foregroundColor(KeyboardTheme.keyText)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                Text("Only this action runs. You'll get a task to track it.")
                    .font(.system(size: 11)).foregroundColor(KeyboardTheme.keyText.opacity(0.4))
                HStack(spacing: 10) {
                    Button { model.dismissAct() } label: { Text("Cancel").frame(maxWidth: .infinity) }
                        .buttonStyle(.bordered)
                    Button { Task { await model.confirmAct() } } label: {
                        Label("Confirm", systemImage: "bolt.fill").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent).tint(.orange)
                }
            }
        case .working(let label):
            VStack(spacing: 10) {
                ProgressView().tint(.orange)
                Text("Starting “\(label)”…").font(.system(size: 14)).foregroundColor(KeyboardTheme.keyText.opacity(0.7))
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .done(_, let label):
            VStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill").foregroundColor(.orange).font(.system(size: 24))
                Text("“\(label)” started").font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.keyText)
                Text("Open Magican to track it.").font(.system(size: 12)).foregroundColor(KeyboardTheme.keyText.opacity(0.5))
                Button("Done") { model.dismissAct() }
                    .font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .error(let message):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle").foregroundColor(.orange).font(.system(size: 22))
                Text(message).font(.system(size: 13)).foregroundColor(KeyboardTheme.keyText.opacity(0.75)).multilineTextAlignment(.center)
                Button("Dismiss") { model.dismissAct() }
                    .font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

/// The Ask lane canvas: a streamed answer with Insert.
struct AskPanel: View {
    @ObservedObject var model: KeyboardModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Ask Magican", systemImage: "sparkles")
                    .font(.system(size: 13, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
                Spacer()
                Button { model.dismissAsk() } label: {
                    Image(systemName: "xmark.circle.fill").foregroundColor(KeyboardTheme.keyText.opacity(0.3))
                }.buttonStyle(.plain)
            }
            body(for: model.ask)
        }
        .padding(14)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(KeyboardTheme.backdrop.opacity(0.98))
    }

    @ViewBuilder
    private func body(for state: KeyboardAskState) -> some View {
        switch state {
        case .idle:
            EmptyView()
        case .streaming(let answer, let question):
            answerView(question: question, answer: answer, streaming: true)
        case .done(let answer, let question):
            answerView(question: question, answer: answer, streaming: false)
        case .error(let message):
            VStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle").foregroundColor(.orange).font(.system(size: 22))
                Text(message).font(.system(size: 13)).foregroundColor(KeyboardTheme.keyText.opacity(0.75)).multilineTextAlignment(.center)
                Button("Dismiss") { model.dismissAsk() }
                    .font(.system(size: 14, weight: .semibold)).foregroundColor(KeyboardTheme.accent)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func answerView(question: String, answer: String, streaming: Bool) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(question)
                .font(.system(size: 12)).foregroundColor(KeyboardTheme.keyText.opacity(0.45)).lineLimit(1)
            ScrollView {
                if answer.isEmpty && streaming {
                    HStack(spacing: 6) {
                        ProgressView().tint(KeyboardTheme.accent)
                        Text("Thinking…").font(.system(size: 13)).foregroundColor(KeyboardTheme.keyText.opacity(0.5))
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    Text(answer)
                        .font(.system(size: 15)).foregroundColor(KeyboardTheme.keyText)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            HStack(spacing: 10) {
                Button { model.dismissAsk() } label: { Text("Close").frame(maxWidth: .infinity) }
                    .buttonStyle(.bordered)
                Button { model.insertAnswer(answer) } label: {
                    Label("Insert", systemImage: "text.insert").frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent).tint(KeyboardTheme.accent)
                .disabled(answer.isEmpty)
            }
        }
    }
}

/// The whole letters/numbers/symbols key grid, isolated behind `Equatable` so it
/// only re-renders when its *value* inputs change (the rows or the shift state).
/// Per-keystroke `@Published` churn on the model (suggestions, autocap) no longer
/// invalidates the grid — that was the sustained typing lag. `model` is a plain
/// `let` (used only to CALL methods from gesture closures), excluded from `==`.
struct KeyGridView: View, Equatable {
    let rows: [[KeyCap]]
    let shift: ShiftState
    let model: KeyboardModel

    static func == (a: KeyGridView, b: KeyGridView) -> Bool {
        a.shift == b.shift && a.rows == b.rows   // ignore `model` (identity) and closures
    }

    var body: some View {
        VStack(spacing: KeyboardTheme.rowSpacing) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                KeyRow(keys: row, shift: shift, model: model)
            }
        }
    }
}

/// The shadowed key body (fill + hairline border + shadow), keyed only on its
/// resolved fill so `.equatable()` can skip re-drawing it when a shift toggle only
/// re-cases the label. Keeping the shadow off the per-render hot path is what makes
/// shift cheap (see `KeyView.keyShape`).
struct KeyBackground: View, Equatable {
    let fill: Color

    var body: some View {
        RoundedRectangle(cornerRadius: KeyboardTheme.cornerRadius)
            .fill(fill)
            .overlay(
                RoundedRectangle(cornerRadius: KeyboardTheme.cornerRadius)
                    .stroke(KeyboardTheme.keyText.opacity(0.10), lineWidth: 0.5)
            )
            .shadow(color: KeyboardTheme.keyShadow, radius: 1.5, x: 0, y: 1)
    }
}

/// Per-key proportional width, read by `ProportionalRow`.
struct KeyWeightKey: LayoutValueKey {
    static let defaultValue: CGFloat = 1
}

/// Lays a row of keys out by their `KeyWeightKey` weight, filling the width with a
/// fixed inter-key spacing. Replaces a per-row `GeometryReader` (which forced a
/// layout re-measure on every grid render and churned the constrained keyboard
/// extension's memory) with a single, cheap layout pass.
struct ProportionalRow: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        CGSize(width: proposal.width ?? 0, height: proposal.height ?? KeyboardTheme.rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard !subviews.isEmpty else { return }
        let totalWeight = subviews.reduce(0) { $0 + $1[KeyWeightKey.self] }
        let available = bounds.width - spacing * CGFloat(max(0, subviews.count - 1))
        let unit = max(1, available / max(1, totalWeight))
        var x = bounds.minX
        for sub in subviews {
            let w = unit * sub[KeyWeightKey.self]
            sub.place(
                at: CGPoint(x: x, y: bounds.midY),
                anchor: .leading,
                proposal: ProposedViewSize(width: w, height: bounds.height)
            )
            x += w + spacing
        }
    }
}

/// One row of keys with proportional widths.
struct KeyRow: View {
    let keys: [KeyCap]
    let shift: ShiftState
    let model: KeyboardModel

    var body: some View {
        ProportionalRow(spacing: KeyboardTheme.keySpacing) {
            ForEach(keys) { key in
                KeyView(key: key, shift: shift, model: model)
                    .layoutValue(key: KeyWeightKey.self, value: key.widthWeight)
            }
        }
        .frame(height: KeyboardTheme.rowHeight)
    }
}

/// A single key: press feedback + repeat (delete) + caps-lock (shift).
struct KeyView: View {
    let key: KeyCap
    /// The shift state passed down as a value, so the key's RENDERING depends only
    /// on its inputs (`key`, `shift`) and not on observing the model — that lets the
    /// grid be skipped entirely on per-keystroke `@Published` churn. Gesture closures
    /// still CALL `model.…` at event time, which needs no view dependency.
    /// Defaults to `.off` for the emoji layer's ABC/globe keys (no shift-cased glyph).
    var shift: ShiftState = .off
    let model: KeyboardModel

    @State private var isPressed = false
    @State private var showCallout = false
    @State private var calloutIndex = 0
    @State private var calloutTimer: Timer?
    @State private var deleteTimer: Timer?
    @State private var deleteTicks = 0
    @State private var trackpadEngaged = false
    @State private var trackpadMoved = 0
    @State private var holdTimer: Timer?
    @State private var spaceSummonedAI = false
    /// The character shown in the pop-up above the key. Kept briefly after release
    /// so it's visible during fast typing (like the system keyboard).
    @State private var keyPreview: String?
    @State private var keyPreviewClear: DispatchWorkItem?
    /// Clears the pressed highlight a fraction after release so the key stays lit
    /// long enough to feel responsive (paired with the haptic).
    @State private var pressReleaseWork: DispatchWorkItem?

    private var callouts: [String] { key.callouts }
    private var showsBalloon: Bool {
        if case .character = key.kind { return true }
        return false
    }

    var body: some View {
        content
            .overlay(alignment: .top) { balloonOverlay }
            .overlay(alignment: .top) { calloutOverlay }
            .contentShape(RoundedRectangle(cornerRadius: KeyboardTheme.cornerRadius))
    }

    @ViewBuilder
    private var content: some View {
        switch key.kind {
        case .delete:
            keyShape
                .gesture(deleteGesture)
        case .space:
            keyShape
                .gesture(spaceGesture)
        case .shift:
            // A single immediate tap — NO `TapGesture(count: 2)`, which would make
            // every Shift wait ~0.35–0.5s for the double-tap to fail. Caps-lock is
            // detected manually inside `shiftTapped()` (two quick taps).
            keyShape
                .onTapGesture { KeyboardHaptics.special(); model.shiftTapped() }
        default:
            keyShape
                .gesture(pressGesture)
        }
    }

    // MARK: balloon + callout overlays

    @ViewBuilder private var balloonOverlay: some View {
        if let c = keyPreview, !showCallout {
            Text(c)
                .font(.system(size: 30))
                .foregroundColor(KeyboardTheme.keyText)
                .frame(width: 48, height: 54)
                .background(RoundedRectangle(cornerRadius: 8).fill(KeyboardTheme.keyFill)
                    .shadow(color: KeyboardTheme.keyShadow, radius: 3, y: 2))
                .offset(y: -54)
                .allowsHitTesting(false)
                .zIndex(1)
                .transition(.opacity)
        }
    }

    @ViewBuilder private var calloutOverlay: some View {
        if showCallout, !callouts.isEmpty {
            HStack(spacing: 2) {
                ForEach(Array(callouts.enumerated()), id: \.offset) { index, glyph in
                    Text(glyph)
                        .font(.system(size: 20))
                        .foregroundColor(index == calloutIndex ? .white : KeyboardTheme.keyText)
                        .frame(width: 34, height: 40)
                        .background(RoundedRectangle(cornerRadius: 6)
                            .fill(index == calloutIndex ? KeyboardTheme.accent : Color.clear))
                }
            }
            .padding(4)
            .background(RoundedRectangle(cornerRadius: 9).fill(KeyboardTheme.keyFill)
                .shadow(color: KeyboardTheme.keyShadow, radius: 4, y: 2))
            .offset(y: -54)
            .allowsHitTesting(false)
        }
    }

    private var keyShape: some View {
        // The shadowed background is an EQUATABLE subview keyed only on its resolved
        // fill. A shift toggle re-cases the label (below) but a letter key's fill is
        // shift-independent, so `.equatable()` lets SwiftUI reuse the already-drawn
        // background — the shadow is NOT re-rasterized. Only the tiny label `Text`
        // re-renders. That's what makes a full-grid shift re-render cheap instead of
        // re-drawing ~30 shadowed shapes (the slow-then-crash path).
        KeyBackground(fill: fill)
            .equatable()
            .overlay(label)   // label on top of the border so it's never clipped
            .scaleEffect(isPressed ? 0.97 : 1)
            .animation(.easeOut(duration: 0.06), value: isPressed)
    }

    // MARK: gestures

    private var pressGesture: some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { value in
                if !isPressed {
                    pressReleaseWork?.cancel()
                    isPressed = true
                    KeyboardHaptics.keyTap()
                    if showsBalloon, case .character(let c) = key.kind {
                        keyPreviewClear?.cancel()
                        keyPreview = c
                    }
                    if !callouts.isEmpty { scheduleCallout() }
                }
                if showCallout {
                    let slot: CGFloat = 36
                    let idx = Int((value.translation.width + slot / 2) / slot)
                    calloutIndex = max(0, min(callouts.count - 1, idx))
                }
            }
            .onEnded { _ in
                let picked = (showCallout && calloutIndex < callouts.count) ? callouts[calloutIndex] : nil
                reset()
                if let picked {
                    model.insertCallout(picked)
                    keyPreview = nil
                } else {
                    model.press(key)
                    lingerKeyPreview()
                }
                releasePress()
            }
    }

    private var deleteGesture: some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { _ in
                if !isPressed {
                    pressReleaseWork?.cancel()
                    isPressed = true
                    KeyboardHaptics.special()
                    model.deleteBackward()
                    startDeleteRepeat()
                }
            }
            .onEnded { _ in
                releasePress()
                stopDeleteRepeat()
            }
    }

    /// Space bar — three intents disambiguated by *movement*:
    /// • plain tap → inserts a space;
    /// • move the finger → **cursor trackpad** (caret follows horizontal drag);
    /// • hold still ~0.5s → **summons the Magican agentic surface**.
    /// Movement wins over the hold, so dragging never fires the AI (and vice-versa).
    private var spaceGesture: some Gesture {
        let charStep: CGFloat = 8        // points of drag per character
        let driftTolerance: CGFloat = 18 // stay within this to count as "held still"
        return DragGesture(minimumDistance: 0)
            .onChanged { value in
                if !isPressed {
                    pressReleaseWork?.cancel()
                    isPressed = true
                    KeyboardHaptics.keyTap()
                    spaceSummonedAI = false
                    // Held still ~0.45s (within the drift tolerance) → reveal the
                    // agentic surface. Scheduled on `.common` so it still fires while
                    // the touch is tracking (a `.default` timer would be starved).
                    let timer = Timer(timeInterval: 0.45, repeats: false) { _ in
                        if !trackpadEngaged, !model.isSensitive {
                            spaceSummonedAI = true
                            KeyboardHaptics.special()
                            model.revealAISurface()
                        }
                    }
                    RunLoop.main.add(timer, forMode: .common)
                    holdTimer = timer
                }
                // A deliberate drag (past the drift tolerance) means "trackpad", not
                // "hold" — so caret-scrubbing never trips the reveal, and vice-versa.
                if !trackpadEngaged, !spaceSummonedAI, abs(value.translation.width) > driftTolerance {
                    engageTrackpad()
                }
                if trackpadEngaged {
                    let target = Int(value.translation.width / charStep)
                    let delta = target - trackpadMoved
                    if delta != 0 {
                        model.moveCursor(by: delta)
                        trackpadMoved = target
                    }
                }
            }
            .onEnded { _ in
                holdTimer?.invalidate()
                holdTimer = nil
                let wasTrackpad = trackpadEngaged
                let summoned = spaceSummonedAI
                releasePress()
                trackpadEngaged = false
                trackpadMoved = 0
                spaceSummonedAI = false
                if wasTrackpad {
                    model.endTrackpad()
                } else if summoned {
                    // consumed by the AI reveal — no space inserted
                } else {
                    model.press(key)   // plain tap → space
                }
            }
    }

    private func engageTrackpad() {
        holdTimer?.invalidate()
        holdTimer = nil
        trackpadEngaged = true
        trackpadMoved = 0
        KeyboardHaptics.special()
    }

    private func scheduleCallout() {
        calloutTimer?.invalidate()
        calloutTimer = Timer.scheduledTimer(withTimeInterval: 0.32, repeats: false) { _ in
            calloutIndex = 0
            KeyboardHaptics.special()
            withAnimation(.easeOut(duration: 0.1)) { showCallout = true }
        }
    }

    private func reset() {
        calloutTimer?.invalidate()
        calloutTimer = nil
        showCallout = false
        calloutIndex = 0
        // `isPressed` is cleared by `releasePress()` so the highlight lingers.
    }

    /// Keep the key pop-up visible briefly after release, then fade it — so it's
    /// perceptible even on a fast tap (the system keyboard does the same).
    private func lingerKeyPreview() {
        keyPreviewClear?.cancel()
        let work = DispatchWorkItem {
            withAnimation(.easeOut(duration: 0.08)) { keyPreview = nil }
        }
        keyPreviewClear = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12, execute: work)
    }

    /// Hold the pressed highlight a fraction after release, then ease it out — so a
    /// quick tap still reads as a clear, satisfying press.
    private func releasePress() {
        pressReleaseWork?.cancel()
        let work = DispatchWorkItem {
            withAnimation(.easeOut(duration: 0.13)) { isPressed = false }
        }
        pressReleaseWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.10, execute: work)
    }

    private func startDeleteRepeat() {
        stopDeleteRepeat()
        deleteTicks = 0
        // Initial hold delay, then repeat: character-by-character at first, then
        // (after ~1s of holding) accelerate to whole-word deletion — standard iOS.
        deleteTimer = Timer.scheduledTimer(withTimeInterval: 0.45, repeats: false) { _ in
            deleteTimer = Timer.scheduledTimer(withTimeInterval: 0.09, repeats: true) { _ in
                deleteTicks += 1
                if deleteTicks < 12 {
                    model.deleteBackward()
                } else {
                    model.deleteWordBackward()
                }
            }
        }
    }

    private func stopDeleteRepeat() {
        deleteTimer?.invalidate()
        deleteTimer = nil
        deleteTicks = 0
    }

    // MARK: appearance

    private var isFunctionKey: Bool {
        switch key.kind {
        case .character, .space: return false
        default: return true
        }
    }

    private var fill: Color {
        if isPressed {
            return KeyboardTheme.keyPressed
        }
        // An engaged shift/caps key glows in the accent.
        if case .shift = key.kind, shift != .off {
            return KeyboardTheme.accentSoft
        }
        return isFunctionKey ? KeyboardTheme.functionFill : KeyboardTheme.keyFill
    }

    @ViewBuilder
    private var label: some View {
        switch key.kind {
        case .character(let c):
            Text(c).font(.system(size: 22)).foregroundColor(KeyboardTheme.keyText)
        case .space:
            if trackpadEngaged {
                Image(systemName: "arrow.left.and.right")
                    .font(.system(size: 15)).foregroundColor(KeyboardTheme.accent)
            } else {
                // Branded "Magican Bar" (parity with OpenActi's Acti Bar): a sparkle +
                // wordmark, kept faint so it reads as a subtle watermark on the key.
                HStack(spacing: 5) {
                    Image(systemName: "sparkles")
                        .font(.system(size: 12)).foregroundColor(KeyboardTheme.accent.opacity(0.5))
                    Text("Magican")
                        .font(.system(size: 14, weight: .medium)).foregroundColor(KeyboardTheme.keyText.opacity(0.32))
                }
            }
        case .ret:
            Image(systemName: "return").font(.system(size: 17)).foregroundColor(KeyboardTheme.keyText)
        case .delete:
            Image(systemName: "delete.left").font(.system(size: 18)).foregroundColor(KeyboardTheme.keyText)
        case .shift:
            Image(systemName: shift == .capsLock ? "capslock.fill" : (shift == .on ? "shift.fill" : "shift"))
                .font(.system(size: 18))
                .foregroundColor(shift != .off ? KeyboardTheme.accent : KeyboardTheme.keyText)
        case .globe:
            Image(systemName: "globe").font(.system(size: 18)).foregroundColor(KeyboardTheme.keyText)
        case .layer(let l):
            switch l {
            case .emoji:
                Image(systemName: "face.smiling").font(.system(size: 20)).foregroundColor(KeyboardTheme.keyText)
            case .letters:
                Text("ABC").font(.system(size: 15)).foregroundColor(KeyboardTheme.keyText)
            case .numbers:
                Text("123").font(.system(size: 15)).foregroundColor(KeyboardTheme.keyText)
            case .symbols:
                Text("#+=").font(.system(size: 15)).foregroundColor(KeyboardTheme.keyText)
            }
        case .dismiss:
            Image(systemName: "keyboard.chevron.compact.down").font(.system(size: 18)).foregroundColor(KeyboardTheme.keyText)
        }
    }
}
