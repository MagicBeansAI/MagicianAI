import Foundation

/// Cross-process handoff for the Share Extension → app. The extension writes
/// shared items (text / URL / image / file) into the App Group container; the app
/// drains them on launch/foreground/deep-link and routes them into chat (text/URL
/// prefill the composer; images/files stage as attachments). Foundation-only so
/// both the app and the extension can link it.
public struct SharedItem: Codable, Equatable {
    public enum Kind: String, Codable { case text, url, image, file }
    /// Optional in-app destination override. Absent = the default chat routing
    /// (composer prefill / attachment staging). The Magican Assist **Start Thinking
    /// Map** choice stamps `thinking_map` so the app seeds a new Thinking Map
    /// with the shared text instead of prefilling the chat composer; **Add to
    /// current Thinking Map** stamps `thinking_map_append` so the app appends
    /// the seed to the most-recent non-archived map instead.
    public enum Dest: String, Codable {
        case thinkingMap = "thinking_map"
        case thinkingMapAppend = "thinking_map_append"
    }
    public var id: String
    public var kind: Kind
    public var text: String?
    /// Name of the copied blob inside the inbox dir (image/file kinds).
    public var storedName: String?
    public var filename: String?
    public var mime: String?
    public var dest: Dest?
    /// Originating page URL when the share carried one — provenance for
    /// destination surfaces (e.g. the Thinking Map seed's detail).
    public var sourceURL: String?
}

public enum SharedInbox {
    public static let appGroup = "group.ai.magicbeans.magician.shared"

    private static var containerURL: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup)
    }
    private static var inboxDir: URL? {
        guard let base = containerURL else { return nil }
        let dir = base.appendingPathComponent("ShareInbox", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }
    private static var manifestURL: URL? { inboxDir?.appendingPathComponent("manifest.json") }

    // MARK: Writer (extension side)

    /// Append a text or URL item. `dest` optionally routes the item to a
    /// non-chat surface in the app; `sourceURL` carries provenance.
    public static func enqueueText(
        _ text: String,
        kind: SharedItem.Kind,
        dest: SharedItem.Dest? = nil,
        sourceURL: String? = nil
    ) {
        append(SharedItem(
            id: UUID().uuidString, kind: kind, text: text,
            dest: dest, sourceURL: sourceURL))
    }

    /// Copy a blob into the inbox and append a file/image item.
    public static func enqueueBlob(_ data: Data, filename: String, mime: String, isImage: Bool) {
        guard let dir = inboxDir else { return }
        let storedName = "\(UUID().uuidString)-\(filename)"
        try? data.write(to: dir.appendingPathComponent(storedName))
        append(SharedItem(id: UUID().uuidString, kind: isImage ? .image : .file,
                          storedName: storedName, filename: filename, mime: mime))
    }

    private static func append(_ item: SharedItem) {
        var items = readManifest()
        items.append(item)
        writeManifest(items)
    }

    // MARK: Reader (app side)

    public static var hasPending: Bool { !readManifest().isEmpty }

    /// Read all items (loading blob bytes), then clear the inbox.
    public static func drain() -> [(item: SharedItem, data: Data?)] {
        let items = readManifest()
        guard !items.isEmpty else { return [] }
        let dir = inboxDir
        let result: [(SharedItem, Data?)] = items.map { item in
            guard let stored = item.storedName, let dir = dir else { return (item, nil) }
            let data = try? Data(contentsOf: dir.appendingPathComponent(stored))
            return (item, data)
        }
        clear()
        return result
    }

    private static func clear() {
        guard let dir = inboxDir else { return }
        for item in readManifest() {
            if let stored = item.storedName {
                try? FileManager.default.removeItem(at: dir.appendingPathComponent(stored))
            }
        }
        writeManifest([])
    }

    // MARK: Manifest I/O

    private static func readManifest() -> [SharedItem] {
        guard let url = manifestURL, let data = try? Data(contentsOf: url),
              let items = try? JSONDecoder().decode([SharedItem].self, from: data) else { return [] }
        return items
    }

    private static func writeManifest(_ items: [SharedItem]) {
        guard let url = manifestURL, let data = try? JSONEncoder().encode(items) else { return }
        try? data.write(to: url)
    }
}
