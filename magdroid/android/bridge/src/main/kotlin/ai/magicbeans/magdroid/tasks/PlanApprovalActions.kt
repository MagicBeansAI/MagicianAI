package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/*
 * Plan approval — Review plan · Reject · Approve · Approve & Run.
 *
 * Android port of iOS `PlanApprovalActions.swift` (web `planReview` prompt in
 * `promptFor.ts` and `startRunAfterPlanApproval` in `respondToHitl.ts`).
 * Approving a plan does not start a run, so the two affirmatives are spelled
 * out. Approve & Run posts the same answer as Approve
 * (`{type: confirmation, confirmed: true}`), then
 * `POST /v3/tasks/{id}/execute`. The approval stands if the start fails: the
 * outcome says so and the task's Run is still there.
 */

/** One button of a plan-approval prompt, in display order. */
enum class PlanApprovalChoice {
    Review, Reject, Approve, ApproveAndRun;

    /**
     * The fixed labels. Reject and Approve take the backend's own labels when
     * it named them (`deny_label` / `confirm_label`).
     */
    fun label(confirmLabel: String? = null, denyLabel: String? = null): String = when (this) {
        Review -> "Review plan"
        Reject -> nonEmpty(denyLabel) ?: "Reject"
        Approve -> nonEmpty(confirmLabel) ?: "Approve"
        ApproveAndRun -> "Approve & Run"
    }

    /** The `confirmed` value posted, or null for Review (which posts nothing). */
    val confirmed: Boolean?
        get() = when (this) {
            Review -> null
            Reject -> false
            Approve, ApproveAndRun -> true
        }

    private fun nonEmpty(value: String?): String? = value?.trim()?.takeIf { it.isNotEmpty() }
}

/** The answer to send: who asked, and the task the plan belongs to. */
data class PlanApprovalRequest(
    /** `/hitl/{correlationId}/respond`. */
    val correlationId: String,
    /** The task the plan belongs to; Approve & Run starts its run. */
    val taskId: String?,
    /** The run the answer resumes, when the ask names one. */
    val executionId: String? = null,
    val source: String = "plan_approval",
    val channel: String = "android",
)

/** The approval POST's result. */
sealed class PlanApprovalPost {
    data object Ok : PlanApprovalPost()
    data class Failed(val reason: String) : PlanApprovalPost()
}

/** What happened, and the one line the surface shows for it. */
sealed class PlanApprovalOutcome {
    /** Approve alone: approved, no run started. */
    data object Approved : PlanApprovalOutcome()

    /** Approve & Run: approved and the run started. */
    data object ApprovedAndStarted : PlanApprovalOutcome()

    /** Approve & Run: approved, but the run did not start. The approval stands. */
    data class ApprovedRunNotStarted(val reason: String) : PlanApprovalOutcome()

    data object Rejected : PlanApprovalOutcome()

    /** The approval (or rejection) itself did not land; nothing changed. */
    data class Failed(val reason: String) : PlanApprovalOutcome()

    /** The plan is approved, whether or not a run started. */
    val isApproved: Boolean
        get() = this is Approved || this is ApprovedAndStarted || this is ApprovedRunNotStarted

    val message: String
        get() = when (this) {
            Approved -> "Plan approved. Run it when you're ready."
            ApprovedAndStarted -> "Plan approved. The run has started."
            is ApprovedRunNotStarted -> {
                val detail = reason.trim()
                if (detail.isEmpty()) "Approved — the run did not start."
                else "Approved — the run did not start. $detail"
            }
            Rejected -> "Plan rejected. It will not run."
            is Failed -> reason
        }
}

/** The two calls Approve & Run makes. The app's HTTP client in production, a stub in tests. */
interface PlanApprovalClient {
    /** POST the confirmation to `/api/magician/v2/hitl/{id}/respond`. */
    suspend fun respond(request: PlanApprovalRequest, confirmed: Boolean): PlanApprovalPost

    /** `POST /api/magician/v3/tasks/{id}/execute`. Throws when the run did not start. */
    suspend fun startRun(taskId: String)
}

object PlanApprovalActions {
    /** The buttons, in order. Review only when there is a task to open. */
    fun choices(hasTask: Boolean): List<PlanApprovalChoice> =
        if (hasTask) PlanApprovalChoice.entries.toList()
        else PlanApprovalChoice.entries.filter { it != PlanApprovalChoice.Review }

    /** A plan approval is a `plan_approval` confirmation (web `planReviewTaskId`). */
    fun isPlanApproval(source: String?, inputType: String?): Boolean =
        source == "plan_approval" && inputType == "confirmation"

    /**
     * Approve & Run's core: run [approve], and only once it landed start the
     * task's run. A failed start keeps the approval and says so.
     */
    suspend fun approveAndRun(
        taskId: String?,
        approve: suspend () -> PlanApprovalPost,
        startRun: suspend (String) -> Unit,
    ): PlanApprovalOutcome {
        when (val posted = approve()) {
            is PlanApprovalPost.Failed -> return PlanApprovalOutcome.Failed(posted.reason)
            PlanApprovalPost.Ok -> Unit
        }
        val id = taskId?.trim()?.takeIf { it.isNotEmpty() }
            ?: return PlanApprovalOutcome.ApprovedRunNotStarted("This plan names no task to run.")
        return try {
            startRun(id)
            PlanApprovalOutcome.ApprovedAndStarted
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            PlanApprovalOutcome.ApprovedRunNotStarted(error.message ?: error.toString())
        }
    }

    /** Approve & Run through a client: post the confirmation, then start the run. */
    suspend fun approveAndRun(request: PlanApprovalRequest, client: PlanApprovalClient): PlanApprovalOutcome =
        approveAndRun(
            taskId = request.taskId,
            approve = { client.respond(request, confirmed = true) },
            startRun = { client.startRun(it) },
        )

    /** Approve alone: post the confirmation; no run starts. */
    suspend fun approve(request: PlanApprovalRequest, client: PlanApprovalClient): PlanApprovalOutcome =
        when (val posted = client.respond(request, confirmed = true)) {
            PlanApprovalPost.Ok -> PlanApprovalOutcome.Approved
            is PlanApprovalPost.Failed -> PlanApprovalOutcome.Failed(posted.reason)
        }

    suspend fun reject(request: PlanApprovalRequest, client: PlanApprovalClient): PlanApprovalOutcome =
        when (val posted = client.respond(request, confirmed = false)) {
            PlanApprovalPost.Ok -> PlanApprovalOutcome.Rejected
            is PlanApprovalPost.Failed -> PlanApprovalOutcome.Failed(posted.reason)
        }

    /** Any of the three answering choices. Review posts nothing: null. */
    suspend fun perform(
        choice: PlanApprovalChoice,
        request: PlanApprovalRequest,
        client: PlanApprovalClient,
    ): PlanApprovalOutcome? = when (choice) {
        PlanApprovalChoice.Review -> null
        PlanApprovalChoice.Reject -> reject(request, client)
        PlanApprovalChoice.Approve -> approve(request, client)
        PlanApprovalChoice.ApproveAndRun -> approveAndRun(request, client)
    }

    /** The respond body (web `resolveHitlCall` parity: planning sources name the task). */
    fun respondPayload(request: PlanApprovalRequest, confirmed: Boolean): JsonObject {
        val payload = linkedMapOf<String, JsonElement>(
            "source" to JsonPrimitive(request.source),
            "channel" to JsonPrimitive(request.channel),
            "value" to JsonObject(linkedMapOf(
                "type" to JsonPrimitive("confirmation"),
                "confirmed" to JsonPrimitive(confirmed),
            )),
            "input_type" to JsonPrimitive("confirmation"),
            "correlation_id" to JsonPrimitive(request.correlationId),
            "pause_state_id" to JsonPrimitive(request.correlationId),
        )
        request.executionId?.takeIf { it.isNotEmpty() }?.let { payload["execution_id"] = JsonPrimitive(it) }
        request.taskId?.takeIf { it.isNotEmpty() }?.let { payload["task_id"] = JsonPrimitive(it) }
        return JsonObject(payload)
    }

    /**
     * How a `/hitl/{id}/respond` answer reads (iOS `LivePlanApprovalClient`):
     * a 2xx is Ok unless the body says `accepted: false` / `resumed: false`;
     * a 409 `already_resolved` is Ok; anything else fails with the server's
     * `message` / `reason`.
     */
    fun respondOutcome(status: Int, body: JsonObject?): PlanApprovalPost {
        val message = body?.get("message").jStr() ?: body?.get("reason").jStr()
        return when {
            status in 200..299 ->
                if (body?.get("accepted").jBool() == false || body?.get("resumed").jBool() == false)
                    PlanApprovalPost.Failed(message ?: "The answer was not accepted.")
                else PlanApprovalPost.Ok
            status == 409 && body?.get("reason").jStr() == "already_resolved" -> PlanApprovalPost.Ok
            else -> PlanApprovalPost.Failed(message ?: "The answer failed (HTTP $status).")
        }
    }
}
