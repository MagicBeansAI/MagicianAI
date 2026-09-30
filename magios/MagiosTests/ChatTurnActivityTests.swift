import XCTest
@testable import Magician

final class ChatTurnActivityTests: XCTestCase {
    /// The turn-events stream rides an endpoint with NO heartbeat, so the idle
    /// allowance is the only thing standing between a healthy-but-quiet turn
    /// and a -1001 every sixty seconds. Pinned at a floor rather than a value:
    /// shortening it back toward URLSession's default is the regression this
    /// guards against; lengthening it is a judgement the constant's doc owns.
    func testTheActivityStreamIdleAllowanceOutlastsQuietTurns() {
        XCTAssertGreaterThanOrEqual(ChatViewModel.turnActivityStreamIdleTimeout, 15 * 60)
    }

    func testIgnoresInternalAndUnknownEvents() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "__events_ready"])
        sut.ingest(["event_type": "unknown", "data": [:]])
        XCTAssertEqual(sut.snapshot(), [])
    }

    func testPauseEventSetsStateAndResetClearsEverything() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "HitlRequested", "data": ["correlation_id": "c1", "prompt": "  Choose   wisely "]])
        XCTAssertTrue(sut.pauseActive)
        XCTAssertEqual(sut.snapshot().first?.key, "pause:c1")
        XCTAssertEqual(sut.snapshot().first?.detail, "Choose wisely")
        sut.reset()
        XCTAssertFalse(sut.pauseActive)
        XCTAssertTrue(sut.snapshot().isEmpty)
    }

    func testLLMLifecycleCoalescesAndFormatsTiming() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", ["trace_id": "t", "model": "gpt-test"]))
        sut.ingest(event("llm.first_token", ["trace_id": "t", "duration_ms": 700]))
        sut.ingest(event("llm.succeeded", ["trace_id": "t", "duration_ms": 2100.0, "ttft_ms": 700.0]))
        let row = sut.snapshot().first
        XCTAssertEqual(sut.snapshot().count, 1)
        XCTAssertEqual(row?.label, "Thinking with gpt-test")
        XCTAssertEqual(row?.detail, "700ms (2.1s)")
        XCTAssertEqual(row?.status, .done)
        XCTAssertEqual(row?.durationMs, 2100)
    }

    func testAgentEnvelopePrefixesDelegatedToolAndReportsFailure() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(agentEvent("tool.call.started", agent: "researcher", payload: ["call_id": "c", "tool_name": "search"]))
        sut.ingest(agentEvent("tool.call.failed", agent: "researcher", payload: ["call_id": "c", "tool_name": "search", "error": "network down"]))
        let row = sut.snapshot().first
        XCTAssertEqual(row?.label, "[researcher] search failed")
        XCTAssertEqual(row?.detail, "network down")
        XCTAssertEqual(row?.status, .failed)
        XCTAssertEqual(row?.tone, .error)
    }

    func testDelegateToolDescribesParallelTargets() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("tool.call.started", [
            "call_id": "d", "tool_name": "delegate_to_agent",
            "args": ["delegation_targets": [["target_agent_id": "a"], ["target_agent_id": "b"]]]
        ]))
        XCTAssertEqual(sut.snapshot().first?.label, "Decomposing into 2 parallel agents")
        XCTAssertEqual(sut.snapshot().first?.detail, "a, b")
    }

    func testReasoningContentMergesAndTrims() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("reasoning.content", ["trace_id": "r", "delta": "first"]))
        sut.ingest(event("reasoning.content", ["trace_id": "r", "delta": "second"]))
        sut.ingest(event("reasoning.end", ["trace_id": "r", "duration_ms": 12]))
        XCTAssertEqual(sut.snapshot().first?.detail, "first second")
        XCTAssertEqual(sut.snapshot().first?.status, .done)
        XCTAssertEqual(sut.snapshot().first?.durationMs, 12)
    }

    func testStepArtifactTutorAndCodingLifecycles() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("step.started", ["step_id": "compile"]))
        sut.ingest(event("step.completed", ["step_id": "compile"]))
        sut.ingest(event("artifact.created", ["name": "report.md"]))
        sut.ingest(event("tutor.run.completed", ["run_id": "tutor-1"]))
        sut.ingest(event("coding.failed", ["shadow_workspace_id": "w", "error": "conflict"]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.map(\.status), [.done, .done, .done, .failed])
        XCTAssertEqual(rows[1].label, "Produced report.md")
        XCTAssertNil(rows[1].detail)
        XCTAssertEqual(rows[2].label, "Tutor completed")
        XCTAssertEqual(rows[3].detail, "conflict")
    }

    func testPureHelpersHandleBoundaries() {
        XCTAssertTrue(isPauseEvent(outer: "", event: "input.requested"))
        XCTAssertFalse(isPauseEvent(outer: "", event: "input.completed"))
        XCTAssertEqual(trimTo(" a   b ", 10), "a b")
        XCTAssertEqual(trimTo("123456", 5), "1234…")
        XCTAssertEqual(formatLatency(999.5), "1000ms")
        XCTAssertEqual(formatLatency(1250), "1.2s")
        XCTAssertEqual(formatLatency(-1), "—")
        XCTAssertEqual(formatLatency(.infinity), "—")
        XCTAssertEqual(formatLatencyPair(nil, 1000), "… (1.0s)")
    }

    func testPreviewKeepsOnlyNewestFiveRowsAndPreservesOrder() {
        let rows = (1...7).map { index in
            ActivityRow(
                key: "step-\(index)",
                kind: .step,
                label: "Step \(index)",
                detail: nil,
                status: .done,
                tone: .info,
                durationMs: nil
            )
        }

        XCTAssertEqual(activityPreviewRows(rows).map(\.key), [
            "step-3", "step-4", "step-5", "step-6", "step-7"
        ])
        XCTAssertEqual(activityPreviewRows(rows, limit: 0), [])
    }

    func testTaskLifecycleRetainsIndependentRunTargetAndFiles() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("task.status_changed", [
            "task_id": "task_0123456789abcdef0123456789abcdef",
            "target_agent_id": "researcher",
            "status": "running"
        ]))
        sut.ingest(event("tool.call.started", [
            "execution_id": "exec_0123456789abcdef0123456789abcdef",
            "call_id": "research",
            "tool_name": "search"
        ]))
        sut.ingest(event("chat.delegate.status_changed", [
            "task_id": "task_0123456789abcdef0123456789abcdef",
            "target_agent_id": "researcher",
            "status": "completed",
            "summary": "Research is complete.",
            "output_files": [[
                "absolute_path": "/tmp/report.md",
                "display_name": "report.md"
            ]]
        ]))

        let rows = sut.snapshot()
        let taskRow = try! XCTUnwrap(rows.first { $0.key.hasPrefix("task::") })
        XCTAssertEqual(rows.count, 2)
        XCTAssertFalse(rows.contains { $0.status == .running })
        XCTAssertEqual(taskRow.status, .done)
        XCTAssertEqual(taskRow.detail, "Research is complete.")
        XCTAssertEqual(taskRow.files, [
            ActivityFileRef(absolutePath: "/tmp/report.md", label: "report.md")
        ])
        XCTAssertEqual(activityInspectTarget(rows), ActivityInspectTarget(
            taskId: "task_0123456789abcdef0123456789abcdef",
            executionId: "exec_0123456789abcdef0123456789abcdef"
        ))
        XCTAssertEqual(activityInspectTarget(rows)?.detailPresentation, .runInspection)
    }

    func testInlineLLMTurnDoesNotExposeRunInspection() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", [
            "trace_id": "inline-turn",
            "execution_id": "chat-turn-inline",
            "model": "gpt-test"
        ]))
        sut.ingest(event("llm.succeeded", [
            "trace_id": "inline-turn",
            "execution_id": "chat-turn-inline",
            "duration_ms": 1200.0
        ]))

        let rows = sut.snapshot()
        XCTAssertEqual(rows.first?.executionId, "chat-turn-inline")
        XCTAssertNil(activityInspectTarget(rows))
    }

    func testTaskWithoutExecutionDoesNotExposeRunInspection() {
        let row = ActivityRow(
            key: "task::task-42",
            kind: .step,
            label: "Task queued",
            detail: nil,
            status: .waiting,
            tone: .info,
            durationMs: nil,
            taskId: "task-42"
        )

        XCTAssertNil(activityInspectTarget([row]))
    }

    func testTerminalTransitionMovesLongRunningRowIntoInlineTail() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("tool.call.started", ["call_id": "old", "tool_name": "shell"]))
        for index in 1...6 {
            sut.ingest(event("step.started", ["step_id": "step-\(index)"]))
            sut.ingest(event("step.completed", ["step_id": "step-\(index)"]))
        }
        sut.ingest(event("tool.call.finished", ["call_id": "old", "tool_name": "shell"]))

        XCTAssertEqual(activityPreviewRows(sut.snapshot()).last?.label, "shell returned")
    }

    func testActivityLinkClassificationMatchesWebDestinations() {
        let taskId = "task_0123456789abcdef0123456789abcdef"
        let executionId = "exec_0123456789abcdef0123456789abcdef"
        XCTAssertEqual(classifyActivityLink(taskId), .task(taskId))
        XCTAssertEqual(classifyActivityLink(executionId), .execution(executionId))
        XCTAssertEqual(
            classifyActivityLink("/tasks?filter=all&selected=\(taskId)"),
            .task(taskId)
        )
        XCTAssertEqual(classifyActivityLink("/t/general"), .thread("general"))
        XCTAssertEqual(
            classifyActivityLink("/attention?item_id=attention-1"),
            .attention("attention-1")
        )
        XCTAssertEqual(classifyActivityLink("file:///tmp/report.md"), .file("/tmp/report.md"))
        XCTAssertEqual(classifyActivityLink("https://example.com/report"), .external("https://example.com/report"))
    }

    func testCompletedTaskStatusKeepsConcreteExecutionForInspection() {
        let seed = TaskDetailSeed(TaskStatusModel(
            taskId: "task_0123456789abcdef0123456789abcdef",
            title: "Completed task",
            status: "completed",
            steps: [],
            executionId: "exec_0123456789abcdef0123456789abcdef"
        ))

        XCTAssertNil(seed.activeRootExecutionId)
        XCTAssertEqual(seed.latestRootExecutionId, "exec_0123456789abcdef0123456789abcdef")
    }

    func testPathClientUsesSessionScopedOpenEndpointAndBody() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        var captured: URLRequest?
        MockURLProtocol.handler = { request in
            captured = request
            return (response(for: request), Data())
        }

        try await ActivityPathClient.perform(
            sessionId: "session with spaces",
            absolutePath: "/tmp/report.md",
            action: .folder,
            session: session
        )

        XCTAssertEqual(captured?.httpMethod, "POST")
        XCTAssertEqual(
            captured?.url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false)?.percentEncodedPath },
            "/api/magician/v2/chat/sessions/session%20with%20spaces/outputs/open-folder"
        )
        let body = try XCTUnwrap(captured.flatMap(requestBody))
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: String])
        XCTAssertEqual(object["absolute_path"], "/tmp/report.md")
    }

    func testProjectedToolResultPatchesMatchingRowWithCompleteResultIdentity() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("tool.call.started", [
            "call_id": "call-7",
            "tool_name": "search_memory"
        ]))
        sut.ingest(event("tool.result.projected", [
            "tool_call_id": "call-7",
            "tool_name": "search_memory",
            "result_ref": "result_ref_opaque",
            "content_hash": "blake3:verified",
            "size_bytes": 71_423,
            "result_owner": [
                "kind": "chat",
                "session_id": "session-parent"
            ],
            "task_id": "task-7",
            "execution_id": "exec-7"
        ]))

        let row = sut.snapshot().first
        XCTAssertEqual(sut.snapshot().count, 1)
        XCTAssertEqual(row?.resultRef, "result_ref_opaque")
        XCTAssertEqual(row?.resultHash, "blake3:verified")
        XCTAssertEqual(row?.resultSizeBytes, 71_423)
        XCTAssertEqual(row?.resultOwner, .chat(sessionId: "session-parent"))
        XCTAssertEqual(row?.taskId, "task-7")
        XCTAssertEqual(row?.executionId, "exec-7")
    }

    func testCompleteResultClientReadsEveryAuthorizedPageAndReconstructsArray() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        var requests: [URLRequest] = []
        MockURLProtocol.handler = { request in
            requests.append(request)
            let body = try XCTUnwrap(requestBody(request))
            let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let cursor = payload["cursor"] as? String
            let page: [String: Any]
            if cursor == nil {
                page = [
                    "content_hash": "blake3:complete",
                    "entries": [["field_path": "", "source_index": 0, "value": ["id": 1]]],
                    "next_cursor": "cursor-2"
                ]
            } else {
                XCTAssertEqual(cursor, "cursor-2")
                page = [
                    "content_hash": "blake3:complete",
                    "entries": [["field_path": "", "source_index": 1, "value": ["id": 2]]]
                ]
            }
            let data = try JSONSerialization.data(withJSONObject: ["page": page])
            return (response(for: request), data)
        }

        let result = try await ActivityResultClient.readAll(
            sessionId: "session-7",
            resultRef: "result-ref-7",
            session: session
        )

        XCTAssertEqual(requests.count, 2)
        XCTAssertTrue(requests.allSatisfy {
            $0.url?.path == "/api/magician/v2/chat/sessions/session-7/results/read"
        })
        XCTAssertEqual(result.contentHash, "blake3:complete")
        XCTAssertTrue(result.text.contains("\"id\" : 1"))
        XCTAssertTrue(result.text.contains("\"id\" : 2"))
    }

    func testCompleteResultClientReconstructsTypedContainersAndUnicodeFragments() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        let first = "नम"
        let second = "स्ते"
        let firstBytes = first.utf8.count
        let totalBytes = (first + second).utf8.count
        MockURLProtocol.handler = { request in
            let payload = try XCTUnwrap(
                JSONSerialization.jsonObject(with: XCTUnwrap(requestBody(request))) as? [String: Any]
            )
            let cursor = payload["cursor"] as? String
            let entries: [[String: Any]]
            let nextCursor: String?
            if cursor == nil {
                entries = [
                    ["field_path": "", "reconstruction_path": "", "kind": "container", "value": [:]],
                    [
                        "field_path": "",
                        "reconstruction_path": "/slash~1key~0",
                        "kind": "string_fragment",
                        "string_fragment": ["byte_start": 0, "byte_end": firstBytes, "total_bytes": totalBytes],
                        "value": first
                    ]
                ]
                nextCursor = "fragment-page-2"
            } else {
                XCTAssertEqual(cursor, "fragment-page-2")
                entries = [
                    [
                        "field_path": "",
                        "reconstruction_path": "/slash~1key~0",
                        "kind": "string_fragment",
                        "string_fragment": ["byte_start": firstBytes, "byte_end": totalBytes, "total_bytes": totalBytes],
                        "value": second
                    ],
                    ["field_path": "", "reconstruction_path": "/status", "kind": "complete_value", "value": "ok"]
                ]
                nextCursor = nil
            }
            var page: [String: Any] = [
                "reconstruction_version": 1,
                "content_hash": "blake3:fragmented",
                "entries": entries
            ]
            if let nextCursor { page["next_cursor"] = nextCursor }
            return (response(for: request), try JSONSerialization.data(withJSONObject: ["page": page]))
        }

        let result = try await ActivityResultClient.readAll(
            sessionId: "session-fragmented",
            resultRef: "result-fragmented",
            session: session
        )

        XCTAssertEqual(result.contentHash, "blake3:fragmented")
        XCTAssertTrue(result.text.contains("\"slash/key~\" : \"नमस्ते\""))
        XCTAssertTrue(result.text.contains("\"status\" : \"ok\""))
    }

    func testCompleteResultClientPreservesRootScalar() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        MockURLProtocol.handler = { request in
            let data = try JSONSerialization.data(withJSONObject: [
                "page": [
                    "reconstruction_version": 1,
                    "content_hash": "blake3:scalar",
                    "page_start": 0,
                    "total_entries": 1,
                    "entries": [[
                        "field_path": "",
                        "reconstruction_path": "",
                        "kind": "complete_value",
                        "value": "नमस्ते 🧭"
                    ]]
                ]
            ])
            return (response(for: request), data)
        }

        let result = try await ActivityResultClient.readAll(
            sessionId: "session-scalar",
            resultRef: "result-scalar",
            session: session
        )

        XCTAssertEqual(result.text, "\"नमस्ते 🧭\"")
    }

    func testCompleteResultClientRejectsChangedHashAcrossPages() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        MockURLProtocol.handler = { request in
            let payload = try XCTUnwrap(
                JSONSerialization.jsonObject(with: XCTUnwrap(requestBody(request))) as? [String: Any]
            )
            let isFirst = payload["cursor"] == nil
            var page: [String: Any] = [
                "content_hash": isFirst ? "blake3:first" : "blake3:changed",
                "page_start": isFirst ? 0 : 1,
                "total_entries": 2,
                "entries": [["field_path": "", "source_index": isFirst ? 0 : 1, "value": isFirst ? "a" : "b"]]
            ]
            if isFirst { page["next_cursor"] = "page-2" }
            return (response(for: request), try JSONSerialization.data(withJSONObject: ["page": page]))
        }

        do {
            _ = try await ActivityResultClient.readAll(
                sessionId: "session-hash-change",
                resultRef: "result-hash-change",
                session: session
            )
            XCTFail("changed page hash should fail closed")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("hash changed"))
        }
    }

    func testCompleteResultClientRejectsSkippedLegacyArrayIndex() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        MockURLProtocol.handler = { request in
            let data = try JSONSerialization.data(withJSONObject: [
                "page": [
                    "content_hash": "blake3:skipped",
                    "entries": [["field_path": "", "source_index": 1, "value": "missing-zero"]]
                ]
            ])
            return (response(for: request), data)
        }

        do {
            _ = try await ActivityResultClient.readAll(
                sessionId: "session-skipped",
                resultRef: "result-skipped",
                session: session
            )
            XCTFail("skipped array index should fail closed")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("missing or out of order"))
        }
    }

    func testCompleteResultClientRejectsProjectedHashMismatchAndOverlappingScalarPaths() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        MockURLProtocol.handler = { request in
            let data = try JSONSerialization.data(withJSONObject: [
                "page": [
                    "reconstruction_version": 1,
                    "content_hash": "blake3:actual",
                    "page_start": 0,
                    "total_entries": 2,
                    "entries": [
                        ["field_path": "", "reconstruction_path": "/record", "kind": "complete_value", "value": ["value": 1]],
                        ["field_path": "", "reconstruction_path": "/record/value", "kind": "complete_value", "value": 2]
                    ]
                ]
            ])
            return (response(for: request), data)
        }

        do {
            _ = try await ActivityResultClient.readAll(
                sessionId: "session-overlap",
                resultRef: "result-overlap",
                expectedContentHash: "blake3:projected",
                session: session
            )
            XCTFail("a projected/display hash mismatch must fail closed")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("projected content hash"))
        }

        do {
            _ = try await ActivityResultClient.readAll(
                sessionId: "session-overlap",
                resultRef: "result-overlap",
                expectedContentHash: "blake3:actual",
                session: session
            )
            XCTFail("a complete parent followed by a descendant must fail closed")
        } catch {
            XCTAssertTrue(error.localizedDescription.contains("overlapping scalar"))
        }
    }

    func testCompleteResultClientUsesTaskOwnerAndExecutionBinding() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        var captured: URLRequest?
        MockURLProtocol.handler = { request in
            captured = request
            let data = try JSONSerialization.data(withJSONObject: [
                "page": [
                    "content_hash": "blake3:task",
                    "entries": [["field_path": "", "value": "done"]]
                ]
            ])
            return (response(for: request), data)
        }

        let result = try await ActivityResultClient.readAll(
            sessionId: nil,
            resultRef: "task-result-ref",
            owner: .task(taskId: "task 7", executionId: "exec-7"),
            taskId: "task 7",
            executionId: "exec-7",
            session: session
        )

        XCTAssertEqual(
            captured?.url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false)?.percentEncodedPath },
            "/api/magician/v3/tasks/task%207/results/read"
        )
        let body = try XCTUnwrap(captured.flatMap(requestBody))
        let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(payload["result_ref"] as? String, "task-result-ref")
        XCTAssertEqual(payload["execution_id"] as? String, "exec-7")
        XCTAssertTrue(result.text.contains("done"))
    }

    func testCompleteResultClientUsesCanonicalChatOwnerInsteadOfDelegatedTaskNavigation() async throws {
        let session = makeMockSession()
        defer { MockURLProtocol.handler = nil }
        var captured: URLRequest?
        MockURLProtocol.handler = { request in
            captured = request
            let data = try JSONSerialization.data(withJSONObject: [
                "page": [
                    "content_hash": "blake3:chat",
                    "entries": [["field_path": "", "value": "delegated"]]
                ]
            ])
            return (response(for: request), data)
        }

        let result = try await ActivityResultClient.readAll(
            sessionId: "host-session",
            resultRef: "chat-result-ref",
            owner: .chat(sessionId: "parent session"),
            taskId: "delegated-task",
            executionId: "delegated-exec",
            session: session
        )

        XCTAssertEqual(
            captured?.url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false)?.percentEncodedPath },
            "/api/magician/v2/chat/sessions/parent%20session/results/read"
        )
        let body = try XCTUnwrap(captured.flatMap(requestBody))
        let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertNil(payload["execution_id"])
        XCTAssertTrue(result.text.contains("delegated"))
    }

    private func event(_ type: String, _ payload: [String: Any]) -> [String: Any] {
        ["event_type": type, "data": payload]
    }

    private func agentEvent(_ type: String, agent: String, payload: [String: Any]) -> [String: Any] {
        ["event_type": "AgentEvent", "data": ["event": ["event_type": type, "agent_id": agent, "payload": payload]]]
    }
}
