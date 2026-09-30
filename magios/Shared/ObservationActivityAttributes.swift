import ActivityKit
import Foundation

/// Live Activity for an ongoing observation (lock screen + Dynamic Island). The
/// fixed attributes identify the session; the `ContentState` carries what changes
/// (phase + the rolling summary). The elapsed timer renders from `startedAt` with
/// SwiftUI's `Text(timerInterval:)`, so it ticks without any updates.
public struct ObservationActivityAttributes: ActivityAttributes {
    public struct ContentState: Codable, Hashable {
        /// "listening" | "paused" | "ended".
        public var phase: String
        /// Latest rolling summary line (may be empty early on).
        public var latestSummary: String

        public init(phase: String, latestSummary: String = "") {
            self.phase = phase
            self.latestSummary = latestSummary
        }
    }

    /// Meeting title (or "Listening" for an ad-hoc room capture).
    public var title: String
    /// "mic" (in-app room capture) or "screen" (broadcast: screen + audio + you).
    public var kind: String
    public var startedAt: Date
    public var sessionId: String
    public var threadId: String

    public init(title: String, kind: String, startedAt: Date, sessionId: String, threadId: String) {
        self.title = title
        self.kind = kind
        self.startedAt = startedAt
        self.sessionId = sessionId
        self.threadId = threadId
    }
}
