import Foundation

public struct TutorOverlayHandoff: Codable, Equatable {
    public let token: String
    public let storedName: String
    public let width: Int
    public let height: Int
    public let question: String

    public init(token: String, storedName: String, width: Int, height: Int, question: String) {
        self.token = token
        self.storedName = storedName
        self.width = width
        self.height = height
        self.question = question
    }
}

public enum TutorOverlayInbox {
    public static let appGroup = SharedInbox.appGroup
    private static let pendingTokenKey = "pending_tutor_overlay_token"

    private static var store: UserDefaults {
        UserDefaults(suiteName: appGroup) ?? .standard
    }

    private static var directory: URL? {
        guard let base = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) else {
            return nil
        }
        let directory = base.appendingPathComponent("TutorOverlayInbox", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    public static func save(pngData: Data, width: Int, height: Int, question: String) -> String? {
        guard let directory else { return nil }
        let token = UUID().uuidString
        let storedName = "\(token).png"
        let handoff = TutorOverlayHandoff(
            token: token,
            storedName: storedName,
            width: width,
            height: height,
            question: question
        )
        guard let metadata = try? JSONEncoder().encode(handoff) else { return nil }
        do {
            try pngData.write(to: directory.appendingPathComponent(storedName), options: .atomic)
            try metadata.write(to: directory.appendingPathComponent("\(token).json"), options: .atomic)
            return token
        } catch {
            return nil
        }
    }

    /// Persist a Tutor request before asking iOS to foreground the containing app.
    /// Action/Share extensions are not guaranteed to be allowed to open it, so the
    /// app also claims this marker on its next ordinary foreground activation.
    public static func savePending(
        pngData: Data,
        width: Int,
        height: Int,
        question: String
    ) -> String? {
        guard let token = save(
            pngData: pngData,
            width: width,
            height: height,
            question: question
        ) else { return nil }
        store.set(token, forKey: pendingTokenKey)
        return token
    }

    public static func claimPendingToken() -> String? {
        guard let token = store.string(forKey: pendingTokenKey), !token.isEmpty else { return nil }
        store.removeObject(forKey: pendingTokenKey)
        return token
    }

    public static func clearPendingToken(ifMatching token: String) {
        guard store.string(forKey: pendingTokenKey) == token else { return }
        store.removeObject(forKey: pendingTokenKey)
    }

    public static func load(token: String, consume: Bool = true) -> (TutorOverlayHandoff, Data)? {
        guard let directory,
              let metadata = try? Data(contentsOf: directory.appendingPathComponent("\(token).json")),
              let handoff = try? JSONDecoder().decode(TutorOverlayHandoff.self, from: metadata),
              handoff.token == token,
              let data = try? Data(contentsOf: directory.appendingPathComponent(handoff.storedName)) else {
            return nil
        }
        if consume {
            try? FileManager.default.removeItem(at: directory.appendingPathComponent("\(token).json"))
            try? FileManager.default.removeItem(at: directory.appendingPathComponent(handoff.storedName))
        }
        return (handoff, data)
    }
}
