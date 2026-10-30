package ai.magicbeans.magdroid.tasks

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The queued wording and the wire-status map, ported from iOS
 * `TaskVerdictTests` (web `taskVerdict.test.ts`) and the detail-verdict case
 * of iOS `TaskParityRulesTests`. The older verdict cases stay in
 * `TaskParityContractTest`.
 */
class TaskVerdictTest {
    private fun base(
        status: String,
        queuedFor: QueuedReason? = null,
        synthesisPending: Boolean = false,
        attention: TaskAttention? = null,
    ) = TaskVerdictInput(
        status = status,
        queuedFor = queuedFor,
        synthesisPending = synthesisPending,
        attention = attention,
        currentStep = 4,
        totalSteps = 7,
        currentStepLabel = "Draft the brief",
        elapsedSeconds = 192.0,
        nowMillis = 1_000_000L,
    )

    @Test fun reports_an_unstarted_task_as_not_started_rather_than_as_started() {
        val v = TaskVerdict.derive(base("queued", QueuedReason.Start))
        assertEquals(TaskVerdictState.Queued, v.state)
        assertEquals("Queued", v.headline)
        assertEquals("Not started yet", v.detail)
    }

    /** One state, four statuses, four sentences. */
    @Test fun gives_each_reason_a_queued_task_has_not_started_its_own_second_line() {
        fun detail(reason: QueuedReason?) = TaskVerdict.derive(base("queued", reason)).detail
        assertEquals("Not started yet", detail(QueuedReason.Start))
        assertEquals("Plan approved, not started yet", detail(QueuedReason.Approved))
        assertEquals("Working out a plan before it starts", detail(QueuedReason.Plan))
        assertEquals("Scheduled to start later", detail(QueuedReason.Schedule))

        // Distinct, not merely present.
        val lines = QueuedReason.entries.map { detail(it) }.toSet()
        assertEquals(4, lines.size)
        assertEquals(4, QueuedReason.entries.size)
        for (line in lines) assertFalse(line.isEmpty())
    }

    @Test fun says_less_rather_than_something_false_when_nothing_named_the_reason() {
        val v = TaskVerdict.derive(base("queued", null))
        assertEquals(TaskVerdictState.Queued, v.state)
        assertEquals("Waiting to start", v.detail)
        assertNotEquals("Not started yet", v.detail)
    }

    /** Web `VERDICT_STATUS` over `normalizeV3TaskStatus`. */
    @Test fun maps_each_task_status_to_its_verdict_word_and_queued_reason() {
        val table = listOf(
            Triple("running", "running", null), Triple("failed", "failed", null),
            Triple("cancelled", "cancelled", null), Triple("completed", "finished", null),
            Triple("pending", "queued", QueuedReason.Start), Triple("planning", "queued", QueuedReason.Plan),
            Triple("ready", "queued", QueuedReason.Approved), Triple("deferred", "queued", QueuedReason.Schedule),
            Triple("sleeping", "queued", QueuedReason.Schedule), Triple("paused", "paused", null),
            Triple("waiting_for_user", "paused", null), Triple("waiting_for_children", "running", null),
            Triple("archived", "archived", null),
            // An unknown status is `pending` (web): queued, never `Finished`.
            Triple("teleported", "queued", QueuedReason.Start),
        )
        for ((wire, word, reason) in table) {
            val mapped = TaskVerdict.verdictStatus(wire)
            assertEquals(wire, word, mapped.word)
            assertEquals(wire, reason, mapped.queuedFor)
        }
    }

    /** No queued shape claims capacity, and only `ready` says its plan is approved. */
    @Test fun never_claims_capacity_and_only_an_approved_plan_says_it_is_approved() {
        for (wire in listOf("pending", "planning", "ready", "deferred", "sleeping")) {
            val mapped = TaskVerdict.verdictStatus(wire)
            val v = TaskVerdict.derive(base(mapped.word, mapped.queuedFor))
            assertEquals(wire, TaskVerdictState.Queued, v.state)
            assertNotEquals(wire, "Waiting for a free slot", v.detail)
            assertEquals(wire, wire == "ready", v.detail == "Plan approved, not started yet")
        }
    }

    /** iOS `TaskParityRulesTests.testDetailVerdictReadsAnUnstartedTaskAsQueuedNotFinished`. */
    @Test fun detail_verdict_reads_an_unstarted_task_as_queued_not_finished() {
        val ready = TaskVerdict.deriveForTaskStatus(base("ready"))
        assertEquals(TaskVerdictState.Queued, ready.state)
        assertEquals("Queued", ready.headline)
        assertEquals("Plan approved, not started yet", ready.detail)
        assertEquals("Not started yet", TaskVerdict.deriveForTaskStatus(base("pending")).detail)
        assertEquals(TaskVerdictState.Finished, TaskVerdict.deriveForTaskStatus(base("completed")).state)
    }

    @Test fun synthesis_pending_on_a_completed_task_reads_as_finalizing() {
        val v = TaskVerdict.deriveForTaskStatus(base("completed", synthesisPending = true))
        assertEquals(TaskVerdictState.Finalizing, v.state)
        assertEquals("Preparing final result", v.headline)
        assertEquals(TaskVerdictSeverity.Progress, v.severity)
    }

    @Test fun synthesis_pending_never_hides_a_failure_a_cancel_or_an_open_ask() {
        assertEquals(TaskVerdictState.Failed, TaskVerdict.deriveForTaskStatus(base("failed", synthesisPending = true)).state)
        assertEquals(TaskVerdictState.Cancelled, TaskVerdict.deriveForTaskStatus(base("cancelled", synthesisPending = true)).state)
        assertEquals(
            TaskVerdictState.Waiting,
            TaskVerdict.deriveForTaskStatus(
                base("completed", synthesisPending = true, attention = TaskAttention(TaskAttentionSource.Approval)),
            ).state,
        )
        assertEquals(TaskVerdictState.Running, TaskVerdict.deriveForTaskStatus(base("running", synthesisPending = true)).state)
    }

    @Test fun without_the_flag_a_completed_task_still_finishes() {
        val v = TaskVerdict.deriveForTaskStatus(base("completed", synthesisPending = false))
        assertEquals(TaskVerdictState.Finished, v.state)
        assertEquals("Finished · 3m 12s", v.headline)
    }

    @Test fun finalizing_leads_with_the_output_act() {
        val all = listOf(TaskDetailAct.Plan, TaskDetailAct.Run, TaskDetailAct.Output)
        assertEquals(TaskDetailAct.Output, TaskVerdict.defaultOpenAct(TaskVerdictState.Finalizing, all, null))
        assertEquals(
            listOf(TaskDetailAct.Output, TaskDetailAct.Plan, TaskDetailAct.Run),
            TaskActs.actOrder(TaskVerdictState.Finalizing, all),
        )
    }

    @Test fun the_wait_goes_stale_only_after_ten_minutes() {
        val start = 1_000_000L
        assertFalse(SynthesisWait.isStale(start, start))
        assertFalse(SynthesisWait.isStale(start, start + 600_000L))
        assertTrue(SynthesisWait.isStale(start, start + 600_001L))
        // A device clock behind the server's instant is not stale.
        assertFalse(SynthesisWait.isStale(start, start - 30_000L))
    }
}
