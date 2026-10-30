import SwiftUI

/// A preference alignment hold, answered on iOS (P6.2) — the port of the
/// web's `AlignmentAskFields.svelte`.
///
/// iOS is an owner-private surface, so this reads why the act was held and
/// keeps three things visibly apart:
/// 1. what to do with **this act** (allow once, something else, cancel);
/// 2. what Magician **remembers** (change or remember a preference) — never
///    the act's answer, never preselected;
/// 3. a **reply** in the owner's own words — an instruction, not consent.
///
/// A lasting change is two steps: the first answer asks the server to read
/// the exact wording back; the second confirms that readback with its token.
struct AlignmentAskView: View {
    let item: AttentionItem
    let projection: AlignmentProjection
    @ObservedObject var viewModel: AttentionViewModel
    /// Called after an answer is posted, so the modal can close.
    let onAnswered: () -> Void
    @StateObject private var themeManager = ThemeManager.shared

    @State private var outcome: AttentionViewModel.AlignmentDetailOutcome?
    @State private var replyText = ""
    @State private var wordingText = ""
    @State private var changing = false

    private var detail: AlignmentDetail? {
        if case .ready(let detail) = outcome, detail.isPrivate { return detail }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch outcome {
            case .none:
                note("Loading why this was held…")
            case .gone:
                note("This review was already answered or replaced. Open the current one to continue.")
            case .unavailable, .ready:
                if detail == nil {
                    note("The reason could not be loaded here. Your answer still works.")
                }
            }
            if outcome != .gone {
                if let reason = detail?.retryReason, !reason.isEmpty {
                    Text(reason).font(.themed(14)).foregroundColor(themeManager.textColor)
                }
                if let detail, detail.alignment.isWordingStage || projection.isWordingStage {
                    wordingStage(detail)
                } else {
                    if let detail { reason(detail.alignment) }
                    actChoices
                    if let detail, !detail.alignment.lastingChoices.isEmpty {
                        lastingSection(detail.alignment)
                    }
                }
                if projection.freeText != false { replySection }
                if let line = AlignmentAsk.deadlineLine(
                    deadlineAtMs: detail?.alignment.deadlineAtMs ?? projection.deadlineAtMs) {
                    note(line)
                }
            }
        }
        .onAppear {
            viewModel.fetchAlignmentDetail(item) { outcome = $0 }
        }
    }

    // MARK: pieces

    private func note(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(themeManager.secondaryTextColor)
    }

    @ViewBuilder
    private func reason(_ alignment: AlignmentPrivateProjection) -> some View {
        if let explanation = alignment.explanation, !explanation.isEmpty {
            Text(explanation).font(.themed(15)).foregroundColor(themeManager.textColor)
        }
        if let act = alignment.actSummary, !act.isEmpty {
            note("Held: \(act)")
        }
        if !alignment.alsoCovers.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                note("It also needs your approval:")
                ForEach(alignment.alsoCovers) { covered in
                    Text("• \(covered.line)").font(.themed(14)).foregroundColor(themeManager.textColor)
                }
                note("Allowing it approves it too. Any other answer leaves it unapproved.")
            }
            .accessibilityIdentifier("alignment-also-covers")
        }
        ForEach(alignment.intentions) { intention in
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(intention.sourceLabel)
                    .font(.themed(11, weight: .medium))
                    .foregroundColor(themeManager.secondaryTextColor)
                Text(intention.statement).font(.themed(14)).foregroundColor(themeManager.textColor)
            }
        }
    }

    private var actChoices: some View {
        let choices: [AlignmentChoice] = detail.map { detail in
            detail.alignment.actChoices.map { AlignmentChoice(id: $0.id, label: $0.label) }
        } ?? projection.actChoices
        return VStack(spacing: 8) {
            ForEach(choices) { choice in
                // "Cancel the action", so it never reads as closing the sheet,
                // which leaves the hold waiting.
                choiceButton(choice.id == "cancel" ? "Cancel the action" : choice.label) {
                    answer(choice.id)
                }
            }
        }
    }

    @ViewBuilder
    private func lastingSection(_ alignment: AlignmentPrivateProjection) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Change what Magician remembers")
                .font(.themed(13, weight: .semibold))
                .foregroundColor(themeManager.textColor)
            ForEach(alignment.lastingChoices) { choice in
                if choice.needsWording {
                    if changing {
                        TextField("New wording", text: $wordingText, axis: .vertical)
                            .lineLimit(2...4)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityLabel("New wording for the preference")
                        HStack(spacing: 8) {
                            choiceButton("Back") { changing = false }
                            choiceButton("Read it back") { answer("change_preference", input: wordingText) }
                                .disabled(wordingText.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    } else {
                        choiceButton(choice.label) { changing = true }
                    }
                } else {
                    if choice.id == "remember_preference", let remember = alignment.remember {
                        note("“\(remember.statement)”")
                    }
                    choiceButton(choice.label) { answer(choice.id) }
                }
            }
        }
        .padding(10)
        .overlay(RoundedRectangle(cornerRadius: 10)
            .stroke(themeManager.secondaryTextColor.opacity(0.4), style: StrokeStyle(lineWidth: 1, dash: [4])))
    }

    @ViewBuilder
    private func wordingStage(_ detail: AlignmentDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if let readback = detail.alignment.readback {
                Text("Save exactly this as your preference?")
                    .font(.themed(15, weight: .semibold))
                    .foregroundColor(themeManager.textColor)
                Text(readback.wording)
                    .font(.themed(15))
                    .foregroundColor(themeManager.textColor)
                    .padding(.leading, 8)
                    .overlay(alignment: .leading) {
                        Rectangle().fill(themeManager.secondaryTextColor.opacity(0.4)).frame(width: 2)
                    }
                if let superseded = readback.superseded {
                    note("Replaces: \(superseded)")
                }
                note("This changes what Magician remembers. It does not run the held action; that is checked again afterwards.")
                choiceButton("Save this wording") { answer("confirm_wording", input: readback.token) }
            }
            choiceButton("Don't change it") { answer("keep_as_is") }
            choiceButton("Cancel the action") { answer("cancel") }
        }
    }

    private var replySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField("Or reply with what to do instead", text: $replyText, axis: .vertical)
                .lineLimit(2...4)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("Reply with what to do instead")
            choiceButton("Send reply") { answer(AlignmentAsk.replyChoiceId, input: replyText) }
                .disabled(replyText.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private func choiceButton(_ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .frame(maxWidth: .infinity)
                .padding(12)
                .foregroundColor(themeManager.textColor)
                .overlay(RoundedRectangle(cornerRadius: 10)
                    .stroke(themeManager.secondaryTextColor.opacity(0.5), lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    private func answer(_ choiceId: String, input: String? = nil) {
        guard AlignmentAsk.answerValue(choiceId: choiceId, input: input) != nil else { return }
        viewModel.submitAlignment(item, choiceId: choiceId, input: input)
        onAnswered()
    }
}
