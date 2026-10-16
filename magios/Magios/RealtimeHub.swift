//
//  RealtimeHub.swift
//  Magios
//
//  One realtime connection per app process (plan:
//  docs/archive/plans/2026-10-01-single-event-hub.md). Every feature that wants live
//  events — chat, a chat turn's steps, Attention, Tasks, Today, task detail,
//  the Thinking Map, the Tutor overlay, the task Live Activity — subscribes
//  here instead of opening its own `/api/magician/v2/realtime/ws` socket.
//  The voice control socket (`RealtimeVoiceClient`) stays separate.
//
//  The hub owns ONE `URLSessionWebSocketTask` (with
//  `supports_structured_presentation=true`), reconnects forever with
//  jittered backoff capped at 30 s, retries at once when the app becomes
//  active or the network comes back, resumes with the last `epoch`/`seq`,
//  dedupes by `(epoch, seq)`, keeps the session alive with the app ping
//  `{"type":"ping"}`, and fans `hub.resync` (or a reconnect that could not
//  resume) out to every subscriber's `onResync`, where each one re-reads its
//  state over REST.
//
//  It also works against a server that predates the hub contract: frames
//  without `seq`/`epoch`/`refs`/`categories` are delivered as they are, refs
//  are pulled from the payload client-side, and a reconnect that brings no
//  `hub.hello` counts as a reconnect without resume (resync).
//
//  Threading: the hub is main-thread confined. Subscribe, cancel and every
//  callback happen on the main thread; the socket session delivers on
//  `OperationQueue.main`. Not an actor so the non-isolated view models
//  (chat, Attention, Tasks) can subscribe synchronously.
//

import Foundation
import Network
import UIKit

// MARK: - Wire model (pure, unit-tested)

/// The ids an event concerns. The server stamps them as top-level `refs`;
/// an absent field (or a legacy frame without `refs`) falls back to the
/// payload, so subscribers filter one way on either server.
struct RealtimeHubRefs: Equatable {
    var taskId: String?
    var executionId: String?
    var chatTurnId: String?
    var agentId: String?
    var sessionId: String?

    /// Server `refs` win field by field; the payload fills the rest.
    static func resolve(serverRefs: [String: Any]?, data: [String: Any]) -> RealtimeHubRefs {
        func pick(_ key: String) -> String? {
            nonEmpty(serverRefs?[key]) ?? extract(key, from: data)
        }
        return RealtimeHubRefs(
            taskId: pick("task_id"),
            executionId: pick("execution_id"),
            chatTurnId: pick("chat_turn_id"),
            agentId: pick("agent_id"),
            sessionId: pick("session_id")
        )
    }

    /// Envelope locations first (the places the variants name these ids),
    /// then a bounded breadth-first scan, which is how the server's chat-turn
    /// sink finds `chat_turn_id` in hand-built payloads.
    static func extract(_ key: String, from data: [String: Any]) -> String? {
        let event = data["event"] as? [String: Any]
        let state = data["state"] as? [String: Any]
        let envelopes: [[String: Any]?] = [
            data,
            event,
            event?["payload"] as? [String: Any],
            data["message"] as? [String: Any],
            data["payload"] as? [String: Any],
            state?["overview"] as? [String: Any],
        ]
        for envelope in envelopes {
            if let value = nonEmpty(envelope?[key]) { return value }
        }
        var frontier: [Any] = [data]
        var depth = 0
        while !frontier.isEmpty, depth < maxScanDepth {
            var next: [Any] = []
            for node in frontier {
                if let object = node as? [String: Any] {
                    if let value = nonEmpty(object[key]) { return value }
                    next.append(contentsOf: object.values)
                } else if let array = node as? [Any] {
                    next.append(contentsOf: array)
                }
            }
            frontier = next
            depth += 1
        }
        return nil
    }

    private static let maxScanDepth = 6

    fileprivate static func nonEmpty(_ value: Any?) -> String? {
        guard let text = value as? String else { return nil }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

/// One forwarded event frame: the raw frame plus the hub's view of it.
struct RealtimeHubEvent {
    /// The frame text exactly as received (UTF-8 of a binary frame), for
    /// subscribers whose classifiers read text.
    let text: String
    /// The whole decoded frame (`event_type`, `data`, and on a hub server
    /// `seq`, `epoch`, `categories`, `user_relevant`, `refs`).
    let object: [String: Any]
    /// The outer `event_type` (`AgentEvent` for agent events).
    let eventType: String
    let data: [String: Any]
    /// `AgentEvent` unwrapped: `data.event`. Nil for every other variant.
    let inner: [String: Any]?
    let refs: RealtimeHubRefs
    /// Server-classified `/v3/events` categories; nil on a legacy frame.
    let categories: [String]?
    /// Server `user_relevant`; nil on a legacy frame.
    let userRelevant: Bool?
    let seq: UInt64?
    let epoch: String?

    /// The inner `event.event_type` of an `AgentEvent`.
    var innerEventType: String? { inner?["event_type"] as? String }
    /// The type a subscriber reasons about: the inner type of an
    /// `AgentEvent`, else the outer one.
    var effectiveEventType: String { innerEventType ?? eventType }
    /// The inner `event.payload` of an `AgentEvent`, else `data`.
    var payload: [String: Any] { (inner?["payload"] as? [String: Any]) ?? data }

    /// Categories with the legacy fallback: the payload's own `categories`
    /// or `category`, else none.
    var resolvedCategories: [String] {
        if let categories { return categories }
        if let list = data["categories"] as? [String] { return list }
        if let one = RealtimeHubRefs.nonEmpty(data["category"]) { return [one] }
        return []
    }

    /// `user_relevant` with the legacy fallback to the payload.
    var resolvedUserRelevant: Bool? { userRelevant ?? (data["user_relevant"] as? Bool) }
}

/// What one socket frame means to the hub.
enum RealtimeHubFrame {
    case event(RealtimeHubEvent)
    /// First frame of every connection on a hub server.
    case hello(epoch: String, seq: UInt64, resumed: Bool)
    /// The client missed events; every subscriber re-reads its state.
    case resync(reason: String)
    /// Any other control frame (`{"type":"pong"}`, …): not delivered.
    case control(type: String)
    case invalid

    static func parse(_ message: URLSessionWebSocketTask.Message) -> RealtimeHubFrame {
        switch message {
        case .string(let text): return parse(text)
        case .data(let data): return parse(String(data: data, encoding: .utf8) ?? "")
        @unknown default: return .invalid
        }
    }

    static func parse(_ text: String) -> RealtimeHubFrame {
        guard let bytes = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any]
        else { return .invalid }

        if let eventType = object["event_type"] as? String, !eventType.isEmpty {
            let data = object["data"] as? [String: Any] ?? [:]
            let inner = eventType == "AgentEvent" ? data["event"] as? [String: Any] : nil
            return .event(RealtimeHubEvent(
                text: text,
                object: object,
                eventType: eventType,
                data: data,
                inner: inner,
                refs: RealtimeHubRefs.resolve(serverRefs: object["refs"] as? [String: Any], data: data),
                categories: object["categories"] as? [String],
                userRelevant: object["user_relevant"] as? Bool,
                seq: uint64(object["seq"]),
                epoch: RealtimeHubRefs.nonEmpty(object["epoch"])
            ))
        }

        guard let type = object["type"] as? String, !type.isEmpty else { return .invalid }
        switch type {
        case "hub.hello":
            guard let epoch = RealtimeHubRefs.nonEmpty(object["epoch"]) else { return .invalid }
            return .hello(
                epoch: epoch,
                seq: uint64(object["seq"]) ?? 0,
                resumed: object["resumed"] as? Bool ?? false
            )
        case "hub.resync":
            return .resync(reason: object["reason"] as? String ?? "")
        case "ping", "pong":
            return .control(type: type)
        default:
            // Other `type` frames (command acks, snapshots) were seen by every
            // per-feature socket before the hub; keep delivering them so the
            // match-all subscribers (Today's wire, chat) behave as they did.
            let data = object["data"] as? [String: Any] ?? object
            return .event(RealtimeHubEvent(
                text: text,
                object: object,
                eventType: type,
                data: data,
                inner: nil,
                refs: RealtimeHubRefs.resolve(serverRefs: nil, data: data),
                categories: nil,
                userRelevant: nil,
                seq: nil,
                epoch: nil
            ))
        }
    }

    static func uint64(_ value: Any?) -> UInt64? {
        if let number = value as? NSNumber {
            let double = number.doubleValue
            guard double >= 0, double.rounded() == double else { return nil }
            return number.uint64Value
        }
        if let text = value as? String { return UInt64(text) }
        return nil
    }
}

/// A subscription's declarative filter. Every set field must match; an
/// empty filter matches everything.
struct RealtimeHubFilter {
    /// Prefix match on the outer `event_type` and on an `AgentEvent`'s inner
    /// `event.event_type`. Empty = any type.
    var eventTypes: [String] = []
    /// Any-of on the event's categories. Empty = any.
    var categories: [String] = []
    var taskId: String?
    var executionId: String?
    var chatTurnId: String?
    var agentId: String?
    var sessionId: String?
    var userRelevant: Bool?

    static let all = RealtimeHubFilter()

    func matches(_ event: RealtimeHubEvent) -> Bool {
        if !eventTypes.isEmpty {
            let outer = event.eventType
            let inner = event.innerEventType
            let typed = eventTypes.contains { prefix in
                outer.hasPrefix(prefix) || (inner?.hasPrefix(prefix) ?? false)
            }
            guard typed else { return false }
        }
        if !categories.isEmpty {
            let own = event.resolvedCategories
            guard categories.contains(where: { own.contains($0) }) else { return false }
        }
        if let taskId, event.refs.taskId != taskId { return false }
        if let executionId, event.refs.executionId != executionId { return false }
        if let chatTurnId, event.refs.chatTurnId != chatTurnId { return false }
        if let agentId, event.refs.agentId != agentId { return false }
        if let sessionId, event.refs.sessionId != sessionId { return false }
        if let userRelevant, event.resolvedUserRelevant != userRelevant { return false }
        return true
    }
}

/// Where a reconnect resumes from.
struct RealtimeHubResumePoint: Equatable {
    let epoch: String
    let seq: UInt64
}

/// Drops a frame already delivered, by `(epoch, seq)`. A frame without both
/// (a legacy server) is always admitted.
struct RealtimeHubDeduper {
    private(set) var epoch: String?
    private(set) var highestSeq: UInt64?
    /// Everything at or below this seq is treated as already seen.
    private(set) var floor: UInt64?
    private var seen: Set<UInt64> = []
    private var order: [UInt64] = []
    let capacity: Int

    init(capacity: Int = 4096) {
        self.capacity = max(1, capacity)
    }

    var resumePoint: RealtimeHubResumePoint? {
        guard let epoch, let seq = highestSeq ?? floor else { return nil }
        return RealtimeHubResumePoint(epoch: epoch, seq: seq)
    }

    mutating func reset(epoch: String?, floor: UInt64?) {
        self.epoch = epoch
        self.floor = floor
        highestSeq = floor
        seen.removeAll(keepingCapacity: true)
        order.removeAll(keepingCapacity: true)
    }

    mutating func admit(epoch frameEpoch: String?, seq: UInt64?) -> Bool {
        guard let frameEpoch, let seq else { return true }
        if frameEpoch != epoch { reset(epoch: frameEpoch, floor: nil) }
        if let floor, seq <= floor { return false }
        guard seen.insert(seq).inserted else { return false }
        order.append(seq)
        highestSeq = max(highestSeq ?? seq, seq)
        if order.count > capacity {
            let evicted = order.removeFirst()
            seen.remove(evicted)
            floor = max(floor ?? evicted, evicted)
        }
        return true
    }
}

/// Per-connection bookkeeping that decides when subscribers must re-read
/// their state. Pure so the legacy (no `hub.hello`) path is testable.
struct RealtimeHubConnectionState {
    enum Action: Equatable { case none, resync }

    /// A connection has opened before in this hub's live period, so the
    /// current one is a reconnect and anything in between may be lost.
    private(set) var hasConnectedBefore = false
    private(set) var isReconnect = false
    /// Opened, and neither a `hub.hello` nor an event has arrived yet.
    private(set) var awaitingHello = false
    /// Between a `hub.hello` and the first event: a `hub.resync` here is the
    /// server's follow-up to a resume it could not satisfy.
    private var helloFollowUp = false
    private var resyncedThisConnection = false

    mutating func connectionOpened() {
        isReconnect = hasConnectedBefore
        hasConnectedBefore = true
        awaitingHello = true
        helloFollowUp = false
        resyncedThisConnection = false
    }

    mutating func receivedHello(resumed: Bool) -> Action {
        awaitingHello = false
        helloFollowUp = true
        guard isReconnect, !resumed else { return .none }
        resyncedThisConnection = true
        return .resync
    }

    mutating func receivedResyncFrame() -> Action {
        if helloFollowUp {
            helloFollowUp = false
            // A first connection has nothing to catch up on (subscribers just
            // loaded), and a reconnect already resynced on its hello.
            guard isReconnect, !resyncedThisConnection else { return .none }
            resyncedThisConnection = true
            return .resync
        }
        return .resync
    }

    /// A server that predates the hub sends events with no `hub.hello`.
    mutating func receivedEvent() -> Action {
        helloFollowUp = false
        guard awaitingHello else { return .none }
        return legacyConnectionConfirmed()
    }

    /// No `hub.hello` arrived in time after the socket opened.
    mutating func helloTimedOut() -> Action {
        guard awaitingHello else { return .none }
        return legacyConnectionConfirmed()
    }

    mutating func reset() {
        self = RealtimeHubConnectionState()
    }

    private mutating func legacyConnectionConfirmed() -> Action {
        awaitingHello = false
        guard isReconnect, !resyncedThisConnection else { return .none }
        resyncedThisConnection = true
        return .resync
    }
}

// MARK: - Subscription

/// Keeps a hub subscription alive. `cancel()` (or releasing the token)
/// stops delivery.
final class RealtimeHubSubscription {
    private weak var hub: RealtimeHub?
    private let id: UUID
    private var cancelled = false

    fileprivate init(hub: RealtimeHub, id: UUID) {
        self.hub = hub
        self.id = id
    }

    func cancel() {
        guard !cancelled else { return }
        cancelled = true
        let hub = self.hub
        let id = self.id
        if Thread.isMainThread {
            hub?.unsubscribe(id)
        } else {
            DispatchQueue.main.async { hub?.unsubscribe(id) }
        }
    }

    deinit { cancel() }
}

// MARK: - Hub

final class RealtimeHub: NSObject, URLSessionWebSocketDelegate {
    static let shared = RealtimeHub()

    static let socketPath = "/api/magician/v2/realtime/ws"
    static let appPingFrame = #"{"type":"ping"}"#
    static let maxBackoffSeconds: Double = 30
    /// How long an opened socket may stay silent before it is treated as a
    /// server without `hub.hello`.
    static let helloTimeoutSeconds: Double = 3
    static let keepaliveSeconds: TimeInterval = 25
    /// How long the socket outlives its last subscriber, so moving between
    /// screens does not churn the connection.
    static let idleGraceSeconds: Double = 10
    static let probeTimeoutSeconds: Double = 5

    private struct Subscriber {
        let id: UUID
        let matches: (RealtimeHubEvent) -> Bool
        let onEvent: (RealtimeHubEvent) -> Void
        let onResync: (() -> Void)?
    }

    private let connectsSockets: Bool
    private var subscribers: [Subscriber] = []
    private var deduper = RealtimeHubDeduper()
    private var connection = RealtimeHubConnectionState()

    private var urlSession: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var socketOpened = false
    private(set) var isConnected = false
    private var reconnectAttempt = 0
    private var reconnectWork: DispatchWorkItem?
    private var helloTimeout: DispatchWorkItem?
    private var idleWork: DispatchWorkItem?
    private var probeWork: DispatchWorkItem?
    private var keepaliveTimer: Timer?
    private var pathMonitor: NWPathMonitor?
    private var pathSatisfied = true
    private var observers: [NSObjectProtocol] = []

    /// `connectsSockets: false` builds an inert hub (unit tests): frames are
    /// fed through `ingest(_:)` and `connectionDidOpen()` by hand.
    init(connectsSockets: Bool = !isRunningUnderTests) {
        self.connectsSockets = connectsSockets
        super.init()
        if connectsSockets { observeLifecycle() }
    }

    deinit {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        pathMonitor?.cancel()
    }

    var subscriberCount: Int { subscribers.count }

    // MARK: Subscribe

    /// Deliver every event the filter matches. `onResync` runs when events
    /// may have been missed; re-read state there with one REST request.
    func subscribe(
        _ filter: RealtimeHubFilter = .all,
        onEvent: @escaping (RealtimeHubEvent) -> Void,
        onResync: (() -> Void)? = nil
    ) -> RealtimeHubSubscription {
        subscribe(where: { filter.matches($0) }, onEvent: onEvent, onResync: onResync)
    }

    /// Deliver every event the predicate accepts.
    func subscribe(
        where predicate: @escaping (RealtimeHubEvent) -> Bool,
        onEvent: @escaping (RealtimeHubEvent) -> Void,
        onResync: (() -> Void)? = nil
    ) -> RealtimeHubSubscription {
        assert(Thread.isMainThread, "RealtimeHub is main-thread confined")
        let id = UUID()
        subscribers.append(Subscriber(id: id, matches: predicate, onEvent: onEvent, onResync: onResync))
        idleWork?.cancel()
        idleWork = nil
        connectIfNeeded()
        return RealtimeHubSubscription(hub: self, id: id)
    }

    fileprivate func unsubscribe(_ id: UUID) {
        subscribers.removeAll { $0.id == id }
        guard subscribers.isEmpty, connectsSockets else { return }
        idleWork?.cancel()
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.subscribers.isEmpty else { return }
            self.disconnect()
        }
        idleWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.idleGraceSeconds, execute: work)
    }

    // MARK: Frames (internal so tests drive the hub without a socket)

    /// Process one socket frame: control frames update the connection,
    /// event frames are deduped and fanned out.
    func ingest(_ text: String) {
        handle(RealtimeHubFrame.parse(text))
    }

    /// The socket opened (the delegate callback, or a test).
    func connectionDidOpen() {
        socketOpened = true
        isConnected = true
        connection.connectionOpened()
        helloTimeout?.cancel()
        helloTimeout = nil
        guard connectsSockets else { return }
        let socketAtOpen = socket
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.socket === socketAtOpen else { return }
            self.perform(self.connection.helloTimedOut())
        }
        helloTimeout = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.helloTimeoutSeconds, execute: work)
        startKeepalive()
    }

    private func handle(_ frame: RealtimeHubFrame) {
        switch frame {
        case .hello(let epoch, let seq, let resumed):
            reconnectAttempt = 0
            helloTimeout?.cancel()
            helloTimeout = nil
            if !resumed || deduper.epoch != epoch {
                deduper.reset(epoch: epoch, floor: seq)
            }
            perform(connection.receivedHello(resumed: resumed))
        case .resync(let reason):
            debugLog("[realtime-hub] resync: \(reason)")
            perform(connection.receivedResyncFrame())
        case .event(let event):
            reconnectAttempt = 0
            perform(connection.receivedEvent())
            guard deduper.admit(epoch: event.epoch, seq: event.seq) else { return }
            deliver(event)
        case .control:
            reconnectAttempt = 0
        case .invalid:
            break
        }
    }

    private func perform(_ action: RealtimeHubConnectionState.Action) {
        guard action == .resync else { return }
        fanOutResync()
    }

    private func deliver(_ event: RealtimeHubEvent) {
        for subscriber in subscribers {
            // A callback may cancel a later subscriber; honour that at once.
            guard subscribers.contains(where: { $0.id == subscriber.id }) else { continue }
            if subscriber.matches(event) { subscriber.onEvent(event) }
        }
    }

    private func fanOutResync() {
        for subscriber in subscribers {
            guard subscribers.contains(where: { $0.id == subscriber.id }) else { continue }
            subscriber.onResync?()
        }
    }

    // MARK: Pure helpers

    /// The socket URL: structured presentation always, plus the resume
    /// point when there is one.
    static func socketURL(base: URL, resume: RealtimeHubResumePoint?) -> URL? {
        guard var components = URLComponents(
            url: base.appendingPathComponent(socketPath),
            resolvingAgainstBaseURL: false
        ) else { return nil }
        var items = [URLQueryItem(name: "supports_structured_presentation", value: "true")]
        if let resume {
            items.append(URLQueryItem(name: "resume_epoch", value: resume.epoch))
            items.append(URLQueryItem(name: "resume_seq", value: String(resume.seq)))
        }
        components.queryItems = items
        return components.url
    }

    /// Exponential backoff (1, 2, 4, 8, 16, 30 s…) with jitter in the upper
    /// half of each step. `jitter` is in 0...1.
    static func backoffDelay(attempt: Int, jitter: Double) -> Double {
        let exponent = Double(min(max(attempt, 0), 5))
        let base = min(maxBackoffSeconds, pow(2, exponent))
        let unit = min(max(jitter, 0), 1)
        return base * (0.5 + 0.5 * unit)
    }

    // MARK: Socket

    private func connectIfNeeded() {
        guard connectsSockets, socket == nil, !subscribers.isEmpty,
              MagicianAccess.isConfigured,
              let url = Self.socketURL(
                base: MagicianAccess.webSocketBaseURL,
                resume: deduper.resumePoint
              )
        else { return }
        reconnectWork?.cancel()
        reconnectWork = nil
        var request = URLRequest(url: url)
        // The paired bearer authorizes the upgrade and binds the event scope.
        MagicianAccess.authorize(&request)
        let session = URLSession(configuration: .default, delegate: self, delegateQueue: OperationQueue.main)
        let task = session.webSocketTask(with: request)
        // Execution-panel snapshots can exceed the 1 MiB default; an
        // oversized frame would fail the receive and loop the reconnect.
        task.maximumMessageSize = 16 * 1024 * 1024
        urlSession = session
        socket = task
        socketOpened = false
        task.resume()
        receive(on: task)
    }

    private func receive(on task: URLSessionWebSocketTask) {
        task.receive { [weak self, weak task] result in
            DispatchQueue.main.async {
                guard let self, let task, self.socket === task else { return }
                switch result {
                case .success(let message):
                    if !self.socketOpened { self.connectionDidOpen() }
                    self.handle(RealtimeHubFrame.parse(message))
                    if self.socket === task { self.receive(on: task) }
                case .failure(let error):
                    debugLog("[realtime-hub] receive error: \(error.localizedDescription)")
                    self.socketFailed(task)
                }
            }
        }
    }

    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didOpenWithProtocol protocol: String?
    ) {
        DispatchQueue.main.async {
            guard self.socket === webSocketTask, !self.socketOpened else { return }
            self.connectionDidOpen()
        }
    }

    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didCloseWith closeCode: URLSessionWebSocketTask.CloseCode,
        reason: Data?
    ) {
        DispatchQueue.main.async { self.socketFailed(webSocketTask) }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let webSocketTask = task as? URLSessionWebSocketTask else { return }
        DispatchQueue.main.async { self.socketFailed(webSocketTask) }
    }

    private func socketFailed(_ task: URLSessionWebSocketTask) {
        guard socket === task else { return }
        tearDownSocket()
        scheduleReconnect()
    }

    private func tearDownSocket() {
        let task = socket
        let session = urlSession
        socket = nil
        urlSession = nil
        socketOpened = false
        isConnected = false
        helloTimeout?.cancel(); helloTimeout = nil
        probeWork?.cancel(); probeWork = nil
        keepaliveTimer?.invalidate(); keepaliveTimer = nil
        task?.cancel(with: .goingAway, reason: nil)
        session?.invalidateAndCancel()
    }

    private func scheduleReconnect() {
        guard connectsSockets, !subscribers.isEmpty, reconnectWork == nil else { return }
        let delay = Self.backoffDelay(attempt: reconnectAttempt, jitter: Double.random(in: 0...1))
        reconnectAttempt += 1
        let work = DispatchWorkItem { [weak self] in
            self?.reconnectWork = nil
            self?.connectIfNeeded()
        }
        reconnectWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    /// The last subscriber left (after the grace): close and forget the
    /// resume point. The next subscribers load their own state, so replaying
    /// the gap to them would only duplicate it.
    private func disconnect() {
        reconnectWork?.cancel(); reconnectWork = nil
        tearDownSocket()
        deduper = RealtimeHubDeduper()
        connection.reset()
        reconnectAttempt = 0
    }

    /// Retry now: the app came to the foreground or the network came back.
    /// A pending backoff is skipped; an open socket is probed, since a
    /// suspended app's socket is often dead without having said so.
    private func retryNow() {
        guard connectsSockets, !subscribers.isEmpty else { return }
        if let task = socket {
            guard socketOpened else { return }   // still connecting
            probe(task)
            return
        }
        reconnectWork?.cancel(); reconnectWork = nil
        reconnectAttempt = 0
        connectIfNeeded()
    }

    private func probe(_ task: URLSessionWebSocketTask) {
        probeWork?.cancel()
        let work = DispatchWorkItem { [weak self, weak task] in
            guard let self, let task else { return }
            debugLog("[realtime-hub] probe timed out; reconnecting")
            self.reconnectImmediately(replacing: task)
        }
        probeWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.probeTimeoutSeconds, execute: work)
        task.sendPing { [weak self, weak task] error in
            DispatchQueue.main.async {
                guard let self, let task, self.socket === task else { return }
                self.probeWork?.cancel(); self.probeWork = nil
                if error != nil { self.reconnectImmediately(replacing: task) }
            }
        }
    }

    /// Drop a socket that failed its probe and dial again without backoff.
    private func reconnectImmediately(replacing task: URLSessionWebSocketTask) {
        guard socket === task else { return }
        tearDownSocket()
        reconnectWork?.cancel(); reconnectWork = nil
        reconnectAttempt = 0
        connectIfNeeded()
    }

    private func startKeepalive() {
        keepaliveTimer?.invalidate()
        let timer = Timer(timeInterval: Self.keepaliveSeconds, repeats: true) { [weak self] _ in
            guard let self, let task = self.socket, self.socketOpened else { return }
            task.send(.string(Self.appPingFrame)) { [weak self, weak task] error in
                guard error != nil else { return }
                DispatchQueue.main.async {
                    guard let self, let task else { return }
                    self.socketFailed(task)
                }
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        keepaliveTimer = timer
    }

    /// A different backend (or none): drop the socket and the resume point,
    /// reconnect to the new origin, and have every subscriber re-read.
    private func connectionProfileChanged() {
        reconnectWork?.cancel(); reconnectWork = nil
        tearDownSocket()
        deduper = RealtimeHubDeduper()
        connection.reset()
        reconnectAttempt = 0
        guard !subscribers.isEmpty else { return }
        connectIfNeeded()
        fanOutResync()
    }

    private func observeLifecycle() {
        let center = NotificationCenter.default
        observers.append(center.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.retryNow() })
        observers.append(center.addObserver(
            forName: .magicianMobileConnectionDidChange, object: nil, queue: .main
        ) { [weak self] _ in self?.connectionProfileChanged() })

        let monitor = NWPathMonitor()
        monitor.pathUpdateHandler = { [weak self] path in
            DispatchQueue.main.async {
                guard let self else { return }
                let satisfied = path.status == .satisfied
                let cameBack = satisfied && !self.pathSatisfied
                self.pathSatisfied = satisfied
                if cameBack { self.retryNow() }
            }
        }
        monitor.start(queue: DispatchQueue.main)
        pathMonitor = monitor
    }
}
