import Foundation

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
    public let createdAt: Int
    public let updatedAt: Int

    enum CodingKeys: String, CodingKey {
        case principal, workspace, id, name, archived
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
