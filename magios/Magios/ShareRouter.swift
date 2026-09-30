import SwiftUI

/// PURE builder: shared text (+ optional source URL) → the Thinking Map seed.
/// The seed's `thought` becomes the new map's root node label (first non-empty
/// line, bounded so cards stay legible); `detail` preserves the FULL shared text
/// prefixed with a provenance line ("Shared from …").
///
/// Provenance note: the canonical backend's owner `/operations` surface only
/// accepts owner-authored origins (`owner_spoken`/`owner_edited` — see the
/// authority matrix in `thinking_map/validation.rs`; `imported_source` is
/// reserved for the `Imported` actor, which has no client surface). The share
/// seed is an OWNER capture — the user explicitly chose "Start Thinking Map" —
/// so it is asserted `owner_spoken`, and the imported-content provenance is
/// preserved truthfully in the node's detail markdown instead.
enum ShareThinkingMapSeed {
    struct Seed: Equatable {
        let thought: String
        let detail: String
    }

    /// Max root-node label length; the full text always survives in `detail`.
    static let maxThoughtLength = 140

    static func build(text: String, sourceURL: String?) -> Seed? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let source = sourceURL?.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = trimmed.isEmpty ? (source ?? "") : trimmed
        guard !body.isEmpty else { return nil }

        // Label: the first non-empty line, bounded.
        let firstLine = body
            .split(separator: "\n", omittingEmptySubsequences: true)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty } ?? body
        let thought = firstLine.count > maxThoughtLength
            ? String(firstLine.prefix(maxThoughtLength - 1)) + "…"
            : firstLine

        let provenance: String
        if let source, !source.isEmpty {
            if let host = URL(string: source)?.host, !host.isEmpty {
                provenance = "Shared from \(host) — \(source)"
            } else {
                provenance = "Shared from \(source)"
            }
        } else {
            provenance = "Shared from another app"
        }
        return Seed(thought: thought, detail: provenance + "\n\n" + body)
    }
}

/// App-side bridge for content shared in from other apps. `App.swift` drains the
/// `SharedInbox` on launch / foreground / `magican://share` deep-link and publishes
/// here; the active `ChatView` observes and applies — text/URLs prefill the
/// composer, images/files stage as attachments. Items stamped
/// `dest = thinking_map` (the Magican Assist "Start Thinking Map" choice) are
/// diverted to `ThinkingMapRouter` instead: the app seeds a NEW canonical map
/// with the shared text (owner-captured; provenance in the seed node's detail).
/// Items stamped `dest = thinking_map_append` ("Add to current Thinking Map")
/// append the same seed to the MOST-RECENT non-archived map instead (falling
/// back to a new map when the library is empty).
final class ShareRouter: ObservableObject {
    static let shared = ShareRouter()
    private init() {}

    struct IncomingBlob: Identifiable { let id = UUID(); let data: Data; let filename: String; let mime: String }

    /// Text to append to the composer (text/URL shares), consumed by ChatView.
    @Published var pendingText: String?
    /// Blobs to stage as attachments, consumed by ChatView.
    @Published var pendingBlobs: [IncomingBlob] = []

    /// Drain the App Group inbox and publish anything found (main-actor safe).
    func drainInbox() {
        let drained = SharedInbox.drain()
        guard !drained.isEmpty else { return }
        var texts: [String] = []
        var blobs: [IncomingBlob] = []
        var mapTexts: [String] = []
        var mapSource: String?
        var appendTexts: [String] = []
        var appendSource: String?
        for (item, data) in drained {
            if item.dest == .thinkingMap {
                if let t = item.text, !t.isEmpty { mapTexts.append(t) }
                if mapSource == nil { mapSource = item.sourceURL }
                continue
            }
            if item.dest == .thinkingMapAppend {
                if let t = item.text, !t.isEmpty { appendTexts.append(t) }
                if appendSource == nil { appendSource = item.sourceURL }
                continue
            }
            switch item.kind {
            case .text, .url:
                if let t = item.text, !t.isEmpty { texts.append(t) }
            case .image, .file:
                if let data = data {
                    blobs.append(IncomingBlob(data: data, filename: item.filename ?? "shared", mime: item.mime ?? "application/octet-stream"))
                }
            }
        }
        DispatchQueue.main.async {
            if !texts.isEmpty {
                self.pendingText = (self.pendingText.map { $0 + " " } ?? "") + texts.joined(separator: " ")
            }
            self.pendingBlobs.append(contentsOf: blobs)
        }
        if !mapTexts.isEmpty || mapSource != nil {
            let joined = mapTexts.joined(separator: "\n\n")
            if let seed = ShareThinkingMapSeed.build(text: joined, sourceURL: mapSource) {
                Task { @MainActor in
                    ThinkingMapRouter.shared.present(
                        initialThought: seed.thought, detail: seed.detail)
                }
            }
        }
        // The append lane routes through the SAME seed builder (bounded label
        // + provenance-prefixed detail) but lands on the most-recent
        // non-archived map. If both lanes somehow arrive in one drain, the
        // append presents last and wins the one-shot router slot — acceptable
        // for a deliberate double-share edge case.
        if !appendTexts.isEmpty || appendSource != nil {
            let joined = appendTexts.joined(separator: "\n\n")
            if let seed = ShareThinkingMapSeed.build(text: joined, sourceURL: appendSource) {
                Task { @MainActor in
                    ThinkingMapRouter.shared.present(
                        initialThought: seed.thought, detail: seed.detail,
                        disposition: .appendToRecent)
                }
            }
        }
    }
}
