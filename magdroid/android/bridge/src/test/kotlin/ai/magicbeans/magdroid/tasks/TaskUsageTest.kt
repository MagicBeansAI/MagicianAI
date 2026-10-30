package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.chat.ChatTurnActivityAccumulator
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Model usage per task and per turn. One-for-one port of iOS
 * `TaskUsageModelTests` (web `taskUsage.test.ts`, `runCost`, `formatUsd`,
 * `formatTokenCount`, `llmUsageSuffix`).
 */
class TaskUsageTest {
    private fun json(text: String): JsonObject = Json.parseToJsonElement(text).jsonObject

    // Ledger (web taskUsage.test.ts)

    @Test fun sums_calls_tokens_and_cost_per_task() {
        val ledger = TaskUsageLedger()
        ledger.record("task-1", "call-a", 1200, 0.01)
        ledger.record("task-1", "call-b", 800, null)
        ledger.record("task-2", "call-c", 50, 0.002)
        assertEquals(TaskUsage(2, 2000, 0.01), ledger.total("task-1")?.usage)
        assertEquals(TaskUsage(1, 50, 0.002), ledger.total("task-2")?.usage)
    }

    @Test fun counts_a_replayed_call_once() {
        val ledger = TaskUsageLedger()
        assertTrue(ledger.record("task-1", "call-a", 1200, 0.01))
        assertFalse(ledger.record("task-1", "call-a", 1200, 0.01))
        assertEquals(1, ledger.total("task-1")?.usage?.calls)
    }

    @Test fun reports_no_cost_until_a_call_is_priced() {
        val ledger = TaskUsageLedger()
        ledger.record("task-1", "call-a", 100, null)
        assertNull(ledger.total("task-1")?.usage?.costUsd)
    }

    @Test fun nothing_reported_is_no_usage_rather_than_zeros() {
        assertNull(TaskUsageLedger().total("task-1"))
        assertNull(TaskUsageModel.total(live = null, snapshot = TaskUsageSnapshot(run = null, planning = null)))
    }

    @Test fun late_subscription_unions_stored_live_and_auxiliary_calls() {
        val ledger = TaskUsageLedger()
        val stored = mapOf("a" to TaskCallUsage(1000, 0.01, "run"))
        ledger.setSnapshot(TaskUsageSnapshot(TaskUsageModel.sumCalls(stored), TaskUsage(2, 900, 0.01), "run", stored), "task")
        ledger.record("task", "a", 1000, null, "run")
        ledger.record("task", "b", 100, null, "run")
        val history = TaskRunEvidence.history(json("""{"columns":["llm_call_id","input_tokens","output_tokens","cost_usd"],
            "rows":[["a",800,200,0.01],["judge",20,10,0.002]]}"""), "run")
        ledger.setSnapshot(TaskUsageSnapshot(TaskUsageModel.sumCalls(history), null, "run", history), "task")
        val total = ledger.total("task")
        assertEquals(3, total?.run?.calls)
        assertEquals(1130, total?.run?.tokens)
        assertEquals(2, total?.planning?.calls)
        assertEquals(5, total?.usage?.calls)
        assertEquals(2030, total?.usage?.tokens)
        assertEquals(0.022, total?.usage?.costUsd ?: -1.0, 1e-12)
    }

    @Test fun replayed_price_only_call_is_enriched_without_duplicating_it() {
        val ledger = TaskUsageLedger()
        ledger.record("task", "a", null, 0.01)
        assertEquals("1 call · tokens unavailable · $0.010", TaskUsageFormat.taskLine(ledger.total("task")!!.usage))
        ledger.record("task", "a", 250, null)
        assertEquals(TaskUsage(1, 250, 0.01), ledger.total("task")?.usage)
    }

    @Test fun partial_snapshots_keep_history_but_new_executions_reset_it() {
        val ledger = TaskUsageLedger()
        fun snapshot(root: String, at: Double, ids: Set<String>): TaskUsageSnapshot {
            val calls = mapOf(root to TaskCallUsage(100, null, root))
            return TaskUsageSnapshot(TaskUsageModel.sumCalls(calls), null, root, calls, ids, at)
        }
        ledger.setSnapshot(snapshot("old", 10.0, setOf("a", "b")), "task")
        ledger.setSnapshot(snapshot("old", 9.0, setOf("b")), "task")
        assertEquals(2, ledger.total("task")?.activityCount)
        ledger.setSnapshot(snapshot("new", 20.0, setOf("c")), "task")
        assertFalse(ledger.setSnapshot(snapshot("old", 30.0, setOf("a")), "task"))
        assertFalse(ledger.record("task", "late", 999, null, "old"))
        assertEquals("new", ledger.total("task")?.executionId)
        assertEquals(100, ledger.total("task")?.usage?.tokens)
        assertEquals(1, ledger.total("task")?.activityCount)
    }

    @Test fun work_evidence_requires_matching_root_and_actual_work() {
        val panel = json("""{"overview":{"execution_id":"run"},"run":{"activity_log":[
            {"created_at":2000,"metadata":{"event_type":"tool.call.started"}},
            {"created_at":9000,"metadata":{"event_type":"heartbeat"}},
            {"created_at":10000,"metadata":{"event_type":"llm.succeeded","provider_attempt_count":0}}]}}""")
        assertEquals(2000L, TaskRunEvidence.progressAt(panel, "run"))
        assertNull(TaskRunEvidence.progressAt(panel, "another-run"))
        assertFalse(TaskRunEvidence.isProgress("metrics.updated"))
        val event = json("""{"event_type":"AgentEvent","data":{"event":{"event_type":"llm.first_token",
            "principal":"owner","workspace":"default","timestamp":5000,
            "payload":{"execution_id":"child","root_execution_id":"run"}}}}""")
        assertEquals("run", parseTaskRealtimeEvent(event, "owner", "default")?.executionId)
        assertNull(parseTaskRealtimeEvent(event, "other", "default"))
        val first = TaskDetailBundle(panel = panel).withTaskProgress()
        val stale = TaskDetailBundle(panel = json("""{"overview":{"execution_id":"run"}}""")).withTaskProgress(first)
        assertEquals(2000L, stale.observedProgressAt)
        assertNull(TaskDetailBundle(panel = json("""{"overview":{"execution_id":"new"}}""")).withTaskProgress(stale).observedProgressAt)
    }

    private fun round(value: Double?): Double? = value?.let { Math.round(it * 1e10) / 1e10 }

    // Snapshot from /execution-panel

    @Test fun reads_planning_usage_from_the_planning_timeline() {
        val panel = json("""{
            "overview": {"task_id": "task-1", "status": "ready"},
            "run": {},
            "debug": {
                "timeline": [],
                "planning_timeline": [
                    {"id": "a", "timestamp": 1, "severity": "info", "title": "Progress", "message": "Outlined", "cost_usd": 0.02, "tokens": 9000},
                    {"id": "b", "timestamp": 2, "severity": "info", "title": "Progress", "message": "Outlining the plan"},
                    {"id": "c", "timestamp": 3, "severity": "info", "title": "Progress", "message": "Worked out the steps", "cost_usd": 0.03, "tokens": 27000}
                ]
            }
        }""")
        val snapshot = TaskUsageModel.usageSnapshot(panel)
        assertNull(snapshot.run)
        assertEquals(2, snapshot.planning?.calls)
        assertEquals(36000, snapshot.planning?.tokens)
        assertEquals(0.05, snapshot.planning?.costUsd ?: -1.0, 1e-10)
    }

    /** A boot-time backfill writes one summary row per planning execution, `llm_calls` standing for the calls it sums. */
    @Test fun a_backfilled_planning_row_counts_its_calls() {
        val panel = json("""{"debug": {"planning_timeline": [
            {"id": "a", "message": "Planning model usage", "planning_stage": "llm", "llm_calls": 4, "tokens": 12000, "cost_usd": 0.04},
            {"id": "b", "message": "Outlined", "tokens": 1000, "cost_usd": 0.01}
        ]}}""")
        val planning = TaskUsageModel.usageSnapshot(panel).planning
        assertEquals(5, planning?.calls)
        assertEquals(13000, planning?.tokens)
    }

    /** Coordinator contract: a lone `llm_calls: 4` row with tokens and cost is four calls. */
    @Test fun a_lone_backfilled_row_with_four_calls_is_four_calls() {
        val panel = json("""{"debug": {"planning_timeline": [
            {"id": "a", "message": "Planning model usage", "planning_stage": "llm",
             "metadata": {"backfilled": true}, "llm_calls": 4, "tokens": 8000, "cost_usd": 0.02}
        ]}}""")
        val planning = TaskUsageModel.usageSnapshot(panel).planning
        assertEquals(4, planning?.calls)
        assertEquals(8000, planning?.tokens)
        assertEquals(0.02, planning?.costUsd ?: -1.0, 1e-12)
    }

    @Test fun an_older_payload_without_planning_usage_has_none() {
        val panel = json("""{"debug": {"planning_timeline": [{"id": "a", "message": "Outlined"}]}}""")
        assertNull(TaskUsageModel.usageSnapshot(panel).planning)
    }

    @Test fun run_usage_sums_the_activity_log_like_run_cost() {
        val panel = json("""{"run": {
            "activity_log": [
                {"id": "1", "metadata": {"event_type": "llm.requested"}},
                {"id": "2", "metadata": {"event_type": "llm.succeeded", "input_tokens": 1000, "output_tokens": 200, "cache_read_tokens": 800, "cost_usd": 0.01}},
                {"id": "3", "metadata": {"event_type": "llm.succeeded", "output_tokens": 50, "cost": 0.002}},
                {"id": "4", "metadata": {"event_type": "tool.call.finished", "cost_usd": 9}},
                {"id": "5", "metadata": {"event_type": "llm.succeeded", "cache_read_tokens": 10}}
            ],
            "recent_activity": [{"id": "x", "metadata": {"event_type": "llm.succeeded", "input_tokens": 99999}}]
        }}""")
        val run = TaskUsageModel.usageSnapshot(panel).run
        assertEquals(3, run?.calls)
        assertEquals(1250, run?.tokens)
        assertEquals(0.012, run?.costUsd ?: -1.0, 1e-12)
    }

    @Test fun run_usage_falls_back_to_recent_activity_and_is_null_when_nothing_reported() {
        val recent = json("""{"run": {"activity_log": [], "recent_activity": [
            {"id": "1", "metadata": {"event_type": "llm.succeeded", "input_tokens": 10, "output_tokens": 5}}]}}""")
        assertEquals(TaskUsage(1, 15, null), TaskUsageModel.usageSnapshot(recent).run)

        val unbilled = json("""{"run": {"activity_log": [
            {"id": "1", "metadata": {"event_type": "llm.succeeded", "latency_ms": 900}}]}}""")
        assertNull(TaskUsageModel.usageSnapshot(unbilled).run)
        assertNull(TaskUsageModel.usageSnapshot(JsonObject(emptyMap())).run)
    }

    @Test fun a_boolean_is_not_a_token_count() {
        val panel = json("""{"run": {"activity_log": [
            {"id": "1", "metadata": {"event_type": "llm.succeeded", "input_tokens": true}}]}}""")
        assertNull(TaskUsageModel.usageSnapshot(panel).run)
    }

    // Formatting

    /**
     * The cost rule shared with web `formatUsd` and magios
     * `TaskTimelineFormatting.usd`: three decimals below a dollar, two from a
     * dollar up, half-up, and a positive amount under half a thousandth reads
     * `<$0.001`. The same cases on all three platforms.
     */
    @Test fun usd_matches_web_format_usd() {
        val cases = listOf<Pair<Double?, String?>>(
            0.0 to "$0.000",
            0.0001 to "<$0.001",
            0.0004 to "<$0.001",
            0.0005 to "$0.001",
            0.00503936 to "$0.005",
            0.04 to "$0.040",
            0.999 to "$0.999",
            0.9996 to "$1.00",
            1.0 to "$1.00",
            3.225331 to "$3.23",
            12.0 to "$12.00",
            null to null,
        )
        for ((amount, expected) in cases) assertEquals("$amount", expected, TaskUsageFormat.usd(amount))
        assertNull(TaskUsageFormat.usd(-1.0))
        assertNull(TaskUsageFormat.usd(Double.NaN))
    }

    @Test fun spoken_usd_says_less_than_a_tenth_of_a_cent() {
        assertEquals("less than a tenth of a cent", TaskUsageFormat.spokenUsd(0.0001))
        assertEquals("$3.23", TaskUsageFormat.spokenUsd(3.225331))
        assertNull(TaskUsageFormat.spokenUsd(null))
        assertEquals(
            "1 model call, 10 tokens, less than a tenth of a cent",
            TaskUsageFormat.taskAccessibilityLabel(TaskUsageTotal(TaskUsage(1, 10, 0.0002), null, null)),
        )
    }

    @Test fun compact_tokens() {
        assertEquals("999", TaskUsageFormat.compactTokens(999))
        assertEquals("1.0k", TaskUsageFormat.compactTokens(1000))
        assertEquals("12.3k", TaskUsageFormat.compactTokens(12_345))
        assertEquals("0", TaskUsageFormat.compactTokens(0))
    }

    @Test fun token_count_matches_web_format_token_count() {
        assertEquals("0", TaskUsageFormat.tokenCount(0))
        assertEquals("850", TaskUsageFormat.tokenCount(850))
        assertEquals("9.1k", TaskUsageFormat.tokenCount(9_100))
        assertEquals("42k", TaskUsageFormat.tokenCount(42_400))
        assertEquals("1.2M", TaskUsageFormat.tokenCount(1_234_567))
    }

    @Test fun step_suffix_and_with_usage() {
        assertEquals(" · 9.1k tok · $0.012", TaskUsageFormat.stepSuffix(9_100, 0.0123))
        assertEquals("", TaskUsageFormat.stepSuffix(null, null))
        assertEquals("", TaskUsageFormat.stepSuffix(0, null))
        assertEquals("700ms (2.1s) · 9.1k tok", TaskUsageFormat.withUsage("700ms (2.1s)", " · 9.1k tok"))
        assertEquals("9.1k tok · $0.01", TaskUsageFormat.withUsage(null, " · 9.1k tok · $0.01"))
        assertNull(TaskUsageFormat.withUsage(null, ""))
        assertEquals("detail", TaskUsageFormat.withUsage("detail", ""))
    }

    @Test fun task_line_is_hidden_with_no_calls() {
        assertEquals("3 calls · 12.3k tok · $0.040", TaskUsageFormat.taskLine(TaskUsage(3, 12_345, 0.04)))
        assertEquals("1 call · 50 tok", TaskUsageFormat.taskLine(TaskUsage(1, 50, null)))
        assertNull(TaskUsageFormat.taskLine(TaskUsage(0, 0, null)))
    }

    @Test fun task_accessibility_label_splits_planning_and_run() {
        val total = TaskUsageTotal(
            usage = TaskUsage(3, 1500, 0.03),
            run = TaskUsage(2, 1000, 0.02),
            planning = TaskUsage(1, 500, 0.01),
        )
        assertEquals(
            "3 model calls, 1500 tokens, $0.030. Planning: 1 call, $0.010. Run, including delegated work: 2 calls, $0.020",
            TaskUsageFormat.taskAccessibilityLabel(total),
        )
    }

    @Test fun planning_suffix_sums_tokens_and_priced_calls() {
        val entries = listOf(
            PlanningEntry("a", 0, "m", false, "llm", true, tokens = 9000, costUsd = 0.02),
            PlanningEntry("b", 0, "m", false, "outline", false),
            PlanningEntry("c", 0, "m", false, "llm", true, tokens = 27000, costUsd = 0.03),
        )
        assertEquals(" · 36.0k tok · $0.050", TaskUsageFormat.planningSuffix(entries))
        assertEquals("", TaskUsageFormat.planningSuffix(listOf(entries[1])))
    }

    @Test fun run_summary_appends_run_usage() {
        assertEquals(
            "4 steps · 2m · 5 calls · 9.1k tok · $0.030",
            TaskUsageFormat.runSummary("4 steps · 2m", TaskUsage(5, 9_100, 0.03)),
        )
        assertEquals("4 steps", TaskUsageFormat.runSummary("4 steps", null))
    }

    // Turn chip (web RequestActivityCard)

    private fun llmSucceeded(trace: String, payload: String): JsonObject {
        val merged = if (payload.isBlank()) """"trace_id": "$trace"""" else """$payload, "trace_id": "$trace""""
        return json("""{"event_type": "AgentEvent", "data": {"event": {"event_type": "llm.succeeded", "payload": {$merged}}}}""")
    }

    private fun toolFinished(call: String, tool: String, payload: String): JsonObject =
        json("""{"event_type": "AgentEvent", "data": {"event": {"event_type": "tool.call.finished",
            "payload": {$payload, "call_id": "$call", "tool_name": "$tool"}}}}""")

    @Test fun turn_sums_its_own_calls_and_priced_cost() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"input_tokens": 1000, "output_tokens": 200, "cost_usd": 0.01, "duration_ms": 900"""))
        sut.ingest(llmSucceeded("t2", """"input_tokens": 500, "output_tokens": 100, "cost": 0.002, "latency_ms": 400"""))
        val rows = sut.snapshot()
        assertEquals(900.0, rows.first().durationMs)
        assertEquals("task-run calls report latency_ms", 400.0, rows.last().durationMs)

        val summary = TurnUsage.summary(rows) { null }
        assertEquals(2, summary?.calls)
        assertEquals(1800, summary?.tokens)
        assertEquals(0.012, summary?.costUsd ?: -1.0, 1e-12)
        assertEquals("2 calls · 1.8k tok · $0.012", summary?.let(TaskUsageFormat::turnLine))
    }

    @Test fun turn_without_usage_has_no_chip() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"duration_ms": 900"""))
        assertNull(TurnUsage.summary(sut.snapshot()) { null })
    }

    @Test fun token_availability_false_is_not_reported() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"input_tokens": 0, "output_tokens": 0, "usage_availability": {"tokens": false}, "cost_usd": 0.01"""))
        val summary = TurnUsage.summary(sut.snapshot()) { null }
        assertEquals("a priced call with no reported tokens still counts", 1, summary?.calls)
        assertEquals(0, summary?.tokens)
    }

    @Test fun one_call_delivered_by_two_transports_counts_once() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"llm_call_id": "call-1", "input_tokens": 100, "output_tokens": 20"""))
        sut.ingest(json("""{"event_type": "LLMResponseReceived", "data": {
            "timestamp": 5, "capability": "reason", "llm_call_id": "call-1", "input_tokens": 100, "output_tokens": 20}}"""))
        assertEquals(1, TurnUsage.summary(sut.snapshot()) { null }?.calls)
    }

    @Test fun turn_includes_tasks_it_started() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"input_tokens": 1000, "output_tokens": 0, "cost_usd": 0.01"""))
        sut.ingest(toolFinished("c1", "create_task", """"spawned_task_ids": ["task_a"]"""))
        val legacyId = "task_" + "ab".repeat(16)
        sut.ingest(toolFinished("c2", "run_task", """"content_preview": "Started $legacyId ok""""))
        // Delegation is not a spawned task: its calls are already the turn's.
        sut.ingest(toolFinished("c3", "delegate_to_agent", """"content_preview": "child $legacyId""""))
        val rows = sut.snapshot()
        assertEquals(listOf("task_a", legacyId), TurnUsage.spawnedTaskIds(rows))

        val usage = mapOf("task_a" to TaskUsageTotal(TaskUsage(4, 4000, 0.03), run = null, planning = null))
        val summary = TurnUsage.summary(rows) { usage[it] }
        assertEquals(5, summary?.calls)
        assertEquals(5000, summary?.tokens)
        assertEquals(2, summary?.spawnedTasks)
        assertEquals("5 calls · 5.0k tok · $0.040 · incl. 2 tasks", summary?.let(TaskUsageFormat::turnLine))
        assertEquals("5.0k tok · $0.040", summary?.let(TaskUsageFormat::turnShortLine))
    }

    @Test fun step_row_carries_its_call_usage() {
        val sut = ChatTurnActivityAccumulator()
        sut.ingest(llmSucceeded("t1", """"input_tokens": 9000, "output_tokens": 100, "cost_usd": 0.0123, "duration_ms": 2100, "ttft_ms": 700"""))
        val row = sut.snapshot().first()
        assertEquals("700ms (2.1s)", row.detail)
        assertEquals(
            "700ms (2.1s) · 9.1k tok · $0.012",
            TaskUsageFormat.withUsage(row.detail, TaskUsageFormat.stepSuffix(row.callUsage?.stepTokens, row.callUsage?.costUsd)),
        )
        assertNotEquals(null, row.callUsage)
    }
}
