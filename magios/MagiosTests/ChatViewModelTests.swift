import XCTest
import UIKit
@testable import Magician

/// Core ChatViewModel logic: transcript mutation, optimistic send, and canonical
/// chat HITL payloads. Built with `connectsOnInit: false` to skip the WS.
final class ChatViewModelTests: XCTestCase {
    private func makeVM() -> ChatViewModel {
        ChatViewModel(networkSession: makeMockSession(chatCatalogFixture: true), connectsOnInit: false)
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
        XCTAssertTrue(m.messages.isEmpty)             // the view shows its welcome screen
        XCTAssertEqual(m.currentSessionIdValue, "s1")
    }

    func testDeleteMessageRemovesLocally() {
        let m = makeVM()
        m.messages = [ChatMessage(id: "a", isUser: true, text: "one", type: .text),
                      ChatMessage(id: "b", isUser: false, text: "two", type: .text)]
        m.deleteMessage("a")
        XCTAssertEqual(m.messages.map(\.id), ["b"])
    }

    func testClearMessagesEmptiesTheTranscript() {
        let m = makeVM()
        m.messages = [ChatMessage(id: "a", isUser: true, text: "x", type: .text),
                      ChatMessage(id: "b", isUser: false, text: "y", type: .text)]
        m.clearMessages()
        XCTAssertTrue(m.messages.isEmpty)
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
        XCTAssertTrue(m.messages.isEmpty)
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
        XCTAssertTrue(m.messages.isEmpty)
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
        XCTAssertTrue(m.messages.isEmpty)
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
        XCTAssertTrue(m.messages.isEmpty)
    }

    @MainActor
    func testVoiceAppCopilotIsRejectedWithoutOrdinaryChatFallback() {
        let m = makeVM()
        m.guidedFlowScreenIsLocked = { false }
        var spoken: String?
        m.guidedFlowSpeaker = { spoken = $0 }

        prepareChatModelCatalog(m)
        m.sendMessage("App Copilot show me how to create a note", viaVoice: true)

        XCTAssertEqual(m.queueNoticeMessage, "App Copilot isn't available on this device yet.")
        XCTAssertEqual(spoken, "App Copilot isn't available on this device yet.")
        XCTAssertTrue(m.messages.isEmpty)
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

    func testReloadPlacesTaskCardAfterTheReplyOfTheTurnThatStartedIt() throws {
        let user = try decodeRawMessage(id: "user", direction: "user", chatTurnId: "turn-1", content: [
            "type": "text", "text": "Research this"
        ])
        let card = try decodeRawMessage(id: "task-running", chatTurnId: "turn-1", content: [
            "type": "task_status_update", "task_id": "task-1",
            "status": "running", "display_label": "Research"
        ])
        let reply = try decodeRawMessage(id: "reply", chatTurnId: "turn-1", content: [
            "type": "text", "text": "I started the research."
        ])
        let orphan = try decodeRawMessage(id: "task-orphan", content: [
            "type": "task_status_update", "task_id": "task-2",
            "status": "running", "display_label": "Other"
        ])
        let nextUser = try decodeRawMessage(id: "user-2", direction: "user", chatTurnId: "turn-2", content: [
            "type": "text", "text": "Thanks"
        ])

        let projected = ChatViewModel.projectedMessages(from: [user, card, orphan, reply, nextUser])

        // The turn-1 card follows its reply; a card without a turn id keeps
        // its server position.
        XCTAssertEqual(projected.map(\.id), ["user", "task-orphan", "reply", "task-running", "user-2"])
    }

    func testCanonicalReplyLandingAfterItsTaskCardIsPlacedAboveIt() throws {
        let card = ChatViewModel.projectedMessage(from: try decodeRawMessage(
            id: "task-running", chatTurnId: "turn-1", content: [
                "type": "task_status_update", "task_id": "task-1",
                "status": "running", "display_label": "Research"
            ]))
        let reply = ChatViewModel.projectedMessage(from: try decodeRawMessage(
            id: "reply", chatTurnId: "turn-1", content: ["type": "text", "text": "Started."]
        ))
        var transcript = [card]
        XCTAssertTrue(ChatViewModel.reconcileCanonicalText(reply, into: &transcript))
        XCTAssertEqual(transcript.map(\.id), ["reply", "task-running"])
    }

    func testTerminalTaskStatusesUseOnlyTheLatestCardPerTask() throws {
        let done = try decodeRawMessage(id: "a", content: [
            "type": "task_status_update", "task_id": "task-1", "status": "completed", "display_label": "A"
        ])
        let running = try decodeRawMessage(id: "b", content: [
            "type": "task_status_update", "task_id": "task-2", "status": "running", "display_label": "B"
        ])
        let projected = ChatViewModel.projectedMessages(from: [done, running])
        XCTAssertEqual(ChatViewModel.terminalTaskStatuses(projected), ["task-1": "completed"])
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

        prepareChatModelCatalog(m)
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

    func testVerificationReasonUsesCanonicalParagraphsInsteadOfFlattenedProjection() throws {
        let reason = "figure 93217, 45330 not found in any tool result"
        let answer = "**My suggested route:** try a sample.\n\nContact: **+91 93217 45330**."
        let canonical = "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: \(reason)\n\n\(answer)"
        let flattened = canonical.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
        let raw = try decodeRawMessage(
            presentation: [
                "schema": "magician.structured_response", "version": 1,
                "plain_text": flattened,
                "blocks": [["kind": "markdown", "text": answer]]
            ],
            content: ["type": "text", "text": canonical]
        )
        let projected = ChatViewModel.projectedMessage(from: raw)
        XCTAssertNotNil(projected.structuredResponse)
        XCTAssertEqual(projected.text, flattened)
        let display = AnswerVerificationDisplay.split(projected.verificationSourceText ?? projected.text)
        XCTAssertEqual(display.verification?.details, reason)
        XCTAssertEqual(display.body, answer)
        XCTAssertFalse(display.verification?.details?.contains("My suggested route") ?? true)
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

    func testPresentationValidatesArtifactLocatorsForShortenedIds() throws {
        let shortenedId = "magician-artifact:task:task-1:outputs/\(String(repeating: "x", count: 100))~0123456789abcdef"
        let fullPath = "outputs/\(String(repeating: "x", count: 150))/a.md"
        func presentation(locator: [String: Any]) -> [String: Any] {
            [
                "schema": "magician.structured_response",
                "version": 1,
                "plain_text": "Completed",
                "blocks": [["kind": "artifacts", "items": [["label": "a.md", "artifact_id": shortenedId]]]],
                "meta": ["artifact_locators": [locator]]
            ]
        }

        let valid = try decodeRawMessage(
            presentation: presentation(locator: [
                "artifact_id": shortenedId,
                "task_id": "task-1",
                "relative_path": fullPath
            ]),
            content: ["type": "text", "text": "Completed"]
        )
        let locator = try XCTUnwrap(
            ChatViewModel.projectedMessage(from: valid).structuredResponse?.meta?.artifactLocators?.first
        )
        XCTAssertEqual(locator["artifact_id"], shortenedId)
        XCTAssertEqual(locator["task_id"], "task-1")
        XCTAssertEqual(locator["relative_path"], fullPath)

        let invalidLocators: [[String: Any]] = [
            ["artifact_id": shortenedId],
            ["artifact_id": shortenedId, "relative_path": String(repeating: "y", count: 2_049)],
            ["artifact_id": String(repeating: "a", count: 161), "relative_path": "a.md"],
            ["artifact_id": shortenedId, "task_id": "", "relative_path": "a.md"]
        ]
        for invalid in invalidLocators {
            let raw = try decodeRawMessage(
                presentation: presentation(locator: invalid),
                content: ["type": "text", "text": "Completed"]
            )
            XCTAssertNil(ChatViewModel.projectedMessage(from: raw).structuredResponse)
        }
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
            networkSession: makeMockSession(chatCatalogFixture: true),
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
            networkSession: makeMockSession(chatCatalogFixture: true),
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


final class AnswerVerificationDisplayTests: XCTestCase {
    private let caveat = "> ⚠️ **Not fully verified.** Magician's evidence check could not confirm part of this answer: figure 93217, 45330 not found in any tool result"

    func testGeneratedCaveatLeavesPhoneNumbersAndBodyIntact() {
        let body = "Contact: +91 93217 45330."
        let display = AnswerVerificationDisplay.split(caveat + "\n\n" + body)
        XCTAssertEqual(display.body, body)
        XCTAssertEqual(display.verification?.details, "figure 93217, 45330 not found in any tool result")
        XCTAssertEqual(AnswerVerificationDisplay.split("⚠️ Not fully verified. Total: 93217.").body, "Total: 93217.")
    }

    func testOrdinaryTextCodeAndQuotedCaveatsAreUntouched() {
        for body in ["Not fully verified is a status.", "Figure 93217; record 45330.", "```\n" + caveat + "\n```", "Quoted below:\n\n" + caveat] {
            XCTAssertEqual(AnswerVerificationDisplay.split(body), AnswerVerificationDisplay(body: body, verification: nil))
        }
        XCTAssertEqual(AnswerVerificationDisplay.split(caveat + "\r\n\r\nAnswer.").body, "Answer.")
    }
}

/// Server turn state (`turn_state` on session/queue reads, the
/// `chat.turn.state_changed` event): a turn still running after a stream drop
/// or relaunch is a live bubble with its steps; a settled one shows its reply
/// or a status line.
final class ChatTurnStateTests: XCTestCase {
    override func setUp() {
        super.setUp()
        // Opening a session also reads its mention catalog; state-only tests do not need entries.
        MockURLProtocol.handler = { request in (response(for: request), jsonData([:])) }
    }
    private func makeVM() -> ChatViewModel {
        ChatViewModel(networkSession: makeMockSession(chatCatalogFixture: true), connectsOnInit: false)
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        super.tearDown()
    }

    private func stateEvent(
        _ turnId: String,
        _ status: String,
        session: String = "s1",
        reason: String? = nil
    ) -> [String: Any] {
        var payload: [String: Any] = [
            "chat_turn_id": turnId, "chat_session_id": session,
            "status": status, "started_at": 100
        ]
        if status != "running" { payload["ended_at"] = 200 }
        if let reason { payload["reason"] = reason }
        return [
            "event_type": "AgentEvent",
            "data": ["event": [
                "event_type": ChatTurnState.eventType,
                "agent_id": "personal-assistant",
                "payload": payload
            ]]
        ]
    }

    private func messageEvent(
        id: String, direction: String, text: String, turnId: String, session: String = "s1"
    ) -> [String: Any] {
        [
            "event_type": "ChatMessageReceived",
            "data": [
                "session_id": session,
                "message": [
                    "id": id, "session_id": session, "direction": direction,
                    "content": ["type": "text", "text": text],
                    "chat_turn_id": turnId
                ]
            ]
        ]
    }

    private func sessionBody(messages: [[String: Any]], turnState: Any?) -> Data {
        var body: [String: Any] = [
            "session": [
                "id": "s1", "principal": "anonymous", "workspace": "default",
                "agent_id": "personal-assistant", "ui_thread_id": "general",
                "title": "History", "status": "active",
                "created_at": 1, "updated_at": 2
            ],
            "messages": messages
        ]
        if let turnState { body["turn_state"] = turnState }
        return jsonData(body)
    }

    private func userRow(_ id: String, turn: String, text: String = "Research this") -> [String: Any] {
        ["id": id, "session_id": "s1", "direction": "user",
         "content": ["type": "text", "text": text], "chat_turn_id": turn]
    }

    // MARK: Wire model

    func testParsesTheWireStateAndItsStatusLine() throws {
        let running = try XCTUnwrap(ChatTurnState.parse(
            ["chat_turn_id": " t1 ", "status": "running", "started_at": 10] as [String: Any], sessionId: "s1"))
        XCTAssertEqual(running.chatTurnId, "t1")
        XCTAssertEqual(running.sessionId, "s1")
        XCTAssertTrue(running.isRunning)
        XCTAssertNil(running.notice)

        func notice(_ status: String, _ reason: String?) -> String? {
            var raw: [String: Any] = ["chat_turn_id": "t", "status": status, "started_at": 1, "ended_at": 2]
            if let reason { raw["reason"] = reason }
            return ChatTurnState.parse(raw, sessionId: "s1")?.notice
        }
        XCTAssertNil(notice("completed", nil))
        XCTAssertEqual(notice("stopped", "user_stop"), "Stopped")
        XCTAssertEqual(notice("stopped", nil), "Stopped")
        XCTAssertNil(notice("stopped", "turn_stalled"), "a watchdog stop has its persisted notice")
        XCTAssertNil(notice("stopped", "turn_wall_clock_exceeded"))
        XCTAssertEqual(notice("failed", "error"), "This turn failed before it could answer")
        XCTAssertEqual(notice("failed", "interrupted"), "Interrupted before it finished")

        XCTAssertNil(ChatTurnState.parse(["chat_turn_id": "t", "status": "paused"] as [String: Any], sessionId: "s1"))
        XCTAssertNil(ChatTurnState.parse(["status": "running"] as [String: Any], sessionId: "s1"))
        XCTAssertNil(ChatTurnState.parse(["chat_turn_id": "t", "status": "running"] as [String: Any], sessionId: nil))
    }

    func testReadsTheStateEventEnvelope() throws {
        let state = try XCTUnwrap(ChatTurnState.fromEvent(stateEvent("t1", "stopped", reason: "user_stop")))
        XCTAssertEqual(state.chatTurnId, "t1")
        XCTAssertEqual(state.sessionId, "s1")
        XCTAssertEqual(state.status, .stopped)
        XCTAssertEqual(state.endedAt, 200)
        XCTAssertNil(ChatTurnState.fromEvent(messageEvent(id: "a", direction: "assistant", text: "x", turnId: "t1")))
    }

    func testSnapshotTellsAnOlderServerFromNoTurn() {
        XCTAssertEqual(ChatTurnStateSnapshot.parse(object: ["queued": []], sessionId: "s1"), .unsupported)
        XCTAssertEqual(ChatTurnStateSnapshot.parse(object: ["turn_state": NSNull()], sessionId: "s1"), .noTurn)
        let latest = ChatTurnStateSnapshot.parse(
            object: ["turn_state": ["chat_turn_id": "t", "status": "completed", "started_at": 1]],
            sessionId: "s1")
        XCTAssertEqual(latest.state?.status, .completed)
        XCTAssertEqual(latest.state?.sessionId, "s1")
    }

    func testSettledStateIsNeverReplacedByAStaleRunningOne() {
        let running = ChatTurnState(chatTurnId: "t", sessionId: "s1", status: .running, startedAt: 1)
        let done = ChatTurnState(chatTurnId: "t", sessionId: "s1", status: .completed, startedAt: 1, endedAt: 2)
        XCTAssertTrue(ChatTurnState.shouldRecord(running, over: nil))
        XCTAssertTrue(ChatTurnState.shouldRecord(done, over: running))
        XCTAssertFalse(ChatTurnState.shouldRecord(running, over: done))
    }

    func testOnlyATrailingUnansweredTypedTurnReadsAsInterrupted() {
        var question = ChatMessage(id: "u1", isUser: true, text: "Hi", type: .text)
        question.chatTurnId = "t1"
        var answer = ChatMessage(id: "a1", isUser: false, text: "Hello", type: .text)
        answer.chatTurnId = "t1"
        var voice = ChatMessage(id: "u2", isUser: true, text: "Hi", type: .text)
        voice.chatTurnId = "voice-request-1"

        XCTAssertEqual(ChatViewModel.unansweredTrailingUserTurnId([question]), "t1")
        XCTAssertNil(ChatViewModel.unansweredTrailingUserTurnId([question, answer]))
        XCTAssertNil(ChatViewModel.unansweredTrailingUserTurnId([answer]))
        XCTAssertNil(ChatViewModel.unansweredTrailingUserTurnId([voice]))
    }

    // MARK: Live events

    func testRunningTurnFromAnotherSurfaceIsALiveBubbleUntilItsReply() throws {
        let m = makeVM()
        m.startNewSession("s1")

        m.handleIncomingObject(stateEvent("turn-web", "running"))
        let live = try XCTUnwrap(m.messages.last)
        XCTAssertFalse(live.isUser)
        XCTAssertEqual(live.chatTurnId, "turn-web")
        XCTAssertEqual(live.text, "")
        XCTAssertTrue(live.activityIsLive)
        XCTAssertTrue(m.serverRunning, "the composer offers Stop")
        XCTAssertEqual(m.runningServerTurnId, "turn-web")

        // The question lands above the live bubble it started.
        m.handleIncomingObject(messageEvent(id: "u1", direction: "user", text: "Research", turnId: "turn-web"))
        XCTAssertEqual(m.messages.suffix(2).map(\.id), ["u1", live.id])

        m.handleIncomingObject(messageEvent(id: "a1", direction: "assistant", text: "Found it", turnId: "turn-web"))
        XCTAssertEqual(m.messages.last?.id, "a1")
        XCTAssertFalse(m.messages.contains { $0.id == live.id }, "the reply replaces the live bubble")

        m.handleIncomingObject(stateEvent("turn-web", "completed"))
        XCTAssertFalse(m.serverRunning)
        XCTAssertNil(m.runningServerTurnId)
        XCTAssertEqual(m.messages.last?.activityIsLive, false)
        XCTAssertNil(m.messages.last?.turnStatusNotice)
    }

    func testUserStopShowsStoppedOnTheBubble() {
        let m = makeVM()
        m.startNewSession("s1")
        m.handleIncomingObject(stateEvent("t1", "running"))
        m.handleIncomingObject(stateEvent("t1", "stopped", reason: "user_stop"))

        XCTAssertEqual(m.messages.last?.turnStatusNotice, "Stopped")
        XCTAssertEqual(m.messages.last?.activityIsLive, false)
        XCTAssertFalse(m.serverRunning)
    }

    func testFailedTurnShowsTheFailedLine() {
        let m = makeVM()
        m.startNewSession("s1")
        m.handleIncomingObject(stateEvent("t1", "running"))
        m.handleIncomingObject(stateEvent("t1", "failed", reason: "error"))
        XCTAssertEqual(m.messages.last?.turnStatusNotice, ChatTurnState.failedNotice)
    }

    func testWatchdogStopKeepsItsPersistedNoticeWithoutAStatusLine() {
        let m = makeVM()
        m.startNewSession("s1")
        m.handleIncomingObject(stateEvent("t1", "running"))
        m.handleIncomingObject(messageEvent(
            id: "notice-1", direction: "assistant", text: "I stopped: no progress.", turnId: "t1"))
        m.handleIncomingObject(stateEvent("t1", "stopped", reason: "turn_stalled"))

        XCTAssertEqual(m.messages.last?.id, "notice-1")
        XCTAssertEqual(m.messages.last?.text, "I stopped: no progress.")
        XCTAssertNil(m.messages.last?.turnStatusNotice)
        XCTAssertFalse(m.messages.contains { $0.id.hasPrefix("streaming-") })
    }

    func testStaleRunningAfterTheEndDoesNotReviveTheTurn() {
        let m = makeVM()
        m.startNewSession("s1")
        m.handleIncomingObject(stateEvent("t1", "running"))
        m.handleIncomingObject(stateEvent("t1", "stopped", reason: "user_stop"))
        m.handleIncomingObject(stateEvent("t1", "running"))
        XCTAssertFalse(m.serverRunning)
        XCTAssertEqual(m.messages.last?.turnStatusNotice, "Stopped")
    }

    func testOtherSessionsAndRealtimeVoiceTurnsGetNoBubble() {
        let m = makeVM()
        m.startNewSession("s1")
        let before = m.messages.map(\.id)
        m.handleIncomingObject(stateEvent("t-elsewhere", "running", session: "s2"))
        m.handleIncomingObject(stateEvent("voice-request-1", "running"))
        XCTAssertEqual(m.messages.map(\.id), before)
        XCTAssertFalse(m.serverRunning)
    }

    // MARK: Reopen (relaunch / reconnect)

    func testReopenedRunningTurnIsLiveWithItsStepsFromTheStart() {
        let m = makeVM()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req), self.sessionBody(
                    messages: [self.userRow("u1", turn: "t1")],
                    turnState: ["chat_turn_id": "t1", "status": "running", "started_at": 100] as [String: Any]))
            }
            if req.url?.path.hasSuffix("/turns/t1/events") == true {
                return (response(for: req), jsonData([
                    "events": [
                        ["event_type": "tool.call.started", "data": ["call_id": "c1", "tool_name": "search"]]
                    ],
                    "count": 1, "total": 1, "chat_turn_id": "t1"
                ]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        m.loadSession("s1")
        waitUntil(timeout: 5) {
            m.messages.last?.chatTurnId == "t1" && m.messages.last?.isUser == false
                && m.messages.last?.activityRows.isEmpty == false
        }
        XCTAssertEqual(m.messages.map(\.isUser), [true, false])
        XCTAssertTrue(m.messages.last?.activityIsLive == true)
        XCTAssertEqual(m.messages.last?.activityRows.first?.status, .running)
        XCTAssertTrue(m.serverRunning)
    }

    func testReopenAfterRestartShowsTheUnansweredTurnAsInterrupted() {
        let m = makeVM()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req), self.sessionBody(
                    messages: [self.userRow("u1", turn: "t1")], turnState: NSNull()))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        waitUntil(timeout: 5) { m.messages.count == 2 }
        XCTAssertEqual(m.messages.last?.turnStatusNotice, ChatTurnState.interruptedNotice)
        XCTAssertEqual(m.messages.last?.chatTurnId, "t1")
        XCTAssertFalse(m.serverRunning)

        // A load that raced the turn's start: its running state revives it.
        m.handleIncomingObject(stateEvent("t1", "running"))
        XCTAssertNil(m.messages.last?.turnStatusNotice)
        XCTAssertTrue(m.messages.last?.activityIsLive == true)
    }

    func testOpeningAConversationIsNotTheWelcomeScreenAndAFailureCanRetry() {
        let m = makeVM()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req, status: 500), Data("{}".utf8))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        XCTAssertEqual(m.openingSessionId, "s1", "an empty view is loading, not a new chat")
        waitUntil(timeout: 5) { m.sessionOpenFailed }
        XCTAssertNil(m.openingSessionId)

        m.startNewSession("s2")
        XCTAssertNil(m.openingSessionId, "a new chat is never shown as loading")
        XCTAssertFalse(m.sessionOpenFailed)
    }

    func testOlderServerWithoutTurnStateLeavesTheTranscriptAlone() {
        let m = makeVM()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req), self.sessionBody(
                    messages: [self.userRow("u1", turn: "t1")], turnState: nil))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        waitUntil(timeout: 5) { m.messages.first?.id == "u1" }
        waitForMainQueue()
        XCTAssertEqual(m.messages.map(\.id), ["u1"])
    }

    func testQueuePollSettlesATurnWhoseEndEventWasMissed() {
        let m = makeVM()
        m.startNewSession("s1")
        m.handleIncomingObject(stateEvent("t1", "running"))
        XCTAssertTrue(m.serverRunning)
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1/queue") == true {
                return (response(for: req), jsonData([
                    "queued": [], "active": false, "session_id": "s1",
                    "turn_state": ["chat_turn_id": "t1", "status": "stopped", "started_at": 100,
                                   "ended_at": 200, "reason": "user_stop"]
                ]))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.fetchQueue()
        waitUntil(timeout: 5) { !m.serverRunning }
        XCTAssertEqual(m.messages.last?.turnStatusNotice, "Stopped")
    }
}

/// Older-history paging: `has_more`, one page per scroll to the top, prepend.
final class ChatOlderHistoryTests: XCTestCase {
    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), Data("{}".utf8)) }
        super.tearDown()
    }

    private func makeVM() -> ChatViewModel {
        ChatViewModel(networkSession: makeMockSession(chatCatalogFixture: true), connectsOnInit: false)
    }

    private func row(_ id: String, session: String = "s1", direction: String = "user", turn: String? = nil) -> [String: Any] {
        var row: [String: Any] = ["id": id, "session_id": session, "direction": direction,
                                  "content": ["type": "text", "text": "message \(id)"]]
        if let turn { row["chat_turn_id"] = turn }
        return row
    }

    private func detail(_ session: String, rows: [[String: Any]], hasMore: Bool?) -> Data {
        var body: [String: Any] = [
            "session": [
                "id": session, "principal": "anonymous", "workspace": "default",
                "agent_id": "personal-assistant", "ui_thread_id": "general",
                "title": "History", "status": "active", "created_at": 1, "updated_at": 2
            ],
            "messages": rows
        ]
        if let hasMore { body["has_more"] = hasMore }
        return jsonData(body)
    }

    private func query(_ request: URLRequest, _ name: String) -> String? {
        URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == name })?.value
    }

    // MARK: has_more

    func testDetailDecodesHasMoreAndToleratesAnOlderServer() throws {
        let withFlag = try JSONDecoder().decode(
            ChatSessionDetailResponse.self, from: detail("s1", rows: [row("m1")], hasMore: true))
        XCTAssertEqual(withFlag.hasMore, true)
        let legacy = try JSONDecoder().decode(
            ChatSessionDetailResponse.self, from: detail("s1", rows: [row("m1")], hasMore: nil))
        XCTAssertNil(legacy.hasMore)
    }

    func testInitialHasMoreFallsBackToAFullPageGuess() {
        XCTAssertTrue(ChatViewModel.initialHasMore(serverHasMore: nil, embeddedCount: 200))
        XCTAssertFalse(ChatViewModel.initialHasMore(serverHasMore: nil, embeddedCount: 199))
        XCTAssertFalse(ChatViewModel.initialHasMore(serverHasMore: false, embeddedCount: 200))
        XCTAssertTrue(ChatViewModel.initialHasMore(serverHasMore: true, embeddedCount: 3))
    }

    // MARK: Trigger

    func testTriggerNeedsArmedScrolledUpAndAnIdlePageWithMore() {
        let ready = OlderHistoryState(hasMore: true)
        XCTAssertTrue(ChatOlderHistoryTrigger.shouldLoad(armed: true, isAtBottom: false, state: ready))
        // The open: the top row shows before the scroll to the bottom.
        XCTAssertFalse(ChatOlderHistoryTrigger.shouldLoad(armed: false, isAtBottom: false, state: ready))
        XCTAssertFalse(ChatOlderHistoryTrigger.shouldLoad(armed: true, isAtBottom: true, state: ready))
        XCTAssertFalse(ChatOlderHistoryTrigger.shouldLoad(
            armed: true, isAtBottom: false, state: OlderHistoryState(hasMore: false)))
        XCTAssertFalse(ChatOlderHistoryTrigger.shouldLoad(
            armed: true, isAtBottom: false, state: OlderHistoryState(hasMore: true, isLoading: true)))
        XCTAssertFalse(ChatOlderHistoryTrigger.shouldLoad(
            armed: true, isAtBottom: false, state: OlderHistoryState(hasMore: true, failed: true)))
    }

    // MARK: Merge

    func testPrependDropsShownRowsAndOlderCardsOfAShownTask() {
        let current = [
            ChatMessage(id: "b", isUser: true, text: "b", type: .text),
            ChatMessage(id: "card-2", isUser: false, text: "",
                        type: .taskStatus(TaskStatusModel(taskId: "t1", title: "T", status: "completed", steps: [])))
        ]
        let older = [
            ChatMessage(id: "a", isUser: true, text: "a", type: .text),
            ChatMessage(id: "a", isUser: true, text: "a again", type: .text),
            ChatMessage(id: "card-1", isUser: false, text: "",
                        type: .taskStatus(TaskStatusModel(taskId: "t1", title: "T", status: "running", steps: []))),
            ChatMessage(id: "b", isUser: true, text: "b", type: .text)
        ]
        let merged = ChatViewModel.prependOlderMessages(older, to: current)
        XCTAssertEqual(merged.map(\.id), ["a", "b", "card-2"])
        XCTAssertEqual(ChatViewModel.prependOlderMessages([], to: current).map(\.id), ["b", "card-2"])
    }

    func testPrependLeavesTheActivityAnchorOnTheNewerReplyOfASplitTurn() {
        var newer = ChatMessage(id: "reply-2", isUser: false, text: "second", type: .text)
        newer.chatTurnId = "turn-1"
        var older = ChatMessage(id: "reply-1", isUser: false, text: "first", type: .text)
        older.chatTurnId = "turn-1"
        var other = ChatMessage(id: "reply-0", isUser: false, text: "zero", type: .text)
        other.chatTurnId = "turn-0"
        let merged = ChatViewModel.prependOlderMessages([other, older], to: [newer])
        XCTAssertEqual(merged.map(\.id), ["reply-0", "reply-1", "reply-2"])
        XCTAssertEqual(merged.map(\.chatTurnId), ["turn-0", nil, "turn-1"])
    }

    // MARK: Paging

    func testScrollToTopLoadsOneOlderPageAndPrependsIt() {
        let m = makeVM()
        var pageRequests: [URLRequest] = []
        let release = DispatchSemaphore(value: 0)
        MockURLProtocol.handler = { req in
            let path = req.url?.path ?? ""
            if path.hasSuffix("/chat/sessions/s1") {
                return (response(for: req), self.detail("s1", rows: [self.row("m3"), self.row("m4")], hasMore: true))
            }
            if path.hasSuffix("/chat/sessions/s1/messages") {
                pageRequests.append(req)
                release.wait()
                return (response(for: req), jsonData([
                    "messages": [self.row("m1"), self.row("m2")], "has_more": false
                ]))
            }
            return (response(for: req), Data("{}".utf8))
        }

        m.loadSession("s1")
        waitUntil { m.olderHistory.hasMore }
        XCTAssertEqual(m.messages.map(\.id), ["m3", "m4"])

        m.loadOlderMessages()
        XCTAssertTrue(m.olderHistory.isLoading)
        m.loadOlderMessages()  // in flight: no second request
        waitUntil { pageRequests.count == 1 }
        release.signal()
        waitUntil { !m.olderHistory.isLoading }

        XCTAssertEqual(pageRequests.count, 1)
        XCTAssertEqual(query(pageRequests[0], "before"), "m3")
        XCTAssertEqual(query(pageRequests[0], "limit"), "50")
        XCTAssertEqual(m.messages.map(\.id), ["m1", "m2", "m3", "m4"])
        XCTAssertEqual(m.olderHistory, OlderHistoryState(hasMore: false))
        XCTAssertEqual(m.olderHistoryPrepend?.anchorMessageId, "m3")

        m.loadOlderMessages()  // exhausted: nothing more to ask for
        waitForMainQueue()
        XCTAssertEqual(pageRequests.count, 1)
    }

    func testOlderServerWithAFullPageStillOffersOlderHistory() {
        let m = makeVM()
        let rows = (0..<200).map { row("m\($0)") }
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/chat/sessions/s1") == true {
                return (response(for: req), self.detail("s1", rows: rows, hasMore: nil))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        waitUntil(timeout: 5) { m.messages.count == 200 }
        XCTAssertTrue(m.olderHistory.hasMore)
    }

    func testFailedPageKeepsMessagesAndOffersARetry() {
        let m = makeVM()
        var failPage = true
        MockURLProtocol.handler = { req in
            let path = req.url?.path ?? ""
            if path.hasSuffix("/chat/sessions/s1") {
                return (response(for: req), self.detail("s1", rows: [self.row("m2")], hasMore: true))
            }
            if path.hasSuffix("/chat/sessions/s1/messages") {
                if failPage { return (response(for: req, status: 500), Data("{}".utf8)) }
                return (response(for: req), jsonData(["messages": [self.row("m1")], "has_more": true]))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        waitUntil { m.olderHistory.hasMore }
        m.loadOlderMessages()
        waitUntil { m.olderHistory.failed }
        XCTAssertFalse(m.olderHistory.isLoading)
        XCTAssertEqual(m.messages.map(\.id), ["m2"])

        failPage = false
        m.loadOlderMessages()  // the retry
        waitUntil { m.messages.count == 2 }
        XCTAssertEqual(m.messages.map(\.id), ["m1", "m2"])
        XCTAssertEqual(m.olderHistory, OlderHistoryState(hasMore: true))
    }

    func testPageForAChatTheUserLeftIsDropped() {
        let m = makeVM()
        let release = DispatchSemaphore(value: 0)
        var pageRequested = false
        MockURLProtocol.handler = { req in
            let path = req.url?.path ?? ""
            if path.hasSuffix("/chat/sessions/s1") {
                return (response(for: req), self.detail("s1", rows: [self.row("m2")], hasMore: true))
            }
            if path.hasSuffix("/chat/sessions/s1/messages") {
                pageRequested = true
                release.wait()
                return (response(for: req), jsonData(["messages": [self.row("m1")], "has_more": true]))
            }
            return (response(for: req), Data("{}".utf8))
        }
        m.loadSession("s1")
        waitUntil { m.olderHistory.hasMore }
        m.loadOlderMessages()
        waitUntil { pageRequested }
        m.startNewSession("s2")
        release.signal()
        let settled = expectation(description: "a late page would have landed")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { settled.fulfill() }
        wait(for: [settled], timeout: 2)

        XCTAssertEqual(m.currentSessionIdValue, "s2")
        XCTAssertFalse(m.messages.contains { $0.id == "m1" })
        XCTAssertEqual(m.olderHistory, OlderHistoryState())
    }
}
