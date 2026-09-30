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
}
