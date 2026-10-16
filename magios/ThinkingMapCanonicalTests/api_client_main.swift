//  api_client_main.swift
//  Live Thinking Map (LTM) — S1b: standalone verification for `LTM.APIClient`.
//
//  NOT part of the Magios app target. It lives OUTSIDE `magios/Magios/` (which
//  the project.yml `sources:` glob would pull in) so this `@main` entry point
//  never leaks into the app build. It is ALSO compiled SEPARATELY from the S1a
//  `wire_roundtrip_main.swift` harness (which has its own `@main`) — only the
//  `ThinkingMapCanonical/*.swift` sources + THIS file, so there is exactly one
//  entry point.
//
//  Compile + run standalone with swiftc (NOT the Xcode project):
//
//    /usr/bin/swiftc -o /tmp/ltm_api_test \
//        magios/Magios/ThinkingMapCanonical/*.swift \
//        magios/ThinkingMapCanonicalTests/api_client_main.swift \
//      && /tmp/ltm_api_test magios/Magios/ThinkingMapCanonical/Fixtures
//
//  A `MockTransport` conforms to `LTM.Transport`, records the LAST `URLRequest`
//  the client produced, and returns a canned `(Data, HTTPURLResponse)` (loaded
//  from the S1a fixtures where they fit). For each client method we assert:
//    1. the REQUEST is correct — method, path (+id, +query), scope headers, and
//       (POST/PATCH) the body decodes back to the expected fields;
//    2. the RESPONSE decodes into the right `LTM` type;
//    3. error paths map to the right `LTM.APIError`.

import Foundation

// MARK: - Mock transport

/// A `Transport` that records the request it was handed and replies with a
/// pre-seeded `(Data, status)`. `send` also asserts a request was actually made.
final class MockTransport: LTM.Transport, @unchecked Sendable {
    /// The last request the client produced (captured for assertions).
    private(set) var lastRequest: URLRequest?
    /// Canned response bytes + status for the next `send`.
    var nextBody: Data = Data("{}".utf8)
    var nextStatus: Int = 200

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        lastRequest = request
        let http = HTTPURLResponse(
            url: request.url!, statusCode: nextStatus, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        return (nextBody, http)
    }

    func seed(_ body: Data, status: Int = 200) {
        nextBody = body
        nextStatus = status
    }
}

// MARK: - Harness

@main
enum LTMAPIClientTest {
    static var failures = 0
    static var checks = 0

    static let decoder = LTM.Wire.makeDecoder()
    static let baseURL = URL(string: "https://ios.example.test")!
    static let scope = LTM.Scope(principal: "anonymous", workspace: "default")

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

    /// Wrap N fixture JSON blobs into a top-level JSON array (for list endpoints).
    static func arrayFixture(_ names: [String]) -> Data {
        let inner = names.map { String(data: loadFixture($0), encoding: .utf8)! }
        return Data("[\(inner.joined(separator: ","))]".utf8)
    }

    // ── Request assertion helpers ──────────────────────────────────────────────

    static func assertScopeHeaders(_ req: URLRequest, _ label: String) {
        check(req.value(forHTTPHeaderField: "X-Principal") == nil,
              "\(label): no caller principal header")
        check(req.value(forHTTPHeaderField: "X-Workspace") == nil,
              "\(label): no caller workspace header")
    }

    static func assertMethodPath(
        _ req: URLRequest, method: String, path: String, _ label: String
    ) {
        check(req.httpMethod == method, "\(label): method == \(method)")
        check(req.url?.path == path, "\(label): path == \(path) (got \(req.url?.path ?? "nil"))")
    }

    static func queryValue(_ req: URLRequest, _ name: String) -> String? {
        guard let url = req.url,
              let comps = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        return comps.queryItems?.first { $0.item(name) }?.value
    }

    static func bodyObject(_ req: URLRequest) -> [String: Any] {
        guard let body = req.httpBody,
              let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return [:] }
        return obj
    }

    static func main() async {
        print("== LTM.APIClient standalone verification ==")
        print("baseURL: \(baseURL) scope: \(scope.principal)/\(scope.workspace)\n")

        let transport = MockTransport()
        let client = LTM.APIClient(baseURL: baseURL, scope: scope, transport: transport)

        // 1. createMap — POST /thinking-maps, body {title, source, map_id}, 201 → Map
        print("createMap:")
        transport.seed(loadFixture("map"), status: 201)
        do {
            let map = try await client.createMap(
                title: "iOS thinking map migration",
                source: .meeting(threadId: "thread-42"),
                mapID: "map-1")
            let req = transport.lastRequest!
            assertMethodPath(req, method: "POST", path: "/api/magician/v2/thinking-maps", "createMap")
            assertScopeHeaders(req, "createMap")
            check(req.value(forHTTPHeaderField: "Content-Type") == "application/json",
                  "createMap: Content-Type == application/json")
            let body = bodyObject(req)
            check(body["title"] as? String == "iOS thinking map migration",
                  "createMap: body.title round-trips")
            check(body["map_id"] as? String == "map-1", "createMap: body.map_id round-trips")
            check((body["source"] as? [String: Any])?["kind"] as? String == "meeting",
                  "createMap: body.source.kind == meeting")
            check((body["source"] as? [String: Any])?["thread_id"] as? String == "thread-42",
                  "createMap: body.source.thread_id == thread-42")
            check(map.mapId == "map-1", "createMap: response decodes to Map(map-1)")
            check(map.revision == 4, "createMap: response Map.revision == 4")
        } catch {
            check(false, "createMap threw: \(error)")
        }
        // createMap with nil source/mapID omits those keys.
        transport.seed(loadFixture("map"), status: 201)
        do {
            _ = try await client.createMap(title: "Solo", source: nil, mapID: nil)
            let body = bodyObject(transport.lastRequest!)
            check(body["source"] == nil, "createMap: nil source omits the key")
            check(body["map_id"] == nil, "createMap: nil mapID omits the key")
            check(body["title"] as? String == "Solo", "createMap: title still present")
        } catch {
            check(false, "createMap(nil,nil) threw: \(error)")
        }
        print("")

        // 2. listMaps — GET /thinking-maps → [Summary]
        print("listMaps:")
        transport.seed(arrayFixture(["summary", "summary"]))
        do {
            let summaries = try await client.listMaps()
            let req = transport.lastRequest!
            assertMethodPath(req, method: "GET", path: "/api/magician/v2/thinking-maps", "listMaps")
            assertScopeHeaders(req, "listMaps")
            check(req.httpBody == nil, "listMaps: GET has no body")
            check(summaries.count == 2, "listMaps: decodes 2 summaries")
            check(summaries.first?.mapId == "map-1", "listMaps: summary[0].mapId == map-1")
            check(summaries.first?.lifecycle == .active, "listMaps: summary[0].lifecycle == .active")
        } catch {
            check(false, "listMaps threw: \(error)")
        }
        print("")

        // 3. getMap — GET /thinking-maps/{id} → Map ; 404 not_found → .notFound
        print("getMap:")
        transport.seed(loadFixture("map"))
        do {
            let map = try await client.getMap("map-1")
            let req = transport.lastRequest!
            assertMethodPath(req, method: "GET", path: "/api/magician/v2/thinking-maps/map-1", "getMap")
            assertScopeHeaders(req, "getMap")
            check(map.mapId == "map-1", "getMap: decodes Map(map-1)")
            check(map.revision == 4, "getMap: Map.revision == 4 (fixture)")
        } catch {
            check(false, "getMap threw: \(error)")
        }
        // id needing percent-encoding.
        transport.seed(loadFixture("map"))
        do {
            _ = try await client.getMap("a b/c")
            let req = transport.lastRequest!
            check(req.url?.path == "/api/magician/v2/thinking-maps/a%20b%2Fc",
                  "getMap: id 'a b/c' percent-encodes the segment (got \(req.url?.path ?? "nil"))")
        } catch {
            check(false, "getMap(encoded) threw: \(error)")
        }
        // 404 not_found → .notFound
        transport.seed(Data(#"{"error":"not_found","map_id":"ghost"}"#.utf8), status: 404)
        do {
            _ = try await client.getMap("ghost")
            check(false, "getMap(ghost) should throw .notFound")
        } catch LTM.APIError.notFound {
            check(true, "getMap 404 not_found → .notFound")
        } catch {
            check(false, "getMap(ghost) threw wrong error: \(error)")
        }
        // 404 feature_disabled → .featureDisabled
        transport.seed(Data(#"{"error":"feature_disabled"}"#.utf8), status: 404)
        do {
            _ = try await client.getMap("map-1")
            check(false, "getMap should throw .featureDisabled when off")
        } catch LTM.APIError.featureDisabled {
            check(true, "getMap 404 feature_disabled → .featureDisabled")
        } catch {
            check(false, "getMap(disabled) threw wrong error: \(error)")
        }
        print("")

        // 4. patchMap — PATCH /thinking-maps/{id}, body {title?, lifecycle?} → ApplyOutcome
        print("patchMap:")
        transport.seed(loadFixture("response_applied"))
        do {
            let outcome = try await client.patchMap("map-1", title: "New title", lifecycle: .archived)
            let req = transport.lastRequest!
            assertMethodPath(req, method: "PATCH", path: "/api/magician/v2/thinking-maps/map-1", "patchMap")
            assertScopeHeaders(req, "patchMap")
            let body = bodyObject(req)
            check(body["title"] as? String == "New title", "patchMap: body.title round-trips")
            check(body["lifecycle"] as? String == "archived",
                  "patchMap: body.lifecycle == archived (snake_case)")
            if case let .applied(rev, _, embeddedMap) = outcome {
                check(rev == 5, "patchMap: outcome .applied resulting_revision == 5")
                check(embeddedMap.mapId == "map-1", "patchMap: outcome.map.mapId == map-1")
            } else {
                check(false, "patchMap: outcome should be .applied")
            }
        } catch {
            check(false, "patchMap threw: \(error)")
        }
        // patchMap nil title omits it.
        transport.seed(loadFixture("response_applied"))
        do {
            _ = try await client.patchMap("map-1", title: nil, lifecycle: .paused)
            let body = bodyObject(transport.lastRequest!)
            check(body["title"] == nil, "patchMap: nil title omits the key")
            check(body["lifecycle"] as? String == "paused", "patchMap: lifecycle present")
        } catch {
            check(false, "patchMap(nil title) threw: \(error)")
        }
        // 400 nothing_to_patch → .nothingToPatch
        transport.seed(Data(#"{"error":"nothing_to_patch"}"#.utf8), status: 400)
        do {
            _ = try await client.patchMap("map-1", title: nil, lifecycle: nil)
            check(false, "patchMap(nil,nil) should throw .nothingToPatch")
        } catch LTM.APIError.nothingToPatch {
            check(true, "patchMap 400 nothing_to_patch → .nothingToPatch")
        } catch {
            check(false, "patchMap(nil,nil) threw wrong error: \(error)")
        }
        print("")

        // 5. applyOperations — POST /{id}/operations → ApplyOutcome
        print("applyOperations:")
        transport.seed(loadFixture("response_applied"))
        let addNode = LTM.Operation.addNode(node: LTM.Node(
            nodeId: "n1", kind: .idea, label: "hello",
            epistemicState: .provisional, assertionOrigin: .ownerSpoken, confidence: 0.5,
            createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z"))
        do {
            let outcome = try await client.applyOperations(
                "map-1", operations: [addNode, .setTitle(title: "T")],
                baseRevision: 4, idempotencyKey: "idem-1",
                envelopeID: "env-1", utteranceID: "utt-1")
            let req = transport.lastRequest!
            assertMethodPath(req, method: "POST",
                             path: "/api/magician/v2/thinking-maps/map-1/operations", "applyOperations")
            assertScopeHeaders(req, "applyOperations")
            let body = bodyObject(req)
            check(body["idempotency_key"] as? String == "idem-1",
                  "applyOperations: body.idempotency_key round-trips")
            check((body["base_revision"] as? NSNumber)?.uint64Value == 4,
                  "applyOperations: body.base_revision == 4")
            check(body["envelope_id"] as? String == "env-1", "applyOperations: body.envelope_id")
            check(body["utterance_id"] as? String == "utt-1", "applyOperations: body.utterance_id")
            let ops = body["operations"] as? [[String: Any]] ?? []
            check(ops.count == 2, "applyOperations: body.operations has 2 entries")
            check(ops.first?["op"] as? String == "add_node",
                  "applyOperations: operations[0].op == add_node")
            check((ops.first?["node"] as? [String: Any])?["node_id"] as? String == "n1",
                  "applyOperations: operations[0].node.node_id == n1")
            // Round-trip the ops back through the canonical decoder as a strong check.
            let opsData = try JSONSerialization.data(withJSONObject: body["operations"]!)
            let decodedOps = try decoder.decode([LTM.Operation].self, from: opsData)
            check(decodedOps.count == 2 && decodedOps[0] == addNode,
                  "applyOperations: body.operations decode back to the sent LTM.Operation values")
            if case .applied = outcome {
                check(true, "applyOperations: outcome decodes .applied")
            } else {
                check(false, "applyOperations: outcome should be .applied")
            }
        } catch {
            check(false, "applyOperations threw: \(error)")
        }
        // envelope_id/utterance_id nil omitted; empty ops array still present.
        transport.seed(loadFixture("response_idempotent"))
        do {
            let outcome = try await client.applyOperations(
                "map-1", operations: [], baseRevision: 0, idempotencyKey: "k",
                envelopeID: nil, utteranceID: nil)
            let body = bodyObject(transport.lastRequest!)
            check(body["envelope_id"] == nil, "applyOperations: nil envelopeID omits the key")
            check(body["utterance_id"] == nil, "applyOperations: nil utteranceID omits the key")
            check((body["operations"] as? [Any])?.isEmpty == true,
                  "applyOperations: empty operations still sent as []")
            if case let .idempotentReplay(rev) = outcome {
                check(rev == 4, "applyOperations: idempotent_replay outcome (rev 4)")
            } else {
                check(false, "applyOperations: outcome should be .idempotentReplay")
            }
        } catch {
            check(false, "applyOperations(empty) threw: \(error)")
        }
        // 400 validation_failed → .validationFailed
        transport.seed(Data(#"{"error":"validation_failed","details":"x"}"#.utf8), status: 400)
        do {
            _ = try await client.applyOperations(
                "map-1", operations: [addNode], baseRevision: 0, idempotencyKey: "k",
                envelopeID: nil, utteranceID: nil)
            check(false, "applyOperations should throw .validationFailed")
        } catch LTM.APIError.validationFailed {
            check(true, "applyOperations 400 validation_failed → .validationFailed")
        } catch {
            check(false, "applyOperations(bad) threw wrong error: \(error)")
        }
        print("")

        // 6. interpret — POST /{id}/interpret → ApplyOutcome (no_operations / 503)
        print("interpret:")
        transport.seed(loadFixture("response_no_operations"))
        do {
            let outcome = try await client.interpret(
                "map-1", text: "ship v1", utteranceID: "utt-9",
                threadID: "thread-42", intent: .breakOpen, focusNodeID: "node-7")
            let req = transport.lastRequest!
            assertMethodPath(req, method: "POST",
                             path: "/api/magician/v2/thinking-maps/map-1/interpret", "interpret")
            assertScopeHeaders(req, "interpret")
            let body = bodyObject(req)
            check(body["text"] as? String == "ship v1", "interpret: body.text round-trips")
            check(body["utterance_id"] as? String == "utt-9", "interpret: body.utterance_id")
            check(body["thread_id"] as? String == "thread-42", "interpret: body.thread_id")
            check(body["intent"] as? String == "break_open",
                  "interpret: body.intent == break_open (snake_case wire value)")
            check(body["focus_node_id"] as? String == "node-7",
                  "interpret: body.focus_node_id carries selected branch")
            check(outcome == .noOperations,
                  "interpret: no_operations body → ApplyOutcome.noOperations")
        } catch {
            check(false, "interpret threw: \(error)")
        }
        // default intent value + nil optionals.
        transport.seed(loadFixture("response_no_operations"))
        do {
            _ = try await client.interpret(
                "map-1", text: "hi", utteranceID: nil, threadID: nil, intent: .continueThinking)
            let body = bodyObject(transport.lastRequest!)
            check(body["intent"] as? String == "continue_thinking",
                  "interpret: continueThinking → 'continue_thinking'")
            check(body["utterance_id"] == nil, "interpret: nil utteranceID omitted")
            check(body["thread_id"] == nil, "interpret: nil threadID omitted")
            check(body["focus_node_id"] == nil, "interpret: nil focusNodeID omitted")
        } catch {
            check(false, "interpret(defaults) threw: \(error)")
        }
        // 503 → .llmUnavailable
        transport.seed(Data(#"{"error":"llm_unavailable"}"#.utf8), status: 503)
        do {
            _ = try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil, intent: .continueThinking)
            check(false, "interpret should throw .llmUnavailable on 503")
        } catch LTM.APIError.llmUnavailable {
            check(true, "interpret 503 → .llmUnavailable")
        } catch {
            check(false, "interpret(503) threw wrong error: \(error)")
        }
        // 502 interpretation_failed → .interpretationFailed
        transport.seed(Data(#"{"error":"interpretation_failed","details":"x"}"#.utf8), status: 502)
        do {
            _ = try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil, intent: .continueThinking)
            check(false, "interpret should throw .interpretationFailed on 502")
        } catch LTM.APIError.interpretationFailed {
            check(true, "interpret 502 → .interpretationFailed")
        } catch {
            check(false, "interpret(502) threw wrong error: \(error)")
        }
        // A stale/unknown request-scoped branch gets a typed, user-readable error.
        transport.seed(Data(#"{"error":"invalid_focus_node"}"#.utf8), status: 400)
        do {
            _ = try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil,
                intent: .breakOpen, focusNodeID: "missing")
            check(false, "interpret should throw .invalidFocusNode on 400")
        } catch LTM.APIError.invalidFocusNode {
            check(true, "interpret 400 invalid_focus_node → .invalidFocusNode")
        } catch {
            check(false, "interpret(invalid focus) threw wrong error: \(error)")
        }
        print("")

        // 7. events — GET /{id}/events?after_seq=N → [Event]
        print("events:")
        transport.seed(arrayFixture(["event"]))
        do {
            let events = try await client.events("map-1", afterSeq: 3)
            let req = transport.lastRequest!
            assertMethodPath(req, method: "GET",
                             path: "/api/magician/v2/thinking-maps/map-1/events", "events")
            assertScopeHeaders(req, "events")
            check(queryValue(req, "after_seq") == "3", "events: query after_seq == 3")
            check(events.count == 1, "events: decodes 1 event")
            check(events.first?.sequence == 1, "events: event[0].sequence == 1")
            check(events.first?.envelope.operations.count == 10,
                  "events: event[0].envelope has 10 operations")
        } catch {
            check(false, "events threw: \(error)")
        }
        print("")

        // 8. replay — GET /{id}/replay?at_seq=N → Map
        print("replay:")
        transport.seed(loadFixture("map"))
        do {
            let map = try await client.replay("map-1", atSeq: 2)
            let req = transport.lastRequest!
            assertMethodPath(req, method: "GET",
                             path: "/api/magician/v2/thinking-maps/map-1/replay", "replay")
            assertScopeHeaders(req, "replay")
            check(queryValue(req, "at_seq") == "2", "replay: query at_seq == 2")
            check(map.mapId == "map-1", "replay: decodes Map(map-1)")
        } catch {
            check(false, "replay threw: \(error)")
        }
        print("")

        // 9. restore — POST /{id}/restore, body {at_sequence,new_map_id,new_title}, 201 → Map
        print("restore:")
        transport.seed(loadFixture("map"), status: 201)
        do {
            let branch = try await client.restore(
                "map-1", atSequence: 1, newMapID: "branch1", newTitle: "Forked")
            let req = transport.lastRequest!
            assertMethodPath(req, method: "POST",
                             path: "/api/magician/v2/thinking-maps/map-1/restore", "restore")
            assertScopeHeaders(req, "restore")
            let body = bodyObject(req)
            check((body["at_sequence"] as? NSNumber)?.uint64Value == 1,
                  "restore: body.at_sequence == 1")
            check(body["new_map_id"] as? String == "branch1", "restore: body.new_map_id")
            check(body["new_title"] as? String == "Forked", "restore: body.new_title")
            check(branch.mapId == "map-1", "restore: response decodes to a Map")
        } catch {
            check(false, "restore threw: \(error)")
        }
        // 409 already_exists → .alreadyExists
        transport.seed(Data(#"{"error":"already_exists","map_id":"branch1"}"#.utf8), status: 409)
        do {
            _ = try await client.restore(
                "map-1", atSequence: 1, newMapID: "branch1", newTitle: "Forked")
            check(false, "restore should throw .alreadyExists on 409")
        } catch LTM.APIError.alreadyExists {
            check(true, "restore 409 already_exists → .alreadyExists")
        } catch {
            check(false, "restore(409) threw wrong error: \(error)")
        }
        print("")

        // 10. Generic error mapping — missing_scope + unrecognized code fallback.
        print("error mapping:")
        transport.seed(Data(#"{"error":"missing_scope"}"#.utf8), status: 400)
        do {
            _ = try await client.listMaps()
            check(false, "listMaps should throw .missingScope on 400 missing_scope")
        } catch LTM.APIError.missingScope {
            check(true, "400 missing_scope → .missingScope")
        } catch {
            check(false, "missing_scope threw wrong error: \(error)")
        }
        transport.seed(Data(#"{"error":"io_error","details":"disk"}"#.utf8), status: 500)
        do {
            _ = try await client.getMap("map-1")
            check(false, "getMap should throw .ioError on 500 io_error")
        } catch LTM.APIError.ioError {
            check(true, "500 io_error → .ioError")
        } catch {
            check(false, "io_error threw wrong error: \(error)")
        }
        transport.seed(Data(#"{"error":"teapot"}"#.utf8), status: 418)
        do {
            _ = try await client.getMap("map-1")
            check(false, "getMap should throw .http on an unrecognized status/code")
        } catch let LTM.APIError.http(status, code) {
            check(status == 418 && code == "teapot",
                  "unrecognized 418 teapot → .http(418, \"teapot\")")
        } catch {
            check(false, "unrecognized error threw wrong case: \(error)")
        }
        // A non-2xx with no JSON body → .http(status, nil).
        transport.seed(Data("not json".utf8), status: 500)
        do {
            _ = try await client.getMap("map-1")
            check(false, "getMap should throw on a 500 with a non-JSON body")
        } catch let LTM.APIError.http(status, code) {
            check(status == 500 && code == nil, "500 non-JSON body → .http(500, nil)")
        } catch LTM.APIError.ioError {
            check(false, "500 non-JSON body should be .http(500, nil), not .ioError")
        } catch {
            check(false, "500 non-JSON threw wrong case: \(error)")
        }
        // A malformed success body → .decoding.
        transport.seed(Data(#"{"unexpected":"shape"}"#.utf8), status: 200)
        do {
            _ = try await client.getMap("map-1")
            check(false, "getMap should throw .decoding on a malformed 200 body")
        } catch let LTM.APIError.decoding(detail) {
            check(!detail.isEmpty, "malformed 200 body → .decoding")
        } catch {
            check(false, "malformed 200 threw wrong case: \(error)")
        }
        print("")

        print("== result: \(checks - failures)/\(checks) checks passed ==")
        if failures == 0 {
            print("ALL API-CLIENT CHECKS VERIFIED")
            exit(0)
        } else {
            print("\(failures) FAILURE(S)")
            exit(1)
        }
    }
}

// MARK: - Small helpers

private extension URLQueryItem {
    /// Name-only match helper (keeps the call sites terse).
    func item(_ name: String) -> Bool { self.name == name }
}
