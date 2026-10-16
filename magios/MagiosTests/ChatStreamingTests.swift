import XCTest
@testable import Magician

/// Deep coverage of the SSE token-streaming assembly in ChatViewModel
/// (postMessageStreaming): tokens accumulate into one assistant message keyed by
/// the streamId, isThinking clears on the first token, and empty/error streams
/// leave no dangling assistant bubble.
final class ChatStreamingTests: XCTestCase {
    private func makeVM() -> ChatViewModel {
        ChatViewModel(networkSession: makeMockSession(), connectsOnInit: false)
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        DeferredMockURLProtocol.handler = nil
        super.tearDown()
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

        let m = ChatViewModel(networkSession: makeDeferredMockSession(), connectsOnInit: false)
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

        m.sendMessage("Use the selected engine")
        waitUntil(timeout: 5) { finishSessionCreation != nil }
        m.selectedHarnessEngine = "claude_code"
        m.selectedProfile = "profile-after-send"
        finishSessionCreation?()
        waitUntil(timeout: 5) { sentPayload != nil }

        XCTAssertEqual(sentPayload?["harness_engine"] as? String, "pi")
        XCTAssertEqual(sentPayload?["harness_model"] as? String, "default")
        XCTAssertEqual(sentPayload?["profile"] as? String, "profile-before-send")
    }

    func testStreamAssemblesTokensIntoOneAssistantMessage() {
        let m = makeVM(); m.startNewSession("s1")
        let body = sse(["Hello", ", ", "world"])
        MockURLProtocol.handler = { req in
            if req.url!.absoluteString.contains("/messages/stream") { return (response(for: req), body) }
            return (response(for: req), Data("{}".utf8))
        }
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
        m.sendMessage("hi")
        waitUntil(timeout: 5) { !m.isThinking && m.messages.last?.text == "A" }
        XCTAssertFalse(m.isThinking)
    }

    func testLiveBubbleAndCanonicalStepAppearBeforeFirstToken() {
        let m = ChatViewModel(networkSession: makeDeferredMockSession(), connectsOnInit: false)
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

        m.sendMessage("research this")
        waitUntil(timeout: 5) {
            m.messages.last?.text == "Finished"
                && m.messages.last?.activityRows.first?.status == .done
        }

        XCTAssertTrue(capturedTurnId?.hasPrefix("ios-chat-") == true)
        XCTAssertEqual(m.messages.last?.chatTurnId, capturedTurnId)
        XCTAssertEqual(m.messages.last?.activityRows.first?.label, "Step complete: research")
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
        m.sendMessage("edit the file")
        waitUntil(timeout: 2) { captured["mode"] as? String == "accept_in_scope" }
        XCTAssertEqual(captured["mode"] as? String, "accept_in_scope")

        captured = [:]
        m.composerMode = .plan
        m.sendMessage("draft a plan")
        waitUntil(timeout: 2) { captured["mode"] as? String == "plan" }
        XCTAssertEqual(captured["mode"] as? String, "plan")

        captured = [:]
        m.composerMode = .ask
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
