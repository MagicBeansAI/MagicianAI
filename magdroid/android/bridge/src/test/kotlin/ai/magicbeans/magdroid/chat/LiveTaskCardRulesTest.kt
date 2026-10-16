package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.chat.LiveTaskCardAction as A
import ai.magicbeans.magdroid.chat.LiveTaskCardRules.SegmentState as S
import ai.magicbeans.magdroid.tasks.PlanStepMark
import ai.magicbeans.magdroid.tasks.PlanStepRow
import ai.magicbeans.magdroid.tasks.PlanSteps
import ai.magicbeans.magdroid.tasks.PlanningEntry
import ai.magicbeans.magdroid.tasks.PlanningPhase
import ai.magicbeans.magdroid.tasks.PlanningTrack
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** One-for-one port of iOS `LiveTaskCardSupportTests`. */
class LiveTaskCardRulesTest {
    private fun json(text: String): JsonObject = Json.parseToJsonElement(text).jsonObject

    private fun task(
        status: String,
        id: String = "task_abc123",
        storeStatus: String? = null,
        outputs: Int = 0,
    ) = TaskStatusCard(
        taskId = id, title = "Write the brief", status = status,
        executionId = "exec_1",
        outputs = List(outputs) { ContentBlockRecord(type = "file", filename = "a.md") },
        storeStatus = storeStatus,
        startedAtMs = 1_759_300_000_000L,
        updatedAtMs = 1_759_300_065_000L,
    )

    private fun state(model: TaskStatusCard): Pair<LiveTaskCardState, LiveTaskCardPhase> {
        val input = LiveTaskCardRules.input(model, messageId = "m1")
        val state = LiveTaskCardState.of(input)
        return state to LiveTaskCardRules.phase(state, LiveTaskCardRules.effectiveStatus(input))
    }

    // Phase

    @Test fun phase_follows_the_task_life() {
        assertEquals(LiveTaskCardPhase.Planning, state(task("planning")).second)
        assertEquals(LiveTaskCardPhase.PlanReady, state(task("draft")).second)
        assertEquals(LiveTaskCardPhase.NotStarted, state(task("ready")).second)
        assertEquals(LiveTaskCardPhase.NotStarted, state(task("pending")).second)
        assertEquals(LiveTaskCardPhase.Running, state(task("running")).second)
        assertEquals(LiveTaskCardPhase.Running, state(task("paused")).second)
        assertEquals(LiveTaskCardPhase.Finished, state(task("completed")).second)
        assertEquals(LiveTaskCardPhase.Finished, state(task("failed")).second)
    }

    @Test fun store_status_wins_over_a_message_stuck_at_planning_or_draft() {
        val (cardState, phase) = state(task("draft", storeStatus = "ready"))
        assertEquals(LiveTaskCardPhase.NotStarted, phase)
        assertTrue("an approved plan offers Run", cardState.startable)
        // A store status never overrides a live run.
        assertEquals(LiveTaskCardPhase.Running, state(task("running", storeStatus = "ready")).second)
    }

    // Action cluster

    private fun actions(
        phase: LiveTaskCardPhase, taskId: Boolean = true, startable: Boolean = false,
        canCancel: Boolean = false, showsPlan: Boolean = false,
    ) = LiveTaskCardRules.actions(phase, taskId, startable, canCancel, showsPlan)

    @Test fun actions_sit_inspect_first_primary_next_cancel_last() {
        // Planning: Inspect, then Cancel beside the chevron.
        assertEquals(listOf(A.Inspect, A.Cancel), actions(LiveTaskCardPhase.Planning, canCancel = true))
        // Plan ready: the primary before the secondary; Review stands in for Plan graph.
        assertEquals(listOf(A.Inspect, A.ApproveAndRun, A.Review, A.Cancel),
            actions(LiveTaskCardPhase.PlanReady, canCancel = true, showsPlan = true))
        // Not started and startable: Run right next to Inspect, then the plan.
        assertEquals(listOf(A.Inspect, A.Run, A.PlanGraph),
            actions(LiveTaskCardPhase.NotStarted, startable = true, showsPlan = true))
        assertEquals(listOf(A.Inspect), actions(LiveTaskCardPhase.NotStarted, startable = false))
        // Running: Cancel last; a waiting run with a known plan offers it first.
        assertEquals(listOf(A.Inspect, A.Cancel), actions(LiveTaskCardPhase.Running, canCancel = true))
        assertEquals(listOf(A.Inspect, A.PlanGraph, A.Cancel),
            actions(LiveTaskCardPhase.Running, canCancel = true, showsPlan = true))
        assertEquals(listOf(A.Inspect), actions(LiveTaskCardPhase.Finished))
    }

    @Test fun without_a_task_id_only_inspect_is_offered() {
        assertEquals(listOf(A.Inspect),
            actions(LiveTaskCardPhase.PlanReady, taskId = false, canCancel = true, showsPlan = true))
        assertEquals(listOf(A.Inspect),
            actions(LiveTaskCardPhase.NotStarted, taskId = false, startable = true))
    }

    @Test fun card_states_map_to_their_actions() {
        fun cluster(model: TaskStatusCard, plan: Boolean = false): List<A> {
            val (cardState, phase) = state(model)
            return LiveTaskCardRules.actions(
                phase, cardState.taskId != null, cardState.startable, cardState.canCancel,
                cardState.showsPlanPreview(if (plan) 3 else 0),
            )
        }
        assertEquals(listOf(A.Inspect, A.Run, A.PlanGraph), cluster(task("draft", storeStatus = "ready"), plan = true))
        assertEquals(listOf(A.Inspect, A.Cancel), cluster(task("running")))
        assertEquals(listOf(A.Inspect), cluster(task("completed")))
    }

    @Test fun the_card_has_a_clock_from_its_first_update() {
        val (cardState, _) = state(task("running"))
        assertTrue(cardState.showsTimer)
        assertEquals(1_759_300_000_000L, cardState.startedAtMillis)
        val (done, _) = state(task("completed"))
        assertEquals("1:05", done.elapsedText(Long.MAX_VALUE))
    }

    @Test fun finished_outputs_sit_in_the_body() {
        val (cardState, _) = state(task("completed", outputs = 3))
        assertTrue(cardState.outputsInBody)
        assertEquals(1, cardState.hiddenOutputCount(expanded = false))
    }

    // Polling

    @Test fun polls_only_while_visible_active_and_planning_or_running() {
        val r = LiveTaskCardRules
        assertTrue(r.shouldPoll(LiveTaskCardPhase.Planning, hasTaskId = true, visible = true, appActive = true))
        assertTrue(r.shouldPoll(LiveTaskCardPhase.Running, hasTaskId = true, visible = true, appActive = true))
        assertFalse(r.shouldPoll(LiveTaskCardPhase.Running, hasTaskId = true, visible = false, appActive = true))
        assertFalse(r.shouldPoll(LiveTaskCardPhase.Running, hasTaskId = true, visible = true, appActive = false))
        assertFalse(r.shouldPoll(LiveTaskCardPhase.Running, hasTaskId = false, visible = true, appActive = true))
        for (phase in listOf(LiveTaskCardPhase.PlanReady, LiveTaskCardPhase.NotStarted, LiveTaskCardPhase.Finished)) {
            assertFalse(r.shouldPoll(phase, hasTaskId = true, visible = true, appActive = true))
        }
        assertEquals(4_000L, r.POLL_INTERVAL_MILLIS)
    }

    // Line

    private var nextId = 0
    private fun entry(message: String, stage: String?, model: Boolean = false) = PlanningEntry(
        id = "e${nextId++}", atMillis = 0L, message = message, warning = false, stage = stage, isModelCall = model,
    )

    @Test fun planning_line_is_the_latest_milestone_not_a_model_call() {
        val (cardState, _) = state(task("planning"))
        val line = LiveTaskCardRules.line(
            LiveTaskCardPhase.Planning, cardState,
            planningEntries = listOf(entry("Outlining the plan", "outline"),
                entry("Outlined · gpt · 2s", "llm", model = true)),
            stepRows = emptyList(), latestActivity = null,
        )
        assertEquals("Outlining the plan", line)
    }

    @Test fun running_planned_task_line_is_the_step_summary() {
        val (cardState, _) = state(task("running"))
        val rows = PlanSteps.rows(json("""{"s": [
            {"name": "Gather", "status": "completed"},
            {"name": "Draft", "status": "in_progress"},
            {"name": "Polish", "status": "pending"}
        ]}""")["s"]!!.jsonArray.map { it.jsonObject }, runEnded = false)
        assertEquals("Step 2 of 3 · Draft", LiveTaskCardRules.line(
            LiveTaskCardPhase.Running, cardState, emptyList(), rows, latestActivity = "Fetched"))
        // No plan: the latest activity, never the bare run label.
        assertEquals("Fetched", LiveTaskCardRules.line(
            LiveTaskCardPhase.Running, cardState, emptyList(), emptyList(), latestActivity = "Fetched\nmore"))
    }

    @Test fun retry_or_failure_makes_the_planning_line_bold() {
        assertTrue(LiveTaskCardRules.lineIsEmphasised(
            LiveTaskCardPhase.Planning, PlanningTrack(PlanningPhase.Steps, retrying = true, failed = false)))
        assertFalse(LiveTaskCardRules.lineIsEmphasised(
            LiveTaskCardPhase.Planning, PlanningTrack(PlanningPhase.Steps, retrying = false, failed = false)))
        assertFalse(LiveTaskCardRules.lineIsEmphasised(
            LiveTaskCardPhase.Running, PlanningTrack(PlanningPhase.Steps, retrying = true, failed = false)))
    }

    // Planning track

    @Test fun planning_segments_fill_up_to_the_current_phase() {
        assertEquals(listOf(S.Current, S.Ahead, S.Ahead, S.Ahead),
            LiveTaskCardRules.planningSegments(PlanningTrack(null, retrying = false, failed = false)))
        assertEquals(listOf(S.Finished, S.Finished, S.Current, S.Ahead),
            LiveTaskCardRules.planningSegments(PlanningTrack(PlanningPhase.Steps, retrying = true, failed = false)))
        assertEquals(listOf(S.Finished, S.Finished, S.Finished, S.Current),
            LiveTaskCardRules.planningSegments(PlanningTrack(PlanningPhase.Check, retrying = false, failed = false)))
    }

    // Step bar

    private fun rows(count: Int, done: Int): List<PlanStepRow> = (0 until count).map {
        PlanStepRow(number = it + 1, name = "S$it", mark = if (it < done) PlanStepMark.Done else PlanStepMark.Pending,
            note = null, stepId = null)
    }

    @Test fun step_bar_is_segmented_up_to_twelve_steps_then_continuous() {
        assertNull(LiveTaskStepBar.layout(emptyList()))
        assertEquals(
            LiveTaskStepBar.Layout.Segments(listOf(PlanStepMark.Done, PlanStepMark.Pending, PlanStepMark.Pending)),
            LiveTaskStepBar.layout(rows(3, done = 1)),
        )
        val twelve = LiveTaskStepBar.layout(rows(12, done = 0))
        assertTrue("12 steps stay segmented", twelve is LiveTaskStepBar.Layout.Segments)
        assertEquals(12, (twelve as LiveTaskStepBar.Layout.Segments).marks.size)
        assertEquals(LiveTaskStepBar.Layout.Continuous(1.0), LiveTaskStepBar.layout(rows(13, done = 13)))
        assertEquals(LiveTaskStepBar.Layout.Continuous(0.25), LiveTaskStepBar.layout(rows(20, done = 5)))
    }

    // Plan payload

    @Test fun plan_payload_reads_id_status_and_step_names() {
        val payload = json("""{"plan": {
            "plan_id": "plan_9", "status": "draft",
            "plan_graph": {"steps": [
                {"id": "s1", "task": "Gather sources"},
                {"id": "s2", "metadata": {"description": "Draft the brief"}},
                {"id": "s3"}
            ]}
        }}""")
        assertEquals(
            LiveTaskPlan(planId = "plan_9", status = "draft", steps = listOf("Gather sources", "Draft the brief", "Step s3")),
            LiveTaskPlan.parse(payload),
        )
        assertNull(LiveTaskPlan.parse(json("""{"plan": {}}""")))
    }

    // Copy path

    @Test fun copy_path_prefers_the_host_path() {
        assertEquals("/Users/me/out/a.md", LiveTaskCardRules.copyPath("/Users/me/out/a.md", "outputs/a.md"))
        assertEquals("outputs/a.md", LiveTaskCardRules.copyPath("out/a.md", "outputs/a.md"))
        assertNull(LiveTaskCardRules.copyPath(null, "  "))
    }

    // Dot

    @Test fun dot_tone_follows_the_status() {
        assertEquals(LiveTaskDotTone.NotStarted, LiveTaskCardRules.dotTone(LiveTaskStatus.Queued))
        assertEquals(LiveTaskDotTone.Running, LiveTaskCardRules.dotTone(LiveTaskStatus.Running))
        assertEquals(LiveTaskDotTone.Partial, LiveTaskCardRules.dotTone(LiveTaskStatus.Partial))
        assertEquals(LiveTaskDotTone.Failed, LiveTaskCardRules.dotTone(LiveTaskStatus.Failed))
    }

    // Task 35eb15: the chat's copy of a deliverable (`session_output`) was
    // asked of the task endpoint and read "Task output not found".
    @Test fun session_copy_of_a_task_output_is_read_from_the_session() {
        assertEquals(LiveTaskOutputHome.Session("s1"),
            LiveTaskCardRules.outputHome("session_output", null, "task_1", "s1"))
        assertEquals(LiveTaskOutputHome.Session("s1"),
            LiveTaskCardRules.outputHome("chat_output", null, "task_1", "s1"))
    }

    @Test fun task_output_is_read_from_the_task() {
        assertEquals(LiveTaskOutputHome.Task("task_2"),
            LiveTaskCardRules.outputHome("task_output", "task_2", "task_1", "s1"))
        assertEquals(LiveTaskOutputHome.Task("task_1"),
            LiveTaskCardRules.outputHome("task_output", null, "task_1", "s1"))
    }

    @Test fun sourceless_block_prefers_the_session_then_the_task() {
        assertEquals(LiveTaskOutputHome.Session("s1"), LiveTaskCardRules.outputHome(null, null, "task_1", "s1"))
        assertEquals(LiveTaskOutputHome.Task("task_1"), LiveTaskCardRules.outputHome(null, null, "task_1", null))
        assertNull(LiveTaskCardRules.outputHome("session_output", null, "task_1", null))
    }

    // The task's stored status (task_ee4d571626814eb889945c968172385d)

    @Test fun store_record_reads_the_current_nested_shape() {
        val record = json("""{"task": {
            "manifest": {"title": "Brief", "chat_session_id": "sess_1"},
            "state": {"status": "ready", "active_root_execution_id": "exec_9"},
            "refs": {}
        }}""")
        assertEquals(TaskStoreRecord("ready", "exec_9", "sess_1"), TaskStoreRecord.parse(record))
    }

    @Test fun store_record_still_reads_the_flat_shape() {
        val record = json("""{"task": {"status": "running", "active_root_execution_id": "exec_1", "chat_session_id": "sess_2"}}""")
        assertEquals(TaskStoreRecord("running", "exec_1", "sess_2"), TaskStoreRecord.parse(record))
        assertNull(TaskStoreRecord.parse(json("""{"task": {"state": {}}}""")))
        assertNull(TaskStoreRecord.parse(json("{}")))
    }

    /** The reported card: the last message is `draft` but the plan was approved, so the task is `ready`. */
    @Test fun a_draft_message_with_a_ready_store_is_queued_with_run_and_no_timer_or_review() {
        val message = task("draft").copy(summary = "Plan ready for review")
        val reconciled = LiveTaskCardRules.reconciled(message, storeStatus = "ready")
        assertEquals("the card's own store-wins rule shows it", "draft", reconciled.status)
        assertEquals("ready", reconciled.storeStatus)

        val (state, phase) = state(reconciled)
        assertEquals(LiveTaskStatus.Queued, state.status)
        assertEquals("not PlanReady: no Review / Approve & Run", LiveTaskCardPhase.NotStarted, phase)
        assertTrue("Run chip", state.startable)
        assertFalse(state.showsTimer)
        assertFalse(state.ticks)
        assertFalse(state.canCancel)
        assertNull("the draft summary described a state the task left", state.reason)
        assertEquals("Not started", state.tagText)
    }

    @Test fun a_planning_message_with_a_pending_store_is_queued() {
        val (state, phase) = state(LiveTaskCardRules.reconciled(task("planning"), storeStatus = "pending"))
        assertEquals(LiveTaskStatus.Queued, state.status)
        assertEquals(LiveTaskCardPhase.NotStarted, phase)
        assertFalse(state.startable)
    }

    @Test fun a_terminal_store_status_replaces_a_stale_running_message() {
        val message = task("running").copy(summary = "Working on step 3")
        val reconciled = LiveTaskCardRules.reconciled(message, storeStatus = "completed")
        assertEquals("completed", reconciled.status)
        assertNull(reconciled.summary)
        val (state, phase) = state(reconciled)
        assertTrue(state.terminal)
        assertEquals(LiveTaskCardPhase.Finished, phase)
        assertFalse(state.canCancel)
    }

    @Test fun a_matching_store_status_keeps_the_message() {
        val message = task("running").copy(summary = "Working on step 3")
        val reconciled = LiveTaskCardRules.reconciled(message, storeStatus = "running")
        assertEquals("running", reconciled.status)
        assertEquals("Working on step 3", reconciled.summary)
    }
}
