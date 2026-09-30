import SwiftUI

/// The single-item HITL popup. Renders the input control for the item's
/// input_type and submits the matching AgenticResumeValue, or cancels (aborted).
///
/// **All eleven input types have a shape of their own here.** Five used to
/// borrow another's and lose the schema that said what was being asked, exactly
/// as they did on the web (see `docs/components/unified-ui/unified-task-panel.md`,
/// *Answering an ask*): `tool_authorization` and `sandbox_override` rendered as
/// an ordinary option list, `external_action` dropped `instructions`,
/// `file_path` dropped `multiple` and `filter`, and `confirmation` dropped
/// `destructive`. Every one of those fields was already on the wire.
struct AttentionDetailModal: View {
    let item: AttentionItem
    @ObservedObject var viewModel: AttentionViewModel
    @Environment(\.dismiss) private var dismiss
    @StateObject private var themeManager = ThemeManager.shared

    @State private var textInput = ""
    @State private var otherInput = ""
    @State private var selectedIds: Set<String> = []
    /// A choice option that requires an accompanying note (web `requires_input`) —
    /// selecting it reveals a field instead of submitting immediately.
    @State private var selectedChoiceId: String?
    /// Per-file selection for a multi-file diff_approval (defaults to all).
    @State private var selectedFilePaths: Set<String> = []
    @State private var formValues: [String: String] = [:]
    @State private var formSkipped: Set<String> = []
    /// Whether an answer (or an explicit cancel) has been posted. A secret ask
    /// dismissed any other way is cancelled on the way out — the pending
    /// operation must retire rather than wait out its window — and the typed
    /// value is cleared with the view.
    @State private var answered = false
    /// Ticks once a second while a secret ask with a deadline is shown, so the
    /// window counts down and the field closes when it ends.
    @State private var now = Date()
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let chain = item.chainLabel {
                        Text(chain.uppercased())
                            .font(.themed(11, weight: .bold))
                            .foregroundColor(themeManager.accentColor)
                    }
                    Text(item.prompt)
                        .font(.themed(17, weight: .semibold))
                        .foregroundColor(themeManager.textColor)
                    if let hint = item.hint, !hint.isEmpty, hint != item.prompt {
                        Text(hint)
                            .font(.themed(15))
                            .foregroundColor(themeManager.secondaryTextColor)
                    }

                    input

                    if let url = item.reviewURL {
                        Link(destination: url) {
                            HStack(spacing: 6) {
                                Image(systemName: "arrow.up.right.square")
                                Text(item.reviewLabel)
                            }
                            .font(.themed(14, weight: .medium))
                            .foregroundColor(themeManager.accentColor)
                        }
                    }

                    Button(action: { answered = true; viewModel.cancel(item); dismiss() }) {
                        Text("Cancel")
                            .frame(maxWidth: .infinity)
                            .padding()
                            .foregroundColor(themeManager.dangerColor)
                    }
                    .buttonStyle(.plain)
                }
                .padding()
            }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle(item.itemType.isEmpty ? "Attention" : item.itemType.capitalized)
            .navigationBarTitleDisplayMode(.inline)
            .navigationBarItems(trailing: Button("Close") { dismiss() })
        }
        .onReceive(clock) { tick in
            if item.sensitiveDeadline != nil { now = tick }
        }
        .onDisappear {
            // Closing a secret ask without answering is an answer: cancel it so
            // the run can raise a fresh challenge instead of waiting out the
            // window. Nothing typed survives the view.
            if item.isSensitive && !answered {
                answered = true
                viewModel.cancel(item, reason: "dismissed")
            }
            textInput = ""
            formValues = [:]
        }
    }

    private var secretExpired: Bool {
        guard let deadline = item.sensitiveDeadline else { return false }
        return now >= deadline
    }

    private var secretSecondsLeft: Int? {
        guard let deadline = item.sensitiveDeadline else { return nil }
        return max(0, Int(deadline.timeIntervalSince(now).rounded(.up)))
    }

    /// What will happen to the value, the window when there is one, and past
    /// the window a fresh ask instead of a field the destination would refuse.
    @ViewBuilder
    private var sensitiveBanner: some View {
        if item.sensitiveSpec != nil {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text(secretExpired
                        ? "This code's window has closed — ask for a fresh one."
                        : item.isOneTime
                            ? "Used once, then discarded. Never shown to the assistant."
                            : "Held privately for this run. Never shown to the assistant.")
                        .font(.themed(13))
                        .foregroundColor(themeManager.secondaryTextColor)
                    Spacer()
                    if let seconds = secretSecondsLeft, !secretExpired {
                        Text(seconds >= 120 ? "\(seconds / 60) min left" : "\(seconds)s left")
                            .font(.themedMono(.caption))
                            .foregroundColor(themeManager.secondaryTextColor)
                            .accessibilityLabel("Time left")
                    }
                }
                if secretExpired {
                    Button("Request a fresh code") {
                        answered = true
                        textInput = ""
                        viewModel.cancel(item, reason: "fresh_code_requested")
                        dismiss()
                    }
                    .font(.themed(14, weight: .medium))
                    .buttonStyle(.plain)
                    .foregroundColor(themeManager.accentColor)
                }
            }
            .padding(10)
            .background(themeManager.secondaryTextColor.opacity(0.08))
            .cornerRadius(10)
        }
    }

    @ViewBuilder
    private var input: some View {
        sensitiveBanner
        switch item.renderKind {
        // **A grant, not a question**, and the reason it left the `choice` arm.
        // Both of these used to render as an ordinary option list: `Deny` looked
        // identical to `Allow for This Run`, the tool or command being authorized
        // appeared only inside the sentence the backend composed around it, and a
        // sandbox escape read exactly like "which quarter?".
        case "tool_authorization", "sandbox_override":
            VStack(alignment: .leading, spacing: 12) {
                grantBlock
                // The refusal first and in the filled style, then every grant the
                // ask actually offers. A tool authorization carries three ids —
                // `allow_once`, `allow_always`, `deny` — and the middle one writes
                // the tool into the session allowlist, so collapsing them into one
                // Allow button would answer a broader question than was asked.
                Button(action: {
                    answered = true
                    viewModel.submitChoice(item, optionId: item.grantDenyOption?.id ?? "deny")
                    dismiss()
                }) {
                    Text(item.grantDenyOption?.label ?? "Deny")
                        .font(.themed(15, weight: .bold))
                        .frame(maxWidth: .infinity).padding()
                        .background(themeManager.accentColor)
                        .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                ForEach(item.grantAllowOptions) { opt in
                    Button(action: { answered = true; viewModel.submitChoice(item, optionId: opt.id); dismiss() }) {
                        Text(opt.label)
                            .font(.themed(15, weight: .medium))
                            .frame(maxWidth: .infinity).padding()
                            .foregroundColor(themeManager.dangerColor)
                            .overlay(RoundedRectangle(cornerRadius: 10)
                                .stroke(themeManager.dangerColor.opacity(0.5), lineWidth: 1))
                    }
                    .buttonStyle(.plain)
                }
            }

        case "choice":
            VStack(spacing: 8) {
                ForEach(item.options, id: \.id) { opt in
                    Button(action: {
                        if opt.requiresInput == true {
                            selectedChoiceId = opt.id       // reveal the note field; submit below
                        } else {
                            answered = true
                            viewModel.submitChoice(item, optionId: opt.id, otherValue: otherInput)
                            dismiss()
                        }
                    }) { optionLabel(opt.label, selected: selectedChoiceId == opt.id) }
                    .buttonStyle(.plain)
                }
                if item.metadata?.inputSchema?.allowOther == true {
                    TextField("Other…", text: $otherInput).textFieldStyle(.roundedBorder)
                }
                // A selected option that requires input: show the field + submit.
                if let sel = selectedChoiceId,
                   item.options.first(where: { $0.id == sel })?.requiresInput == true {
                    TextField("Add details…", text: $otherInput, axis: .vertical)
                        .lineLimit(1...4).textFieldStyle(.roundedBorder)
                    submitButton("Submit") {
                        answered = true
                        viewModel.submitChoice(item, optionId: sel, otherValue: otherInput); dismiss()
                    }
                    .disabled(otherInput.isEmpty)
                }
            }

        case "diff_approval":
            if item.diffFiles.count > 1 {
                // Per-file selection (web parity): choose which files to apply.
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(item.diffFiles) { file in
                        Button(action: {
                            if selectedFilePaths.contains(file.path) { selectedFilePaths.remove(file.path) }
                            else { selectedFilePaths.insert(file.path) }
                        }) {
                            HStack(spacing: 8) {
                                Image(systemName: selectedFilePaths.contains(file.path) ? "checkmark.square.fill" : "square")
                                    .foregroundColor(themeManager.accentColor)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(file.path).font(.themed(13, weight: .medium))
                                        .foregroundColor(themeManager.textColor).lineLimit(1).truncationMode(.middle)
                                    HStack(spacing: 8) {
                                        if let a = file.additions, a > 0 { Text("+\(a)").foregroundColor(themeManager.successColor) }
                                        if let d = file.deletions, d > 0 { Text("−\(d)").foregroundColor(themeManager.dangerColor) }
                                        Text(fileStatusLabel(file.status)).foregroundColor(themeManager.secondaryTextColor)
                                    }.font(.themed(11))
                                }
                                Spacer()
                            }
                            .padding(10).background(themeManager.surfaceColor).cornerRadius(8)
                        }
                        .buttonStyle(.plain)
                    }
                    HStack(spacing: 8) {
                        Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: false); dismiss() }) {
                            Text("Reject").frame(maxWidth: .infinity).padding()
                                .background(themeManager.dangerColor.opacity(0.12))
                                .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                        }
                        .buttonStyle(.plain)
                        Button(action: {
                            let all = selectedFilePaths.count == item.diffFiles.count
                            answered = true
                            viewModel.submitDiffApproval(item, apply: true, paths: all ? [] : Array(selectedFilePaths))
                            dismiss()
                        }) {
                            Text(selectedFilePaths.count == item.diffFiles.count ? "Apply all" : "Apply \(selectedFilePaths.count)")
                                .frame(maxWidth: .infinity).padding()
                                .background(themeManager.accentColor)
                                .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                        }
                        .buttonStyle(.plain)
                        .disabled(selectedFilePaths.isEmpty)
                    }
                }
                .onAppear { if selectedFilePaths.isEmpty { selectedFilePaths = Set(item.diffFiles.map(\.path)) } }
            } else {
                // Whole-proposal apply/reject.
                HStack(spacing: 8) {
                    Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: false); dismiss() }) {
                        Text("Reject").frame(maxWidth: .infinity).padding()
                            .background(themeManager.dangerColor.opacity(0.12))
                            .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                    }
                    .buttonStyle(.plain)
                    Button(action: { answered = true; viewModel.submitDiffApproval(item, apply: true); dismiss() }) {
                        Text("Apply").frame(maxWidth: .infinity).padding()
                            .background(themeManager.accentColor)
                            .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
                    }
                    .buttonStyle(.plain)
                }
            }

        case "file_path":
            VStack(alignment: .leading, spacing: 8) {
                // `multiple` and `filter` were both on the wire and neither reached
                // the reader, so a prompt wanting three CSVs looked exactly like one
                // wanting a config file. The separator is stated because the answer
                // is split on it.
                Text(item.pathFieldLabel)
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(themeManager.secondaryTextColor)
                TextField(item.placeholder.isEmpty ? "/path/to/file" : item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled(true)
                    .textInputAutocapitalization(.never)
                submitButton("Send") { answered = true; viewModel.submitFilePaths(item, paths: pathsFromInput); dismiss() }
                    .disabled(pathsFromInput.isEmpty)
            }

        case "external_action":
            VStack(alignment: .leading, spacing: 8) {
                // `instructions` is the whole content of this input type and was
                // read by no surface: the reader got a bare note field under a
                // prompt that assumed they already knew what to go and do.
                if let instructions = item.externalInstructions {
                    Text(instructions)
                        .font(.themed(15))
                        .foregroundColor(themeManager.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                        .background(themeManager.surfaceColor)
                        .cornerRadius(8)
                }
                TextField("Optional note…", text: $textInput, axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.roundedBorder)
                // The acknowledgement is the answer and the note is optional, so
                // this is never disabled.
                submitButton(item.externalDoneLabel) {
                    answered = true
                    viewModel.submitExternalDone(item, guidance: textInput.isEmpty ? nil : textInput); dismiss()
                }
            }

        case "multi_choice":
            VStack(spacing: 8) {
                ForEach(item.options, id: \.id) { opt in
                    Button(action: {
                        if selectedIds.contains(opt.id) { selectedIds.remove(opt.id) } else { selectedIds.insert(opt.id) }
                    }) {
                        HStack {
                            Image(systemName: selectedIds.contains(opt.id) ? "checkmark.square.fill" : "square")
                                .foregroundColor(themeManager.accentColor)
                            Text(opt.label).foregroundColor(themeManager.textColor)
                            Spacer()
                        }
                        .padding(10)
                        .background(themeManager.surfaceColor)
                        .cornerRadius(8)
                    }
                    .buttonStyle(.plain)
                }
                submitButton("Submit") {
                    answered = true
                    viewModel.submitMultiChoice(item, ids: Array(selectedIds)); dismiss()
                }
                .disabled(selectedIds.isEmpty)
            }

        case "confirmation":
            HStack(spacing: 8) {
                Button(action: { answered = true; viewModel.submitConfirmation(item, confirmed: false); dismiss() }) {
                    Text(item.denyLabel).frame(maxWidth: .infinity).padding()
                        .background(themeManager.dangerColor.opacity(0.12))
                        .foregroundColor(themeManager.dangerColor).cornerRadius(10)
                }
                .buttonStyle(.plain)
                // The backend's `destructive` flag bands the affirmative and
                // changes nothing about what is posted — a confirmed destructive
                // action and a confirmed benign one are the same response value.
                // Filled accent invites the tap; outlined does not.
                Button(action: { answered = true; viewModel.submitConfirmation(item, confirmed: true); dismiss() }) {
                    Text(item.confirmLabel).frame(maxWidth: .infinity).padding()
                        .background(item.isDestructive ? Color.clear : themeManager.accentColor)
                        .foregroundColor(item.isDestructive ? themeManager.dangerColor : themeManager.onAccentColor)
                        .cornerRadius(10)
                        .overlay(RoundedRectangle(cornerRadius: 10)
                            .stroke(item.isDestructive ? themeManager.dangerColor.opacity(0.6) : Color.clear, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }

        case "password":
            // A masked field. When the widget type is `text`/`guidance` (the
            // backend classified the wording), the value posted keeps the
            // widget's type — the pause expects it — and is never trimmed.
            VStack(spacing: 8) {
                SecureField(item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    // No `.password` content type: it would invite the save-password prompt (a Keychain copy).
                    .disabled(secretExpired)
                submitButton("Send") {
                    answered = true
                    switch item.inputType {
                    case "text": viewModel.submitText(item, textInput)
                    case "guidance": viewModel.submitGuidance(item, textInput)
                    default: viewModel.submitPassword(item, textInput)
                    }
                    dismiss()
                }
                .disabled(textInput.isEmpty || secretExpired)
            }

        case "otp":
            // A one-time code: masked, numeric-friendly, offered by the
            // keyboard when the phone just received it, posted as the exact
            // string. The wire value rides the password shape; the type is
            // what says "code".
            VStack(spacing: 8) {
                SecureField(item.placeholder == "Type your response…" ? "Enter the code…" : item.placeholder, text: $textInput)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.oneTimeCode)
                    .disabled(secretExpired)
                submitButton("Send") {
                    answered = true
                    switch item.inputType {
                    case "text": viewModel.submitText(item, textInput)
                    case "guidance": viewModel.submitGuidance(item, textInput)
                    default: viewModel.submitPassword(item, textInput)
                    }
                    dismiss()
                }
                .disabled(textInput.isEmpty || secretExpired)
            }

        case "guidance":
            VStack(spacing: 8) {
                TextField(item.placeholder, text: $textInput, axis: .vertical)
                    .lineLimit(2...6).textFieldStyle(.roundedBorder)
                submitButton("Send") { answered = true; viewModel.submitGuidance(item, textInput); dismiss() }
                    .disabled(textInput.isEmpty)
            }

        case "form":
            VStack(alignment: .leading, spacing: 12) {
                ForEach(item.formQuestions) { question in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(question.prompt.isEmpty ? "Question" : question.prompt)
                                .font(.themed(15, weight: .medium))
                                .foregroundColor(themeManager.textColor)
                            Spacer()
                            Button(formSkipped.contains(question.id) ? "Unskip" : "Skip") {
                                if formSkipped.contains(question.id) { formSkipped.remove(question.id) }
                                else { formSkipped.insert(question.id) }
                            }
                            .font(.themed(12, weight: .medium))
                            .buttonStyle(.plain)
                            .foregroundColor(themeManager.secondaryTextColor)
                        }
                        if !formSkipped.contains(question.id) {
                            let fieldKind = item.sensitiveFieldKind(question.id)
                            let binding = Binding(
                                get: { formValues[question.id] ?? "" },
                                set: { formValues[question.id] = $0 }
                            )
                            if hitlFieldIsMasked(sensitiveKind: fieldKind) {
                                SecureField(fieldKind == "otp" ? "Enter the code…" : "Kept private", text: binding)
                                    .textFieldStyle(.roundedBorder)
                                    .textContentType(fieldKind == "otp" ? .oneTimeCode : nil)
                                    .disabled(secretExpired)
                            } else {
                                TextField("Your answer", text: binding)
                                    .textFieldStyle(.roundedBorder)
                                    .textContentType(fieldKind == "login_identifier" ? .username : nil)
                                    .autocorrectionDisabled(fieldKind == "login_identifier")
                                    .textInputAutocapitalization(fieldKind == "login_identifier" ? .never : nil)
                                if fieldKind == "login_identifier" {
                                    Text("Kept private: used only to sign in, never shown to the assistant.")
                                        .font(.themed(12))
                                        .foregroundColor(themeManager.secondaryTextColor)
                                }
                            }
                        }
                    }
                }
                HStack {
                    Button("Skip all") {
                        formSkipped = Set(item.formQuestions.map(\.id))
                        submitFormAnswers()
                    }
                    .buttonStyle(.plain)
                    .foregroundColor(themeManager.secondaryTextColor)
                    .disabled(item.formQuestions.isEmpty)
                    Spacer()
                    submitButton("Submit") { submitFormAnswers() }
                        .disabled(!formCanSubmit || secretExpired)
                }
            }

        default: // "text" and any unknown type fall back to free text
            VStack(spacing: 8) {
                TextField(item.placeholder, text: $textInput, axis: .vertical)
                    .lineLimit(1...6).textFieldStyle(.roundedBorder)
                submitButton("Send") { answered = true; viewModel.submitText(item, textInput); dismiss() }
                    .disabled(textInput.isEmpty)
            }
        }
    }

    /// What is being authorized, shown verbatim.
    ///
    /// Banded and outlined, which no other field here is: a reader working
    /// through a queue must not approve a sandbox escape the way they answer
    /// "which quarter?", and the only thing stopping them is this block not
    /// looking like a question. The subject is monospaced and wraps rather than
    /// truncating — a command you can only see half of is a command you cannot
    /// judge.
    @ViewBuilder
    private var grantBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(item.grantKind == "sandbox" ? "SANDBOX OVERRIDE REQUESTED" : "TOOL AUTHORIZATION REQUESTED")
                .font(.themed(11, weight: .bold))
                .foregroundColor(themeManager.dangerColor)
            if !item.grantSubject.isEmpty {
                Text(item.grantSubject)
                    .font(.themedMono(.footnote))
                    .foregroundColor(themeManager.textColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let detail = item.grantDetail {
                Text(detail)
                    .font(.themed(13))
                    .foregroundColor(themeManager.secondaryTextColor)
                    .fixedSize(horizontal: false, vertical: true)
            }
            // Empty for shell overrides, which renders nothing rather than "no roots".
            ForEach(item.grantRoots, id: \.self) { root in
                Text("• \(root)")
                    .font(.themedMono(.caption2))
                    .foregroundColor(themeManager.secondaryTextColor)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(themeManager.dangerColor.opacity(0.08))
        .cornerRadius(10)
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(themeManager.dangerColor.opacity(0.45), lineWidth: 1))
    }

    /// The paths typed into the field.
    ///
    /// Split on commas **only when the ask allows several**, so a single path
    /// containing a comma is still one path. Blank entries are dropped, and an
    /// empty result disables the button — a path is the answer, so an empty one
    /// is not an answer, unlike a free-text response where the empty string is a
    /// thing a reader may mean.
    private var pathsFromInput: [String] {
        let trimmed = textInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard item.wantsMultiplePaths else { return trimmed.isEmpty ? [] : [trimmed] }
        return trimmed
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }

    private func optionLabel(_ label: String, selected: Bool = false) -> some View {
        Text(label)
            .font(.themed(15, weight: .bold))
            .frame(maxWidth: .infinity)
            .padding(12)
            .background(selected ? themeManager.accentColor.opacity(0.15) : themeManager.surfaceColor)
            .foregroundColor(themeManager.accentColor)
            .cornerRadius(10)
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(themeManager.accentColor.opacity(selected ? 0.8 : 0.3), lineWidth: selected ? 2 : 1))
    }

    private func fileStatusLabel(_ status: String?) -> String {
        switch status?.uppercased() {
        case "A": return "Added"
        case "D": return "Deleted"
        case "R": return "Renamed"
        default: return "Modified"
        }
    }

    private var formCanSubmit: Bool {
        let questions = item.formQuestions
        guard !questions.isEmpty else { return false }
        return questions.allSatisfy { question in
            formSkipped.contains(question.id) || !(formValues[question.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private func submitFormAnswers() {
        let answers: [[String: Any]] = item.formQuestions.map { question in
            let skipped = formSkipped.contains(question.id)
            var entry: [String: Any] = ["id": question.id, "skipped": skipped]
            if !skipped {
                entry["value"] = formValues[question.id] ?? ""
            }
            return entry
        }
        answered = true
        viewModel.submitForm(item, answers: answers)
        dismiss()
    }

    private func submitButton(_ title: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).bold()
                .frame(maxWidth: .infinity).padding()
                .background(themeManager.accentColor)
                .foregroundColor(themeManager.onAccentColor).cornerRadius(10)
        }
        .buttonStyle(.plain)
    }
}
