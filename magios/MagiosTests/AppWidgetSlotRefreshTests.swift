import XCTest
@testable import Magician

/// The refresh, staleness and presentation rules behind a page's app widget
/// slots: a 304 renews every item, a near-now deadline is accepted after the
/// refresh floor, the last good render survives within the staleness bound,
/// and a hidden workspace default reads as an empty slot.
final class AppWidgetSlotRefreshTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_788_000_000)

    // MARK: - Deadlines

    func testFutureDeadlineIsKeptAsIs() {
        let future = now.addingTimeInterval(60)
        let accepted = AppNativeSlotRefreshPolicy.acceptedDeadline(
            AppNativeSlotRefreshPolicy.timestamp(future),
            now: now
        )
        XCTAssertEqual(accepted.timeIntervalSince1970, future.timeIntervalSince1970, accuracy: 0.001)
    }

    func testPastNowOrMissingDeadlineIsAcceptedAfterTheRefreshFloor() {
        let floor = now.addingTimeInterval(AppNativeSurfaceContract.minimumRefreshSeconds)
        for raw in [
            AppNativeSlotRefreshPolicy.timestamp(now.addingTimeInterval(-0.01)),
            AppNativeSlotRefreshPolicy.timestamp(now),
            AppNativeSlotRefreshPolicy.timestamp(now.addingTimeInterval(1)),
            nil,
            "not-a-timestamp"
        ] as [String?] {
            let accepted = AppNativeSlotRefreshPolicy.acceptedDeadline(raw, now: now)
            XCTAssertEqual(
                accepted.timeIntervalSince1970,
                floor.timeIntervalSince1970,
                accuracy: 0.001,
                "deadline \(raw ?? "nil") should fall back to the floor"
            )
        }
    }

    // MARK: - 304 renewal

    func testNotModifiedRenewsEveryCachedItemDeadline() throws {
        let batch = try AppNativeSurfaceContract.decodeWidgets(try encode(widgetBatch()))
        XCTAssertEqual(batch.widgets.count, 2)
        let deadline = try XCTUnwrap(AppNativeSurfaceContract.date("2026-09-02T11:00:00Z"))

        let renewed = AppNativeSlotRefreshPolicy.renewing(batch, until: deadline)

        XCTAssertEqual(AppNativeSurfaceContract.date(renewed.refreshAfter), deadline)
        for item in renewed.widgets {
            XCTAssertEqual(AppNativeSurfaceContract.date(item.refreshAfter), deadline)
        }
        // Everything else about the validated representation is unchanged.
        XCTAssertEqual(renewed.etag, batch.etag)
        XCTAssertEqual(renewed.revision, batch.revision)
        XCTAssertEqual(renewed.widgets.map(\.id), batch.widgets.map(\.id))
        XCTAssertEqual(renewed.widgets.map(\.revision), batch.widgets.map(\.revision))
        XCTAssertEqual(renewed.widgets.map(\.state), batch.widgets.map(\.state))
        XCTAssertEqual(
            renewed.widgets.map(\.installationGeneration),
            batch.widgets.map(\.installationGeneration)
        )

        // A renewed item is live again although its original deadline passed.
        let afterOldDeadline = try XCTUnwrap(AppNativeSurfaceContract.date("2026-09-02T10:30:00Z"))
        XCTAssertFalse(AppNativeSlotRefreshPolicy.isItemLive(
            refreshAfter: batch.widgets[0].refreshAfter,
            confirmedAt: nil,
            now: afterOldDeadline
        ))
        XCTAssertTrue(AppNativeSlotRefreshPolicy.isItemLive(
            refreshAfter: renewed.widgets[0].refreshAfter,
            confirmedAt: nil,
            now: afterOldDeadline
        ))
    }

    // MARK: - Staleness

    func testLastGoodRenderIsRetainedOnlyWithinTheStalenessBound() {
        let bound = AppNativeSlotRefreshPolicy.defaultMaximumStalenessSeconds
        XCTAssertEqual(bound, 300)
        XCTAssertFalse(AppNativeSlotRefreshPolicy.retainsLastGood(confirmedAt: nil, now: now))
        XCTAssertTrue(AppNativeSlotRefreshPolicy.retainsLastGood(
            confirmedAt: now.addingTimeInterval(-10), now: now
        ))
        XCTAssertTrue(AppNativeSlotRefreshPolicy.retainsLastGood(
            confirmedAt: now.addingTimeInterval(-bound), now: now
        ))
        XCTAssertFalse(AppNativeSlotRefreshPolicy.retainsLastGood(
            confirmedAt: now.addingTimeInterval(-bound - 1), now: now
        ))
        // A payload-supplied bound overrides the default.
        XCTAssertFalse(AppNativeSlotRefreshPolicy.retainsLastGood(
            confirmedAt: now.addingTimeInterval(-61), now: now, maximumStaleness: 60
        ))
    }

    func testExpiredItemStaysLiveWhileTheBatchIsWithinTheStalenessBound() {
        let expired = AppNativeSlotRefreshPolicy.timestamp(now.addingTimeInterval(-1))
        XCTAssertTrue(AppNativeSlotRefreshPolicy.isItemLive(
            refreshAfter: expired,
            confirmedAt: now.addingTimeInterval(-30),
            now: now
        ))
        XCTAssertFalse(AppNativeSlotRefreshPolicy.isItemLive(
            refreshAfter: expired,
            confirmedAt: now.addingTimeInterval(-301),
            now: now
        ))
        XCTAssertTrue(AppNativeSlotRefreshPolicy.isItemLive(
            refreshAfter: AppNativeSlotRefreshPolicy.timestamp(now.addingTimeInterval(1)),
            confirmedAt: nil,
            now: now
        ))
    }

    // MARK: - Presentation

    func testRenderedItemAlwaysPresentsTheCard() throws {
        let slot = try decodeSlot(assigned(source: "workspace_default"))
        XCTAssertEqual(AppNativeSlotRefreshPolicy.presentation(assignment: slot, hasItem: true), .card)
    }

    func testEmptySlotOffersAdd() throws {
        let slot = try decodeSlot(["slot_id": slotID, "pinned_system_default": false, "opted_out": false])
        XCTAssertEqual(AppNativeSlotRefreshPolicy.presentation(assignment: slot, hasItem: false), .add)
    }

    func testHiddenWorkspaceDefaultIsAnEmptySlotNotAnUnavailableWidget() throws {
        for reason in ["package_unavailable", "quarantined", "package_identity_changed",
                       "package_digest_changed", "generation_rollback", "widget_no_longer_declared"] {
            let slot = try decodeSlot(hidden(source: "workspace_default", reason: reason))
            XCTAssertEqual(
                AppNativeSlotRefreshPolicy.presentation(assignment: slot, hasItem: false),
                .add,
                reason
            )
        }
    }

    func testDisabledOrUpdatePendingWorkspaceDefaultKeepsThePlaceholder() throws {
        for reason in ["disabled", "update_pending"] {
            let slot = try decodeSlot(hidden(source: "workspace_default", reason: reason))
            XCTAssertEqual(
                AppNativeSlotRefreshPolicy.presentation(assignment: slot, hasItem: false),
                .unavailable,
                reason
            )
        }
    }

    func testUserAssignmentWithoutARenderKeepsThePlaceholder() throws {
        let hiddenUser = try decodeSlot(hidden(source: "user", reason: "package_unavailable"))
        XCTAssertEqual(
            AppNativeSlotRefreshPolicy.presentation(assignment: hiddenUser, hasItem: false),
            .unavailable
        )
        let assignedUser = try decodeSlot(assigned(source: "user"))
        XCTAssertEqual(
            AppNativeSlotRefreshPolicy.presentation(assignment: assignedUser, hasItem: false),
            .unavailable
        )
    }

    // MARK: - Fixtures

    private let slotID = "page:2f:primary"
    private var digest: String { "blake3:" + String(repeating: "a", count: 64) }
    private var packageDigest: String { "blake3:" + String(repeating: "b", count: 64) }

    private func decodeSlot(_ value: [String: Any]) throws -> AppNativeResolvedSlot {
        try AppNativeSurfaceContract.decodeSlot(try encode(value))
    }

    private func hidden(source: String, reason: String) -> [String: Any] {
        [
            "slot_id": slotID,
            "source": source,
            "pinned_system_default": false,
            "opted_out": false,
            "hidden_reason": reason
        ]
    }

    private func assigned(source: String) -> [String: Any] {
        let binding: [String: Any] = [
            "package": [
                "installation_id": "install-1",
                "package_id": "app:planner",
                "package_revision_ref": "app-package-revision:one",
                "package_content_digest": packageDigest,
                "installation_generation": 4
            ],
            "widget_id": "plans"
        ]
        return [
            "slot_id": slotID,
            "source": source,
            "pinned_system_default": false,
            "opted_out": false,
            "widget": [
                "pinned": binding,
                "current": binding,
                "restored_across_generation": false,
                "assignment_compatibility": "exact_digest_only"
            ]
        ]
    }

    private func widgetBatch() -> [String: Any] {
        let row: [String: Any] = [
            "entity": "plan",
            "record_id": "plan-1",
            "record_revision": 3,
            "fields": ["title": "Ship native widgets"]
        ]
        let model: [String: Any] = [
            "model": "detail", "row": row, "hints": ["display_field": "title"], "actions": []
        ]
        func item(_ widgetID: String, refreshAfter: String) -> [String: Any] {
            [
                "installation_id": "install-1",
                "widget_id": widgetID,
                "title": "Plans",
                "installation_generation": 4,
                "revision": digest,
                "rendered_at": "2026-09-02T10:00:00Z",
                "refresh_after": refreshAfter,
                "state": "ready",
                "model": model
            ]
        }
        return [
            "schema_version": 1,
            "revision": digest,
            "etag": digest,
            "rendered_at": "2026-09-02T10:00:00Z",
            "refresh_after": "2026-09-02T10:01:00Z",
            "widgets": [
                item("plans", refreshAfter: "2026-09-02T10:01:00Z"),
                item("reviews", refreshAfter: "2026-09-02T10:05:00Z")
            ]
        ]
    }

    private func encode(_ value: Any) throws -> Data {
        try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    }
}
