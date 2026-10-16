package ai.magicbeans.magdroid.tasks

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/**
 * One-for-one port of iOS `TaskActsModelTests`: web `actOrder`
 * (`taskCapabilities.test.ts`), `planSummary` / `runSummary` /
 * `outputSummary` (`actSummaries.test.ts`) and `outputKindOf`
 * (`taskPanelModel.test.ts`).
 */
class TaskActsTest {
    private val all = listOf(TaskDetailAct.Plan, TaskDetailAct.Run, TaskDetailAct.Output)

    private val subsets: List<List<TaskDetailAct>>
        get() = all.fold(listOf(emptyList())) { acc, act -> acc + acc.map { it + act } }

    // actOrder

    @Test fun puts_the_result_first_for_a_finished_task_and_keeps_the_rest_in_lifecycle_order() {
        val expected = listOf(TaskDetailAct.Output, TaskDetailAct.Plan, TaskDetailAct.Run)
        assertEquals(expected, TaskActs.actOrder(TaskVerdictState.Finished, all))
        assertEquals(expected, TaskActs.actOrder(TaskVerdictState.Archived, all))
    }

    @Test fun puts_the_live_run_first_while_it_runs_and_the_run_first_once_it_stopped_short() {
        for (state in listOf(TaskVerdictState.Running, TaskVerdictState.Stalled, TaskVerdictState.Paused,
            TaskVerdictState.Failed, TaskVerdictState.Cancelled)) {
            assertEquals(state.name, listOf(TaskDetailAct.Run, TaskDetailAct.Plan, TaskDetailAct.Output),
                TaskActs.actOrder(state, all))
        }
    }

    @Test fun keeps_lifecycle_order_while_waiting_on_the_reader_or_queued() {
        assertEquals(all, TaskActs.actOrder(TaskVerdictState.Waiting, all))
        assertEquals(all, TaskActs.actOrder(TaskVerdictState.Queued, all))
    }

    @Test fun leaves_the_order_alone_when_the_leading_act_is_absent() {
        assertEquals(listOf(TaskDetailAct.Plan, TaskDetailAct.Run),
            TaskActs.actOrder(TaskVerdictState.Finished, listOf(TaskDetailAct.Plan, TaskDetailAct.Run)))
        assertEquals(listOf(TaskDetailAct.Plan, TaskDetailAct.Output),
            TaskActs.actOrder(TaskVerdictState.Running, listOf(TaskDetailAct.Plan, TaskDetailAct.Output)))
    }

    @Test fun is_a_permutation_of_its_input_for_every_state_and_every_subset() {
        for (state in TaskVerdictState.entries) {
            for (acts in subsets) {
                val ordered = TaskActs.actOrder(state, acts)
                assertEquals("$state · $acts", acts.map { it.name }.sorted(), ordered.map { it.name }.sorted())
            }
        }
    }

    @Test fun leads_with_the_act_that_opens_by_default_whenever_no_ask_is_pending() {
        for (state in TaskVerdictState.entries) {
            if (state == TaskVerdictState.Waiting || state == TaskVerdictState.Queued) continue
            val opens = TaskActs.defaultOpenAct(state, all, null)
            assertEquals(state.name, opens, TaskActs.actOrder(state, all).first())
        }
    }

    @Test fun delegates_presence_to_the_capability_rule() {
        assertEquals(
            listOf(TaskDetailAct.Run, TaskDetailAct.Output),
            TaskActs.deriveActs(ActCapabilities(hasPlanAct = false, hasRunAct = true, hasOutputAct = true)),
        )
    }

    // planSummary

    /** A real epoch, so an approval behind it is a real date. */
    private val now = Instant.parse("2026-07-29T12:00:00Z").toEpochMilli()

    private fun plan(
        status: TaskPlanStatus? = TaskPlanStatus.Draft,
        approvedAgoSeconds: Double? = null,
        questions: List<String> = emptyList(),
    ) = PlanSummaryInput(
        status = status,
        approvedAtMillis = approvedAgoSeconds?.let { now - (it * 1_000).toLong() },
        questions = questions,
        nowMillis = now,
    )

    @Test fun quotes_the_first_waiting_question_instead_of_counting_them() {
        assertEquals("2 questions waiting — \"Which quarter?\"",
            ActSummaries.planSummary(plan(questions = listOf("Which quarter?", "Include forecasts?"))))
    }

    @Test fun uses_the_singular_noun_when_there_is_one_question() {
        assertEquals("1 question waiting — \"Which quarter?\"",
            ActSummaries.planSummary(plan(questions = listOf("Which quarter?"))))
    }

    @Test fun trims_the_quoted_question() {
        assertEquals("1 question waiting — \"Which quarter?\"",
            ActSummaries.planSummary(plan(questions = listOf("  Which quarter?\n"))))
    }

    @Test fun falls_back_to_the_bare_count_when_the_question_carries_no_content() {
        assertEquals("2 questions waiting", ActSummaries.planSummary(plan(questions = listOf("   ", "Include forecasts?"))))
        assertEquals("1 question waiting", ActSummaries.planSummary(plan(questions = listOf(""))))
    }

    @Test fun says_a_draft_is_unapproved_when_nothing_is_being_asked() {
        assertEquals("draft, not yet approved", ActSummaries.planSummary(plan()))
    }

    @Test fun says_when_the_plan_was_approved_relative_to_now() {
        assertEquals("approved 6m ago", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 6.0 * 60)))
        assertEquals("approved 3h ago", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 3.0 * 3_600)))
        assertEquals("approved 2d ago", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 2.0 * 86_400)))
    }

    @Test fun says_just_now_rather_than_now_ago_for_a_fresh_approval() {
        assertEquals("approved just now", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 5.0)))
        assertEquals("approved just now", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, -60.0)))
    }

    @Test fun names_the_date_once_the_approval_is_over_a_week_old() {
        val old = ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 30.0 * 86_400))
        assertTrue(old, old.startsWith("approved on "))
        assertFalse(old, old.endsWith(" ago"))
        assertTrue(old.length > "approved on ".length)
    }

    @Test fun omits_the_approval_time_when_it_cannot_be_read() {
        assertEquals("approved", ActSummaries.planSummary(plan(TaskPlanStatus.Approved, null)))
    }

    @Test fun ranks_a_waiting_question_above_the_approval() {
        val summary = ActSummaries.planSummary(plan(TaskPlanStatus.Approved, 6.0 * 60, listOf("Which quarter?")))
        assertEquals("1 question waiting — \"Which quarter?\"", summary)
        assertFalse(summary.contains("approved"))
    }

    @Test fun reports_the_current_status_even_when_the_plan_was_approved_earlier() {
        assertEquals("rejected, so it will not run", ActSummaries.planSummary(plan(TaskPlanStatus.Rejected, 6.0 * 60)))
        assertEquals("draft, not yet approved", ActSummaries.planSummary(plan(TaskPlanStatus.Draft, 6.0 * 60)))
    }

    @Test fun renders_every_status_in_the_readers_own_words() {
        val lines = listOf(
            TaskPlanStatus.Planning to "being planned",
            TaskPlanStatus.Eliciting to "gathering what it needs before planning",
            TaskPlanStatus.Draft to "draft, not yet approved",
            TaskPlanStatus.Approved to "approved",
            TaskPlanStatus.Rejected to "rejected, so it will not run",
            TaskPlanStatus.Failed to "planning failed",
        )
        assertEquals(TaskPlanStatus.entries.size, lines.size)
        for ((status, expected) in lines) assertEquals(expected, ActSummaries.planSummary(plan(status)))
        assertEquals(TaskPlanStatus.Approved, TaskPlanStatus.fromWire(" Approved "))
        assertNull(TaskPlanStatus.fromWire("teleported"))
    }

    @Test fun renders_nothing_when_the_status_is_unknown() {
        assertEquals("", ActSummaries.planSummary(plan(status = null)))
        assertEquals("1 question waiting — \"Which quarter?\"",
            ActSummaries.planSummary(plan(status = null, questions = listOf("Which quarter?"))))
    }

    // runSummary

    private fun run(
        steps: Int = 14, retries: Int = 0, elapsed: Double? = null,
        currentStep: Int? = null, totalSteps: Int? = null, events: Int = 0,
    ) = RunSummaryInput(steps, retries, elapsed, currentStep, totalSteps, events)

    @Test fun summarises_a_finished_run_and_folds_retries_into_their_step() {
        assertEquals("14 steps · 2 retries · 3m 12s", ActSummaries.runSummary(run(retries = 2, elapsed = 192.0)))
        assertEquals("14 steps · 2 retries · 1h 20m", ActSummaries.runSummary(run(retries = 2, elapsed = 80.0 * 60)))
    }

    @Test fun omits_retries_when_there_were_none() {
        assertEquals("14 steps · 3m 12s", ActSummaries.runSummary(run(elapsed = 192.0)))
    }

    @Test fun uses_the_singular_for_one_step_and_one_retry() {
        assertEquals("1 step · 1 retry · 3m 12s", ActSummaries.runSummary(run(steps = 1, retries = 1, elapsed = 192.0)))
    }

    @Test fun shows_position_instead_of_totals_while_live() {
        val live = ActSummaries.runSummary(run(currentStep = 4, totalSteps = 7))
        assertEquals("step 4 of 7", live)
        assertFalse(live.contains("14"))
    }

    @Test fun drops_the_denominator_while_live_when_the_run_was_never_planned() {
        assertEquals("step 4", ActSummaries.runSummary(run(currentStep = 4, totalSteps = null)))
    }

    @Test fun keeps_retries_and_elapsed_visible_while_live() {
        assertEquals("step 4 of 7 · 2 retries · 1m 40s",
            ActSummaries.runSummary(run(retries = 2, elapsed = 100.0, currentStep = 4, totalSteps = 7)))
    }

    @Test fun drops_the_elapsed_segment_rather_than_its_separator_when_the_time_cannot_be_read() {
        for (elapsed in listOf(null, -1.0, Double.NaN)) {
            assertEquals("14 steps · 2 retries", ActSummaries.runSummary(run(retries = 2, elapsed = elapsed)))
        }
    }

    @Test fun keeps_an_instant_run_distinct_from_one_with_no_timing_recorded() {
        assertEquals("14 steps · 0s", ActSummaries.runSummary(run(elapsed = 0.0)))
        assertEquals("14 steps", ActSummaries.runSummary(run(elapsed = null)))
    }

    @Test fun says_a_run_has_not_started_rather_than_rendering_an_empty_line() {
        assertEquals("not started", ActSummaries.runSummary(run(steps = 0)))
    }

    @Test fun counts_events_under_their_own_noun_beside_the_steps() {
        assertEquals("14 steps · 2 retries · 217 events · 3m 12s",
            ActSummaries.runSummary(run(retries = 2, elapsed = 192.0, events = 217)))
        assertEquals("step 4 of 7 · 217 events",
            ActSummaries.runSummary(run(currentStep = 4, totalSteps = 7, events = 217)))
    }

    @Test fun uses_the_singular_for_one_event() {
        assertEquals("14 steps · 1 event", ActSummaries.runSummary(run(events = 1)))
    }

    @Test fun drops_the_event_segment_rather_than_rendering_zero_events() {
        val line = ActSummaries.runSummary(run(elapsed = 192.0, events = 0))
        assertEquals("14 steps · 3m 12s", line)
        assertFalse(line.contains("event"))
    }

    // outputSummary

    private fun file(name: String, kind: OutputKind) = ActOutputFile(name, kind)

    @Test fun names_one_file_and_counts_the_rest_by_kind() {
        assertEquals("report.md and 2 images", ActSummaries.outputSummary(listOf(
            file("report.md", OutputKind.Document), file("revenue.png", OutputKind.Image), file("costs.png", OutputKind.Image),
        )))
    }

    @Test fun counts_the_whole_remainder() {
        assertEquals("report.md and 3 images", ActSummaries.outputSummary(listOf(
            file("report.md", OutputKind.Document), file("revenue.png", OutputKind.Image),
            file("costs.png", OutputKind.Image), file("margin.png", OutputKind.Image),
        )))
    }

    @Test fun names_both_files_when_there_are_exactly_two() {
        assertEquals("report.md and notes.md", ActSummaries.outputSummary(listOf(
            file("report.md", OutputKind.Document), file("notes.md", OutputKind.Document),
        )))
    }

    @Test fun names_a_lone_file_on_its_own() {
        assertEquals("report.md", ActSummaries.outputSummary(listOf(file("report.md", OutputKind.Document))))
    }

    @Test fun is_explicit_about_having_produced_nothing() {
        assertEquals("no output", ActSummaries.outputSummary(emptyList<ActOutputFile>()))
    }

    @Test fun counts_a_same_kind_remainder_by_its_own_word() {
        assertEquals("revenue.png and 2 documents", ActSummaries.outputSummary(listOf(
            file("revenue.png", OutputKind.Image), file("report.md", OutputKind.Document), file("notes.md", OutputKind.Document),
        )))
    }

    @Test fun falls_back_to_a_neutral_plural_when_the_rest_do_not_share_a_kind() {
        assertEquals("report.md and 2 other files", ActSummaries.outputSummary(listOf(
            file("report.md", OutputKind.Document), file("revenue.png", OutputKind.Image), file("raw.bin", OutputKind.Other),
        )))
    }

    @Test fun gives_unclassifiable_files_the_same_neutral_plural() {
        assertEquals("report.md and 2 other files", ActSummaries.outputSummary(listOf(
            file("report.md", OutputKind.Document), file("raw.bin", OutputKind.Other), file("dump.bin", OutputKind.Other),
        )))
    }

    @Test fun names_the_first_file_so_the_header_matches_the_acts_order() {
        val files = listOf(file("revenue.png", OutputKind.Image), file("report.md", OutputKind.Document),
            file("costs.png", OutputKind.Image))
        assertEquals("revenue.png and 2 other files", ActSummaries.outputSummary(files))
        assertEquals("costs.png and 2 other files", ActSummaries.outputSummary(files.reversed()))
    }

    // outputKindOf

    @Test fun classifies_from_the_media_type_first_and_the_extension_only_when_there_is_none() {
        assertEquals(OutputKind.Image, ActSummaries.outputKind("image/png", "chart.bin"))
        assertEquals(OutputKind.Document, ActSummaries.outputKind("application/pdf", "notes"))
        assertEquals(OutputKind.Other, ActSummaries.outputKind("application/json", "data.md"))
        assertEquals(OutputKind.Image, ActSummaries.outputKind(null, "revenue.PNG"))
        assertEquals(OutputKind.Document, ActSummaries.outputKind(null, "report.md"))
        assertEquals(OutputKind.Other, ActSummaries.outputKind("", "archive.tar.gz"))
    }

    @Test fun summarises_task_deliverables_by_their_basename_and_kind() {
        val artifacts = listOf(
            TaskOutputFile(id = "1", relativePath = "outputs/report.md", mediaType = "text/markdown"),
            TaskOutputFile(id = "2", relativePath = "outputs/a.png", mediaType = null),
            TaskOutputFile(id = "3", relativePath = "outputs/b.png", mediaType = "image/png"),
        )
        assertEquals("report.md and 2 images", ActSummaries.outputSummaryOfFiles(artifacts))
    }
}
