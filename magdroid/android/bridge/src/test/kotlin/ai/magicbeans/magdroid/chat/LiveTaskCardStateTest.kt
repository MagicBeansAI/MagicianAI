package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * One-for-one port of iOS `LiveTaskCardModelTests` (web `liveTask.test.ts`,
 * every case, and the state rules `LiveTaskCard.component.test.ts` asserts
 * through the DOM).
 */
class LiveTaskCardStateTest {
    private val t0 = 1_760_000_000_000.0

    /** A `task_status_update` message, as the web fixture builds it. */
    private fun message(
        id: String,
        status: String,
        executionId: String? = null,
        createdAt: Double? = null,
        summary: String? = null,
        synthesisPending: Boolean = false,
        completionKind: String? = null,
        outputs: Int = 0,
    ) = LiveTaskCardInput(
        messageId = id,
        taskId = "task-1",
        displayLabel = "Review release",
        status = status,
        summary = summary,
        synthesisPending = synthesisPending,
        completionKind = completionKind,
        executionId = executionId,
        createdAt = createdAt ?: t0,
        outputCount = outputs,
    )

    /** One run group: its latest update's status, ids and outputs, and every update's time. */
    private fun group(executionId: String, updates: List<LiveTaskCardInput>): LiveTaskRunInput {
        val latest = updates.last()
        return LiveTaskRunInput(
            id = "task:task-1::execution:$executionId",
            status = latest.status,
            executionId = latest.executionId,
            sessionId = "session-1",
            timestamps = updates.mapNotNull { it.createdAt },
            outputCount = latest.outputCount,
        )
    }

    private fun view(input: LiveTaskCardInput, runs: List<LiveTaskRunInput> = emptyList(), store: String? = null) =
        LiveTaskCardState.of(input.copy(runs = runs, storeStatus = store))

    private fun millis(ms: Double): Long = ms.toLong()

    // liveTask.test.ts

    @Test fun reads_a_draft_plan_as_waiting_for_review_not_as_running() {
        val v = view(message("m1", "draft"))
        assertEquals(LiveTaskStatus.Waiting, v.status)
        assertEquals("Plan ready for review", v.verb)
        assertEquals("Plan ready for review", v.headline)
    }

    @Test fun lets_the_task_store_win_when_the_card_message_lags_it_at_draft_or_planning() {
        val lagging = view(message("m1", "draft", summary = "Plan ready for review."), store = "ready")
        assertEquals(LiveTaskStatus.Queued, lagging.status)
        assertEquals("Plan approved, not started", lagging.verb)
        assertNull(lagging.reason)
        assertTrue(lagging.startable)

        assertEquals(LiveTaskStatus.Queued, view(message("m1", "planning"), store = "pending").status)
        // A message that has moved on (a run started) is not overridden.
        assertEquals(LiveTaskStatus.Running, view(message("m2", "running"), store = "ready").status)
    }

    @Test fun reads_an_approved_or_unplanned_task_as_not_started_never_as_running() {
        val approved = view(message("m1", "ready"))
        assertEquals(LiveTaskStatus.Queued, approved.status)
        assertEquals("Plan approved, not started", approved.verb)
        assertFalse(approved.terminal)
        assertTrue(approved.startable)

        val unplanned = view(message("m2", "pending"))
        assertEquals(LiveTaskStatus.Queued, unplanned.status)
        assertEquals("Not started", unplanned.verb)
        assertFalse(unplanned.startable)

        val rejected = view(message("m3", "rejected"))
        assertEquals(LiveTaskStatus.Queued, rejected.status)
        assertEquals("Plan rejected", rejected.verb)
    }

    @Test fun takes_id_title_and_the_summary_reason_from_the_content() {
        val v = view(message("m1", "running", summary = "Checking CI. Then tags."))
        assertEquals("task-1", v.id)
        assertEquals("Review release", v.title)
        assertEquals(LiveTaskStatus.Running, v.status)
        assertEquals("Checking CI. Then tags.", v.reason)
        assertFalse(v.terminal)
    }

    @Test fun never_titles_the_card_with_a_raw_id() {
        var m = message("m1", "running").copy(displayLabel = null)
        assertEquals("Task · 1", view(m).title)
        // Live: the card read `task_2679cefd6f664bf7bd4a3b57fae4ce75`.
        m = m.copy(taskId = "task_2679cefd6f664bf7bd4a3b57fae4ce75")
        assertEquals("Task · 2679ce", view(m).title)
        // The reason is not a title, even when it is the only prose there is.
        m = m.copy(summary = "Searching flights.")
        assertEquals("Task · 2679ce", view(m).title)
        m = m.copy(taskId = null)
        assertEquals("Task", view(m).title)
        assertEquals("m1", view(m).id)
    }

    @Test fun titles_from_the_display_label_through_the_drawers_title_rule() {
        var m = message("m1", "running")
        // A routed prompt passed as the label becomes its goal.
        m = m.copy(displayLabel = "web-researcher / Do not delegate further. Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL…")
        assertEquals("Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL", view(m).title)
        // An id-shaped label is not a label.
        m = m.copy(displayLabel = "task-1")
        assertEquals("Task · 1", view(m).title)
    }

    /** Web: `'progress' in view` is false. The type declares no progress fraction: a run list, never a percentage. */
    @Test fun carries_no_progress_fraction() {
        val v = view(message("m1", "running"))
        assertEquals(emptyList<LiveTaskStep>(), v.steps)
        assertFalse(LiveTaskCardState::class.java.declaredFields.any { it.name == "progress" })
    }

    @Test fun lists_one_step_per_run_with_its_status() {
        val done = message("r1", "completed", executionId = "e1")
        val running = message("r2", "running", executionId = "e2", summary = "Rerunning the flaky suite.")
        val v = view(running, listOf(group("e1", listOf(done)), group("e2", listOf(running))))
        assertEquals(listOf(LiveTaskStepStatus.Done, LiveTaskStepStatus.Running), v.steps.map { it.status })
        assertEquals(listOf("Run 1", "Run 2"), v.steps.map { it.label })
        // The running run's label is a real step, distinct from the reason.
        assertEquals("Run 2", v.currentStep)
        assertEquals("Rerunning the flaky suite.", v.reason)
    }

    @Test fun drops_the_summary_derived_step_when_the_reason_already_says_it() {
        val running = message("r2", "running", executionId = "e2", summary = "Rerunning the flaky suite. Almost there.")
        val v = view(running, listOf(group("e2", listOf(running))))
        assertEquals("Rerunning the flaky suite. Almost there.", v.reason)
        assertNull(v.currentStep)
    }

    @Test fun shows_the_reason_for_every_state_that_has_one() {
        val failed = view(message("m", "failed", summary = "Auth expired for `gh` - re-login needed."))
        assertEquals(LiveTaskStatus.Failed, failed.status)
        // Kept verbatim (markdown, hyphens intact): the card renders it as markdown.
        assertEquals("Auth expired for `gh` - re-login needed.", failed.reason)

        val waiting = view(message("m", "paused", summary = "Needs your approval to send the e-mail."))
        assertEquals(LiveTaskStatus.Waiting, waiting.status)
        assertEquals("Needs your approval to send the e-mail.", waiting.reason)

        val done = view(message("m", "completed", summary = "Release **v1.2** tagged."))
        assertEquals("Release **v1.2** tagged.", done.reason)

        assertNull(view(message("m", "completed", summary = "   ")).reason)
        assertNull(view(message("m", "completed")).reason)
    }

    @Test fun maps_a_partial_completion_to_partial_terminal_with_its_detail_as_the_reason() {
        val v = view(message("m", "completed", summary = "Tagged; changelog still open.", completionKind = "partial"))
        assertEquals(LiveTaskStatus.Partial, v.status)
        assertTrue(v.terminal)
        assertEquals("Partially completed", v.verb)
        assertEquals("Tagged; changelog still open.", v.reason)

        val disposition = view(message("m", "partial_success"))
        assertEquals(LiveTaskStatus.Partial, disposition.status)
        assertTrue(disposition.terminal)

        assertEquals(LiveTaskStatus.Completed, view(message("m", "completed", completionKind = "full")).status)
    }

    @Test fun does_not_read_a_partial_completion_kind_on_a_still_running_task() {
        val v = view(message("m", "running", completionKind = "partial"))
        assertEquals(LiveTaskStatus.Running, v.status)
        assertFalse(v.terminal)
    }

    @Test fun falls_back_to_the_running_step_label_then_the_status_verb() {
        val running = message("r1", "running", executionId = "e1")
        assertEquals("Run", view(running, listOf(group("e1", listOf(running)))).currentStep)
        assertEquals("Planning", view(message("m1", "planning")).currentStep)
    }

    @Test fun detects_terminal_statuses() {
        val done = message("r1", "completed", executionId = "e1", createdAt = t0 + 5_000)
        val started = message("r0", "running", executionId = "e1", createdAt = t0 + 2_000)
        val v = view(done, listOf(group("e1", listOf(started, done))))
        assertTrue(v.terminal)
        assertEquals(LiveTaskStatus.Completed, v.status)
        assertNull(v.currentStep)
        assertEquals(millis(t0 + 2_000), v.startedAtMillis)
        assertEquals(millis(t0 + 5_000), v.finishedAtMillis)

        assertTrue(view(message("m", "failed")).terminal)
        assertEquals(LiveTaskStatus.Cancelled, view(message("m", "cancelled")).status)

        // An archived task never runs again: settled, muted, never "Running".
        val archived = view(message("m", "archived"))
        assertTrue(archived.terminal)
        assertEquals(LiveTaskStatus.Cancelled, archived.status)
        assertNull(archived.currentStep)
    }

    @Test fun keeps_a_finished_but_synthesizing_task_running() {
        val v = view(message("m", "running", synthesisPending = true))
        assertFalse(v.terminal)
        assertEquals("Preparing final result", v.currentStep)
    }

    @Test fun maps_a_paused_task_to_waiting_on_the_user() {
        val paused = message("r1", "paused", executionId = "e1")
        val v = view(paused, listOf(group("e1", listOf(paused))))
        assertEquals(LiveTaskStatus.Waiting, v.status)
        assertFalse(v.terminal)
        assertEquals(LiveTaskStepStatus.Waiting, v.steps.first().status)
        assertTrue(v.needsYou)
    }

    @Test fun normalises_second_resolution_timestamps_to_milliseconds() {
        val v = view(message("m", "running", createdAt = 1_700_000_000.0))
        assertEquals(1_700_000_000_000L, v.startedAtMillis)
        assertNull(v.finishedAtMillis)
    }

    @Test fun collects_per_run_outputs_and_the_latest_run_ids() {
        val first = message("r1", "completed", executionId = "e1")
        val second = message("r2", "completed", executionId = "e2", outputs = 1)
        val v = view(second, listOf(group("e1", listOf(first)), group("e2", listOf(second))))
        assertEquals(1, v.outputs.size)
        assertEquals("task:task-1::execution:e2", v.outputs[0].key)
        assertEquals("session-1", v.outputs[0].sessionId)
        assertNotNull(v.outputs[0].label)
        assertEquals(1, v.outputs[0].count)
        assertEquals("task-1", v.taskId)
        assertEquals("e2", v.executionId)
    }

    @Test fun has_no_outputs_and_no_execution_id_for_a_bare_status_message() {
        val v = view(message("m1", "running"))
        assertEquals(emptyList<LiveTaskOutput>(), v.outputs)
        assertNull(v.executionId)
    }

    // LiveTaskCard.component.test.ts (state, not DOM)

    @Test fun ticks_the_elapsed_time_while_running() {
        val v = view(message("m", "running"))
        assertTrue(v.showsTimer)
        assertTrue(v.ticks)
        val now = millis(t0 + 5_000)
        assertEquals("0:05", v.elapsedText(now))
        assertEquals("0:07", v.elapsedText(now + 2_000))
    }

    @Test fun does_not_tick_once_the_task_is_terminal() {
        val v = view(message("m", "completed"))
        assertFalse(v.ticks)
        // A finished task still shows how long it took.
        assertEquals("0:00", v.elapsedText(Long.MAX_VALUE))
    }

    @Test fun formats_elapsed_as_minutes_seconds_then_hours() {
        assertEquals("0:00", LiveTaskCardState.formatElapsed(0.0))
        assertEquals("1:05", LiveTaskCardState.formatElapsed(65.0))
        assertEquals("1:02:05", LiveTaskCardState.formatElapsed(3_725.0))
        assertEquals("0:00", LiveTaskCardState.formatElapsed(-4.0))
    }

    @Test fun shows_an_approved_unstarted_task_as_not_started_no_timer_no_cancel() {
        val v = view(message("r1", "ready", executionId = "e1"))
        assertEquals("Plan approved, not started", v.verb)
        assertEquals("Not started", v.tagText)
        assertFalse(v.showsTimer)
        assertFalse(v.ticks)
        assertNull(v.elapsedText(Long.MAX_VALUE))
        assertFalse(v.canCancel)
        assertFalse(v.showsLiveActivity)
    }

    @Test fun tags_only_an_unstarted_task() {
        assertNull(view(message("m", "running")).tagText)
        assertNull(view(message("m", "draft")).tagText)
        assertEquals("Not started", view(message("m", "pending")).tagText)
    }

    @Test fun offers_cancel_only_while_live() {
        assertTrue(view(message("m", "running")).canCancel)
        assertTrue(view(message("m", "planning")).canCancel)
        assertFalse(view(message("m", "completed")).canCancel)
        assertFalse(view(message("m", "failed")).canCancel)
        assertFalse(view(message("m", "pending")).canCancel)
    }

    @Test fun offers_the_live_activity_only_while_running_with_a_real_task_id() {
        assertTrue(view(message("m", "running")).showsLiveActivity)
        assertFalse(view(message("m", "completed")).showsLiveActivity)
        assertFalse(view(message("m", "running").copy(taskId = null)).showsLiveActivity)
    }

    @Test fun offers_the_run_and_plan_preview_and_asks_for_the_plan_while_not_started() {
        val ready = view(message("r1", "ready"))
        assertTrue(ready.startable)
        assertTrue(ready.wantsPlan)
        assertTrue(ready.showsPlanPreview(2))
        assertFalse(ready.showsPlanPreview(0))
        assertTrue(ready.isExpandable(activityHasContent = false, planStepCount = 2, reasonOverflows = false))

        // A drafted plan waiting for review previews too; a running task does not.
        assertTrue(view(message("m", "draft")).wantsPlan)
        assertFalse(view(message("m", "running")).wantsPlan)
        assertFalse(view(message("m", "running")).showsPlanPreview(3))
    }

    @Test fun shows_no_runs_list_and_is_not_expandable_for_a_finished_single_run_task() {
        val run = message("r1", "completed", executionId = "exec-1")
        val v = view(run, listOf(group("exec-1", listOf(run))))
        assertFalse(v.showsRuns(liveActivityAvailable = false))
        assertFalse(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun shows_the_runs_list_for_a_finished_task_whose_runs_ended_differently() {
        val failed = message("r1", "failed", executionId = "e1")
        val done = message("r2", "completed", executionId = "e2")
        val v = view(done, listOf(group("e1", listOf(failed)), group("e2", listOf(done))))
        assertTrue(v.showsRuns(liveActivityAvailable = false))
        assertEquals(listOf(LiveTaskStepStatus.Failed, LiveTaskStepStatus.Done), v.steps.map { it.status })
        assertTrue(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun shows_no_runs_list_for_a_finished_task_whose_runs_all_ended_the_same_way() {
        val a = message("r1", "completed", executionId = "e1")
        val b = message("r2", "completed", executionId = "e2")
        val v = view(b, listOf(group("e1", listOf(a)), group("e2", listOf(b))))
        assertFalse(v.showsRuns(liveActivityAvailable = false))
        assertFalse(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun shows_the_runs_list_while_running_when_there_is_no_activity_slot() {
        val run = message("r1", "running", executionId = "e1")
        val v = view(run, listOf(group("e1", listOf(run))))
        assertTrue(v.showsRuns(liveActivityAvailable = false))
        // With the activity slot the steps stream there instead.
        assertFalse(v.showsRuns(liveActivityAvailable = true))
    }

    @Test fun is_not_expandable_when_the_only_extra_would_be_cancel() {
        // A planning task: no run id yet, an empty live activity slot.
        val v = view(message("m", "planning"))
        assertTrue(v.canCancel)
        assertFalse(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun becomes_expandable_once_the_live_activity_renders_something() {
        val m = message("m", "running", executionId = "exec-2")
        val v = view(m, listOf(group("exec-2", listOf(m))))
        assertFalse(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
        assertTrue(v.isExpandable(activityHasContent = true, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun offers_more_when_the_reason_overflows_its_clamp() {
        val v = view(message("m", "completed", summary = "A long reason."))
        assertFalse(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
        assertTrue(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = true))
    }

    @Test fun keeps_a_finished_tasks_outputs_in_the_body_and_a_running_tasks_in_the_details() {
        val done = message("r1", "completed", executionId = "e1", outputs = 1)
        val finished = view(done, listOf(group("e1", listOf(done))))
        assertTrue(finished.outputsInBody)
        assertFalse(finished.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))

        val partial = message("r1", "running", executionId = "e1", outputs = 1)
        val running = view(partial, listOf(group("e1", listOf(partial))))
        assertFalse(running.outputsInBody)
        assertTrue(running.isExpandable(
            activityHasContent = false, planStepCount = 0, reasonOverflows = false, liveActivityAvailable = true,
        ))
    }

    @Test fun shows_the_first_two_outputs_and_counts_the_rest_behind_more() {
        val done = message("r1", "completed", executionId = "e1", outputs = 5)
        val v = view(done, listOf(group("e1", listOf(done))))
        assertEquals(3, v.hiddenOutputCount(expanded = false))
        assertEquals(0, v.hiddenOutputCount(expanded = true))
        assertTrue(v.isExpandable(activityHasContent = false, planStepCount = 0, reasonOverflows = false))
    }

    @Test fun the_bare_expandability_rule_is_any_of_its_inputs() {
        assertFalse(LiveTaskCardState.isExpandable(
            hasSteps = false, hasRuns = false, hasOutputs = false,
            hasClampedReason = false, hasHiddenOutputs = false, hasPlanPreview = false))
        assertTrue(LiveTaskCardState.isExpandable(
            hasSteps = false, hasRuns = false, hasOutputs = false,
            hasClampedReason = false, hasHiddenOutputs = false, hasPlanPreview = true))
        assertTrue(LiveTaskCardState.isExpandable(
            hasSteps = true, hasRuns = false, hasOutputs = false,
            hasClampedReason = false, hasHiddenOutputs = false, hasPlanPreview = false))
    }

    // Android chat inputs (iOS "iOS chat inputs")

    @Test fun builds_from_the_chat_task_status_card() {
        val model = TaskStatusCard(
            taskId = "task-1", title = "Review release", status = "draft",
            summary = "Plan ready for review.", executionId = "e1",
        )
        val lagging = LiveTaskCardState.of(LiveTaskCardInput.from(model, messageId = "m1", storeStatus = "ready"))
        assertEquals(LiveTaskStatus.Queued, lagging.status)
        assertNull(lagging.reason)
        assertEquals("Review release", lagging.title)

        // The chat falls back to the raw id as the title; the card never shows it.
        val untitled = TaskStatusCard(
            taskId = "task_2679cefd6f664bf7bd4a3b57fae4ce75",
            title = "task_2679cefd6f664bf7bd4a3b57fae4ce75", status = "running",
        )
        assertEquals("Task · 2679ce", LiveTaskCardState.of(LiveTaskCardInput.from(untitled, messageId = "m2")).title)
    }

    @Test
    fun `an id stand-in title prints Task once and leaves the id to the chip`() {
        val id = "task_2679cefd6f664bf7bd4a3b57fae4ce75"
        assertEquals("2679ce", TaskTitleRules.idStandInShortId("Task · 2679ce", id))
        assertEquals("Task", TaskTitleRules.cardHeading("Task · 2679ce", id))
        // A real title, a different id, or no id keeps the title as written.
        assertNull(TaskTitleRules.idStandInShortId("Summarise the inbox", id))
        assertEquals("Summarise the inbox", TaskTitleRules.cardHeading("Summarise the inbox", id))
        assertNull(TaskTitleRules.idStandInShortId("Task · 2679ce", "task_ffffffaaaa"))
        assertEquals("Task · 2679ce", TaskTitleRules.cardHeading("Task · 2679ce", null))
        assertEquals("Task", TaskTitleRules.cardHeading("Task", id))
        // The rule round-trips the fallback the card state builds.
        val heading = TaskTitleRules.labelOrShortIdTitle(null, id)
        assertEquals("Task", TaskTitleRules.cardHeading(heading, id))
    }
}
