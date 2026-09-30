import Foundation

/// iOS port of the web `pendingHitlStore`
/// (ui/unified-ui/src/lib/stores/pendingHitlStore.ts): the count of currently
/// pending HITL requests — the `approval.requested` family minus their resolved
/// counterparts, deduped by `correlation_id`. Feeds the Attention badge's
/// `pendingHitl` input so the badge ticks up the instant a HITL arrives, a beat
/// before the feed refetch updates `needs_action`.
///
/// The web comments flag a real "monotonically-climbing badge" bug from wrong
/// keying — this port matches its `correlationKey` / `isResolutionEvent` /
/// `ingestLine` semantics exactly.
final class PendingHitlTracker: ObservableObject {
    static let shared = PendingHitlTracker()

    @Published private(set) var count = 0
    private var ids = Set<String>()

    private init() {}

    /// Ingest one realtime event (mirrors web `ingestLine`). Safe off the main
    /// thread — the `@Published` update is marshalled to main.
    func apply(eventText: String) {
        guard let data = eventText.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        let eventType = (obj["event_type"] as? String) ?? ""
        if eventType.isEmpty || eventType.hasPrefix("__events_") { return }
        guard let key = Self.correlationKey(obj) else { return }
        if Self.isResolutionEvent(eventType) {
            mutate { $0.remove(key) }
            return
        }
        // Count ONLY the canonical `HitlRequested`. Legacy dual-emit twins
        // (AgenticWaitingForUser / UserRequestPending / …) are co-emitted for the
        // SAME pause but keyed on `pause_state_id`, so counting them double-counts
        // the badge and orphans an entry no `HitlResolved` ever clears.
        guard eventType == "HitlRequested" else { return }
        mutate { $0.insert(key) }
    }

    /// Replace the baseline from the authoritative feed (its actionable HITL rows).
    /// Live events adjust the set between fetches; the next fetch re-seeds.
    func seed(correlationIds: [String]) {
        let fresh = Set(correlationIds.filter { !$0.isEmpty })
        mutate { $0 = fresh }
    }

    /// Optimistically drop a just-resolved id (called from a local respond handler)
    /// so the badge ticks down immediately instead of waiting for the WS/feed.
    func drop(_ correlationId: String) {
        guard !correlationId.isEmpty else { return }
        mutate { $0.remove(correlationId) }
    }

    /// Membership snapshot for a main-thread optimistic transaction, allowing
    /// rollback to restore exactly the tracker state that existed before it.
    func contains(_ correlationId: String) -> Bool {
        ids.contains(correlationId)
    }

    /// Compensate an optimistic local response when its API request fails.
    /// This is intentionally the inverse of `drop`, rather than a full `seed`,
    /// so realtime requests that arrived while the mutation was in flight stay
    /// in the tracker.
    func restore(_ correlationId: String) {
        guard !correlationId.isEmpty else { return }
        mutate { $0.insert(correlationId) }
    }

    /// Test helper — clear all state.
    func reset() { mutate { $0.removeAll() } }

    /// Mutate the id set and publish `count`, always on the main thread.
    private func mutate(_ change: @escaping (inout Set<String>) -> Void) {
        let run = { [weak self] in
            guard let self = self else { return }
            change(&self.ids)
            let c = self.ids.count
            if self.count != c { self.count = c }
        }
        if Thread.isMainThread { run() } else { DispatchQueue.main.async(execute: run) }
    }

    // MARK: - Web-parity helpers (ports of pendingHitlStore.ts)

    /// Layered correlation-id extractor (`correlationKey`, pendingHitlStore.ts).
    /// Key-first across every nesting layer; `correlation_id` is the dedup
    /// contract so it's tried before `pause_state_id` etc. `id` may match ONLY on
    /// the `data.request` layer (the legacy `UserRequest` payload).
    static func correlationKey(_ obj: [String: Any]) -> String? {
        let data = obj["data"] as? [String: Any]
        let dataEvent = data?["event"] as? [String: Any]
        let dataEventPayload = dataEvent?["payload"] as? [String: Any]
        let payload = obj["payload"] as? [String: Any]
        let dataRequest = data?["request"] as? [String: Any]

        let layers: [(layer: [String: Any]?, isDataRequest: Bool)] = [
            (obj, false), (data, false), (dataEvent, false),
            (dataEventPayload, false), (payload, false), (dataRequest, true),
        ]
        let keys = ["correlation_id", "pause_state_id", "approval_id", "clarification_id", "request_id", "id"]
        for key in keys {
            for entry in layers {
                guard let layer = entry.layer else { continue }
                if key == "id" && !entry.isDataRequest { continue }
                if let v = layer[key] as? String, !v.isEmpty { return v }
            }
        }
        return nil
    }

    /// Resolution predicate (`isResolutionEvent`, pendingHitlStore.ts).
    static func isResolutionEvent(_ eventType: String) -> Bool {
        if eventType == "HitlResolved" || eventType == "UserRequestResolved" { return true }
        return eventType.hasSuffix(".resolved")
            || eventType.hasSuffix(".responded")
            || eventType.hasSuffix(".expired")
            || eventType.hasSuffix(".cancelled")
            || eventType.hasSuffix(".dismissed")
    }
}
