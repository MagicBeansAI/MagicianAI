package ai.magicbeans.magdroid.tasks

import kotlin.math.round

enum class TaskDetailAct { Plan, Run, Output }

enum class TaskAttentionSource(val wire: String, val copy: String, val answeredIn: TaskDetailAct) {
    Agentic("agentic", "The run needs an answer before it can continue", TaskDetailAct.Run),
    UserRequest("user_request", "The run is asking you something", TaskDetailAct.Run),
    Approval("approval", "Approve this before it can continue", TaskDetailAct.Run),
    PlanApproval("plan_approval", "Approve the plan before it can run", TaskDetailAct.Plan),
    Clarification("clarification", "Answer a question so planning can finish", TaskDetailAct.Plan),
    Escalation("escalation", "The run got stuck and needs a decision", TaskDetailAct.Run),
    DiffApproval("diff_approval", "Review the file changes before they are applied", TaskDetailAct.Run),
    ServiceHealth("service_health", "Check service credentials, balance, or connection", TaskDetailAct.Run),
    BotAuth("bot_auth", "Sign in to the connected account to continue", TaskDetailAct.Run),
    McpOAuth("mcp_oauth", "Connect the service in your browser to continue", TaskDetailAct.Run);

    companion object {
        fun fromWire(value: String?): TaskAttentionSource? = entries.firstOrNull { it.wire == value }
    }
}

data class TaskAttention(
    val source: TaskAttentionSource,
    val summary: String? = null,
    val raisedAtMillis: Long? = null,
)

/**
 * [PartlyDone]: completed with `completion_kind == "partial"`, requirements still open.
 * [Finalizing]: the work is done but the final answer is still being written
 * (`synthesis_pending`).
 */
enum class TaskVerdictState { Waiting, Failed, Stalled, Running, Paused, Cancelled, Queued, Archived, Finalizing, Finished, PartlyDone }
enum class TaskVerdictSeverity { Attention, Failure, Progress, Neutral, Success }

data class TaskVerdictValue(
    val state: TaskVerdictState,
    val headline: String,
    val detail: String,
) {
    val severity: TaskVerdictSeverity get() = when (state) {
        TaskVerdictState.Waiting, TaskVerdictState.Stalled, TaskVerdictState.PartlyDone -> TaskVerdictSeverity.Attention
        TaskVerdictState.Failed -> TaskVerdictSeverity.Failure
        TaskVerdictState.Running, TaskVerdictState.Finalizing -> TaskVerdictSeverity.Progress
        TaskVerdictState.Paused, TaskVerdictState.Cancelled,
        TaskVerdictState.Queued, TaskVerdictState.Archived -> TaskVerdictSeverity.Neutral
        TaskVerdictState.Finished -> TaskVerdictSeverity.Success
    }
}

/**
 * Why a task that has not started has not started. The verdict keeps one
 * state for all of them (`Queued`); the second line is a function of the
 * reason. V3 has no runner queue, so no reason is "waiting for a free slot".
 * Web `QueuedReason` (`taskVerdict.ts`), iOS `QueuedReason`.
 */
enum class QueuedReason(val copy: String) {
    /** `pending`: never planned, never started. */
    Start("Not started yet"),

    /** `ready`: planned and approved; approval does not start a run. */
    Approved("Plan approved, not started yet"),

    /** `planning`: the planner is working now. */
    Plan("Working out a plan before it starts"),

    /** `deferred` / `sleeping`: waiting on a time. */
    Schedule("Scheduled to start later"),
}

/**
 * A task status as the verdict reads it: the status word `derive` checks,
 * and why a `queued` one has not started. Web `VERDICT_STATUS`.
 */
data class TaskVerdictStatus(val word: String, val queuedFor: QueuedReason?)

data class TaskVerdictInput(
    val status: String,
    val attention: TaskAttention? = null,
    val error: String? = null,
    val currentStep: Int? = null,
    val totalSteps: Int? = null,
    val currentStepLabel: String? = null,
    val elapsedSeconds: Double? = null,
    val lastProgressAtMillis: Long? = null,
    val nowMillis: Long = System.currentTimeMillis(),
    /** Why a `queued` task has not started; null reads `Waiting to start`. */
    val queuedFor: QueuedReason? = null,
    /** A finished task's `completion_kind` (`full` | `partial`); partial reads Partly done. */
    val completionKind: String? = null,
    /** A partial completion's `open_items`; the first is the verdict's detail line. */
    val openItems: List<String> = emptyList(),
    /**
     * The work is done but its final answer is still being written
     * (`synthesis_pending`). Only changes a would-be finished verdict: failed,
     * waiting, cancelled and the live states all outrank it.
     */
    val synthesisPending: Boolean = false,
)

/**
 * The client side of `synthesis_pending`: when to stop spinning. The backend
 * heals a stuck synthesis (startup reconciler, failure path), but a client must
 * not spin forever on a flag that never clears.
 */
object SynthesisWait {
    const val STALE_AFTER_MILLIS = 10 * 60 * 1_000L

    /** True once the flag has been pending for more than ten minutes since [baselineMillis]. */
    fun isStale(baselineMillis: Long, nowMillis: Long): Boolean =
        nowMillis - baselineMillis > STALE_AFTER_MILLIS
}

/** Exact Android mirror of the shared web/iOS task verdict and act ordering. */
object TaskVerdict {
    const val STALL_AFTER_SECONDS = 5 * 60

    fun stepPhrase(step: Int?, total: Int?): String = when {
        step == null -> ""
        total == null -> "step $step"
        else -> "step $step of $total"
    }

    fun durationIfKnown(seconds: Double?): String? {
        if (seconds == null || !seconds.isFinite() || seconds < 0) return null
        val rounded = round(seconds)
        if (rounded > Long.MAX_VALUE.toDouble()) return null
        val whole = rounded.toLong()
        if (whole < 60) return "${whole}s"
        val minutes = whole / 60
        if (minutes < 60) {
            val remainder = whole % 60
            return if (remainder == 0L) "${minutes}m" else "${minutes}m ${remainder}s"
        }
        val hours = minutes / 60
        val remainder = minutes % 60
        return if (remainder == 0L) "${hours}h" else "${hours}h ${remainder}m"
    }

    fun derive(input: TaskVerdictInput): TaskVerdictValue {
        val status = input.status.lowercase().let { if (it == "canceled") "cancelled" else it }
        val step = stepPhrase(input.currentStep, input.totalSteps)
        input.attention?.let { attention ->
            val blocked = attention.raisedAtMillis?.let {
                durationIfKnown((input.nowMillis - it).toDouble() / 1_000.0)
            }
            return TaskVerdictValue(
                TaskVerdictState.Waiting,
                blocked?.let { "Waiting on you · $it" } ?: "Waiting on you",
                attention.summary ?: attention.source.copy,
            )
        }
        if (status == "failed") return TaskVerdictValue(
            TaskVerdictState.Failed,
            if (step.isEmpty()) "Failed" else "Failed · at $step",
            input.error ?: "No error message was recorded",
        )
        if (status == "running" && input.lastProgressAtMillis != null) {
            val silent = (input.nowMillis - input.lastProgressAtMillis).toDouble() / 1_000.0
            if (silent >= STALL_AFTER_SECONDS) durationIfKnown(silent)?.let { duration ->
                val numbered = stepPhrase(input.currentStep, null)
                return TaskVerdictValue(
                    TaskVerdictState.Stalled,
                    "Stalled · no progress for $duration",
                    input.currentStepLabel?.let {
                        "Still on ${if (numbered.isEmpty()) "this step" else numbered}: $it"
                    } ?: "The run has not advanced",
                )
            }
        }
        if (status == "running") return TaskVerdictValue(
            TaskVerdictState.Running,
            if (step.isEmpty()) "Running" else "Running · $step",
            input.currentStepLabel ?: "Working",
        )
        if (status == "paused") return TaskVerdictValue(
            TaskVerdictState.Paused, "Paused", "Ready to resume when you are",
        )
        if (status == "cancelled") {
            val ran = durationIfKnown(input.elapsedSeconds)
            return TaskVerdictValue(
                TaskVerdictState.Cancelled,
                ran?.let { "Cancelled · after $it" } ?: "Cancelled",
                if (step.isEmpty()) "You stopped this" else "You stopped this at $step",
            )
        }
        // A null reason is one nobody stated: say less rather than borrow
        // another reason's sentence.
        if (status == "queued") return TaskVerdictValue(
            TaskVerdictState.Queued, "Queued", input.queuedFor?.copy ?: "Waiting to start",
        )
        if (status == "archived") return TaskVerdictValue(
            TaskVerdictState.Archived, "Archived", "No longer active",
        )
        val took = durationIfKnown(input.elapsedSeconds)
        // After every state above, so a failure, an ask or a cancel still wins;
        // before Finished / PartlyDone so the screen never says "Finished"
        // while the answer is not there yet.
        if (input.synthesisPending) return TaskVerdictValue(
            TaskVerdictState.Finalizing,
            "Preparing final result",
            took?.let { "The work finished in $it; the answer is being written" }
                ?: "The work finished; the answer is being written",
        )
        if (input.completionKind == "partial") return TaskVerdictValue(
            TaskVerdictState.PartlyDone,
            took?.let { "${TaskCompletion.PARTLY_DONE} · $it" } ?: TaskCompletion.PARTLY_DONE,
            TaskCompletion.openLine(input.openItems) ?: "Finished with requirements still open",
        )
        return TaskVerdictValue(
            TaskVerdictState.Finished,
            took?.let { "Finished · $it" } ?: "Finished",
            "",
        )
    }

    /**
     * A wire task status as the verdict reads it (web `VERDICT_STATUS` over
     * `normalizeV3TaskStatus`, plus the chat's spellings). Anything else is
     * `pending`: queued, never `Finished`.
     */
    fun verdictStatus(taskStatus: String): TaskVerdictStatus = when (taskStatus.trim().lowercase()) {
        "running", "waiting_for_children", "in_progress", "executing" -> TaskVerdictStatus("running", null)
        "failed", "error" -> TaskVerdictStatus("failed", null)
        "cancelled", "canceled" -> TaskVerdictStatus("cancelled", null)
        "completed", "complete", "done" -> TaskVerdictStatus("finished", null)
        "planning" -> TaskVerdictStatus("queued", QueuedReason.Plan)
        "ready" -> TaskVerdictStatus("queued", QueuedReason.Approved)
        "deferred", "sleeping" -> TaskVerdictStatus("queued", QueuedReason.Schedule)
        "paused", "waiting_for_user", "waiting_for_confirmation", "paused_by_user" -> TaskVerdictStatus("paused", null)
        "archived" -> TaskVerdictStatus("archived", null)
        else -> TaskVerdictStatus("queued", QueuedReason.Start)
    }

    /**
     * [derive] over a raw wire task status (`input.status` as the task store
     * sends it): mapped through [verdictStatus] first, so `ready` and
     * `pending` read Queued instead of falling through to Finished.
     */
    fun deriveForTaskStatus(input: TaskVerdictInput): TaskVerdictValue {
        val mapped = verdictStatus(input.status)
        return derive(input.copy(status = mapped.word, queuedFor = mapped.queuedFor ?: input.queuedFor))
    }

    fun acts(hasPlan: Boolean, hasRun: Boolean, hasOutput: Boolean): List<TaskDetailAct> =
        TaskDetailAct.entries.filter { act ->
            when (act) {
                TaskDetailAct.Plan -> hasPlan
                TaskDetailAct.Run -> hasRun
                TaskDetailAct.Output -> hasOutput
            }
        }

    fun defaultOpenAct(
        state: TaskVerdictState,
        acts: List<TaskDetailAct>,
        attention: TaskAttentionSource?,
    ): TaskDetailAct? {
        val preferred = attention?.answeredIn ?: when (state) {
            TaskVerdictState.Waiting, TaskVerdictState.Queued -> TaskDetailAct.Plan
            TaskVerdictState.Failed, TaskVerdictState.Stalled, TaskVerdictState.Running,
            TaskVerdictState.Paused, TaskVerdictState.Cancelled -> TaskDetailAct.Run
            TaskVerdictState.Archived, TaskVerdictState.Finalizing, TaskVerdictState.Finished,
            TaskVerdictState.PartlyDone -> TaskDetailAct.Output
        }
        if (preferred in acts) return preferred
        val from = TaskDetailAct.entries.indexOf(preferred)
        for (index in from - 1 downTo 0) if (TaskDetailAct.entries[index] in acts) return TaskDetailAct.entries[index]
        for (index in from + 1 until TaskDetailAct.entries.size) if (TaskDetailAct.entries[index] in acts) return TaskDetailAct.entries[index]
        return null
    }
}
