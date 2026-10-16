import Foundation

// MARK: - Chunk accumulator

/// Accumulates raw PCM16 (16 kHz mono little-endian) bytes and emits
/// fixed-duration chunks. Pure and synchronous: the audio tap feeds it, the
/// upload loop drains completed chunks. Matches the backend's expected chunk
/// cadence (the server's own streaming STT windows 4–8 s of PCM).
struct ObservationChunkAccumulator {
    /// 16 kHz * 2 bytes/sample (16-bit) mono = 32000 bytes/sec.
    static let bytesPerSecond = 16_000 * 2
    let chunkBytes: Int
    private var buffer = Data()

    init(chunkSeconds: Int = 6) {
        self.chunkBytes = ObservationChunkAccumulator.bytesPerSecond * max(1, chunkSeconds)
    }

    /// Append PCM and return any completed full-size chunks (usually 0 or 1).
    mutating func append(_ pcm: Data) -> [Data] {
        buffer.append(pcm)
        var out: [Data] = []
        while buffer.count >= chunkBytes {
            out.append(buffer.prefix(chunkBytes))
            buffer.removeFirst(chunkBytes)
        }
        return out
    }

    /// Emit whatever remains (call on stop/pause) and empty the buffer.
    mutating func flush() -> Data? {
        guard !buffer.isEmpty else { return nil }
        let rest = buffer
        buffer.removeAll(keepingCapacity: true)
        return rest
    }

    var pendingBytes: Int { buffer.count }
}

// MARK: - Session + errors

/// A live `capture: "client"` listen session.
struct ObservationSession: Equatable {
    let sessionId: String
    /// Bearer echoed on every audio chunk POST.
    let uploadToken: String
    let threadId: String
    /// True when an already-live client session on this thread was returned.
    let reused: Bool
}

enum ObservationUplinkError: Error, Equatable {
    case offline
    case timedOut
    case cancelled
    case unauthorized
    /// Server said the session is gone (HTTP 410) — terminal; stop capturing.
    case sessionEnded
    /// Another source (e.g. the Mac) already observes this meeting (HTTP 409).
    case alreadyObserved(existingSessionId: String)
    case http(Int)
    case decoding
}

// MARK: - Client

/// Client for the observation ingest contract. `session` is injectable so tests
/// drive it through `MockURLProtocol`; nothing here reads global mutable state.
struct ObservationUplinkClient {
    var session: URLSession = .shared
    var baseURL: URL = MagicianAccess.baseURL
    var timeout: TimeInterval = 20

    /// `POST /meetings/listen {capture:"client"}` → session id + upload token.
    func startSession(title: String?, url: String?, mic: Bool) async throws -> ObservationSession {
        var req = authorized(path: "/api/magician/v2/meetings/listen", method: "POST")
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        var body: [String: Any] = ["capture": "client", "mic": mic]
        if let title = title?.trimmingCharacters(in: .whitespacesAndNewlines), !title.isEmpty {
            body["title"] = title
        }
        if let url = url?.trimmingCharacters(in: .whitespacesAndNewlines), !url.isEmpty {
            body["url"] = url
        }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, resp) = try await perform(req)
        try Self.check(resp, body: data)
        guard
            let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let sid = obj["session_id"] as? String,
            let tok = obj["upload_token"] as? String,
            let thread = obj["thread_id"] as? String
        else { throw ObservationUplinkError.decoding }
        return ObservationSession(
            sessionId: sid,
            uploadToken: tok,
            threadId: thread,
            reused: (obj["reused"] as? Bool) ?? false
        )
    }

    /// `POST /meetings/{id}/audio?channel&seq` — one PCM chunk. Throws
    /// `.sessionEnded` on 410 (the terminal signal).
    func uploadChunk(
        sessionId: String,
        token: String,
        channel: String,
        seq: UInt64,
        pcm: Data
    ) async throws {
        var comps = URLComponents(
            url: baseURL.appendingPathComponent("/api/magician/v2/meetings/\(sessionId)/audio"),
            resolvingAgainstBaseURL: false
        )!
        comps.queryItems = [
            URLQueryItem(name: "channel", value: channel),
            URLQueryItem(name: "seq", value: String(seq)),
        ]
        var req = URLRequest(url: comps.url!, timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("audio/pcm", forHTTPHeaderField: "Content-Type")
        req.setValue(token, forHTTPHeaderField: "X-Upload-Token")
        MagicianAccess.authorize(&req)
        req.httpBody = pcm
        let (data, resp) = try await perform(req)
        try Self.check(resp, body: data)
    }

    /// `POST /screen/observe/frame?observe_id=…` — one client-pushed screen
    /// keyframe (JPEG). Throws `.sessionEnded` on 410 (the observation ended).
    func uploadFrame(observeId: String, token: String, jpeg: Data) async throws {
        var comps = URLComponents(
            url: baseURL.appendingPathComponent("/api/magician/v2/screen/observe/frame"),
            resolvingAgainstBaseURL: false
        )!
        comps.queryItems = [
            URLQueryItem(name: "observe_id", value: observeId),
        ]
        var req = URLRequest(url: comps.url!, timeoutInterval: timeout)
        req.httpMethod = "POST"
        req.setValue("image/jpeg", forHTTPHeaderField: "Content-Type")
        req.setValue(token, forHTTPHeaderField: "X-Upload-Token")
        MagicianAccess.authorize(&req)
        req.httpBody = jpeg
        let (data, resp) = try await perform(req)
        try Self.check(resp, body: data)
    }

    /// `POST /meetings/{id}/stop` — best effort; ignores the response.
    func stopSession(sessionId: String) async {
        let req = authorized(path: "/api/magician/v2/meetings/\(sessionId)/stop", method: "POST")
        _ = try? await perform(req)
    }

    // MARK: internals

    private func authorized(path: String, method: String) -> URLRequest {
        var req = URLRequest(url: baseURL.appendingPathComponent(path), timeoutInterval: timeout)
        req.httpMethod = method
        MagicianAccess.authorize(&req)
        return req
    }

    private func perform(_ req: URLRequest) async throws -> (Data, URLResponse) {
        do {
            return try await session.data(for: req)
        } catch let e as URLError {
            switch e.code {
            case .notConnectedToInternet, .networkConnectionLost, .cannotConnectToHost:
                throw ObservationUplinkError.offline
            case .timedOut: throw ObservationUplinkError.timedOut
            case .cancelled: throw ObservationUplinkError.cancelled
            default: throw ObservationUplinkError.http(e.errorCode)
            }
        }
    }

    private static func check(_ resp: URLResponse, body: Data) throws {
        guard let http = resp as? HTTPURLResponse else { return }
        switch http.statusCode {
        case 200..<300:
            return
        case 410:
            throw ObservationUplinkError.sessionEnded
        case 409:
            let existing = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?
                .flatMap { $0["existing_session_id"] as? String } ?? ""
            throw ObservationUplinkError.alreadyObserved(existingSessionId: existing)
        case 401, 403:
            throw ObservationUplinkError.unauthorized
        default:
            throw ObservationUplinkError.http(http.statusCode)
        }
    }
}

// MARK: - Upload pump

/// Serializes chunk uploads with **newest-wins** backpressure: at most one upload
/// is in flight, and a chunk submitted while busy replaces any older pending
/// chunk (real-time audio must never queue stale). Stops permanently on the
/// server's `410` (session ended) and notifies via `onEnded`. Pure and testable
/// through an injected `ObservationUplinkClient` (mock `URLSession`).
actor ObservationUploadPump {
    private let client: ObservationUplinkClient
    private let sessionId: String
    private let token: String
    private let channel: String
    private var seq: UInt64 = 0
    private var pending: Data?
    private var draining = false
    private var endedFlag = false
    private var droppedCount: UInt64 = 0
    private var onEnded: (@Sendable () -> Void)?
    private var drainTask: Task<Void, Never>?

    init(
        client: ObservationUplinkClient,
        sessionId: String,
        token: String,
        channel: String = "primary"
    ) {
        self.client = client
        self.sessionId = sessionId
        self.token = token
        self.channel = channel
    }

    func setOnEnded(_ cb: @escaping @Sendable () -> Void) { onEnded = cb }

    /// Submit a completed chunk. If an upload is in flight, this becomes the new
    /// pending chunk and any prior pending is dropped (counted).
    func submit(_ chunk: Data) {
        guard !endedFlag else { return }
        if pending != nil { droppedCount += 1 }
        pending = chunk
        guard !draining else { return }
        draining = true
        drainTask = Task { await self.drain() }
    }

    /// Stop accepting audio, discard queued audio, and cancel the current HTTP
    /// upload. Local capture teardown must not wait for a slow server request.
    func stop() {
        endedFlag = true
        pending = nil
        drainTask?.cancel()
        drainTask = nil
        draining = false
    }

    /// Submit the final partial chunk and give the current upload a short,
    /// bounded chance to land before cancellation. The microphone is already
    /// stopped while this runs, so this preserves closing speech without
    /// allowing network latency to keep capture alive.
    func finish(finalChunk: Data?, grace: Duration = .milliseconds(1_500)) async {
        if let finalChunk, !finalChunk.isEmpty {
            submit(finalChunk)
        }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: grace)
        while draining, !endedFlag, clock.now < deadline {
            try? await Task.sleep(for: .milliseconds(25))
        }
        stop()
    }

    private func drain() async {
        while !Task.isCancelled, let chunk = pending {
            pending = nil
            let s = seq
            seq += 1
            do {
                try await client.uploadChunk(
                    sessionId: sessionId,
                    token: token,
                    channel: channel,
                    seq: s,
                    pcm: chunk
                )
            } catch ObservationUplinkError.sessionEnded {
                endedFlag = true
                draining = false
                drainTask = nil
                onEnded?()
                return
            } catch {
                // Transient (offline / timeout / 5xx): drop this chunk, keep going.
            }
        }
        draining = false
        drainTask = nil
    }

    /// Test/observability introspection.
    func snapshot() -> (nextSeq: UInt64, ended: Bool, dropped: UInt64) {
        (seq, endedFlag, droppedCount)
    }
}

// MARK: - Frame pusher

/// Newest-wins pusher for client screen keyframes (JPEG) — at most one upload in
/// flight; a frame submitted while busy replaces any older pending frame. Stops
/// permanently once the observation ends (410). Screen keyframes are already
/// throttled at the source, so this mainly guards against overlap under a slow
/// network.
actor ObservationFramePusher {
    private let client: ObservationUplinkClient
    private let observeId: String
    private let token: String
    private var pending: Data?
    private var draining = false
    private var stopped = false

    init(client: ObservationUplinkClient, observeId: String, token: String) {
        self.client = client
        self.observeId = observeId
        self.token = token
    }

    var isStopped: Bool { stopped }

    func push(_ jpeg: Data) {
        guard !stopped else { return }
        pending = jpeg
        guard !draining else { return }
        draining = true
        Task { await self.drain() }
    }

    private func drain() async {
        while let jpeg = pending {
            pending = nil
            do {
                try await client.uploadFrame(observeId: observeId, token: token, jpeg: jpeg)
            } catch ObservationUplinkError.sessionEnded {
                stopped = true
                draining = false
                return
            } catch {
                // Transient (offline / timeout / 5xx): drop this frame, keep going.
            }
        }
        draining = false
    }
}

// MARK: - Durable arm handoff (App Group)

/// The last-armed / active client session, persisted in the App Group so a
/// resume-after-interruption flow (or a future broadcast extension) can find it
/// even if the app was backgrounded. The in-app listener holds the live session
/// in memory; this is the durable backstop the app claims on foreground.
struct ObservationArm: Codable, Equatable {
    let sessionId: String
    let uploadToken: String
    let threadId: String
    let micEnabled: Bool
    /// Paired client screen-observation (set only for the broadcast flow) — the
    /// broadcast extension pushes `.video` keyframes to it.
    let observeId: String?
    let frameToken: String?

    init(
        sessionId: String,
        uploadToken: String,
        threadId: String,
        micEnabled: Bool,
        observeId: String? = nil,
        frameToken: String? = nil
    ) {
        self.sessionId = sessionId
        self.uploadToken = uploadToken
        self.threadId = threadId
        self.micEnabled = micEnabled
        self.observeId = observeId
        self.frameToken = frameToken
    }

    private static let key = "observation.activeArm"
    private static var store: UserDefaults {
        UserDefaults(suiteName: MagicianAccess.appGroup) ?? .standard
    }

    func save() {
        if let d = try? JSONEncoder().encode(self) {
            Self.store.set(d, forKey: Self.key)
        }
    }

    static func claim() -> ObservationArm? {
        guard
            let d = store.data(forKey: key),
            let a = try? JSONDecoder().decode(ObservationArm.self, from: d)
        else { return nil }
        return a
    }

    static func clear() {
        store.removeObject(forKey: key)
    }
}
