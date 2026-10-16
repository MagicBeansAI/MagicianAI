package ai.magicbeans.magdroid.tasks

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Plan approval: the four choices and Approve & Run's two-call order. One-for-
 * one port of iOS `PlanApprovalActionsTests` (web `promptFor.test.ts` and
 * `respondToHitl.ts` `startRunAfterPlanApproval`).
 */
class PlanApprovalActionsTest {
    /** Records every call in order; each answer is scripted. */
    private class StubClient : PlanApprovalClient {
        var respondResult: PlanApprovalPost = PlanApprovalPost.Ok
        var startError: Exception? = null
        val calls = mutableListOf<String>()
        val postedConfirmed = mutableListOf<Boolean>()

        override suspend fun respond(request: PlanApprovalRequest, confirmed: Boolean): PlanApprovalPost {
            calls.add("respond:${request.correlationId}")
            postedConfirmed.add(confirmed)
            return respondResult
        }

        override suspend fun startRun(taskId: String) {
            calls.add("execute:$taskId")
            startError?.let { throw it }
        }
    }

    private val request = PlanApprovalRequest(correlationId = "plan_1", taskId = "task_42")

    // Choices

    @Test fun offers_review_reject_approve_and_approve_and_run_in_that_order() {
        assertEquals(
            listOf(PlanApprovalChoice.Review, PlanApprovalChoice.Reject, PlanApprovalChoice.Approve, PlanApprovalChoice.ApproveAndRun),
            PlanApprovalActions.choices(hasTask = true),
        )
        assertEquals(
            listOf("Review plan", "Reject", "Approve", "Approve & Run"),
            PlanApprovalActions.choices(hasTask = true).map { it.label() },
        )
    }

    @Test fun drops_review_when_there_is_no_task_to_open() {
        assertEquals(
            listOf(PlanApprovalChoice.Reject, PlanApprovalChoice.Approve, PlanApprovalChoice.ApproveAndRun),
            PlanApprovalActions.choices(hasTask = false),
        )
    }

    @Test fun reject_and_approve_take_the_backend_labels_but_approve_and_run_keeps_its_own() {
        assertEquals("Discard", PlanApprovalChoice.Reject.label(denyLabel = "Discard"))
        assertEquals("Accept", PlanApprovalChoice.Approve.label(confirmLabel = "Accept"))
        assertEquals("Approve", PlanApprovalChoice.Approve.label(confirmLabel = "  "))
        assertEquals("Approve & Run", PlanApprovalChoice.ApproveAndRun.label(confirmLabel = "Accept"))
    }

    @Test fun only_a_plan_approval_confirmation_is_a_plan_review() {
        assertTrue(PlanApprovalActions.isPlanApproval("plan_approval", "confirmation"))
        assertFalse(PlanApprovalActions.isPlanApproval("plan_approval", "text"))
        assertFalse(PlanApprovalActions.isPlanApproval("agentic", "confirmation"))
    }

    @Test fun approve_and_run_posts_the_same_confirmation_as_approve() {
        assertEquals(true, PlanApprovalChoice.ApproveAndRun.confirmed)
        assertEquals(true, PlanApprovalChoice.Approve.confirmed)
        assertEquals(false, PlanApprovalChoice.Reject.confirmed)
        assertNull(PlanApprovalChoice.Review.confirmed)
    }

    @Test fun the_respond_body_names_the_task_and_the_confirmation() {
        val body = PlanApprovalActions.respondPayload(request.copy(executionId = "planexec_1"), confirmed = true)
        assertEquals("plan_approval", body["source"].jStr())
        assertEquals("task_42", body["task_id"].jStr())
        assertEquals("planexec_1", body["execution_id"].jStr())
        assertEquals("plan_1", body["correlation_id"].jStr())
        val value = body["value"].jObj()
        assertEquals("confirmation", value?.get("type").jStr())
        assertEquals(true, value?.get("confirmed").jBool())
    }

    // Two-call sequence

    @Test fun approve_and_run_approves_first_then_starts_the_run() = runBlocking<Unit> {
        val client = StubClient()
        val outcome = PlanApprovalActions.approveAndRun(request, client)
        assertEquals(PlanApprovalOutcome.ApprovedAndStarted, outcome)
        assertEquals(listOf("respond:plan_1", "execute:task_42"), client.calls)
        assertEquals(listOf(true), client.postedConfirmed)
    }

    @Test fun a_failed_start_keeps_the_approval_and_says_the_run_did_not_start() = runBlocking<Unit> {
        val client = StubClient()
        client.startError = IllegalStateException("Busy.")
        val outcome = PlanApprovalActions.approveAndRun(request, client)
        assertEquals(PlanApprovalOutcome.ApprovedRunNotStarted("Busy."), outcome)
        assertTrue(outcome.isApproved)
        assertTrue(outcome.message.startsWith("Approved — the run did not start"))
        assertEquals(listOf("respond:plan_1", "execute:task_42"), client.calls)
    }

    @Test fun a_failed_approval_never_starts_a_run() = runBlocking<Unit> {
        val client = StubClient()
        client.respondResult = PlanApprovalPost.Failed("Gone.")
        val outcome = PlanApprovalActions.approveAndRun(request, client)
        assertEquals(PlanApprovalOutcome.Failed("Gone."), outcome)
        assertFalse(outcome.isApproved)
        assertEquals(listOf("respond:plan_1"), client.calls)
    }

    @Test fun approve_and_run_without_a_task_approves_and_says_the_run_did_not_start() = runBlocking<Unit> {
        val client = StubClient()
        val outcome = PlanApprovalActions.approveAndRun(PlanApprovalRequest(correlationId = "plan_1", taskId = null), client)
        assertTrue(outcome.toString(), outcome is PlanApprovalOutcome.ApprovedRunNotStarted)
        assertEquals(listOf("respond:plan_1"), client.calls)
    }

    @Test fun approve_alone_starts_nothing_and_says_to_run_it_when_ready() = runBlocking<Unit> {
        val client = StubClient()
        val outcome = PlanApprovalActions.approve(request, client)
        assertEquals(PlanApprovalOutcome.Approved, outcome)
        assertEquals(listOf("respond:plan_1"), client.calls)
        assertTrue(outcome.message.contains("Run it when you're ready"))
    }

    /** Android extra: how the app's HTTP client reads a respond answer (iOS `LivePlanApprovalClient`). */
    @Test fun a_respond_answer_is_ok_unless_refused_and_already_resolved_counts_as_ok() {
        assertEquals(PlanApprovalPost.Ok, PlanApprovalActions.respondOutcome(200, planObj("accepted" to true)))
        assertEquals(PlanApprovalPost.Failed("Paused elsewhere"),
            PlanApprovalActions.respondOutcome(200, planObj("resumed" to false, "message" to "Paused elsewhere")))
        assertEquals(PlanApprovalPost.Ok, PlanApprovalActions.respondOutcome(409, planObj("reason" to "already_resolved")))
        assertEquals(PlanApprovalPost.Failed("The answer failed (HTTP 500)."), PlanApprovalActions.respondOutcome(500, null))
    }

    @Test fun reject_posts_false_and_review_posts_nothing()= runBlocking<Unit> {
        val client = StubClient()
        val rejected = PlanApprovalActions.perform(PlanApprovalChoice.Reject, request, client)
        assertEquals(PlanApprovalOutcome.Rejected, rejected)
        assertEquals(listOf(false), client.postedConfirmed)
        val reviewed = PlanApprovalActions.perform(PlanApprovalChoice.Review, request, client)
        assertNull(reviewed)
        assertEquals(1, client.calls.size)
    }
}
