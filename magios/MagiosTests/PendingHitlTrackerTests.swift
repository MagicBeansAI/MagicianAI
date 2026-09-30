import XCTest
@testable import Magician

/// Mirrors the web `pendingHitlStore` semantics
/// (ui/unified-ui/src/lib/stores/pendingHitlStore.ts) — especially the keying
/// rules the web flags as the source of a "monotonically-climbing badge" bug.
final class PendingHitlTrackerTests: XCTestCase {
    private var tracker: PendingHitlTracker { .shared }

    override func setUp() {
        super.setUp()
        tracker.reset()   // singleton — isolate each test
    }

    private func apply(_ obj: [String: Any]) {
        let data = try! JSONSerialization.data(withJSONObject: obj)
        tracker.apply(eventText: String(data: data, encoding: .utf8)!)
    }

    // correlationKey resolves the SAME id across the three wire shapes.
    func testCorrelationKeyAcrossWireShapes() {
        let live: [String: Any] = ["event_type": "HitlRequested", "data": ["correlation_id": "c1"]]
        let envelope: [String: Any] = ["event_type": "AgentEvent",
                                       "data": ["event": ["payload": ["correlation_id": "c1"]]]]
        let backfill: [String: Any] = ["event_id": "evt_x", "event_type": "HitlRequested",
                                       "payload": ["correlation_id": "c1"]]
        XCTAssertEqual(PendingHitlTracker.correlationKey(live), "c1")
        XCTAssertEqual(PendingHitlTracker.correlationKey(envelope), "c1")
        XCTAssertEqual(PendingHitlTracker.correlationKey(backfill), "c1")
    }

    // `correlation_id` (the dedup contract) wins over `pause_state_id`.
    func testCorrelationKeyPrefersCorrelationId() {
        let obj: [String: Any] = ["pause_state_id": "p1", "payload": ["correlation_id": "c1"]]
        XCTAssertEqual(PendingHitlTracker.correlationKey(obj), "c1")
    }

    // `id` matches ONLY via `data.request`, never a stray top-level id.
    func testIdKeyOnlyFromDataRequest() {
        let legacy: [String: Any] = ["event_type": "UserRequestPending", "data": ["request": ["id": "r1"]]]
        XCTAssertEqual(PendingHitlTracker.correlationKey(legacy), "r1")
        let strayId: [String: Any] = ["id": "x", "event_type": "HitlRequested"]
        XCTAssertNil(PendingHitlTracker.correlationKey(strayId))
    }

    func testIsResolutionEvent() {
        XCTAssertTrue(PendingHitlTracker.isResolutionEvent("HitlResolved"))
        XCTAssertTrue(PendingHitlTracker.isResolutionEvent("UserRequestResolved"))
        for suffix in [".resolved", ".responded", ".expired", ".cancelled", ".dismissed"] {
            XCTAssertTrue(PendingHitlTracker.isResolutionEvent("x\(suffix)"))
        }
        XCTAssertFalse(PendingHitlTracker.isResolutionEvent("HitlRequested"))
    }

    func testApplyAddsResolvesAndDedups() {
        apply(["event_type": "HitlRequested", "data": ["correlation_id": "c1"]])
        XCTAssertEqual(tracker.count, 1)
        apply(["event_type": "HitlRequested", "data": ["correlation_id": "c1"]])   // dup key
        XCTAssertEqual(tracker.count, 1)
        apply(["event_type": "HitlResolved", "data": ["correlation_id": "c1"]])
        XCTAssertEqual(tracker.count, 0)
    }

    func testApplyIgnoresNonCanonicalAndControl() {
        // Only canonical HitlRequested counts — legacy twins would double-count.
        apply(["event_type": "AgenticWaitingForUser", "data": ["correlation_id": "c1"]])
        XCTAssertEqual(tracker.count, 0)
        apply(["event_type": "__events_heartbeat", "data": ["correlation_id": "c1"]])
        XCTAssertEqual(tracker.count, 0)
    }

    func testSeedReplacesAndDropRemoves() {
        tracker.seed(correlationIds: ["a", "b", ""])   // blank filtered out
        XCTAssertEqual(tracker.count, 2)
        XCTAssertTrue(tracker.contains("a"))
        tracker.drop("a")
        XCTAssertEqual(tracker.count, 1)
        XCTAssertFalse(tracker.contains("a"))
        tracker.restore("a")
        XCTAssertEqual(tracker.count, 2)
        tracker.seed(correlationIds: ["z"])            // replaces the set
        XCTAssertEqual(tracker.count, 1)
    }

    // The whole point of the tracker: a brand-new HITL leads `needs_action` in
    // the transient window before the feed refetch confirms it.
    func testBadgeLeadsWithPendingHitl() {
        tracker.seed(correlationIds: ["a", "b"])   // baseline == the fetched needs_action rows
        XCTAssertEqual(
            AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: tracker.count, needsAction: 2, failed: 1),
            3)   // max(2,2)+1
        apply(["event_type": "HitlRequested", "data": ["correlation_id": "c"]])   // arrives before refetch
        XCTAssertEqual(tracker.count, 3)
        XCTAssertEqual(
            AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: tracker.count, needsAction: 2, failed: 1),
            4)   // pendingHitl 3 > needs_action 2 → 3 + 1
    }
}
