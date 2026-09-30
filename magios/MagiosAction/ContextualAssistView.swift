import SwiftUI
import UIKit

struct ContextualAssistView: View {
    private enum Screen: Equatable {
        case choices
        case writing
        case recognizing
        case processing(ContextualAssistAction)
        case result(ContextualAssistResponse)
        case webpageQuestion
        case webpageProcessing(WebpageAssistOperation)
        case webpageResult(taskID: String)
        case error(String)
    }

    let content: ShareAssistContent
    let addToChat: () -> Void
    let startThinkingMap: () -> Void
    let appendToThinkingMap: () -> Void
    let startTutor: (String) -> Bool
    let returnText: (String) -> Void
    let continueThread: (String) -> Void
    let continueTask: (String) -> Void
    let close: () -> Void

    @State private var screen: Screen = .choices
    @State private var writingText = ""
    @State private var guidance = ""
    @State private var tutorQuestion = ""
    @State private var webpageQuestion = ""
    @State private var lastAction: ContextualAssistAction?
    @State private var requestTask: Task<Void, Never>?
    @State private var copied = false
    @State private var sessionKey = "contextual-writing:ios:\(UUID().uuidString.lowercased())"

    private let accent = Color(red: 0.42, green: 0.31, blue: 0.94)

    init(
        content: ShareAssistContent,
        addToChat: @escaping () -> Void,
        startThinkingMap: @escaping () -> Void,
        appendToThinkingMap: @escaping () -> Void,
        startTutor: @escaping (String) -> Bool,
        returnText: @escaping (String) -> Void,
        continueThread: @escaping (String) -> Void,
        continueTask: @escaping (String) -> Void,
        close: @escaping () -> Void
    ) {
        self.content = content
        self.addToChat = addToChat
        self.startThinkingMap = startThinkingMap
        self.appendToThinkingMap = appendToThinkingMap
        self.startTutor = startTutor
        self.returnText = returnText
        self.continueThread = continueThread
        self.continueTask = continueTask
        self.close = close
        if case .text(let text, _) = content {
            _writingText = State(initialValue: text)
        }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 18) {
                    header
                    switch screen {
                    case .choices: choices
                    case .writing: writingMenu
                    case .recognizing: progress(title: "Finding text", detail: "Recognition stays on this iPhone.")
                    case .processing(let action):
                        progress(title: action.label, detail: "Sam is working with the shared text.")
                    case .result(let response): result(response)
                    case .webpageQuestion: webpageQuestionView
                    case .webpageProcessing(let operation):
                        progress(title: operation.label, detail: "Sam is starting this as a direct background task.")
                    case .webpageResult(let taskID):
                        webpageResult(taskID: taskID)
                    case .error(let message): errorView(message)
                    }
                }
                .frame(maxWidth: 620)
                .padding(.horizontal, 20)
                .padding(.bottom, 28)
                .frame(maxWidth: .infinity)
            }
            .background(Color(uiColor: .systemGroupedBackground).ignoresSafeArea())
            .toolbar(.hidden, for: .navigationBar)
        }
        .tint(accent)
        .onDisappear { requestTask?.cancel() }
    }

    private var header: some View {
        HStack(spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(accent.gradient)
                Image(systemName: "wand.and.stars")
                    .font(.system(size: 19, weight: .semibold))
                    .foregroundStyle(.white)
            }
            .frame(width: 42, height: 42)

            VStack(alignment: .leading, spacing: 2) {
                Text("Magican Assist")
                    .font(.headline)
                Text(contentSubtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(action: close) {
                Image(systemName: "xmark")
                    .font(.system(size: 14, weight: .bold))
                    .frame(width: 36, height: 36)
                    .background(.thinMaterial, in: Circle())
            }
            .accessibilityLabel("Close Magican Assist")
        }
        .padding(.top, 12)
    }

    @ViewBuilder
    private var choices: some View {
        previewCard

        switch content {
        case .text:
            optionButton(
                title: "Writing Help",
                detail: "Rewrite, summarize, clarify, or continue",
                systemImage: "pencil.and.outline",
                primary: true
            ) { screen = .writing }
            optionButton(
                title: "Start Thinking Map",
                detail: "Grow this thought into a live idea map",
                systemImage: "point.3.connected.trianglepath.dotted"
            ) { startThinkingMap() }
            optionButton(
                title: "Add to current Thinking Map",
                detail: "Append this to your latest idea map",
                systemImage: "plus.diamond"
            ) { appendToThinkingMap() }
            optionButton(
                title: "Add to Chat",
                detail: "Continue with Sam in a full conversation",
                systemImage: "bubble.left.and.bubble.right"
            ) { addToChat() }

        case .image:
            VStack(alignment: .leading, spacing: 8) {
                Text("Optional question for Tutor")
                    .font(.subheadline.weight(.semibold))
                TextField("What should Tutor explain?", text: $tutorQuestion, axis: .vertical)
                    .lineLimit(2...4)
                    .textFieldStyle(.roundedBorder)
            }
            optionButton(
                title: "Start Tutor",
                detail: "Explain visually with narration and drawing",
                systemImage: "graduationcap.fill",
                primary: true
            ) {
                if !startTutor(tutorQuestion) {
                    screen = .error("The image could not be prepared for Tutor.")
                }
            }
            optionButton(
                title: "Writing Help",
                detail: "Recognize text in the image, then improve it",
                systemImage: "text.viewfinder"
            ) { recognizeImageText() }
            optionButton(
                title: "Add to Chat",
                detail: "Attach the image to a conversation with Sam",
                systemImage: "bubble.left.and.bubble.right"
            ) { addToChat() }

        case .webpage:
            optionButton(
                title: "Summarize Page",
                detail: "Read the page and capture its key points",
                systemImage: "doc.text.magnifyingglass",
                primary: true
            ) { runWebpageAction(.summarizePage) }
            optionButton(
                title: "Ask Sam",
                detail: "Ask a specific question about this page",
                systemImage: "sparkles"
            ) { screen = .webpageQuestion }
            optionButton(
                title: "Start Thinking Map",
                detail: "Capture this link as the seed of an idea map",
                systemImage: "point.3.connected.trianglepath.dotted"
            ) { startThinkingMap() }
            optionButton(
                title: "Add to current Thinking Map",
                detail: "Append this link to your latest idea map",
                systemImage: "plus.diamond"
            ) { appendToThinkingMap() }
            optionButton(
                title: "Add to Chat",
                detail: "Attach the full URL to a conversation",
                systemImage: "bubble.left.and.bubble.right"
            ) { addToChat() }

        case .unsupported:
            ContentUnavailableView(
                "Nothing Magican Assist can use",
                systemImage: "square.and.arrow.up.trianglebadge.exclamationmark",
                description: Text("Share one text selection, screenshot, photo, or webpage URL.")
            )
        }

        if content != .unsupported {
            Label("Text and webpage actions use your configured backend. Image text recognition stays on-device.", systemImage: "hand.raised")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.top, 4)
        }
    }

    private var webpageQuestionView: some View {
        VStack(spacing: 18) {
            previewCard

            VStack(alignment: .leading, spacing: 7) {
                Text("What should Sam help with?")
                    .font(.title3.weight(.bold))
                TextField(
                    "For example: What are the main claims and are they supported?",
                    text: $webpageQuestion,
                    axis: .vertical
                )
                .lineLimit(3...7)
                .textFieldStyle(.roundedBorder)
                .submitLabel(.go)
                .onSubmit {
                    if !webpageQuestion.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        runWebpageAction(.askSam)
                    }
                }
            }

            Button {
                runWebpageAction(.askSam)
            } label: {
                Label("Ask About This Page", systemImage: "sparkles")
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)
            .disabled(webpageQuestion.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

            Button("Back") { screen = .choices }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .frame(minHeight: 44)
        }
    }

    @ViewBuilder
    private var previewCard: some View {
        Group {
            switch content {
            case .text(let text, _):
                VStack(alignment: .leading, spacing: 8) {
                    Label("Shared text", systemImage: "text.quote")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Text(text)
                        .font(.body)
                        .lineLimit(5)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            case .image(let image):
                if let uiImage = UIImage(data: image.pngData) {
                    Image(uiImage: uiImage)
                        .resizable()
                        .scaledToFit()
                        .frame(maxHeight: 260)
                        .frame(maxWidth: .infinity)
                        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                        .accessibilityLabel("Shared screenshot or photo")
                }
            case .webpage(let url):
                VStack(alignment: .leading, spacing: 8) {
                    Label(url.host ?? "Webpage", systemImage: "safari")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Text(url.absoluteString)
                        .font(.footnote.monospaced())
                        .lineLimit(4)
                        .textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            case .unsupported:
                EmptyView()
            }
        }
        .padding(14)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18, style: .continuous))
    }

    private var writingMenu: some View {
        VStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Work with this text")
                    .font(.title3.weight(.bold))
                Text(writingText)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 16))

            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 12) {
                ForEach(ContextualAssistAction.allCases) { action in
                    Button {
                        runWritingAction(action)
                    } label: {
                        VStack(spacing: 8) {
                            Image(systemName: action.systemImage)
                                .font(.title3.weight(.semibold))
                            Text(action.label)
                                .font(.subheadline.weight(.semibold))
                        }
                        .frame(maxWidth: .infinity, minHeight: 76)
                    }
                    .buttonStyle(.bordered)
                    .buttonBorderShape(.roundedRectangle(radius: 14))
                }
            }

            VStack(alignment: .leading, spacing: 7) {
                Text("Add instructions")
                    .font(.subheadline.weight(.semibold))
                TextField("For example: friendly and concise", text: $guidance, axis: .vertical)
                    .lineLimit(2...4)
                    .textFieldStyle(.roundedBorder)
            }

            Button("Back") { screen = .choices }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .frame(minHeight: 44)
        }
    }

    private func progress(title: String, detail: String) -> some View {
        VStack(spacing: 18) {
            ProgressView()
                .controlSize(.large)
            Text(title)
                .font(.title3.weight(.bold))
            Text(detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Cancel", role: .cancel) {
                requestTask?.cancel()
                screen = writingText.isEmpty ? .choices : .writing
            }
            .frame(minHeight: 44)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 54)
    }

    private func result(_ response: ContextualAssistResponse) -> some View {
        VStack(spacing: 16) {
            if let draft = response.draftText, !draft.isEmpty {
                VStack(alignment: .leading, spacing: 10) {
                    Label("Draft ready", systemImage: "checkmark.circle.fill")
                        .font(.headline)
                        .foregroundStyle(.green)
                    Text(draft)
                        .font(.body)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(16)
                .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 18))

                Button {
                    returnText(draft)
                } label: {
                    Label("Use Draft", systemImage: "checkmark")
                        .frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.borderedProminent)

                Button {
                    UIPasteboard.general.string = draft
                    copied = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) { close() }
                } label: {
                    Label(copied ? "Copied" : "Copy & Close", systemImage: copied ? "checkmark" : "doc.on.doc")
                        .frame(maxWidth: .infinity, minHeight: 44)
                }
                .buttonStyle(.bordered)
            } else {
                ContentUnavailableView(
                    "Saved to Magican",
                    systemImage: "clock.badge.checkmark",
                    description: Text("Sam accepted the writing request. Continue in Magican to follow its progress.")
                )
            }

            if let action = lastAction {
                Button("Regenerate") { runWritingAction(action) }
                    .frame(minHeight: 44)
            }
            if let threadID = response.threadID, !threadID.isEmpty {
                Button("Continue in Magican") { continueThread(threadID) }
                    .frame(minHeight: 44)
            }
            Button("Try another action") { screen = .writing }
                .foregroundStyle(.secondary)
                .frame(minHeight: 44)
        }
    }

    private func webpageResult(taskID: String) -> some View {
        VStack(spacing: 16) {
            ContentUnavailableView(
                "Working",
                systemImage: "clock.arrow.circlepath",
                description: Text("Sam is reading the page in the background. This can take a few minutes; you can close this sheet safely.")
            )

            Button {
                continueTask(taskID)
            } label: {
                Label("Check Status in Magican", systemImage: "arrow.up.forward.app")
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(.borderedProminent)

            Button("Close", action: close)
                .foregroundStyle(.secondary)
                .frame(minHeight: 44)
        }
    }

    private func errorView(_ message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.largeTitle)
                .foregroundStyle(.orange)
            Text("Magican Assist could not finish")
                .font(.title3.weight(.bold))
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Try Again") {
                screen = writingText.isEmpty ? .choices : .writing
            }
            .buttonStyle(.borderedProminent)
            Button("Close", action: close)
                .foregroundStyle(.secondary)
                .frame(minHeight: 44)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 36)
    }

    private func optionButton(
        title: String,
        detail: String,
        systemImage: String,
        primary: Bool = false,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: systemImage)
                    .font(.system(size: 21, weight: .semibold))
                    .frame(width: 34)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title).font(.headline)
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(primary ? Color.white.opacity(0.82) : Color(uiColor: .secondaryLabel))
                        .multilineTextAlignment(.leading)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(primary ? Color.white.opacity(0.75) : Color(uiColor: .tertiaryLabel))
            }
            .padding(.horizontal, 15)
            .frame(maxWidth: .infinity, minHeight: 68)
        }
        .buttonStyle(primary ? AnyAssistButtonStyle(FilledAssistButtonStyle(accent: accent))
                             : AnyAssistButtonStyle(OutlinedAssistButtonStyle()))
    }

    private var contentSubtitle: String {
        switch content {
        case .text: return "Shared text"
        case .image: return "Screenshot or photo"
        case .webpage: return "Webpage"
        case .unsupported: return "Shared content"
        }
    }

    private var sourceURL: URL? {
        if case .text(_, let url) = content { return url }
        return nil
    }

    private func recognizeImageText() {
        guard case .image(let image) = content else { return }
        screen = .recognizing
        requestTask?.cancel()
        requestTask = Task {
            do {
                let text = try await VisionAssistTextRecognizer.recognize(image)
                guard !Task.isCancelled else { return }
                writingText = String(text.prefix(ContextualAssistRequest.maximumTextCharacters))
                screen = .writing
            } catch is CancellationError {
                screen = .choices
            } catch {
                screen = .error(error.localizedDescription)
            }
        }
    }

    private func runWritingAction(_ action: ContextualAssistAction) {
        guard !writingText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            screen = .error("There is no text to work with.")
            return
        }
        lastAction = action
        copied = false
        screen = .processing(action)
        requestTask?.cancel()
        let body = ContextualAssistRequest.make(
            action: action,
            text: writingText,
            guidance: guidance,
            sourceURL: sourceURL,
            sessionKey: sessionKey
        )
        requestTask = Task {
            do {
                let response = try await ContextualAssistClient().run(body)
                guard !Task.isCancelled else { return }
                if ["draft_ready", "queued", "accepted"].contains(response.status) {
                    if response.status != "draft_ready", let threadID = response.threadID {
                        SharedActions.setPendingThread(threadID)
                    }
                    screen = .result(response)
                } else {
                    screen = .error("Sam returned an unsupported status: \(response.status).")
                }
            } catch is CancellationError {
                screen = .writing
            } catch ContextualAssistClientError.cancelled {
                screen = .writing
            } catch {
                screen = .error(error.localizedDescription)
            }
        }
    }

    private func runWebpageAction(_ operation: WebpageAssistOperation) {
        guard case .webpage(let url) = content else {
            screen = .error("There is no webpage URL to use.")
            return
        }
        screen = .webpageProcessing(operation)
        requestTask?.cancel()
        requestTask = Task {
            let client = WebpageAssistClient()
            do {
                let task = try await client.start(
                    operation: operation,
                    url: url,
                    guidance: operation == .askSam ? webpageQuestion : nil
                )

                // Persist as the first post-acceptance action—even if the user
                // cancelled while the server response was in flight.
                SharedActions.setPendingTask(task.id)
                guard !Task.isCancelled else { return }
                screen = .webpageResult(taskID: task.id)
            } catch WebpageAssistClientError.cancelled {
                screen = operation == .askSam ? .webpageQuestion : .choices
            } catch {
                screen = .error(error.localizedDescription)
            }
        }
    }

}

private struct FilledAssistButtonStyle: ButtonStyle {
    let accent: Color
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.white)
            .background(accent.opacity(configuration.isPressed ? 0.78 : 1), in: RoundedRectangle(cornerRadius: 17, style: .continuous))
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
    }
}

private struct OutlinedAssistButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(.primary)
            .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 17, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 17, style: .continuous)
                    .stroke(Color.secondary.opacity(0.14), lineWidth: 1)
            }
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
    }
}

private struct AnyAssistButtonStyle: ButtonStyle {
    private let make: (Configuration) -> AnyView
    init<S: ButtonStyle>(_ style: S) { make = { AnyView(style.makeBody(configuration: $0)) } }
    func makeBody(configuration: Configuration) -> some View { make(configuration) }
}
