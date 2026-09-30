//  sync_store_main.swift
//  Live Thinking Map (LTM) — S1c: standalone verification for `LTM.SyncStore`.
//
//  NOT part of the Magios app target and compiled SEPARATELY from the S1a
//  (`wire_roundtrip_main.swift`) and S1b (`api_client_main.swift`) harnesses —
//  each has its own `@main`, so exactly one entry point exists per compile.
//
//  Compile + run standalone with swiftc (NOT the Xcode project):
//
//    /usr/bin/swiftc -o /tmp/ltm_sync_test \
//        magios/Magios/ThinkingMapCanonical/*.swift \
//        magios/ThinkingMapCanonicalTests/sync_store_main.swift \
//      && /tmp/ltm_sync_test magios/Magios/ThinkingMapCanonical/Fixtures
//
//  Uses a SCRIPTED `MockTransport` (a FIFO of canned `(Data, status)` responses,
//  flippable to throw for offline) + an in-memory `Persistence`. Covers:
//    1. online apply       — server map adopted, pendingCount 0.
//    2. offline apply      — optimistic node shows, queued; reconnect → adopted.
//    3. revision conflict  — 409 → refresh → retry at the fresh revision → lands.
//    4. conflict escalation— 409 twice → moved to `conflicts`, queue continues.
//    5. persistence        — a NEW store on the SAME persistence resumes offline.
//    6. transport mid-drain— throw on the 2nd of 3 ⇒ 1st landed, 2nd+3rd queued.

import Foundation

// MARK: - In-memory persistence

/// A `Persistence` backed by an in-memory dictionary. The SAME instance shared
/// across two `SyncStore`s proves the queue + cache survive a "restart".
final class MemoryPersistence: LTM.Persistence, @unchecked Sendable {
    private var storage: [String: Data] = [:]
    func load(_ key: String) -> Data? { storage[key] }
    func save(_ key: String, _ data: Data) { storage[key] = data }
    /// Introspection for assertions (not part of the protocol).
    var keys: [String] { Array(storage.keys) }
}

// MARK: - Scripted mock transport

/// One canned reply: either bytes+status, or an error to throw (offline).
enum ScriptedReply {
    case respond(Data, status: Int)
    case throwTransport(String)
}

/// A `Transport` that pops replies from a FIFO script, in call order, recording
/// every request it saw. If the script runs dry it returns an empty 200.
final class ScriptedTransport: LTM.Transport, @unchecked Sendable {
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
}

// MARK: - Harness

@main
enum LTMSyncStoreTest {
    static var failures = 0
    static var checks = 0

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

    // ── Response builders ──────────────────────────────────────────────────────

    /// Build a minimal `LTM.Map` with a fixed revision + the given nodes.
    static func makeMap(revision: UInt64, nodeIDs: [String] = [], title: String = "T") -> LTM.Map {
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

    /// Encode an `applied` outcome body embedding `map`.
    static func appliedBody(revision: UInt64, map: LTM.Map) -> Data {
        let outcome = LTM.ApplyOutcome.applied(
            resultingRevision: revision, semanticHash: "hash-\(revision)", map: map)
        return try! LTM.Wire.makeEncoder().encode(outcome)
    }

    /// Encode a bare map body (for getMap / refresh).
    static func mapBody(_ map: LTM.Map) -> Data {
        try! LTM.Wire.makeEncoder().encode(map)
    }

    static func idempotentBody(revision: UInt64) -> Data {
        try! LTM.Wire.makeEncoder().encode(LTM.ApplyOutcome.idempotentReplay(resultingRevision: revision))
    }

    static let conflictBody = Data(#"{"error":"revision_conflict","expected":5,"actual":0}"#.utf8)

    /// A fresh add_node op targeting `id`.
    static func addNode(_ id: String) -> LTM.Operation {
        .addNode(node: LTM.Node(
            nodeId: id, kind: .idea, label: "opt-\(id)",
            epistemicState: .provisional, assertionOrigin: .ownerSpoken, confidence: 0.5,
            createdAt: "2026-07-20T00:00:00Z", updatedAt: "2026-07-20T00:00:00Z"))
    }

    static func newStore(_ transport: ScriptedTransport, _ persistence: LTM.Persistence) -> LTM.SyncStore {
        let client = LTM.APIClient(baseURL: baseURL, scope: scope, transport: transport)
        return LTM.SyncStore(mapID: "map-1", client: client, persistence: persistence)
    }

    static func main() async {
        print("== LTM.SyncStore standalone verification ==\n")

        await testOnlineApply()
        await testOfflineApplyThenFlush()
        await testRevisionConflictRebaseRetry()
        await testConflictEscalation()
        await testPersistenceSurvivesRestart()
        await testTransportErrorMidDrain()

        print("\n== result: \(checks - failures)/\(checks) checks passed ==")
        if failures == 0 {
            print("ALL SYNC-STORE CHECKS VERIFIED")
            exit(0)
        } else {
            print("\(failures) FAILURE(S)")
            exit(1)
        }
    }

    // 1. Online apply — server map adopted, pendingCount 0.
    static func testOnlineApply() async {
        print("1. online apply:")
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        // The server's authoritative map returned by the applyOperations POST.
        let serverMap = makeMap(revision: 1, nodeIDs: ["server-n1"], title: "server")
        transport.enqueue(.respond(appliedBody(revision: 1, map: serverMap), status: 200))

        await store.apply([addNode("opt-n1")])

        check(store.pendingCount == 0, "online apply: queue drained (pendingCount == 0)")
        check(store.map?.revision == 1, "online apply: adopted server revision (1)")
        check(store.map?.nodes["server-n1"] != nil, "online apply: view is the SERVER map")
        check(store.map?.nodes["opt-n1"] == nil,
              "online apply: optimistic node OVERWRITTEN by the authoritative map")
        check(store.map?.title == "server", "online apply: server title adopted")
        check(store.conflicts.isEmpty, "online apply: no conflicts")
        print("")
    }

    // 2. Offline apply — optimistic node shows + queued; reconnect flushes.
    static func testOfflineApplyThenFlush() async {
        print("2. offline apply then flush:")
        let transport = ScriptedTransport()
        let persistence = MemoryPersistence()
        let store = newStore(transport, persistence)

        // Seed a cached authoritative map at revision 2 so optimistic transform
        // has something to mutate (offline, no getMap available).
        store.isOnline = true
        transport.enqueue(.respond(mapBody(makeMap(revision: 2, nodeIDs: ["base"])), status: 200))
        try? await store.refresh()
        check(store.map?.revision == 2, "offline apply: seeded cache at rev 2")

        // Go offline, apply — should preview + queue, NOT hit the network.
        store.isOnline = false
        let postsBefore = transport.operationsPostCount
        await store.apply([addNode("opt-offline")])
        check(transport.operationsPostCount == postsBefore,
              "offline apply: no /operations POST while offline")
        check(store.pendingCount == 1, "offline apply: op is queued (pendingCount == 1)")
        check(store.map?.nodes["opt-offline"] != nil,
              "offline apply: optimistic node visible immediately")
        check(store.map?.revision == 2,
              "offline apply: optimistic transform did NOT bump revision (still 2)")

        // Reconnect + flush: the server returns the authoritative map at rev 3.
        let serverMap = makeMap(revision: 3, nodeIDs: ["base", "server-offline"], title: "flushed")
        transport.enqueue(.respond(appliedBody(revision: 3, map: serverMap), status: 200))
        store.isOnline = true
        await store.flush()

        check(store.pendingCount == 0, "offline flush: queue drained after reconnect")
        check(store.map?.revision == 3, "offline flush: adopted server revision (3)")
        check(store.map?.nodes["server-offline"] != nil, "offline flush: server node present")
        check(store.map?.title == "flushed", "offline flush: server map adopted (title)")
        print("")
    }

    // 3. Revision conflict — 409 → refresh → retry at fresh revision → lands.
    static func testRevisionConflictRebaseRetry() async {
        print("3. revision conflict rebase+retry:")
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        // Seed cache at a STALE revision 0.
        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()
        check(store.map?.revision == 0, "conflict: seeded stale cache at rev 0")

        // Scripted flush sequence:
        //  (a) first POST at base_revision 0 → 409 revision_conflict (expected 5)
        //  (b) getMap (refresh) → fresh map at revision 5
        //  (c) retried POST at base_revision 5 → applied, resulting map rev 6
        let freshMap = makeMap(revision: 5, nodeIDs: ["rebased-base"])
        let landedMap = makeMap(revision: 6, nodeIDs: ["rebased-base", "landed"], title: "landed")
        transport.enqueue([
            .respond(conflictBody, status: 409),
            .respond(mapBody(freshMap), status: 200),
            .respond(appliedBody(revision: 6, map: landedMap), status: 200),
        ])

        await store.apply([addNode("wants-to-land")])

        check(store.operationsPostCountOK(transport, expected: 2),
              "conflict: exactly TWO /operations POSTs (original + one retry)")
        check(transport.lastOperationsBaseRevision() == 5,
              "conflict: the RETRY used the refreshed base_revision (5), not the stale 0")
        check(store.pendingCount == 0, "conflict: op landed, queue drained")
        check(store.conflicts.isEmpty, "conflict: NOT escalated (retry succeeded)")
        check(store.map?.revision == 6, "conflict: adopted the post-retry server revision (6)")
        check(store.map?.nodes["landed"] != nil, "conflict: server 'landed' node present")
        print("")
    }

    // 4. Conflict escalation — 409 twice ⇒ moved to conflicts, queue continues.
    static func testConflictEscalation() async {
        print("4. conflict escalation:")
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()

        // Two queued ops. The FIRST conflicts twice (refresh doesn't help);
        // the SECOND then lands cleanly — proving the queue is not blocked.
        store.isOnline = false
        await store.apply([addNode("doomed")])   // op A
        await store.apply([addNode("survivor")]) // op B
        check(store.pendingCount == 2, "escalation: two ops queued offline")

        // Script for the drain:
        //  A: POST → 409 ; refresh → map rev 7 ; retry POST → 409 (again) ⇒ escalate
        //  B: POST (base 7) → applied rev 8
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

        check(store.conflicts.count == 1, "escalation: doomed op moved to conflicts (count 1)")
        check(store.conflicts.first?.operations == [addNode("doomed")],
              "escalation: the CONFLICTED op is 'doomed'")
        check(store.pendingCount == 0, "escalation: queue fully drained (survivor landed)")
        check(store.map?.revision == 8, "escalation: adopted survivor's server revision (8)")
        check(store.map?.nodes["survivor-server"] != nil,
              "escalation: survivor's server node present (queue did NOT block)")
        print("")
    }

    // 5. Persistence — a NEW store on the SAME persistence resumes offline.
    static func testPersistenceSurvivesRestart() async {
        print("5. persistence survives restart:")
        let transport = ScriptedTransport()
        let persistence = MemoryPersistence()

        // First store: seed a cache, then enqueue an op OFFLINE.
        let store1 = newStore(transport, persistence)
        store1.isOnline = true
        transport.enqueue(.respond(mapBody(makeMap(revision: 4, nodeIDs: ["persisted-base"])), status: 200))
        try? await store1.refresh()
        store1.isOnline = false
        await store1.apply([addNode("persisted-op")])
        check(store1.pendingCount == 1, "persistence: store1 queued 1 op offline")
        check(persistence.keys.contains { $0.hasSuffix(".queue") },
              "persistence: queue was written to persistence")
        check(persistence.keys.contains { $0.hasSuffix(".cache") },
              "persistence: cache was written to persistence")

        // "Restart": a brand-new store over the SAME persistence, still offline.
        let store2 = newStore(transport, persistence)
        store2.isOnline = false
        check(store2.pendingCount == 1, "persistence: store2 resumed the queued op (pendingCount 1)")
        check(store2.map?.revision == 4, "persistence: store2 resumed the cached map (rev 4)")
        check(store2.map?.nodes["persisted-base"] != nil,
              "persistence: store2 resumed the cached nodes")

        // And it can flush the resumed op when back online.
        let serverMap = makeMap(revision: 5, nodeIDs: ["persisted-base", "resumed"], title: "resumed")
        transport.enqueue(.respond(appliedBody(revision: 5, map: serverMap), status: 200))
        store2.isOnline = true
        await store2.flush()
        check(store2.pendingCount == 0, "persistence: store2 flushed the resumed op")
        check(store2.map?.nodes["resumed"] != nil, "persistence: resumed op landed on the server")
        print("")
    }

    // 6. Transport error mid-drain — throw on 2nd of 3 ⇒ 1st landed, 2+3 queued.
    static func testTransportErrorMidDrain() async {
        print("6. transport error mid-drain:")
        let transport = ScriptedTransport()
        let store = newStore(transport, MemoryPersistence())
        store.isOnline = true

        transport.enqueue(.respond(mapBody(makeMap(revision: 0)), status: 200))
        try? await store.refresh()

        // Queue three ops offline.
        store.isOnline = false
        await store.apply([addNode("op1")])
        await store.apply([addNode("op2")])
        await store.apply([addNode("op3")])
        check(store.pendingCount == 3, "mid-drain: three ops queued")

        // Script: op1 lands (rev 1) ; op2 → transport throw ; (op3 never reached).
        let afterOne = makeMap(revision: 1, nodeIDs: ["op1-server"], title: "one")
        transport.enqueue([
            .respond(appliedBody(revision: 1, map: afterOne), status: 200), // op1
            .throwTransport("connection reset"),                            // op2 → down
        ])
        store.isOnline = true
        await store.flush()

        check(store.map?.revision == 1, "mid-drain: op1 landed (adopted rev 1)")
        check(store.map?.nodes["op1-server"] != nil, "mid-drain: op1's server node present")
        check(store.pendingCount == 2, "mid-drain: op2 + op3 remain queued")
        check(store.isOnline == false, "mid-drain: transport error flipped isOnline to false")
        check(store.conflicts.isEmpty, "mid-drain: a transport error is NOT a conflict")
        print("")
    }
}

// MARK: - Small helpers

private extension LTM.SyncStore {
    /// Convenience assertion helper: was the total /operations POST count `expected`?
    func operationsPostCountOK(_ transport: ScriptedTransport, expected: Int) -> Bool {
        transport.operationsPostCount == expected
    }
}
