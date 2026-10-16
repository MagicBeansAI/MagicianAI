package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The event→row projection, pinned to iOS's `ChatTurnActivityTests` case for
 * case — two phones reading the same turn must say the same things about it.
 */
class ChatTurnActivityTest {

    private fun event(type: String, payload: Map<String, JsonElement> = emptyMap()) =
        buildJsonObject {
            put("event_type", type)
            putJsonObject("data") { payload.forEach { (k, v) -> put(k, v) } }
        }

    private fun agentEvent(type: String, agent: String, payload: Map<String, JsonElement>) =
        buildJsonObject {
            put("event_type", "AgentEvent")
            putJsonObject("data") {
                putJsonObject("event") {
                    put("event_type", type)
                    put("agent_id", agent)
                    putJsonObject("payload") { payload.forEach { (k, v) -> put(k, v) } }
                }
            }
        }

    private fun s(value: String) = JsonPrimitive(value)
    private fun n(value: Number) = JsonPrimitive(value)

    @Test
    fun `internal and unknown events are ignored`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(buildJsonObject { put("event_type", "__events_ready") })
        sut.ingest(event("unknown"))
        assertTrue(sut.snapshot().isEmpty())
    }

    @Test
    fun `a pause event sets state and reset clears everything`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event("HitlRequested", mapOf("correlation_id" to s("c1"), "prompt" to s("  Choose   wisely "))),
        )
        assertTrue(sut.pauseActive)
        assertEquals("pause:c1", sut.snapshotRows().first().key)
        assertEquals("Choose wisely", sut.snapshotRows().first().detail)
        assertEquals("waiting", sut.snapshot().first().status)
        sut.reset()
        assertFalse(sut.pauseActive)
        assertTrue(sut.snapshot().isEmpty())
    }

    private fun innerEvent(type: String, payload: Map<String, JsonElement> = emptyMap()) =
        buildJsonObject {
            put("event_type", "AgentEvent")
            putJsonObject("data") {
                putJsonObject("event") {
                    put("event_type", type)
                    putJsonObject("payload") { payload.forEach { (k, v) -> put(k, v) } }
                }
            }
        }

    @Test
    fun `an answer with a matching correlation settles only that pause row`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("HitlRequested", mapOf("correlation_id" to s("c1"), "prompt" to s("Approve plan?"))))
        sut.ingest(event("HitlRequested", mapOf("correlation_id" to s("c2"), "prompt" to s("Pick one"))))
        assertTrue(activityAwaitsUser(sut.snapshot()))

        sut.ingest(innerEvent("hitl.resolved", mapOf("correlation_id" to s("c1"))))
        val rows = sut.snapshotRows()
        val first = rows.first { it.key == "pause:c1" }
        assertEquals("Answered", first.label)
        assertEquals(ActivityRowStatus.Done, first.status)
        assertEquals(ActivityRowTone.Info, first.tone)
        assertEquals("Approve plan?", first.detail)
        assertEquals(ActivityRowStatus.Waiting, rows.first { it.key == "pause:c2" }.status)
        assertTrue("c2 still waits", sut.pauseActive)
        assertTrue(activityAwaitsUser(sut.snapshot()))

        sut.ingest(innerEvent("input.received", mapOf("correlation_id" to s("c2"))))
        assertFalse(sut.pauseActive)
        assertFalse(activityAwaitsUser(sut.snapshot()))
    }

    @Test
    fun `an id-less answer settles every waiting pause row`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("HitlRequested", mapOf("correlation_id" to s("a"))))
        sut.ingest(innerEvent("input.requested", mapOf("request_id" to s("b"))))
        sut.ingest(innerEvent("input.resolved"))
        assertEquals(listOf("Answered", "Answered"), sut.snapshot().map { it.label })
        assertFalse(sut.pauseActive)
    }

    @Test
    fun `an id-less pause row settles with any answer and unrelated ids stay waiting`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("HitlRequested")) // files under pause:pause
        sut.ingest(event("HitlRequested", mapOf("correlation_id" to s("keep"))))
        sut.ingest(event("HitlResolved", mapOf("pause_state_id" to s("other"))))
        val rows = sut.snapshotRows()
        assertEquals(ActivityRowStatus.Done, rows.first { it.key == "pause:pause" }.status)
        assertEquals(ActivityRowStatus.Waiting, rows.first { it.key == "pause:keep" }.status)
        assertTrue(sut.pauseActive)
    }

    @Test
    fun `an answer settles nothing already done and ignores non-pause rows`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(innerEvent("step.started", mapOf("step_id" to s("s1"), "step_name" to s("Fetch"))))
        sut.ingest(innerEvent("hitl.resolved"))
        assertTrue(sut.snapshot().first().label != "Answered")
        assertFalse(sut.pauseActive)
    }

    @Test
    fun `the llm lifecycle coalesces to one row and formats timing`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", mapOf("trace_id" to s("t"), "model" to s("gpt-test"))))
        sut.ingest(event("llm.first_token", mapOf("trace_id" to s("t"), "duration_ms" to n(700))))
        sut.ingest(
            event(
                "llm.succeeded",
                mapOf("trace_id" to s("t"), "duration_ms" to n(2100.0), "ttft_ms" to n(700.0)),
            ),
        )
        val rows = sut.snapshotRows()
        assertEquals(1, rows.size)
        assertEquals("Thinking with gpt-test", rows.first().label)
        assertEquals("700ms (2.1s)", rows.first().detail)
        assertEquals(ActivityRowStatus.Done, rows.first().status)
        assertEquals(2100.0, rows.first().durationMs!!, 0.0)
    }

    @Test
    fun `llm succeeded relabels the row with the model that actually answered`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", mapOf("trace_id" to s("t"), "model" to s("chat.default"))))
        sut.ingest(
            event(
                "llm.succeeded",
                mapOf("trace_id" to s("t"), "model" to s("Claude Code · opus"), "duration_ms" to n(1200.0)),
            ),
        )
        val rows = sut.snapshotRows()
        assertEquals(1, rows.size)
        assertEquals("Thinking with Claude Code · opus", rows.first().label)
        assertEquals(ActivityRowStatus.Done, rows.first().status)

        // A completion without a model keeps the request-time label.
        val keep = ChatTurnActivityAccumulator()
        keep.ingest(event("llm.requested", mapOf("trace_id" to s("t"), "model" to s("gpt-test"))))
        keep.ingest(event("llm.succeeded", mapOf("trace_id" to s("t"), "duration_ms" to n(900.0))))
        assertEquals("Thinking with gpt-test", keep.snapshotRows().first().label)
    }

    @Test
    fun `a terminal task status settles only that task's running row and then its leaves`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("task.status_changed", mapOf("task_id" to s("task-1"), "status" to s("running"), "display_label" to s("researcher"))))
        sut.ingest(event("task.status_changed", mapOf("task_id" to s("task-2"), "status" to s("running"), "display_label" to s("writer"))))
        sut.ingest(event("tool.call.started", mapOf("call_id" to s("c"), "tool_name" to s("search"))))
        val running = sut.snapshot()

        val partly = settleActivityRows(running, mapOf("task-1" to "completed"))
        assertEquals("done", partly.first { it.key == "task::task-1" }.status)
        assertEquals("researcher completed", partly.first { it.key == "task::task-1" }.label)
        assertEquals("running", partly.first { it.key == "task::task-2" }.status)
        assertEquals("running", partly.first { it.toolName == "search" }.status)

        val all = settleActivityRows(running, mapOf("task-1" to "completed", "task-2" to "failed"))
        assertEquals("failed", all.first { it.key == "task::task-2" }.status)
        assertEquals("error", all.first { it.key == "task::task-2" }.tone)
        assertEquals("failed", all.first { it.toolName == "search" }.status)
        assertEquals(running, settleActivityRows(running, emptyMap()))
    }

    @Test
    fun `an agent envelope prefixes a delegated tool and reports its failure`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            agentEvent(
                "tool.call.started", agent = "researcher",
                payload = mapOf("call_id" to s("c"), "tool_name" to s("search")),
            ),
        )
        sut.ingest(
            agentEvent(
                "tool.call.failed", agent = "researcher",
                payload = mapOf("call_id" to s("c"), "tool_name" to s("search"), "error" to s("network down")),
            ),
        )
        val row = sut.snapshotRows().first()
        assertEquals("[researcher] search failed", row.label)
        assertEquals("network down", row.detail)
        assertEquals(ActivityRowStatus.Failed, row.status)
        assertEquals(ActivityRowTone.Error, row.tone)
    }

    @Test
    fun `projected tool result preserves its canonical reader identity`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event(
                "tool.call.started",
                mapOf("call_id" to s("call-7"), "tool_name" to s("search_memory")),
            ),
        )
        sut.ingest(
            event(
                "tool.result.projected",
                mapOf(
                    "tool_call_id" to s("call-7"),
                    "tool_name" to s("search_memory"),
                    "result_ref" to s("result_ref_opaque"),
                    "content_hash" to s("blake3:verified"),
                    "size_bytes" to n(71_423),
                    "result_owner" to buildJsonObject {
                        put("kind", "chat")
                        put("session_id", "session-parent")
                    },
                    "task_id" to s("task-7"),
                    "execution_id" to s("exec-7"),
                ),
            ),
        )

        val row = sut.snapshot().single()
        assertEquals("result_ref_opaque", row.resultRef)
        assertEquals("blake3:verified", row.resultHash)
        assertEquals(71_423, row.resultSizeBytes)
        assertEquals("chat", row.resultOwner?.kind)
        assertEquals("session-parent", row.resultOwner?.sessionId)
        assertEquals("task-7", row.taskId)
        assertEquals("exec-7", row.executionId)
    }

    @Test
    fun `a delegate tool describes its parallel targets`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event(
                "tool.call.started",
                mapOf(
                    "call_id" to s("d"), "tool_name" to s("delegate_to_agent"),
                    "args" to buildJsonObject {
                        putJsonArray("delegation_targets") {
                            add(buildJsonObject { put("target_agent_id", "a") })
                            add(buildJsonObject { put("target_agent_id", "b") })
                        }
                    },
                ),
            ),
        )
        assertEquals("Decomposing into 2 parallel agents", sut.snapshot().first().label)
        assertEquals("a, b", sut.snapshot().first().detail)
    }

    @Test
    fun `reasoning content merges and trims`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("reasoning.content", mapOf("trace_id" to s("r"), "delta" to s("first"))))
        sut.ingest(event("reasoning.content", mapOf("trace_id" to s("r"), "delta" to s("second"))))
        sut.ingest(event("reasoning.end", mapOf("trace_id" to s("r"), "duration_ms" to n(12))))
        val row = sut.snapshotRows().first()
        assertEquals("first second", row.detail)
        assertEquals(ActivityRowStatus.Done, row.status)
        assertEquals(12.0, row.durationMs!!, 0.0)
    }

    @Test
    fun `step, artifact, tutor and coding lifecycles all land`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("step.started", mapOf("step_id" to s("compile"))))
        sut.ingest(event("step.completed", mapOf("step_id" to s("compile"))))
        sut.ingest(event("artifact.created", mapOf("name" to s("report.md"))))
        sut.ingest(event("tutor.run.completed", mapOf("run_id" to s("tutor-1"))))
        sut.ingest(event("coding.failed", mapOf("shadow_workspace_id" to s("w"), "error" to s("conflict"))))
        val rows = sut.snapshotRows()
        assertEquals(
            listOf(
                ActivityRowStatus.Done, ActivityRowStatus.Done,
                ActivityRowStatus.Done, ActivityRowStatus.Failed,
            ),
            rows.map { it.status },
        )
        assertEquals("Produced report.md", rows[1].label)
        assertNull(rows[1].detail)
        assertEquals("Tutor completed", rows[2].label)
        assertEquals("conflict", rows[3].detail)
    }

    /** The flat loop's typed action event lands in the same tool branch. */
    @Test
    fun `an agentic action executed normalizes into a tool row`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event(
                "AgenticActionExecuted",
                mapOf(
                    "success" to JsonPrimitive(false), "target" to s("web_search"),
                    "step_id" to s("s1"), "iteration" to n(2), "timestamp" to n(99),
                    "error" to s("timed out"), "latency_ms" to n(1500),
                ),
            ),
        )
        val row = sut.snapshotRows().first()
        assertEquals("web_search failed", row.label)
        assertEquals("timed out", row.detail)
        assertEquals(ActivityRowStatus.Failed, row.status)
    }

    @Test
    fun `an agentic action shows running at its start and settles the same row via call_id`() {
        val start = event(
            "AgenticActionStarted",
            mapOf(
                "execution_id" to s("e1"), "plan_id" to s("p1"), "step_id" to s("s1"),
                "iteration" to n(2), "action_type" to s("tool"), "target" to s("metabase-mcp__call_tool"),
                "call_id" to s("call-7"), "detail" to s("metabase-mcp · execute_sql — SELECT count(*) FROM orders"),
                "timestamp" to n(1_700_000_000_000L),
            ),
        )
        val done = event(
            "AgenticActionExecuted",
            mapOf(
                "execution_id" to s("e1"), "plan_id" to s("p1"), "step_id" to s("s1"),
                "iteration" to n(2), "action_type" to s("tool"), "target" to s("metabase-mcp__call_tool"),
                "call_id" to s("call-7"), "success" to JsonPrimitive(true), "latency_ms" to n(420),
                "timestamp" to n(1_700_000_000_900L),
            ),
        )
        val running = chatTurnActivityRows(listOf(start))
        assertEquals(1, running.size)
        assertEquals("running", running[0].status)
        assertEquals("metabase-mcp · execute_sql — SELECT count(*) FROM orders", running[0].label)
        assertEquals("e1", running[0].executionId)

        // Delivered out of order, the start still sorts first and the finish settles its row.
        val rows = chatTurnActivityRows(listOf(done, start))
        assertEquals(1, rows.size)
        assertEquals("done", rows[0].status)
        assertEquals("metabase-mcp · execute_sql — SELECT count(*) FROM orders", rows[0].label)
        assertEquals(420.0, rows[0].durationMs)
        assertEquals("metabase-mcp__call_tool", rows[0].toolName)
    }

    @Test
    fun `a legacy agentic completion without call_id still lands as one row`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event(
                "AgenticActionExecuted",
                mapOf(
                    "success" to JsonPrimitive(true), "target" to s("web_search"),
                    "step_id" to s("s1"), "iteration" to n(2), "timestamp" to n(99),
                ),
            ),
        )
        val row = sut.snapshotRows().single()
        assertTrue(row.key.endsWith("::tool::s1-2-99"))
        assertEquals("web_search returned", row.label)
        assertEquals(ActivityRowStatus.Done, row.status)
    }

    @Test
    fun `an agentic failure with a detail keeps the detail as its label`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            event(
                "AgenticActionExecuted",
                mapOf(
                    "success" to JsonPrimitive(false), "target" to s("web_search"), "call_id" to s("c9"),
                    "detail" to s("web_search — lisbon museums"), "error" to s("timed out"),
                ),
            ),
        )
        val row = sut.snapshotRows().single()
        assertEquals("web_search — lisbon museums", row.label)
        assertEquals("timed out", row.detail)
        assertEquals(ActivityRowStatus.Failed, row.status)
    }

    @Test
    fun `a call_tool row names the pack and remote tool with a short argument summary`() {
        val sut = ChatTurnActivityAccumulator()
        val sql = "SELECT id, name, created_at FROM customers WHERE created_at > now() - interval '30 days' ORDER BY created_at DESC"
        sut.ingest(
            agentEvent(
                "tool.call.started", agent = "__system__",
                payload = mapOf(
                    "call_id" to s("c1"), "tool_name" to s("metabase-mcp__call_tool"),
                    "args" to buildJsonObject {
                        put("name", "execute_sql")
                        put("arguments_json", buildJsonObject { put("database_id", 1); put("sql", sql) }.toString())
                    },
                ),
            ),
        )
        val started = sut.snapshotRows().single()
        assertTrue(started.label.startsWith("Calling metabase-mcp · execute_sql — SELECT id, name"))
        assertTrue(started.label.length <= "Calling ".length + ACTIVITY_TOOL_LABEL_MAX)
        assertFalse(started.label.contains("{"))
        assertEquals("metabase-mcp__call_tool", started.toolName)

        // The finish carries no args: the row keeps the derived name.
        sut.ingest(
            agentEvent(
                "tool.call.finished", agent = "__system__",
                payload = mapOf("call_id" to s("c1"), "tool_name" to s("metabase-mcp__call_tool")),
            ),
        )
        val finished = sut.snapshotRows().single()
        assertTrue(finished.label.startsWith("metabase-mcp · execute_sql — SELECT id, name"))
        assertTrue(finished.label.endsWith(" returned"))
    }

    @Test
    fun `call_tool display derivation`() {
        val args = { name: String?, json: String? ->
            buildJsonObject {
                name?.let { put("name", it) }
                json?.let { put("arguments_json", it) }
            }
        }
        assertNull(callToolDisplayName("web_search", args("x", null)))
        assertEquals("metabase-mcp", callToolDisplayName("metabase-mcp__call_tool", null))
        assertEquals("metabase-mcp · list_databases", callToolDisplayName("metabase-mcp__call_tool", args("list_databases", "{}")))
        assertEquals(
            "metabase-mcp · search — revenue",
            callToolDisplayName("metabase-mcp__call_tool", args("search", """{"term":"revenue","limit":5}""")),
        )
        // Short raw arguments may show; long raw JSON never does.
        assertEquals(
            "metabase-mcp · get_card — {\"card_id\":12}",
            callToolDisplayName("metabase-mcp__call_tool", args("get_card", """{"card_id":12}""")),
        )
        val long = buildJsonObject { (1..20).forEach { put("field_$it", "value number $it") } }.toString()
        assertEquals("metabase-mcp · update", callToolDisplayName("metabase-mcp__call_tool", args("update", long)))
        val longQuery = "q".repeat(200)
        val shown = requireNotNull(callToolDisplayName("p__call_tool", args("run", """{"query":"$longQuery"}""")))
        assertEquals("p · run — ".length + CALL_TOOL_SUMMARY_MAX, shown.length)
    }

    /**
     * Durable replay can miss a child terminal event; when every task row is
     * terminal, stale running leaves settle rather than spin forever.
     */
    @Test
    fun `a terminal task settles the running leaves it left behind`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("tool.call.started", mapOf("call_id" to s("c"), "tool_name" to s("grep"))))
        sut.ingest(
            event(
                "task.status_changed",
                mapOf("task_id" to s("t1"), "display_label" to s("Researcher"), "status" to s("completed")),
            ),
        )
        val rows = sut.snapshotRows()
        val tool = rows.first { it.key.endsWith("::tool::c") }
        assertEquals(ActivityRowStatus.Done, tool.status)
        assertEquals("Settled when task finished", tool.detail)
        assertEquals("Researcher completed", rows.first { it.key == "task::t1" }.label)
    }

    /** Terminal transitions move a long-running row to the tail, as web does. */
    @Test
    fun `a terminal transition moves a long-running row to the tail`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("tool.call.started", mapOf("call_id" to s("slow"), "tool_name" to s("crawler"))))
        sut.ingest(event("step.started", mapOf("step_id" to s("later"))))
        sut.ingest(
            event(
                "tool.call.finished",
                mapOf("call_id" to s("slow"), "tool_name" to s("crawler"), "duration_ms" to n(9000)),
            ),
        )
        assertEquals("crawler returned", sut.snapshotRows().last().label)
    }

    @Test
    fun `duplicated events collapse and file order yields to timestamps`() {
        val started = buildJsonObject {
            put("event_type", "AgentEvent")
            putJsonObject("data") {
                putJsonObject("event") {
                    put("event_type", "tool.call.started")
                    putJsonObject("payload") {
                        put("event_id", "e1"); put("call_id", "c"); put("tool_name", "search")
                        put("timestamp_ms", 1000)
                    }
                }
            }
        }
        val finished = buildJsonObject {
            put("event_type", "AgentEvent")
            putJsonObject("data") {
                putJsonObject("event") {
                    put("event_type", "tool.call.finished")
                    putJsonObject("payload") {
                        put("event_id", "e2"); put("call_id", "c"); put("tool_name", "search")
                        put("timestamp_ms", 2000)
                    }
                }
            }
        }
        // Completion first in the file, start duplicated — normalization must
        // dedupe and re-order so the terminal state wins.
        val rows = chatTurnActivityRows(listOf(finished, started, started))
        assertEquals(1, rows.size)
        assertEquals("search returned", rows.first().label)
        assertEquals("done", rows.first().status)
    }

    @Test
    fun `the pure helpers hold their boundaries`() {
        assertTrue(isPauseEvent(outer = "", event = "input.requested"))
        assertFalse(isPauseEvent(outer = "", event = "input.completed"))
        assertEquals("a b", trimTo(" a   b ", 10))
        assertEquals("1234…", trimTo("123456", 5))
        assertEquals("1000ms", formatLatency(999.5))
        assertEquals("1.2s", formatLatency(1250.0))
        assertEquals("—", formatLatency(-1.0))
        assertEquals("—", formatLatency(Double.POSITIVE_INFINITY))
        assertEquals("… (1.0s)", formatLatencyPair(null, 1000.0))
    }

    @Test
    fun `snapshot projects to the words the strip renders`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(event("llm.requested", mapOf("trace_id" to s("t"), "model" to s("m"))))
        val public = sut.snapshot().first()
        assertEquals("Thinking with m", public.label)
        assertEquals("running", public.status)
        assertNull(public.detail)
    }

    @Test
    fun `tool rows keep their tool name for the dot tint`() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(
            agentEvent(
                "tool.call.started", agent = "researcher",
                payload = mapOf("call_id" to s("c"), "tool_name" to s("web_fetch")),
            ),
        )
        val row = sut.snapshot().first()
        assertEquals("web_fetch", row.toolName)
        assertEquals("web_fetch", activityTintKey(row))
    }

    @Test
    fun `stage dots show the latest three and fold the rest, the running row last`() {
        assertEquals(3, ACTIVITY_STAGE_DOT_LIMIT)
        val settled = (0 until 12).map { ActivityRow(label = "Step $it", status = "done") }
        val rows = listOf(ActivityRow(label = "Calling old", status = "running")) + settled +
            ActivityRow(label = "Calling search", status = "running")
        val window = activityStageDots(rows)
        // The newest running row is the latest (breathing) dot; older running rows wait.
        assertEquals(listOf("Step 10", "Step 11", "Calling search"), window.shown.map { it.label })
        assertEquals(10, window.hidden)
        assertEquals(0, activityStageDots(settled.take(3)).hidden)
        assertEquals(1, activityStageDots(settled.take(4)).hidden)
        assertEquals(listOf("Step 9", "Step 10", "Step 11"), activityStageDots(settled).shown.map { it.label })
    }

    @Test
    fun `tint slot matches web fnv-1a`() {
        // Golden values computed with web `tintFor`.
        assertEquals(1, activityTintSlot("web_fetch"))
        assertEquals(4, activityTintSlot("search"))
        assertEquals(5, activityTintSlot("Step 0"))
    }
}
