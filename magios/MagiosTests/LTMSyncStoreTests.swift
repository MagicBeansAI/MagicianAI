//  LTMSyncStoreTests.swift
//  Live Thinking Map (LTM) — S1c `LTM.SyncStore` verification, as XCTest.
//
//  Ported from `magios/ThinkingMapCanonicalTests/sync_store_main.swift`. Uses a
//  SCRIPTED transport (a FIFO of canned `(Data, status)` replies, flippable to
//  throw for offline) + an in-memory `LTM.Persistence`. Covers the six sync
//  scenarios:
//    1. online apply         — server map adopted, pendingCount 0.
//    2. offline apply → flush — optimistic node shows + queued; reconnect adopts.
//    3. revision conflict     — 409 → refresh → retry at fresh revision → lands.
//    4. conflict escalation   — 409 twice ⇒ moved to `conflicts`, queue continues.
//    5. persistence           — a NEW store on the SAME persistence resumes offline.
//    6. transport mid-drain   — throw on the 2nd of 3 ⇒ 1st landed, 2nd+3rd queued.

import XCTest
@testable import Magician

// MARK: - In-memory persistence

private final class MemoryPersistence: LTM.Persistence, @unchecked Sendable {
    private var storage: [String: Data] = [:]
    func load(_ key: String) -> Data? { storage[key] }
    func save(_ key: String, _ data: Data) { storage[key] = data }
    /// Introspection for assertions (not part of the protocol).
    var keys: [String] { Array(storage.keys) }
}

// MARK: - Scripted mock transport

private enum ScriptedReply {
    case respond(Data, status: Int)
    case throwTransport(String)
}

private final class ScriptedTransport: LTM.Transport, @unchecked Sendable {
    private var script: [ScriptedReply] = []
    private(set) var requests: [URLRequest] = []

    func enqueue(_ reply: ScriptedReply) { script.append(reply) }
    func enqueue(_ replies: [ScriptedReply]) { script.append(contentsOf: replies) }

    var remaining: Int { script.count }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        requests.append(request)
        let reply = script.isEmpty ? .respond(Data("{}".utf8), status: 200) : script.removeFirst()
        switch reply {
        case let .respond(data, status):
            let http = HTTPURLResponse(
                url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])!
            return (data, http)
        case let .throwTransport(message):
            throw LTM.APIError.transport(message)
        }
    }

    /// The base_revision the client sent on the LAST /operations POST.
    func lastOperationsBaseRevision() -> UInt64? {
        for request in requests.reversed() where request.url?.path.hasSuffix("/operations") == true {
            guard let body = request.httpBody,
                  let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                  let rev = obj["base_revision"] as? NSNumber
            else { return nil }
            return rev.uint64Value
        }
        return nil
    }

    var operationsPostCount: Int {
        requests.filter { $0.url?.path.hasSuffix("/operations") == true }.count
    }

    /// The decoded operations sent on the LAST /operations POST, if any.
    func lastOperationsSent() -> [LTM.Operation]? {
        for request in requests.reversed() where request.url?.path.hasSuffix("/operations") == true {
            guard let body = request.httpBody,
                  let sent = try? LTM.Wire.makeDecoder().decode(SentOperations.self, from: body)
            else { return nil }
            return sent.operations
        }
        return nil
    }

    /// The path of the LAST request (for endpoint assertions).
    var lastRequestPath: String? { requests.last?.url?.path }

    /// Decode `{"decision":"…"}` off the last /decision POST body.
    func lastDecision() -> String? {
        for request in requests.reversed() where request.url?.path.hasSuffix("/decision") == true {
            guard let body = request.httpBody,
                  let obj = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
            else { return nil }
            return obj["decision"] as? String
        }
        return nil
    }
}

/// Minimal decode of the /operations body to recover the sent operations.
private struct SentOperations: Decodable {
    let operations: [LTM.Operation]
}

final class LTMSyncStoreTests: XCTestCase {

    private let baseURL = URL(string: "https://ios.example.test")!
    private let scope = LTM.Scope(principal: "anonymous", workspace: "default")

    // MARK: - Builders

    /// Build a minimal `LTM.Map` with a fixed revision + the given nodes.
    private func makeMap(
        revision: UInt64, nodeIDs: [String] = [], title: String = "T"
    ) -> LTM.Map {
        var nodes: [String: LTM.Node] = [:]
        for id in nodeIDs {
            nodes[id] = LTM.Node(
                nodeId: id, kind: .idea, label: "label-\(id)",
                epistemicState: .provisional, assertionOrigin: .ownerSpoken,
                confidence: 0.5, createdAt: "2026-07-20T00:00:00Z",
                updatedAt: "2026-07-20T00:00:00Z")
        }
        return LTM.Map(
            schemaVersion: 1, mapId: "map-1", principal: "anonymous", workspace: "default",
            title: title, source: .solo, lifecycle: .active, revision: revision,
            nodes: nodes, createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z")
    }

    private func appliedBody(revision: UInt64, map: LTM.Map) -> Data {
        let outcome = LTM.ApplyOutcome.applied(
            resultingRevision: revision, semanticHash: "hash-\(revision)", map: map)
        return try! LTM.Wire.makeEncoder().encode(outcome)
    }

    private func mapBody(_ map: LTM.Map) -> Data {
        try! LTM.Wire.makeEncoder().encode(map)
    }

    private func idempotentBody(revision: UInt64) -> Data {
        try! LTM.Wire.makeEncoder().encode(LTM.ApplyOutcome.idempotentReplay(resultingRevision: revision))
    }

    private var conflictBody: Data {
        Data(#"{"error":"revision_conflict","expected":5,"actual":0}"#.utf8)
    }

    private func addNode(_ id: String) -> LTM.Operation {
        .addNode(node: LTM.Node(
            nodeId: id, kind: .idea, label: "opt-\(id)",
            epistemicState: .provisional, assertionOrigin: .ownerSpoken, confidence: 0.5,
            createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z"))
    }

    private func newStore(
        _ transport: ScriptedTransport, _ persistence: LTM.Persistence
    ) -> LTM.SyncStore {
        let client = LTM.APIClient(baseURL: baseURL, scope: scope, transport: transport)
        return LTM.SyncStore(mapID: "map-1", client: client, persistence: persistence)
    }

    // MARK: - 1. online apply

    func testOnlineApply() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        let serverMap = makeMap(revision: 1, nodeIDs: ["server-n1"], title: "server")
        transport.enqueue(.respond(appliedBody(revision: 1, map: serverMap), status: 200))

        await store.apply([addNode("opt-n1")])

        XCTAssertEqual(store.pendingCount, 0, "queue drained")
        XCTAssertEqual(store.map?.revision, 1, "adopted server revision")
        XCTAssertNotNil(store.map?.nodes["server-n1"], "view is the SERVER map")
        XCTAssertNil(store.map?.nodes["opt-n1"], "optimistic node overwritten by authoritative map")
        XCTAssertEqual(store.map?.title, "server")
        XCTAssertTrue(store.conflicts.isEmpty)
    }

    // MARK: - 2. offline apply then flush

    func testOfflineApplyThenFlush() async {
        let transport = ScriptedTransport()
        let persistence = MemoryPersistence()
        let store = newStore(transport, persistence)

        // Seed a cached authoritative map at revision 2.
        store.isOnline = true
        transport.enqueue(.respond(mapBody(makeMap(revision: 2, nodeIDs: ["base"])), status: 200))
        try? await store.refresh()
        XCTAssertEqual(store.map?.revision, 2, "seeded cache at rev 2")

        // Offline apply — preview + queue, no network.
        store.isOnline = false
        let postsBefore = transport.operationsPostCount
        await store.apply([addNode("opt-offline")])
        XCTAssertEqual(transport.operationsPostCount, postsBefore, "no /operations POST while offline")
        XCTAssertEqual(store.pendingCount, 1, "op is queued")
        XCTAssertNotNil(store.map?.nodes["opt-offline"], "optimistic node visible immediately")
        XCTAssertEqual(store.map?.revision, 2, "optimistic transform did NOT bump revision")

        // Reconnect + flush.
        let serverMap = makeMap(revision: 3, nodeIDs: ["base", "server-offline"], title: "flushed")
        transport.enqueue(.respond(appliedBody(revision: 3, map: serverMap), status: 200))
        store.isOnline = true
        await store.flush()

        XCTAssertEqual(store.pendingCount, 0, "queue drained after reconnect")
        XCTAssertEqual(store.map?.revision, 3, "adopted server revision")
        XCTAssertNotNil(store.map?.nodes["server-offline"], "server node present")
        XCTAssertEqual(store.map?.title, "flushed")
    }

    // MARK: - 3. revision conflict rebase + retry

    func testRevisionConflictRebaseRetry() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()
        XCTAssertEqual(store.map?.revision, 0, "seeded stale cache at rev 0")

        let freshMap = makeMap(revision: 5, nodeIDs: ["rebased-base"])
        let landedMap = makeMap(revision: 6, nodeIDs: ["rebased-base", "landed"], title: "landed")
        transport.enqueue([
            .respond(conflictBody, status: 409),
            .respond(mapBody(freshMap), status: 200),
            .respond(appliedBody(revision: 6, map: landedMap), status: 200),
        ])

        await store.apply([addNode("wants-to-land")])

        XCTAssertEqual(transport.operationsPostCount, 2,
                       "exactly TWO /operations POSTs (original + one retry)")
        XCTAssertEqual(transport.lastOperationsBaseRevision(), 5,
                       "the retry used the refreshed base_revision (5), not the stale 0")
        XCTAssertEqual(store.pendingCount, 0, "op landed, queue drained")
        XCTAssertTrue(store.conflicts.isEmpty, "NOT escalated (retry succeeded)")
        XCTAssertEqual(store.map?.revision, 6, "adopted the post-retry server revision")
        XCTAssertNotNil(store.map?.nodes["landed"], "server 'landed' node present")
    }

    // MARK: - 4. conflict escalation

    func testConflictEscalation() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()

        // Two queued ops: A conflicts twice (escalates); B then lands cleanly.
        store.isOnline = false
        await store.apply([addNode("doomed")])
        await store.apply([addNode("survivor")])
        XCTAssertEqual(store.pendingCount, 2, "two ops queued offline")

        let refreshedMap = makeMap(revision: 7)
        let bLandedMap = makeMap(revision: 8, nodeIDs: ["survivor-server"], title: "survived")
        transport.enqueue([
            .respond(conflictBody, status: 409),          // A original
            .respond(mapBody(refreshedMap), status: 200), // A refresh
            .respond(conflictBody, status: 409),          // A retry → conflict again
            .respond(appliedBody(revision: 8, map: bLandedMap), status: 200), // B lands
        ])
        store.isOnline = true
        await store.flush()

        XCTAssertEqual(store.conflicts.count, 1, "doomed op moved to conflicts")
        XCTAssertEqual(store.conflicts.first?.operations, [addNode("doomed")],
                       "the CONFLICTED op is 'doomed'")
        XCTAssertEqual(store.pendingCount, 0, "queue fully drained (survivor landed)")
        XCTAssertEqual(store.map?.revision, 8, "adopted survivor's server revision")
        XCTAssertNotNil(store.map?.nodes["survivor-server"],
                        "survivor's server node present (queue did NOT block)")
    }

    // MARK: - 5. persistence survives restart

    func testPersistenceSurvivesRestart() async {
        let transport = ScriptedTransport()
        let persistence = MemoryPersistence()

        let store1 = newStore(transport, persistence)
        store1.isOnline = true
        transport.enqueue(.respond(mapBody(makeMap(revision: 4, nodeIDs: ["persisted-base"])), status: 200))
        try? await store1.refresh()
        store1.isOnline = false
        await store1.apply([addNode("persisted-op")])
        XCTAssertEqual(store1.pendingCount, 1, "store1 queued 1 op offline")
        XCTAssertTrue(persistence.keys.contains { $0.hasSuffix(".queue") },
                      "queue was written to persistence")
        XCTAssertTrue(persistence.keys.contains { $0.hasSuffix(".cache") },
                      "cache was written to persistence")

        // "Restart": a brand-new store over the SAME persistence, still offline.
        let store2 = newStore(transport, persistence)
        store2.isOnline = false
        XCTAssertEqual(store2.pendingCount, 1, "store2 resumed the queued op")
        XCTAssertEqual(store2.map?.revision, 4, "store2 resumed the cached map")
        XCTAssertNotNil(store2.map?.nodes["persisted-base"], "store2 resumed the cached nodes")

        let serverMap = makeMap(revision: 5, nodeIDs: ["persisted-base", "resumed"], title: "resumed")
        transport.enqueue(.respond(appliedBody(revision: 5, map: serverMap), status: 200))
        store2.isOnline = true
        await store2.flush()
        XCTAssertEqual(store2.pendingCount, 0, "store2 flushed the resumed op")
        XCTAssertNotNil(store2.map?.nodes["resumed"], "resumed op landed on the server")
    }

    // MARK: - 6. transport error mid-drain

    func testTransportErrorMidDrain() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()

        store.isOnline = false
        await store.apply([addNode("op1")])
        await store.apply([addNode("op2")])
        await store.apply([addNode("op3")])
        XCTAssertEqual(store.pendingCount, 3, "three ops queued")

        let afterOne = makeMap(revision: 1, nodeIDs: ["op1-server"], title: "one")
        transport.enqueue([
            .respond(appliedBody(revision: 1, map: afterOne), status: 200), // op1
            .throwTransport("connection reset"),                            // op2 → down
        ])
        store.isOnline = true
        await store.flush()

        XCTAssertEqual(store.map?.revision, 1, "op1 landed (adopted rev 1)")
        XCTAssertNotNil(store.map?.nodes["op1-server"], "op1's server node present")
        XCTAssertEqual(store.pendingCount, 2, "op2 + op3 remain queued")
        XCTAssertEqual(store.isOnline, false, "transport error flipped isOnline to false")
        XCTAssertTrue(store.conflicts.isEmpty, "a transport error is NOT a conflict")
    }

    // MARK: - 7. respondClarification routes a resolve_clarification through apply

    func testRespondClarificationEnqueuesResolveOp() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        // Seed a cache so the optimistic preview path has a map to touch.
        transport.enqueue(.respond(mapBody(makeMap(revision: 1)), status: 200))
        try? await store.refresh()

        // The apply → /operations POST returns an applied outcome.
        transport.enqueue(.respond(appliedBody(revision: 2, map: makeMap(revision: 2)), status: 200))
        await store.respondClarification("clar-1", answer: "iPhone first", state: .answered)

        XCTAssertEqual(store.pendingCount, 0, "resolve op flushed")
        let sent = transport.lastOperationsSent()
        XCTAssertEqual(sent?.count, 1, "one op sent")
        guard case let .resolveClarification(clarID, state, answer) = sent?.first else {
            return XCTFail("expected a resolve_clarification op, got \(String(describing: sent?.first))")
        }
        XCTAssertEqual(clarID, "clar-1")
        XCTAssertEqual(state, .answered)
        XCTAssertEqual(answer, "iPhone first")
    }

    // MARK: - 8. consolidate (online) adopts the map with the pending proposal

    func testConsolidateAdoptsProposalMap() async throws {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        // The consolidate response is `applied` with a map carrying a proposed
        // restructure proposal.
        var proposalMap = makeMap(revision: 3, title: "consolidated")
        let proposal = LTM.RestructureProposal(
            proposalId: "p-1", proposedBy: .model(traceId: "t"), rationale: "Group by theme",
            operations: [.tombstoneNode(nodeId: "x")], state: .proposed,
            affectedNodeIds: ["x"], createdAt: "2026-07-20T00:00:00Z")
        proposalMap.proposals = ["p-1": proposal]
        transport.enqueue(.respond(appliedBody(revision: 3, map: proposalMap), status: 200))

        try await store.consolidate()

        XCTAssertEqual(transport.lastRequestPath?.hasSuffix("/consolidate"), true,
                       "hit the /consolidate endpoint")
        XCTAssertEqual(store.map?.revision, 3, "adopted the consolidated map")
        XCTAssertEqual(store.map?.proposals["p-1"]?.state, .proposed,
                       "map now carries the pending proposal")
    }

    func testConsolidateOfflineThrows() async {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = false
        do {
            try await store.consolidate()
            XCTFail("consolidate should throw offline")
        } catch LTM.APIError.transport {
            // expected
        } catch {
            XCTFail("expected .transport, got \(error)")
        }
    }

    // MARK: - 9. decideProposal (online) posts the decision + adopts the map

    func testDecideProposalConfirmAdoptsMap() async throws {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        // On confirm the server materializes the proposal's ops → a fresh map with
        // the proposal now `confirmed`.
        var confirmedMap = makeMap(revision: 4, title: "restructured")
        confirmedMap.proposals = ["p-1": LTM.RestructureProposal(
            proposalId: "p-1", proposedBy: .model(traceId: "t"), rationale: "Group",
            operations: [], state: .confirmed, affectedNodeIds: [],
            createdAt: "2026-07-20T00:00:00Z")]
        transport.enqueue(.respond(appliedBody(revision: 4, map: confirmedMap), status: 200))

        try await store.decideProposal("p-1", decision: "confirm")

        XCTAssertEqual(transport.lastRequestPath?.hasSuffix("/proposals/p-1/decision"), true,
                       "hit the proposal decision endpoint")
        XCTAssertEqual(transport.lastDecision(), "confirm", "sent the confirm decision")
        XCTAssertEqual(store.map?.revision, 4, "adopted the restructured map")
        XCTAssertEqual(store.map?.proposals["p-1"]?.state, .confirmed)
    }

    // MARK: - 10. attach / detach session (ambient "Listen" mode)

    func testAttachSessionOnlinePassesThrough() async throws {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true
        transport.enqueue(.respond(Data(#"{"attached":true,"source_session_id":"voice-7"}"#.utf8), status: 200))

        let attached = try await store.attachSession("voice-7")

        XCTAssertTrue(attached)
        XCTAssertEqual(transport.lastRequestPath?.hasSuffix("/sessions"), true,
                       "hit the /sessions attach endpoint")
    }

    func testAttachSessionOfflineThrows() async {
        let store = newStore(ScriptedTransport(), MemoryPersistence())
        store.isOnline = false
        do {
            _ = try await store.attachSession("voice-7")
            XCTFail("attachSession should throw offline")
        } catch LTM.APIError.transport {
            // expected — attach is online-only
        } catch {
            XCTFail("expected .transport, got \(error)")
        }
    }

    func testDetachSessionOnlinePassesThrough() async throws {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true
        transport.enqueue(.respond(Data(#"{"detached":true}"#.utf8), status: 200))

        let detached = try await store.detachSession("voice-7")

        XCTAssertTrue(detached)
        XCTAssertEqual(transport.requests.last?.httpMethod, "DELETE")
        XCTAssertEqual(transport.lastRequestPath?.hasSuffix("/sessions/voice-7"), true,
                       "hit the /sessions/{id} detach endpoint")
    }

    func testDetachSessionOfflineThrows() async {
        let store = newStore(ScriptedTransport(), MemoryPersistence())
        store.isOnline = false
        do {
            _ = try await store.detachSession("voice-7")
            XCTFail("detachSession should throw offline")
        } catch LTM.APIError.transport {
            // expected — detach is online-only
        } catch {
            XCTFail("expected .transport, got \(error)")
        }
    }

    // MARK: - 11. promoteNode (governed promotion — online only)

    func testPromoteNodeOnlinePassesThrough() async throws {
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true
        transport.enqueue(.respond(Data(
            #"{"promoted":true,"object_kind":"task","object_id":"task-1"}"#.utf8), status: 200))

        let result = try await store.promoteNode("n1", target: "task", confirm: false)

        XCTAssertTrue(result.promoted)
        XCTAssertEqual(result.objectKind, .task)
        XCTAssertEqual(result.objectId, "task-1")
        XCTAssertEqual(transport.lastRequestPath?.hasSuffix("/nodes/n1/promote"), true,
                       "hit the node promote endpoint")
    }

    func testPromoteNodeOfflineThrows() async {
        let store = newStore(ScriptedTransport(), MemoryPersistence())
        store.isOnline = false
        do {
            _ = try await store.promoteNode("n1", target: "memory", confirm: false)
            XCTFail("promoteNode should throw offline")
        } catch LTM.APIError.transport {
            // expected — promotion creates a durable object, it can't queue
        } catch {
            XCTFail("expected .transport, got \(error)")
        }
    }
}
