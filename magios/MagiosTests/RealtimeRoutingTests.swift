import XCTest
import Combine
@testable import Magician

/// Realtime event routing: the WS event classifiers that gate a reload, and the
/// ChatViewModel event handler's append-vs-suppress (WS-echo dedup) decision.
final class RealtimeRoutingTests: XCTestCase {
    override func setUp() {
        super.setUp()
        // Permissive default so incidental requests (e.g. startNewSession's
        // reference-catalog fetch) don't XCTFail; specific tests override it.
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        for suite in defaultsSuites { UserDefaults.standard.removePersistentDomain(forName: suite) }
        defaultsSuites = []
        DeferredMockURLProtocol.handler = nil
        super.tearDown()
    }

    // MARK: - Voice-call audio focus (suppresses chat auto-speak during a Live call)

    /// A realtime Live call holds audio focus for its whole lifetime so chat
    /// auto-speak never adds a second on-device voice (or steals the shared
    /// AVAudioSession from the still-running capture engine). The focus must
    /// survive until EVERY owner releases — mirrors TutorAudioFocus.
    func testVoiceCallAudioFocusRemainsUntilEveryOwnerReleases() {
        let first = VoiceCallAudioFocus.shared.acquire()
        let second = VoiceCallAudioFocus.shared.acquire()
        XCTAssertTrue(VoiceCallAudioFocus.shared.isActive)
        VoiceCallAudioFocus.shared.release(first)
        XCTAssertTrue(VoiceCallAudioFocus.shared.isActive)
        VoiceCallAudioFocus.shared.release(second)
        XCTAssertFalse(VoiceCallAudioFocus.shared.isActive)
    }

    // MARK: - Event classifiers (gate the debounced reload)

    func testIsTaskEventClassifier() {
        let vm = TasksViewModel(session: makeMockSession())
        XCTAssertTrue(vm.isTaskEvent(#"{"event_type":"TaskUpdated"}"#))
        XCTAssertTrue(vm.isTaskEvent(#"{"event_type":"V3PlanningCompleted"}"#))
        XCTAssertTrue(vm.isTaskEvent(#"{"event_type":"ExecutionPanelDelta"}"#))
        XCTAssertFalse(vm.isTaskEvent(#"{"event_type":"ChatMessage"}"#))
        XCTAssertFalse(vm.isTaskEvent(#"{"event_type":"HitlRequested"}"#))
        XCTAssertFalse(vm.isTaskEvent("not json"))
        XCTAssertFalse(vm.isTaskEvent(#"{"no_event_type":1}"#))
    }

    func testIsHitlEventClassifier() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        XCTAssertTrue(vm.isHitlEvent(#"{"event_type":"HitlRequested"}"#))
        XCTAssertTrue(vm.isHitlEvent(#"{"event_type":"HitlResolved"}"#))
        XCTAssertTrue(vm.isHitlEvent(#"{"event_type":"UserRequestCreated"}"#))
        XCTAssertTrue(vm.isHitlEvent(#"{"event_type":"AttentionUpdated"}"#))
        XCTAssertFalse(vm.isHitlEvent(#"{"event_type":"ChatMessage"}"#))
        XCTAssertFalse(vm.isHitlEvent("garbage"))
    }

    /// The same event can arrive framed as text or as binary. Reading only
    /// `.string` drops the binary ones, and a dropped attention event is
    /// indistinguishable from a quiet inbox.
    func testRealtimeFrameFlattensBinaryAndTextIdentically() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let payload = #"{"event_type":"HitlRequested"}"#

        let fromText = AttentionViewModel.realtimeText(from: .string(payload))
        let fromBinary = AttentionViewModel.realtimeText(
            from: .data(Data(payload.utf8))
        )

        XCTAssertEqual(fromText, payload)
        XCTAssertEqual(fromBinary, payload)
        // Both must reach the same verdict, or liveness depends on framing.
        XCTAssertTrue(vm.isHitlEvent(fromText))
        XCTAssertTrue(vm.isHitlEvent(fromBinary))

        // Undecodable bytes degrade to a non-event rather than crashing.
        let invalid = AttentionViewModel.realtimeText(
            from: .data(Data([0xFF, 0xFE, 0xFD]))
        )
        XCTAssertEqual(invalid, "")
        XCTAssertFalse(vm.isHitlEvent(invalid))
    }

    // MARK: - ChatViewModel handleIncomingJSON (append vs dedup)

    private var defaultsSuites: [String] = []

    private func chat(networkSession: URLSession = makeMockSession()) -> ChatViewModel {
        let suite = "RealtimeRoutingTests-\(UUID().uuidString)"
        defaultsSuites.append(suite)
        let m = ChatViewModel(
            networkSession: networkSession, connectsOnInit: false,
            sessionSelectionStore: ChatSessionSelectionStore(defaults: UserDefaults(suiteName: suite)!)
        )
        m.startNewSession("s1")
        return m
    }

    private func textEvent(id: String, session: String = "s1", embeddedSession: String? = nil,
                           direction: String = "assistant", turn: String? = nil,
                           text: String = "Remote reply") -> String {
        var message: [String: Any] = [
            "id": id, "session_id": embeddedSession ?? session, "direction": direction,
            "content": ["type": "text", "text": text], "source_surface": "android"
        ]
        if let turn { message["chat_turn_id"] = turn }
        return String(data: jsonData([
            "event_type": "ChatMessageReceived",
            "data": ["session_id": session, "message": message]
        ]), encoding: .utf8)!
    }

    func testOtherConversationCannotAppendOrClearThinking() {
        let m = chat()
        m.isThinking = true
        let before = m.messages.map(\.id)
        m.handleIncomingJSON(textEvent(id: "foreign", session: "android-session"))
        XCTAssertEqual(m.messages.map(\.id), before)
        XCTAssertTrue(m.isThinking)
    }

    func testConversationEnvelopeMustAgreeWithEmbeddedMessage() {
        let m = chat()
        let before = m.messages.map(\.id)
        m.handleIncomingJSON(textEvent(id: "conflicting", embeddedSession: "another-session"))
        m.handleIncomingJSON(textEvent(id: "conflicting-outer", session: "another-session", embeddedSession: "s1"))
        XCTAssertEqual(m.messages.map(\.id), before)
    }

    func testNoSelectedConversationDoesNotAdoptBroadcastMessages() {
        let m = chat()
        m.startNewSession(nil)
        let before = m.messages.map(\.id)
        m.handleIncomingJSON(textEvent(id: "unselected"))
        XCTAssertEqual(m.messages.map(\.id), before)
        XCTAssertNil(m.currentSessionIdValue)
    }

    func testSelectedConversationReceivesBothSidesFromAnotherDeviceOnce() {
        let m = chat()
        let user = textEvent(id: "remote-user", direction: "user", turn: "remote-turn", text: "Android asks")
        let reply = textEvent(id: "remote-answer", turn: "remote-turn")
        for _ in 0..<2 {
            m.handleIncomingJSON(user)
            m.handleIncomingJSON(reply)
        }
        XCTAssertEqual(m.messages.filter { $0.id == "remote-user" }.count, 1)
        XCTAssertEqual(m.messages.first { $0.id == "remote-user" }?.text, "Android asks")
        XCTAssertEqual(m.messages.filter { $0.id == "remote-answer" }.count, 1)
        XCTAssertLessThan(m.messages.firstIndex { $0.id == "remote-user" } ?? Int.max,
                          m.messages.firstIndex { $0.id == "remote-answer" } ?? 0)
    }

    func testCanonicalUserEchoReplacesOnlyItsOptimisticTurn() {
        let m = chat()
        var turnID: String?
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/messages/stream") == true {
                let body = try JSONSerialization.jsonObject(with: requestBody(req)!) as! [String: Any]
                turnID = body["chat_turn_id"] as? String
                return (response(for: req), Data())
            }
            return (response(for: req), jsonData([:]))
        }
        m.sendMessage("Identical text")
        waitUntil { turnID != nil }
        m.handleIncomingJSON(textEvent(id: "own-canonical", direction: "user", turn: turnID, text: "Identical text"))
        XCTAssertEqual(m.messages.filter(\.isUser).map(\.id), ["own-canonical"])
        m.handleIncomingJSON(textEvent(id: "other-canonical", direction: "user", turn: "different-turn", text: "Identical text"))
        XCTAssertEqual(m.messages.filter(\.isUser).map(\.id), ["own-canonical", "other-canonical"])
    }

    func testOtherTurnIsNotDroppedDuringStreamingGrace() {
        let m = chat()
        var turnID: String?
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/messages/stream") == true {
                let body = try JSONSerialization.jsonObject(with: requestBody(req)!) as! [String: Any]
                turnID = body["chat_turn_id"] as? String
                return (response(for: req), Data("data: {\"text\": \"Streamed\"}\n".utf8))
            }
            return (response(for: req), jsonData([:]))
        }
        m.sendMessage("hi")
        waitUntil(timeout: 5) { m.messages.last?.text == "Streamed" }
        m.handleIncomingJSON(textEvent(id: "own-answer", turn: turnID, text: "Streamed"))
        XCTAssertEqual(m.messages.filter { $0.text == "Streamed" }.count, 1)
        m.handleIncomingJSON(textEvent(id: "other-answer", turn: "different-turn", text: "Another device's reply"))
        XCTAssertEqual(m.messages.filter { $0.id == "other-answer" }.count, 1)
    }

    func testCanonicalReplyReplacesInterruptedStreamPreviewForTheSameTurn() {
        var preview = ChatMessage(
            id: "streaming-local",
            isUser: false,
            text: "Partial",
            type: .text
        )
        preview.chatTurnId = "durable-turn"
        var canonical = ChatMessage(
            id: "canonical-answer",
            isUser: false,
            text: "Complete answer",
            type: .text
        )
        canonical.chatTurnId = "durable-turn"
        var transcript = [preview]

        XCTAssertTrue(ChatViewModel.reconcileCanonicalText(canonical, into: &transcript))
        XCTAssertEqual(transcript.map(\.id), ["canonical-answer"])
        XCTAssertEqual(transcript.first?.text, "Complete answer")
        XCTAssertFalse(ChatViewModel.reconcileCanonicalText(canonical, into: &transcript))
        XCTAssertEqual(transcript.count, 1)
    }

    func testOldStreamCannotChangeReplacementConversationEvenWhenReopeningSameSession() {
        for selected in ["s2", "s1"] {
            let m = chat(networkSession: makeDeferredMockSession())
            var pending: [String: (URLRequest, (DeferredMockURLProtocol.ResultValue) -> Void)] = [:]
            DeferredMockURLProtocol.handler = { req, finish in
                if req.url?.path.hasSuffix("/messages/stream") == true {
                    let body = try! JSONSerialization.jsonObject(with: requestBody(req)!) as! [String: Any]
                    DispatchQueue.main.async { pending[body["text"] as! String] = (req, finish) }
                } else {
                    finish(.success((response(for: req), jsonData([:]))))
                }
            }
            m.sendMessage("old")
            waitUntil { pending["old"] != nil }
            m.startNewSession(selected)
            m.sendMessage("new")
            waitUntil { pending["new"] != nil }

            let staleUpdate = expectation(description: "old stream must not publish into \(selected)")
            staleUpdate.isInverted = true
            let observation = m.$messages.combineLatest(m.$isThinking).sink { messages, thinking in
                if messages.contains(where: { $0.text == "Old reply" }) || !thinking {
                    staleUpdate.fulfill()
                }
            }
            let old = pending["old"]!
            old.1(.success((response(for: old.0), Data("data: {\"text\": \"Old reply\"}\n".utf8))))
            wait(for: [staleUpdate], timeout: 0.5)
            observation.cancel()
            let new = pending["new"]!
            new.1(.success((response(for: new.0), Data("data: {\"text\": \"New reply\"}\n".utf8))))
            waitUntil { m.messages.contains { $0.text == "New reply" } }
            XCTAssertFalse(m.messages.contains { $0.text == "Old reply" })
            XCTAssertEqual(m.currentSessionIdValue, selected)
        }
    }

    func testUserTurnIdentitySurvivesActivityAnchorProjection() {
        var user = ChatMessage(id: "user", isUser: true, text: "Question", type: .text)
        user.chatTurnId = "same-turn"
        var reply = ChatMessage(id: "reply", isUser: false, text: "Answer", type: .text)
        reply.chatTurnId = "same-turn"
        let projected = ChatViewModel.retainLatestActivityAnchorPerTurn([user, reply])
        XCTAssertEqual(projected[0].chatTurnId, "same-turn")
        XCTAssertTrue(projected[0].activityRows.isEmpty)
        XCTAssertEqual(projected[1].chatTurnId, "same-turn")
    }

    func testLegacyCompletionWithoutConversationCannotChangeChat() {
        let m = chat()
        m.isThinking = true
        let before = m.messages.map(\.id)
        m.handleIncomingJSON(#"{"event_type":"MessageCompleted","data":{"execution_id":"e","turn_id":"t","correlation_id":"c","response":"Orchestrator result"}}"#)
        m.handleIncomingJSON(#"{"event_type":"MessageCompleted","data":{}}"#)
        XCTAssertEqual(m.messages.map(\.id), before)
        XCTAssertTrue(m.isThinking)
    }

    func testUnknownAndMalformedEventsAreNoops() {
        let m = chat(); m.startNewSession("s1")
        let before = m.messages.count
        m.handleIncomingJSON(#"{"event_type":"SomethingUnknown","data":{}}"#)
        m.handleIncomingJSON("not json at all")
        m.handleIncomingJSON(#"{"no_event_type":1}"#)
        XCTAssertEqual(m.messages.count, before)
    }

    // MARK: - ExecutionPanelDelta → task-status rendering (updateExecutionPanel)

    private func executionPanelDelta(
        taskId: String,
        title: String,
        status: String,
        step: String,
        inspectionExecutionId: String? = nil
    ) -> String {
        var overview: [String: Any] = [
            "task_id": taskId, "principal": "local", "workspace": "default",
            "ui_thread_id": "general", "title": title, "description": "",
            "status": status, "assigned_agent_id": "personal-assistant",
            "has_plan": false, "created_at": 0, "updated_at": 1
        ]
        if let inspectionExecutionId { overview["execution_id"] = inspectionExecutionId }
        let state: [String: Any] = [
            "default_tab": "run",
            "overview": overview,
            "run": ["recent_activity": [["id": "a1", "kind": "step", "timestamp": 1, "title": step]]],
            "output": ["deliveries": []],
            "debug": ["history_count": 0]
        ]
        let event: [String: Any] = [
            "event_type": "ExecutionPanelDelta",
            "data": ["principal": "local", "workspace": "default", "task_id": taskId, "state": state]
        ]
        return String(data: try! JSONSerialization.data(withJSONObject: event), encoding: .utf8)!
    }

    func testExecutionPanelDeltaCreatesThenUpdatesTaskStatus() {
        let m = chat()   // no session needed — the delta path just renders a taskStatus card
        m.handleIncomingJSON(executionPanelDelta(taskId: "task-1", title: "My Task", status: "running", step: "Doing A"))
        guard case .taskStatus(let s)? = m.messages.last?.type else { return XCTFail("expected a taskStatus message") }
        XCTAssertEqual(s.taskId, "task-1")
        XCTAssertEqual(s.title, "My Task")
        XCTAssertEqual(s.status, "running")
        XCTAssertEqual(s.steps, ["Doing A"])

        let count = m.messages.count
        // A second delta for the SAME task updates that bubble in place (no duplicate).
        m.handleIncomingJSON(executionPanelDelta(taskId: "task-1", title: "My Task", status: "completed", step: "Doing A"))
        XCTAssertEqual(m.messages.count, count)
        if case .taskStatus(let s2)? = m.messages.last?.type {
            XCTAssertEqual(s2.status, "completed")
        } else {
            XCTFail("expected an updated taskStatus")
        }
    }

    func testRealtimeTaskStatusMessageCreatesAndCompletesOneCard() {
        let m = chat()
        m.startNewSession("s1")

        func event(id: String, status: String, summary: String) -> String {
            let object: [String: Any] = [
                "event_type": "ChatMessageReceived",
                "data": [
                    "session_id": "s1",
                    "message": [
                        "id": id,
                        "session_id": "s1",
                        "direction": "system",
                        "content": [
                            "type": "task_status_update",
                            "task_id": "task-1",
                            "status": status,
                            "display_label": "Prepare report",
                            "summary": summary,
                            "execution_id": "exec-1"
                        ]
                    ]
                ]
            ]
            return String(
                data: try! JSONSerialization.data(withJSONObject: object),
                encoding: .utf8
            )!
        }

        m.handleIncomingJSON(event(id: "running", status: "running", summary: "Working"))
        m.handleIncomingJSON(event(id: "completed", status: "completed", summary: "Ready"))

        let cards = m.messages.compactMap { message -> TaskStatusModel? in
            if case .taskStatus(let task) = message.type { return task }
            return nil
        }
        XCTAssertEqual(cards.count, 1)
        XCTAssertEqual(cards.first?.statusVerb, "Completed")
        XCTAssertEqual(cards.first?.summary, "Ready")
    }

    func testExecutionPanelInspectionIdIsIgnoredUntilTaskProjectionSuppliesActiveRoot() {
        let m = chat()
        m.startNewSession("chat-1")
        MockURLProtocol.handler = { req in
            if req.url?.path.contains("/api/magician/v3/tasks/task-1") == true {
                return (response(for: req), jsonData(["task": [
                    "id": "task-1",
                    "status": "running",
                    "active_root_execution_id": "exec-authoritative",
                    "latest_root_execution_id": "exec-history",
                    "chat_session_id": "chat-1"
                ]]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        m.handleIncomingJSON(executionPanelDelta(
            taskId: "task-1",
            title: "My Task",
            status: "running",
            step: "Doing A",
            inspectionExecutionId: "exec-history"
        ))

        if case .taskStatus(let initial)? = m.messages.last?.type {
            XCTAssertNil(initial.activeExecutionIdForControls)
        } else {
            XCTFail("expected an initial task status")
        }
        waitUntil {
            guard case .taskStatus(let model)? = m.messages.last?.type else { return false }
            return model.activeExecutionIdForControls == "exec-authoritative"
        }
        XCTAssertEqual(m.currentRunExecutionId, "exec-authoritative")
    }

    // MARK: - escalation_resolved, voice origin, @task mentions (web parity)

    private func chatMessageReceived(type: String, id: String, exec: String? = nil, cid: String? = nil,
                                     direction: String = "assistant", voiceOrigin: Bool? = nil,
                                     presenceSessionId: String? = nil,
                                     renderable: Bool = false) -> String {
        var content: [String: Any] = ["type": type]
        if let exec = exec { content["execution_id"] = exec }
        if let cid = cid { content["correlation_id"] = cid }
        if renderable {
            content["text"] = "Approve?"; content["question"] = "Proceed?"
            content["options"] = [["id": "yes", "label": "Yes"]]
        }
        var message: [String: Any] = ["id": id, "session_id": "s1", "direction": direction, "content": content]
        if let v = voiceOrigin { message["voice_origin"] = v }
        if let presenceSessionId { message["presence_session_id"] = presenceSessionId }
        let event: [String: Any] = ["event_type": "ChatMessageReceived",
                                    "data": ["session_id": "s1", "message": message]]
        return String(data: try! JSONSerialization.data(withJSONObject: event), encoding: .utf8)!
    }

    /// Resolution matching under canonical HITL: the question id outranks the
    /// execution id. One planning execution may own several pending questions
    /// at once, so once BOTH cards carry a canonical correlation id, matching
    /// on the shared execution would fold a resolution into the wrong sibling
    /// — one answer resolving a question nobody answered. Execution-level
    /// matching survives only as the fallback for cards that carry no
    /// canonical id at all. (This test previously asserted the pre-canonical
    /// contract — exec match wins outright — and kept passing against the old
    /// code for a year of one-question-per-execution luck.)
    func testEscalationMatchesByExecOrCorrelation() {
        func content(_ e: String?, _ c: String?) -> ChatMessageContentData {
            var d: [String: Any] = ["type": "escalation"]
            if let e = e { d["execution_id"] = e }
            if let c = c { d["correlation_id"] = c }
            return try! JSONDecoder().decode(ChatMessageContentData.self, from: JSONSerialization.data(withJSONObject: d))
        }
        // Canonical id present on both sides: it alone decides.
        XCTAssertTrue(ChatViewModel.escalationMatches(content("eX", "c1"), content("eZ", "c1")))
        XCTAssertFalse(
            ChatViewModel.escalationMatches(content("e1", "c1"), content("e1", "cX")),
            "sibling questions on one execution must not cross-resolve")

        // No canonical id on one side (or both): the execution id is the
        // only identity left, and it still matches.
        XCTAssertTrue(ChatViewModel.escalationMatches(content("e1", nil), content("e1", nil)))
        XCTAssertTrue(ChatViewModel.escalationMatches(content("e1", "c1"), content("e1", nil)))
        XCTAssertFalse(ChatViewModel.escalationMatches(content("e1", nil), content("e2", nil)))

        XCTAssertFalse(ChatViewModel.escalationMatches(content("e1", "c1"), content("e2", "c2")))
        XCTAssertFalse(ChatViewModel.escalationMatches(content(nil, nil), content(nil, nil)))     // no shared id
    }

    func testEscalationResolvedFoldsIntoOpenCard() {
        let m = chat()
        m.handleIncomingJSON(chatMessageReceived(type: "escalation", id: "msg1", exec: "e1", cid: "c1", renderable: true))
        guard case .escalation(let c1)? = m.messages.last?.type else { return XCTFail("expected escalation card") }
        XCTAssertNotEqual(c1.resolved, true)
        let count = m.messages.count
        m.handleIncomingJSON(chatMessageReceived(type: "escalation_resolved", id: "msg2", exec: "e1", cid: "c1"))
        XCTAssertEqual(m.messages.count, count)   // folded into the open card, no duplicate
        guard case .escalation(let c2)? = m.messages.last?.type else { return XCTFail("expected escalation card") }
        XCTAssertEqual(c2.resolved, true)
    }

    func testVoiceOriginFlagSentAndUserEchoBadged() {
        let m = chat(); m.startNewSession("s1")
        let exp = expectation(description: "stream carries voice_origin")
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") {
                if let body = requestBody(req),
                   let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                   json["voice_origin"] as? Bool == true { exp.fulfill() }
                return (response(for: req), Data("data: {\"text\": \"Hi\"}\n".utf8))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.sendMessage("hello", viaVoice: true)
        wait(for: [exp], timeout: 3)
        XCTAssertEqual(m.messages.first(where: { $0.isUser })?.voiceOrigin, true)   // user echo badged
    }

    func testRealtimeVoiceAndTypedUserTranscriptsAppendOnce() {
        let m = chat()
        m.startNewSession("s1")

        m.handleIncomingJSON(chatMessageReceived(
            type: "text",
            id: "dictation-echo",
            direction: "user",
            voiceOrigin: true,
            renderable: true
        ))
        XCTAssertEqual(m.messages.filter { $0.id == "dictation-echo" }.count, 1)

        m.handleIncomingJSON(chatMessageReceived(
            type: "text",
            id: "realtime-user-turn",
            direction: "user",
            voiceOrigin: true,
            presenceSessionId: "voice-session-1",
            renderable: true
        ))
        let transcript = m.messages.first { $0.id == "realtime-user-turn" }
        XCTAssertEqual(transcript?.isUser, true)
        XCTAssertEqual(transcript?.text, "Approve?")
        XCTAssertEqual(transcript?.voiceOrigin, true)

        // A replay/reconnect of the same canonical event must not duplicate it.
        m.handleIncomingJSON(chatMessageReceived(
            type: "text",
            id: "realtime-user-turn",
            direction: "user",
            voiceOrigin: true,
            presenceSessionId: "voice-session-1",
            renderable: true
        ))
        XCTAssertEqual(m.messages.filter { $0.id == "realtime-user-turn" }.count, 1)
    }

    func testCompletedTasksBecomeComposerMentions() {
        let m = chat()
        MockURLProtocol.handler = { req in
            let u = req.url!.absoluteString
            if u.contains("/reference-catalog") { return (response(for: req), jsonData(["agents": [], "skills": []])) }
            if u.contains("/v3/tasks") {
                return (response(for: req), jsonData(["tasks": [
                    ["id": "t1", "title": "Done Task", "status": "completed"],
                    ["id": "t2", "title": "Running Task", "status": "running"]
                ]]))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.startNewSession("s1")   // currentSessionId didSet → fetchReferenceCatalog → tasks
        waitUntil { m.mentionItems.contains { $0.kind == .task } }
        XCTAssertEqual(m.mentionItems.filter { $0.kind == .task }.map(\.label), ["Done Task"])  // completed only
    }
}
