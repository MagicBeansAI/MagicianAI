//  LTMWireRoundtripTests.swift
//  Live Thinking Map (LTM) — S1a wire round-trip verification, as XCTest.
//
//  Ported from the standalone `magios/ThinkingMapCanonicalTests/wire_roundtrip_main.swift`
//  dev harness. For each canonical fixture (the `LTMWireFixtures.*` constants,
//  generated from the Rust emitter) it:
//    1. decodes the JSON into the matching `LTM` type,
//    2. re-encodes,
//    3. asserts the re-encoded JSON is semantically equal to the original
//       (both parsed to `Any` + deep-compared, so key order / whitespace / f32
//       widening differences are ignored),
//    4. asserts decode∘encode∘decode is a fixed point (Equatable),
//    5. checks the field-level invariants.
//  Plus the FieldEdit clear/unchanged/set + `.solo` source wire-shape checks.

import XCTest
@testable import Magician

final class LTMWireRoundtripTests: XCTestCase {

    // MARK: - Semantic JSON equality (order/whitespace-insensitive)

    /// Deep structural equality between two JSON values, ignoring object key order.
    private func jsonEqual(_ a: Any, _ b: Any) -> Bool {
        switch (a, b) {
        case let (da as [String: Any], db as [String: Any]):
            guard da.count == db.count else { return false }
            for (k, va) in da {
                guard let vb = db[k] else { return false }
                if !jsonEqual(va, vb) { return false }
            }
            return true
        case let (aa as [Any], ab as [Any]):
            guard aa.count == ab.count else { return false }
            for i in 0..<aa.count where !jsonEqual(aa[i], ab[i]) { return false }
            return true
        case let (na as NSNumber, nb as NSNumber):
            // Booleans must match by identity (NSNumber conflates 0/1 with false/true).
            let aIsBool = CFGetTypeID(na) == CFBooleanGetTypeID()
            let bIsBool = CFGetTypeID(nb) == CFBooleanGetTypeID()
            if aIsBool || bIsBool { return aIsBool == bIsBool && na == nb }
            // Numeric tolerance: the wire carries f32 values widened to f64 by
            // serde's `to_value` (e.g. confidence `0.9` → `0.8999999761581421`),
            // while Swift decodes to `Float`/`Double` and re-encodes the clean form.
            // Compare via `doubleValue` with an epsilon scaled to the f32 floor.
            let da = na.doubleValue, db = nb.doubleValue
            let scale = max(1.0, max(abs(da), abs(db)))
            return abs(da - db) <= 1e-6 * scale
        case let (sa as String, sb as String):
            return sa == sb
        case (is NSNull, is NSNull):
            return true
        default:
            if let oa = a as? NSObject, let ob = b as? NSObject { return oa.isEqual(ob) }
            return false
        }
    }

    private func parseJSON(_ data: Data) -> Any {
        try! JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
    }

    // MARK: - Coders

    private let decoder = LTM.Wire.makeDecoder()
    private let encoder = LTM.Wire.makeEncoder()

    /// Decode `T` from `json`, re-encode, assert a semantic round-trip + Equatable
    /// fixed point, and return the decoded value for extra field assertions.
    private func roundTrip<T: Codable & Equatable>(
        _ json: String, as type: T.Type, _ label: String,
        file: StaticString = #filePath, line: UInt = #line
    ) throws -> T {
        let original = Data(json.utf8)
        let decoded = try decoder.decode(T.self, from: original)
        let reencoded = try encoder.encode(decoded)
        XCTAssertTrue(
            jsonEqual(parseJSON(original), parseJSON(reencoded)),
            "\(label): re-encodes to semantically-equal JSON", file: file, line: line)
        let decoded2 = try decoder.decode(T.self, from: reencoded)
        XCTAssertEqual(
            decoded, decoded2,
            "\(label): decode∘encode∘decode is stable (Equatable)", file: file, line: line)
        return decoded
    }

    // MARK: - Fixture round-trips

    func testMapRoundTripAndInvariants() throws {
        let map = try roundTrip(LTMWireFixtures.mapJSON, as: LTM.Map.self, "map")
        XCTAssertEqual(map.nodes["node-2"]?.assertionOrigin, .modelInferred)
        XCTAssertEqual(map.nodes["node-1"]?.assertionOrigin, .ownerSpoken)
        XCTAssertEqual(map.nodes["node-1"]?.epistemicState, .asserted)
        XCTAssertEqual(map.nodes["node-2"]?.epistemicState, .provisional)
        if case let .meeting(threadId) = map.source {
            XCTAssertEqual(threadId, "thread-42", "map.source is .meeting(thread-42)")
        } else {
            XCTFail("map.source should be .meeting")
        }
        XCTAssertEqual(map.viewState.lens, .outline)
        XCTAssertEqual(map.edges["edge-1"]?.kind, .relatedTo)
        XCTAssertEqual(map.clarifications["clar-1"]?.state, .open)
        XCTAssertNil(map.nodes["node-2"]?.detailMarkdown, "map.node-2.detailMarkdown absent → nil")
        XCTAssertEqual(map.nodes["node-1"]?.position?.y, 48.5)
        XCTAssertEqual(map.appliedEnvelopes.count, 1, "map.appliedEnvelopes has 1 record")
    }

    func testEnvelopeRoundTripAndInvariants() throws {
        let envelope = try roundTrip(
            LTMWireFixtures.envelopeJSON, as: LTM.OperationEnvelope.self, "envelope")
        if case let .owner(principal) = envelope.actor {
            XCTAssertEqual(principal, "anonymous", "envelope.actor is .owner(anonymous)")
        } else {
            XCTFail("envelope.actor should be .owner")
        }
        XCTAssertEqual(envelope.operations.count, 10, "envelope has 10 operations")
        XCTAssertTrue(
            envelope.operations.contains { if case .setTitle = $0 { return true }; return false },
            "envelope contains a set_title op")
        XCTAssertTrue(
            envelope.operations.contains { if case .setLifecycle = $0 { return true }; return false },
            "envelope contains a set_lifecycle op")
        for op in envelope.operations {
            if case let .updateNode(_, _, detailMarkdown, _) = op {
                if case let .set(value) = detailMarkdown {
                    XCTAssertEqual(value, "Updated detail body.",
                                   "update_node.detail_markdown == .set(\"…\")")
                } else {
                    XCTFail("update_node.detail_markdown should be .set")
                }
            }
        }
        XCTAssertEqual(envelope.modelTrace?.traceId, "trace-1")
    }

    func testSummaryRoundTripAndInvariants() throws {
        // Back-compat: an old server that never sent `node_preview` decodes with
        // `nodePreview == nil` and re-encodes WITHOUT the key.
        let summary = try roundTrip(LTMWireFixtures.summaryJSON, as: LTM.Summary.self, "summary")
        XCTAssertEqual(summary.mapId, "map-1")
        XCTAssertEqual(summary.lifecycle, .active)
        XCTAssertNil(summary.nodePreview, "absent node_preview → nil (back-compat)")
    }

    func testSummaryNodePreviewRoundTripAndInvariants() throws {
        let summary = try roundTrip(
            LTMWireFixtures.summaryWithPreviewJSON, as: LTM.Summary.self, "summary_with_preview")
        let preview = try XCTUnwrap(summary.nodePreview, "node_preview decodes when present")
        XCTAssertEqual(preview.nodes.count, 2)
        XCTAssertEqual(preview.edges.count, 1)
        // Root node: no parent, not suggested.
        XCTAssertNil(preview.nodes[0].parentId)
        XCTAssertFalse(preview.nodes[0].suggested)
        XCTAssertEqual(preview.nodes[0].kind, .idea)
        // Child node: parented to root, model-suggested question.
        XCTAssertEqual(preview.nodes[1].parentId, preview.nodes[0].nodeId)
        XCTAssertTrue(preview.nodes[1].suggested)
        XCTAssertEqual(preview.nodes[1].kind, .question)
        // Branch edge lines up with the two node ids.
        XCTAssertEqual(preview.edges[0].from, preview.nodes[0].nodeId)
        XCTAssertEqual(preview.edges[0].to, preview.nodes[1].nodeId)
    }

    func testEventRoundTripAndInvariants() throws {
        let event = try roundTrip(LTMWireFixtures.eventJSON, as: LTM.Event.self, "event")
        XCTAssertEqual(event.sequence, 1)
        XCTAssertEqual(event.envelope.operations.count, 10, "event.envelope has 10 operations")
        XCTAssertEqual(event.semanticHash, "sha256:deadbeefcafef00d")
    }

    func testManifestRoundTripAndInvariants() throws {
        let manifest = try roundTrip(LTMWireFixtures.manifestJSON, as: LTM.Manifest.self, "manifest")
        XCTAssertEqual(manifest.latestSequence, 1)
        XCTAssertNil(manifest.branchedFromMapId, "manifest.branchedFromMapId absent → nil")
        if case .meeting = manifest.source {
            // ok
        } else {
            XCTFail("manifest.source should be .meeting")
        }
    }

    func testResponseAppliedRoundTripAndInvariants() throws {
        let applied = try roundTrip(
            LTMWireFixtures.responseAppliedJSON, as: LTM.ApplyOutcome.self, "response_applied")
        if case let .applied(rev, hash, embeddedMap) = applied {
            XCTAssertEqual(rev, 5, "response_applied.resulting_revision == 5")
            XCTAssertEqual(hash, "sha256:deadbeefcafef00d")
            XCTAssertEqual(embeddedMap.mapId, "map-1")
        } else {
            XCTFail("response_applied should decode to .applied")
        }
    }

    func testResponseIdempotentRoundTripAndInvariants() throws {
        let idem = try roundTrip(
            LTMWireFixtures.responseIdempotentJSON, as: LTM.ApplyOutcome.self, "response_idempotent")
        if case let .idempotentReplay(rev) = idem {
            XCTAssertEqual(rev, 4, "response_idempotent.resulting_revision == 4")
        } else {
            XCTFail("response_idempotent should decode to .idempotentReplay")
        }
    }

    func testResponseNoOperationsRoundTrip() throws {
        let noop = try roundTrip(
            LTMWireFixtures.responseNoOperationsJSON, as: LTM.ApplyOutcome.self,
            "response_no_operations")
        XCTAssertEqual(noop, .noOperations)
    }

    // MARK: - FieldEdit clear vs unchanged vs set (wire-level)

    func testFieldEditClearVsUnchangedVsSet() throws {
        // detail_markdown:null → .clear ; re-encodes to null.
        let clearJSON = Data(#"{"op":"update_node","node_id":"n","detail_markdown":null}"#.utf8)
        let clearOp = try decoder.decode(LTM.Operation.self, from: clearJSON)
        guard case let .updateNode(_, _, clearEdit, _) = clearOp, case .clear = clearEdit else {
            return XCTFail("detail_markdown:null → .clear")
        }
        let clearOut = String(data: try encoder.encode(clearOp), encoding: .utf8)!
        XCTAssertTrue(clearOut.contains("\"detail_markdown\":null"),
                      ".clear re-encodes detail_markdown as null")

        // detail_markdown absent → .unchanged ; omitted on the wire.
        let unchangedJSON = Data(#"{"op":"update_node","node_id":"n"}"#.utf8)
        let unchangedOp = try decoder.decode(LTM.Operation.self, from: unchangedJSON)
        guard case let .updateNode(_, _, uEdit, _) = unchangedOp, case .unchanged = uEdit else {
            return XCTFail("detail_markdown absent → .unchanged")
        }
        let unchangedOut = String(data: try encoder.encode(unchangedOp), encoding: .utf8)!
        XCTAssertFalse(unchangedOut.contains("detail_markdown"),
                       ".unchanged omits detail_markdown from the wire")
    }

    // MARK: - solo source has no payload

    func testSoloSourceHasNoExtraFields() throws {
        let soloJSON = Data(#"{"kind":"solo"}"#.utf8)
        let solo = try decoder.decode(LTM.Source.self, from: soloJSON)
        XCTAssertEqual(solo, .solo, #"{"kind":"solo"} → .solo"#)
        let soloOut = String(data: try encoder.encode(LTM.Source.solo), encoding: .utf8)!
        XCTAssertTrue(
            jsonEqual(parseJSON(Data(soloOut.utf8)), parseJSON(soloJSON)),
            #".solo re-encodes to {"kind":"solo"}"#)
    }
}
