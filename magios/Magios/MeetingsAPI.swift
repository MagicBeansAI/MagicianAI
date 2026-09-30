import Foundation

// MARK: - Models

/// A currently-live capture session (either rail) from `GET /meetings/active`.
struct ActiveMeeting: Identifiable, Equatable {
    let sessionId: String
    /// "passive" (a listener — host or client mic) or "attendee" (the agent bot).
    let mode: String
    let status: String
    let threadId: String?
    let title: String?
    let url: String?
    let mic: Bool?
    let paused: Bool
    let latestSummary: String?

    var id: String { sessionId }
    var isBot: Bool { mode == "attendee" }

    init?(json: [String: Any]) {
        guard let sid = json["session_id"] as? String else { return nil }
        sessionId = sid
        mode = json["mode"] as? String ?? "passive"
        status = json["status"] as? String ?? ""
        threadId = json["thread_id"] as? String
        title = json["title"] as? String
        url = json["url"] as? String
        mic = json["mic"] as? Bool
        paused = json["paused"] as? Bool ?? false
        latestSummary = json["latest_summary"] as? String
    }
}

/// An upcoming calendar meeting from `GET /meetings/upcoming`.
struct UpcomingMeeting: Identifiable, Equatable {
    let eventId: String?
    let title: String
    let start: Date?
    let end: Date?
    let meetUrl: String?
    let liveNow: Bool
    let account: String?

    var id: String { eventId ?? "\(title)|\(start?.timeIntervalSince1970 ?? 0)" }
    var isJoinable: Bool { (meetUrl?.isEmpty == false) }

    init?(json: [String: Any]) {
        guard let title = json["title"] as? String else { return nil }
        self.title = title
        eventId = json["event_id"] as? String
        start = MeetingsClient.parseDate(json["start"] as? String)
        end = MeetingsClient.parseDate(json["end"] as? String)
        let mu = json["meet_url"] as? String
        meetUrl = (mu?.isEmpty == false) ? mu : nil
        liveNow = json["live_now"] as? Bool ?? false
        account = json["account"] as? String
    }
}

/// One line of a meeting's live transcript ("{speaker}: {text}", split apart).
struct TranscriptLine: Identifiable, Equatable {
    let id: String
    let speaker: String?
    let text: String
    var isYou: Bool { (speaker ?? "").caseInsensitiveCompare("you") == .orderedSame }
}

// MARK: - Client

/// Read client for the meetings surface (active sessions + the owner's upcoming
/// calendar meetings). `session` is injectable so tests drive it via
/// `MockURLProtocol`.
struct MeetingsClient {
    var session: URLSession = .shared
    var baseURL: URL = MagicianAccess.baseURL
    var timeout: TimeInterval = 15

    func fetchActive() async throws -> [ActiveMeeting] {
        let obj = try await getJSON(path: "/api/magician/v2/meetings/active", query: [])
        let arr = obj["active"] as? [[String: Any]] ?? []
        return arr.compactMap(ActiveMeeting.init(json:))
    }

    /// Returns the events plus any per-account fetch errors (an expired token on
    /// one calendar must not blank the whole section).
    func fetchUpcoming(refresh: Bool = false) async throws -> (events: [UpcomingMeeting], errors: [String]) {
        var q = [URLQueryItem]()
        if refresh { q.append(URLQueryItem(name: "refresh", value: "true")) }
        let obj = try await getJSON(path: "/api/magician/v2/meetings/upcoming", query: q)
        let events = (obj["events"] as? [[String: Any]] ?? []).compactMap(UpcomingMeeting.init(json:))
        let errors = (obj["errors"] as? [[String: Any]] ?? []).compactMap { $0["error"] as? String }
        return (events, errors)
    }

    /// `POST /meetings/{id}/stop` — stop a live session (either rail). Best
    /// effort; the caller refreshes the active list afterward.
    func stop(sessionId: String) async {
        var req = URLRequest(
            url: baseURL.appendingPathComponent("/api/magician/v2/meetings/\(sessionId)/stop"),
            timeoutInterval: timeout
        )
        req.httpMethod = "POST"
        authorize(&req)
        _ = try? await session.data(for: req)
    }

    /// `POST /meetings/join` — send the agent bot into the call. Returns the
    /// meeting thread id.
    func joinAsBot(url: String, title: String?) async throws -> String? {
        var req = URLRequest(url: baseURL.appendingPathComponent("/api/magician/v2/meetings/join"), timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        authorize(&req)
        var body: [String: Any] = ["url": url]
        if let title, !title.isEmpty { body["title"] = title }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, resp) = try await session.data(for: req)
        try Self.throwIfNotOK(resp)
        let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return obj?["thread_id"] as? String
    }

    /// `POST /screen/observe/client/start` — start a client-pushed screen
    /// observation narrating into `thread`. Returns (observe_id, frame upload
    /// token) for the broadcast extension to push keyframes with.
    func startClientObserve(thread: String, title: String?) async throws -> (observeId: String, frameToken: String) {
        var req = URLRequest(
            url: baseURL.appendingPathComponent("/api/magician/v2/screen/observe/client/start"),
            timeoutInterval: timeout
        )
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        authorize(&req)
        var payload: [String: Any] = ["thread": thread]
        if let title, !title.isEmpty { payload["title"] = title }
        req.httpBody = try JSONSerialization.data(withJSONObject: payload)
        let (data, resp) = try await session.data(for: req)
        try Self.throwIfNotOK(resp)
        guard
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let oid = obj["observe_id"] as? String,
            let tok = obj["upload_token"] as? String
        else { throw URLError(.cannotParseResponse) }
        return (oid, tok)
    }

    /// `GET /meetings` → `recent` (latest session per meeting thread) as deck rows.
    func fetchRecent() async throws -> [RecentCapture] {
        RecentCapture.meetings(from: try await getJSON(path: "/api/magician/v2/meetings", query: []))
    }

    /// `GET /chat/sessions?ui_thread_id=screen-watch` — screen-watch journals,
    /// merged into the Recent list like the web console does.
    func fetchWatchSessions() async throws -> [RecentCapture] {
        RecentCapture.watchSessions(from: try await getJSON(
            path: "/api/magician/v2/chat/sessions",
            query: [URLQueryItem(name: "ui_thread_id", value: "screen-watch")]
        ))
    }

    /// Resolve a meeting thread to its latest chat session id (the one the
    /// transcript lines land in) via `GET /meetings` → `recent`.
    func chatSessionId(forThread thread: String) async throws -> String? {
        let obj = try await getJSON(path: "/api/magician/v2/meetings", query: [])
        let recent = obj["recent"] as? [[String: Any]] ?? []
        return recent.first { ($0["thread_id"] as? String) == thread }?["session_id"] as? String
    }

    /// Fetch the meeting transcript lines for a chat session: the `System`
    /// messages tagged `source_surface == "meeting-transcript"`, whose text is
    /// "{speaker}: {line}".
    func transcript(sessionId: String) async throws -> [TranscriptLine] {
        let obj = try await getJSON(
            path: "/api/magician/v2/chat/sessions/\(sessionId)/messages",
            query: [URLQueryItem(name: "limit", value: "200")]
        )
        let msgs = obj["messages"] as? [[String: Any]] ?? []
        return msgs.compactMap { m -> TranscriptLine? in
            guard (m["source_surface"] as? String) == "meeting-transcript" else { return nil }
            let body = Self.messageText(m)
            guard !body.isEmpty else { return nil }
            let id = (m["id"] as? String) ?? body
            // Split a leading "{speaker}: " prefix (guarded so we don't split on a
            // colon deep in the sentence).
            if let r = body.range(of: ": "),
               body.distance(from: body.startIndex, to: r.lowerBound) <= 40 {
                return TranscriptLine(
                    id: id,
                    speaker: String(body[body.startIndex..<r.lowerBound]),
                    text: String(body[r.upperBound...])
                )
            }
            return TranscriptLine(id: id, speaker: nil, text: body)
        }
    }

    private static func messageText(_ m: [String: Any]) -> String {
        if let content = m["content"] as? [String: Any], let t = content["text"] as? String {
            return t
        }
        return (m["text"] as? String) ?? ""
    }

    // MARK: internals

    private func getJSON(path: String, query: [URLQueryItem]) async throws -> [String: Any] {
        var comps = URLComponents(url: baseURL.appendingPathComponent(path), resolvingAgainstBaseURL: false)!
        comps.queryItems = query
        var req = URLRequest(url: comps.url!, timeoutInterval: timeout)
        req.httpMethod = "GET"
        authorize(&req)
        let (data, resp) = try await session.data(for: req)
        try Self.throwIfNotOK(resp)
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw URLError(.cannotParseResponse)
        }
        return obj
    }

    private func authorize(_ req: inout URLRequest) {
        MagicianAccess.authorize(&req)
    }

    private static func throwIfNotOK(_ resp: URLResponse) throws {
        if let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw URLError(.badServerResponse)
        }
    }

    /// RFC3339 with a timezone offset (or `Z`), tolerating fractional seconds.
    static func parseDate(_ s: String?) -> Date? {
        guard let s, !s.isEmpty else { return nil }
        let withFrac = ISO8601DateFormatter()
        withFrac.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = withFrac.date(from: s) { return d }
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        return plain.date(from: s)
    }
}

// MARK: - View model

/// Backs the Observe surface's "Now" and "Upcoming" sections. Loads on appear,
/// light-polls while visible, and refreshes on demand after actions.
@MainActor
final class MeetingsViewModel: ObservableObject {
    @Published private(set) var active: [ActiveMeeting] = []
    @Published private(set) var upcoming: [UpcomingMeeting] = []
    @Published private(set) var upcomingErrors: [String] = []
    @Published private(set) var loading = false
    /// True once an upcoming fetch has completed (so "no meetings" is only shown
    /// for a real empty answer, never while loading).
    @Published private(set) var upcomingLoaded = false
    /// Whole-request failures (per-account calendar errors stay in `upcomingErrors`).
    @Published private(set) var upcomingError: String?
    @Published private(set) var activeError: String?
    /// The session a "Prepare session" broadcast armed. Cleared as soon as a
    /// successful active fetch no longer lists it (broadcast ended / stopped /
    /// idle-timed-out), so the card returns to "Prepare session".
    @Published private(set) var armedBroadcastSessionId: String?

    private let client: MeetingsClient
    private var pollTask: Task<Void, Never>?

    init(client: MeetingsClient = MeetingsClient()) { self.client = client }

    /// Begin light polling of the active list. The caller does the initial
    /// `await refresh()` itself so ordering (e.g. reattach after load) is
    /// deterministic. Active sessions move quickly (status/summary); upcoming is
    /// cached server-side, so 15s is responsive without hammering.
    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 15_000_000_000)
                if Task.isCancelled { break }
                await self?.refreshActive()
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
    }

    /// Stop a live server session (a bot attendee or another listener) and
    /// refresh the active list.
    func stopServer(_ sessionId: String) async {
        await client.stop(sessionId: sessionId)
        await refreshActive()
    }

    /// Send the agent bot to join a call; returns the meeting thread id and
    /// refreshes the active list.
    func sendBot(url: String, title: String?) async -> String? {
        let thread = (try? await client.joinAsBot(url: url, title: title)) ?? nil
        await refreshActive()
        return thread
    }

    /// Pre-create a `capture:"client"` session (with the mic/"You" track enabled)
    /// and save the App Group `ObservationArm` so the broadcast upload extension
    /// can claim it. Returns whether the session was created.
    func armBroadcast(title: String?) async -> Bool {
        let uplink = ObservationUplinkClient()
        do {
            let s = try await uplink.startSession(title: title, url: nil, mic: true)
            // Also start a paired screen observation narrating into the SAME
            // meeting thread, so the broadcast's screen keyframes land there too.
            // Best-effort: if a host observation already holds the slot, we still
            // arm audio (frames simply won't push).
            let observe = try? await client.startClientObserve(thread: s.threadId, title: title)
            ObservationArm(
                sessionId: s.sessionId,
                uploadToken: s.uploadToken,
                threadId: s.threadId,
                micEnabled: true,
                observeId: observe?.observeId,
                frameToken: observe?.frameToken
            ).save()
            armedBroadcastSessionId = s.sessionId
            await refreshActive()
            return true
        } catch {
            return false
        }
    }

    /// Keep showing a broadcast session armed before the view (re)appeared.
    func adoptArmedBroadcast(sessionId: String) {
        armedBroadcastSessionId = sessionId
    }

    /// Load active + upcoming. `force` (the Refresh control) bypasses the
    /// server's calendar cache with `refresh=true`.
    func refresh(force: Bool = false) async {
        loading = true
        defer { loading = false }
        await refreshActive()
        do {
            let up = try await client.fetchUpcoming(refresh: force)
            upcoming = up.events
            upcomingErrors = up.errors
            upcomingError = nil
        } catch {
            upcomingError = "Couldn't load your calendar. \(Self.describe(error))"
        }
        upcomingLoaded = true
    }

    func refreshActive() async {
        do {
            let a = try await client.fetchActive()
            active = a
            activeError = nil
            if ObserveCaptureRules.broadcastArmExpired(
                armedSessionId: armedBroadcastSessionId,
                activeSessionIds: a.map(\.sessionId)
            ) {
                if ObservationArm.claim()?.sessionId == armedBroadcastSessionId { ObservationArm.clear() }
                armedBroadcastSessionId = nil
            }
        } catch {
            activeError = "Couldn't load live captures. \(Self.describe(error))"
        }
    }

    static func describe(_ error: Error) -> String {
        if let e = error as? URLError {
            switch e.code {
            case .notConnectedToInternet, .networkConnectionLost, .cannotConnectToHost, .cannotFindHost, .dnsLookupFailed, .timedOut:
                return "Check your connection."
            case .badServerResponse: return "The server returned an error."
            default: break
            }
        }
        return "Try again."
    }
}

// MARK: - Live transcript

/// Backs the inline live-transcript panel in the Observe cockpit. Resolves the
/// meeting thread → its chat session, then polls the transcript lines every few
/// seconds while a capture is live.
@MainActor
final class MeetingTranscriptViewModel: ObservableObject {
    @Published private(set) var lines: [TranscriptLine] = []

    private let client: MeetingsClient
    private var pollTask: Task<Void, Never>?
    private var thread: String?
    private var sessionId: String?

    init(client: MeetingsClient = MeetingsClient()) { self.client = client }

    /// Begin (or continue) streaming the transcript for `thread`. Idempotent for
    /// the same thread.
    func start(thread: String) {
        guard self.thread != thread else { return }
        stop()
        self.thread = thread
        lines = []
        pollTask = Task { [weak self] in await self?.poll(thread: thread) }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
        thread = nil
        sessionId = nil
    }

    private func poll(thread: String) async {
        while !Task.isCancelled {
            if sessionId == nil {
                sessionId = (try? await client.chatSessionId(forThread: thread)) ?? nil
            }
            if let sid = sessionId, let fetched = try? await client.transcript(sessionId: sid) {
                lines = fetched
            }
            try? await Task.sleep(nanoseconds: 4_000_000_000)
        }
    }
}
