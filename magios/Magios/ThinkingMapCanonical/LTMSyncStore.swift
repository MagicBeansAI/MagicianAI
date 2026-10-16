//  LTMSyncStore.swift
//  Live Thinking Map (LTM) — S1c: server-authoritative local reconciliation
//  cache + persisted offline op-queue.
//
//  ## Design principle: SERVER-AUTHORITATIVE
//  The Rust reducer is the single source of truth. This store SENDS operations
//  and adopts whatever authoritative `LTM.Map` the server returns. Offline
//  captures QUEUE (persisted to disk) and FLUSH on reconnect, in order. A thin
//  optimistic transform (`LTM.optimisticApply`, see `LTMOptimistic.swift`) shows
//  a capture immediately for responsiveness, but it is COSMETIC and ALWAYS
//  replaced by the server's authoritative response — it is NOT a second reducer.
//
//  ## Reconciliation on flush (see `flush()`):
//  Ops are drained head-first at the CURRENT cached revision. On:
//    - `.applied(_, _, map)`      → adopt the server map, dequeue, continue.
//    - `.idempotentReplay(rev)`   → op already landed; dequeue, refresh, continue.
//    - `.revisionConflict`        → refresh (rebase), retry ONCE; if it conflicts
//                                   again OR is genuinely rejected → move to
//                                   `conflicts`, dequeue, continue (never block).
//    - transport error            → STOP draining, leave the rest queued.
//
//  ## Concurrency
//  A plain (non-`Sendable`) class — kept Foundation-only so it compiles + tests
//  standalone with swiftc. The app wraps it in `@MainActor`/`ObservableObject`
//  for its actual UI usage; all mutation here happens on the caller's context.

import Foundation

// MARK: - Persistence

extension LTM {
    /// Durable key→bytes store, INJECTED so the store has no hard filesystem
    /// dependency. Production uses `FilePersistence`; tests use an in-memory impl.
    public protocol Persistence: Sendable {
        /// Return the bytes previously saved under `key`, or nil if absent.
        func load(_ key: String) -> Data?
        /// Persist `data` under `key`, overwriting any prior value.
        func save(_ key: String, _ data: Data)
    }

    /// File-backed `Persistence` — one JSON file per key under `directory`.
    /// Keys are sanitized into safe filenames. Best-effort: IO errors are
    /// swallowed (a failed save just means the value isn't durable this run).
    public struct FilePersistence: Persistence {
        public let directory: URL

        public init(directory: URL) {
            self.directory = directory
            try? FileManager.default.createDirectory(
                at: directory, withIntermediateDirectories: true)
        }

        private func fileURL(for key: String) -> URL {
            // Map arbitrary key chars to a filesystem-safe name.
            let safe = key.map { ch -> Character in
                let ok = ch.isLetter || ch.isNumber || ch == "-" || ch == "_" || ch == "."
                return ok ? ch : "_"
            }
            return directory.appendingPathComponent(String(safe) + ".json")
        }

        public func load(_ key: String) -> Data? {
            try? Data(contentsOf: fileURL(for: key))
        }

        public func save(_ key: String, _ data: Data) {
            try? FileManager.default.createDirectory(
                at: directory, withIntermediateDirectories: true)
            try? data.write(to: fileURL(for: key), options: .atomic)
        }
    }
}

// MARK: - Persisted value types

extension LTM {
    /// One queued offline operation batch, awaiting flush. `localID` is a stable
    /// client-side handle; `idempotencyKey` is what the server dedupes on (so a
    /// partial-flush replay is recognized as `idempotent_replay`).
    public struct PendingOp: Codable, Equatable, Sendable {
        public let localID: UUID
        public let operations: [LTM.Operation]
        public let idempotencyKey: String
        public let createdAt: String

        public init(
            localID: UUID = UUID(),
            operations: [LTM.Operation],
            idempotencyKey: String,
            createdAt: String
        ) {
            self.localID = localID
            self.operations = operations
            self.idempotencyKey = idempotencyKey
            self.createdAt = createdAt
        }

        private enum CodingKeys: String, CodingKey {
            case localID = "local_id"
            case operations
            case idempotencyKey = "idempotency_key"
            case createdAt = "created_at"
        }
    }

    /// The persisted authoritative cache: the last server map we adopted plus the
    /// revision it was synced at. `map.revision` is the working base for the next
    /// apply; `lastSyncedRevision` mirrors it after a clean drain.
    public struct CachedMap: Codable, Equatable, Sendable {
        public var map: LTM.Map
        public var lastSyncedRevision: UInt64

        public init(map: LTM.Map, lastSyncedRevision: UInt64) {
            self.map = map
            self.lastSyncedRevision = lastSyncedRevision
        }

        private enum CodingKeys: String, CodingKey {
            case map
            case lastSyncedRevision = "last_synced_revision"
        }
    }
}

// MARK: - SyncStore

extension LTM {
    /// Server-authoritative local reconciliation cache + persisted offline
    /// op-queue for ONE map. See the file header for the design.
    public final class SyncStore {
        // ── Injected collaborators ──────────────────────────────────────────────
        /// The canonical map id this store syncs. Public so app layers can
        /// correlate realtime `ThinkingMapUpdated` notices with the open map.
        public let mapID: String
        private let client: LTM.APIClient
        private let persistence: LTM.Persistence

        // ── Observable-ish state (the app wraps this in an ObservableObject) ─────

        /// The current view of the map: the optimistically-transformed cache while
        /// ops are pending, replaced by the authoritative server map after flush.
        public private(set) var map: LTM.Map?

        /// Number of ops still queued (not yet accepted by the server).
        public private(set) var pendingCount: Int = 0

        /// Ops the server rejected as conflicts even after a rebase+retry (or that
        /// were genuinely rejected). Surfaced for the app to resolve; they do NOT
        /// block the rest of the queue.
        public private(set) var conflicts: [LTM.PendingOp] = []

        /// The app sets this from reachability; tests set it directly. When false,
        /// `flush()` is a no-op and `open()` skips the network refresh.
        public var isOnline: Bool = true

        // ── Internal working state ──────────────────────────────────────────────

        /// The persisted authoritative cache (nil until `open`/`refresh`).
        private var cache: LTM.CachedMap?
        /// The FIFO offline queue (head is flushed first).
        private var queue: [LTM.PendingOp] = []

        // ── Persistence keys (namespaced per map) ───────────────────────────────
        private var queueKey: String { "ltm.syncstore.\(mapID).queue" }
        private var cacheKey: String { "ltm.syncstore.\(mapID).cache" }

        private let encoder = LTM.Wire.makeEncoder()
        private let decoder = LTM.Wire.makeDecoder()

        public init(mapID: String, client: LTM.APIClient, persistence: LTM.Persistence) {
            self.mapID = mapID
            self.client = client
            self.persistence = persistence
            loadFromDisk()
        }

        // MARK: Lifecycle

        /// Load the persisted cache + queue, then (if online) refresh from the
        /// server to adopt the authoritative map and flush any pending ops.
        public func open() async throws {
            loadFromDisk()
            guard isOnline else { return }
            try await refresh()
            await flush()
        }

        /// `getMap` → replace the cache with the authoritative server map. This is
        /// the reconciliation anchor: the server state becomes our base revision.
        public func refresh() async throws {
            let serverMap = try await client.getMap(mapID)
            adopt(serverMap)
        }

        // MARK: Apply (optimistic + enqueue + flush)

        /// Optimistically transform the local view for immediate feedback, append
        /// a fresh `PendingOp`, persist, and try to flush. The optimistic map is
        /// cosmetic and gets overwritten by the server on a successful flush.
        public func apply(_ operations: [LTM.Operation]) async {
            // 1. Cosmetic preview: mutate the local view so the UI updates now.
            if map != nil {
                LTM.optimisticApply(&map!, operations)
            }
            // 2. Enqueue with a fresh idempotency key so the server can dedupe.
            let pending = LTM.PendingOp(
                operations: operations,
                idempotencyKey: UUID().uuidString,
                createdAt: Self.timestamp())
            queue.append(pending)
            pendingCount = queue.count
            persistQueue()
            // 3. Attempt to drain (no-op if offline).
            await flush()
        }

        // MARK: Flush (server-authoritative reconciliation)

        /// Drain the queue IN ORDER against the server. No-op if offline. See the
        /// file header for the per-outcome reconciliation rules. The loop exits
        /// when the queue empties OR `isOnline` flips to false (transport error).
        public func flush() async {
            while isOnline, let head = queue.first {
                let baseRevision = cache?.map.revision ?? 0
                do {
                    let outcome = try await client.applyOperations(
                        mapID,
                        operations: head.operations,
                        baseRevision: baseRevision,
                        idempotencyKey: head.idempotencyKey,
                        envelopeID: nil,
                        utteranceID: nil)
                    handleOutcomeForHead(outcome)
                    // On idempotentReplay we refresh to re-anchor on the head.
                    if case .idempotentReplay = outcome {
                        try? await refresh()
                    }
                } catch LTM.APIError.revisionConflict {
                    // Rebase: pull the fresh authoritative map, then retry ONCE.
                    // Advances the queue (landed OR escalated) or flips offline.
                    await retryAfterConflict(head)
                } catch LTM.APIError.transport {
                    // Network down ⇒ STOP draining and leave the rest of the queue
                    // intact for a later flush. (Checked BEFORE the generic
                    // APIError case so a transport error never escalates a conflict.)
                    isOnline = false
                    return
                } catch is LTM.APIError {
                    // A genuine rejection (validation_failed / http 4xx / …) ⇒
                    // escalate to conflicts; don't block the rest of the queue.
                    escalateHead(head)
                } catch {
                    // Any other unexpected error ⇒ treat as a network stop, leaving
                    // the queue intact rather than silently dropping the op.
                    isOnline = false
                    return
                }
            }
        }

        // MARK: Interpret (online only)

        /// `POST /interpret` — ONLINE only. On `.applied`, adopt the authoritative
        /// map. Throws `LTM.APIError.transport` if called offline.
        public func interpret(
            text: String,
            intent: LTM.InterpretIntent,
            focusNodeID: String? = nil,
            utteranceID: String? = nil
        ) async throws {
            guard isOnline else {
                throw LTM.APIError.transport("interpret requires a network connection")
            }
            let outcome = try await client.interpret(
                mapID,
                text: text,
                utteranceID: utteranceID,
                threadID: nil,
                intent: intent,
                focusNodeID: focusNodeID)
            if case let .applied(rev, _, serverMap) = outcome {
                adopt(serverMap, syncedRevision: rev)
            }
        }

        // MARK: Restructure / consolidate (online only)

        /// `POST /consolidate` — ONLINE only. Asks the model to STAGE a restructure
        /// proposal (nothing is applied yet). On `.applied` the store adopts the
        /// authoritative map, which now carries a pending `proposals[...]` in state
        /// `proposed`. `.noOperations` (the model proposed nothing) is a no-op.
        /// Throws `LTM.APIError.transport` if called offline (like `interpret`).
        public func consolidate() async throws {
            guard isOnline else {
                throw LTM.APIError.transport("consolidate requires a network connection")
            }
            let outcome = try await client.consolidate(mapID)
            if case let .applied(rev, _, serverMap) = outcome {
                adopt(serverMap, syncedRevision: rev)
            }
        }

        /// `POST /proposals/{id}/decision` — ONLINE only. Confirm or reject a
        /// pending restructure proposal; `decision` is `"confirm"` or `"reject"`.
        /// On confirm the proposal's inner ops materialize server-side. On
        /// `.applied` the store adopts the returned authoritative map. Throws
        /// `LTM.APIError.transport` if called offline.
        public func decideProposal(_ proposalID: String, decision: String) async throws {
            guard isOnline else {
                throw LTM.APIError.transport("decideProposal requires a network connection")
            }
            let outcome = try await client.decideProposal(
                mapID, proposalID: proposalID, decision: decision)
            if case let .applied(rev, _, serverMap) = outcome {
                adopt(serverMap, syncedRevision: rev)
            }
        }

        // MARK: Ambient session attach/detach (online only)

        /// `POST /sessions` — attach a live source session so its finalized user
        /// utterances auto-map onto this board ("Listen" mode). ONLINE only.
        /// `sourceSessionID` is the id the coordinator matches — a voice client
        /// passes its **media/voice session id** (the id the server stamps onto
        /// voice-originated turns as `presence_session_id`). Returns whether the
        /// server registered the binding. Throws `.transport` if called offline.
        @discardableResult
        public func attachSession(_ sourceSessionID: String) async throws -> Bool {
            guard isOnline else {
                throw LTM.APIError.transport("attachSession requires a network connection")
            }
            return try await client.attachSession(mapID, sourceSessionID: sourceSessionID)
        }

        /// `DELETE /sessions/{id}` — detach a previously-attached source session.
        /// ONLINE only. Idempotent (returns `false` when nothing was registered).
        /// Throws `.transport` if called offline.
        @discardableResult
        public func detachSession(_ sourceSessionID: String) async throws -> Bool {
            guard isOnline else {
                throw LTM.APIError.transport("detachSession requires a network connection")
            }
            return try await client.detachSession(mapID, sourceSessionID: sourceSessionID)
        }

        // MARK: Node promotion (online only)

        /// `POST /nodes/{id}/promote` — GOVERNED promotion of a node into a
        /// durable object (`target` = `"task"` | `"memory"`). ONLINE only —
        /// the server creates the destination object, so this can't queue.
        /// Rethrows `.confirmationRequired` (409) for non-owner-asserted nodes
        /// so the caller can confirm + retry with `confirm: true`; throws
        /// `.transport` if called offline. The committed `link_promoted_object`
        /// lands on the NEXT refresh (the promote response carries no map).
        public func promoteNode(
            _ nodeID: String, target: String, confirm: Bool
        ) async throws -> LTM.PromoteNodeResult {
            guard isOnline else {
                throw LTM.APIError.transport("promoteNode requires a network connection")
            }
            return try await client.promoteNode(
                mapID, nodeID: nodeID, target: target, confirm: confirm)
        }

        // MARK: Clarifications (owner op — offline-capable)

        /// Answer / resolve a clarification. Unlike consolidate/decideProposal this
        /// is an OWNER operation, so it routes through the normal `apply` path
        /// (optimistic preview + persisted queue + flush) and therefore works
        /// offline — the resolve lands on the next flush. `state` defaults to
        /// `.answered`; pass `.dismissed`/`.deferred` to close it another way.
        public func respondClarification(
            _ clarificationID: String,
            answer: String?,
            state: LTM.ClarificationState = .answered
        ) async {
            await apply([
                .resolveClarification(
                    clarificationId: clarificationID, state: state, answer: answer)
            ])
        }

        // MARK: - Reconciliation helpers

        /// Apply the outcome for the CURRENT head to the cache + queue.
        private func handleOutcomeForHead(_ outcome: LTM.ApplyOutcome) {
            switch outcome {
            case let .applied(rev, _, serverMap):
                adopt(serverMap, syncedRevision: rev)
                dequeueHead()
            case .idempotentReplay:
                // The op already landed during a prior partial flush. Drop it; the
                // caller refreshes afterward to re-anchor on the authoritative map.
                dequeueHead()
            case .noOperations:
                // A no-op envelope — nothing changed server-side. Drop it.
                dequeueHead()
            }
        }

        /// Handle a `revision_conflict` for `head`: refresh to rebase, then retry
        /// ONCE. On a second conflict OR a genuine rejection it escalates `head` to
        /// `conflicts` and dequeues (never blocking the queue). A transport error
        /// during refresh/retry flips `isOnline` to false, leaving `head` queued.
        private func retryAfterConflict(_ head: LTM.PendingOp) async {
            // Rebase onto the fresh authoritative revision.
            do {
                try await refresh()
            } catch {
                // Couldn't refresh (likely transport) — stop the drain; leave the
                // op queued for a later flush.
                isOnline = false
                return
            }
            let baseRevision = cache?.map.revision ?? 0
            do {
                let outcome = try await client.applyOperations(
                    mapID,
                    operations: head.operations,
                    baseRevision: baseRevision,
                    idempotencyKey: head.idempotencyKey,
                    envelopeID: nil,
                    utteranceID: nil)
                handleOutcomeForHead(outcome)
                if case .idempotentReplay = outcome {
                    try? await refresh()
                }
            } catch LTM.APIError.revisionConflict {
                // Conflicted AGAIN after a fresh rebase ⇒ escalate, don't block.
                escalateHead(head)
            } catch LTM.APIError.transport {
                // Transport on the retry ⇒ network down; leave head queued.
                isOnline = false
            } catch is LTM.APIError {
                // Genuine rejection (validation_failed / http 400 / …) ⇒ escalate.
                escalateHead(head)
            } catch {
                // Any other unexpected error ⇒ leave head queued, mark offline.
                isOnline = false
            }
        }

        /// Move the head op into `conflicts`, dequeue, persist.
        private func escalateHead(_ head: LTM.PendingOp) {
            if queue.first == head {
                conflicts.append(head)
                dequeueHead()
            }
        }

        // MARK: - Cache / queue mutation + persistence

        /// Adopt an authoritative server map: it becomes the cache AND the live
        /// view, replacing any optimistic preview. `syncedRevision` defaults to
        /// the map's own revision.
        private func adopt(_ serverMap: LTM.Map, syncedRevision: UInt64? = nil) {
            let synced = syncedRevision ?? serverMap.revision
            cache = LTM.CachedMap(map: serverMap, lastSyncedRevision: synced)
            map = serverMap
            persistCache()
        }

        private func dequeueHead() {
            if !queue.isEmpty { queue.removeFirst() }
            pendingCount = queue.count
            persistQueue()
        }

        private func loadFromDisk() {
            if let data = persistence.load(cacheKey),
               let cached = try? decoder.decode(LTM.CachedMap.self, from: data) {
                cache = cached
                map = cached.map
            }
            if let data = persistence.load(queueKey),
               let ops = try? decoder.decode([LTM.PendingOp].self, from: data) {
                queue = ops
            }
            pendingCount = queue.count
        }

        private func persistQueue() {
            guard let data = try? encoder.encode(queue) else { return }
            persistence.save(queueKey, data)
        }

        private func persistCache() {
            guard let cache, let data = try? encoder.encode(cache) else { return }
            persistence.save(cacheKey, data)
        }

        // MARK: - Utilities

        /// ISO-8601 UTC timestamp for `PendingOp.createdAt`.
        private static func timestamp() -> String {
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime]
            return formatter.string(from: Date())
        }
    }
}
