import XCTest
@testable import Magician

final class ChatTurnActivityTests: XCTestCase {
    func testContextPreparationKeepsMeasuredDurationAndSettlesAfterReplay() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("context.retrieval.started", ["phase_id":"p"]))
        sut.ingest(event("context.retrieval.completed", ["phase_id":"p", "status":"partial", "duration_ms":123, "summary":"Memory search: 100 ms"]))
        sut.ingest(event("context.retrieval.started", ["phase_id":"p"]))
        XCTAssertEqual(sut.snapshot().count, 1)
        XCTAssertEqual(sut.snapshot().first?.label, "Context prepared (partial)")
        XCTAssertEqual(sut.snapshot().first?.durationMs, 123)
        XCTAssertEqual(sut.snapshot().first?.status, .done)
        XCTAssertNil(sut.snapshot().first?.callUsage)
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

    func testAnswerWithMatchingCorrelationSettlesOnlyThatPauseRow() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "HitlRequested", "data": ["correlation_id": "c1", "prompt": "Approve plan?"]])
        sut.ingest(["event_type": "HitlRequested", "data": ["correlation_id": "c2", "prompt": "Pick one"]])
        XCTAssertTrue(activityAwaitsUser(sut.snapshot()))

        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "hitl.resolved", "payload": ["correlation_id": "c1"]]]])
        let rows = sut.snapshot()
        let first = rows.first { $0.key == "pause:c1" }
        XCTAssertEqual(first?.label, "Answered")
        XCTAssertEqual(first?.status, .done)
        XCTAssertEqual(first?.tone, .info)
        XCTAssertEqual(first?.detail, "Approve plan?")
        XCTAssertEqual(rows.first { $0.key == "pause:c2" }?.status, .waiting)
        XCTAssertTrue(sut.pauseActive, "c2 still waits")
        XCTAssertTrue(activityAwaitsUser(rows))

        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "input.received", "payload": ["correlation_id": "c2"]]]])
        XCTAssertFalse(sut.pauseActive)
        XCTAssertFalse(activityAwaitsUser(sut.snapshot()))
    }

    func testIdLessAnswerSettlesEveryWaitingPauseRow() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "HitlRequested", "data": ["correlation_id": "a"]])
        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "input.requested", "payload": ["request_id": "b"]]]])
        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "input.resolved", "payload": [:]]]])
        XCTAssertEqual(sut.snapshot().map(\.label), ["Answered", "Answered"])
        XCTAssertFalse(sut.pauseActive)
    }

    func testIdLessPauseRowSettlesWithAnyAnswerAndUnrelatedIdsStayWaiting() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "HitlRequested", "data": [:]])  // files under pause:pause
        sut.ingest(["event_type": "HitlRequested", "data": ["correlation_id": "keep"]])
        sut.ingest(["event_type": "HitlResolved", "data": ["pause_state_id": "other"]])
        let rows = sut.snapshot()
        XCTAssertEqual(rows.first { $0.key == "pause:pause" }?.status, .done)
        XCTAssertEqual(rows.first { $0.key == "pause:keep" }?.status, .waiting)
        XCTAssertTrue(sut.pauseActive)
    }

    func testAnswerSettlesNothingAlreadyDoneAndIgnoresNonPauseRows() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "step.started", "payload": ["step_id": "s1", "step_name": "Fetch"]]]])
        sut.ingest(["event_type": "AgentEvent", "data": ["event": [
            "event_type": "hitl.resolved", "payload": [:]]]])
        XCTAssertNotEqual(sut.snapshot().first?.label, "Answered")
        XCTAssertFalse(sut.pauseActive)
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

    func testLLMSucceededRelabelsRowWithTheModelThatAnswered() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", ["trace_id": "t", "model": "chat.default"]))
        sut.ingest(event("llm.succeeded", [
            "trace_id": "t", "model": "Claude Code · opus", "duration_ms": 1200.0
        ]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.label, "Thinking with Claude Code · opus")
        XCTAssertEqual(rows.first?.status, .done)

        // A completion without a model keeps the request-time label.
        let keep = ChatTurnActivityAccumulator()
        keep.ingest(event("llm.requested", ["trace_id": "t", "model": "gpt-test"]))
        keep.ingest(event("llm.succeeded", ["trace_id": "t", "duration_ms": 900.0]))
        XCTAssertEqual(keep.snapshot().first?.label, "Thinking with gpt-test")
    }

    func testTerminalTranscriptCardSettlesItsRunningTaskRowAndLeaves() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(event("task.status_changed", ["task_id": "task-1", "status": "running", "display_label": "researcher"]))
        sut.ingest(event("task.status_changed", ["task_id": "task-2", "status": "running", "display_label": "writer"]))
        sut.ingest(event("tool.call.started", ["call_id": "c", "tool_name": "search"]))
        let running = sut.snapshot()
        XCTAssertEqual(running.first { $0.key == "task::task-1" }?.status, .running)

        let partly = settleActivityRows(running, terminalTaskStatuses: ["task-1": "completed"])
        XCTAssertEqual(partly.first { $0.key == "task::task-1" }?.status, .done)
        XCTAssertEqual(partly.first { $0.key == "task::task-1" }?.label, "researcher completed")
        XCTAssertEqual(partly.first { $0.key == "task::task-2" }?.status, .running)
        XCTAssertEqual(partly.first { $0.toolName == "search" }?.status, .running)

        let all = settleActivityRows(running, terminalTaskStatuses: [
            "task-1": "completed", "task-2": "failed"
        ])
        XCTAssertEqual(all.first { $0.key == "task::task-2" }?.status, .failed)
        XCTAssertEqual(all.first { $0.key == "task::task-2" }?.tone, .error)
        XCTAssertEqual(all.first { $0.toolName == "search" }?.status, .failed)
        XCTAssertEqual(settleActivityRows(running, terminalTaskStatuses: [:]), running)
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

    func testToolRowsKeepTheirToolNameForTheDotTint() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(agentEvent("tool.call.started", agent: "researcher", payload: ["call_id": "c", "tool_name": "web_fetch"]))
        sut.ingest(agentEvent("tool.call.finished", agent: "researcher", payload: ["call_id": "c", "tool_name": "web_fetch"]))
        let row = sut.snapshot().first { $0.kind == .tool }
        XCTAssertEqual(row?.toolName, "web_fetch")
        XCTAssertEqual(row.map(activityTintKey), "web_fetch")
    }

    func testStageDotsShowTheLatestThreeSettledRowsAndFoldTheRest() {
        var rows = (0..<12).map { index in
            ActivityRow(key: "step-\(index)", kind: .step, label: "Step \(index)", detail: nil,
                        status: .done, tone: .info, durationMs: nil)
        }
        rows.append(ActivityRow(key: "live", kind: .tool, label: "Calling search", detail: nil,
                                status: .running, tone: .tool, durationMs: nil))
        let window = activityStageDots(rows)
        XCTAssertEqual(activityStageDotLimit, 3)
        XCTAssertEqual(window.shown.map(\.key), ["step-9", "step-10", "step-11"])
        XCTAssertEqual(window.hidden, 9)
        XCTAssertEqual(activityStageDots(Array(rows.prefix(3))).hidden, 0)
        XCTAssertEqual(activityStageDots(Array(rows.prefix(4))).hidden, 1)
        XCTAssertEqual(activityStageDots(rows, limit: 5).hidden, 7)
    }

    func testStepsHeaderSaysInspectAndTightensOnlyWhenExpanded() {
        XCTAssertEqual(ActivityStepsHeader.inspectTitle, "Inspect")
        XCTAssertEqual(ActivityStepsHeader.inspectAccessibilityLabel, "Inspect run")
        let collapsed = ActivityStepsHeader.verticalPadding(expanded: false)
        let expanded = ActivityStepsHeader.verticalPadding(expanded: true)
        XCTAssertEqual(collapsed.top, 7)
        XCTAssertEqual(collapsed.bottom, 7)
        XCTAssertEqual(expanded.top, 6)
        XCTAssertEqual(expanded.bottom, 4)
        XCTAssertTrue((4...6).contains(ActivityStepsHeader.chevronTrailingSpace))
    }

    func testActiveRunBreathesTheLatestDotAndRingsTheCaretWithoutASpinner() {
        let live = ActivityStepsHeader.liveIndicators(isRunning: true)
        XCTAssertFalse(live.spinner, "the separate spinner is gone")
        XCTAssertTrue(live.breathingDot)
        XCTAssertTrue(live.chevronRing)
        XCTAssertFalse(live.stepCount, "an active header hides the total step count")
        XCTAssertEqual(
            ActivityStepsHeader.liveIndicators(isRunning: false),
            .init(spinner: false, breathingDot: false, chevronRing: false, stepCount: true)
        )

        let period = ActivityStepsHeader.breathPeriod
        XCTAssertTrue((1.2...1.6).contains(period))
        XCTAssertEqual(ActivityStepsHeader.breathPhase(at: 0), 0, accuracy: 1e-9)
        XCTAssertEqual(ActivityStepsHeader.breathPhase(at: period / 2), 1, accuracy: 1e-9)
        XCTAssertEqual(ActivityStepsHeader.breathPhase(at: period * 3), 0, accuracy: 1e-9)
        XCTAssertEqual(ActivityStepsHeader.ringTurns(at: ActivityStepsHeader.ringPeriod * 2.25), 0.25, accuracy: 1e-9)
    }

    // MARK: Agentic actions (flat loop)

    private func agentic(_ type: String, _ data: [String: Any]) -> [String: Any] {
        var payload: [String: Any] = [
            "execution_id": "exec-1", "plan_id": "plan-1", "step_id": "s1", "iteration": 2,
            "action_type": "tool", "target": "web_fetch", "timestamp": 1_000
        ]
        payload.merge(data) { _, new in new }
        return ["event_type": type, "data": payload]
    }

    func testAgenticActionStartedRunsThenExecutedSettlesTheSameRowByCallId() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(agentic("AgenticActionStarted", ["call_id": "call-7", "timestamp": 1_000]))
        var rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.status, .running)
        XCTAssertEqual(rows.first?.label, "Calling web_fetch")

        sut.ingest(agentic("AgenticActionExecuted", [
            "call_id": "call-7", "timestamp": 1_450, "success": true, "latency_ms": 450
        ]))
        rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1, "the finish updates the started row")
        XCTAssertEqual(rows.first?.status, .done)
        XCTAssertEqual(rows.first?.durationMs, 450)
        XCTAssertEqual(rows.first?.toolName, "web_fetch")
    }

    func testAgenticStartSortsBeforeItsFinishOnATimestampTie() {
        let started = agentic("AgenticActionStarted", ["call_id": "c", "timestamp": 5])
        let executed = agentic("AgenticActionExecuted", ["call_id": "c", "timestamp": 5, "success": false])
        let ordered = ChatViewModel.normalizedActivityEvents([executed, started])
        XCTAssertEqual(ordered.map { $0["event_type"] as? String },
                       ["AgenticActionStarted", "AgenticActionExecuted"])
        let sut = ChatTurnActivityAccumulator()
        ordered.forEach { sut.ingest($0) }
        XCTAssertEqual(sut.snapshot().map(\.status), [.failed])
    }

    func testLegacyAgenticExecutedWithoutCallIdStillMakesADoneRow() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(agentic("AgenticActionExecuted", ["success": true]))
        sut.ingest(agentic("AgenticActionExecuted", ["success": true, "iteration": 3, "timestamp": 2_000]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.map(\.status), [.done, .done], "each legacy action keeps its own composite row")
        XCTAssertEqual(rows.first?.label, "web_fetch returned")
    }

    func testAgenticStartedEventIsOwnedByItsExecution() {
        let event = agentic("AgenticActionStarted", ["call_id": "c"])
        let refs = RealtimeHubRefs.resolve(serverRefs: nil, data: event["data"] as! [String: Any])
        XCTAssertEqual(refs.executionId, "exec-1", "liveTurnOwns matches it by execution_id")
    }

    func testAgenticDetailIsTheRowLabelAndTarget() {
        let sut = ChatTurnActivityAccumulator()
        let detail = "metabase-mcp · execute_sql — SELECT count(*) FROM orders"
        sut.ingest(agentic("AgenticActionStarted", [
            "call_id": "m1", "target": "metabase-mcp__call_tool", "detail": detail
        ]))
        XCTAssertEqual(sut.snapshot().first?.label, "metabase-mcp · execute_sql")
        XCTAssertEqual(sut.snapshot().first?.detail, "SELECT count(*) FROM orders")

        // The finish has no detail; the row keeps the call's name and query.
        sut.ingest(agentic("AgenticActionExecuted", [
            "call_id": "m1", "target": "metabase-mcp__call_tool", "success": true, "timestamp": 1_200
        ]))
        let row = sut.snapshot().first
        XCTAssertEqual(row?.label, "metabase-mcp · execute_sql")
        XCTAssertEqual(row?.detail, "SELECT count(*) FROM orders")
        XCTAssertEqual(row?.status, .done)
        XCTAssertFalse(row?.label.contains("__call_tool") ?? true)
    }

    func testCliLabelNamesTheCommandFamilyWithoutPayloadArguments() {
        XCTAssertEqual(activityToolDisplay(tool: "creative__run", payload: ["args": ["args": ["generate", "create", "model", "--prompt", "private prompt"]]])?.label,
                       "creative · generate create")
        XCTAssertNil(activityToolDisplay(tool: "creative__run", payload: ["args": ["args": ["--token", "private token"]]]))
        XCTAssertEqual(activityToolDisplay(tool: "groceries__call_tool", payload: ["args": ["tool_name": "search_products", "arguments_json": "{}"]])?.label,
                       "groceries · search_products")
    }

    func testCallToolLabelComesFromTheRemoteToolNameAndItsQuery() {
        let longSQL = "SELECT id, name, created_at FROM customers WHERE created_at > now() - interval '30 days' ORDER BY created_at DESC"
        let argumentsJSON = String(decoding: try! JSONSerialization.data(withJSONObject: ["sql": longSQL]), as: UTF8.self)
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(agentEvent("tool.call.started", agent: "analyst", payload: [
            "call_id": "t1", "tool_name": "metabase-mcp__call_tool",
            "args": ["name": "execute_sql", "arguments_json": argumentsJSON]
        ]))
        var row = sut.snapshot().first { $0.kind == .tool }
        XCTAssertEqual(row?.label, "[analyst] metabase-mcp · execute_sql")
        XCTAssertEqual(row?.detail?.count, 80)
        XCTAssertTrue(row?.detail?.hasPrefix("SELECT id, name") ?? false)

        let blob = String(repeating: "x", count: 400)
        sut.ingest(agentEvent("tool.call.finished", agent: "analyst", payload: [
            "call_id": "t1", "tool_name": "metabase-mcp__call_tool", "content_preview": "{\"rows\":\"\(blob)\"}"
        ]))
        row = sut.snapshot().first { $0.kind == .tool }
        XCTAssertEqual(row?.label, "[analyst] metabase-mcp · execute_sql")
        XCTAssertTrue(row?.detail?.hasPrefix("SELECT id, name") ?? false)
        XCTAssertEqual(row?.toolName, "metabase-mcp__call_tool", "the dot tint still hashes the tool")

        // A non-query argument object shows only when it is short; a blob never.
        XCTAssertEqual(
            activityToolDisplay(tool: "metabase-mcp__call_tool",
                                payload: ["args": ["name": "list_tables", "arguments_json": "{\"db\":1}"]]),
            ActivityToolDisplay(label: "metabase-mcp · list_tables", target: "{\"db\":1}")
        )
        XCTAssertNil(activityToolDisplay(
            tool: "metabase-mcp__call_tool",
            payload: ["args": ["name": "dump", "arguments_json": "{\"blob\":\"\(blob)\"}"]]
        )?.target)
        XCTAssertNil(activityToolDisplay(tool: "metabase-mcp__call_tool", payload: [:]))
        XCTAssertNil(activityToolDisplay(tool: "web_fetch", payload: ["args": ["name": "x"]]))
    }

    // MARK: Typed LLM calls (flat loop)

    private func typedLLM(_ type: String, call: String?, _ data: [String: Any]) -> [String: Any] {
        var payload: [String: Any] = [
            "execution_id": "exec-1", "plan_id": "plan-1", "step_id": "s1",
            "capability": "reasoning", "timestamp": 1_700_000_000_000
        ]
        if let call { payload["correlation"] = ["trace_id": "trace-1", "llm_call_id": call] }
        payload.merge(data) { _, new in new }
        return ["event_type": type, "data": payload]
    }

    func testTypedLLMCallRunsThenRoutesThenSettlesOneRow() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(typedLLM("LLMRequestSent", call: "llm-1", [:]))
        var rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.label, "Thinking…")
        XCTAssertEqual(rows.first?.status, .running)
        XCTAssertEqual(rows.first?.startedAtMs, 1_700_000_000_000)

        sut.ingest(typedLLM("LLMRouted", call: "llm-1", [
            "provider": "anthropic", "model": "claude-test", "profile": "reasoning",
            "timestamp": 1_700_000_000_050
        ]))
        rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.label, "Thinking with claude-test")
        XCTAssertEqual(rows.first?.status, .running)

        sut.ingest(typedLLM("LLMResponseReceived", call: "llm-1", [
            "success": true, "latency_ms": 2_400, "timestamp": 1_700_000_002_400,
            "input_tokens": 100, "output_tokens": 20
        ]))
        rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1, "the response settles the request's row in place")
        XCTAssertEqual(rows.first?.label, "Thinking with claude-test", "the routed model stays")
        XCTAssertEqual(rows.first?.status, .done)
        XCTAssertEqual(rows.first?.durationMs, 2_400)
        XCTAssertEqual(rows.first?.callUsage?.callId, "llm-1")
        XCTAssertEqual(rows.first?.startedAtMs, 1_700_000_000_000, "the clock started at the request")
    }

    func testTypedLLMResponseNamesTheModelThatAnsweredAndCanFail() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(typedLLM("LLMRequestSent", call: "a", [:]))
        sut.ingest(typedLLM("LLMRouted", call: "a", ["model": "primary-model"]))
        // A fallback re-routes the same call.
        sut.ingest(typedLLM("LLMRouted", call: "a", ["model": "fallback-model", "timestamp": 1_700_000_000_900]))
        XCTAssertEqual(sut.snapshot().first?.label, "Thinking with fallback-model")
        sut.ingest(typedLLM("LLMResponseReceived", call: "a", [
            "success": false, "error": "rate limited", "model": "answered-model", "timestamp": 1_700_000_001_000
        ]))
        let row = sut.snapshot().first
        XCTAssertEqual(sut.snapshot().count, 1)
        XCTAssertEqual(row?.label, "Thinking with answered-model")
        XCTAssertEqual(row?.status, .failed)
        XCTAssertEqual(row?.tone, .error)
        XCTAssertEqual(row?.detail, "rate limited")
    }

    func testDecisionModelClassificationRetainsMeasuredInputWithZeroOutput() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(typedLLM("LLMResponseReceived", call: "decision-local", [
            "success": true, "model": "local-judge", "response_kind": "decision_model",
            "latency_ms": 80, "input_tokens": 1200, "output_tokens": 0,
            "usage_reported": true, "cost": 0.0,
            "correlation": ["llm_call_id": "decision-local", "usage_availability": [
                "tokens": true, "cache_read": false, "cost": true
            ]]
        ]))
        let row = sut.snapshot().first
        XCTAssertEqual(row?.status, .done)
        XCTAssertEqual(row?.callUsage?.stepTokens, 1200)
        XCTAssertEqual(row?.callUsage?.costUsd, 0.0)
        XCTAssertEqual(row?.durationMs, 80)
    }

    func testUnreportedDecisionModelUsageDoesNotShowTransportPlaceholderZeroes() {
        let usage = ActivityCallUsage.read([
            "input_tokens": 0, "output_tokens": 0, "cost": 0.0, "usage_reported": false,
            "correlation": ["usage_availability": ["tokens": false, "cost": false]]
        ])
        XCTAssertNil(usage.stepTokens)
        XCTAssertNil(usage.inputTokens)
        XCTAssertNil(usage.outputTokens)
        XCTAssertNil(usage.costUsd)
    }

    func testTypedLLMWithoutCallIdKeepsTheOlderBehaviour() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(typedLLM("LLMRequestSent", call: nil, [:]))
        sut.ingest(typedLLM("LLMRouted", call: nil, ["model": "m"]))
        XCTAssertEqual(sut.snapshot(), [], "an older server's request opens no row")
        sut.ingest(typedLLM("LLMResponseReceived", call: nil, [
            "success": true, "latency_ms": 900, "timestamp": 1_700_000_000_900
        ]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.label, "Thinking with reasoning")
        XCTAssertEqual(rows.first?.status, .done)
    }

    func testIdLessTypedResponseSettlesTheOpenRequestOfItsExecution() {
        let sut = ChatTurnActivityAccumulator()
        sut.ingest(typedLLM("LLMRequestSent", call: "open-1", [:]))
        sut.ingest(typedLLM("LLMResponseReceived", call: nil, [
            "success": true, "latency_ms": 300, "timestamp": 1_700_000_000_300
        ]))
        let rows = sut.snapshot()
        XCTAssertEqual(rows.count, 1, "no running row is stranded")
        XCTAssertEqual(rows.first?.status, .done)
        XCTAssertEqual(rows.first?.durationMs, 300)
    }

    /// One model call reaches the turn twice: the chat-side `llm.*` events
    /// (keyed by trace; only `llm.succeeded` names the call id) and the typed
    /// copies (keyed by call id). Either order renders one row, and the
    /// chat-side settle's latency and usage survive the typed response.
    func testChatAndTypedEventsForOneModelCallRenderOneRow() {
        let requested = event("llm.requested", ["trace_id": "t", "model": "chat.default"])
        let succeeded = event("llm.succeeded", [
            "trace_id": "t", "llm_call_id": "call-1", "model": "claude-test",
            "duration_ms": 2100.0, "ttft_ms": 700.0, "input_tokens": 100, "output_tokens": 20
        ])
        let sent = typedLLM("LLMRequestSent", call: "call-1", [:])
        let routed = typedLLM("LLMRouted", call: "call-1", ["model": "claude-test"])
        let response = typedLLM("LLMResponseReceived", call: "call-1", [
            "success": true, "latency_ms": 2_400, "model": "claude-test"
        ])
        let orders: [String: [[String: Any]]] = [
            "typed before the settle": [requested, sent, routed, response, succeeded],
            "typed after the settle": [requested, succeeded, sent, routed, response],
            "typed first": [sent, routed, requested, response, succeeded],
        ]
        for (name, events) in orders {
            let sut = ChatTurnActivityAccumulator()
            events.forEach { sut.ingest($0) }
            let rows = sut.snapshot()
            XCTAssertEqual(rows.count, 1, name)
            XCTAssertEqual(rows.first?.status, .done, name)
            XCTAssertEqual(rows.first?.label, "Thinking with claude-test", name)
            XCTAssertEqual(rows.first?.durationMs, 2100, name)
            XCTAssertEqual(rows.first?.detail, "700ms (2.1s)", name)
            XCTAssertEqual(rows.first?.callUsage?.callId, "call-1", name)
            XCTAssertEqual(rows.first?.callUsage?.stepTokens, 120, name)
            // A reset forgets the alias: the next turn's call opens its own row.
            sut.reset()
            sut.ingest(sent)
            XCTAssertEqual(sut.snapshot().map(\.key), ["system::llm::call::call-1"], name)
        }
    }

    func testTypedLLMLifecycleSortsRequestRouteResponseOnATimestampTie() {
        let response = typedLLM("LLMResponseReceived", call: "t", ["success": true])
        let routed = typedLLM("LLMRouted", call: "t", ["model": "m"])
        let request = typedLLM("LLMRequestSent", call: "t", [:])
        let ordered = ChatViewModel.normalizedActivityEvents([response, routed, request])
        XCTAssertEqual(ordered.map { $0["event_type"] as? String },
                       ["LLMRequestSent", "LLMRouted", "LLMResponseReceived"])
        let sut = ChatTurnActivityAccumulator()
        ordered.forEach { sut.ingest($0) }
        XCTAssertEqual(sut.snapshot().map(\.status), [.done])
        XCTAssertEqual(sut.snapshot().first?.label, "Thinking with m")
    }

    func testRunningRowsShowALiveClockOnlyWhileTheTurnIsLive() {
        let start = 1_700_000_000_000.0
        let running = ActivityRow(key: "k", kind: .llm, label: "Thinking…", detail: nil,
                                  status: .running, tone: .info, durationMs: nil, startedAtMs: start)
        let at = { (seconds: Double) in Date(timeIntervalSince1970: start / 1_000 + seconds) }
        XCTAssertEqual(ActivityLiveClock.label(running, isLive: true, now: at(5.7)), "5s", "whole seconds, never rounded up")
        XCTAssertEqual(ActivityLiveClock.label(running, isLive: true, now: at(65)), "1m 5s")
        XCTAssertEqual(ActivityLiveClock.label(running, isLive: true, now: at(-3)), "0s", "a trailing device clock clamps")
        XCTAssertNil(ActivityLiveClock.label(running, isLive: false, now: at(5)), "a finished turn does not tick")

        var tool = running
        tool.kind = .tool
        XCTAssertTrue(ActivityLiveClock.ticks(tool, isLive: true))

        var settled = running
        settled.status = .done
        settled.durationMs = 2_100
        XCTAssertNil(ActivityLiveClock.label(settled, isLive: true, now: at(5)), "a settled row shows its duration")

        var reasoning = running
        reasoning.kind = .reasoning
        XCTAssertFalse(ActivityLiveClock.ticks(reasoning, isLive: true))

        var unstamped = running
        unstamped.startedAtMs = nil
        XCTAssertFalse(ActivityLiveClock.ticks(unstamped, isLive: true))
    }

    func testTintSlotMatchesWebFnv1a() {
        // Web `tintFor`: FNV-1a 32-bit over UTF-16 units, % 5 + 1.
        XCTAssertEqual(activityTintSlot(""), Int(UInt32(0x811c9dc5) % 5) + 1)
        // Golden values computed with web `tintFor`.
        XCTAssertEqual(activityTintSlot("web_fetch"), 1)
        XCTAssertEqual(activityTintSlot("search"), 4)
        XCTAssertEqual(activityTintSlot("Step 0"), 5)
    }

    private func agentEvent(_ type: String, agent: String, payload: [String: Any]) -> [String: Any] {
        ["event_type": "AgentEvent", "data": ["event": ["event_type": type, "agent_id": agent, "payload": payload]]]
    }
}
