import ActivityKit
import Foundation

/// Owns the observation Live Activity (lock screen + Dynamic Island) for the
/// in-app capture session. Start on listening, update on pause/resume + rolling
/// summary, end on stop. After an app relaunch it adopts the Live Activity that
/// survived (rather than starting a duplicate).
@MainActor
final class ObservationActivity {
    static let shared = ObservationActivity()

    nonisolated init() {}

    private var activity: Activity<ObservationActivityAttributes>?
    private var lastSummary = ""
    private var lastPhase = "listening"

    func start(title: String, kind: String, sessionId: String, threadId: String) {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        // Adopt a surviving activity for this session (e.g. after relaunch).
        if let existing = Activity<ObservationActivityAttributes>.activities
            .first(where: { $0.attributes.sessionId == sessionId }) {
            activity = existing
            return
        }
        guard activity == nil else { return }
        lastSummary = ""
        let attributes = ObservationActivityAttributes(
            title: title.isEmpty ? "Listening" : title,
            kind: kind,
            startedAt: Date(),
            sessionId: sessionId,
            threadId: threadId
        )
        let state = ObservationActivityAttributes.ContentState(phase: "listening")
        activity = try? Activity.request(
            attributes: attributes,
            content: ActivityContent(state: state, staleDate: nil),
            pushType: nil
        )
    }

    func update(phase: String, summary: String? = nil) {
        guard let activity else { return }
        lastPhase = phase
        if let summary, !summary.isEmpty { lastSummary = summary }
        let state = ObservationActivityAttributes.ContentState(phase: phase, latestSummary: lastSummary)
        Task { await activity.update(ActivityContent(state: state, staleDate: nil)) }
    }

    /// Update just the rolling summary, preserving the current phase.
    func updateSummary(_ summary: String) {
        guard activity != nil, !summary.isEmpty, summary != lastSummary else { return }
        update(phase: lastPhase, summary: summary)
    }

    func end() {
        guard let activity else { return }
        self.activity = nil
        let summary = lastSummary
        Task {
            await activity.end(
                ActivityContent(
                    state: ObservationActivityAttributes.ContentState(phase: "ended", latestSummary: summary),
                    staleDate: nil
                ),
                dismissalPolicy: .immediate
            )
        }
    }
}
