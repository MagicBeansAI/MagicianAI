//  wire_roundtrip_main.swift
//  Live Thinking Map (LTM) — standalone round-trip verification harness.
//
//  NOT part of the Magios app target. It lives OUTSIDE `magios/Magios/` (which
//  the project.yml `sources: [Magios, ...]` glob would otherwise pull in), so
//  this `@main` entry point never leaks into the app build.
//
//  Compile + run standalone with swiftc (NOT the Xcode project):
//
//    /usr/bin/swiftc -o /tmp/ltm_wire_test \
//        magios/Magios/ThinkingMapCanonical/*.swift \
//        magios/ThinkingMapCanonicalTests/wire_roundtrip_main.swift \
//      && /tmp/ltm_wire_test magios/Magios/ThinkingMapCanonical/Fixtures
//
//  For each fixture it:
//    1. reads Fixtures/<name>.json,
//    2. decodes into the matching `LTM` type,
//    3. re-encodes,
//    4. asserts the re-encoded JSON is *semantically equal* to the original
//       (normalized: both parsed to `Any` and deep-compared, so key order and
//       whitespace differences are ignored),
//    5. checks a few specific field-level invariants.
//
//  Wrapped in a `@main enum` (not top-level code) so the file name is free to be
//  descriptive — top-level statements are only permitted in a file literally
//  named `main.swift`.

import Foundation

// MARK: - Semantic JSON equality (order/whitespace-insensitive)

/// Deep structural equality between two JSON values, ignoring object key order.
/// Numbers are compared via `NSNumber` so `0.9` == `0.9` regardless of the
/// concrete Swift number type Foundation parsed them into.
func jsonEqual(_ a: Any, _ b: Any) -> Bool {
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
        // while Swift decodes to `Float`/`Double` and re-encodes the clean form
        // (`0.9`, and `120.0` → `120`). These are the SAME number, so compare via
        // `doubleValue` with an epsilon scaled to the f32 rounding floor.
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

func parseJSON(_ data: Data) -> Any {
    try! JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
}

// MARK: - Harness

@main
enum LTMWireRoundTrip {
    static var failures = 0
    static var checks = 0

    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        checks += 1
        if condition() {
            print("  ok: \(message)")
        } else {
            failures += 1
            print("  FAIL: \(message)")
        }
    }

    static var fixturesDir: URL {
        let args = CommandLine.arguments
        let path = args.count > 1 ? args[1] : "magios/Magios/ThinkingMapCanonical/Fixtures"
        return URL(fileURLWithPath: path, isDirectory: true)
    }

    static func loadFixture(_ name: String) -> Data {
        let url = fixturesDir.appendingPathComponent("\(name).json")
        guard let data = try? Data(contentsOf: url) else {
            fatalError("could not read fixture \(url.path)")
        }
        return data
    }

    static let decoder = LTM.Wire.makeDecoder()
    static let encoder = LTM.Wire.makeEncoder()

    /// Decode `T` from `<name>.json`, re-encode, assert semantic round-trip, and
    /// return the decoded value for extra field assertions.
    static func roundTrip<T: Codable & Equatable>(_ name: String, as type: T.Type) -> T {
        print("fixture: \(name).json")
        let original = loadFixture(name)
        let decoded: T
        do {
            decoded = try decoder.decode(T.self, from: original)
        } catch {
            failures += 1
            checks += 1
            print("  FAIL: decode \(name) as \(T.self): \(error)")
            fatalError("decode failed for \(name)")
        }
        let reencoded: Data
        do {
            reencoded = try encoder.encode(decoded)
        } catch {
            failures += 1
            checks += 1
            print("  FAIL: re-encode \(name): \(error)")
            fatalError("encode failed for \(name)")
        }
        check(jsonEqual(parseJSON(original), parseJSON(reencoded)),
              "\(name) re-encodes to semantically-equal JSON")

        // Decode-encode-decode must be a fixed point (structural stability).
        let decoded2 = try! decoder.decode(T.self, from: reencoded)
        check(decoded == decoded2, "\(name) decode∘encode∘decode is stable (Equatable)")
        return decoded
    }

    static func main() {
        print("== LTM canonical wire round-trip ==")
        print("fixtures: \(fixturesDir.path)\n")

        // 1. Map
        let map = roundTrip("map", as: LTM.Map.self)
        check(map.nodes["node-2"]?.assertionOrigin == .modelInferred,
              "map.node-2.assertionOrigin == .modelInferred")
        check(map.nodes["node-1"]?.assertionOrigin == .ownerSpoken,
              "map.node-1.assertionOrigin == .ownerSpoken")
        check(map.nodes["node-1"]?.epistemicState == .asserted,
              "map.node-1.epistemicState == .asserted")
        check(map.nodes["node-2"]?.epistemicState == .provisional,
              "map.node-2.epistemicState == .provisional")
        if case let .meeting(threadId) = map.source {
            check(threadId == "thread-42", "map.source is .meeting(thread-42)")
        } else {
            check(false, "map.source is .meeting")
        }
        check(map.viewState.lens == .outline, "map.viewState.lens == .outline")
        check(map.edges["edge-1"]?.kind == .relatedTo, "map.edge-1.kind == .relatedTo")
        check(map.clarifications["clar-1"]?.state == .open, "map.clar-1.state == .open")
        check(map.nodes["node-2"]?.detailMarkdown == nil, "map.node-2.detailMarkdown absent → nil")
        check(map.nodes["node-1"]?.position?.y == 48.5, "map.node-1.position.y == 48.5")
        check(map.appliedEnvelopes.count == 1, "map.appliedEnvelopes has 1 record")
        print("")

        // 2. Envelope — the operation spread
        let envelope = roundTrip("envelope", as: LTM.OperationEnvelope.self)
        if case let .owner(principal) = envelope.actor {
            check(principal == "anonymous", "envelope.actor is .owner(anonymous)")
        } else {
            check(false, "envelope.actor is .owner")
        }
        check(envelope.operations.count == 10, "envelope has 10 operations")
        check(envelope.operations.contains { if case .setTitle = $0 { return true }; return false },
              "envelope contains a set_title op")
        check(envelope.operations.contains { if case .setLifecycle = $0 { return true }; return false },
              "envelope contains a set_lifecycle op")
        for op in envelope.operations {
            if case let .updateNode(_, _, detailMarkdown, _) = op {
                if case let .set(value) = detailMarkdown {
                    check(value == "Updated detail body.",
                          "update_node.detail_markdown == .set(\"…\")")
                } else {
                    check(false, "update_node.detail_markdown should be .set")
                }
            }
        }
        check(envelope.modelTrace?.traceId == "trace-1", "envelope.modelTrace.traceId == trace-1")
        print("")

        // 3. Summary
        let summary = roundTrip("summary", as: LTM.Summary.self)
        check(summary.mapId == "map-1", "summary.mapId == map-1")
        check(summary.lifecycle == .active, "summary.lifecycle == .active")
        print("")

        // 4. Event (wraps an envelope)
        let event = roundTrip("event", as: LTM.Event.self)
        check(event.sequence == 1, "event.sequence == 1")
        check(event.envelope.operations.count == 10, "event.envelope has 10 operations")
        check(event.semanticHash == "sha256:deadbeefcafef00d", "event.semanticHash matches")
        print("")

        // 5. Manifest
        let manifest = roundTrip("manifest", as: LTM.Manifest.self)
        check(manifest.latestSequence == 1, "manifest.latestSequence == 1")
        check(manifest.branchedFromMapId == nil, "manifest.branchedFromMapId absent → nil")
        if case .meeting = manifest.source {
            check(true, "manifest.source is .meeting")
        } else {
            check(false, "manifest.source is .meeting")
        }
        print("")

        // 6. Response — applied (embeds a full map)
        let applied = roundTrip("response_applied", as: LTM.ApplyOutcome.self)
        if case let .applied(rev, hash, embeddedMap) = applied {
            check(rev == 5, "response_applied.resulting_revision == 5")
            check(hash == "sha256:deadbeefcafef00d", "response_applied.semantic_hash matches")
            check(embeddedMap.mapId == "map-1", "response_applied.map.mapId == map-1")
        } else {
            check(false, "response_applied decodes to .applied")
        }
        print("")

        // 7. Response — idempotent_replay
        let idem = roundTrip("response_idempotent", as: LTM.ApplyOutcome.self)
        if case let .idempotentReplay(rev) = idem {
            check(rev == 4, "response_idempotent.resulting_revision == 4")
        } else {
            check(false, "response_idempotent decodes to .idempotentReplay")
        }
        print("")

        // 8. Response — no_operations
        let noop = roundTrip("response_no_operations", as: LTM.ApplyOutcome.self)
        check(noop == .noOperations, "response_no_operations decodes to .noOperations")
        print("")

        // Extra: FieldEdit clear vs unchanged vs set (wire-level)
        print("fieldEdit: absent vs null vs value semantics")
        do {
            let clearJSON = Data(#"{"op":"update_node","node_id":"n","detail_markdown":null}"#.utf8)
            let clearOp = try decoder.decode(LTM.Operation.self, from: clearJSON)
            if case let .updateNode(_, _, edit, _) = clearOp, case .clear = edit {
                check(true, "detail_markdown:null → .clear")
            } else {
                check(false, "detail_markdown:null → .clear")
            }
            let clearOut = String(data: try encoder.encode(clearOp), encoding: .utf8)!
            check(clearOut.contains("\"detail_markdown\":null"),
                  ".clear re-encodes detail_markdown as null")

            let unchangedJSON = Data(#"{"op":"update_node","node_id":"n"}"#.utf8)
            let unchangedOp = try decoder.decode(LTM.Operation.self, from: unchangedJSON)
            if case let .updateNode(_, _, edit, _) = unchangedOp, case .unchanged = edit {
                check(true, "detail_markdown absent → .unchanged")
            } else {
                check(false, "detail_markdown absent → .unchanged")
            }
            let unchangedOut = String(data: try encoder.encode(unchangedOp), encoding: .utf8)!
            check(!unchangedOut.contains("detail_markdown"),
                  ".unchanged omits detail_markdown from the wire")
        } catch {
            check(false, "FieldEdit checks threw: \(error)")
        }
        print("")

        // Extra: solo source has no payload
        print("source: solo has no extra fields")
        do {
            let soloJSON = Data(#"{"kind":"solo"}"#.utf8)
            let solo = try decoder.decode(LTM.Source.self, from: soloJSON)
            check(solo == .solo, "{\"kind\":\"solo\"} → .solo")
            let soloOut = String(data: try encoder.encode(LTM.Source.solo), encoding: .utf8)!
            check(jsonEqual(parseJSON(Data(soloOut.utf8)), parseJSON(soloJSON)),
                  ".solo re-encodes to {\"kind\":\"solo\"}")
        } catch {
            check(false, "solo source check threw: \(error)")
        }
        print("")

        print("== result: \(checks - failures)/\(checks) checks passed ==")
        if failures == 0 {
            print("ALL FIXTURES VERIFIED")
            exit(0)
        } else {
            print("\(failures) FAILURE(S)")
            exit(1)
        }
    }
}
