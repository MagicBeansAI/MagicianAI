import SwiftUI
import UIKit
import UniformTypeIdentifiers

final class ActionViewController: UIViewController {
    private var hostingController: UIHostingController<ContextualAssistView>?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
        let inputItems = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        Task { [weak self] in
            let content = await ShareAssistContentLoader.load(from: inputItems)
            guard let self else { return }
            self.presentAssist(content)
        }
    }

    private func presentAssist(_ content: ShareAssistContent) {
        let root = ContextualAssistView(
            content: content,
            addToChat: { [weak self] in self?.addToChat(content) },
            startThinkingMap: { [weak self] in self?.startThinkingMap(content) },
            appendToThinkingMap: { [weak self] in self?.appendToThinkingMap(content) },
            startTutor: { [weak self] question in self?.startTutor(content, question: question) ?? false },
            returnText: { [weak self] text in self?.returnTextToHost(text) },
            continueThread: { [weak self] threadID in self?.continueInMagican(threadID: threadID) },
            continueTask: { [weak self] taskID in self?.continueInMagican(taskID: taskID) },
            close: { [weak self] in self?.complete() }
        )
        let hosting = UIHostingController(rootView: root)
        hosting.view.backgroundColor = .clear
        addChild(hosting)
        hosting.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(hosting.view)
        NSLayoutConstraint.activate([
            hosting.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            hosting.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            hosting.view.topAnchor.constraint(equalTo: view.topAnchor),
            hosting.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        hosting.didMove(toParent: self)
        hostingController = hosting
    }

    private func addToChat(_ content: ShareAssistContent) {
        switch content {
        case .text(let text, let sourceURL):
            SharedInbox.enqueueText(text, kind: .text)
            if let sourceURL { SharedInbox.enqueueText(sourceURL.absoluteString, kind: .url) }
        case .webpage(let url):
            SharedInbox.enqueueText(url.absoluteString, kind: .url)
        case .image(let image):
            SharedInbox.enqueueBlob(
                image.pngData,
                filename: image.filename,
                mime: "image/png",
                isImage: true
            )
        case .unsupported:
            return
        }
        openHostAppAndComplete(URL(string: "magican://share")!)
    }

    /// Start Thinking Map: persist the shared text/URL into the App Group inbox
    /// stamped `dest = thinking_map`, then foreground the app — the app seeds a
    /// NEW map from it (owner-captured, provenance in the seed node's detail).
    /// Thin by design: no networking here, mirroring `addToChat`.
    private func startThinkingMap(_ content: ShareAssistContent) {
        switch content {
        case .text(let text, let sourceURL):
            SharedInbox.enqueueText(
                text, kind: .text,
                dest: .thinkingMap, sourceURL: sourceURL?.absoluteString)
        case .webpage(let url):
            SharedInbox.enqueueText(
                url.absoluteString, kind: .url,
                dest: .thinkingMap, sourceURL: url.absoluteString)
        case .image, .unsupported:
            return
        }
        openHostAppAndComplete(URL(string: "magican://share")!)
    }

    /// Add to current Thinking Map: same thin App-Group handoff as
    /// `startThinkingMap`, stamped `dest = thinking_map_append` — the app
    /// appends the seed to the MOST-RECENT non-archived map (falling back to
    /// a new map when the library is empty).
    private func appendToThinkingMap(_ content: ShareAssistContent) {
        switch content {
        case .text(let text, let sourceURL):
            SharedInbox.enqueueText(
                text, kind: .text,
                dest: .thinkingMapAppend, sourceURL: sourceURL?.absoluteString)
        case .webpage(let url):
            SharedInbox.enqueueText(
                url.absoluteString, kind: .url,
                dest: .thinkingMapAppend, sourceURL: url.absoluteString)
        case .image, .unsupported:
            return
        }
        openHostAppAndComplete(URL(string: "magican://share")!)
    }

    private func startTutor(_ content: ShareAssistContent, question: String) -> Bool {
        guard case .image(let image) = content,
              let token = TutorOverlayInbox.savePending(
                pngData: image.pngData,
                width: image.width,
                height: image.height,
                question: question.trimmingCharacters(in: .whitespacesAndNewlines)
              ) else { return false }
        openHostAppAndComplete(URL(string: "magican://tutor-overlay?token=\(token)")!)
        return true
    }

    private func returnTextToHost(_ text: String) {
        let item = NSExtensionItem()
        item.attributedContentText = NSAttributedString(string: text)
        item.attachments = [NSItemProvider(object: text as NSString)]
        extensionContext?.completeRequest(returningItems: [item], completionHandler: nil)
    }

    private func continueInMagican(threadID: String) {
        SharedActions.setPendingThread(threadID)
        let encoded = threadID.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? threadID
        openHostAppAndComplete(URL(string: "magican://thread/\(encoded)")!)
    }

    private func continueInMagican(taskID: String) {
        SharedActions.setPendingTask(taskID)
        let encoded = taskID.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? taskID
        openHostAppAndComplete(URL(string: "magican://task/\(encoded)")!)
    }

    /// Extension points may suppress foregrounding the containing app. The App
    /// Group payload is always persisted before this best-effort responder-chain
    /// request, and the app drains it on its next normal activation.
    private func openHostAppAndComplete(_ url: URL) {
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(url, options: [:], completionHandler: nil)
                break
            }
            responder = current.next
        }
        complete()
    }

    private func complete() {
        extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
    }
}
