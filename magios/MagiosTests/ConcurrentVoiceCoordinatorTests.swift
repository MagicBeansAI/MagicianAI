import XCTest
@testable import Magician

@MainActor
final class ConcurrentVoiceCoordinatorTests: XCTestCase {
    func testAutomatedBranchAndTextQueueMetadata() throws {
        let session = try JSONDecoder().decode(ChatSession.self, from: jsonData([
            "id": "branch", "principal": "me", "workspace": "default", "agent_id": "personal-assistant",
            "ui_thread_id": "general", "status": "active", "created_at": 1, "updated_at": 2,
            "history_lane": "automated", "internal_voice": ["kind": "branch", "parent_session_id": "parent"]
        ]))
        XCTAssertEqual(session.historyLane, "automated")
        XCTAssertEqual(session.internalVoice?.parentSessionId, "parent")
        let queued = try JSONDecoder().decode(QueuedMessage.self, from: jsonData([
            "id": "q", "text": "Follow up", "attachment_ids": ["file"]
        ]))
        XCTAssertEqual(queued.attachmentIds, ["file"])
    }

    func testOriginalAnswerLinksSurviveProjectionAndResolveLegacyCopiesPrecisely() throws {
        func raw(_ id: String, at: Int = 42, direction: String = "assistant", origin: Bool = false) throws -> ChatMessageRawData {
            var payload: [String: Any] = ["id": id, "session_id": origin ? "parent" : "branch",
                "direction": direction, "created_at": at, "chat_turn_id": "turn",
                "content": ["type": "text", "text": "Answer"]]
            if origin { payload["context_origin"] = ["ui_thread_id": "ideas", "session_id": "branch", "request_id": "turn", "message_id": "saved"] }
            return try JSONDecoder().decode(ChatMessageRawData.self, from: jsonData(payload))
        }
        let link = try XCTUnwrap(ChatViewModel.projectedMessage(from: raw("copy", origin: true)).originalAnswer)
        XCTAssertEqual(link.origin.uiThreadId, "ideas")
        XCTAssertTrue(try link.matches(raw("saved")))
        XCTAssertFalse(try link.matches(raw("other")))
        let legacyOrigin = try JSONDecoder().decode(ChatMessageOrigin.self, from: jsonData([
            "ui_thread_id": "ideas", "session_id": "branch", "request_id": "turn"]))
        let legacy = OriginalAnswerLink(origin: legacyOrigin, turnId: "turn", createdAt: 42)
        XCTAssertTrue(try legacy.matches(raw("saved")))
        XCTAssertFalse(try legacy.matches(raw("later", at: 43)))
        XCTAssertFalse(try legacy.matches(raw("user", direction: "user")))
        XCTAssertNil(try ChatViewModel.projectedMessage(from: raw("saved")).originalAnswer)
    }

    func testLinkedTaskAnswerIsNotHiddenByALaterTaskUpdate() throws {
        let rows = try ["original", "newer"].map { id in
            try JSONDecoder().decode(ChatMessageRawData.self, from: jsonData([
                "id": id, "session_id": "branch", "direction": "system",
                "content": ["type": "task_status_update", "task_id": "task", "execution_id": "run",
                            "status": "completed", "summary": id]
            ]))
        }
        let origin = try JSONDecoder().decode(ChatMessageOrigin.self, from: jsonData([
            "ui_thread_id": "ideas", "session_id": "branch", "request_id": "turn", "message_id": "original"]))
        XCTAssertEqual(ChatViewModel.projectedMessages(from: rows).count, 1)
        let linked = ChatViewModel.projectedMessages(from: rows,
            target: OriginalAnswerLink(origin: origin, turnId: nil, createdAt: nil))
        XCTAssertEqual(linked.map(\.id), ["original", "newer"])
        XCTAssertTrue(linked[0].linkedAnswerTarget)
        XCTAssertFalse(linked[1].linkedAnswerTarget)
    }

    func testOriginalAnswerNavigationLoadsAnOlderPageWithoutTheVoiceLedger() async throws {
        let pageRequested = expectation(description: "Older history requested")
        let origin = try JSONDecoder().decode(ChatMessageOrigin.self, from: jsonData([
            "ui_thread_id": "ideas", "session_id": "branch", "request_id": "turn", "message_id": "saved"]))
        let rows: [[String: Any]] = (0..<200).map { index in
            ["id": "later-\(index)", "session_id": "branch", "direction": "assistant", "created_at": 100 + index,
             "content": ["type": "text", "text": "Later answer"]]
        }
        MockURLProtocol.handler = { request in
            let path = request.url!.path
            if path.hasSuffix("/branch/messages") {
                XCTAssertTrue(request.url!.query!.contains("before=later-0"))
                pageRequested.fulfill()
                return (response(for: request), jsonData(["has_more": false, "messages": [[
                    "id": "saved", "session_id": "branch", "direction": "assistant", "created_at": 42,
                    "content": ["type": "text", "text": "Original answer"]]]]))
            }
            if path.hasSuffix("/branch") {
                return (response(for: request), jsonData(["session": [
                    "id": "branch", "principal": "anonymous", "workspace": "default", "agent_id": "personal-assistant",
                    "ui_thread_id": "ideas", "title": "#con1-Task", "status": "active", "created_at": 1, "updated_at": 2
                ], "messages": rows]))
            }
            XCTAssertFalse(path.contains("/media/voice"))
            return (response(for: request), jsonData([:]))
        }
        let suite = "OriginalAnswerTests-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let vm = ChatViewModel(networkSession: makeMockSession(), connectsOnInit: false,
                               sessionSelectionStore: ChatSessionSelectionStore(defaults: defaults))
        defer { MockURLProtocol.handler = { request in (response(for: request), jsonData([:])) } }
        vm.loadSession("branch", target: OriginalAnswerLink(origin: origin, turnId: nil, createdAt: nil))
        await fulfillment(of: [pageRequested], timeout: 5)
        for _ in 0..<100 where vm.focusedMessageId == nil { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertEqual(vm.focusedMessageId, "saved")
        XCTAssertEqual(vm.messages.count, 201)
        XCTAssertNil(vm.originalAnswerError)
    }

    @MainActor private final class Fixture {
        var commands: [[String: Any]] = []
        var paths: [String] = []
        var output: [String: Any]?
        var busy = false
        var time: TimeInterval = 100
        var plays = 0
        var onClaim: () -> Void = {}
        var started: (() -> Void)?
        var finished: ((SpeechPlaybackResult) -> Void)?
        var rows = [Fixture.row("a")]
        var revision = 0
        lazy var coordinator = ConcurrentVoiceCoordinator(request: { [unowned self] path, body in
            paths.append(path)
            if let body {
                commands.append(body)
                if body["action"] as? String == "acquire" {
                    output = ["device_id": body["device_id"]!, "interaction_id": body["interaction_id"]!, "epoch": 1]
                }
                if body["action"] as? String == "claim" { onClaim() }
            }
            revision += 1
            var snapshot: [String: Any] = ["revision": revision, "requests": rows]
            snapshot["output"] = output
            return try JSONSerialization.data(withJSONObject: snapshot)
        }, play: { [unowned self] _, start, finish in
            plays += 1; started = start; finished = finish
        }, stop: { [unowned self] in finished?(.cancelled) }, now: { [unowned self] in time })
        static func row(_ id: String) -> [String: Any] {
            ["id": id, "parent_session_id": "parent-\(id)", "branch_session_id": "branch-\(id)",
             "title": "Topic \(id)", "work_status": "completed", "delivery_status": "pending",
             "speech_text": "Answer \(id)", "created_at": 1, "updated_at": 1]
        }
        func record(_ id: String) throws -> VoiceRequestRecord {
            try ConcurrentVoiceCoordinator.decode(VoiceRequestRecord.self, JSONSerialization.data(withJSONObject: Self.row(id)))
        }
        var events: [String] { commands.compactMap { $0["event"] as? String } }
        func activate() {
            coordinator.screenActive = true; coordinator.voiceInteracted = true
            coordinator.foregroundBusy = { [unowned self] in busy }
            coordinator.activate()
        }
    }
    private func drainTasks() async { for _ in 0..<20 { await Task.yield() } }

    func testPanelLifetimeFollowsActiveUnreadAndSelectedTopic() throws {
        let f = Fixture()
        var row = try f.record("a")
        XCTAssertTrue(row.visible(selectedId: nil))
        row.readAt = 42
        XCTAssertFalse(row.visible(selectedId: nil)); XCTAssertTrue(row.visible(selectedId: "a"))
        row.pendingTasks = ["task"]
        XCTAssertTrue(row.visible(selectedId: nil))
        var wire = Fixture.row("a"); wire["delivery_status"] = "played"
        row = try ConcurrentVoiceCoordinator.decode(VoiceRequestRecord.self, JSONSerialization.data(withJSONObject: wire))
        XCTAssertFalse(row.visible(selectedId: nil)); XCTAssertTrue(row.visible(selectedId: "a"))
        wire["delivery_status"] = "dismissed"
        row = try ConcurrentVoiceCoordinator.decode(VoiceRequestRecord.self, JSONSerialization.data(withJSONObject: wire))
        XCTAssertFalse(row.visible(selectedId: "a"))
    }
    func testReadResultStaysQuietUntilExplicitReplay() async {
        let f = Fixture(); f.rows[0]["read_at"] = 42; f.activate()
        await f.coordinator.tick(); XCTAssertEqual(f.plays, 0)
        f.coordinator.replay("a"); await f.coordinator.tick(); XCTAssertEqual(f.plays, 1)
    }
    func testDismissedSelectionClearsAfterCapturedInputSettles() async throws {
        let f = Fixture(); f.coordinator.select(try f.record("a")); f.coordinator.captureStarted()
        f.rows[0]["delivery_status"] = "dismissed"; await f.coordinator.tick()
        XCTAssertNil(f.coordinator.focus); XCTAssertEqual(f.coordinator.target("parent-a").context, "branch-a")
        f.coordinator.captureStopped(); f.coordinator.inputSettled()
        XCTAssertNil(f.coordinator.target("parent-a").context)
    }
    func testForegroundAudioAndActualPlaybackCallbacksControlDelivery() async {
        let f = Fixture(); f.activate(); f.busy = true
        await f.coordinator.tick(); XCTAssertEqual(f.plays, 0)
        f.busy = false; await f.coordinator.tick(); XCTAssertEqual(f.plays, 1)
        XCTAssertNil(f.coordinator.focus); XCTAssertFalse(f.events.contains("completed"))
        f.started?(); await drainTasks()
        XCTAssertEqual(f.coordinator.focus?.id, "a"); XCTAssertTrue(f.events.contains("started"))
        XCTAssertFalse(f.events.contains("completed"))
        f.finished?(.completed); await drainTasks(); XCTAssertTrue(f.events.contains("completed"))
    }
    func testCaptureDuringClaimRejectsOutputWithoutCancellingWork() async {
        let f = Fixture(); f.activate(); f.onClaim = { f.coordinator.captureStarted() }
        await f.coordinator.tick()
        XCTAssertEqual(f.plays, 0); XCTAssertTrue(f.events.contains("rejected"))
        XCTAssertFalse(f.paths.contains { $0.hasSuffix("/cancel") })
    }
    func testInterruptionPreservesWorkAndReportsUnfinishedAudio() async {
        let f = Fixture(); f.activate(); await f.coordinator.tick(); f.started?(); await drainTasks()
        f.coordinator.captureStarted(); await drainTasks()
        XCTAssertTrue(f.events.contains("interrupted")); XCTAssertFalse(f.events.contains("completed"))
        XCTAssertFalse(f.paths.contains { $0.hasSuffix("/cancel") })
    }
    func testCaptureKeepsOriginalTopicThroughSelectionChange() throws {
        let f = Fixture(); f.coordinator.select(try f.record("a")); f.coordinator.captureStarted()
        f.coordinator.select(try f.record("b"))
        XCTAssertEqual(f.coordinator.target("other").context, "branch-a")
        f.coordinator.captureStopped(); f.coordinator.inputSettled()
        XCTAssertEqual(f.coordinator.target("other").context, "branch-b")
    }
    func testHiddenScreenDoesNotClaimOrPlay() async {
        let f = Fixture(); f.activate(); f.coordinator.screenActive = false
        await f.coordinator.tick(); XCTAssertEqual(f.plays, 0); XCTAssertTrue(f.commands.isEmpty)
    }
    func testOtherDeviceOwnsOutputWithoutClaimingOrError() async {
        let f = Fixture(); f.activate()
        f.output = ["device_id": "other", "interaction_id": "other", "epoch": 2, "expires_at": 130000]
        await f.coordinator.tick()
        XCTAssertEqual(f.plays, 0); XCTAssertTrue(f.commands.isEmpty); XCTAssertNil(f.coordinator.error)
    }
    func testLostAdmissionAcknowledgementReusesIdempotencyKey() async throws {
        var bodies: [[String: Any]] = []
        let coordinator = ConcurrentVoiceCoordinator(request: { _, body in
            bodies.append(body!)
            if bodies.count == 1 { throw URLError(.networkConnectionLost) }
            return Data()
        }, play: { _, _, _ in }, stop: {})
        try await coordinator.submit("parent", text: "Question", options: [:])
        XCTAssertEqual(bodies.count, 2)
        XCTAssertEqual(bodies[0]["submission_id"] as? String, bodies[1]["submission_id"] as? String)
    }
    func testTypedParallelSendPreservesPendingVoiceContextAndUsesDisplayedChat() async throws {
        let f = Fixture(); f.activate()
        f.coordinator.select(try f.record("a")); f.coordinator.captureStarted(); f.coordinator.captureStopped()
        f.coordinator.select(try f.record("b"))
        try await f.coordinator.submit("typed-parent", text: "Typed question", options: [:])
        XCTAssertTrue(f.paths.contains("/chat/sessions/typed-parent/voice/requests"))
        XCTAssertNil(f.commands.first?["context_session_id"])
        XCTAssertEqual(f.coordinator.target("typed-parent").parent, "parent-a")
        XCTAssertEqual(f.coordinator.target("typed-parent").context, "branch-a")
        f.time += 1; await f.coordinator.tick(); XCTAssertEqual(f.plays, 0)
        f.coordinator.inputSettled(); f.time += 1; await f.coordinator.tick(); XCTAssertEqual(f.plays, 1)
        XCTAssertFalse(f.paths.contains { $0.hasSuffix("/cancel") })
    }

}
