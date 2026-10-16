//  LTMAPIClient.swift
//  Live Thinking Map (LTM) — S1b: the REST client over the canonical backend.
//
//  Talks to the owner surface at `/api/magician/v2/thinking-maps` implemented by
//  `magician_v2::api::thinking_maps_api`. Every request is authorized by an
//  opaque bearer whose server-side record binds the principal and workspace.
//
//  ## Decoupling / testability
//  The client depends on NOTHING from the Magios app. `baseURL`, `scope`, and the
//  `Transport` are ALL injected, so this file + the S1a wire types compile and
//  test standalone with `swiftc` (no URLSession/network needed — a mock transport
//  returns canned bytes). Production wiring lives in the app, e.g.:
//
//  ```swift
//  let client = LTM.APIClient(
//      baseURL: MagicianAccess.baseURL,
//      scope: LTM.Scope(principal: MagicianAccess.principal,
//                       workspace: MagicianAccess.workspace),
//      transport: LTM.URLSessionTransport(session: .shared))
//  ```
//
//  (Do NOT import or reference `MagicianAccess` here — that bridge is app
//  integration, wired later. Keeping it out preserves standalone compilation.)
//
//  ## Endpoint → method table (all `async throws`)
//    POST   /thinking-maps                     → createMap      → LTM.Map (201)
//    GET    /thinking-maps                     → listMaps       → [LTM.Summary]
//    GET    /thinking-maps?limit=&offset=      → listMaps(limit:offset:) → LTM.SummaryPage
//    GET    /thinking-maps/{id}                → getMap         → LTM.Map (404 → .notFound)
//    PATCH  /thinking-maps/{id}                → patchMap       → LTM.ApplyOutcome
//    POST   /thinking-maps/{id}/operations     → applyOperations→ LTM.ApplyOutcome
//    POST   /thinking-maps/{id}/interpret      → interpret      → LTM.ApplyOutcome
//    GET    /thinking-maps/{id}/events?after_seq=N → events     → [LTM.Event]
//    GET    /thinking-maps/{id}/replay?at_seq=N    → replay     → LTM.Map
//    POST   /thinking-maps/{id}/restore        → restore        → LTM.Map (201)
//    POST   /thinking-maps/{id}/nodes/{n}/promote → promoteNode  → LTM.PromoteNodeResult

import Foundation

extension LTM {

    // MARK: - Scope

    /// Scope metadata used for local state and persistence partitioning. It is
    /// not sent as HTTP authority.
    public struct Scope: Equatable, Sendable {
        public let principal: String
        public let workspace: String

        public init(principal: String, workspace: String) {
            self.principal = principal
            self.workspace = workspace
        }
    }

    // MARK: - Transport (injectable)

    /// Abstracts the HTTP round-trip so tests can substitute a mock returning
    /// canned `(Data, HTTPURLResponse)` with no network. Production uses
    /// `URLSessionTransport`.
    public protocol Transport: Sendable {
        func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
    }

    /// Production `Transport` backed by `URLSession`.
    public struct URLSessionTransport: Transport {
        public let session: URLSession
        public let extraHeaders: [String: String]

        public init(session: URLSession = .shared, extraHeaders: [String: String] = [:]) {
            self.session = session
            self.extraHeaders = extraHeaders
        }

        public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
            var request = request
            for (key, value) in extraHeaders {
                request.setValue(value, forHTTPHeaderField: key)
            }
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else {
                throw APIError.transport("non-HTTP response")
            }
            return (data, http)
        }
    }

    // MARK: - Errors

    /// Errors surfaced by the client. Non-2xx responses are mapped to precise
    /// cases where the backend uses a well-known `"error"` code, else `.http`.
    public enum APIError: Error, Equatable, Sendable {
        /// A non-2xx status with no more-specific mapping. `code` is the body's
        /// `"error"` field when present.
        case http(status: Int, code: String?)
        /// The success body failed to decode into the expected `LTM` type.
        case decoding(String)
        /// The transport itself failed (no HTTP response, network error, …).
        case transport(String)
        /// 404 `{"error":"feature_disabled"}` — the server has the feature off.
        case featureDisabled
        /// 404 `{"error":"not_found"}` — the map (or branch source) is absent.
        case notFound
        /// 409 `{"error":"already_exists"}` — a create/restore id collided.
        case alreadyExists
        /// 409 `{"error":"confirmation_required"}` — promoting a node that is
        /// not owner-asserted (model-inferred / provisional / participant)
        /// needs an explicit owner confirmation; retry with `confirm: true`.
        case confirmationRequired
        /// 409 `{"error":"revision_conflict","expected":N,"actual":M}` — the
        /// submitted `base_revision` was stale. `expected` is the revision the
        /// server would accept (i.e. the current head); offline clients should
        /// rebase onto the fresh map and retry. (Added in S1c.)
        case revisionConflict(expected: UInt64)
        /// 400 `{"error":"missing_scope"}` — no principal/workspace resolved.
        case missingScope
        /// 400 `{"error":"validation_failed"}` — reducer/store rejected the op.
        case validationFailed
        /// 400 `{"error":"invalid_id"}` — a malformed map/branch id.
        case invalidId
        /// 400 `{"error":"nothing_to_patch"}` — PATCH with neither field.
        case nothingToPatch
        /// 400 `{"error":"invalid_focus_node"}` — the request-scoped node is
        /// absent or was removed before interpretation began.
        case invalidFocusNode
        /// 502 `{"error":"interpretation_failed"}` — the interpreter LLM/parse
        /// failed.
        case interpretationFailed
        /// 503 `{"error":"llm_unavailable"}` — no LLM router is configured.
        case llmUnavailable
        /// 503 `{"error":"coordinator_unavailable"}` — the ambient session
        /// coordinator is not wired into the running server, so attach/detach
        /// cannot register a live source session. (Distinct from
        /// `.llmUnavailable`; used by `attachSession`/`detachSession`.)
        case coordinatorUnavailable
        /// 500 `{"error":"corrupt"}` — a stored map failed to load.
        case corrupt
        /// 500 `{"error":"io_error"}` — an underlying store IO failure.
        case ioError
    }

    // MARK: - Client

    /// REST client for the canonical Thinking Maps surface. `baseURL` + `scope` +
    /// `transport` are injected → fully testable with no app dependency.
    public final class APIClient {
        private let baseURL: URL
        private let scope: Scope
        private let transport: Transport
        // The ONE canonical wire coder pair (shared with the S1a types).
        private let encoder = LTM.Wire.makeEncoder()
        private let decoder = LTM.Wire.makeDecoder()

        public init(baseURL: URL, scope: Scope, transport: Transport) {
            self.baseURL = baseURL
            self.scope = scope
            self.transport = transport
        }

        // ── Endpoints ──────────────────────────────────────────────────────────

        /// `POST /thinking-maps` — create a map. 201 → `LTM.Map`.
        public func createMap(
            title: String, source: LTM.Source?, mapID: String?
        ) async throws -> LTM.Map {
            let body = CreateMapBody(title: title, source: source, mapId: mapID)
            let request = try makeRequest(
                method: "POST", path: "/thinking-maps", body: body)
            return try await send(request, expecting: LTM.Map.self, successStatuses: [200, 201])
        }

        /// `GET /thinking-maps` — list ALL summaries (legacy bare array,
        /// most recently updated first). 200 → `[LTM.Summary]`.
        public func listMaps() async throws -> [LTM.Summary] {
            let request = try makeRequest(method: "GET", path: "/thinking-maps")
            return try await send(request, expecting: [LTM.Summary].self)
        }

        /// `GET /thinking-maps?limit=&offset=` — one server-paginated page
        /// (most recently updated first; the server clamps limit to 1...200).
        /// 200 → `LTM.SummaryPage`.
        public func listMaps(limit: Int, offset: Int = 0) async throws -> LTM.SummaryPage {
            let request = try makeRequest(
                method: "GET", path: "/thinking-maps",
                query: [
                    URLQueryItem(name: "limit", value: String(limit)),
                    URLQueryItem(name: "offset", value: String(offset)),
                ])
            return try await send(request, expecting: LTM.SummaryPage.self)
        }

        /// `GET /thinking-maps/{id}` — fetch one map. 200 → `LTM.Map`
        /// (404 → `.notFound`).
        public func getMap(_ id: String) async throws -> LTM.Map {
            let request = try makeRequest(
                method: "GET", path: "/thinking-maps/\(encodePathSegment(id))")
            return try await send(request, expecting: LTM.Map.self)
        }

        /// `PATCH /thinking-maps/{id}` — update title and/or lifecycle. At least
        /// one must be non-nil (else the server returns 400 `nothing_to_patch`).
        /// 200 → `LTM.ApplyOutcome`.
        public func patchMap(
            _ id: String, title: String?, lifecycle: LTM.MapLifecycle?
        ) async throws -> LTM.ApplyOutcome {
            let body = PatchMapBody(title: title, lifecycle: lifecycle)
            let request = try makeRequest(
                method: "PATCH", path: "/thinking-maps/\(encodePathSegment(id))", body: body)
            return try await send(request, expecting: LTM.ApplyOutcome.self)
        }

        /// `POST /thinking-maps/{id}/operations` — apply an owner envelope. The
        /// server forces `actor = Owner{principal}` + `map_id = {id}`; those are
        /// intentionally NOT part of the body. 200 → `LTM.ApplyOutcome`.
        public func applyOperations(
            _ id: String,
            operations: [LTM.Operation],
            baseRevision: UInt64,
            idempotencyKey: String,
            envelopeID: String?,
            utteranceID: String?
        ) async throws -> LTM.ApplyOutcome {
            let body = ApplyOperationsBody(
                operations: operations,
                idempotencyKey: idempotencyKey,
                baseRevision: baseRevision,
                envelopeId: envelopeID,
                utteranceId: utteranceID)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/operations",
                body: body)
            return try await send(request, expecting: LTM.ApplyOutcome.self)
        }

        /// `POST /thinking-maps/{id}/interpret` — interpret an utterance into a
        /// model-authored envelope and apply it. A valid zero-move interpretation
        /// returns `LTM.ApplyOutcome.noOperations`. 200 → `LTM.ApplyOutcome`.
        public func interpret(
            _ id: String,
            text: String,
            utteranceID: String?,
            threadID: String?,
            intent: LTM.InterpretIntent,
            focusNodeID: String? = nil
        ) async throws -> LTM.ApplyOutcome {
            let body = InterpretBody(
                utteranceId: utteranceID,
                text: text,
                threadId: threadID,
                intent: intent,
                focusNodeId: focusNodeID)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/interpret",
                body: body)
            return try await send(request, expecting: LTM.ApplyOutcome.self)
        }

        /// `GET /thinking-maps/{id}/events?after_seq=N` — events after a
        /// sequence. 200 → `[LTM.Event]`.
        public func events(_ id: String, afterSeq: UInt64) async throws -> [LTM.Event] {
            let request = try makeRequest(
                method: "GET",
                path: "/thinking-maps/\(encodePathSegment(id))/events",
                query: [URLQueryItem(name: "after_seq", value: String(afterSeq))])
            return try await send(request, expecting: [LTM.Event].self)
        }

        /// `GET /thinking-maps/{id}/replay?at_seq=N` — the map replayed to a
        /// sequence. 200 → `LTM.Map`.
        public func replay(_ id: String, atSeq: UInt64) async throws -> LTM.Map {
            let request = try makeRequest(
                method: "GET",
                path: "/thinking-maps/\(encodePathSegment(id))/replay",
                query: [URLQueryItem(name: "at_seq", value: String(atSeq))])
            return try await send(request, expecting: LTM.Map.self)
        }

        /// `POST /thinking-maps/{id}/restore` — fork a new map from a historical
        /// sequence, leaving the source untouched. 201 → `LTM.Map` (the branch).
        public func restore(
            _ id: String, atSequence: UInt64, newMapID: String, newTitle: String
        ) async throws -> LTM.Map {
            let body = RestoreBody(
                atSequence: atSequence, newMapId: newMapID, newTitle: newTitle)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/restore",
                body: body)
            return try await send(request, expecting: LTM.Map.self, successStatuses: [200, 201])
        }

        /// `POST /thinking-maps/{id}/consolidate` — ask the model to propose a
        /// reorganization, staged as a PENDING restructure proposal (nothing is
        /// applied yet). Empty body. 200 → `LTM.ApplyOutcome` (`.applied` with the
        /// map now carrying a `proposals[...]` in state `proposed`, or
        /// `.noOperations` when the model proposes nothing).
        public func consolidate(_ id: String) async throws -> LTM.ApplyOutcome {
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/consolidate",
                body: EmptyBody())
            return try await send(request, expecting: LTM.ApplyOutcome.self)
        }

        /// `POST /thinking-maps/{id}/proposals/{proposalID}/decision` — confirm or
        /// reject a pending restructure proposal. Body `{"decision": decision}`
        /// where `decision` is `"confirm"` or `"reject"`. On confirm the proposal's
        /// inner ops materialize. 200 → `LTM.ApplyOutcome` (`.applied`).
        public func decideProposal(
            _ id: String, proposalID: String, decision: String
        ) async throws -> LTM.ApplyOutcome {
            let body = ProposalDecisionBody(decision: decision)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/proposals/\(encodePathSegment(proposalID))/decision",
                body: body)
            return try await send(request, expecting: LTM.ApplyOutcome.self)
        }

        /// `POST /thinking-maps/{id}/sessions` — attach a live source session so
        /// its finalized user utterances auto-map onto this board (ambient
        /// "Listen" mode). `sourceSessionID` is matched by the coordinator
        /// against a chat message's `session_id` OR its `presence_session_id`,
        /// so a voice client passes its **media/voice session id** here (the id
        /// the server stamps onto voice-originated turns). 200 → `true`
        /// (404 `.notFound` if the map is absent, 503 `.coordinatorUnavailable`
        /// if the server has no ambient coordinator wired).
        @discardableResult
        public func attachSession(
            _ id: String, sourceSessionID: String
        ) async throws -> Bool {
            let body = AttachSessionBody(sourceSessionId: sourceSessionID)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/sessions",
                body: body)
            return try await send(request, expecting: AttachSessionResponse.self).attached
        }

        /// `DELETE /thinking-maps/{id}/sessions/{sourceSessionID}` — detach a
        /// live source session. Idempotent: 200 → `false` when the session was
        /// not registered (503 `.coordinatorUnavailable` if no coordinator).
        @discardableResult
        public func detachSession(
            _ id: String, sourceSessionID: String
        ) async throws -> Bool {
            let request = try makeRequest(
                method: "DELETE",
                path: "/thinking-maps/\(encodePathSegment(id))/sessions/\(encodePathSegment(sourceSessionID))")
            return try await send(request, expecting: DetachSessionResponse.self).detached
        }

        /// `POST /thinking-maps/{id}/nodes/{nodeID}/promote` — GOVERNED
        /// promotion of a node into a durable Magician object. `target` is
        /// `"task"` or `"memory"`. Owner-asserted nodes promote directly;
        /// anything else (model-inferred / provisional) returns
        /// 409 `confirmation_required` (→ `.confirmationRequired`) until the
        /// caller retries with `confirm: true` — the confirmation is recorded
        /// server-side as an owner assertion. IDEMPOTENT: a node already
        /// linked to an object of this kind returns `promoted: false` with the
        /// EXISTING object id (no duplicate is created).
        public func promoteNode(
            _ id: String, nodeID: String, target: String, confirm: Bool
        ) async throws -> LTM.PromoteNodeResult {
            let body = PromoteNodeBody(target: target, confirm: confirm)
            let request = try makeRequest(
                method: "POST",
                path: "/thinking-maps/\(encodePathSegment(id))/nodes/\(encodePathSegment(nodeID))/promote",
                body: body)
            return try await send(request, expecting: LTM.PromoteNodeResult.self)
        }

        // ── Request construction ────────────────────────────────────────────────

        /// Build a request with no body (GET) — headers + optional query only.
        private func makeRequest(
            method: String, path: String, query: [URLQueryItem] = []
        ) throws -> URLRequest {
            try makeRequest(method: method, path: path, query: query, body: Optional<Never>.none)
        }

        /// Build a request with an encodable JSON body.
        private func makeRequest<Body: Encodable>(
            method: String, path: String, query: [URLQueryItem] = [], body: Body?
        ) throws -> URLRequest {
            guard var components = URLComponents(
                url: baseURL.appendingPathComponent(basePath + path),
                resolvingAgainstBaseURL: false)
            else {
                throw APIError.transport("could not build URL for \(path)")
            }
            if !query.isEmpty { components.queryItems = query }
            guard let url = components.url else {
                throw APIError.transport("could not resolve URL for \(path)")
            }

            var request = URLRequest(url: url)
            request.httpMethod = method
            if let body = body {
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                do {
                    request.httpBody = try encoder.encode(body)
                } catch {
                    throw APIError.decoding("encode \(Body.self): \(error)")
                }
            }
            return request
        }

        // ── Send + decode ───────────────────────────────────────────────────────

        private func send<T: Decodable>(
            _ request: URLRequest,
            expecting: T.Type,
            successStatuses: Set<Int> = [200]
        ) async throws -> T {
            let (data, http): (Data, HTTPURLResponse)
            do {
                (data, http) = try await transport.send(request)
            } catch let error as APIError {
                throw error
            } catch {
                throw APIError.transport(String(describing: error))
            }

            guard successStatuses.contains(http.statusCode) else {
                throw mapError(status: http.statusCode, data: data)
            }
            do {
                return try decoder.decode(T.self, from: data)
            } catch {
                throw APIError.decoding("decode \(T.self): \(error)")
            }
        }

        // ── Error mapping ─────────────────────────────────────────────────────────

        /// Map a non-2xx `(status, body)` to a precise `APIError`. Reads the
        /// body's `"error"` field and maps the documented codes to dedicated
        /// cases; anything unrecognized falls back to `.http(status, code)`.
        private func mapError(status: Int, data: Data) -> APIError {
            let code = Self.errorCode(from: data)
            switch (status, code) {
            case (404, "feature_disabled"): return .featureDisabled
            case (404, "not_found"): return .notFound
            case (409, "already_exists"): return .alreadyExists
            case (409, "confirmation_required"): return .confirmationRequired
            case (409, "revision_conflict"):
                // Rebase signal: body carries {"expected":N,"actual":M}. `expected`
                // is the head the server will accept; used by the SyncStore to
                // refresh + retry. Default 0 if the field is somehow absent.
                return .revisionConflict(expected: Self.expectedRevision(from: data) ?? 0)
            case (400, "missing_scope"): return .missingScope
            case (400, "validation_failed"): return .validationFailed
            case (400, "invalid_id"): return .invalidId
            case (400, "nothing_to_patch"): return .nothingToPatch
            case (400, "invalid_focus_node"): return .invalidFocusNode
            case (502, "interpretation_failed"): return .interpretationFailed
            case (503, "coordinator_unavailable"): return .coordinatorUnavailable
            case (503, "llm_unavailable"): return .llmUnavailable
            case (503, _): return .llmUnavailable
            case (500, "corrupt"): return .corrupt
            case (500, "io_error"): return .ioError
            default: return .http(status: status, code: code)
            }
        }

        /// Extract the `"error"` string from a `{"error":"...", ...}` body, if any.
        private static func errorCode(from data: Data) -> String? {
            guard
                let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { return nil }
            return object["error"] as? String
        }

        /// Extract the `"expected"` revision from a `revision_conflict` body.
        /// Accepts any JSON number shape (`NSNumber`) → `UInt64`.
        private static func expectedRevision(from data: Data) -> UInt64? {
            guard
                let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                let expected = object["expected"] as? NSNumber
            else { return nil }
            return expected.uint64Value
        }

        // ── Path helpers ──────────────────────────────────────────────────────────

        /// The scope prefix the routes are mounted under (see `magician.rs`:
        /// `web::scope("/api/magician/v2")` → `thinking_maps_api::configure`'s
        /// `web::scope("/thinking-maps")`).
        private let basePath = "/api/magician/v2"

        /// Percent-encode one path segment (an id may contain `/`, spaces, …).
        private func encodePathSegment(_ segment: String) -> String {
            segment.addingPercentEncoding(withAllowedCharacters: Self.pathSegmentAllowed)
                ?? segment
        }

        /// URL path characters allowed in a single segment (RFC 3986 `pchar`
        /// minus the sub-delims we conservatively drop, and WITHOUT `/`).
        private static let pathSegmentAllowed: CharacterSet = {
            var set = CharacterSet.alphanumerics
            set.insert(charactersIn: "-._~")
            return set
        }()
    }
}

extension LTM.APIError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .http(let status, let code):
            let suffix = code.map { " (\($0))" } ?? ""
            return "Thinking Map request failed with HTTP \(status)\(suffix)."
        case .decoding:
            return "The backend returned a Thinking Map response the app could not read."
        case .transport(let detail):
            return detail.isEmpty
                ? "The backend could not be reached."
                : "The backend could not be reached: \(detail)"
        case .featureDisabled:
            return "Thinking Maps are disabled on this backend."
        case .notFound:
            return "This Thinking Map is no longer available."
        case .alreadyExists:
            return "A Thinking Map with that identifier already exists."
        case .confirmationRequired:
            return "This change needs your confirmation before it can continue."
        case .revisionConflict:
            return "This Thinking Map changed elsewhere. Refresh it and try again."
        case .missingScope:
            return "The backend could not determine the active workspace."
        case .validationFailed:
            return "The Thinking Map rejected this change as invalid."
        case .invalidId:
            return "The Thinking Map identifier is invalid."
        case .nothingToPatch:
            return "There are no Thinking Map changes to save."
        case .invalidFocusNode:
            return "That thought is no longer available on this map. Refresh the map and try again."
        case .interpretationFailed:
            return "The facilitator could not shape the next Thinking Map branches. Try again."
        case .llmUnavailable:
            return "Thinking Map intelligence is temporarily unavailable."
        case .coordinatorUnavailable:
            return "Live Thinking Map listening is temporarily unavailable."
        case .corrupt:
            return "This Thinking Map could not be read from storage."
        case .ioError:
            return "The backend could not access Thinking Map storage."
        }
    }
}

// MARK: - Request bodies (client-side, snake_case wire keys)

extension LTM {
    /// `POST /thinking-maps` body. `source`/`map_id` are omitted when nil.
    fileprivate struct CreateMapBody: Encodable {
        let title: String
        let source: LTM.Source?
        let mapId: String?

        private enum CodingKeys: String, CodingKey {
            case title
            case source
            case mapId = "map_id"
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(title, forKey: .title)
            try c.encodeIfPresent(source, forKey: .source)
            try c.encodeIfPresent(mapId, forKey: .mapId)
        }
    }

    /// `PATCH /thinking-maps/{id}` body. Both fields omitted when nil.
    fileprivate struct PatchMapBody: Encodable {
        let title: String?
        let lifecycle: LTM.MapLifecycle?

        private enum CodingKeys: String, CodingKey {
            case title
            case lifecycle
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(title, forKey: .title)
            try c.encodeIfPresent(lifecycle, forKey: .lifecycle)
        }
    }

    /// `POST /thinking-maps/{id}/operations` body. `operations` is always sent
    /// (may be empty); `envelope_id`/`utterance_id` omitted when nil.
    fileprivate struct ApplyOperationsBody: Encodable {
        let operations: [LTM.Operation]
        let idempotencyKey: String
        let baseRevision: UInt64
        let envelopeId: String?
        let utteranceId: String?

        private enum CodingKeys: String, CodingKey {
            case operations
            case idempotencyKey = "idempotency_key"
            case baseRevision = "base_revision"
            case envelopeId = "envelope_id"
            case utteranceId = "utterance_id"
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(operations, forKey: .operations)
            try c.encode(idempotencyKey, forKey: .idempotencyKey)
            try c.encode(baseRevision, forKey: .baseRevision)
            try c.encodeIfPresent(envelopeId, forKey: .envelopeId)
            try c.encodeIfPresent(utteranceId, forKey: .utteranceId)
        }
    }

    /// `POST /thinking-maps/{id}/interpret` body. `text` always sent; the rest
    /// omitted when nil. `intent` is the snake_case string.
    fileprivate struct InterpretBody: Encodable {
        let utteranceId: String?
        let text: String
        let threadId: String?
        let intent: LTM.InterpretIntent
        let focusNodeId: String?

        private enum CodingKeys: String, CodingKey {
            case utteranceId = "utterance_id"
            case text
            case threadId = "thread_id"
            case intent
            case focusNodeId = "focus_node_id"
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(utteranceId, forKey: .utteranceId)
            try c.encode(text, forKey: .text)
            try c.encodeIfPresent(threadId, forKey: .threadId)
            try c.encode(intent, forKey: .intent)
            try c.encodeIfPresent(focusNodeId, forKey: .focusNodeId)
        }
    }

    /// `POST /thinking-maps/{id}/restore` body — all fields required.
    fileprivate struct RestoreBody: Encodable {
        let atSequence: UInt64
        let newMapId: String
        let newTitle: String

        private enum CodingKeys: String, CodingKey {
            case atSequence = "at_sequence"
            case newMapId = "new_map_id"
            case newTitle = "new_title"
        }
    }

    /// An empty JSON object body `{}` — used by `consolidate` (the handler takes
    /// no parameters but the client always sends `application/json`).
    fileprivate struct EmptyBody: Encodable {
        func encode(to encoder: Encoder) throws {
            _ = encoder.container(keyedBy: EmptyCodingKey.self)
        }
        private enum EmptyCodingKey: CodingKey {}
    }

    /// `POST /thinking-maps/{id}/proposals/{pid}/decision` body — `{"decision":…}`.
    fileprivate struct ProposalDecisionBody: Encodable {
        let decision: String

        private enum CodingKeys: String, CodingKey {
            case decision
        }
    }

    /// `POST /thinking-maps/{id}/nodes/{nodeID}/promote` body —
    /// `{"target":"task"|"memory","confirm":…}`.
    fileprivate struct PromoteNodeBody: Encodable {
        let target: String
        let confirm: Bool

        private enum CodingKeys: String, CodingKey {
            case target
            case confirm
        }
    }

    /// `POST /thinking-maps/{id}/nodes/{nodeID}/promote` response —
    /// `{"promoted":…,"object_kind":…,"object_id":…}`. `promoted == false`
    /// means the node ALREADY carried a promotion link of this kind and the
    /// existing object was returned (idempotent replay — no duplicate).
    public struct PromoteNodeResult: Decodable, Equatable, Sendable {
        public let promoted: Bool
        public let objectKind: PromotionKind
        public let objectId: String

        private enum CodingKeys: String, CodingKey {
            case promoted
            case objectKind = "object_kind"
            case objectId = "object_id"
        }
    }

    /// `POST /thinking-maps/{id}/sessions` body — `{"source_session_id":…}`.
    fileprivate struct AttachSessionBody: Encodable {
        let sourceSessionId: String

        private enum CodingKeys: String, CodingKey {
            case sourceSessionId = "source_session_id"
        }
    }

    /// `POST /thinking-maps/{id}/sessions` response — `{"attached":true,…}`.
    fileprivate struct AttachSessionResponse: Decodable {
        let attached: Bool
    }

    /// `DELETE /thinking-maps/{id}/sessions/{sid}` response — `{"detached":…}`.
    fileprivate struct DetachSessionResponse: Decodable {
        let detached: Bool
    }
}
