import Foundation

/// The one small, privacy-safe projection shared by the app and widget
/// extension. It is deliberately reduced from Today rather than becoming a
/// second server-owned dashboard model.
struct MagicanGlanceSnapshot: Codable, Equatable, Sendable {
    enum Focus: String, Codable, Sendable {
        case needsYou = "needs_you"
        case activeWork = "active_work"
        case ready
    }

    let generatedAt: Int64
    let focus: Focus
    let title: String
    let subtitle: String
    let needsYouCount: Int
    let activeWorkCount: Int
    let taskID: String?

    init(
        generatedAt: Int64,
        focus: Focus,
        title: String,
        subtitle: String,
        needsYouCount: Int,
        activeWorkCount: Int,
        taskID: String? = nil
    ) {
        self.generatedAt = generatedAt
        self.focus = focus
        self.title = title
        self.subtitle = subtitle
        self.needsYouCount = max(0, needsYouCount)
        self.activeWorkCount = max(0, activeWorkCount)
        self.taskID = taskID?.trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty
    }

    static var ready: Self {
        Self(
            generatedAt: 0,
            focus: .ready,
            title: "Ready when you are",
            subtitle: "Talk to Magican",
            needsYouCount: 0,
            activeWorkCount: 0
        )
    }

    var destinationURL: URL {
        switch focus {
        case .needsYou:
            return URL(string: "magican://attention")!
        case .activeWork:
            guard let taskID,
                  let encoded = taskID.addingPercentEncoding(withAllowedCharacters: .magicanPathSegment)
            else { return URL(string: "magican://today?tab=active_work")! }
            return URL(string: "magican://task/\(encoded)")!
        case .ready:
            return URL(string: "magican://voice")!
        }
    }

    func hasSameVisibleContent(as other: Self) -> Bool {
        focus == other.focus
            && title == other.title
            && subtitle == other.subtitle
            && needsYouCount == other.needsYouCount
            && activeWorkCount == other.activeWorkCount
            && taskID == other.taskID
    }

    /// Reduce the already-canonical Today response. Needs You wins because a
    /// blocked run needs a person; active work wins over the idle Talk state.
    static func reducingToday(_ data: Data) throws -> Self {
        let value = try JSONDecoder().decode(TodayEnvelope.self, from: data)
        if value.counts.needsYou > 0 {
            let first = value.sections.needsYou.first
            return Self(
                generatedAt: value.generatedAt,
                focus: .needsYou,
                title: "\(value.counts.needsYou) need\(value.counts.needsYou == 1 ? "s" : "") you",
                subtitle: safeLine(first?.title) ?? "Open Attention",
                needsYouCount: value.counts.needsYou,
                activeWorkCount: value.counts.activeWork
            )
        }
        if value.counts.activeWork > 0 {
            let first = value.sections.activeWork.first
            return Self(
                generatedAt: value.generatedAt,
                focus: .activeWork,
                title: safeLine(first?.title) ?? "Work in progress",
                subtitle: safeLine(first?.reason) ?? safeLine(first?.status) ?? "Working…",
                needsYouCount: value.counts.needsYou,
                activeWorkCount: value.counts.activeWork,
                taskID: first?.taskID
            )
        }
        return Self(
            generatedAt: value.generatedAt,
            focus: .ready,
            title: "Ready when you are",
            subtitle: "Talk to Magican",
            needsYouCount: 0,
            activeWorkCount: 0
        )
    }

    /// A lock-screen/widget line is not a transcript surface. Bound both
    /// control characters and length before it crosses into another process.
    private static func safeLine(_ raw: String?) -> String? {
        let collapsed = raw?
            .components(separatedBy: .whitespacesAndNewlines)
            .filter { !$0.isEmpty }
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard let collapsed, !collapsed.isEmpty else { return nil }
        return String(collapsed.prefix(96))
    }
}

enum MagicanGlanceCache {
    private static let key = "magicanGlanceSnapshotV1"
    private static let filename = "magican-glance-snapshot-v1.json"
    private static let lock = NSLock()

    /// Production app/WidgetKit reads use an App Group file coordinated across
    /// processes. An `NSLock` around shared `UserDefaults` only serializes one
    /// process and still permits a slow extension write to replace a newer app
    /// projection.
    static func load() -> MagicanGlanceSnapshot? {
        lock.lock()
        defer { lock.unlock() }
        guard let url = cacheURL else { return loadUnlocked(store: MagicianAccess.store) }
        var result: MagicanGlanceSnapshot?
        var coordinationError: NSError?
        NSFileCoordinator(filePresenter: nil).coordinate(
            readingItemAt: url,
            options: [],
            error: &coordinationError
        ) { coordinatedURL in
            result = loadFileUnlocked(at: coordinatedURL)
        }
        // Preserve the pre-file cache through one upgrade/first refresh.
        return result ?? loadUnlocked(store: MagicianAccess.store)
    }

    /// Injectable storage remains available for deterministic unit coverage.
    static func load(store: UserDefaults) -> MagicanGlanceSnapshot? {
        lock.lock()
        defer { lock.unlock() }
        return loadUnlocked(store: store)
    }

    private static func loadUnlocked(store: UserDefaults) -> MagicanGlanceSnapshot? {
        guard let data = store.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(MagicanGlanceSnapshot.self, from: data)
    }

    @discardableResult
    static func save(_ snapshot: MagicanGlanceSnapshot) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let url = cacheURL else {
            return saveUnlocked(snapshot, store: MagicianAccess.store)
        }
        var changed = false
        var coordinationError: NSError?
        NSFileCoordinator(filePresenter: nil).coordinate(
            writingItemAt: url,
            options: [],
            error: &coordinationError
        ) { coordinatedURL in
            let previous = loadFileUnlocked(at: coordinatedURL)
                ?? loadUnlocked(store: MagicianAccess.store)
            guard previous.map({ snapshot.generatedAt >= $0.generatedAt }) ?? true,
                  let data = try? JSONEncoder().encode(snapshot),
                  (try? data.write(to: coordinatedURL, options: .atomic)) != nil else { return }
            changed = previous?.hasSameVisibleContent(as: snapshot) != true
        }
        return changed
    }

    /// Injectable storage remains available for deterministic unit coverage.
    @discardableResult
    static func save(_ snapshot: MagicanGlanceSnapshot, store: UserDefaults) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return saveUnlocked(snapshot, store: store)
    }

    private static func saveUnlocked(
        _ snapshot: MagicanGlanceSnapshot,
        store: UserDefaults
    ) -> Bool {
        let previous = loadUnlocked(store: store)
        guard previous.map({ snapshot.generatedAt >= $0.generatedAt }) ?? true,
              let data = try? JSONEncoder().encode(snapshot) else { return false }
        store.set(data, forKey: key)
        return previous?.hasSameVisibleContent(as: snapshot) != true
    }

    private static var cacheURL: URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: MagicianAccess.appGroup)?
            .appendingPathComponent(filename, isDirectory: false)
    }

    private static func loadFileUnlocked(at url: URL) -> MagicanGlanceSnapshot? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(MagicanGlanceSnapshot.self, from: data)
    }
}

private struct TodayEnvelope: Decodable {
    let generatedAt: Int64
    let counts: Counts
    let sections: Sections

    enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case counts, sections
    }

    struct Counts: Decodable {
        let needsYou: Int
        let activeWork: Int

        enum CodingKeys: String, CodingKey {
            case needsYou = "needs_you"
            case activeWork = "active_work"
        }
    }

    struct Sections: Decodable {
        let needsYou: [Item]
        let activeWork: [Item]

        enum CodingKeys: String, CodingKey {
            case needsYou = "needs_you"
            case activeWork = "active_work"
        }
    }

    struct Item: Decodable {
        let title: String?
        let reason: String?
        let status: String?
        let taskID: String?

        enum CodingKeys: String, CodingKey {
            case title, reason, status
            case taskID = "task_id"
        }
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

private extension CharacterSet {
    static let magicanPathSegment: CharacterSet = {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return allowed
    }()
}
