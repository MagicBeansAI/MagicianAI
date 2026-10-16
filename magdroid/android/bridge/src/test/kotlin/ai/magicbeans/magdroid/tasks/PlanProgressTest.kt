package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Planning phases and plan-step marks from the `/execution-panel` payload.
 * One-for-one port of iOS `PlanProgressModelTests` (itself mirroring web
 * `planProgressFrom` in `taskPanelModel.test.ts`, design §4's stage → phase
 * table and `v3_adapter.rs::plan_step_statuses_from`).
 */
class PlanProgressTest {
    private fun obj(vararg pairs: Pair<String, Any?>): JsonObject =
        JsonObject(pairs.filter { it.second != null }.associate { it.first to json(it.second) })

    private fun json(value: Any?): JsonElement = when (value) {
        is JsonElement -> value
        is String -> JsonPrimitive(value)
        is Number -> JsonPrimitive(value)
        is Boolean -> JsonPrimitive(value)
        is List<*> -> JsonArray(value.map { json(it) })
        else -> error("unsupported $value")
    }

    private fun entry(
        id: String,
        at: Double,
        message: String,
        severity: String = "info",
        title: String = "Progress",
        execution: String? = "planexec_1",
        stage: String? = null,
    ): JsonObject = obj(
        "id" to id, "timestamp" to at, "severity" to severity, "title" to title, "message" to message,
        "execution_id" to execution, "planning_stage" to stage,
    )

    /** The web fixture: an older server's shared timeline. */
    private fun olderTimeline(): List<JsonObject> = listOf(
        entry("p2", 30_000.0, "Outlining the plan"),
        entry("p1", 20_000.0, "Understood the request"),
        entry("p3", 40_000.0, "Planning hit a problem: boom", severity = "warning"),
        entry("status", 10_000.0, "Execution entered planning", title = "Status changed to Planning"),
        entry("run-progress", 50_000.0, "Execution is running.", execution = "exec-run"),
    )

    private fun olderPanel(): JsonObject = obj("debug" to obj("timeline" to olderTimeline()))

    // Entries (web planProgressFrom)

    @Test fun lists_the_planning_milestones_oldest_first_from_the_shared_timeline_on_an_older_server() {
        val entries = PlanProgress.entries(olderPanel())
        assertEquals(listOf("p1", "p2", "p3"), entries.map { it.id })
        assertEquals(listOf(20_000L, 30_000L, 40_000L), entries.map { it.atMillis })
        assertEquals(
            listOf("Understood the request", "Outlining the plan", "Planning hit a problem: boom"),
            entries.map { it.message },
        )
        assertEquals(listOf(false, false, true), entries.map { it.warning })
    }

    @Test fun reads_the_dedicated_planning_timeline_when_the_server_sends_one() {
        val panel = obj("debug" to obj(
            "timeline" to olderTimeline(),
            "planning_timeline" to listOf(
                entry("llm1", 15_000.0, "Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens"),
            ),
        ))
        val entries = PlanProgress.entries(panel)
        assertEquals(listOf("llm1"), entries.map { it.id })
        assertEquals("Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens", entries[0].message)
        assertFalse(entries[0].warning)
        assertTrue(entries[0].isModelCall)
        assertEquals(emptyList<PlanningEntry>(), PlanProgress.entries(JsonObject(emptyMap())))
    }

    @Test fun an_empty_dedicated_timeline_is_the_answer_not_a_reason_to_fall_back() {
        val panel = obj("debug" to obj("timeline" to olderTimeline(), "planning_timeline" to emptyList<Any>()))
        assertEquals(emptyList<PlanningEntry>(), PlanProgress.entries(panel))
    }

    @Test fun counts_an_error_as_a_warning() {
        val panel = obj("debug" to obj("planning_timeline" to listOf(
            entry("e", 1_000.0, "Planning hit a problem: x", severity = "error"),
        )))
        assertEquals(listOf(true), PlanProgress.entries(panel).map { it.warning })
    }

    @Test fun reads_a_backfilled_usage_rows_call_count_tokens_and_cost() {
        val row = JsonObject(entry("u", 1_000.0, "Planning usage", stage = "llm") + mapOf(
            "llm_calls" to JsonPrimitive(4), "tokens" to JsonPrimitive(1200), "cost_usd" to JsonPrimitive(0.02),
        ))
        val entries = PlanProgress.entries(obj("debug" to obj("planning_timeline" to listOf(row))))
        assertEquals(4, entries[0].llmCalls)
        assertEquals(1200, entries[0].tokens)
        assertEquals(0.02, entries[0].costUsd!!, 1e-12)
        assertTrue(entries[0].isModelCall)
    }

    // Stage

    @Test fun reads_the_stage_field_when_present() {
        assertEquals("outline", PlanProgress.stage(entry("x", 1.0, "anything", stage = "outline")))
    }

    @Test fun falls_back_to_the_stage_segment_of_the_planning_id() {
        assertEquals("expansion", PlanProgress.stage(entry("planning:planexec_1:expansion:1760000000000", 1.0, "m")))
        assertEquals("plan_retry", PlanProgress.stage(entry("planning:planexec_1:plan_retry:1760000000000", 1.0, "m")))
        // Not a planning id: no stage from it.
        assertNull(PlanProgress.stage(entry("b9f1c3de-uuid", 1.0, "Execution entered planning")))
        assertNull(PlanProgress.stage(entry("status:a:b:c", 1.0, "m")))
    }

    @Test fun falls_back_to_the_backends_fixed_milestone_wording_last() {
        val cases = listOf(
            "Understood the request" to "query_analysis",
            "Picking out the details the task needs" to "slot_extraction",
            "Restated the task (2 objectives)" to "clarified_task",
            "Outlining the plan" to "outline",
            "Outlined 3 goals" to "outline_done",
            "Working out the steps for 3 goals" to "expansion",
            "Checking the drafted plan" to "validation",
            "Plan drafted and checked" to "plan_ready",
            "The draft did not pass its checks; revising (attempt 2)" to "plan_retry",
            "Planning hit a problem: boom" to "failed",
            "Outlined the plan · gpt · 12.0s" to "llm",
            "Analyzed the request · gpt-6-luna · 8.5s · 9.1K tokens" to "llm",
        )
        for ((message, stage) in cases) {
            assertEquals(message, stage, PlanProgress.stage(entry("uuid", 1.0, message)))
        }
        assertNull(PlanProgress.stage(entry("uuid", 1.0, "Execution entered planning")))
    }

    @Test fun the_stage_field_outranks_the_id_and_the_id_outranks_the_wording() {
        val fielded = entry("planning:planexec_1:outline:1", 1.0, "Plan drafted and checked", stage = "validation")
        assertEquals("validation", PlanProgress.stage(fielded))
        val ided = entry("planning:planexec_1:outline:1", 1.0, "Plan drafted and checked")
        assertEquals("outline", PlanProgress.stage(ided))
    }

    // Stage → phase (design §4)

    @Test fun maps_every_stage_to_its_phase() {
        val table = mapOf(
            "query_analysis" to PlanningPhase.Understand, "slot_extraction" to PlanningPhase.Understand,
            "clarified_task" to PlanningPhase.Understand,
            "outline" to PlanningPhase.Outline, "outline_done" to PlanningPhase.Outline,
            "expansion" to PlanningPhase.Steps, "plan_retry" to PlanningPhase.Steps,
            "validation" to PlanningPhase.Check, "plan_ready" to PlanningPhase.Check,
        )
        for ((stage, phase) in table) assertEquals(stage, phase, PlanProgress.phase(stage))
        assertNull(PlanProgress.phase("llm"))
        assertNull(PlanProgress.phase("failed"))
        assertNull(PlanProgress.phase("something_new"))
    }

    @Test fun names_the_phases_in_track_order() {
        assertEquals(listOf("Understand", "Outline", "Steps", "Check"), PlanningPhase.entries.map { it.title })
    }

    // Current phase

    private fun entries(stages: List<String>): List<PlanningEntry> = stages.mapIndexed { index, stage ->
        PlanningEntry(
            id = "e$index", atMillis = index * 1_000L, message = stage,
            warning = false, stage = stage, isModelCall = stage == "llm",
        )
    }

    @Test fun the_latest_phase_reached_is_current() {
        val track = PlanProgress.currentPhase(entries(listOf("query_analysis", "outline", "outline_done", "expansion")))
        assertEquals(PlanningTrack(PlanningPhase.Steps, retrying = false, failed = false), track)
        assertEquals(PlanningTrack(null, retrying = false, failed = false), PlanProgress.currentPhase(emptyList()))
    }

    @Test fun a_retry_steps_back_to_steps() {
        val track = PlanProgress.currentPhase(entries(listOf("outline", "expansion", "validation", "plan_retry")))
        assertEquals(PlanningTrack(PlanningPhase.Steps, retrying = true, failed = false), track)
        // The revision moving on clears the retry.
        val moved = PlanProgress.currentPhase(entries(listOf("validation", "plan_retry", "validation")))
        assertEquals(PlanningTrack(PlanningPhase.Check, retrying = false, failed = false), moved)
    }

    @Test fun model_calls_never_move_the_track() {
        val track = PlanProgress.currentPhase(entries(listOf("query_analysis", "outline", "llm", "llm")))
        assertEquals(PlanningTrack(PlanningPhase.Outline, retrying = false, failed = false), track)
        assertNull(PlanProgress.currentPhase(entries(listOf("llm"))).phase)
    }

    @Test fun failed_keeps_the_current_phase_and_marks_it() {
        val track = PlanProgress.currentPhase(entries(listOf("query_analysis", "outline", "failed")))
        assertEquals(PlanningTrack(PlanningPhase.Outline, retrying = false, failed = true), track)
        // A durable retry that resumes planning clears the failure.
        val resumed = PlanProgress.currentPhase(entries(listOf("outline", "failed", "llm", "expansion")))
        assertEquals(PlanningTrack(PlanningPhase.Steps, retrying = false, failed = false), resumed)
    }

    @Test fun the_older_server_timeline_drives_the_track_through_the_wording_fallback() {
        val track = PlanProgress.currentPhase(PlanProgress.entries(olderPanel()))
        assertEquals(PlanningTrack(PlanningPhase.Outline, retrying = false, failed = true), track)
    }

    @Test fun summarises_planning_activity_as_count_and_duration() {
        val rows = PlanProgress.entries(olderPanel())
        assertEquals("3 events · 20s", PlanProgress.activitySummary(rows))
        assertEquals("1 event", PlanProgress.activitySummary(rows.take(1)))
        assertEquals("", PlanProgress.activitySummary(emptyList()))
    }

    // Plan steps

    private fun panel(statuses: List<JsonObject>): JsonObject =
        obj("debug" to obj("selected_execution" to obj("step_statuses" to statuses)))

    private fun step(number: Int, name: String, status: String, note: String? = null, progress: String = ""): JsonObject =
        obj("number" to number, "name" to name, "status" to status, "progress" to progress,
            "step_id" to "s$number", "note" to note)

    @Test fun maps_each_step_status_to_its_mark() {
        val rows = PlanSteps.rows(panel(listOf(
            step(1, "Look up the weather", "completed"),
            step(2, "Draft the brief", "failed"),
            step(3, "Check the facts", "in_progress"),
            step(4, "Send it", "pending"),
            step(5, "Archive", "skipped"),
            step(6, "Celebrate", "teleported"),
        )), runEnded = false)
        assertEquals(
            listOf(PlanStepMark.Done, PlanStepMark.Failed, PlanStepMark.InProgress,
                PlanStepMark.Pending, PlanStepMark.Skipped, PlanStepMark.Pending),
            rows.map { it.mark },
        )
        assertEquals(listOf(1, 2, 3, 4, 5, 6), rows.map { it.number })
        assertEquals("Look up the weather", rows.first().name)
        assertEquals("s1", rows.first().stepId)
    }

    @Test fun reads_the_agents_reason_from_note_not_progress_on_any_mark() {
        val rows = PlanSteps.rows(panel(listOf(
            step(1, "Search flights", "skipped", note = "  The user already booked one.  "),
            step(2, "Compare fares", "completed", note = "Used the cached fares instead."),
            step(3, "Pay", "failed", note = "Card declined."),
            step(4, "Email", "pending", progress = "50%"),
        )), runEnded = false)
        assertEquals(
            listOf("The user already booked one.", "Used the cached fares instead.", "Card declined.", null),
            rows.map { it.note },
        )
    }

    @Test fun a_pending_step_reads_not_reached_once_the_run_ended() {
        val statuses = listOf(step(1, "A", "completed"), step(2, "B", "pending"), step(3, "C", "teleported"))
        assertEquals(
            listOf(PlanStepMark.Done, PlanStepMark.NotReached, PlanStepMark.NotReached),
            PlanSteps.rows(panel(statuses), runEnded = true).map { it.mark },
        )
        assertEquals(
            listOf(PlanStepMark.Done, PlanStepMark.Pending, PlanStepMark.Pending),
            PlanSteps.rows(panel(statuses), runEnded = false).map { it.mark },
        )
    }

    @Test fun keeps_the_plans_order_when_steps_finish_out_of_order() {
        val rows = PlanSteps.rows(panel(listOf(
            step(1, "A", "pending"), step(2, "B", "completed"), step(3, "C", "in_progress"),
        )), runEnded = false)
        assertEquals(listOf("A", "B", "C"), rows.map { it.name })
    }

    @Test fun names_a_step_with_no_name_by_its_position_and_tolerates_missing_fields() {
        val rows = PlanSteps.rows(panel(listOf(obj("status" to "completed"), obj("name" to "  ", "number" to 2))),
            runEnded = false)
        assertEquals(listOf("Step 1", "Step 2"), rows.map { it.name })
        assertEquals(listOf(PlanStepMark.Done, PlanStepMark.Pending), rows.map { it.mark })
        assertEquals(emptyList<PlanStepRow>(), PlanSteps.rows(JsonObject(emptyMap()), runEnded = true))
    }

    @Test fun summarises_the_run_against_the_plan() {
        val live = PlanSteps.rows(panel(listOf(
            step(1, "A", "completed"), step(2, "B", "completed"), step(3, "Check the facts", "in_progress"),
            step(4, "D", "pending"), step(5, "E", "pending"), step(6, "F", "pending"),
        )), runEnded = false)
        assertEquals("Step 3 of 6 · Check the facts", PlanSteps.summary(live))

        val done = (1..6).map { step(it, "S$it", "completed") }
        assertEquals("6 of 6 done", PlanSteps.summary(PlanSteps.rows(panel(done), runEnded = true)))

        val failed = PlanSteps.rows(panel(listOf(
            step(1, "A", "completed"), step(2, "Pay", "failed"), step(3, "C", "pending"),
        )), runEnded = true)
        assertEquals("Step 2 of 3 failed · Pay", PlanSteps.summary(failed))

        val skipped = PlanSteps.rows(panel(listOf(
            step(1, "A", "completed"), step(2, "B", "skipped"), step(3, "C", "completed"),
        )), runEnded = true)
        assertEquals("2 of 3 done · 1 skipped", PlanSteps.summary(skipped))

        assertEquals("", PlanSteps.summary(emptyList()))
    }
}
