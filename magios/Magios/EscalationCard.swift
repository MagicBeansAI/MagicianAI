import SwiftUI

struct EscalationCard: View {
    let messageId: String
    let executionId: String?
    let title: String
    let question: String
    let hint: String?
    let previousAnswer: String?
    let inputType: String
    let inputTypeIsAuthoritative: Bool
    let inputSchema: ChatEscalationInputSchemaData?
    let options: [EscalationOptionData]
    let resolved: Bool
    /// Called with the canonical typed response when the operator answers.
    var onRespond: (ChatHitlSubmission, @escaping (Bool) -> Void) -> Void = { _, completion in
        completion(false)
    }
    /// Historical cards without authoritative option actions resolve through
    /// the current canonical Attention record instead of guessing old IDs.
    var onOpenCanonical: (() -> Void)?

    @ObservedObject var theme: ThemeManager
    @State private var localResolved = false
    @State private var submittingOptionID: String?
    @State private var responseText = ""
    @State private var selectedOptionID: String?
    @State private var selectedOptionIDs: Set<String> = []

    private var isResolved: Bool { resolved || localResolved }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: isResolved ? "checkmark.circle.fill" : "hand.raised.fill")
                    .foregroundColor(isResolved ? theme.successColor : theme.warningColor)
                Text(title)
                    .font(.themed(17, weight: .semibold))
                    .foregroundColor(isResolved ? theme.secondaryTextColor : theme.textColor)
                Spacer()
            }

            Text(question)
                .font(.themed(17))
                .foregroundColor(isResolved ? theme.secondaryTextColor : theme.textColor)

            if let hint = hint?.trimmingCharacters(in: .whitespacesAndNewlines),
               !hint.isEmpty {
                Text(hint)
                    .font(.themed(13))
                    .foregroundColor(theme.secondaryTextColor)
            }

            if !isResolved {
                if needsCanonicalAttentionFallback {
                    canonicalAttentionFallback.padding(.top, 4)
                } else {
                    responseControls.padding(.top, 4)
                }
            } else {
                Text("Resolved")
                    .font(.themed(12))
                    .foregroundColor(theme.secondaryTextColor)
                    .padding(.top, 4)
            }
        }
        .padding()
        .background(theme.cardColor.opacity(isResolved ? 0.58 : 1.0))
        .cornerRadius(12)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(isResolved ? theme.cardBorderColor : theme.warningColor.opacity(0.45), lineWidth: 1)
        )
        .onAppear { restorePreviousAnswerIfNeeded(previousAnswer) }
        .onChange(of: previousAnswer) { _, value in
            restorePreviousAnswerIfNeeded(value)
        }
    }

    private var needsCanonicalAttentionFallback: Bool {
        ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: inputType,
            options: options,
            allowsOther: inputSchema?.allowOther == true,
            inputTypeIsAuthoritative: inputTypeIsAuthoritative
        )
    }

    @ViewBuilder
    private var canonicalAttentionFallback: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Open this request in Attention to respond safely.")
                .font(.themed(13))
                .foregroundColor(theme.secondaryTextColor)
            if let onOpenCanonical {
                Button(action: onOpenCanonical) {
                    optionLabel("Open in Attention", selected: false, loading: false)
                }
                .buttonStyle(.plain)
            }
        }
    }

    /// The backend's classification, when the ask collects a secret. Masks
    /// the field and keeps the value exact; the type posted stays `inputType`.
    private var sensitiveKind: String? { inputSchema?.sensitive?.kind }
    private var isSensitive: Bool { inputSchema?.sensitive != nil }
    private var renderKind: String { hitlRenderKind(inputType: inputType, sensitiveKind: sensitiveKind) }

    @ViewBuilder
    private var responseControls: some View {
        switch renderKind {
        case "text", "guidance":
            VStack(spacing: 8) {
                TextField(
                    inputSchema?.placeholder ?? "Type your response…",
                    text: $responseText,
                    axis: .vertical
                )
                .lineLimit(inputSchema?.multiline == false ? 1...1 : 1...4)
                .textFieldStyle(.roundedBorder)
                submitButton(label: "Submit", key: inputType) {
                    ChatHitlResponseComposer.compose(inputType: inputType, text: responseText, sensitive: isSensitive)
                }
            }

        case "otp":
            // A one-time code, masked and offered by the keyboard when the
            // phone just received it. Posted exactly as typed.
            VStack(spacing: 8) {
                if let deadline = inputSchema?.sensitive?.collectionDeadlineMs, deadline > 0,
                   inputSchema?.sensitive?.isExpired() == true {
                    Text("This code's window has closed — ask for a fresh one.")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                } else {
                    Text("Used once, then discarded. Never shown to the assistant.")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                }
                SecureField(inputSchema?.placeholder ?? "Enter the code…", text: $responseText)
                    .textFieldStyle(.roundedBorder)
                    .textContentType(.oneTimeCode)
                    .disabled(inputSchema?.sensitive?.isExpired() == true)
                submitButton(label: "Submit", key: inputType) {
                    ChatHitlResponseComposer.compose(inputType: inputType, text: responseText, sensitive: true)
                }
            }

        case "file_path":
            VStack(alignment: .leading, spacing: 8) {
                Text(fileFieldLabel)
                    .font(.themed(12, weight: .medium))
                    .foregroundColor(theme.secondaryTextColor)
                TextField(inputSchema?.placeholder ?? "/path/to/file", text: $responseText)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled(true)
                    .textInputAutocapitalization(.never)
                submitButton(label: "Submit", key: inputType) {
                    ChatHitlResponseComposer.compose(
                        inputType: inputType,
                        text: responseText,
                        allowsMultipleFiles: inputSchema?.multiple == true
                    )
                }
            }

        case "password":
            VStack(spacing: 8) {
                if isSensitive {
                    Text("Held privately for this run. Never shown to the assistant.")
                        .font(.themed(12))
                        .foregroundColor(theme.secondaryTextColor)
                }
                SecureField(inputSchema?.placeholder ?? "Enter securely…", text: $responseText)
                    .textFieldStyle(.roundedBorder)
                    // No `.password` content type: it would invite the save-password prompt (a Keychain copy).
                submitButton(label: "Submit", key: inputType) {
                    ChatHitlResponseComposer.compose(inputType: inputType, text: responseText, sensitive: isSensitive)
                }
            }

        case "multi_choice":
            VStack(spacing: 8) {
                ForEach(options, id: \.id) { option in
                    Button {
                        if selectedOptionIDs.contains(option.id) {
                            selectedOptionIDs.remove(option.id)
                        } else {
                            selectedOptionIDs.insert(option.id)
                        }
                    } label: {
                        optionLabel(
                            option.label,
                            selected: selectedOptionIDs.contains(option.id),
                            loading: false
                        )
                    }
                    .buttonStyle(.plain)
                    .disabled(submittingOptionID != nil)
                }
                submitButton(label: "Submit", key: inputType) {
                    ChatHitlResponseComposer.compose(
                        inputType: inputType,
                        selectedIds: options.map(\.id).filter(selectedOptionIDs.contains)
                    )
                }
                .disabled(!multiChoiceSelectionIsValid)
            }

        default:
            VStack(spacing: 8) {
                if inputType == "external_action",
                   let instructions = inputSchema?.instructions?.trimmingCharacters(in: .whitespacesAndNewlines),
                   !instructions.isEmpty {
                    Text(instructions)
                        .font(.themed(15))
                        .foregroundColor(theme.textColor)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                        .background(theme.surfaceColor)
                        .cornerRadius(8)
                }
                ForEach(options, id: \.id) { option in
                    Button {
                        if option.requiresInput == true {
                            selectedOptionID = option.id
                        } else if let value = ChatHitlResponseComposer.compose(
                            inputType: inputType,
                            option: option
                        ) {
                            submit(value, key: option.id)
                        }
                    } label: {
                        optionLabel(
                            option.label,
                            selected: selectedOptionID == option.id,
                            loading: submittingOptionID == option.id
                        )
                    }
                    .buttonStyle(.plain)
                    .disabled(submittingOptionID != nil)
                }

                if inputSchema?.allowOther == true,
                   !options.contains(where: { $0.id == "other" }) {
                    Button {
                        selectedOptionID = "other"
                    } label: {
                        optionLabel(
                            "Other…",
                            selected: selectedOptionID == "other",
                            loading: submittingOptionID == "other"
                        )
                    }
                    .buttonStyle(.plain)
                    .disabled(submittingOptionID != nil)
                }

                if let selectedOptionID,
                   let option = selectedOption(withId: selectedOptionID),
                   option.requiresInput == true {
                    TextField(inputSchema?.placeholder ?? "Add details…", text: $responseText, axis: .vertical)
                        .lineLimit(1...4)
                        .textFieldStyle(.roundedBorder)
                    submitButton(label: "Submit", key: option.id) {
                        ChatHitlResponseComposer.compose(
                            inputType: inputType,
                            option: option,
                            text: responseText
                        )
                    }
                }
            }
        }
    }

    private func submitButton(
        label: String,
        key: String,
        value: @escaping () -> ChatHitlSubmission?
    ) -> some View {
        Button {
            guard let response = value() else { return }
            submit(response, key: key)
        } label: {
            optionLabel(label, selected: false, loading: submittingOptionID == key)
        }
        .buttonStyle(.plain)
        .disabled(submittingOptionID != nil || value() == nil)
    }

    private func optionLabel(_ label: String, selected: Bool, loading: Bool) -> some View {
        HStack(spacing: 7) {
            if loading {
                ProgressView().controlSize(.small)
            } else if selected {
                Image(systemName: "checkmark.circle.fill")
            }
            Text(label)
        }
        .font(.themed(15, weight: .bold))
        .frame(maxWidth: .infinity)
        .padding(10)
        .background(selected ? theme.accentColor.opacity(0.12) : theme.surfaceColor)
        .foregroundColor(theme.accentColor)
        .cornerRadius(8)
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.accentColor.opacity(selected ? 0.65 : 0.3), lineWidth: 1)
        )
    }

    private var fileFieldLabel: String {
        var components = [inputSchema?.multiple == true ? "Enter paths separated by commas" : "Enter one path"]
        if let filter = inputSchema?.filter?.trimmingCharacters(in: .whitespacesAndNewlines),
           !filter.isEmpty {
            components.append("Accepted: \(filter)")
        }
        return components.joined(separator: " · ")
    }

    private var multiChoiceSelectionIsValid: Bool {
        let count = selectedOptionIDs.count
        guard count > 0 else { return false }
        if let minimum = inputSchema?.minSelections, count < minimum { return false }
        if let maximum = inputSchema?.maxSelections, maximum > 0, count > maximum { return false }
        return true
    }

    private func selectedOption(withId id: String) -> EscalationOptionData? {
        if let option = options.first(where: { $0.id == id }) { return option }
        guard id == "other", inputSchema?.allowOther == true else { return nil }
        return EscalationOptionData(
            id: "other",
            label: "Other",
            requiresInput: true,
            action: EscalationOptionActionData(type: "respond_choice")
        )
    }

    private func submit(_ value: ChatHitlSubmission, key: String) {
        guard submittingOptionID == nil else { return }
        submittingOptionID = key
        onRespond(value) { success in
            submittingOptionID = nil
            if success { localResolved = true }
        }
    }

    private func restorePreviousAnswerIfNeeded(_ value: String?) {
        guard responseText.isEmpty, let value, !value.isEmpty else { return }
        responseText = value
    }
}
