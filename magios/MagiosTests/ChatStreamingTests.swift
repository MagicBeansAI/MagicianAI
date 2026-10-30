import XCTest
@testable import Magician

/// Deep coverage of the SSE token-streaming assembly in ChatViewModel
/// (postMessageStreaming): tokens accumulate into one assistant message keyed by
/// the streamId, isThinking clears on the first token, and empty/error streams
/// leave no dangling assistant bubble.
final class ChatStreamingTests: XCTestCase {
    private final class SessionLifetime: NSObject, URLSessionDelegate, @unchecked Sendable {
        let invalidated: XCTestExpectation

        init(_ invalidated: XCTestExpectation) {
            self.invalidated = invalidated
        }

        func urlSession(_ session: URLSession, didBecomeInvalidWithError error: Error?) {
            invalidated.fulfill()
        }
    }

    private var deferredFixtures: [(ChatViewModel, URLSession, XCTestExpectation)] = []

    private func makeVM(deferred: Bool = false) -> ChatViewModel {
        guard deferred else {
            return ChatViewModel(networkSession: makeMockSession(chatCatalogFixture: true), connectsOnInit: false)
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ChatCatalogFixtureURLProtocol.self, DeferredMockURLProtocol.self]
        // Teardown owns this wait; keep it out of the test body's expectations.
        let invalidated = XCTestExpectation(description: "chat fixture session invalidated")
        let session = URLSession(configuration: configuration, delegate: SessionLifetime(invalidated), delegateQueue: nil)
        let model = ChatViewModel(networkSession: session, connectsOnInit: false)
        deferredFixtures.append((model, session, invalidated))
        return model
    }

    override func tearDown() async throws {
        let invalidated = await MainActor.run {
            let invalidated = deferredFixtures.map { $0.2 }
            for (model, session, _) in deferredFixtures {
                model.startNewSession(nil)
                model.disconnect()
                session.invalidateAndCancel()
            }
            deferredFixtures.removeAll()
            return invalidated
        }
        // Keep the strict handlers installed until cancellation finishes, so
        // a late backfill cannot escape into the following test's fixture.
        if !invalidated.isEmpty { await fulfillment(of: invalidated, timeout: 5) }
        await MainActor.run {
            MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
            DeferredMockURLProtocol.handler = nil
        }
        try await super.tearDown()
    }

    /// Build an SSE body: one `data: {"text": "…"}` frame per token.
    private func sse(_ tokens: [String]) -> Data {
        Data(tokens.map { "data: {\"text\": \"\($0)\"}\n" }.joined().utf8)
    }

    func testNewSessionSendKeepsHarnessChoiceMadeAtSendTime() {
        let keys = ["magios.chat.profile", "magios.chat.harnessEngine", "magios.chat.harnessModel"]
        let saved = keys.map { UserDefaults.standard.object(forKey: $0) }
        defer {
            for (key, value) in zip(keys, saved) {
                if let value { UserDefaults.standard.set(value, forKey: key) }
                else { UserDefaults.standard.removeObject(forKey: key) }
            }
        }

        let m = makeVM(deferred: true)
        m.selectedHarnessEngine = "pi"
        m.selectedHarnessModel = "default"
        m.selectedProfile = "profile-before-send"
        var finishSessionCreation: (() -> Void)?
        var sentPayload: [String: Any]?
        DeferredMockURLProtocol.handler = { request, completion in
            if request.url?.path.hasSuffix("/chat/new") == true {
                DispatchQueue.main.async {
                    finishSessionCreation = {
                        completion(.success((response(for: request), jsonData(["session": ["id": "new-session"]]))))
                    }
                }
            } else if request.url?.path.hasSuffix("/messages/stream") == true {
                let body = requestBody(request)
                let payload = body.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                DispatchQueue.main.async { sentPayload = payload }
                completion(.success((response(for: request), self.sse(["Done"]))))
            } else {
                completion(.success((response(for: request), jsonData([:]))))
            }
        }

        prepareChatModelCatalog(m)
        m.sendMessage("Use the selected engine")
        waitUntil(timeout: 5) { finishSessionCreation != nil }
        m.selectedHarnessEngine = "claude_code"
        m.selectedProfile = "profile-after-send"
        finishSessionCreation?()
        waitUntil(timeout: 5) { sentPayload != nil }

        XCTAssertEqual(sentPayload?["harness_engine"] as? String, "pi")
        XCTAssertEqual((sentPayload?["model_selection"] as? [String: String])?["model_id"], "fixture/chat")
        XCTAssertNil(sentPayload?["profile"])
    }

    func testStreamAssemblesTokensIntoOneAssistantMessage() {
        let m = makeVM(); m.startNewSession("s1")
        let body = sse(["Hello", ", ", "world"])
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req), body) }
            return (response(for: req), Data("{}".utf8))
        }
        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { m.messages.last?.isUser == false && m.messages.last?.text == "Hello, world" }
        XCTAssertEqual(m.messages.last?.text, "Hello, world")
        XCTAssertEqual(m.messages.last?.isUser, false)
        XCTAssertFalse(m.isThinking)
        // Exactly one assistant streamed bubble was created (not one per token).
        XCTAssertEqual(m.messages.filter { !$0.isUser && $0.text == "Hello, world" }.count, 1)
    }

    func testStreamClearsIsThinkingOnFirstToken() {
        let m = makeVM(); m.startNewSession("s1")
        let body = sse(["A"])
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req), body) }
            return (response(for: req), Data("{}".utf8))
        }
        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { !m.isThinking && m.messages.last?.text == "A" }
        XCTAssertFalse(m.isThinking)
    }

    func testLiveBubbleAndCanonicalStepAppearBeforeFirstToken() {
        let m = makeVM(deferred: true)
        m.startNewSession("s1")
        let streamStarted = expectation(description: "answer stream started")
        var finishAnswer: (() -> Void)?
        var capturedTurnId: String?

        DeferredMockURLProtocol.handler = { request, completion in
            if request.url?.path.hasSuffix("/messages/stream") == true {
                let body = requestBody(request)
                let payload = body.flatMap {
                    try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
                }
                let response = response(for: request)
                DispatchQueue.main.async {
                    capturedTurnId = payload?["chat_turn_id"] as? String
                    finishAnswer = {
                        completion(.success((response, self.sse(["Finished"]))))
                    }
                    streamStarted.fulfill()
                }
                return
            }

            let started = [
                "event_type": "step.started",
                "data": ["step_id": "research"],
            ] as [String: Any]
            if request.url?.path.hasSuffix("/events/stream") == true {
                let line = String(data: jsonData(started), encoding: .utf8)! + "\n"
                completion(.success((response(for: request), Data(line.utf8))))
                return
            }
            if request.url?.path.hasSuffix("/events") == true {
                completion(.success((response(for: request), jsonData([
                    "events": [started], "count": 1, "total": 1,
                ]))))
                return
            }
            completion(.success((response(for: request), Data("{}".utf8))))
        }

        prepareChatModelCatalog(m)
        m.sendMessage("research this")

        let placeholder = m.messages.last
        XCTAssertEqual(placeholder?.isUser, false)
        XCTAssertEqual(placeholder?.text, "")
        XCTAssertTrue(placeholder?.activityIsLive == true)
        XCTAssertTrue(placeholder?.chatTurnId?.hasPrefix("ios-chat-") == true)

        wait(for: [streamStarted], timeout: 2)
        waitUntil(timeout: 5) {
            m.messages.last?.text.isEmpty == true
                && m.messages.last?.activityRows.first?.label == "Step: research"
        }
        XCTAssertEqual(m.messages.last?.chatTurnId, capturedTurnId)

        finishAnswer?()
        waitUntil(timeout: 5) { m.messages.last?.text == "Finished" }
    }

    func testEmptyStreamLeavesNoAssistantBubble() {
        let m = makeVM(); m.startNewSession("s1")
        // No `data:` frames → nothing streamed; the placeholder must be dropped.
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req), Data("event: ping\n".utf8)) }
            return (response(for: req), Data("{}".utf8))
        }
        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { !m.isThinking }
        XCTAssertEqual(m.messages.last?.text, "hi")        // the user echo is the last message
        XCTAssertEqual(m.messages.last?.isUser, true)
    }

    func testHTTPErrorStreamLeavesNoAssistantBubble() {
        let m = makeVM(); m.startNewSession("s1")
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req, status: 500), Data()) }
            return (response(for: req), Data("{}".utf8))
        }
        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { !m.isThinking }
        XCTAssertEqual(m.messages.last?.text, "hi")
        XCTAssertFalse(m.messages.contains { !$0.isUser && $0.text.isEmpty })  // no empty placeholder left
    }

    func testStreamIgnoresNonDataAndMalformedFrames() {
        let m = makeVM(); m.startNewSession("s1")
        // Interleave a comment line, a non-JSON data line, and an empty-text token.
        let body = Data([
            "event: token\n",
            "data: not-json\n",
            #"data: {"text": "Good"}"# + "\n",
            #"data: {"text": ""}"# + "\n",
            #"data: {"text": " day"}"# + "\n"
        ].joined().utf8)
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req), body) }
            return (response(for: req), Data("{}".utf8))
        }
        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { m.messages.last?.text == "Good day" }
        XCTAssertEqual(m.messages.last?.text, "Good day")   // malformed/empty frames skipped
    }

    func testStreamingTurnSendsCorrelationAndShowsCanonicalActivity() {
        let m = makeVM(); m.startNewSession("s1")
        var capturedTurnId: String?
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/messages/stream") == true {
                if let body = requestBody(req),
                   let payload = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                    capturedTurnId = payload["chat_turn_id"] as? String
                    XCTAssertEqual(payload["source_surface"] as? String, "ios")
                    XCTAssertEqual(payload["continue_on_disconnect"] as? Bool, true)
                }
                return (response(for: req), self.sse(["Finished"]))
            }
            if req.url?.path.hasSuffix("/events/stream") == true {
                let events = [
                    #"{"event_type":"step.started","data":{"step_id":"research"}}"#,
                    #"{"event_type":"step.completed","data":{"step_id":"research"}}"#
                ].joined(separator: "\n") + "\n"
                return (response(for: req), Data(events.utf8))
            }
            if req.url?.path.hasSuffix("/events") == true {
                return (response(for: req), jsonData([
                    "events": [
                        ["event_type": "step.started", "data": ["step_id": "research"]],
                        ["event_type": "step.completed", "data": ["step_id": "research"]]
                    ],
                    "count": 2,
                    "total": 2,
                    "chat_turn_id": capturedTurnId ?? ""
                ]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        prepareChatModelCatalog(m)
        m.sendMessage("research this")
        waitUntil(timeout: 5) {
            m.messages.last?.text == "Finished"
                && m.messages.last?.activityRows.first?.status == .done
        }

        XCTAssertTrue(capturedTurnId?.hasPrefix("ios-chat-") == true)
        XCTAssertEqual(m.messages.last?.chatTurnId, capturedTurnId)
        XCTAssertEqual(m.messages.last?.activityRows.first?.label, "Step complete: research")
    }

    // MARK: Stream end vs. the server's turn state

    /// Stream, and queue whose `turn_state` names the stream's own turn.
    private func streamThenQueue(
        stream: @escaping () throws -> Data,
        turnState: @escaping (String) -> Any?
    ) -> () -> String? {
        var turnId: String?
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/messages/stream") == true {
                if let body = requestBody(req),
                   let payload = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                    turnId = payload["chat_turn_id"] as? String
                }
                return (response(for: req), try stream())
            }
            if req.url?.path.hasSuffix("/queue") == true {
                var body: [String: Any] = ["queued": [], "active": false, "session_id": "s1"]
                if let state = turnState(turnId ?? "") { body["turn_state"] = state }
                return (response(for: req), jsonData(body))
            }
            return (response(for: req), Data("{}".utf8))
        }
        return { turnId }
    }

    func testDroppedStreamKeepsTheBubbleLiveWhileTheServerRunsTheTurn() throws {
        let m = makeVM(); m.startNewSession("s1")
        let turnId = streamThenQueue(
            stream: { throw URLError(.networkConnectionLost) },
            turnState: { ["chat_turn_id": $0, "status": "running", "started_at": 1] as [String: Any] })

        prepareChatModelCatalog(m)
        m.sendMessage("research this")
        waitUntil(timeout: 5) { !m.isThinking }
        let bubble = try XCTUnwrap(m.messages.last)
        XCTAssertFalse(bubble.isUser, "the bubble survives the dropped stream")
        XCTAssertEqual(bubble.chatTurnId, turnId())
        XCTAssertTrue(bubble.activityIsLive)
        XCTAssertTrue(m.serverRunning)

        // The reply arrives over the realtime feed and takes the bubble's place.
        m.handleIncomingObject([
            "event_type": "ChatMessageReceived",
            "data": [
                "session_id": "s1",
                "message": [
                    "id": "a1", "session_id": "s1", "direction": "assistant",
                    "content": ["type": "text", "text": "Found it"],
                    "chat_turn_id": turnId() ?? ""
                ]
            ]
        ])
        XCTAssertEqual(m.messages.last?.id, "a1")
        XCTAssertEqual(m.messages.filter { !$0.isUser && $0.chatTurnId == turnId() }.count, 1)
    }

    func testCancelledStreamShowsStoppedFromTheTurnState() {
        let m = makeVM(); m.startNewSession("s1")
        _ = streamThenQueue(
            stream: { Data("event: done\ndata: {\"cancelled\":true}\n\n".utf8) },
            turnState: { [
                "chat_turn_id": $0, "status": "stopped", "started_at": 1,
                "ended_at": 2, "reason": "user_stop"
            ] as [String: Any] })

        prepareChatModelCatalog(m)
        m.sendMessage("long task")
        waitUntil(timeout: 5) { m.messages.last?.turnStatusNotice == "Stopped" }
        XCTAssertFalse(m.isThinking)
        XCTAssertEqual(m.messages.last?.isUser, false)
        XCTAssertEqual(m.messages.last?.activityIsLive, false)
    }

    func testErrorFrameShowsTheFailedLineOnAnOlderServer() {
        let m = makeVM(); m.startNewSession("s1")
        _ = streamThenQueue(
            stream: {
                Data("event: error\ndata: {\"error\":\"provider down\"}\n\nevent: done\ndata: {}\n\n".utf8)
            },
            turnState: { _ in nil })

        prepareChatModelCatalog(m)
        m.sendMessage("hi")
        waitUntil(timeout: 5) { !m.isThinking }
        XCTAssertEqual(m.messages.last?.isUser, false)
        XCTAssertEqual(m.messages.last?.turnStatusNotice, ChatTurnState.failedNotice)
        XCTAssertEqual(m.messages.last?.text, "", "the error text is not shown as a reply")
    }

    func testAcceptAndPlanModesAreNamedOnTheWire() {
        let m = makeVM(); m.startNewSession("s1")
        var captured: [String: Any] = [:]
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/messages/stream") == true {
                if let body = requestBody(req),
                   let payload = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                    captured = payload
                }
                return (response(for: req), self.sse(["ok"]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        m.composerMode = .acceptInScope
        prepareChatModelCatalog(m)
        m.sendMessage("edit the file")
        waitUntil(timeout: 2) { captured["mode"] as? String == "accept_in_scope" }
        XCTAssertEqual(captured["mode"] as? String, "accept_in_scope")

        captured = [:]
        m.composerMode = .plan
        prepareChatModelCatalog(m)
        m.sendMessage("draft a plan")
        waitUntil(timeout: 2) { captured["mode"] as? String == "plan" }
        XCTAssertEqual(captured["mode"] as? String, "plan")

        captured = [:]
        m.composerMode = .ask
        prepareChatModelCatalog(m)
        m.sendMessage("just ask")
        waitUntil(timeout: 2) { captured["text"] as? String == "just ask" }
        XCTAssertNil(captured["mode"])
    }

    func testDoPermissionIsRememberedAcrossPlan() {
        let m = makeVM()
        XCTAssertEqual(m.composerMode, .ask)
        XCTAssertEqual(m.composerDoPermission, .ask)

        m.composerMode = .acceptInScope
        XCTAssertEqual(m.composerDoPermission, .acceptInScope)

        m.composerMode = .plan
        XCTAssertEqual(m.composerMode, .plan)
        XCTAssertEqual(m.composerDoPermission, .acceptInScope)

        m.composerMode = m.composerDoPermission.mode
        XCTAssertEqual(m.composerMode, .acceptInScope)
    }
}
