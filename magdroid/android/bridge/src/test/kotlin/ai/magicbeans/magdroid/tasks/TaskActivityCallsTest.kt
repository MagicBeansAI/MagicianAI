package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** One row per call on the task detail's Run timeline: pairing by id, and the id-less fallback. */
class TaskActivityCallsTest {

    @Test
    fun `context start and completion fold by phase without a model call id`() {
        val entries = TaskActivityCalls.fold(listOf(
            row("s", "context.retrieval.started", "running", 1, "\"phase_id\":\"p\""),
            row("e", "context.retrieval.completed", "done", 2, "\"phase_id\":\"p\",\"status\":\"partial\",\"latency_ms\":123"),
        ))
        assertEquals(1, entries.size)
        assertEquals("Context prepared (partial)", TaskActivityCalls.title(entries.single()))
        assertFalse(entries.single().running)
    }

    /** Epoch millis the fixtures count from; [row]'s `at` is an offset from it. */
    private val base = 1_700_000_000_000L

    private fun row(
        id: String,
        event: String,
        status: String,
        at: Long,
        meta: String = "",
        execution: String = "e1",
    ): JsonObject = Json.parseToJsonElement(
        """{"id":"$id","status":"$status","title":"t-$id","created_at":${base + at},
            "metadata":{"event_type":"$event","execution_id":"$execution"${if (meta.isEmpty()) "" else ", $meta"}}}""",
    ).jsonObject

    private fun ids(entries: List<ActivityCallEntry>) = entries.map { TaskActivityCalls.text(it.row, "id") }

    @Test fun modelCallPairsRequestRouteAndResultByLlmCallId() {
        val requested = row("r1", "llm.requested", "running", 1_000, """"llm_call_id":"c1","capability":"decision","started_at_ms":${base + 900}""")
        val routed = row("r2", "llm.routed", "info", 1_050, """"llm_call_id":"c1","model":"claude-sonnet","provider":"anthropic"""")
        val other = row("x", "step.started", "info", 1_100)
        val settled = row("r3", "llm.succeeded", "done", 2_000, """"llm_call_id":"c1","model":"claude-sonnet","cost_usd":0.01""")

        val running = TaskActivityCalls.fold(listOf(requested, routed, other))
        assertEquals(listOf("r1", "x"), ids(running))
        assertTrue(running[0].running)
        assertEquals("Thinking with claude-sonnet", TaskActivityCalls.title(running[0]))
        assertEquals(base + 900, running[0].startedAtMillis)

        val done = TaskActivityCalls.fold(listOf(requested, routed, other, settled))
        // The settled row is drawn in the call's place, after the step it outlived.
        assertEquals(listOf("x", "r3"), ids(done))
        val call = done[1]
        assertFalse(call.running)
        assertEquals("r1", call.start?.let { TaskActivityCalls.text(it, "id") })
        assertEquals("r2", call.routed?.let { TaskActivityCalls.text(it, "id") })
        assertEquals("Thinking with claude-sonnet", TaskActivityCalls.title(call))
    }

    @Test fun requestBeforeAnyRouteReadsThinkingAndTheAnsweringModelWins() {
        val requested = row("r1", "llm.requested", "running", 1_000, """"llm_call_id":"c1","capability":"decision"""")
        val only = TaskActivityCalls.fold(listOf(requested)).single()
        assertEquals("Thinking…", TaskActivityCalls.title(only))
        assertEquals(base + 1_000, only.startedAtMillis)

        val routes = listOf(
            requested,
            row("r2", "llm.routed", "info", 1_010, """"llm_call_id":"c1","model":"primary""""),
            row("r3", "llm.routed", "info", 1_020, """"llm_call_id":"c1","model":"fallback""""),
        )
        assertEquals("Thinking with fallback", TaskActivityCalls.title(TaskActivityCalls.fold(routes).single()))

        val answered = routes + row("r4", "llm.failed", "failed", 1_500, """"llm_call_id":"c1","harness_model":"opus"""")
        val call = TaskActivityCalls.fold(answered).single()
        assertEquals("Thinking with opus", TaskActivityCalls.title(call))
        assertEquals("opus", call.model)
    }

    @Test fun parallelCallsPairByTheirOwnIds() {
        val rows = listOf(
            row("a1", "llm.requested", "running", 1, """"llm_call_id":"a""""),
            row("b1", "llm.requested", "running", 2, """"llm_call_id":"b""""),
            row("b2", "llm.succeeded", "done", 3, """"llm_call_id":"b","model":"mb""""),
            row("a2", "llm.succeeded", "done", 4, """"llm_call_id":"a","model":"ma""""),
        )
        val entries = TaskActivityCalls.fold(rows)
        assertEquals(listOf("b2", "a2"), ids(entries))
        assertEquals(listOf("Thinking with mb", "Thinking with ma"), entries.map(TaskActivityCalls::title))
    }

    @Test fun toolCallPairsStartAndResultByCallId() {
        val started = row("t1", "tool.started", "running", 10, """"call_id":"k1","target":"web_search","started_at_ms":${base + 9}""")
        val running = TaskActivityCalls.fold(listOf(started)).single()
        assertTrue(running.running)
        assertEquals("Using web_search", TaskActivityCalls.title(running))
        assertEquals(base + 9, running.startedAtMillis)

        val failed = row("t2", "tool.failed", "failed", 20, """"call_id":"k1"""")
        val call = TaskActivityCalls.fold(listOf(started, failed)).single()
        assertFalse(call.running)
        // The result names no tool; the start does.
        assertEquals("web_search failed", TaskActivityCalls.title(call))
    }

    @Test fun idlessRequestFoldsIntoTheNextSettledModelRowOfItsExecution() {
        val rows = listOf(
            row("r1", "llm.requested", "running", 1, """"capability":"decision""""),
            row("o1", "llm.succeeded", "done", 2, """"capability":"decision"""", execution = "other"),
            row("r2", "llm.succeeded", "done", 3, """"capability":"decision""""),
        )
        val entries = TaskActivityCalls.fold(rows)
        // Another execution's result never answers e1's request.
        assertEquals(listOf("o1", "r2"), ids(entries))
        assertEquals("r1", entries[1].start?.let { TaskActivityCalls.text(it, "id") })
        assertNull(entries[0].start)
        assertEquals("Thinking with decision", TaskActivityCalls.title(entries[1]))
    }

    @Test fun anUnansweredRequestStaysItsOwnRowAndALaterRequestOpensANewCall() {
        val rows = listOf(
            row("r1", "llm.requested", "running", 1),
            row("r2", "llm.requested", "running", 2),
            row("s2", "llm.succeeded", "done", 3),
        )
        val entries = TaskActivityCalls.fold(rows)
        assertEquals(listOf("r1", "s2"), ids(entries))
        assertTrue(entries[0].running)
        assertEquals("Thinking…", TaskActivityCalls.title(entries[0]))
        assertEquals("Model responded", TaskActivityCalls.title(entries[1]))
    }

    @Test fun anIdlessRequestIsAdoptedByTheFirstIdBearingRowOfItsExecution() {
        val rows = listOf(
            row("r1", "llm.requested", "running", 1),
            row("r2", "llm.routed", "info", 2, """"llm_call_id":"c1","model":"m""""),
            row("r3", "llm.succeeded", "done", 3, """"llm_call_id":"c1""""),
            // Nothing is left open for an id-less result to settle.
            row("r4", "llm.succeeded", "done", 4),
        )
        val entries = TaskActivityCalls.fold(rows)
        assertEquals(listOf("r3", "r4"), ids(entries))
        assertEquals("r1", entries[0].start?.let { TaskActivityCalls.text(it, "id") })
        assertNull(entries[1].start)
    }

    @Test fun idlessToolRowsAndOtherRowsStayOneRowEach() {
        val rows = listOf(
            row("t1", "tool.started", "running", 1, """"target":"a""""),
            row("t2", "tool.succeeded", "done", 2, """"target":"a""""),
            row("n1", "execution.waiting", "needs_action", 3),
        )
        val entries = TaskActivityCalls.fold(rows)
        assertEquals(listOf("t1", "t2", "n1"), ids(entries))
        assertEquals("t-n1", TaskActivityCalls.title(entries[2]))
        assertFalse(entries[2].running)
    }

    @Test fun liveElapsedUsesTheDurationFormatAndNeverGoesNegative() {
        assertEquals("0s", LiveElapsed.text(10_000, 9_000))
        assertEquals("12s", LiveElapsed.text(0, 12_400))
        assertEquals("3m 4s", LiveElapsed.text(0, 184_000))
        assertEquals(1_000L, LiveElapsed.TICK_MILLIS)
    }
}
