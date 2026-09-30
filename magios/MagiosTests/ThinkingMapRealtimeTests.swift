//
//  ThinkingMapRealtimeTests.swift
//  MagiosTests
//
//  Routing tests for the ThinkingMapUpdated push subscriber: crafted realtime
//  frames are fed straight into `handleIncomingJSON` (no socket — the class is
//  inert under tests by design), asserting the map-id + event-type filter.
//

import XCTest
@testable import Magician

@MainActor
final class ThinkingMapRealtimeTests: XCTestCase {

    private func frame(type: String, mapID: String?) -> String {
        var data: [String: Any] = ["revision": 7, "principal": "anonymous", "workspace": "default"]
        if let mapID { data["map_id"] = mapID }
        let object: [String: Any] = ["event_type": type, "data": data]
        let encoded = try! JSONSerialization.data(withJSONObject: object)
        return String(data: encoded, encoding: .utf8)!
    }

    func testNoticeFiresOnlyForMatchingMapAndType() {
        let realtime = ThinkingMapRealtime()
        var notices = 0
        realtime.start(mapID: "map-1") { notices += 1 }

        // Matching notice → fires.
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-1"))
        XCTAssertEqual(notices, 1)

        // A different map's notice → ignored.
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-2"))
        XCTAssertEqual(notices, 1)

        // A different event type → ignored.
        realtime.handleIncomingJSON(frame(type: "ChatMessageReceived", mapID: "map-1"))
        XCTAssertEqual(notices, 1)

        // Payload missing the map id → ignored.
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: nil))
        XCTAssertEqual(notices, 1)

        // Non-JSON noise → ignored, no crash.
        realtime.handleIncomingJSON("not json at all")
        XCTAssertEqual(notices, 1)

        // A second matching notice still fires (no dedup at this layer — the
        // authoritative getMap is idempotent).
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-1"))
        XCTAssertEqual(notices, 2)
    }

    func testStopSilencesNotices() {
        let realtime = ThinkingMapRealtime()
        var notices = 0
        realtime.start(mapID: "map-1") { notices += 1 }
        realtime.stop()
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-1"))
        XCTAssertEqual(notices, 0)
        XCTAssertFalse(realtime.active)
    }

    func testRestartRepointsToNewMap() {
        let realtime = ThinkingMapRealtime()
        var first = 0
        var second = 0
        realtime.start(mapID: "map-1") { first += 1 }
        realtime.start(mapID: "map-2") { second += 1 }

        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-1"))
        realtime.handleIncomingJSON(frame(type: "ThinkingMapUpdated", mapID: "map-2"))
        XCTAssertEqual(first, 0, "the replaced subscription must not fire")
        XCTAssertEqual(second, 1)
    }

    // MARK: - Interpretation progress

    private func progressFrame(
        mapID: String,
        utteranceID: String?,
        stage: String?,
        nodeCount: Int? = nil
    ) -> String {
        var data: [String: Any] = ["map_id": mapID, "principal": "anonymous", "workspace": "default"]
        if let utteranceID { data["utterance_id"] = utteranceID }
        if let stage { data["stage"] = stage }
        if let nodeCount { data["node_count"] = nodeCount }
        let object: [String: Any] = ["event_type": "ThinkingMapInterpretProgress", "data": data]
        let encoded = try! JSONSerialization.data(withJSONObject: object)
        return String(data: encoded, encoding: .utf8)!
    }

    func testProgressRoutesWithRunIDStageAndCount() {
        let realtime = ThinkingMapRealtime()
        var received: [(String, String, Int?)] = []
        realtime.start(
            mapID: "map-1",
            onNotice: {},
            onInterpretProgress: { received.append(($0, $1, $2)) })

        realtime.handleIncomingJSON(
            progressFrame(mapID: "map-1", utteranceID: "u1", stage: "preparing", nodeCount: 34))
        XCTAssertEqual(received.count, 1)
        XCTAssertEqual(received[0].0, "u1")
        XCTAssertEqual(received[0].1, "preparing")
        XCTAssertEqual(received[0].2, 34)

        // Another map's narration is not this subscriber's.
        realtime.handleIncomingJSON(
            progressFrame(mapID: "map-2", utteranceID: "u1", stage: "shaping"))
        XCTAssertEqual(received.count, 1)

        // A frame without its run id can never be claimed by anyone: dropped.
        realtime.handleIncomingJSON(
            progressFrame(mapID: "map-1", utteranceID: nil, stage: "shaping"))
        XCTAssertEqual(received.count, 1)

        // The stage string rides through verbatim — mapping (and rejecting
        // strangers) is the subscriber's job, so a new server stage reaches it.
        realtime.handleIncomingJSON(
            progressFrame(mapID: "map-1", utteranceID: "u1", stage: "brand_new_stage"))
        XCTAssertEqual(received.count, 2)
        XCTAssertEqual(received[1].1, "brand_new_stage")
        XCTAssertNil(received[1].2, "no count on a non-preparing stage")
    }

    func testProgressWithoutSubscriberDoesNotCrossFireNotices() {
        let realtime = ThinkingMapRealtime()
        var notices = 0
        // Notice-only subscription, as the model's Listen mode uses.
        realtime.start(mapID: "map-1") { notices += 1 }
        realtime.handleIncomingJSON(
            progressFrame(mapID: "map-1", utteranceID: "u1", stage: "facilitating"))
        XCTAssertEqual(notices, 0, "a progress frame is not an update notice")
    }

    func testWireStagesMapOntoDisplayStatesAndStrangersDoNot() {
        XCTAssertEqual(
            ThinkingMapIntelligenceProgress.fromWire("preparing", nodeCount: 12),
            .preparing(12))
        XCTAssertEqual(
            ThinkingMapIntelligenceProgress.fromWire("loading_context", nodeCount: nil),
            .loadingContext)
        XCTAssertEqual(
            ThinkingMapIntelligenceProgress.fromWire("facilitating", nodeCount: nil),
            .facilitating)
        XCTAssertEqual(ThinkingMapIntelligenceProgress.fromWire("parsing", nodeCount: nil), .parsing)
        XCTAssertEqual(ThinkingMapIntelligenceProgress.fromWire("shaping", nodeCount: nil), .shaping)
        XCTAssertEqual(ThinkingMapIntelligenceProgress.fromWire("idle", nodeCount: nil), .idle)
        // The two labels this enum used to carry for work no server step backs
        // are strangers now, exactly like any future stage this build predates.
        XCTAssertNil(ThinkingMapIntelligenceProgress.fromWire("grounding", nodeCount: nil))
        XCTAssertNil(ThinkingMapIntelligenceProgress.fromWire("opening_thread", nodeCount: nil))
    }

    func testPreparingDetailComposesTheCountAndRefusesZero() {
        XCTAssertTrue(
            ThinkingMapIntelligenceProgress.preparing(34).detail.contains("34 thoughts"))
        XCTAssertTrue(
            ThinkingMapIntelligenceProgress.preparing(1).detail.contains("1 thought "))
        // No count, or an empty board: the generic line. "Reading 0 thoughts"
        // reads as a bug, not a board.
        XCTAssertFalse(ThinkingMapIntelligenceProgress.preparing(nil).detail.contains("0"))
        XCTAssertFalse(ThinkingMapIntelligenceProgress.preparing(0).detail.contains("0"))
    }
}
