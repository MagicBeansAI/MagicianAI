import ActivityKit
import Foundation

/// The "Magician at work" Live Activity (lock screen + Dynamic Island): one
/// summary of the owner's long-running work — VibeDev coding runs, VibeDev
/// crews and Work tasks. It features one item, lists the next two and counts
/// the rest. One activity covers everything; the app keeps it in step with
/// `GET /work/live` (`GET /vibedev/runs/live` on an older server) and ends it
/// when nothing is going.
///
/// The type keeps its coding-runs name: the server's push-to-start names it,
/// and the `coding_runs` / `coding_runs_start` routes carry it. The fields
/// after `detail` are optional, so a payload from an older server (counts,
/// headline and detail only) still decodes, and the widget then shows the
/// headline and detail as before.
public struct CodingRunsActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        /// One line under the featured item (`next` in the content state).
        public struct NextItem: Codable, Hashable {
            public var id: String
            /// `coding_run`, `crew` or `work_task`.
            public var kind: String
            /// `needs_you`, `working`, `paused`, `failed`, `stopped` or `done`.
            public var state: String
            public var title: String

            public init(id: String, kind: String, state: String, title: String) {
                self.id = id
                self.kind = kind
                self.state = state
                self.title = title
            }

            public init(from decoder: Decoder) throws {
                let c = try decoder.container(keyedBy: CodingKeys.self)
                id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
                kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? ""
                state = try c.decodeIfPresent(String.self, forKey: .state) ?? ""
                title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
            }

            enum CodingKeys: String, CodingKey {
                case id, kind, state, title
            }
        }

        public var working: Int
        public var needsYou: Int
        public var done: Int
        public var failed: Int
        /// The featured item in one line, e.g. "API lane · Shop".
        public var headline: String
        /// What that item is doing or why it waits.
        public var detail: String
        /// The featured item: `coding_run`, `crew` or `work_task`.
        public var featuredKind: String?
        public var featuredState: String?
        public var featuredTitle: String?
        /// Its project name ("" for a Work task).
        public var featuredContext: String?
        public var featuredStep: String?
        /// What the featured item asks of the owner: `approve_diff` or `answer`.
        public var actionKind: String?
        /// The approval or request id that answers it.
        public var actionId: String?
        /// Where the featured item opens (`magican://vibe?…`, `magican://work?task=…`).
        public var deepLink: String?
        /// Items beyond the featured one and `next`.
        public var more: Int?
        /// The next two items by rank.
        public var next: [NextItem]?

        public init(
            working: Int,
            needsYou: Int,
            done: Int = 0,
            failed: Int = 0,
            headline: String = "",
            detail: String = "",
            featuredKind: String? = nil,
            featuredState: String? = nil,
            featuredTitle: String? = nil,
            featuredContext: String? = nil,
            featuredStep: String? = nil,
            actionKind: String? = nil,
            actionId: String? = nil,
            deepLink: String? = nil,
            more: Int? = nil,
            next: [NextItem]? = nil
        ) {
            self.working = working
            self.needsYou = needsYou
            self.done = done
            self.failed = failed
            self.headline = headline
            self.detail = detail
            self.featuredKind = featuredKind
            self.featuredState = featuredState
            self.featuredTitle = featuredTitle
            self.featuredContext = featuredContext
            self.featuredStep = featuredStep
            self.actionKind = actionKind
            self.actionId = actionId
            self.deepLink = deepLink
            self.more = more
            self.next = next
        }

        /// Every key may be missing: an older server sends only the first six,
        /// and a newer one may leave out what it does not know.
        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            working = try c.decodeIfPresent(Int.self, forKey: .working) ?? 0
            needsYou = try c.decodeIfPresent(Int.self, forKey: .needsYou) ?? 0
            done = try c.decodeIfPresent(Int.self, forKey: .done) ?? 0
            failed = try c.decodeIfPresent(Int.self, forKey: .failed) ?? 0
            headline = try c.decodeIfPresent(String.self, forKey: .headline) ?? ""
            detail = try c.decodeIfPresent(String.self, forKey: .detail) ?? ""
            featuredKind = try c.decodeIfPresent(String.self, forKey: .featuredKind)
            featuredState = try c.decodeIfPresent(String.self, forKey: .featuredState)
            featuredTitle = try c.decodeIfPresent(String.self, forKey: .featuredTitle)
            featuredContext = try c.decodeIfPresent(String.self, forKey: .featuredContext)
            featuredStep = try c.decodeIfPresent(String.self, forKey: .featuredStep)
            actionKind = try c.decodeIfPresent(String.self, forKey: .actionKind)
            actionId = try c.decodeIfPresent(String.self, forKey: .actionId)
            deepLink = try c.decodeIfPresent(String.self, forKey: .deepLink)
            more = try c.decodeIfPresent(Int.self, forKey: .more)
            next = try c.decodeIfPresent([NextItem].self, forKey: .next)
        }

        enum CodingKeys: String, CodingKey {
            case working, needsYou, done, failed, headline, detail
            case featuredKind, featuredState, featuredTitle, featuredContext, featuredStep
            case actionKind, actionId, deepLink, more, next
        }

        public var isActive: Bool { working + needsYou > 0 }

        /// A newer server named the featured item; without it the widget shows
        /// `headline` and `detail`.
        public var hasFeatured: Bool { !(featuredTitle ?? "").isEmpty }

        /// Lock screen order (`relevanceScore`): Observation and Ambient (100)
        /// > a summary that needs the owner (80) > a followed item (60) > a
        /// routine summary (40).
        public var relevanceScore: Double { needsYou > 0 ? 80 : 40 }
    }

    public var title: String

    public init(title: String = "Magician at work") {
        self.title = title
    }
}

// MARK: - GET /work/live

/// One item of `GET /api/magician/v2/work/live`, as the phone needs it. Every
/// field but `id` has a default, so a key the server leaves out never drops
/// the whole answer.
public struct WorkLiveItem: Decodable, Equatable {
    public struct Action: Decodable, Equatable {
        /// `approve_diff` or `answer`.
        public let kind: String
        public let id: String
    }

    /// `run:<id>`, `crew:<id>` or `task:<id>`.
    public let id: String
    /// `coding_run`, `crew` or `work_task`.
    public let kind: String
    public let title: String
    /// The project name; "" for a Work task.
    public let context: String
    /// `needs_you`, `working`, `paused`, `failed`, `stopped` or `done`.
    public let state: String
    public let step: String?
    public let followed: Bool
    public let updatedAt: String?
    public let spentUsd: Double?
    public let action: Action?
    public let deepLink: String?

    public init(
        id: String,
        kind: String,
        title: String,
        context: String = "",
        state: String,
        step: String? = nil,
        followed: Bool = false,
        updatedAt: String? = nil,
        spentUsd: Double? = nil,
        action: Action? = nil,
        deepLink: String? = nil
    ) {
        self.id = id
        self.kind = kind
        self.title = title
        self.context = context
        self.state = state
        self.step = step
        self.followed = followed
        self.updatedAt = updatedAt
        self.spentUsd = spentUsd
        self.action = action
        self.deepLink = deepLink
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = (try? c.decodeIfPresent(String.self, forKey: .kind)) ?? ""
        title = (try? c.decodeIfPresent(String.self, forKey: .title)) ?? ""
        context = (try? c.decodeIfPresent(String.self, forKey: .context)) ?? ""
        state = (try? c.decodeIfPresent(String.self, forKey: .state)) ?? ""
        step = (try? c.decodeIfPresent(String.self, forKey: .step))
        followed = (try? c.decodeIfPresent(Bool.self, forKey: .followed)) ?? false
        updatedAt = (try? c.decodeIfPresent(String.self, forKey: .updatedAt))
        spentUsd = (try? c.decodeIfPresent(Double.self, forKey: .spentUsd))
        action = (try? c.decodeIfPresent(Action.self, forKey: .action))
        deepLink = (try? c.decodeIfPresent(String.self, forKey: .deepLink))
    }

    enum CodingKeys: String, CodingKey {
        case id, kind, title, context, state, step, followed, action
        case updatedAt = "updated_at"
        case spentUsd = "spent_usd"
        case deepLink = "deep_link"
    }

    /// The item finished one way or another; a followed item's activity ends.
    public var isEnded: Bool { WorkLiveSummary.isEnded(state) }

    /// `updated_at` as epoch seconds, when it parses.
    public var updatedAtEpoch: Double? {
        guard let updatedAt, !updatedAt.isEmpty else { return nil }
        let precise = ISO8601DateFormatter()
        precise.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = precise.date(from: updatedAt) { return date.timeIntervalSince1970 }
        return ISO8601DateFormatter().date(from: updatedAt)?.timeIntervalSince1970
    }

    /// "Title · Project" (the title alone for a Work task).
    public var headline: String {
        context.isEmpty ? title : "\(title) · \(context)"
    }
}

/// The body of `GET /api/magician/v2/work/live`.
public struct WorkLive: Decodable, Equatable {
    public struct Counts: Decodable, Equatable {
        public let needsYou: Int
        /// Includes paused items.
        public let working: Int
        public let failed: Int
        public let done: Int

        public init(needsYou: Int = 0, working: Int = 0, failed: Int = 0, done: Int = 0) {
            self.needsYou = needsYou
            self.working = working
            self.failed = failed
            self.done = done
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            needsYou = (try? c.decodeIfPresent(Int.self, forKey: .needsYou)) ?? 0
            working = (try? c.decodeIfPresent(Int.self, forKey: .working)) ?? 0
            failed = (try? c.decodeIfPresent(Int.self, forKey: .failed)) ?? 0
            done = (try? c.decodeIfPresent(Int.self, forKey: .done)) ?? 0
        }

        enum CodingKeys: String, CodingKey {
            case needsYou = "needs_you"
            case working, failed, done
        }

        public var total: Int { needsYou + working + failed + done }
    }

    public let featured: WorkLiveItem?
    /// At most two, by rank after the featured one.
    public let next: [WorkLiveItem]
    public let counts: Counts
    /// At most 50, ranked.
    public let items: [WorkLiveItem]
    /// Item ids this scope follows (at most 3).
    public let followed: [String]

    public init(
        featured: WorkLiveItem?,
        next: [WorkLiveItem] = [],
        counts: Counts = Counts(),
        items: [WorkLiveItem] = [],
        followed: [String] = []
    ) {
        self.featured = featured
        self.next = next
        self.counts = counts
        self.items = items
        self.followed = followed
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        featured = (try? c.decodeIfPresent(WorkLiveItem.self, forKey: .featured))
        next = (try? c.decodeIfPresent([WorkLiveItem].self, forKey: .next)) ?? []
        // Required: a body without counts is not a work summary (an older
        // server's fallback page, say), and the caller then reads the old route.
        counts = try c.decode(Counts.self, forKey: .counts)
        items = (try? c.decodeIfPresent([WorkLiveItem].self, forKey: .items)) ?? []
        followed = (try? c.decodeIfPresent([String].self, forKey: .followed)) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case featured, next, counts, items, followed
    }

    /// Coding runs and crews waiting on the owner (the left-menu VibeDev
    /// badge). A crew lane that waits is counted through its crew.
    public var codingRunsNeedingYou: Int {
        items.filter { ($0.kind == "coding_run" || $0.kind == "crew") && $0.state == "needs_you" }.count
    }
}

public enum WorkLiveSummary {
    public static func parse(_ data: Data) -> WorkLive? {
        try? JSONDecoder().decode(WorkLive.self, from: data)
    }

    public static func isEnded(_ state: String) -> Bool {
        state == "done" || state == "failed" || state == "stopped"
    }

    /// The summary activity's state for `live`: the server already ranked the
    /// items, so this only copies the featured one, the next two and the counts.
    public static func state(for live: WorkLive) -> CodingRunsActivityAttributes.ContentState {
        let featured = live.featured
        let next = Array(live.next.prefix(2))
        let shown = (featured == nil ? 0 : 1) + next.count
        let total = max(live.items.count, live.counts.total)
        let detail: String = {
            guard let featured else { return "" }
            if featured.state == "needs_you" { return "Needs you" }
            return featured.step ?? ""
        }()
        return CodingRunsActivityAttributes.ContentState(
            working: live.counts.working,
            needsYou: live.counts.needsYou,
            done: live.counts.done,
            failed: live.counts.failed,
            headline: featured?.headline ?? "",
            detail: detail,
            featuredKind: featured?.kind,
            featuredState: featured?.state,
            featuredTitle: featured?.title,
            featuredContext: featured?.context,
            featuredStep: featured?.step,
            actionKind: featured?.action?.kind,
            actionId: featured?.action?.id,
            deepLink: featured?.deepLink,
            more: max(0, total - shown),
            next: next.map {
                CodingRunsActivityAttributes.ContentState.NextItem(
                    id: $0.id, kind: $0.kind, state: $0.state, title: $0.title)
            }
        )
    }
}

// MARK: - GET /vibedev/runs/live (older servers)

/// One row of `GET /api/magician/v2/vibedev/runs/live`, as the activity needs
/// it. Read only when the server has no `/work/live` yet.
public struct CodingLiveRun: Decodable, Equatable {
    public let runId: String
    public let projectName: String
    public let title: String
    public let state: String
    public let step: String?
    public let summary: String?
    public let lane: String?

    enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case projectName = "project_name"
        case title, state, step, summary, lane
    }
}

public enum CodingRunsSummary {
    private struct Envelope: Decodable {
        let runs: [CodingLiveRun]
    }

    public static func parse(_ data: Data) -> [CodingLiveRun]? {
        (try? JSONDecoder().decode(Envelope.self, from: data))?.runs
    }

    /// The activity state for `runs` (server order: needs-you first, then going, then ended).
    public static func state(for runs: [CodingLiveRun]) -> CodingRunsActivityAttributes.ContentState {
        let count = { (state: String) in runs.filter { $0.state == state }.count }
        let top = runs.first { $0.state == "needs_you" } ?? runs.first { $0.state == "working" } ?? runs.first
        let name: (CodingLiveRun) -> String = { run in
            let label = run.lane.map { "\($0) lane" } ?? run.title
            return run.projectName.isEmpty ? label : "\(label) · \(run.projectName)"
        }
        let detail: String = {
            guard let top else { return "" }
            if top.state == "needs_you" { return "Needs you" }
            return top.step ?? top.summary ?? ""
        }()
        return CodingRunsActivityAttributes.ContentState(
            working: count("working") + count("paused"),
            needsYou: count("needs_you"),
            done: count("done"),
            failed: count("failed"),
            headline: top.map(name) ?? "",
            detail: detail
        )
    }
}
