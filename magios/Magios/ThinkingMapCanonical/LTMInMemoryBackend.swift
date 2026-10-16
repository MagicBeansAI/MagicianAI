//  LTMInMemoryBackend.swift
//  Live Thinking Map (LTM) — serverless backing for UI tests + demo mode.
//
//  Under `--ui-test` the app is deliberately OFFLINE (no backend fetches), but
//  the canonical thinking map is server-backed — so the demo seed
//  (`ThinkingMapModel.loadExample`) and the library (create/list/open) would
//  dead-end without a server. E0 never had this problem (its demo lived in
//  `UserDefaults`), and the E0-era XCUITests are the parity contract that
//  caught the gap.
//
//  `LTM.InMemoryTransport` is a tiny in-process fake of the thinking-maps REST
//  surface: it holds maps in a dictionary and answers the exact requests
//  `LTM.APIClient` makes. Operations are applied with `LTM.optimisticApply`
//  (the same cosmetic subset the offline preview uses) plus the two ops UI
//  tests additionally need (`set_shared_view` for the active-node cursor,
//  `disconnect`). It is NOT a reducer: no authority checks, no idempotency
//  ledger, no event log — deterministic-enough state for driving the UI, and
//  nothing more. Production never constructs it; the wiring lives in
//  `CanonicalThinkingMapBackend`'s convenience init behind the `--ui-test`
//  launch argument.

import Foundation

extension LTM {

    /// Dict-backed `Persistence` for UI-test launches: state lives and dies
    /// with the process, so no `FilePersistence` cache leaks between tests
    /// (the simulator app install persists across test cases).
    public final class EphemeralPersistence: Persistence, @unchecked Sendable {
        private let lock = NSLock()
        private var storage: [String: Data] = [:]

        public init() {}

        public func load(_ key: String) -> Data? {
            lock.lock()
            defer { lock.unlock() }
            return storage[key]
        }

        public func save(_ key: String, _ data: Data) {
            lock.lock()
            defer { lock.unlock() }
            storage[key] = data
        }
    }

    /// In-process fake of the thinking-maps REST surface (see the file header).
    public final class InMemoryTransport: Transport, @unchecked Sendable {
        /// One store per process so every `SyncStore`/`APIClient` the app
        /// constructs during a UI test sees the same maps.
        public static let shared = InMemoryTransport()

        private let lock = NSLock()
        private var maps: [String: LTM.Map] = [:]

        private let encoder = LTM.Wire.makeEncoder()
        private let decoder = LTM.Wire.makeDecoder()

        public init() {}

        // ── Transport ────────────────────────────────────────────────────────

        public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
            let method = request.httpMethod ?? "GET"
            let path = request.url?.path ?? ""
            // Everything below `/thinking-maps`: ["", "api", "magician", "v2",
            // "thinking-maps", {id}, {action}, ...]
            let segments = path.split(separator: "/").map(String.init)
            guard let base = segments.firstIndex(of: "thinking-maps") else {
                return respond(status: 404, json: ["error": "not_found"], to: request)
            }
            let rest = Array(segments[(base + 1)...])

            return lock.withLock {
                switch (method, rest.count) {
                case ("POST", 0):
                    return createMap(request)
                case ("GET", 0):
                    return listMaps(request)
                case ("GET", 1):
                    return getMap(id: rest[0], request)
                case ("PATCH", 1):
                    return patchMap(id: rest[0], request)
                case ("POST", 2) where rest[1] == "operations":
                    return applyOperations(id: rest[0], request)
                case ("POST", 2) where rest[1] == "interpret":
                    // The UI-test palette short-circuits before interpret is ever
                    // called (`--ui-test` → the DEMO fallback), so any arrival here
                    // just degrades the same way a serverless environment would.
                    return respond(status: 503, json: ["error": "llm_unavailable"], to: request)
                case ("POST", 2) where rest[1] == "sessions":
                    return respond(status: 200, json: ["attached": true], to: request)
                case ("DELETE", 3) where rest[1] == "sessions":
                    return respond(status: 200, json: ["detached": true], to: request)
                default:
                    return respond(status: 404, json: ["error": "not_found"], to: request)
                }
            }
        }

        // ── Handlers (caller holds the lock) ─────────────────────────────────

        private func createMap(_ request: URLRequest) -> (Data, HTTPURLResponse) {
            let body = jsonBody(request)
            let title = (body["title"] as? String) ?? "Untitled"
            let mapID = (body["map_id"] as? String) ?? UUID().uuidString
            if maps[mapID] != nil {
                return respond(status: 409, json: ["error": "already_exists"], to: request)
            }
            let now = timestamp()
            let map = LTM.Map(
                schemaVersion: 1, mapId: mapID, principal: "anonymous",
                workspace: "default", title: title, source: .solo, revision: 0,
                createdAt: now, updatedAt: now)
            maps[mapID] = map
            return respondWire(status: 201, map, to: request)
        }

        private func listMaps(_ request: URLRequest) -> (Data, HTTPURLResponse) {
            let summaries = maps.values
                .sorted { $0.updatedAt > $1.updatedAt }
                .map {
                    LTM.Summary(
                        mapId: $0.mapId, title: $0.title, lifecycle: $0.lifecycle,
                        latestRevision: $0.revision, updatedAt: $0.updatedAt,
                        nodePreview: Self.nodePreview(for: $0))
                }
            // Server parity: `?limit=` selects the page envelope (clamped to
            // 1...200); no limit keeps the legacy bare array.
            let query = request.url.flatMap {
                URLComponents(url: $0, resolvingAgainstBaseURL: false)?.queryItems
            }
            guard let rawLimit = query?.first(where: { $0.name == "limit" })?.value,
                let parsedLimit = Int(rawLimit)
            else {
                return respondWire(status: 200, summaries, to: request)
            }
            let limit = min(max(parsedLimit, 1), 200)
            let offset = query?.first(where: { $0.name == "offset" })?.value
                .flatMap(Int.init) ?? 0
            let window = summaries.dropFirst(offset).prefix(limit)
            let page = LTM.SummaryPage(
                maps: Array(window), total: summaries.count, offset: offset, limit: limit)
            return respondWire(status: 200, page, to: request)
        }

        /// Server-parity bounded node preview: the first 10 live nodes (ordered by
        /// createdAt then nodeId) + their branch edges among the previewed set.
        /// Mirrors the Rust `NodePreview::from_map`. `nil` for an empty map.
        private static func nodePreview(for map: LTM.Map) -> LTM.NodePreview? {
            let maxNodes = 10
            var live = map.nodes.values.filter { !$0.tombstoned }
            guard !live.isEmpty else { return nil }
            live.sort {
                $0.createdAt == $1.createdAt ? $0.nodeId < $1.nodeId : $0.createdAt < $1.createdAt
            }
            let capped = Array(live.prefix(maxNodes))
            let previewIDs = Set(capped.map(\.nodeId))
            var nodes: [LTM.NodePreviewNode] = []
            var edges: [LTM.NodePreviewEdge] = []
            for node in capped {
                let parentID = node.parentId.flatMap { previewIDs.contains($0) ? $0 : nil }
                if let parentID {
                    edges.append(LTM.NodePreviewEdge(from: parentID, to: node.nodeId))
                }
                let suggested = node.assertionOrigin == .modelInferred
                    || node.epistemicState == .provisional
                nodes.append(LTM.NodePreviewNode(
                    nodeId: node.nodeId, parentId: parentID, kind: node.kind,
                    suggested: suggested, title: String(node.label.prefix(80))))
            }
            return LTM.NodePreview(nodes: nodes, edges: edges)
        }

        private func getMap(id: String, _ request: URLRequest) -> (Data, HTTPURLResponse) {
            guard let map = maps[id] else {
                return respond(status: 404, json: ["error": "not_found"], to: request)
            }
            return respondWire(status: 200, map, to: request)
        }

        private func patchMap(id: String, _ request: URLRequest) -> (Data, HTTPURLResponse) {
            guard var map = maps[id] else {
                return respond(status: 404, json: ["error": "not_found"], to: request)
            }
            let body = jsonBody(request)
            if let title = body["title"] as? String { map.title = title }
            if let raw = body["lifecycle"] as? String,
               let lifecycle = LTM.MapLifecycle(rawValue: raw) {
                map.lifecycle = lifecycle
            }
            bump(&map)
            maps[id] = map
            return applied(map, to: request)
        }

        private func applyOperations(id: String, _ request: URLRequest) -> (Data, HTTPURLResponse) {
            guard var map = maps[id] else {
                return respond(status: 404, json: ["error": "not_found"], to: request)
            }
            guard let data = request.httpBody,
                  let envelope = try? decoder.decode(OperationsBody.self, from: data)
            else {
                return respond(status: 400, json: ["error": "validation_failed"], to: request)
            }
            LTM.optimisticApply(&map, envelope.operations)
            // The two ops the UI needs that the cosmetic preview skips.
            for op in envelope.operations {
                switch op {
                case let .setSharedView(viewState):
                    map.viewState = viewState
                case let .disconnect(edgeId):
                    if var edge = map.edges[edgeId] {
                        edge.tombstoned = true
                        map.edges[edgeId] = edge
                    }
                default:
                    break
                }
            }
            bump(&map)
            maps[id] = map
            return applied(map, to: request)
        }

        // ── Helpers ──────────────────────────────────────────────────────────

        /// The wire shape of `POST /{id}/operations` (mirrors the client's
        /// fileprivate body struct; only the fields the fake needs).
        private struct OperationsBody: Decodable {
            let operations: [LTM.Operation]

            private enum CodingKeys: String, CodingKey {
                case operations
            }
        }

        private func bump(_ map: inout LTM.Map) {
            map.revision += 1
            map.updatedAt = timestamp()
        }

        private func applied(_ map: LTM.Map, to request: URLRequest) -> (Data, HTTPURLResponse) {
            let outcome = LTM.ApplyOutcome.applied(
                resultingRevision: map.revision,
                semanticHash: "in-memory",
                map: map)
            return respondWire(status: 200, outcome, to: request)
        }

        private func jsonBody(_ request: URLRequest) -> [String: Any] {
            guard let data = request.httpBody,
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { return [:] }
            return object
        }

        private func respondWire<T: Encodable>(
            status: Int, _ value: T, to request: URLRequest
        ) -> (Data, HTTPURLResponse) {
            let data = (try? encoder.encode(value)) ?? Data("{}".utf8)
            return (data, httpResponse(status: status, request))
        }

        private func respond(
            status: Int, json: [String: Any], to request: URLRequest
        ) -> (Data, HTTPURLResponse) {
            let data = (try? JSONSerialization.data(withJSONObject: json)) ?? Data("{}".utf8)
            return (data, httpResponse(status: status, request))
        }

        private func httpResponse(status: Int, _ request: URLRequest) -> HTTPURLResponse {
            HTTPURLResponse(
                url: request.url ?? URL(string: "memory://thinking-maps")!,
                statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])!
        }

        private func timestamp() -> String {
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime]
            return formatter.string(from: Date())
        }
    }
}
