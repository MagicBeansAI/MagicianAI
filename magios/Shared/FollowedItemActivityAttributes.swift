import ActivityKit
import Foundation

/// Live Activity for one item the owner follows — a VibeDev coding run, a
/// VibeDev crew or a Work task — until it ends. The app starts it from the
/// item's Follow button and registers its push token as a `work_item` route
/// carrying the item id; the server then moves it and ends it with the item's
/// final state. At most three run at a time (the server's follow limit).
///
/// `content-state` keys are the Swift property names; `updatedAt` is epoch
/// seconds (not a `Date`, whose default decoding counts from 2001).
public struct FollowedItemActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        /// `needs_you`, `working`, `paused`, `failed`, `stopped` or `done`.
        public var state: String
        public var title: String
        /// The project name; "" for a Work task.
        public var context: String
        public var step: String
        public var spentUsd: Double?
        /// `approve_diff` or `answer` while the item waits on the owner.
        public var actionKind: String?
        public var actionId: String?
        public var deepLink: String?
        /// Epoch seconds.
        public var updatedAt: Double?

        public init(
            state: String,
            title: String,
            context: String = "",
            step: String = "",
            spentUsd: Double? = nil,
            actionKind: String? = nil,
            actionId: String? = nil,
            deepLink: String? = nil,
            updatedAt: Double? = nil
        ) {
            self.state = state
            self.title = title
            self.context = context
            self.step = step
            self.spentUsd = spentUsd
            self.actionKind = actionKind
            self.actionId = actionId
            self.deepLink = deepLink
            self.updatedAt = updatedAt
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            state = try c.decodeIfPresent(String.self, forKey: .state) ?? "working"
            title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
            context = try c.decodeIfPresent(String.self, forKey: .context) ?? ""
            step = try c.decodeIfPresent(String.self, forKey: .step) ?? ""
            spentUsd = try c.decodeIfPresent(Double.self, forKey: .spentUsd)
            actionKind = try c.decodeIfPresent(String.self, forKey: .actionKind)
            actionId = try c.decodeIfPresent(String.self, forKey: .actionId)
            deepLink = try c.decodeIfPresent(String.self, forKey: .deepLink)
            updatedAt = try c.decodeIfPresent(Double.self, forKey: .updatedAt)
        }

        enum CodingKeys: String, CodingKey {
            case state, title, context, step, spentUsd, actionKind, actionId, deepLink, updatedAt
        }

        /// The state of `item` in `GET /work/live`.
        public init(item: WorkLiveItem) {
            self.init(
                state: item.state,
                title: item.title,
                context: item.context,
                step: item.step ?? "",
                spentUsd: item.spentUsd,
                actionKind: item.action?.kind,
                actionId: item.action?.id,
                deepLink: item.deepLink,
                updatedAt: item.updatedAtEpoch
            )
        }

        public var isEnded: Bool { WorkLiveSummary.isEnded(state) }

        /// Below the summary when it needs the owner, above a routine one
        /// (see `CodingRunsActivityAttributes.ContentState.relevanceScore`).
        public static let relevanceScore: Double = 60
    }

    /// `run:<id>`, `crew:<id>` or `task:<id>`.
    public var itemId: String
    /// `coding_run`, `crew` or `work_task`.
    public var kind: String

    public init(itemId: String, kind: String) {
        self.itemId = itemId
        self.kind = kind
    }
}
