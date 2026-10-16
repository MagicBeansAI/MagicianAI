//  LTMAPIClientTests.swift
//  Live Thinking Map (LTM) — S1b `LTM.APIClient` verification, as XCTest.
//
//  Ported from `magios/ThinkingMapCanonicalTests/api_client_main.swift`. A
//  `MockTransport: LTM.Transport` records the last request and returns canned
//  `(Data, HTTPURLResponse)` (bodies from the `LTMWireFixtures` constants). For
//  each endpoint we assert the REQUEST (method/path/query/headers/body), the
//  RESPONSE decoding, and the error-path mapping.

import XCTest
@testable import Magician

// MARK: - Mock transport

/// A `Transport` that records the request it was handed and replies with a
/// pre-seeded `(Data, status)`.
private final class MockTransport: LTM.Transport, @unchecked Sendable {
    private(set) var lastRequest: URLRequest?
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

final class LTMAPIClientTests: XCTestCase {

    private let decoder = LTM.Wire.makeDecoder()
    private let baseURL = URL(string: "https://ios.example.test")!
    private let scope = LTM.Scope(principal: "anonymous", workspace: "default")

    private var transport: MockTransport!
    private var client: LTM.APIClient!

    override func setUp() {
        super.setUp()
        transport = MockTransport()
        client = LTM.APIClient(baseURL: baseURL, scope: scope, transport: transport)
    }

    // MARK: - Fixture helpers

    private func fixture(_ json: String) -> Data { Data(json.utf8) }

    /// Wrap N fixture JSON blobs into a top-level JSON array (for list endpoints).
    private func arrayFixture(_ jsons: [String]) -> Data {
        Data("[\(jsons.joined(separator: ","))]".utf8)
    }

    // MARK: - Request assertion helpers

    private func assertScopeHeaders(
        _ req: URLRequest, file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertNil(req.value(forHTTPHeaderField: "X-Principal"), file: file, line: line)
        XCTAssertNil(req.value(forHTTPHeaderField: "X-Workspace"), file: file, line: line)
    }

    private func assertMethodPath(
        _ req: URLRequest, method: String, path: String,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertEqual(req.httpMethod, method, "method", file: file, line: line)
        XCTAssertEqual(req.url?.path, path, "path", file: file, line: line)
    }

    private func queryValue(_ req: URLRequest, _ name: String) -> String? {
        guard let url = req.url,
              let comps = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        return comps.queryItems?.first { $0.name == name }?.value
    }

    private func bodyObject(_ req: URLRequest) -> [String: Any] {
        guard let body = req.httpBody,
              let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return [:] }
        return obj
    }

    // MARK: - 1. createMap

    func testCreateMap() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON), status: 201)
        let map = try await client.createMap(
            title: "iOS thinking map migration",
            source: .meeting(threadId: "thread-42"),
            mapID: "map-1")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "POST", path: "/api/magician/v2/thinking-maps")
        assertScopeHeaders(req)
        XCTAssertEqual(req.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = bodyObject(req)
        XCTAssertEqual(body["title"] as? String, "iOS thinking map migration")
        XCTAssertEqual(body["map_id"] as? String, "map-1")
        XCTAssertEqual((body["source"] as? [String: Any])?["kind"] as? String, "meeting")
        XCTAssertEqual((body["source"] as? [String: Any])?["thread_id"] as? String, "thread-42")
        XCTAssertEqual(map.mapId, "map-1")
        XCTAssertEqual(map.revision, 4)
    }

    func testCreateMapNilSourceAndIDOmitKeys() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON), status: 201)
        _ = try await client.createMap(title: "Solo", source: nil, mapID: nil)
        let body = bodyObject(transport.lastRequest!)
        XCTAssertNil(body["source"], "nil source omits the key")
        XCTAssertNil(body["map_id"], "nil mapID omits the key")
        XCTAssertEqual(body["title"] as? String, "Solo")
    }

    // MARK: - 2. listMaps

    func testListMaps() async throws {
        transport.seed(arrayFixture([LTMWireFixtures.summaryJSON, LTMWireFixtures.summaryJSON]))
        let summaries = try await client.listMaps()
        let req = transport.lastRequest!
        assertMethodPath(req, method: "GET", path: "/api/magician/v2/thinking-maps")
        assertScopeHeaders(req)
        XCTAssertNil(req.httpBody, "listMaps: GET has no body")
        XCTAssertEqual(summaries.count, 2)
        XCTAssertEqual(summaries.first?.mapId, "map-1")
        XCTAssertEqual(summaries.first?.lifecycle, .active)
    }

    func testListMapsPaginated() async throws {
        transport.seed(fixture(
            """
            {"maps": [\(LTMWireFixtures.summaryJSON)], "total": 7, "offset": 4, "limit": 2}
            """))
        let page = try await client.listMaps(limit: 2, offset: 4)
        let req = transport.lastRequest!
        assertMethodPath(req, method: "GET", path: "/api/magician/v2/thinking-maps")
        assertScopeHeaders(req)
        XCTAssertEqual(queryValue(req, "limit"), "2")
        XCTAssertEqual(queryValue(req, "offset"), "4")
        XCTAssertEqual(page.total, 7)
        XCTAssertEqual(page.offset, 4)
        XCTAssertEqual(page.limit, 2)
        XCTAssertEqual(page.maps.count, 1)
        XCTAssertEqual(page.maps.first?.mapId, "map-1")
    }

    // MARK: - 3. getMap

    func testGetMap() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON))
        let map = try await client.getMap("map-1")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "GET", path: "/api/magician/v2/thinking-maps/map-1")
        assertScopeHeaders(req)
        XCTAssertEqual(map.mapId, "map-1")
        XCTAssertEqual(map.revision, 4)
    }

    func testGetMapPercentEncodesID() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON))
        _ = try await client.getMap("a b/c")
        XCTAssertEqual(transport.lastRequest?.url?.path,
                       "/api/magician/v2/thinking-maps/a%20b%2Fc")
    }

    func testGetMap404NotFound() async {
        transport.seed(Data(#"{"error":"not_found","map_id":"ghost"}"#.utf8), status: 404)
        await XCTAssertThrowsErrorAsync(try await client.getMap("ghost")) { error in
            XCTAssertEqual(error as? LTM.APIError, .notFound)
        }
    }

    func testGetMap404FeatureDisabled() async {
        transport.seed(Data(#"{"error":"feature_disabled"}"#.utf8), status: 404)
        await XCTAssertThrowsErrorAsync(try await client.getMap("map-1")) { error in
            XCTAssertEqual(error as? LTM.APIError, .featureDisabled)
        }
    }

    // MARK: - 4. patchMap

    func testPatchMap() async throws {
        transport.seed(fixture(LTMWireFixtures.responseAppliedJSON))
        let outcome = try await client.patchMap("map-1", title: "New title", lifecycle: .archived)
        let req = transport.lastRequest!
        assertMethodPath(req, method: "PATCH", path: "/api/magician/v2/thinking-maps/map-1")
        assertScopeHeaders(req)
        let body = bodyObject(req)
        XCTAssertEqual(body["title"] as? String, "New title")
        XCTAssertEqual(body["lifecycle"] as? String, "archived", "lifecycle snake_case")
        if case let .applied(rev, _, embeddedMap) = outcome {
            XCTAssertEqual(rev, 5)
            XCTAssertEqual(embeddedMap.mapId, "map-1")
        } else {
            XCTFail("patchMap outcome should be .applied")
        }
    }

    func testPatchMapNilTitleOmitted() async throws {
        transport.seed(fixture(LTMWireFixtures.responseAppliedJSON))
        _ = try await client.patchMap("map-1", title: nil, lifecycle: .paused)
        let body = bodyObject(transport.lastRequest!)
        XCTAssertNil(body["title"], "nil title omits the key")
        XCTAssertEqual(body["lifecycle"] as? String, "paused")
    }

    func testPatchMap400NothingToPatch() async {
        transport.seed(Data(#"{"error":"nothing_to_patch"}"#.utf8), status: 400)
        await XCTAssertThrowsErrorAsync(
            try await client.patchMap("map-1", title: nil, lifecycle: nil)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .nothingToPatch)
        }
    }

    // MARK: - 5. applyOperations

    private func sampleAddNode() -> LTM.Operation {
        .addNode(node: LTM.Node(
            nodeId: "n1", kind: .idea, label: "hello",
            epistemicState: .provisional, assertionOrigin: .ownerSpoken, confidence: 0.5,
            createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z"))
    }

    func testApplyOperations() async throws {
        transport.seed(fixture(LTMWireFixtures.responseAppliedJSON))
        let addNode = sampleAddNode()
        let outcome = try await client.applyOperations(
            "map-1", operations: [addNode, .setTitle(title: "T")],
            baseRevision: 4, idempotencyKey: "idem-1",
            envelopeID: "env-1", utteranceID: "utt-1")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "POST",
                         path: "/api/magician/v2/thinking-maps/map-1/operations")
        assertScopeHeaders(req)
        let body = bodyObject(req)
        XCTAssertEqual(body["idempotency_key"] as? String, "idem-1")
        XCTAssertEqual((body["base_revision"] as? NSNumber)?.uint64Value, 4)
        XCTAssertEqual(body["envelope_id"] as? String, "env-1")
        XCTAssertEqual(body["utterance_id"] as? String, "utt-1")
        let ops = body["operations"] as? [[String: Any]] ?? []
        XCTAssertEqual(ops.count, 2)
        XCTAssertEqual(ops.first?["op"] as? String, "add_node")
        XCTAssertEqual((ops.first?["node"] as? [String: Any])?["node_id"] as? String, "n1")
        // Round-trip the ops back through the canonical decoder as a strong check.
        let opsData = try JSONSerialization.data(withJSONObject: body["operations"]!)
        let decodedOps = try decoder.decode([LTM.Operation].self, from: opsData)
        XCTAssertEqual(decodedOps.count, 2)
        XCTAssertEqual(decodedOps.first, addNode,
                       "body.operations decode back to the sent LTM.Operation")
        if case .applied = outcome {} else { XCTFail("outcome should be .applied") }
    }

    func testApplyOperationsEmptyOpsAndNilOptionals() async throws {
        transport.seed(fixture(LTMWireFixtures.responseIdempotentJSON))
        let outcome = try await client.applyOperations(
            "map-1", operations: [], baseRevision: 0, idempotencyKey: "k",
            envelopeID: nil, utteranceID: nil)
        let body = bodyObject(transport.lastRequest!)
        XCTAssertNil(body["envelope_id"], "nil envelopeID omits the key")
        XCTAssertNil(body["utterance_id"], "nil utteranceID omits the key")
        XCTAssertEqual((body["operations"] as? [Any])?.isEmpty, true,
                       "empty operations still sent as []")
        if case let .idempotentReplay(rev) = outcome {
            XCTAssertEqual(rev, 4)
        } else {
            XCTFail("outcome should be .idempotentReplay")
        }
    }

    func testApplyOperations400ValidationFailed() async {
        transport.seed(Data(#"{"error":"validation_failed","details":"x"}"#.utf8), status: 400)
        await XCTAssertThrowsErrorAsync(
            try await client.applyOperations(
                "map-1", operations: [sampleAddNode()], baseRevision: 0, idempotencyKey: "k",
                envelopeID: nil, utteranceID: nil)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .validationFailed)
        }
    }

    // MARK: - 6. interpret

    func testInterpret() async throws {
        transport.seed(fixture(LTMWireFixtures.responseNoOperationsJSON))
        let outcome = try await client.interpret(
            "map-1", text: "ship v1", utteranceID: "utt-9",
            threadID: "thread-42", intent: .breakOpen, focusNodeID: "node-7")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "POST",
                         path: "/api/magician/v2/thinking-maps/map-1/interpret")
        assertScopeHeaders(req)
        let body = bodyObject(req)
        XCTAssertEqual(body["text"] as? String, "ship v1")
        XCTAssertEqual(body["utterance_id"] as? String, "utt-9")
        XCTAssertEqual(body["thread_id"] as? String, "thread-42")
        XCTAssertEqual(body["intent"] as? String, "break_open", "intent snake_case wire value")
        XCTAssertEqual(body["focus_node_id"] as? String, "node-7")
        XCTAssertEqual(outcome, .noOperations)
    }

    func testInterpretDefaultIntentAndNilOptionals() async throws {
        transport.seed(fixture(LTMWireFixtures.responseNoOperationsJSON))
        _ = try await client.interpret(
            "map-1", text: "hi", utteranceID: nil, threadID: nil, intent: .continueThinking)
        let body = bodyObject(transport.lastRequest!)
        XCTAssertEqual(body["intent"] as? String, "continue_thinking")
        XCTAssertNil(body["utterance_id"], "nil utteranceID omitted")
        XCTAssertNil(body["thread_id"], "nil threadID omitted")
        XCTAssertNil(body["focus_node_id"], "nil focusNodeID omitted")
    }

    func testInterpret503LLMUnavailable() async {
        transport.seed(Data(#"{"error":"llm_unavailable"}"#.utf8), status: 503)
        await XCTAssertThrowsErrorAsync(
            try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil, intent: .continueThinking)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .llmUnavailable)
        }
    }

    func testInterpret502InterpretationFailed() async {
        transport.seed(Data(#"{"error":"interpretation_failed","details":"x"}"#.utf8), status: 502)
        await XCTAssertThrowsErrorAsync(
            try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil, intent: .continueThinking)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .interpretationFailed)
        }
    }

    func testInterpretInvalidFocusNodeHasTypedReadableError() async {
        transport.seed(Data(#"{"error":"invalid_focus_node"}"#.utf8), status: 400)
        await XCTAssertThrowsErrorAsync(
            try await client.interpret(
                "map-1", text: "x", utteranceID: nil, threadID: nil,
                intent: .breakOpen, focusNodeID: "missing")
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .invalidFocusNode)
            XCTAssertEqual(
                error.localizedDescription,
                "That thought is no longer available on this map. Refresh the map and try again.")
        }
    }

    // MARK: - 7. events

    func testEvents() async throws {
        transport.seed(arrayFixture([LTMWireFixtures.eventJSON]))
        let events = try await client.events("map-1", afterSeq: 3)
        let req = transport.lastRequest!
        assertMethodPath(req, method: "GET",
                         path: "/api/magician/v2/thinking-maps/map-1/events")
        assertScopeHeaders(req)
        XCTAssertEqual(queryValue(req, "after_seq"), "3")
        XCTAssertEqual(events.count, 1)
        XCTAssertEqual(events.first?.sequence, 1)
        XCTAssertEqual(events.first?.envelope.operations.count, 10)
    }

    // MARK: - 8. replay

    func testReplay() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON))
        let map = try await client.replay("map-1", atSeq: 2)
        let req = transport.lastRequest!
        assertMethodPath(req, method: "GET",
                         path: "/api/magician/v2/thinking-maps/map-1/replay")
        assertScopeHeaders(req)
        XCTAssertEqual(queryValue(req, "at_seq"), "2")
        XCTAssertEqual(map.mapId, "map-1")
    }

    // MARK: - 9. restore

    func testRestore() async throws {
        transport.seed(fixture(LTMWireFixtures.mapJSON), status: 201)
        let branch = try await client.restore(
            "map-1", atSequence: 1, newMapID: "branch1", newTitle: "Forked")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "POST",
                         path: "/api/magician/v2/thinking-maps/map-1/restore")
        assertScopeHeaders(req)
        let body = bodyObject(req)
        XCTAssertEqual((body["at_sequence"] as? NSNumber)?.uint64Value, 1)
        XCTAssertEqual(body["new_map_id"] as? String, "branch1")
        XCTAssertEqual(body["new_title"] as? String, "Forked")
        XCTAssertEqual(branch.mapId, "map-1")
    }

    func testRestore409AlreadyExists() async {
        transport.seed(Data(#"{"error":"already_exists","map_id":"branch1"}"#.utf8), status: 409)
        await XCTAssertThrowsErrorAsync(
            try await client.restore("map-1", atSequence: 1, newMapID: "branch1", newTitle: "Forked")
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .alreadyExists)
        }
    }

    // MARK: - 10. Generic error mapping

    func testMissingScope() async {
        transport.seed(Data(#"{"error":"missing_scope"}"#.utf8), status: 400)
        await XCTAssertThrowsErrorAsync(try await client.listMaps()) { error in
            XCTAssertEqual(error as? LTM.APIError, .missingScope)
        }
    }

    func testIOError500() async {
        transport.seed(Data(#"{"error":"io_error","details":"disk"}"#.utf8), status: 500)
        await XCTAssertThrowsErrorAsync(try await client.getMap("map-1")) { error in
            XCTAssertEqual(error as? LTM.APIError, .ioError)
        }
    }

    func testUnrecognizedErrorFallsBackToHTTP() async {
        transport.seed(Data(#"{"error":"teapot"}"#.utf8), status: 418)
        await XCTAssertThrowsErrorAsync(try await client.getMap("map-1")) { error in
            XCTAssertEqual(error as? LTM.APIError, .http(status: 418, code: "teapot"))
        }
    }

    func testNonJSON500FallsBackToHTTPNilCode() async {
        transport.seed(Data("not json".utf8), status: 500)
        await XCTAssertThrowsErrorAsync(try await client.getMap("map-1")) { error in
            XCTAssertEqual(error as? LTM.APIError, .http(status: 500, code: nil),
                           "500 non-JSON body → .http(500, nil), not .ioError")
        }
    }

    func testMalformedSuccessBodyIsDecodingError() async {
        transport.seed(Data(#"{"unexpected":"shape"}"#.utf8), status: 200)
        await XCTAssertThrowsErrorAsync(try await client.getMap("map-1")) { error in
            guard case let LTM.APIError.decoding(detail) = (error as? LTM.APIError) ?? .transport("") else {
                return XCTFail("malformed 200 body should be .decoding, got \(error)")
            }
            XCTAssertFalse(detail.isEmpty)
        }
    }

    // MARK: - attach / detach session (ambient "Listen" mode)

    func testAttachSession() async throws {
        transport.seed(Data(#"{"attached":true,"source_session_id":"voice-42"}"#.utf8))
        let attached = try await client.attachSession("map-1", sourceSessionID: "voice-42")
        let req = transport.lastRequest!
        assertMethodPath(req, method: "POST", path: "/api/magician/v2/thinking-maps/map-1/sessions")
        assertScopeHeaders(req)
        XCTAssertEqual(req.value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(bodyObject(req)["source_session_id"] as? String, "voice-42")
        XCTAssertTrue(attached)
    }

    func testAttachSession404NotFound() async {
        transport.seed(Data(#"{"error":"not_found","map_id":"ghost"}"#.utf8), status: 404)
        await XCTAssertThrowsErrorAsync(
            try await client.attachSession("ghost", sourceSessionID: "voice-42")
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .notFound)
        }
    }

    func testAttachSession503CoordinatorUnavailable() async {
        transport.seed(Data(#"{"error":"coordinator_unavailable"}"#.utf8), status: 503)
        await XCTAssertThrowsErrorAsync(
            try await client.attachSession("map-1", sourceSessionID: "voice-42")
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .coordinatorUnavailable,
                           "503 coordinator_unavailable maps to its own case, not .llmUnavailable")
        }
    }

    func testDetachSession() async throws {
        transport.seed(Data(#"{"detached":true}"#.utf8))
        let detached = try await client.detachSession("map-1", sourceSessionID: "voice 42/x")
        let req = transport.lastRequest!
        XCTAssertEqual(req.httpMethod, "DELETE")
        // The session id is percent-encoded into the path segment.
        XCTAssertEqual(req.url?.path, "/api/magician/v2/thinking-maps/map-1/sessions/voice%2042%2Fx")
        assertScopeHeaders(req)
        XCTAssertTrue(detached)
    }

    func testDetachSessionNotRegisteredReturnsFalse() async throws {
        transport.seed(Data(#"{"detached":false}"#.utf8))
        let detached = try await client.detachSession("map-1", sourceSessionID: "voice-42")
        XCTAssertFalse(detached, "idempotent detach of an unregistered session → false")
    }

    // MARK: - promoteNode (governed node promotion)

    func testPromoteNode() async throws {
        transport.seed(Data(
            #"{"promoted":true,"object_kind":"task","object_id":"task-9","resulting_revision":7}"#.utf8))
        let result = try await client.promoteNode(
            "map-1", nodeID: "n 1/x", target: "task", confirm: false)
        let req = transport.lastRequest!
        XCTAssertEqual(req.httpMethod, "POST")
        // The node id is percent-encoded into its path segment.
        XCTAssertEqual(req.url?.path,
                       "/api/magician/v2/thinking-maps/map-1/nodes/n%201%2Fx/promote")
        assertScopeHeaders(req)
        XCTAssertEqual(req.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = bodyObject(req)
        XCTAssertEqual(body["target"] as? String, "task")
        XCTAssertEqual(body["confirm"] as? Bool, false)
        XCTAssertTrue(result.promoted)
        XCTAssertEqual(result.objectKind, .task)
        XCTAssertEqual(result.objectId, "task-9")
    }

    func testPromoteNodeMemoryWithConfirm() async throws {
        transport.seed(Data(
            #"{"promoted":true,"object_kind":"memory","object_id":"cand-3"}"#.utf8))
        let result = try await client.promoteNode(
            "map-1", nodeID: "n1", target: "memory", confirm: true)
        let body = bodyObject(transport.lastRequest!)
        XCTAssertEqual(body["target"] as? String, "memory")
        XCTAssertEqual(body["confirm"] as? Bool, true)
        XCTAssertEqual(result.objectKind, .memory)
        XCTAssertEqual(result.objectId, "cand-3")
    }

    func testPromoteNodeAlreadyLinkedReturnsPromotedFalse() async throws {
        // Idempotent replay: the existing object is returned, nothing new made.
        transport.seed(Data(
            #"{"promoted":false,"object_kind":"task","object_id":"task-existing"}"#.utf8))
        let result = try await client.promoteNode(
            "map-1", nodeID: "n1", target: "task", confirm: false)
        XCTAssertFalse(result.promoted)
        XCTAssertEqual(result.objectId, "task-existing")
    }

    func testPromoteNode409ConfirmationRequired() async {
        transport.seed(Data(
            #"{"error":"confirmation_required","assertion_origin":"model_inferred","epistemic_state":"provisional"}"#.utf8),
            status: 409)
        await XCTAssertThrowsErrorAsync(
            try await client.promoteNode("map-1", nodeID: "n1", target: "task", confirm: false)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .confirmationRequired,
                           "409 confirmation_required maps to its own case, not .alreadyExists")
        }
    }

    func testPromoteNode409NotPromotableFallsBackToHTTP() async {
        // A rejected/tombstoned node never promotes — no dedicated case needed.
        transport.seed(Data(
            #"{"error":"not_promotable","epistemic_state":"rejected","tombstoned":false}"#.utf8),
            status: 409)
        await XCTAssertThrowsErrorAsync(
            try await client.promoteNode("map-1", nodeID: "n1", target: "task", confirm: true)
        ) { error in
            XCTAssertEqual(error as? LTM.APIError, .http(status: 409, code: "not_promotable"))
        }
    }
}

// MARK: - Async throwing assertion helper

extension XCTestCase {
    /// Assert that an async autoclosure throws, then hand the error to `handler`.
    func XCTAssertThrowsErrorAsync<T>(
        _ expression: @autoclosure () async throws -> T,
        _ message: String = "",
        file: StaticString = #filePath,
        line: UInt = #line,
        _ handler: (Error) -> Void = { _ in }
    ) async {
        do {
            _ = try await expression()
            XCTFail(message.isEmpty ? "expected an error to be thrown" : message,
                    file: file, line: line)
        } catch {
            handler(error)
        }
    }
}
