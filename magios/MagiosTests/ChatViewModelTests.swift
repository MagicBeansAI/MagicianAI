import XCTest
import UIKit
@testable import Magician

/// Core ChatViewModel logic: transcript mutation, optimistic send, and canonical
/// chat HITL payloads. Built with `connectsOnInit: false` to skip the WS.
final class ChatViewModelTests: XCTestCase {
    private func makeVM() -> ChatViewModel {
        ChatViewModel(networkSession: makeMockSession(), connectsOnInit: false)
    }

    private func escalationContent(_ overrides: [String: Any] = [:]) -> ChatMessageContentData {
        var payload: [String: Any] = [
            "type": "escalation",
            "execution_id": "exec-1",
            "pause_state_id": "pause-1",
            "escalation_type": "confirmation",
            "input_type": "confirmation",
            "question": "Continue?",
            "options": [
                [
                    "id": "approve", "label": "Approve", "requires_input": false,
                    "action": ["type": "respond_confirmation", "confirmed": true]
                ],
                [
                    "id": "deny", "label": "Deny", "requires_input": false,
                    "action": ["type": "respond_confirmation", "confirmed": false]
                ]
            ],
            "resolved": false
        ]
        overrides.forEach { payload[$0.key] = $0.value }
        return try! JSONDecoder().decode(ChatMessageContentData.self, from: jsonData(payload))
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        super.tearDown()
    }

    func testStartNewSessionResetsTranscript() {
        let m = makeVM()
        m.messages.append(ChatMessage(id: "x", isUser: true, text: "hi", type: .text))
        m.startNewSession("s1")
        XCTAssertEqual(m.messages.count, 1)
        XCTAssertFalse(m.messages[0].isUser)          // greeting is an assistant message
        XCTAssertEqual(m.currentSessionIdValue, "s1")
    }

    func testDeleteMessageRemovesLocally() {
        let m = makeVM()
        m.messages = [ChatMessage(id: "a", isUser: true, text: "one", type: .text),
                      ChatMessage(id: "b", isUser: false, text: "two", type: .text)]
        m.deleteMessage("a")
        XCTAssertEqual(m.messages.map(\.id), ["b"])
    }

    func testClearMessagesKeepsOnlyGreeting() {
        let m = makeVM()
        m.messages = [ChatMessage(id: "a", isUser: true, text: "x", type: .text),
                      ChatMessage(id: "b", isUser: false, text: "y", type: .text)]
        m.clearMessages()
        XCTAssertEqual(m.messages.count, 1)
        XCTAssertFalse(m.messages[0].isUser)
    }

    @MainActor
    func testLockedVoiceTutorSpeaksAndDoesNotPresentOrSend() {
        let m = makeVM()
        m.guidedFlowScreenIsLocked = { true }
        var spoken: [String] = []
        var presentations = 0
        m.guidedFlowSpeaker = { spoken.append($0) }
        m.tutorPresenter = { _, _, _ in presentations += 1 }
        MockURLProtocol.handler = { _ in
            XCTFail("locked Tutor must not send a request")
            return (HTTPURLResponse(), Data())
        }

        m.sendMessage("Tutor Quick blackboard explain recursion", viaVoice: true)

        XCTAssertEqual(m.queueNoticeMessage, "Please unlock your screen to use Tutor.")
        XCTAssertEqual(spoken, ["Please unlock your screen to use Tutor."])
        XCTAssertEqual(presentations, 0)
        XCTAssertEqual(m.messages.count, 1)
    }

    @MainActor
    func testUnlockedVoiceTutorQuickPresentsSourceFreeBlackboardAndAutoStarts() {
        let m = makeVM()
        m.guidedFlowScreenIsLocked = { false }
        var presentedQuestion: String?
        var presentedImage: UIImage?
        var didAutoStart = false
        m.tutorPresenter = { question, image, autoStart in
            presentedQuestion = question
            presentedImage = image
            didAutoStart = autoStart
        }

        m.sendMessage("Tutor Quick blackboard explain recursion", viaVoice: true)
        waitForMainQueue()

        XCTAssertEqual(presentedQuestion, "#quick blackboard explain recursion")
        XCTAssertNil(presentedImage)
        XCTAssertTrue(didAutoStart)
        XCTAssertEqual(m.messages.count, 1)
    }

    @MainActor
    func testVoiceTutorLockBetweenAdmissionAndPresentationNeverStarts() {
        let m = makeVM()
        var lockChecks = 0
        m.guidedFlowScreenIsLocked = {
            defer { lockChecks += 1 }
            return lockChecks > 0
        }
        var spoken: [String] = []
        var presentations = 0
        m.guidedFlowSpeaker = { spoken.append($0) }
        m.tutorPresenter = { _, _, _ in presentations += 1 }

        m.sendMessage("Tutor blackboard explain recursion", viaVoice: true)
        waitForMainQueue()

        XCTAssertEqual(lockChecks, 2)
        XCTAssertEqual(spoken, ["Please unlock your screen to use Tutor."])
        XCTAssertEqual(presentations, 0)
        XCTAssertEqual(m.messages.count, 1)
    }

    @MainActor
    func testVoiceScreenTutorIsRejectedWithoutPresentationOrChatFallback() {
        let m = makeVM()
        m.guidedFlowScreenIsLocked = { false }
        var spoken: [String] = []
        var presentations = 0
        m.guidedFlowSpeaker = { spoken.append($0) }
        m.tutorPresenter = { _, _, _ in presentations += 1 }

        m.sendMessage("Tutor screen explain this graph", viaVoice: true)

        XCTAssertEqual(
            m.queueNoticeMessage,
            "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
        )
        XCTAssertEqual(
            spoken,
            ["Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."]
        )
        XCTAssertEqual(presentations, 0)
        XCTAssertEqual(m.messages.count, 1)
    }

    @MainActor
    func testVoiceAppCopilotIsRejectedWithoutOrdinaryChatFallback() {
        let m = makeVM()
        m.guidedFlowScreenIsLocked = { false }
        var spoken: String?
        m.guidedFlowSpeaker = { spoken = $0 }

        m.sendMessage("App Copilot show me how to create a note", viaVoice: true)

        XCTAssertEqual(m.queueNoticeMessage, "App Copilot isn't available on this device yet.")
        XCTAssertEqual(spoken, "App Copilot isn't available on this device yet.")
        XCTAssertEqual(m.messages.count, 1)
    }

    func testDeleteQueuedRemovesOnlyAfterBackendSuccess() {
        let m = makeVM()
        m.startNewSession("s1")
        m.queuedMessages = [QueuedMessage(id: "q1", text: "a"), QueuedMessage(id: "q2", text: "b")]
        let exp = expectation(description: "delete queued")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "DELETE", req.url!.path.hasSuffix("/queue/q1") { exp.fulfill() }
            return (response(for: req), jsonData(["deleted": true]))
        }

        m.deleteQueued("q1")

        wait(for: [exp], timeout: 2)
        waitUntil { !m.queueMutationInFlight }
        XCTAssertEqual(m.queuedMessages.map(\.id), ["q2"])
        XCTAssertEqual(m.queueNoticeMessage, "Queued message removed.")
    }

    func testDeleteQueuedFailureKeepsCardAndSurfacesError() {
        let m = makeVM()
        m.startNewSession("s1")
        m.queuedMessages = [QueuedMessage(id: "q1", text: "a")]
        MockURLProtocol.handler = { req in
            if req.httpMethod == "DELETE" {
                return (response(for: req, status: 500), jsonData(["error": "Queue storage unavailable."]))
            }
            return (response(for: req), jsonData(["queued": [["id": "q1", "text": "a"]]]))
        }

        m.deleteQueued("q1")

        waitUntil { !m.queueMutationInFlight && m.queueErrorMessage != nil }
        XCTAssertEqual(m.queuedMessages.map(\.id), ["q1"])
        XCTAssertEqual(m.queueErrorMessage, "Queue storage unavailable.")
    }

    func testClearQueuedUsesCollectionEndpointAndClearsAllCards() {
        let m = makeVM()
        m.startNewSession("s1")
        m.queuedMessages = [QueuedMessage(id: "q1", text: "a"), QueuedMessage(id: "q2", text: "b")]
        let exp = expectation(description: "clear queue")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "DELETE", req.url!.path.hasSuffix("/sessions/s1/queue") { exp.fulfill() }
            return (response(for: req), jsonData(["cleared": 2]))
        }

        m.clearQueued()

        wait(for: [exp], timeout: 2)
        waitUntil { !m.queueMutationInFlight }
        XCTAssertTrue(m.queuedMessages.isEmpty)
        XCTAssertEqual(m.queueNoticeMessage, "Cleared 2 queued messages.")
    }

    func testQueuedMessageDecodesBackendTimestamp() throws {
        let queued = try JSONDecoder().decode(QueuedMessage.self, from: jsonData([
            "id": "q1", "text": "later", "queued_at": 1_752_000_000_000
        ]))
        XCTAssertEqual(queued.queuedAt, 1_752_000_000_000)
    }

    func testChatMessageDecodesDurableTurnCorrelation() throws {
        let raw = try JSONDecoder().decode(ChatMessageRawData.self, from: jsonData([
            "id": "assistant-1",
            "session_id": "session-1",
            "direction": "assistant",
            "content": ["type": "text", "text": "Done"],
            "chat_turn_id": "turn-1"
        ]))

        XCTAssertEqual(raw.chatTurnId, "turn-1")
    }

    func testPersistedTaskStatusProjectsCompleteCardData() throws {
        let raw = try decodeRawMessage(chatTurnId: "turn-report", content: [
            "type": "task_status_update",
            "task_id": "task-1",
            "status": "completed",
            "display_label": "Prepare report",
            "summary": "The **report** is ready.",
            "execution_id": "exec-1",
            "ui_thread_id": "finance",
            "synthesis_pending": false,
            "speech_tts": "The report is ready.",
            "output_files": [[
                "type": "file",
                "source": ["type": "task_output", "task_id": "task-1"],
                "relative_path": "reports/final.pdf",
                "display_name": "Final report",
                "mime_type": "application/pdf",
                "size": 4096
            ]]
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        guard case .taskStatus(let task) = projected.type else {
            return XCTFail("expected task status card")
        }
        XCTAssertEqual(task.taskId, "task-1")
        XCTAssertEqual(task.title, "Prepare report")
        XCTAssertEqual(task.statusVerb, "Completed")
        XCTAssertTrue(task.isTerminal)
        XCTAssertEqual(task.summary, "The **report** is ready.")
        XCTAssertEqual(task.executionId, "exec-1")
        XCTAssertEqual(task.uiThreadId, "finance")
        XCTAssertEqual(task.outputFiles.first?.relativePath, "reports/final.pdf")
        XCTAssertEqual(task.outputFiles.first?.source?.taskId, "task-1")
        XCTAssertEqual(projected.chatTurnId, "turn-report")
    }

    func testRunningTaskKeepsOnlyItsExplicitSpawningTurnLive() throws {
        let reply = try decodeRawMessage(id: "reply", chatTurnId: "turn-1", content: [
            "type": "text", "text": "I started it."
        ])
        let running = try decodeRawMessage(id: "task-running", chatTurnId: "turn-1", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "running", "display_label": "Prepare report"
        ])
        let unrelated = try decodeRawMessage(id: "other-task", content: [
            "type": "task_status_update", "task_id": "task-2",
            "status": "running", "display_label": "Uncorrelated legacy task"
        ])
        let projected = ChatViewModel.projectedMessages(from: [reply, running, unrelated])

        XCTAssertEqual(ChatViewModel.latestActiveTaskTurnId(projected), "turn-1")
    }

    func testLaterUncorrelatedTaskStatusInheritsOnlySameTaskTurnAndTerminalSettlesIt() throws {
        let running = try decodeRawMessage(id: "task-running", chatTurnId: "turn-1", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "running", "display_label": "Prepare report"
        ])
        let completed = try decodeRawMessage(id: "task-complete", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "completed", "display_label": "Prepare report"
        ])
        let projected = ChatViewModel.projectedMessages(from: [running, completed])

        XCTAssertEqual(projected.count, 1)
        XCTAssertEqual(projected[0].chatTurnId, "turn-1")
        XCTAssertNil(ChatViewModel.latestActiveTaskTurnId(projected))
    }

    func testTaskCorrelationDoesNotStealTextActivityAnchor() throws {
        let reply = try decodeRawMessage(id: "reply", chatTurnId: "turn-1", content: [
            "type": "text", "text": "I started it."
        ])
        let running = try decodeRawMessage(id: "task-running", chatTurnId: "turn-1", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "running", "display_label": "Prepare report"
        ])

        let anchored = ChatViewModel.retainLatestActivityAnchorPerTurn(
            ChatViewModel.projectedMessages(from: [reply, running])
        )

        XCTAssertEqual(anchored.first(where: { $0.id == "reply" })?.chatTurnId, "turn-1")
        XCTAssertEqual(anchored.first(where: { $0.id == "task-running" })?.chatTurnId, "turn-1")
    }

    func testPersistedTaskUpdatesCoalesceToLatestTerminalCard() throws {
        let created = try decodeRawMessage(id: "task-created", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "created", "display_label": "Prepare report"
        ])
        let reply = try decodeRawMessage(id: "reply", content: [
            "type": "text", "text": "I started it."
        ])
        let completed = try decodeRawMessage(id: "task-completed", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "completed", "display_label": "Prepare report",
            "summary": "Done"
        ])

        let projected = ChatViewModel.projectedMessages(from: [created, reply, completed])
        let taskCards = projected.compactMap { message -> TaskStatusModel? in
            if case .taskStatus(let task) = message.type { return task }
            return nil
        }
        XCTAssertEqual(taskCards.count, 1)
        XCTAssertEqual(taskCards.first?.status, "completed")
        XCTAssertEqual(projected.last?.id, "task-completed")
    }

    func testPersistedRichToolResultRetainsTextAndFileBlocks() throws {
        let raw = try decodeRawMessage(content: [
            "type": "rich_tool_result",
            "tool_name": "research",
            "summary": "Found the answer",
            "content_blocks": [
                ["type": "text", "text": "Supporting detail"],
                [
                    "type": "file",
                    "source": ["type": "session_output"],
                    "relative_path": "notes/result.md",
                    "display_name": "Result",
                    "mime_type": "text/markdown",
                    "size": 128
                ]
            ]
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertTrue(projected.text.contains("Action completed: research"))
        XCTAssertTrue(projected.text.contains("Supporting detail"))
        XCTAssertEqual(projected.richBlocks.first?.relativePath, "notes/result.md")
    }

    func testPersistedTextUsesStructuredPresentationPlainTextWhenAvailable() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Presentation answer",
                "blocks": [["kind": "text", "text": "Presentation answer"]]
            ],
            content: [
            "type": "text",
            "text": "Presentation answer"
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertEqual(projected.text, "Presentation answer")
        XCTAssertNotNil(projected.structuredResponse)
    }

    func testPersistedTextFallsBackWhenPresentationHasWrongSchema() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "legacy.schema",
                "version": 1,
                "plain_text": "Presentation answer"
            ],
            content: [
            "type": "text",
            "text": "Legacy answer"
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertEqual(projected.text, "Legacy answer")
    }

    func testPersistedTextFallsBackWhenPresentationIsOversized() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": String(repeating: "x", count: 40_000)
            ],
            content: [
            "type": "text",
            "text": "Legacy answer"
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertEqual(projected.text, "Legacy answer")
    }

    func testPersistedTextUsesStructuredPresentationWhenCanonicalMatchesAfterWhitespaceNormalization() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Legacy answer with spacing",
                "blocks": [["kind": "text", "text": "Legacy answer with spacing"]]
            ],
            content: [
            "type": "text",
            "text": "Legacy    answer\nwith   spacing"
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertEqual(projected.text, "Legacy answer with spacing")
        XCTAssertNotNil(projected.structuredResponse)
    }

    func testPersistedTextFallsBackWhenPresentationDiffersFromCanonicalText() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Different answer"
            ],
            content: [
            "type": "text",
            "text": "Legacy answer"
        ])

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertEqual(projected.text, "Legacy answer")
    }

    func testPersistedPlanReplyAndUnknownMessageRemainVisible() throws {
        let reply = try decodeRawMessage(direction: "user", content: [
            "type": "text",
            "text": "Use option B",
            "plan_reply": [
                "task_id": "task-1", "task_title": "Launch plan",
                "question_id": "question-1", "question_text": "Which option?"
            ]
        ])
        let unknown = try decodeRawMessage(id: "future", content: ["type": "future_card"])

        XCTAssertEqual(ChatViewModel.projectedMessage(from: reply).planReplyContext, "Launch plan")
        let fallback = ChatViewModel.projectedMessage(from: unknown)
        guard case .system(let text) = fallback.type else {
            return XCTFail("unknown content must have a visible fallback")
        }
        XCTAssertTrue(text.contains("future card"))
    }

    func testUnmatchedResolvedEscalationRendersSummaryNotice() throws {
        let resolved = try decodeRawMessage(content: [
            "type": "escalation_resolved",
            "correlation_id": "request-1",
            "summary": "Approval was completed elsewhere."
        ])

        let projected = ChatViewModel.projectedMessages(from: [resolved])
        guard case .system(let text)? = projected.first?.type else {
            return XCTFail("expected a visible resolution notice")
        }
        XCTAssertEqual(text, "Approval was completed elsewhere.")
    }

    func testLatestAssistantMessageOwnsTurnActivityDisclosure() {
        var first = ChatMessage(id: "a1", isUser: false, text: "Working", type: .text)
        first.chatTurnId = "turn-1"
        var final = ChatMessage(id: "a2", isUser: false, text: "Done", type: .text)
        final.chatTurnId = "turn-1"

        let anchored = ChatViewModel.retainLatestActivityAnchorPerTurn([first, final])

        XCTAssertNil(anchored[0].chatTurnId)
        XCTAssertEqual(anchored[1].chatTurnId, "turn-1")
    }

    func testActivityResponseUsesCanonicalEventCoalescer() {
        let rows = ChatViewModel.activityRowsFromResponse(jsonData([
            "events": [
                ["event_type": "step.started", "data": ["step_id": "research"]],
                ["event_type": "step.completed", "data": ["step_id": "research"]]
            ],
            "count": 2,
            "total": 2,
            "chat_turn_id": "turn-1"
        ]))

        XCTAssertEqual(rows?.count, 1)
        XCTAssertEqual(rows?.first?.label, "Step complete: research")
        XCTAssertEqual(rows?.first?.status, .done)
    }

    func testActivityResponseSortsLifecycleAndDeduplicatesFanoutEvents() {
        let completed = wrappedActivityEvent(
            type: "step.completed",
            eventId: "event-complete",
            timestampMs: 200,
            payload: ["step_id": "research"]
        )
        let started = wrappedActivityEvent(
            type: "step.started",
            eventId: "event-start",
            timestampMs: 100,
            payload: ["step_id": "research"]
        )
        let rows = ChatViewModel.activityRowsFromResponse(jsonData([
            "events": [completed, started, started],
            "count": 3,
            "total": 3,
            "chat_turn_id": "turn-1"
        ]))

        XCTAssertEqual(rows?.count, 1)
        XCTAssertEqual(rows?.first?.label, "Step complete: research")
        XCTAssertEqual(rows?.first?.status, .done)
    }

    func testSessionHistoryHydratesActivityForMatchingAssistantTurn() {
        let m = makeVM()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req), jsonData([
                    "session": [
                        "id": "s1", "principal": "anonymous", "workspace": "default",
                        "agent_id": "personal-assistant", "ui_thread_id": "general",
                        "title": "History", "status": "active",
                        "created_at": 1, "updated_at": 2
                    ],
                    "messages": [
                        [
                            "id": "u1", "session_id": "s1", "direction": "user",
                            "content": ["type": "text", "text": "Research this"],
                            "chat_turn_id": "turn-1"
                        ],
                        [
                            "id": "a1", "session_id": "s1", "direction": "assistant",
                            "content": ["type": "text", "text": "Done"],
                            "chat_turn_id": "turn-1"
                        ]
                    ]
                ]))
            }
            if req.url?.path.hasSuffix("/turns/turn-1/events") == true {
                return (response(for: req), jsonData([
                    "events": [
                        ["event_type": "tool.call.started", "data": [
                            "call_id": "c1", "tool_name": "search"
                        ]],
                        ["event_type": "tool.call.succeeded", "data": [
                            "call_id": "c1", "tool_name": "search", "duration_ms": 40
                        ]]
                    ],
                    "count": 2,
                    "total": 2,
                    "chat_turn_id": "turn-1"
                ]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        m.loadSession("s1")
        waitUntil { m.messages.last?.id == "a1" }
        m.loadActivityIfNeeded(messageId: "a1", chatTurnId: "turn-1")
        waitUntil { m.messages.last?.activityRows.first?.status == .done }

        XCTAssertEqual(m.messages.last?.chatTurnId, "turn-1")
        XCTAssertEqual(m.messages.last?.activityRows.first?.label, "search returned")
    }

    @MainActor
    func testSendMessageAppendsOptimisticEchoAndLiveResponsePlaceholder() throws {
        let m = makeVM()
        m.startNewSession("s1")
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        let initialMessageCount = m.messages.count

        m.sendMessage("Hello world")

        // Sending also mounts the assistant row immediately so activity can
        // arrive before the first response token. The echo precedes that row.
        let appended = Array(m.messages.dropFirst(initialMessageCount))
        XCTAssertEqual(appended.count, 2)
        let echo = try XCTUnwrap(appended.first)
        XCTAssertEqual(echo.text, "Hello world")
        XCTAssertTrue(echo.isUser)
        let reply = try XCTUnwrap(appended.last)
        XCTAssertFalse(reply.isUser)
        XCTAssertEqual(reply.text, "")
        XCTAssertNotNil(reply.chatTurnId)
        XCTAssertTrue(reply.activityIsLive)
        XCTAssertTrue(m.isThinking)
    }

    private func wrappedActivityEvent(
        type: String,
        eventId: String,
        timestampMs: Int,
        payload: [String: Any]
    ) -> [String: Any] {
        var eventPayload = payload
        eventPayload["event_id"] = eventId
        eventPayload["timestamp_ms"] = timestampMs
        return [
            "event_type": "AgentEvent",
            "data": [
                "event": [
                    "event_type": type,
                    "agent_id": "personal-assistant",
                    "payload": eventPayload
                ]
            ]
        ]
    }

    func testRichPresentationDoesNotRetainLegacyArtifactCards() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [
                    ["kind": "markdown", "text": "Completed"],
                    [
                        "kind": "artifacts",
                        "items": [
                            [
                                "label": "result.md",
                                "artifact_id": "magician-artifact:session:notes/result.md"
                            ]
                        ]
                    ]
                ]
            ],
            content: [
                "type": "rich_tool_result",
                "tool_name": "research",
                "summary": "Completed",
                "content_blocks": [
                    [
                        "type": "file",
                        "relative_path": "notes/result.md",
                        "display_name": "result.md",
                        "mime_type": "text/markdown",
                        "size": 128
                    ]
                ]
            ]
        )

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertNotNil(projected.structuredResponse)
        XCTAssertTrue(projected.richBlocks.isEmpty)
    }

    func testResolvedEscalationRetainsAcceptedStructuredPresentation() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Request resolved",
                "blocks": [["kind": "markdown", "text": "Request resolved"]]
            ],
            content: [
                "type": "escalation_resolved",
                "summary": "Request resolved"
            ]
        )

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertNotNil(projected.structuredResponse)
    }

    func testUserMessagesCannotRenderStructuredPresentation() throws {
        let raw = try decodeRawMessage(
            direction: "user",
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "User message",
                "blocks": [["kind": "markdown", "text": "User message"]]
            ],
            content: ["type": "text", "text": "User message"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testAssistantAttachmentRetainsAcceptedStructuredPresentation() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "report.pdf",
                "blocks": [["kind": "artifacts", "items": [[
                    "label": "report.pdf",
                    "artifact_id": "magician-artifact:session:report.pdf",
                    "size": 512
                ]]]]
            ],
            content: [
                "type": "attachment",
                "filename": "report.pdf",
                "label": "Quarterly report"
            ]
        )

        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertNotNil(projected.structuredResponse)
        guard case .attachment = projected.type else {
            return XCTFail("attachment should retain its semantic message type")
        }
    }

    func testPresentationRejectsNonStringTableCellWithoutDroppingTheMessage() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [[
                    "kind": "table",
                    "columns": [["key": "status", "label": "Status"]],
                    "rows": [["status": true]]
                ]]
            ],
            content: ["type": "text", "text": "Completed"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testPresentationRejectsOverlongMultibyteValue() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [[
                    "kind": "key_values",
                    "items": [["label": "Status", "value": String(repeating: "é", count: 1_025)]]
                ]]
            ],
            content: ["type": "text", "text": "Completed"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testPresentationRejectsArtifactsOutsideServerContract() throws {
        let invalidArtifacts: [[String: Any]] = [
            ["label": "report", "href": "javascript:alert(1)"],
            ["label": "report", "artifact_id": String(repeating: "a", count: 161)],
            ["label": "report", "size": Int(Int32.max) + 1],
            ["label": "report", "source": "legacy-path"]
        ]

        for artifact in invalidArtifacts {
            let raw = try decodeRawMessage(
                presentation: [
                    "schema": "magician.structured_response",
                    "version": 1,
                    "plain_text": "Completed",
                    "blocks": [["kind": "artifacts", "items": [artifact]]]
                ],
                content: ["type": "text", "text": "Completed"]
            )

            XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
        }
    }

    func testPresentationRejectsOverlongArtifactActionIdentifier() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [["kind": "text", "text": "Completed"]],
                "actions": [[
                    "kind": "open_artifact",
                    "label": "Open",
                    "artifact_id": String(repeating: "a", count: 161)
                ]]
            ],
            content: ["type": "text", "text": "Completed"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testPresentationRejectsInvalidStructuredMeta() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [["kind": "markdown", "text": "Completed"]],
                "meta": ["confidence": 1.5]
            ],
            content: ["type": "text", "text": "Completed"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testPresentationRejectsInvalidModelContext() throws {
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [["kind": "markdown", "text": "Completed"]],
                "model_context": [
                    "summary": "",
                    "privacy": "public"
                ]
            ],
            content: ["type": "text", "text": "Completed"]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testPresentationRejectsSerializedEnvelopeOver64KiB() throws {
        let maximumField = String(repeating: "x", count: 32_768)
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": maximumField,
                "blocks": [["kind": "markdown", "text": maximumField]]
            ],
            content: ["type": "text", "text": maximumField]
        )

        XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
    }

    func testRealtimeSocketURLRequestsStructuredPresentation() throws {
        let url = try XCTUnwrap(ChatViewModel.realtimeSocketURL(baseURL: "wss://example.invalid"))
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        XCTAssertEqual(
            components.queryItems?.first(where: { $0.name == "supports_structured_presentation" })?.value,
            "true"
        )
    }

    private func decodeRawMessage(
        id: String = "message-1",
        direction: String = "assistant",
        chatTurnId: String? = nil,
        presentation: [String: Any]? = nil,
        content: [String: Any]
    ) throws -> ChatMessageRawData {
        var payload: [String: Any] = [
            "id": id,
            "session_id": "session-1",
            "direction": direction,
            "content": content
        ]
        if let chatTurnId { payload["chat_turn_id"] = chatTurnId }
        if let presentation { payload["presentation"] = presentation }
        return try JSONDecoder().decode(ChatMessageRawData.self, from: jsonData(payload))
    }

    func testSendMessageIgnoresBlank() {
        let m = makeVM()
        m.startNewSession("s1")
        let before = m.messages.count
        m.sendMessage("   \n ")
        XCTAssertEqual(m.messages.count, before)
    }

    func testClarificationTargetKeepsQuestionResponderAndExecutionDistinct() {
        let content = escalationContent([
            "execution_id": "planexec-7",
            "pause_state_id": "question-1",
            "request_id": "clarification_responder:task-1",
            "escalation_type": "clarification",
            "input_type": "text",
            "question": "Which account?",
            "options": [["id": "respond", "label": "Respond", "requires_input": true]]
        ])

        XCTAssertEqual(content.hitlCorrelationId, "question-1")
        XCTAssertEqual(content.hitlTarget?.source, "clarification")
        XCTAssertEqual(content.hitlTarget?.taskId, "task-1")
        XCTAssertEqual(content.hitlTarget?.executionId, "planexec-7")
        XCTAssertEqual(content.hitlTarget?.inputType, "text")
    }

    func testRespondToClarificationUsesCanonicalTypedPayload() {
        let m = makeVM()
        let content = escalationContent([
            "execution_id": "planexec-7",
            "pause_state_id": "question-1",
            "request_id": "clarification_responder:task-1",
            "escalation_type": "clarification",
            "input_type": "text",
            "question": "Which account?",
            "options": [["id": "respond", "label": "Respond", "requires_input": true]]
        ])
        let exp = expectation(description: "clarification")
        let completion = expectation(description: "clarification completion")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.path.hasSuffix("/hitl/question-1/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["source"] as? String, "clarification")
                XCTAssertEqual(json["input_type"] as? String, "text")
                XCTAssertEqual(json["task_id"] as? String, "task-1")
                XCTAssertEqual(json["execution_id"] as? String, "planexec-7")
                XCTAssertEqual(json["correlation_id"] as? String, "question-1")
                let value = json["value"] as? [String: Any]
                XCTAssertEqual(value?["type"] as? String, "text")
                XCTAssertEqual(value?["value"] as? String, "Use billing")
                exp.fulfill()
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.respondToEscalation(content: content, submission: .response(.text("Use billing"))) { success in
            XCTAssertTrue(success)
            completion.fulfill()
        }
        wait(for: [exp, completion], timeout: 2)
        XCTAssertNil(m.escalationResponseErrorMessage)
    }

    func testRespondToNativeEscalationKeepsExecutionOwnedContract() {
        let m = makeVM()
        let content = escalationContent()
        let requestObserved = expectation(description: "native escalation request")
        let completion = expectation(description: "native escalation completion")
        MockURLProtocol.handler = { req in
            let body = requestBody(req)
            let json = body.flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            }
            XCTAssertTrue(req.url?.path.hasSuffix("/hitl/pause-1/respond") == true)
            XCTAssertEqual(json?["source"] as? String, "escalation")
            XCTAssertEqual(json?["input_type"] as? String, "confirmation")
            XCTAssertEqual(json?["execution_id"] as? String, "exec-1")
            XCTAssertNil(json?["task_id"])
            let value = json?["value"] as? [String: Any]
            XCTAssertEqual(value?["type"] as? String, "confirmation")
            XCTAssertEqual(value?["confirmed"] as? Bool, true)
            requestObserved.fulfill()
            return (response(for: req), jsonData(["accepted": true]))
        }

        m.respondToEscalation(content: content, submission: .response(.confirmation(true))) { success in
            XCTAssertTrue(success)
            completion.fulfill()
        }

        wait(for: [requestObserved, completion], timeout: 2)
    }

    func testAlreadyResolvedCanonicalResponseIsSoftSuccess() {
        let m = makeVM()
        let completion = expectation(description: "already resolved completion")
        MockURLProtocol.handler = { req in
            (
                response(for: req, status: 409),
                jsonData(["accepted": false, "reason": "already_resolved"])
            )
        }

        m.respondToEscalation(
            content: escalationContent(),
            submission: .response(.confirmation(true))
        ) { success in
            XCTAssertTrue(success)
            completion.fulfill()
        }

        wait(for: [completion], timeout: 2)
        XCTAssertNil(m.escalationResponseErrorMessage)
    }

    func testRespondToEscalationFailureKeepsCardActionable() {
        let m = makeVM()
        let content = escalationContent()
        let completion = expectation(description: "rejected escalation completion")
        MockURLProtocol.handler = { req in
            (response(for: req, status: 409), jsonData(["error": "Request is no longer active."]))
        }

        m.respondToEscalation(content: content, submission: .response(.confirmation(true))) { success in
            XCTAssertFalse(success)
            completion.fulfill()
        }

        wait(for: [completion], timeout: 2)
        XCTAssertNil(m.respondingEscalationID)
        XCTAssertEqual(m.escalationResponseErrorMessage, "Request is no longer active.")
    }

    func testRespondToEscalationIgnoresEmptyCorrelationId() {
        let m = makeVM()
        let content = escalationContent([
            "pause_state_id": "",
            "execution_id": ""
        ])
        MockURLProtocol.handler = { _ in XCTFail("should not fire"); return (HTTPURLResponse(), Data()) }
        m.respondToEscalation(
            content: content,
            submission: .response(.choice(selectedId: "x", otherValue: nil))
        )
        waitForMainQueue()
        XCTAssertEqual(
            m.escalationResponseErrorMessage,
            "This request is missing its canonical response identity."
        )
    }

    func testRequiresInputChoiceCannotSubmitWithoutDetails() {
        let content = escalationContent([
            "input_type": "choice",
            "options": [[
                "id": "other", "label": "Other", "requires_input": true,
                "action": ["type": "respond_choice"]
            ]]
        ])
        let option = try! XCTUnwrap(content.options?.first)

        XCTAssertNil(ChatHitlResponseComposer.compose(inputType: "choice", option: option))
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(inputType: "choice", option: option, text: "Use staging"),
            .response(.choice(selectedId: "other", otherValue: "Use staging"))
        )
    }

    // P3 Task 3.9: the backend's spec drives masking and exactness.

    func testSensitiveSpecDecodesAndDrivesTheRenderKind() throws {
        let json = """
        {"type":"text","placeholder":"6 digits","sensitive":{"kind":"otp","provenance":"heuristic","one_time":true,"collection_deadline_ms":1700000180000}}
        """.data(using: .utf8)!
        let schema = try JSONDecoder().decode(ChatEscalationInputSchemaData.self, from: json)
        XCTAssertEqual(schema.sensitive?.kind, "otp")
        XCTAssertEqual(schema.sensitive?.oneTime, true)
        XCTAssertEqual(schema.sensitive?.collectionDeadlineMs, 1_700_000_180_000)
        XCTAssertTrue(schema.sensitive!.isExpired(now: Date(timeIntervalSince1970: 1_700_000_180)))
        XCTAssertFalse(schema.sensitive!.isExpired(now: Date(timeIntervalSince1970: 1_700_000_179)))

        XCTAssertEqual(hitlRenderKind(inputType: "text", sensitiveKind: "otp"), "otp")
        XCTAssertEqual(hitlRenderKind(inputType: "text", sensitiveKind: "password"), "password")
        XCTAssertEqual(hitlRenderKind(inputType: "guidance", sensitiveKind: "other"), "password")
        XCTAssertEqual(hitlRenderKind(inputType: "text", sensitiveKind: "login_identifier"), "text", "an identifier stays readable")
        XCTAssertEqual(hitlRenderKind(inputType: "choice", sensitiveKind: "password"), "choice", "a decision is never masked")
        XCTAssertEqual(hitlRenderKind(inputType: "otp", sensitiveKind: nil), "otp")
        XCTAssertEqual(hitlRenderKind(inputType: "text", sensitiveKind: nil), "text")

        let form = try JSONDecoder().decode(ChatSensitiveSpecData.self, from: """
        {"fields":[{"id":"pw","kind":"password"},{"id":"user","kind":"login_identifier"}],"provenance":"form_schema"}
        """.data(using: .utf8)!)
        XCTAssertEqual(form.fieldKind("pw"), "password")
        XCTAssertEqual(form.fieldKind("user"), "login_identifier")
        XCTAssertNil(form.fieldKind("city"))
        XCTAssertTrue(hitlFieldIsMasked(sensitiveKind: "password"))
        XCTAssertTrue(hitlFieldIsMasked(sensitiveKind: "otp"))
        XCTAssertFalse(hitlFieldIsMasked(sensitiveKind: "login_identifier"))
        XCTAssertFalse(hitlFieldIsMasked(sensitiveKind: nil))
    }

    func testOneTimeCodeAndSensitiveTextPostExactStrings() {
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(inputType: "otp", text: "007123"),
            .response(.password("007123")),
            "a code rides the password value shape, leading zero intact"
        )
        XCTAssertNil(ChatHitlResponseComposer.compose(inputType: "otp", text: ""))
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(inputType: "text", text: " 007123 ", sensitive: true),
            .response(.text(" 007123 ")),
            "a text-typed ask the backend classified keeps its type and its exact bytes"
        )
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(inputType: "text", text: " city ", sensitive: false),
            .response(.text("city"))
        )
        XCTAssertFalse(ChatHitlResponseComposer.needsCanonicalAttentionFallback(inputType: "otp", options: []))
    }

    func testTextAndExternalActionResponseCompositionUseTypedWireShapes() {
        let guidanceOption = EscalationOptionData(
            id: "guidance",
            label: "Provide guidance",
            requiresInput: true,
            action: EscalationOptionActionData(type: "respond_external_action")
        )

        XCTAssertEqual(
            ChatHitlResponseComposer.compose(inputType: "text", text: "  billing  "),
            .response(.text("billing"))
        )
        XCTAssertNil(
            ChatHitlResponseComposer.compose(
                inputType: "external_action",
                option: guidanceOption
            )
        )
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(
                inputType: "external_action",
                option: guidanceOption,
                text: "I completed the login"
            ),
            .response(.externalActionCompleted(guidance: "I completed the login"))
        )
    }

    func testConfirmationCompositionUsesServerActionInsteadOfOptionName() {
        let misleadingLegacyOption = EscalationOptionData(
            id: "yes",
            label: "Absolutely",
            requiresInput: false
        )
        let explicitNegativeOption = EscalationOptionData(
            id: "custom-action",
            label: "Do not proceed",
            requiresInput: false,
            action: EscalationOptionActionData(type: "respond_confirmation", confirmed: false)
        )
        let continuationOption = EscalationOptionData(
            id: "anything",
            label: "Try again",
            requiresInput: false,
            action: EscalationOptionActionData(type: "continue_execution")
        )

        XCTAssertNil(
            ChatHitlResponseComposer.compose(
                inputType: "confirmation",
                option: misleadingLegacyOption
            )
        )
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(
                inputType: "confirmation",
                option: explicitNegativeOption
            ),
            .response(.confirmation(false))
        )
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(
                inputType: "confirmation",
                option: continuationOption
            ),
            .continueExecution
        )
    }

    func testChatEscalationRetainsSchemaAndFileMultiplicity() {
        let content = escalationContent([
            "input_type": "file_path",
            "input_schema": [
                "type": "file_path",
                "placeholder": "/reports/input.csv",
                "multiple": false,
                "filter": "*.csv"
            ],
            "options": [[
                "id": "respond", "label": "Respond", "requires_input": true,
                "action": ["type": "respond_choice"]
            ]]
        ])

        XCTAssertEqual(content.inputSchema?.placeholder, "/reports/input.csv")
        XCTAssertEqual(content.inputSchema?.multiple, false)
        XCTAssertEqual(content.inputSchema?.filter, "*.csv")
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(
                inputType: "file_path",
                text: "/tmp/one.csv,/tmp/two.csv",
                allowsMultipleFiles: false
            ),
            .response(.filePath(["/tmp/one.csv,/tmp/two.csv"]))
        )
        XCTAssertEqual(
            ChatHitlResponseComposer.compose(
                inputType: "file_path",
                text: "/tmp/one.csv,/tmp/two.csv",
                allowsMultipleFiles: true
            ),
            .response(.filePath(["/tmp/one.csv", "/tmp/two.csv"]))
        )
    }

    func testRawSchemaValueOptionCannotRejectCanonicalChatMessage() throws {
        let content = escalationContent([
            "input_type": "choice",
            "input_schema": [
                "type": "choice",
                "options": [["value": "prod", "label": "Production"]]
            ],
            "options": [[
                "id": "prod", "label": "Production", "requires_input": false,
                "action": ["type": "respond_choice"]
            ]]
        ])

        XCTAssertEqual(content.inputSchema?.type, "choice")
        XCTAssertEqual(content.options?.first?.id, "prod")
        XCTAssertEqual(content.options?.first?.action?.type, "respond_choice")
    }

    func testValidationReaskRotatesCardIdentityAndPreservesCorrectionContext() throws {
        let m = makeVM()
        let original = escalationContent([
            "input_type": "text",
            "question": "Which account?"
        ])
        m.messages = [
            ChatMessage(id: "escalation-1", isUser: false, text: "", type: .escalation(original))
        ]
        let reaskCompletion = expectation(description: "reask completion")
        MockURLProtocol.handler = { req in
            XCTAssertTrue(req.url?.path.hasSuffix("/hitl/pause-1/respond") == true)
            return (
                response(for: req),
                jsonData([
                    "resumed": false,
                    "status": "reask_required",
                    "message": "Use a named account",
                    "question": "Which named account?",
                    "hint": "For example, billing",
                    "previous_answer": "that one",
                    "pause_state_id": "pause-2"
                ])
            )
        }
        m.respondToEscalation(content: original, submission: .response(.text("that one"))) { success in
            XCTAssertFalse(success)
            reaskCompletion.fulfill()
        }
        wait(for: [reaskCompletion], timeout: 2)

        guard case .escalation(let updated) = try XCTUnwrap(m.messages.first).type else {
            return XCTFail("expected updated escalation")
        }
        XCTAssertEqual(updated.hitlCorrelationId, "pause-2")
        XCTAssertEqual(updated.question, "Which named account?")
        XCTAssertEqual(updated.hint, "For example, billing")
        XCTAssertEqual(updated.previousAnswer, "that one")
        XCTAssertEqual(m.escalationResponseErrorMessage, "More information is needed: Use a named account")

        let acceptedCompletion = expectation(description: "accepted corrected response")
        MockURLProtocol.handler = { req in
            XCTAssertTrue(req.url?.path.hasSuffix("/hitl/pause-2/respond") == true)
            return (response(for: req), jsonData(["accepted": true]))
        }
        m.respondToEscalation(content: updated, submission: .response(.text("billing"))) { success in
            XCTAssertTrue(success)
            acceptedCompletion.fulfill()
        }
        wait(for: [acceptedCompletion], timeout: 2)
        XCTAssertNil(m.escalationResponseErrorMessage)
    }

    func testMalformedReaskAndUnacceptedSuccessResponseStayActionable() {
        XCTAssertNil(ChatViewModel.canonicalReaskData(
            from: ["status": "reask_required", "question": "Try again"],
            fallbackQuestion: "Original"
        ))

        let m = makeVM()
        let completion = expectation(description: "unaccepted response")
        MockURLProtocol.handler = { req in
            (response(for: req), jsonData(["resumed": false, "status": "waiting"]))
        }
        m.respondToEscalation(
            content: escalationContent(),
            submission: .response(.confirmation(true))
        ) { success in
            XCTAssertFalse(success)
            completion.fulfill()
        }
        wait(for: [completion], timeout: 2)
        XCTAssertNotNil(m.escalationResponseErrorMessage)
    }

    func testLegacyOptionDrivenCardsUseCanonicalAttentionFallback() {
        let legacy = EscalationOptionData(id: "yes", label: "Yes", requiresInput: false)
        let canonical = EscalationOptionData(
            id: "approve",
            label: "Approve",
            requiresInput: false,
            action: EscalationOptionActionData(type: "respond_confirmation", confirmed: true)
        )

        XCTAssertTrue(ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: "confirmation",
            options: [legacy]
        ))
        XCTAssertFalse(ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: "confirmation",
            options: [canonical]
        ))
        XCTAssertFalse(ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: "text",
            options: [legacy]
        ))
        XCTAssertTrue(ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: "guidance",
            options: [legacy],
            inputTypeIsAuthoritative: false
        ))
        XCTAssertTrue(ChatHitlResponseComposer.needsCanonicalAttentionFallback(
            inputType: "form",
            options: []
        ))
    }

    func testMaxIterationsContinuationUsesDedicatedFreshBudgetEndpoint() {
        let m = makeVM()
        let content = escalationContent([
            "execution_id": "exec-budget",
            "pause_state_id": "pause-budget",
            "escalation_type": "max_iterations",
            "input_type": "confirmation",
            "options": [[
                "id": "continue", "label": "Keep Trying", "requires_input": false,
                "action": ["type": "continue_execution"]
            ]]
        ])
        let requestObserved = expectation(description: "agentic continuation request")
        let completion = expectation(description: "agentic continuation completion")
        MockURLProtocol.handler = { req in
            XCTAssertTrue(
                req.url?.path.hasSuffix(
                    "/executions/exec-budget/execution/agentic-continue"
                ) == true
            )
            XCTAssertFalse(req.url?.path.contains("/hitl/") == true)
            let body = requestBody(req).flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            }
            XCTAssertEqual(body?["pause_state_id"] as? String, "pause-budget")
            requestObserved.fulfill()
            return (response(for: req), jsonData(["status": "continued"]))
        }

        m.respondToEscalation(content: content, submission: .continueExecution) { success in
            XCTAssertTrue(success)
            completion.fulfill()
        }

        wait(for: [requestObserved, completion], timeout: 2)
        XCTAssertNil(m.escalationResponseErrorMessage)
    }

    func testUserRequestUsesRequestIdWhileNativeEscalationUsesPauseId() {
        let userRequest = escalationContent([
            "request_id": "request-9",
            "pause_state_id": "pause-unused"
        ])
        let native = escalationContent()

        XCTAssertEqual(userRequest.hitlTarget?.source, "user_request")
        XCTAssertEqual(userRequest.hitlCorrelationId, "request-9")
        XCTAssertEqual(native.hitlTarget?.source, "escalation")
        XCTAssertEqual(native.hitlCorrelationId, "pause-1")
    }

    func testResolutionDoesNotMatchSiblingClarificationInSameExecution() {
        let first = escalationContent([
            "execution_id": "planexec-7",
            "pause_state_id": "question-1",
            "request_id": "clarification_responder:task-1",
            "escalation_type": "clarification",
            "input_type": "text"
        ])
        let second = escalationContent([
            "execution_id": "planexec-7",
            "pause_state_id": "question-2",
            "request_id": "clarification_responder:task-1",
            "escalation_type": "clarification",
            "input_type": "text"
        ])

        XCTAssertFalse(ChatViewModel.escalationMatches(first, second))
        XCTAssertTrue(ChatViewModel.escalationMatches(first, first))
    }

    @MainActor
    func testPrimaryStopCoordinatesTheSessionCancellationWithItsActiveExecution() {
        let coordinator = ExecutionControlCoordinator()
        let m = ChatViewModel(
            networkSession: makeMockSession(),
            connectsOnInit: false,
            executionControlCoordinator: coordinator
        )
        m.startNewSession("chat-1")
        let stopped = expectation(description: "session run stopped")
        MockURLProtocol.handler = { req in
            if req.url?.path.contains("/api/magician/v3/tasks/task-1") == true {
                return (response(for: req), self.activeTaskDetail())
            }
            if req.httpMethod == "DELETE", req.url?.path.hasSuffix("/chat/sessions/chat-1/run") == true {
                stopped.fulfill()
                return (response(for: req), jsonData([:]))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.handleIncomingJSON(executionPanelDelta(taskId: "task-1", inspectionExecutionId: "exec-history"))
        waitUntil { m.currentRunExecutionId == "exec-active" }
        m.isThinking = true

        m.cancelRun()

        wait(for: [stopped], timeout: 2)
        waitUntil { coordinator.snapshot(for: "exec-active").revision == 1 }
        XCTAssertNil(coordinator.snapshot(for: "exec-active").busyAction)
        XCTAssertFalse(m.isThinking)
        XCTAssertNil(m.currentRunExecutionId)
    }

    @MainActor
    func testPrimaryStopSurfacesCoordinationConflictWithoutBypassingIt() {
        let coordinator = ExecutionControlCoordinator()
        let m = ChatViewModel(
            networkSession: makeMockSession(),
            connectsOnInit: false,
            executionControlCoordinator: coordinator
        )
        m.startNewSession("chat-1")
        var deleteCount = 0
        MockURLProtocol.handler = { req in
            if req.url?.path.contains("/api/magician/v3/tasks/task-1") == true {
                return (response(for: req), self.activeTaskDetail())
            }
            if req.httpMethod == "DELETE" { deleteCount += 1 }
            return (response(for: req), Data("{}".utf8))
        }
        m.handleIncomingJSON(executionPanelDelta(taskId: "task-1", inspectionExecutionId: "exec-history"))
        waitUntil { m.currentRunExecutionId == "exec-active" }
        XCTAssertTrue(coordinator.begin(.pause, for: "exec-active"))
        m.isThinking = true

        m.cancelRun()

        XCTAssertEqual(deleteCount, 0)
        XCTAssertTrue(m.isThinking)
        XCTAssertEqual(
            m.cancelRunErrorMessage,
            "Another action is already updating this run. Try Stop again when it finishes."
        )
    }

    private func activeTaskDetail() -> Data {
        jsonData(["task": [
            "id": "task-1",
            "status": "running",
            "active_root_execution_id": "exec-active",
            "latest_root_execution_id": "exec-history",
            "chat_session_id": "chat-1"
        ]])
    }

    private func executionPanelDelta(taskId: String, inspectionExecutionId: String) -> String {
        let state: [String: Any] = [
            "default_tab": "run",
            "overview": [
                "task_id": taskId,
                "principal": "local",
                "workspace": "default",
                "ui_thread_id": "general",
                "title": "Task",
                "description": "",
                "status": "running",
                "assigned_agent_id": "personal-assistant",
                "has_plan": false,
                "execution_id": inspectionExecutionId,
                "created_at": 0,
                "updated_at": 1
            ],
            "run": ["recent_activity": []],
            "output": ["deliveries": []],
            "debug": ["history_count": 0]
        ]
        let event: [String: Any] = [
            "event_type": "ExecutionPanelDelta",
            "data": [
                "principal": "local",
                "workspace": "default",
                "task_id": taskId,
                "state": state
            ]
        ]
        return String(
            data: try! JSONSerialization.data(withJSONObject: event),
            encoding: .utf8
        )!
    }
}
