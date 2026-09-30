//  MonitorAPIClient.swift
//  Recurring Monitors (Phase 5, iOS) — the REST client over the Phase 1-3
//  backend routes under `/api/magician/v3/monitors` (monitors_api.rs).
//
//  Mirrors the `LTM.APIClient` idiom: `baseURL` + `scope` + `transport` are ALL
//  injected (tests use a mock transport returning canned bytes; production
//  wires `MagicianAccess` at the call site — never referenced here). Every
//  request authority comes from the scoped bearer attached by the transport.
//
//  ## Endpoint → method table (all `async throws`)
//    GET    /monitors?limit=&cursor=&state=       → list          → ListPageV1
//    GET    /monitors/{id}                        → detail        → DetailV1
//    POST   /monitors                             → create        → MutationResponseV1 (201)
//    PATCH  /monitors/{id}                        → update        → MutationResponseV1
//    POST   /monitors/{id}/convert                → convert       → ConvertResponseV1
//    DELETE /monitors/{id}?remove_files=          → delete        → DeleteResponseV1
//    POST   /monitors/{id}/pause                  → pause         → StateChangeResponseV1
//    POST   /monitors/{id}/resume                 → resume        → StateChangeResponseV1
//    POST   /monitors/{id}/run                    → runNow        → Void (202 accepted)
//    GET    /monitors/{id}/runs?limit=            → runs          → ItemsPageV1<RunResultV1>
//    GET    /monitors/{id}/updates?limit=         → updates       → ItemsPageV1<UpdateDetailV1>
//    GET    /monitor-updates?limit=               → scopeUpdates  → ItemsPageV1<UpdateDetailV1>
//    POST   /monitors/{id}/updates/{uid}/feedback → submitFeedback → FeedbackResponseV1
//    GET    /monitors/{id}/feedback?limit=        → feedback      → ItemsPageV1<FeedbackRecordV1>

import Foundation

extension Monitors {

    // MARK: - Scope

    /// Scope metadata used for local client state. HTTP authority comes from
    /// the opaque bearer attached by the injected transport.
    struct Scope: Equatable, Sendable {
        let principal: String
        let workspace: String

        init(principal: String, workspace: String) {
            self.principal = principal
            self.workspace = workspace
        }
    }

    // MARK: - Transport (injectable)

    /// Abstracts the HTTP round-trip so tests substitute a mock returning
    /// canned `(Data, HTTPURLResponse)` with no network.
    protocol Transport: Sendable {
        func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
    }

    /// Production `Transport` backed by `URLSession`. `extraHeaders` lets the
    /// app attach its gateway headers (CF-Access) without the client knowing.
    struct URLSessionTransport: Transport {
        let session: URLSession
        let extraHeaders: [String: String]

        init(session: URLSession = .shared, extraHeaders: [String: String] = [:]) {
            self.session = session
            self.extraHeaders = extraHeaders
        }

        func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
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

    /// Errors surfaced by the client. Non-2xx responses map the backend's
    /// stable snake_case `"error"` reasons to precise cases.
    enum APIError: Error, Equatable {
        /// A non-2xx status with no more-specific mapping; `code` is the
        /// body's `"error"` field when present.
        case http(status: Int, code: String?)
        case decoding(String)
        case transport(String)
        /// 404 `monitor_not_found` (a plain task reached through a monitor
        /// route, or an archived monitor) / 404 `task_not_found` / 404
        /// `update_not_found` (feedback against a pruned update record).
        case notFound
        /// 409 `monitor_unscheduled` — pause/resume on a schedule-less monitor.
        case unscheduled
        /// 409 Phase 7 convert refusals: `monitor_already_exists` (the task
        /// already carries a spec, incl. archived former monitors) or
        /// `task_not_eligible_for_monitor` (Internal-lifecycle / archived).
        case conflict(reason: String)
        /// 400 with a stable `monitor_*` admission reason
        /// (`monitor_objective_required`, `monitor_sources_required`, …).
        case validation(reason: String)
        /// 400 `missing_scope`.
        case missingScope

        /// Short, user-presentable message (view models surface this).
        var userMessage: String {
            switch self {
            case .notFound:
                return "This monitor or update no longer exists."
            case .unscheduled:
                return "This monitor has no schedule to pause or resume."
            case .conflict(let reason):
                switch reason {
                case "monitor_already_exists":
                    return "This task is already a monitor."
                case "task_not_eligible_for_monitor":
                    return "This task can't be converted to a monitor."
                default:
                    return "Request conflicted: \(reason)."
                }
            case .validation(let reason):
                return MonitorForm.reasonLabel(reason)
            case .missingScope:
                return "No workspace scope was resolved for the request."
            case .decoding:
                return "The server response could not be read."
            case .transport:
                return "You appear to be offline. Check the connection and retry."
            case .http(let status, let code):
                if let code, !code.isEmpty { return "Request failed: \(code) (HTTP \(status))." }
                return "Request failed (HTTP \(status))."
            }
        }
    }

    // MARK: - Client

    final class APIClient {
        private let baseURL: URL
        private let scope: Scope
        private let transport: Transport
        private let encoder = JSONEncoder()
        private let decoder = JSONDecoder()

        /// The mount prefix (`bin/magician.rs` registers the monitor routes
        /// under `web::scope("/api/magician/v3")`, next to `/v3/tasks`).
        private let basePath = "/api/magician/v3"

        init(baseURL: URL, scope: Scope, transport: Transport) {
            self.baseURL = baseURL
            self.scope = scope
            self.transport = transport
        }

        // ── Endpoints ────────────────────────────────────────────────────────

        /// `GET /monitors?limit=&cursor=&state=`. `state` is
        /// `active` | `paused` | nil (all).
        func list(limit: Int = 50, cursor: String? = nil,
                  state: String? = nil) async throws -> ListPageV1 {
            var query = [URLQueryItem(name: "limit", value: String(limit))]
            if let cursor, !cursor.isEmpty {
                query.append(URLQueryItem(name: "cursor", value: cursor))
            }
            if let state, !state.isEmpty {
                query.append(URLQueryItem(name: "state", value: state))
            }
            let request = try makeRequest(method: "GET", path: "/monitors", query: query)
            return try await send(request, expecting: ListPageV1.self)
        }

        /// `GET /monitors/{task_id}` — 404 `.notFound` for plain tasks.
        func detail(_ taskID: String) async throws -> DetailV1 {
            let request = try makeRequest(
                method: "GET", path: "/monitors/\(encodePathSegment(taskID))")
            return try await send(request, expecting: DetailV1.self)
        }

        /// `POST /monitors` — 201 `{task_id, monitor_revision}`.
        func create(title: String? = nil, spec: SpecV1,
                    schedule: ScheduleWire? = nil) async throws -> MutationResponseV1 {
            let body = CreateRequestV1(title: title, spec: spec, schedule: schedule)
            let request = try makeRequest(method: "POST", path: "/monitors", body: body)
            return try await send(request, expecting: MutationResponseV1.self,
                                  successStatuses: [200, 201])
        }

        /// `POST /monitors/{task_id}/convert` — explicit Phase 7 conversion
        /// of an EXISTING eligible task into a monitor. The body is
        /// `{spec, title?}` ONLY: the task keeps its id, schedule, history,
        /// executions, and outputs (revision starts at 1). 404 `.notFound`
        /// for a missing task; 409 `.conflict` for already-a-monitor /
        /// not-eligible; 400 `.validation` for admission reasons.
        func convert(_ taskID: String, spec: SpecV1,
                     title: String? = nil) async throws -> ConvertResponseV1 {
            let body = ConvertRequestV1(spec: spec, title: title)
            let request = try makeRequest(
                method: "POST", path: "/monitors/\(encodePathSegment(taskID))/convert",
                body: body)
            return try await send(request, expecting: ConvertResponseV1.self)
        }

        /// `PATCH /monitors/{task_id}` — provided fields replace; a spec edit
        /// bumps the server-owned revision.
        func update(_ taskID: String, title: String? = nil, spec: SpecV1? = nil,
                    schedule: ScheduleWire? = nil) async throws -> MutationResponseV1 {
            let body = UpdateRequestV1(title: title, spec: spec, schedule: schedule)
            let request = try makeRequest(
                method: "PATCH", path: "/monitors/\(encodePathSegment(taskID))", body: body)
            return try await send(request, expecting: MutationResponseV1.self)
        }

        /// `DELETE /monitors/{task_id}` — soft archive by default (the server
        /// stamps the task `archived`); `removeFiles` physically deletes.
        @discardableResult
        func delete(_ taskID: String, removeFiles: Bool = false) async throws -> DeleteResponseV1 {
            let request = try makeRequest(
                method: "DELETE", path: "/monitors/\(encodePathSegment(taskID))",
                query: [URLQueryItem(name: "remove_files", value: removeFiles ? "true" : "false")])
            return try await send(request, expecting: DeleteResponseV1.self)
        }

        /// `POST /monitors/{task_id}/pause` — 409 `.unscheduled` without a schedule.
        @discardableResult
        func pause(_ taskID: String) async throws -> StateChangeResponseV1 {
            try await postStateChange(taskID, action: "pause")
        }

        /// `POST /monitors/{task_id}/resume`.
        @discardableResult
        func resume(_ taskID: String) async throws -> StateChangeResponseV1 {
            try await postStateChange(taskID, action: "resume")
        }

        private func postStateChange(
            _ taskID: String, action: String
        ) async throws -> StateChangeResponseV1 {
            let request = try makeRequest(
                method: "POST", path: "/monitors/\(encodePathSegment(taskID))/\(action)")
            return try await send(request, expecting: StateChangeResponseV1.self)
        }

        /// `POST /monitors/{task_id}/run` — run-now through the exact task
        /// execute path; 202 `{task, execution}` (payload not needed here).
        func runNow(_ taskID: String) async throws {
            let request = try makeRequest(
                method: "POST", path: "/monitors/\(encodePathSegment(taskID))/run")
            _ = try await sendRaw(request, successStatuses: [200, 202])
        }

        /// `GET /monitors/{task_id}/runs?limit=` — accepted, finalized
        /// `RunResultV1` records, newest first.
        func runs(_ taskID: String, limit: Int = 50) async throws -> ItemsPageV1<RunResultV1> {
            let request = try makeRequest(
                method: "GET", path: "/monitors/\(encodePathSegment(taskID))/runs",
                query: [URLQueryItem(name: "limit", value: String(limit))])
            return try await send(request, expecting: ItemsPageV1<RunResultV1>.self)
        }

        /// `GET /monitors/{task_id}/updates?limit=` — the durable update
        /// ledger (`UpdateDetailV1`), newest first.
        func updates(_ taskID: String, limit: Int = 50) async throws -> ItemsPageV1<UpdateDetailV1> {
            let request = try makeRequest(
                method: "GET", path: "/monitors/\(encodePathSegment(taskID))/updates",
                query: [URLQueryItem(name: "limit", value: String(limit))])
            return try await send(request, expecting: ItemsPageV1<UpdateDetailV1>.self)
        }

        /// `GET /monitor-updates?limit=` — scope-wide updates, newest first
        /// across every monitor in the scope.
        func scopeUpdates(limit: Int = 50) async throws -> ItemsPageV1<UpdateDetailV1> {
            let request = try makeRequest(
                method: "GET", path: "/monitor-updates",
                query: [URLQueryItem(name: "limit", value: String(limit))])
            return try await send(request, expecting: ItemsPageV1<UpdateDetailV1>.self)
        }

        /// `POST /monitors/{task_id}/updates/{update_id}/feedback` — record a
        /// useful/not-relevant verdict against one material update (plan
        /// §10). `recorded:false` = idempotent replay of the same verdict;
        /// the opposite verdict replaces the stored one. 404
        /// `monitor_not_found`/`update_not_found` → `.notFound`, 400
        /// `monitor_feedback_verdict_invalid` → `.validation`.
        @discardableResult
        func submitFeedback(
            _ taskID: String, updateID: String,
            verdict: FeedbackVerdict, note: String? = nil
        ) async throws -> FeedbackResponseV1 {
            let body = FeedbackRequestV1(verdict: verdict, note: note)
            let request = try makeRequest(
                method: "POST",
                path: "/monitors/\(encodePathSegment(taskID))"
                    + "/updates/\(encodePathSegment(updateID))/feedback",
                body: body)
            return try await send(request, expecting: FeedbackResponseV1.self)
        }

        /// `GET /monitors/{task_id}/feedback?limit=` — stored feedback
        /// records, newest first.
        func feedback(
            _ taskID: String, limit: Int = 50
        ) async throws -> ItemsPageV1<FeedbackRecordV1> {
            let request = try makeRequest(
                method: "GET", path: "/monitors/\(encodePathSegment(taskID))/feedback",
                query: [URLQueryItem(name: "limit", value: String(limit))])
            return try await send(request, expecting: ItemsPageV1<FeedbackRecordV1>.self)
        }

        // ── Request construction ─────────────────────────────────────────────

        private func makeRequest(
            method: String, path: String, query: [URLQueryItem] = []
        ) throws -> URLRequest {
            try makeRequest(method: method, path: path, query: query,
                            body: Optional<Never>.none)
        }

        private func makeRequest<Body: Encodable>(
            method: String, path: String, query: [URLQueryItem] = [], body: Body?
        ) throws -> URLRequest {
            guard var components = URLComponents(
                url: baseURL, resolvingAgainstBaseURL: false)
            else {
                throw APIError.transport("could not build URL for \(path)")
            }
            // `path` may contain an ALREADY-percent-encoded id segment
            // (`encodePathSegment`), so splice it via `percentEncodedPath`
            // to encode exactly once — `appendingPathComponent` re-encodes
            // the `%` signs (`task%20x` → `task%2520x`) and would break ids
            // containing a space or slash on the wire.
            let prefix = components.percentEncodedPath.hasSuffix("/")
                ? String(components.percentEncodedPath.dropLast())
                : components.percentEncodedPath
            components.percentEncodedPath = prefix + basePath + path
            if !query.isEmpty { components.queryItems = query }
            guard let url = components.url else {
                throw APIError.transport("could not resolve URL for \(path)")
            }
            var request = URLRequest(url: url)
            request.httpMethod = method
            if let body {
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                do {
                    request.httpBody = try encoder.encode(body)
                } catch {
                    throw APIError.decoding("encode \(Body.self): \(error)")
                }
            }
            return request
        }

        // ── Send + decode ────────────────────────────────────────────────────

        private func send<T: Decodable>(
            _ request: URLRequest, expecting: T.Type,
            successStatuses: Set<Int> = [200]
        ) async throws -> T {
            let data = try await sendRaw(request, successStatuses: successStatuses)
            do {
                return try decoder.decode(T.self, from: data)
            } catch {
                throw APIError.decoding("decode \(T.self): \(error)")
            }
        }

        private func sendRaw(
            _ request: URLRequest, successStatuses: Set<Int>
        ) async throws -> Data {
            let (data, http): (Data, HTTPURLResponse)
            do {
                (data, http) = try await transport.send(request)
            } catch let error as APIError {
                throw error
            } catch {
                throw APIError.transport(String(describing: error))
            }
            guard successStatuses.contains(http.statusCode) else {
                throw Self.mapError(status: http.statusCode, data: data)
            }
            return data
        }

        // ── Error mapping ────────────────────────────────────────────────────

        static func mapError(status: Int, data: Data) -> APIError {
            let code = errorCode(from: data)
            switch (status, code) {
            case (404, "monitor_not_found"), (404, "task_not_found"),
                 (404, "update_not_found"):
                return .notFound
            case (409, "monitor_unscheduled"):
                return .unscheduled
            case (409, "monitor_already_exists"), (409, "task_not_eligible_for_monitor"):
                return .conflict(reason: code ?? "")
            case (400, "missing_scope"):
                return .missingScope
            case (400, .some(let reason)) where reason.hasPrefix("monitor_"):
                return .validation(reason: reason)
            default:
                return .http(status: status, code: code)
            }
        }

        private static func errorCode(from data: Data) -> String? {
            guard
                let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { return nil }
            return object["error"] as? String
        }

        // ── Path helpers ─────────────────────────────────────────────────────

        private func encodePathSegment(_ segment: String) -> String {
            segment.addingPercentEncoding(withAllowedCharacters: Self.pathSegmentAllowed)
                ?? segment
        }

        private static let pathSegmentAllowed: CharacterSet = {
            var set = CharacterSet.alphanumerics
            set.insert(charactersIn: "-._~")
            return set
        }()
    }
}
