import Foundation
import Combine

public enum ChatHistoryLane: String, CaseIterable, Codable, Identifiable {
    case personal
    case automated

    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .personal: "Personal"
        case .automated: "Automated"
        }
    }
}

public struct UiThreadRecord: Codable, Equatable, Identifiable {
    public let principal: String
    public let workspace: String
    public let id: String
    public let name: String
    public let archived: Bool
    public let sortOrder: Int
    public let memorySummary: String?
    public let memoryUpdatedAt: Int?
    public var historyLane: String? = nil
    /// The URL key, which follows the name; `id` never changes.
    public var slug: String? = nil
    public let createdAt: Int
    public let updatedAt: Int

    enum CodingKeys: String, CodingKey {
        case principal, workspace, id, name, archived, slug
        case sortOrder = "sort_order"
        case memorySummary = "memory_summary"
        case memoryUpdatedAt = "memory_updated_at"
        case historyLane = "history_lane"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

public struct UiThreadListResponse: Codable {
    public let threads: [UiThreadRecord]
    public var total: Int? = nil
    public var limit: Int? = nil
    public var offset: Int? = nil
}

public struct ConcurrentSessionOrigin: Codable, Equatable {
    public let kind: String
    public let parentSessionId: String?
    enum CodingKeys: String, CodingKey { case kind; case parentSessionId = "parent_session_id" }
}

public struct ChatSession: Codable, Equatable, Identifiable {
    public var internalVoice: ConcurrentSessionOrigin? = nil
    public let id: String
    public let principal: String
    public let workspace: String
    public let agentId: String
    public let uiThreadId: String
    public let title: String?
    public let status: String
    public var historyLane: String? = nil
    public var isDefaultSession: Bool? = nil
    public let createdAt: Int
    public let updatedAt: Int

    enum CodingKeys: String, CodingKey {
        case id, principal, workspace, title, status
        case internalVoice = "internal_voice"
        case agentId = "agent_id"
        case uiThreadId = "ui_thread_id"
        case historyLane = "history_lane"
        case isDefaultSession = "is_default_session"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }
}

public struct ChatSessionListResponse: Codable {
    public let sessions: [ChatSession]
    public var total: Int? = nil
    public var limit: Int? = nil
    public var offset: Int? = nil
}

public struct ChatSessionDetailResponse: Codable {
    public let session: ChatSession
    public let messages: [ChatMessageRawData]
    /// True when messages older than the first embedded one exist. Absent
    /// on servers that predate it; see `ChatViewModel.initialHasMore`.
    public var hasMore: Bool? = nil

    enum CodingKeys: String, CodingKey {
        case session, messages
        case hasMore = "has_more"
    }
}

public struct ChatSessionEnvelope: Codable {
    public let session: ChatSession
}

public struct HistorySearchItem: Codable, Equatable, Identifiable {
    public let kind: String
    public let historyLane: ChatHistoryLane
    public let session: ChatSession?
    public let thread: UiThreadRecord?

    public var id: String {
        if let session { return "session:\(session.id)" }
        if let thread { return "thread:\(thread.id)" }
        return "invalid:\(kind)"
    }

    enum CodingKeys: String, CodingKey {
        case kind, session, thread
        case historyLane = "history_lane"
    }
}

public struct HistorySearchResponse: Codable {
    public let items: [HistorySearchItem]
    public let total: Int
    public let limit: Int
    public let offset: Int
}

/// The threads this machine has shown the app, for two jobs:
///
/// - a link's `/t/<key>` may be the thread's current slug, an old slug or its
///   id; `resolve` maps it to the canonical id (cached; one
///   `GET /api/magician/v2/ui-threads/{key}` on a miss; the key itself when that
///   fails), and every local lookup keeps using the id;
/// - a thread chip shows the thread's name (`label`), not its id.
///
/// Scoped to the paired machine: a different origin starts empty. Main thread.
final class ThreadDirectory: ObservableObject {
    static let shared = ThreadDirectory()

    struct Entry: Equatable {
        let id: String
        let name: String
        let slug: String?
    }

    /// Known threads by id.
    @Published private(set) var entries: [String: Entry] = [:]
    private var aliases: [String: String] = [:]
    private var origin: String?

    init() {}

    /// Forgets everything when `origin` is not the machine the entries came from.
    func use(origin value: String) {
        guard value != origin else { return }
        origin = value
        entries = [:]
        aliases = [:]
    }

    func remember(_ threads: [UiThreadRecord], origin value: String = MagicianAccess.baseURL.absoluteString) {
        let apply = { [weak self] in
            guard let self else { return }
            self.use(origin: value)
            var next = self.entries
            for thread in threads where !thread.id.isEmpty {
                next[thread.id] = Entry(id: thread.id, name: thread.name, slug: thread.slug)
                if let slug = thread.slug, !slug.isEmpty { self.aliases[slug] = thread.id }
            }
            if next != self.entries { self.entries = next }
        }
        if Thread.isMainThread { apply() } else { DispatchQueue.main.async(execute: apply) }
    }

    /// The canonical id for `key` when already known: an id, then a slug or alias.
    func canonicalId(_ key: String) -> String? {
        if entries[key] != nil { return key }
        return aliases[key] ?? entries.values.first(where: { $0.slug == key })?.id
    }

    /// `#<name>`, or `#<id>` for a thread the app has not listed.
    func label(_ id: String?) -> String {
        let key = (id ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let resolved = key.isEmpty ? "general" : key
        let name = entries[resolved]?.name.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return "#" + (name.isEmpty ? resolved : name)
    }

    /// The thread object in a `{thread: {...}}` envelope or a bare record.
    static func threadObject(_ data: Data?) -> [String: Any]? {
        guard let data, let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let thread = root["thread"] as? [String: Any] ?? root
        guard let id = thread["id"] as? String, !id.isEmpty else { return nil }
        return thread
    }

    /// Calls `completion` on the main thread with the canonical id for a
    /// link's thread `key`.
    func resolve(_ key: String, session: URLSession = .shared, completion: @escaping (String) -> Void) {
        let clean = key.trimmingCharacters(in: .whitespacesAndNewlines)
        let origin = MagicianAccess.baseURL.absoluteString
        use(origin: origin)
        guard !clean.isEmpty else { return completion(key) }
        if let known = canonicalId(clean) { return completion(known) }
        guard let url = URL(string: origin + "/api/magician/v2/ui-threads/" + WorkAPIClient.segment(clean)) else {
            return completion(clean)
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 10
        MagicianAccess.authorize(&request)
        session.dataTask(with: request) { [weak self] data, response, _ in
            let ok = ((response as? HTTPURLResponse)?.statusCode).map { (200..<300).contains($0) } ?? false
            let thread = ok ? Self.threadObject(data) : nil
            DispatchQueue.main.async {
                guard let self, let thread, let id = thread["id"] as? String else { return completion(clean) }
                guard self.origin == origin else { return completion(id) }
                self.aliases[clean] = id
                if let slug = thread["slug"] as? String, !slug.isEmpty { self.aliases[slug] = id }
                if let name = thread["name"] as? String {
                    self.entries[id] = Entry(id: id, name: name, slug: thread["slug"] as? String)
                }
                completion(id)
            }
        }.resume()
    }
}
