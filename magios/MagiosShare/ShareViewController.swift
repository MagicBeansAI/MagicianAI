import UIKit
import UniformTypeIdentifiers
import Social

/// Share Extension: accepts images, text, URLs, and arbitrary files shared from
/// other apps, writes them into the App Group `SharedInbox`, then opens the host
/// app via `magican://share` (and also relies on the app draining the inbox on next
/// foreground, so it works even if the open is suppressed). Thin by design — no
/// networking here; the app does the upload/prefill. The separate `Magican Assist`
/// Action Extension owns contextual choices such as Tutor and Writing Help.
class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        handleShare()
    }

    private func handleShare() {
        let providers = (extensionContext?.inputItems as? [NSExtensionItem])?
            .flatMap { $0.attachments ?? [] } ?? []
        guard !providers.isEmpty else { return complete() }

        let group = DispatchGroup()
        for provider in providers {
            group.enter()
            load(provider) { group.leave() }
        }
        group.notify(queue: .main) { [weak self] in
            self?.openHostAppAndComplete()
        }
    }

    private func load(_ provider: NSItemProvider, done: @escaping () -> Void) {
        // Order matters: image → url → text → generic file.
        if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            provider.loadDataRepresentation(forTypeIdentifier: UTType.image.identifier) { data, _ in
                if let data = data {
                    SharedInbox.enqueueBlob(data, filename: "shared-image.jpg", mime: "image/jpeg", isImage: true)
                }
                done()
            }
        } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
            provider.loadItem(forTypeIdentifier: UTType.url.identifier, options: nil) { item, _ in
                if let url = item as? URL { SharedInbox.enqueueText(url.absoluteString, kind: .url) }
                done()
            }
        } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
            provider.loadItem(forTypeIdentifier: UTType.plainText.identifier, options: nil) { item, _ in
                if let text = item as? String { SharedInbox.enqueueText(text, kind: .text) }
                done()
            }
        } else {
            // Generic file — load its data + a best-effort filename/mime.
            provider.loadDataRepresentation(forTypeIdentifier: UTType.data.identifier) { data, _ in
                if let data = data {
                    let ext = provider.suggestedName ?? "shared-file"
                    SharedInbox.enqueueBlob(data, filename: ext, mime: "application/octet-stream", isImage: false)
                }
                done()
            }
        }
    }

    /// Try to foreground the host app via its URL scheme (walk the responder chain
    /// to reach UIApplication.open, since extensions have no direct handle), then
    /// finish. If the open is blocked, the app still drains the inbox next launch.
    private func openHostAppAndComplete(_ url: URL = URL(string: "magican://share")!) {
        var responder: UIResponder? = self
        while let r = responder {
            if let app = r as? UIApplication {
                app.open(url, options: [:], completionHandler: nil)
                break
            }
            responder = r.next
        }
        complete()
    }

    private func complete() {
        extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
    }
}
