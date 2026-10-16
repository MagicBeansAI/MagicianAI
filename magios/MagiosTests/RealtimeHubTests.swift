import XCTest
@testable import Magician

/// The realtime hub's pure parts: frame parsing (hub and legacy frames),
/// AgentEvent unwrap, filter matching, `(epoch, seq)` dedupe, the resume
/// query, backoff, and when subscribers are told to resync — including a
/// server that never sends `hub.hello`.
final class RealtimeHubTests: XCTestCase {

    // MARK: - Helpers

    private func json(_ object: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }

    private func event(_ text: String, file: StaticString = #filePath, line: UInt = #line) -> RealtimeHubEvent? {
        guard case .event(let event) = RealtimeHubFrame.parse(text) else {
            XCTFail("expected an event frame", file: file, line: line)
            return nil
        }
        return event
    }

    private func frame(
        type: String = "TaskUpdated",
        data: [String: Any] = [:],
        seq: UInt64? = nil,
        epoch: String? = nil
    ) -> String {
        var object: [String: Any] = ["event_type": type, "data": data]
        if let seq { object["seq"] = seq }
        if let epoch { object["epoch"] = epoch }
        return json(object)
    }

    private func agentFrame(innerType: String, payload: [String: Any], seq: UInt64? = nil, epoch: String? = nil) -> String {
        var object: [String: Any] = [
            "event_type": "AgentEvent",
            "data": ["event": ["event_type": innerType, "agent_id": "tutor-agent", "payload": payload]],
        ]
        if let seq { object["seq"] = seq }
        if let epoch { object["epoch"] = epoch }
        return json(object)
    }

    // MARK: - Frame parsing

    func testLegacyFrameWithoutSeqParsesWithClientSideRefs() throws {
        let text = frame(type: "TaskUpdated", data: ["task_id": "task-1", "session_id": "s-1"])
        let parsed = try XCTUnwrap(event(text))
        XCTAssertEqual(parsed.eventType, "TaskUpdated")
        XCTAssertEqual(parsed.text, text)
        XCTAssertNil(parsed.seq)
        XCTAssertNil(parsed.epoch)
        XCTAssertNil(parsed.categories)
        XCTAssertNil(parsed.userRelevant)
        XCTAssertNil(parsed.inner)
        XCTAssertEqual(parsed.refs.taskId, "task-1")
        XCTAssertEqual(parsed.refs.sessionId, "s-1")
        XCTAssertNil(parsed.refs.chatTurnId)
    }

    func testLegacyRefsComeFromEnvelopeLocationsAndNestedPayloads() throws {
        let panel = try XCTUnwrap(event(frame(type: "ExecutionPanelDelta", data: [
            "state": ["overview": ["task_id": "task-9", "execution_id": "exec-9"]],
        ])))
        XCTAssertEqual(panel.refs.taskId, "task-9")
        XCTAssertEqual(panel.refs.executionId, "exec-9")

        let chat = try XCTUnwrap(event(frame(type: "ChatMessageReceived", data: [
            "message": ["session_id": "s-2", "chat_turn_id": "turn-2"],
        ])))
        XCTAssertEqual(chat.refs.sessionId, "s-2")
        XCTAssertEqual(chat.refs.chatTurnId, "turn-2")

        let deep = try XCTUnwrap(event(frame(type: "Custom", data: [
            "metadata": ["context": [["chat_turn_id": "turn-deep"]]],
        ])))
        XCTAssertEqual(deep.refs.chatTurnId, "turn-deep")

        let blank = try XCTUnwrap(event(frame(type: "Custom", data: ["task_id": "  "])))
        XCTAssertNil(blank.refs.taskId)
    }

    func testHubFrameCarriesSeqEpochCategoriesAndServerRefsWin() throws {
        let text = json([
            "event_type": "TaskUpdated",
            "data": ["task_id": "payload-task", "execution_id": "payload-exec"],
            "seq": 42,
            "epoch": "boot-a",
            "categories": ["tasks", "runs"],
            "user_relevant": true,
            "refs": ["task_id": "server-task"],
        ])
        let parsed = try XCTUnwrap(event(text))
        XCTAssertEqual(parsed.seq, 42)
        XCTAssertEqual(parsed.epoch, "boot-a")
        XCTAssertEqual(parsed.categories, ["tasks", "runs"])
        XCTAssertEqual(parsed.userRelevant, true)
        XCTAssertEqual(parsed.refs.taskId, "server-task")
        // A field the server left out falls back to the payload.
        XCTAssertEqual(parsed.refs.executionId, "payload-exec")
    }

    func testControlFramesParse() {
        guard case .hello(let epoch, let seq, let resumed) = RealtimeHubFrame.parse(
            json(["type": "hub.hello", "epoch": "boot-b", "seq": 7, "resumed": true])
        ) else { return XCTFail("expected hello") }
        XCTAssertEqual(epoch, "boot-b")
        XCTAssertEqual(seq, 7)
        XCTAssertTrue(resumed)

        guard case .resync(let reason) = RealtimeHubFrame.parse(
            json(["type": "hub.resync", "reason": "lagged"])
        ) else { return XCTFail("expected resync") }
        XCTAssertEqual(reason, "lagged")

        guard case .control = RealtimeHubFrame.parse(json(["type": "pong"])) else {
            return XCTFail("expected control")
        }
        guard case .invalid = RealtimeHubFrame.parse("not json") else { return XCTFail("expected invalid") }
        guard case .invalid = RealtimeHubFrame.parse(json(["type": "hub.hello"])) else {
            return XCTFail("a hello without an epoch is unusable")
        }
        guard case .invalid = RealtimeHubFrame.parse(json(["no_type": 1])) else {
            return XCTFail("expected invalid")
        }
    }

    func testBinaryFramesParseLikeText() throws {
        let text = frame(type: "HitlRequested", data: ["correlation_id": "c-1"])
        guard case .event(let fromData) = RealtimeHubFrame.parse(.data(Data(text.utf8))) else {
            return XCTFail("expected event")
        }
        XCTAssertEqual(fromData.eventType, "HitlRequested")
        XCTAssertEqual(fromData.text, text)
    }

    func testOtherTypeFramesStillReachSubscribers() throws {
        let parsed = try XCTUnwrap(event(json(["type": "channel.follow_up.updated", "data": ["id": "f-1"]])))
        XCTAssertEqual(parsed.eventType, "channel.follow_up.updated")
        XCTAssertEqual(parsed.data["id"] as? String, "f-1")
    }

    // MARK: - AgentEvent unwrap

    func testAgentEventIsDeliveredRawAndUnwrapped() throws {
        let parsed = try XCTUnwrap(event(agentFrame(
            innerType: "tutor.draw.shape",
            payload: ["chat_turn_id": "turn-t", "shape_json": ["type": "arrow"]]
        )))
        XCTAssertEqual(parsed.eventType, "AgentEvent")
        XCTAssertEqual(parsed.innerEventType, "tutor.draw.shape")
        XCTAssertEqual(parsed.effectiveEventType, "tutor.draw.shape")
        XCTAssertEqual(parsed.payload["chat_turn_id"] as? String, "turn-t")
        XCTAssertEqual(parsed.refs.chatTurnId, "turn-t")
        XCTAssertEqual(parsed.refs.agentId, "tutor-agent")

        let plain = try XCTUnwrap(event(frame(type: "TaskUpdated", data: ["task_id": "t"])))
        XCTAssertNil(plain.innerEventType)
        XCTAssertEqual(plain.effectiveEventType, "TaskUpdated")
        XCTAssertEqual(plain.payload["task_id"] as? String, "t")
    }

    // MARK: - Filter

    func testEventTypePrefixMatchesOuterAndInnerTypes() throws {
        let tutor = try XCTUnwrap(event(agentFrame(innerType: "tutor.run.completed", payload: [:])))
        let panel = try XCTUnwrap(event(frame(type: "ExecutionPanelDelta")))

        XCTAssertTrue(RealtimeHubFilter(eventTypes: ["tutor."]).matches(tutor))
        XCTAssertTrue(RealtimeHubFilter(eventTypes: ["AgentEvent"]).matches(tutor))
        XCTAssertFalse(RealtimeHubFilter(eventTypes: ["tutor."]).matches(panel))
        XCTAssertTrue(RealtimeHubFilter(eventTypes: ["Execution", "tutor."]).matches(panel))
        XCTAssertTrue(RealtimeHubFilter.all.matches(panel))
    }

    func testIdFiltersMatchRefsWithPayloadFallback() throws {
        let legacy = try XCTUnwrap(event(frame(type: "TaskUpdated", data: [
            "task_id": "task-1", "execution_id": "exec-1", "agent_id": "a-1",
        ])))
        XCTAssertTrue(RealtimeHubFilter(taskId: "task-1").matches(legacy))
        XCTAssertFalse(RealtimeHubFilter(taskId: "task-2").matches(legacy))
        XCTAssertTrue(RealtimeHubFilter(taskId: "task-1", executionId: "exec-1", agentId: "a-1").matches(legacy))
        XCTAssertFalse(RealtimeHubFilter(chatTurnId: "turn-1").matches(legacy))
        XCTAssertFalse(RealtimeHubFilter(eventTypes: ["Hitl"], taskId: "task-1").matches(legacy))
    }

    func testCategoryAndUserRelevantFilters() throws {
        let classified = try XCTUnwrap(event(json([
            "event_type": "TaskUpdated", "data": [:],
            "categories": ["tasks"], "user_relevant": false,
        ])))
        XCTAssertTrue(RealtimeHubFilter(categories: ["tasks", "hitl"]).matches(classified))
        XCTAssertFalse(RealtimeHubFilter(categories: ["hitl"]).matches(classified))
        XCTAssertTrue(RealtimeHubFilter(userRelevant: false).matches(classified))
        XCTAssertFalse(RealtimeHubFilter(userRelevant: true).matches(classified))

        // A legacy frame has no classification: a category filter falls back
        // to the payload, and with nothing there it does not match.
        let legacy = try XCTUnwrap(event(frame(type: "TaskUpdated")))
        XCTAssertFalse(RealtimeHubFilter(categories: ["tasks"]).matches(legacy))
        XCTAssertFalse(RealtimeHubFilter(userRelevant: true).matches(legacy))
        let legacyCategory = try XCTUnwrap(event(frame(type: "TaskUpdated", data: ["category": "tasks"])))
        XCTAssertTrue(RealtimeHubFilter(categories: ["tasks"]).matches(legacyCategory))
    }

    // MARK: - Dedupe

    func testDeduperDropsRepeatsAndAdmitsLegacyFrames() {
        var deduper = RealtimeHubDeduper()
        XCTAssertTrue(deduper.admit(epoch: nil, seq: nil))
        XCTAssertTrue(deduper.admit(epoch: nil, seq: nil), "legacy frames are never deduped")

        XCTAssertTrue(deduper.admit(epoch: "a", seq: 1))
        XCTAssertTrue(deduper.admit(epoch: "a", seq: 3))
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 3))
        XCTAssertTrue(deduper.admit(epoch: "a", seq: 2), "a late lower seq not yet seen is admitted")
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 1))
        XCTAssertEqual(deduper.resumePoint, RealtimeHubResumePoint(epoch: "a", seq: 3))

        // A new epoch is a new seq space.
        XCTAssertTrue(deduper.admit(epoch: "b", seq: 1))
        XCTAssertEqual(deduper.resumePoint, RealtimeHubResumePoint(epoch: "b", seq: 1))
    }

    func testDeduperEvictionRaisesTheFloor() {
        var deduper = RealtimeHubDeduper(capacity: 3)
        for seq in UInt64(1)...5 { XCTAssertTrue(deduper.admit(epoch: "a", seq: seq)) }
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 1))
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 2))
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 5))
        XCTAssertTrue(deduper.admit(epoch: "a", seq: 6))
    }

    func testDeduperResetUsesTheHelloSeqAsFloorAndResumePoint() {
        var deduper = RealtimeHubDeduper()
        deduper.reset(epoch: "a", floor: 10)
        XCTAssertEqual(deduper.resumePoint, RealtimeHubResumePoint(epoch: "a", seq: 10))
        XCTAssertFalse(deduper.admit(epoch: "a", seq: 10))
        XCTAssertTrue(deduper.admit(epoch: "a", seq: 11))
        XCTAssertNil(RealtimeHubDeduper().resumePoint)
    }

    // MARK: - Resume query and backoff

    func testSocketURLRequestsStructuredPresentationAndResume() throws {
        let base = try XCTUnwrap(URL(string: "wss://example.invalid"))
        let fresh = try XCTUnwrap(RealtimeHub.socketURL(base: base, resume: nil))
        let freshItems = try XCTUnwrap(URLComponents(url: fresh, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(fresh.path, "/api/magician/v2/realtime/ws")
        XCTAssertEqual(freshItems.first { $0.name == "supports_structured_presentation" }?.value, "true")
        XCTAssertNil(freshItems.first { $0.name == "resume_epoch" })
        XCTAssertNil(freshItems.first { $0.name == "resume_seq" })

        let resumed = try XCTUnwrap(RealtimeHub.socketURL(
            base: base, resume: RealtimeHubResumePoint(epoch: "boot a", seq: 99)
        ))
        let items = try XCTUnwrap(URLComponents(url: resumed, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(items.first { $0.name == "supports_structured_presentation" }?.value, "true")
        XCTAssertEqual(items.first { $0.name == "resume_epoch" }?.value, "boot a")
        XCTAssertEqual(items.first { $0.name == "resume_seq" }?.value, "99")
    }

    func testBackoffIsJitteredAndCappedAtThirtySeconds() {
        XCTAssertEqual(RealtimeHub.backoffDelay(attempt: 0, jitter: 1), 1)
        XCTAssertEqual(RealtimeHub.backoffDelay(attempt: 0, jitter: 0), 0.5)
        XCTAssertEqual(RealtimeHub.backoffDelay(attempt: 3, jitter: 1), 8)
        for attempt in 0..<50 {
            for jitter in [0.0, 0.3, 1.0] {
                let delay = RealtimeHub.backoffDelay(attempt: attempt, jitter: jitter)
                XCTAssertGreaterThan(delay, 0)
                XCTAssertLessThanOrEqual(delay, 30)
            }
        }
        XCTAssertEqual(RealtimeHub.backoffDelay(attempt: 50, jitter: 1), 30)
    }

    // MARK: - When to resync

    func testFirstConnectionNeverResyncs() {
        var state = RealtimeHubConnectionState()
        state.connectionOpened()
        XCTAssertEqual(state.receivedHello(resumed: false), .none)
        XCTAssertEqual(state.receivedResyncFrame(), .none, "the hello's follow-up resync on a first connection")
        XCTAssertEqual(state.receivedEvent(), .none)

        var legacy = RealtimeHubConnectionState()
        legacy.connectionOpened()
        XCTAssertEqual(legacy.receivedEvent(), .none)
    }

    func testReconnectWithoutHelloResyncsOnce() {
        var state = RealtimeHubConnectionState()
        state.connectionOpened()
        XCTAssertEqual(state.receivedEvent(), .none)

        state.connectionOpened()
        XCTAssertTrue(state.isReconnect)
        XCTAssertEqual(state.receivedEvent(), .resync, "a legacy server's reconnect is a reconnect without resume")
        XCTAssertEqual(state.receivedEvent(), .none)
        XCTAssertEqual(state.helloTimedOut(), .none)
    }

    func testReconnectWithoutHelloResyncsOnTimeoutWhenQuiet() {
        var state = RealtimeHubConnectionState()
        state.connectionOpened()
        state.connectionOpened()
        XCTAssertEqual(state.helloTimedOut(), .resync)
        XCTAssertEqual(state.receivedEvent(), .none)
    }

    func testReconnectResyncFollowsTheResumeOutcome() {
        var resumed = RealtimeHubConnectionState()
        resumed.connectionOpened()
        resumed.connectionOpened()
        XCTAssertEqual(resumed.receivedHello(resumed: true), .none)
        XCTAssertEqual(resumed.receivedEvent(), .none)
        XCTAssertEqual(resumed.receivedResyncFrame(), .resync, "a mid-stream resync (lag) always fans out")

        var notResumed = RealtimeHubConnectionState()
        notResumed.connectionOpened()
        notResumed.connectionOpened()
        XCTAssertEqual(notResumed.receivedHello(resumed: false), .resync)
        XCTAssertEqual(notResumed.receivedResyncFrame(), .none, "the server's follow-up resync is coalesced")
        XCTAssertEqual(notResumed.helloTimedOut(), .none)
    }

    func testResetMakesTheNextConnectionAFirstOne() {
        var state = RealtimeHubConnectionState()
        state.connectionOpened()
        state.reset()
        state.connectionOpened()
        XCTAssertFalse(state.isReconnect)
        XCTAssertEqual(state.receivedEvent(), .none)
    }

    // MARK: - Hub fan-out (inert hub, frames fed by hand)

    func testHubDeliversMatchingEventsOnceAndStopsAfterCancel() {
        let hub = RealtimeHub(connectsSockets: false)
        var tutorTypes: [String] = []
        var all = 0
        let tutor = hub.subscribe(
            RealtimeHubFilter(eventTypes: ["tutor."]),
            onEvent: { tutorTypes.append($0.effectiveEventType) }
        )
        let everything = hub.subscribe(onEvent: { _ in all += 1 })
        XCTAssertEqual(hub.subscriberCount, 2)

        hub.connectionDidOpen()
        hub.ingest(json(["type": "hub.hello", "epoch": "e1", "seq": 0, "resumed": false]))
        hub.ingest(agentFrame(innerType: "tutor.draw.shape", payload: [:], seq: 1, epoch: "e1"))
        hub.ingest(agentFrame(innerType: "tutor.draw.shape", payload: [:], seq: 1, epoch: "e1"))   // replayed
        hub.ingest(frame(type: "TaskUpdated", seq: 2, epoch: "e1"))
        hub.ingest(json(["type": "pong"]))
        XCTAssertEqual(tutorTypes, ["tutor.draw.shape"])
        XCTAssertEqual(all, 2)

        tutor.cancel()
        hub.ingest(agentFrame(innerType: "tutor.run.completed", payload: [:], seq: 3, epoch: "e1"))
        XCTAssertEqual(tutorTypes, ["tutor.draw.shape"])
        XCTAssertEqual(all, 3)
        XCTAssertEqual(hub.subscriberCount, 1)
        everything.cancel()
        XCTAssertEqual(hub.subscriberCount, 0)
    }

    func testHubResyncsSubscribersWhenAReconnectBringsNoHello() {
        let hub = RealtimeHub(connectsSockets: false)
        var resyncs = 0
        var events: [String] = []
        let subscription = hub.subscribe(
            onEvent: { events.append($0.eventType) },
            onResync: { resyncs += 1 }
        )

        // A server that predates the hub: no hello, no seq.
        hub.connectionDidOpen()
        hub.ingest(frame(type: "TaskUpdated"))
        XCTAssertEqual(resyncs, 0, "the first connection has nothing to catch up on")

        hub.connectionDidOpen()   // reconnect
        hub.ingest(frame(type: "TaskUpdated"))
        XCTAssertEqual(resyncs, 1)
        XCTAssertEqual(events, ["TaskUpdated", "TaskUpdated"], "legacy frames are not deduped")
        withExtendedLifetime(subscription) {}
    }

    func testHubResyncFrameAndHelloFloor() {
        let hub = RealtimeHub(connectsSockets: false)
        var resyncs = 0
        var seqs: [UInt64] = []
        let subscription = hub.subscribe(
            onEvent: { if let seq = $0.seq { seqs.append(seq) } },
            onResync: { resyncs += 1 }
        )

        hub.connectionDidOpen()
        hub.ingest(json(["type": "hub.hello", "epoch": "e1", "seq": 5, "resumed": false]))
        hub.ingest(frame(seq: 5, epoch: "e1"))   // at the hello's seq: already reflected
        hub.ingest(frame(seq: 6, epoch: "e1"))
        XCTAssertEqual(seqs, [6])
        XCTAssertEqual(resyncs, 0)

        hub.ingest(json(["type": "hub.resync", "reason": "lagged"]))
        XCTAssertEqual(resyncs, 1)

        // Reconnect that the server could not resume: one resync, not two.
        hub.connectionDidOpen()
        hub.ingest(json(["type": "hub.hello", "epoch": "e2", "seq": 100, "resumed": false]))
        hub.ingest(json(["type": "hub.resync", "reason": "epoch"]))
        XCTAssertEqual(resyncs, 2)
        hub.ingest(frame(seq: 101, epoch: "e2"))
        XCTAssertEqual(seqs, [6, 101])
        withExtendedLifetime(subscription) {}
    }

    func testReleasingTheSubscriptionUnsubscribes() {
        let hub = RealtimeHub(connectsSockets: false)
        var delivered = 0
        var subscription: RealtimeHubSubscription? = hub.subscribe(onEvent: { _ in delivered += 1 })
        hub.ingest(frame())
        XCTAssertNotNil(subscription)
        subscription = nil
        hub.ingest(frame())
        XCTAssertEqual(delivered, 1)
        XCTAssertEqual(hub.subscriberCount, 0)
    }
}
