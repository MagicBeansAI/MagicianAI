import XCTest
import UIKit
import UniformTypeIdentifiers
@testable import Magician

final class ContextualAssistTests: XCTestCase {
    override func tearDown() {
        MockURLProtocol.handler = nil
        _ = TutorOverlayInbox.claimPendingToken()
        super.tearDown()
    }

    func testAllWritingActionsMapToBackendIntents() {
        let expected: [ContextualAssistAction: (String, String)] = [
            .rewrite: ("rewrite", "rewrite_selection"),
            .summarize: ("summarize", "summarize_selection"),
            .reply: ("draft_reply", "draft_reply_to_selection"),
            .shorten: ("shorten", "shorten_field_selection"),
            .clarify: ("clarify", "clarify_field_selection"),
            .continueWriting: ("continue_draft", "continue_after_selection"),
        ]
        for (action, mapping) in expected {
            let request = makeRequest(action: action)
            XCTAssertEqual(request.action.id, mapping.0)
            XCTAssertEqual(request.action.intent, mapping.1)
            XCTAssertEqual(request.routing.actionIntent, mapping.1)
            XCTAssertFalse(request.action.requiresScreenshot)
        }
    }

    func testRequestEncodingUsesBackendCamelCaseAndOmitsVisualContext() throws {
        let request = ContextualAssistRequest.make(
            action: .rewrite,
            text: "Hello",
            guidance: "  friendlier  ",
            sourceURL: URL(string: "https://example.com/article"),
            sessionKey: "contextual-writing:ios:test"
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any]
        )
        let action = try XCTUnwrap(object["action"] as? [String: Any])
        let routing = try XCTUnwrap(object["routing"] as? [String: Any])
        XCTAssertEqual(object["userPrompt"] as? String, "friendlier")
        XCTAssertEqual(action["opensHud"] as? Bool, false)
        XCTAssertNil(action["opensHUD"])
        XCTAssertEqual(routing["agentId"] as? String, "writing-assistant")
        XCTAssertEqual(routing["rootUrl"] as? String, "https://example.com/article")
        XCTAssertNil(object["visualContext"])
        XCTAssertNil(object["screenshot"])
    }

    func testRequestCapsTextWithoutBreakingUnicode() {
        let text = String(repeating: "🫘", count: ContextualAssistRequest.maximumTextCharacters + 10)
        let request = ContextualAssistRequest.make(
            action: .shorten,
            text: text,
            guidance: nil,
            sessionKey: "contextual-writing:ios:test"
        )
        XCTAssertEqual(request.context.contextText?.count, ContextualAssistRequest.maximumTextCharacters)
        XCTAssertEqual(request.context.contextText?.last, "🫘")
    }

    func testClientPostsAuthorizedCamelCaseRequestAndDecodesDraft() async throws {
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/magician/v2/contextual-writing/actions")
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
            let body = try XCTUnwrap(requestBody(request))
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            XCTAssertNotNil(object["userPrompt"])
            return (
                response(for: request),
                jsonData(["status": "draft_ready", "sessionId": "s1", "threadId": "t1", "draftText": "Better text"])
            )
        }

        let result = try await ContextualAssistClient(
            session: makeMockSession(),
            baseURL: URL(string: "https://example.test")!
        ).run(makeRequest(action: .clarify, guidance: "plain language"))

        XCTAssertEqual(result.status, "draft_ready")
        XCTAssertEqual(result.sessionID, "s1")
        XCTAssertEqual(result.threadID, "t1")
        XCTAssertEqual(result.draftText, "Better text")
    }

    func testClientCreatesForcedDurableSessionInRequestedThread() async throws {
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/magician/v2/contextual-writing/sessions")
            XCTAssertEqual(request.httpMethod, "POST")
            let body = try XCTUnwrap(requestBody(request))
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            XCTAssertEqual(object["threadId"] as? String, "brainstorming")
            XCTAssertEqual(object["agentId"] as? String, "")
            XCTAssertEqual(object["surface"] as? String, "thinking_map")
            XCTAssertEqual(object["featureMode"] as? String, "brainstorm")
            XCTAssertEqual(object["title"] as? String, "Brainstorm — Reversible decisions")
            return (
                response(for: request, status: 201),
                jsonData([
                    "status": "ready",
                    "sessionId": "loom-session-1",
                    "threadId": "brainstorming",
                    "sessionTitle": "Brainstorm — Reversible decisions",
                ])
            )
        }

        let result = try await ContextualAssistClient(
            session: makeMockSession(),
            baseURL: URL(string: "https://example.test")!
        ).createSession(.init(
            principal: "anonymous",
            workspace: "default",
            threadID: "brainstorming",
            agentID: "",
            surface: "thinking_map",
            featureMode: "brainstorm",
            sourceKey: "app:ios:thinking-map",
            title: "Brainstorm — Reversible decisions"
        ))

        XCTAssertEqual(result.sessionID, "loom-session-1")
        XCTAssertEqual(result.threadID, "brainstorming")
    }

    func testClientPrefersStructuredDetailsReason() async {
        MockURLProtocol.handler = { request in
            (response(for: request, status: 422), jsonData([
                "error": "generic",
                "details": ["reason": "Selection is empty"],
            ]))
        }
        do {
            _ = try await ContextualAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).run(makeRequest(action: .rewrite))
            XCTFail("Expected validation failure")
        } catch {
            XCTAssertEqual(error as? ContextualAssistClientError, .validation("Selection is empty"))
        }
    }

    func testClientClassifiesMissingBoundFeatureSessionAsStale() async {
        var request = makeRequest(action: .rewrite)
        request = ContextualAssistRequest(
            principal: request.principal,
            workspace: request.workspace,
            userPrompt: request.userPrompt,
            action: request.action,
            context: request.context,
            routing: .init(
                agentID: "",
                surface: "thinking_map",
                featureMode: "brainstorm",
                sourceKind: "app",
                sourceKey: "app:ios:thinking-map",
                sessionKey: "thinking-map:test",
                threadID: "brainstorming",
                sessionID: "stale-session",
                sessionTitle: "Brainstorm — Test",
                rootURL: nil,
                targetTextKind: "thinking_graph",
                actionIntent: "continue_thinking",
                personality: "active"
            ),
            chatTurnID: nil
        )

        for status in [404, 409, 410] {
            MockURLProtocol.handler = { urlRequest in
                (response(for: urlRequest, status: status), jsonData([
                    "details": ["reason": "Thinking Map session no longer exists"],
                ]))
            }

            do {
                _ = try await ContextualAssistClient(
                    session: makeMockSession(),
                    baseURL: URL(string: "https://example.test")!
                ).run(request)
                XCTFail("Expected stale-session failure for HTTP \(status)")
            } catch {
                XCTAssertEqual(
                    error as? ContextualAssistClientError,
                    .staleSession("Thinking Map session no longer exists")
                )
            }
        }
    }

    func testClientKeepsOrdinaryContextualNotFoundAsValidationFailure() async {
        MockURLProtocol.handler = { request in
            (response(for: request, status: 404), jsonData([
                "details": ["reason": "Selection source was not found"],
            ]))
        }

        do {
            _ = try await ContextualAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).run(makeRequest(action: .rewrite))
            XCTFail("Expected validation failure")
        } catch {
            XCTAssertEqual(
                error as? ContextualAssistClientError,
                .validation("Selection source was not found")
            )
        }
    }

    func testWebpageSummaryRequestUsesDirectBrowserExecutionAndExplicitFetchPrompt() throws {
        let request = try WebpageAssistExecutionRequest.make(
            operation: .summarizePage,
            url: URL(string: "https://www.example.com/guide?chapter=2")!,
            guidance: nil
        )
        XCTAssertEqual(request.title, "Summarize example.com")
        XCTAssertEqual(request.uiThreadID, "general")
        XCTAssertTrue(request.skipPlanning)
        XCTAssertTrue(request.internalTask)
        XCTAssertEqual(request.envMode, "browser")
        XCTAssertTrue(request.initialMessage.contains("execute it now without creating or proposing a plan"))
        XCTAssertTrue(request.initialMessage.contains("connection_mode=\"headless\" first"))
        XCTAssertTrue(request.initialMessage.contains("retry with connection_mode=\"headed\""))
        XCTAssertTrue(request.initialMessage.contains("actual page contents, not the URL string"))
        XCTAssertTrue(request.initialMessage.contains("https://www.example.com/guide?chapter=2"))

        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any]
        )
        XCTAssertEqual(object["ui_thread_id"] as? String, "general")
        XCTAssertEqual(object["skip_planning"] as? Bool, true)
        XCTAssertEqual(object["internal"] as? Bool, true)
        XCTAssertEqual(object["env_mode"] as? String, "browser")
        XCTAssertNotNil(object["initial_message"])
        XCTAssertNil(object["description"])
        XCTAssertNil(object["approved"])
    }

    func testWebpageAskRequestTrimsGuidanceAndRequiresPageEvidence() throws {
        let request = try WebpageAssistExecutionRequest.make(
            operation: .askSam,
            url: URL(string: "https://example.com/research")!,
            guidance: "  Are these claims supported?  "
        )
        XCTAssertEqual(request.title, "Ask Sam about example.com")
        XCTAssertTrue(request.initialMessage.contains("User request: Are these claims supported?"))
        XCTAssertTrue(request.initialMessage.contains("distinguish page facts from your own inference"))
        XCTAssertTrue(request.initialMessage.contains("if neither browser mode can access the page"))
        XCTAssertTrue(request.initialMessage.contains("connection_mode=\"headless\" first"))
        XCTAssertTrue(request.initialMessage.contains("retry with connection_mode=\"headed\""))
    }

    func testWebpageAskRejectsEmptyGuidanceAndNonWebURL() {
        XCTAssertThrowsError(try WebpageAssistExecutionRequest.make(
            operation: .askSam,
            url: URL(string: "https://example.com")!,
            guidance: " \n "
        )) { error in
            XCTAssertEqual(
                error as? WebpageAssistClientError,
                .validation("Add a question or instruction for Sam.")
            )
        }
        XCTAssertThrowsError(try WebpageAssistExecutionRequest.make(
            operation: .summarizePage,
            url: URL(fileURLWithPath: "/tmp/page.html"),
            guidance: nil
        )) { error in
            XCTAssertEqual(
                error as? WebpageAssistClientError,
                .validation("Summarize Page and Ask Sam require an HTTP or HTTPS webpage.")
            )
        }
    }

    func testWebpageClientStartsTaskBackedDirectExecutionAndReturnsExactTask() async throws {
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/magician/v2/executions")
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
            let body = try XCTUnwrap(requestBody(request))
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            XCTAssertEqual(object["title"] as? String, "Summarize example.com")
            XCTAssertEqual(object["skip_planning"] as? Bool, true)
            XCTAssertEqual(object["internal"] as? Bool, true)
            XCTAssertEqual(object["env_mode"] as? String, "browser")
            XCTAssertNotNil(object["initial_message"])
            return (
                response(for: request, status: 202),
                jsonData([
                    "execution_id": "run-web-1",
                    "execution": [
                        "id": "run-web-1",
                        "task_id": "task-web-1",
                    ],
                    "initial_message_enqueued": true,
                    "skip_planning": true,
                ])
            )
        }

        let task = try await WebpageAssistClient(
            session: makeMockSession(),
            baseURL: URL(string: "https://example.test")!
        ).start(
            operation: .summarizePage,
            url: URL(string: "https://example.com/article")!
        )
        XCTAssertEqual(task.id, "task-web-1")
        XCTAssertEqual(task.executionID, "run-web-1")
    }

    func testWebpageClientRejectsResponseThatDidNotUseDirectExecution() async {
        MockURLProtocol.handler = { request in
            (
                response(for: request, status: 202),
                jsonData([
                    "execution_id": "run-web-1",
                    "execution": ["id": "run-web-1", "task_id": "task-web-1"],
                    "initial_message_enqueued": true,
                    "skip_planning": false,
                ])
            )
        }

        do {
            _ = try await WebpageAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).start(
                operation: .summarizePage,
                url: URL(string: "https://example.com/article")!
            )
            XCTFail("Expected malformed response")
        } catch {
            XCTAssertEqual(error as? WebpageAssistClientError, .malformedResponse)
        }
    }

    func testWebpageClientRejectsResponseThatDidNotEnqueueGoal() async {
        MockURLProtocol.handler = { request in
            (
                response(for: request, status: 200),
                jsonData([
                    "execution_id": "run-web-1",
                    "execution": ["id": "run-web-1", "task_id": "task-web-1"],
                    "initial_message_enqueued": false,
                    "skip_planning": true,
                ])
            )
        }

        do {
            _ = try await WebpageAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).start(
                operation: .summarizePage,
                url: URL(string: "https://example.com/article")!
            )
            XCTFail("Expected malformed response")
        } catch {
            XCTAssertEqual(error as? WebpageAssistClientError, .malformedResponse)
        }
    }

    func testWebpageClientPreservesStructuredStartFailure() async {
        MockURLProtocol.handler = { request in
            (
                response(for: request, status: 409),
                jsonData(["details": ["reason": "Task is already running"]])
            )
        }
        do {
            _ = try await WebpageAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).start(
                operation: .summarizePage,
                url: URL(string: "https://example.com/article")!
            )
            XCTFail("Expected validation failure")
        } catch {
            XCTAssertEqual(
                error as? WebpageAssistClientError,
                .validation("Task is already running")
            )
        }
    }

    func testWebpageClientRejectsMalformedCreateResponse() async {
        MockURLProtocol.handler = { request in
            (
                response(for: request, status: 202),
                jsonData([
                    "execution_id": "run-without-task",
                    "execution": ["id": "run-without-task"],
                    "initial_message_enqueued": true,
                    "skip_planning": true,
                ])
            )
        }
        do {
            _ = try await WebpageAssistClient(
                session: makeMockSession(),
                baseURL: URL(string: "https://example.test")!
            ).start(
                operation: .summarizePage,
                url: URL(string: "https://example.com")!
            )
            XCTFail("Expected malformed response")
        } catch {
            XCTAssertEqual(error as? WebpageAssistClientError, .malformedResponse)
        }
    }

    func testAttributedTextTakesPrecedenceDuringShareLoading() async {
        let item = NSExtensionItem()
        item.attributedContentText = NSAttributedString(string: "Selected words")
        item.attachments = [NSItemProvider(object: "fallback" as NSString)]
        let content = await ShareAssistContentLoader.load(from: [item])
        XCTAssertEqual(content, .text("Selected words", sourceURL: nil))
    }

    func testBareSharedURLLoadsAsWebpage() async {
        let url = URL(string: "https://example.com/guide")!
        let provider = NSItemProvider()
        provider.registerDataRepresentation(forTypeIdentifier: UTType.url.identifier, visibility: .all) { completion in
            completion(url.absoluteString.data(using: .utf8), nil)
            return nil
        }
        let item = NSExtensionItem()
        item.attachments = [provider]
        let content = await ShareAssistContentLoader.load(from: [item])
        XCTAssertEqual(content, .webpage(url))
    }

    func testImageNormalizationCapsLongestDimension() throws {
        let source = UIGraphicsImageRenderer(size: CGSize(width: 4_000, height: 2_000)).image { context in
            UIColor.systemPurple.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4_000, height: 2_000))
        }
        let normalized = try XCTUnwrap(ShareAssistContentLoader.normalizeImage(
            try XCTUnwrap(source.pngData()),
            maximumDimension: 1_000
        ))
        XCTAssertEqual(normalized.width, 1_000)
        XCTAssertEqual(normalized.height, 500)
        XCTAssertNotNil(UIImage(data: normalized.pngData))
    }

    func testPendingTutorTokenIsOneShot() {
        let token = "pending-token"
        let store = UserDefaults(suiteName: TutorOverlayInbox.appGroup) ?? .standard
        store.set(token, forKey: "pending_tutor_overlay_token")
        XCTAssertEqual(TutorOverlayInbox.claimPendingToken(), token)
        XCTAssertNil(TutorOverlayInbox.claimPendingToken())
    }

    func testPendingTutorTokenOnlyClearsWhenItMatches() {
        let store = UserDefaults(suiteName: TutorOverlayInbox.appGroup) ?? .standard
        store.set("keep-me", forKey: "pending_tutor_overlay_token")
        TutorOverlayInbox.clearPendingToken(ifMatching: "different")
        XCTAssertEqual(TutorOverlayInbox.claimPendingToken(), "keep-me")

        store.set("clear-me", forKey: "pending_tutor_overlay_token")
        TutorOverlayInbox.clearPendingToken(ifMatching: "clear-me")
        XCTAssertNil(TutorOverlayInbox.claimPendingToken())
    }

    private func makeRequest(
        action: ContextualAssistAction,
        guidance: String? = "be clear"
    ) -> ContextualAssistRequest {
        ContextualAssistRequest.make(
            action: action,
            text: "Some shared text",
            guidance: guidance,
            sessionKey: "contextual-writing:ios:test"
        )
    }
}
