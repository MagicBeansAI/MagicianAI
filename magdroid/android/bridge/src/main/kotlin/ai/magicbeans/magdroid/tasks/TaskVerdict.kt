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
    BotAuth("bot_auth", "Sign in to the connected account to continue", TaskDetailAct.Run);

    companion object {
        fun fromWire(value: String?): TaskAttentionSource? = entries.firstOrNull { it.wire == value }
    }
}

data class TaskAttention(
    val source: TaskAttentionSource,
    val summary: String? = null,
    val raisedAtMillis: Long? = null,
)

enum class TaskVerdictState { Waiting, Failed, Stalled, Running, Paused, Cancelled, Queued, Archived, Finished }
enum class TaskVerdictSeverity { Attention, Failure, Progress, Neutral, Success }

data class TaskVerdictValue(
    val state: TaskVerdictState,
    val headline: String,
    val detail: String,
) {
    val severity: TaskVerdictSeverity get() = when (state) {
        TaskVerdictState.Waiting, TaskVerdictState.Stalled -> TaskVerdictSeverity.Attention
        TaskVerdictState.Failed -> TaskVerdictSeverity.Failure
        TaskVerdictState.Running -> TaskVerdictSeverity.Progress
        TaskVerdictState.Paused, TaskVerdictState.Cancelled,
        TaskVerdictState.Queued, TaskVerdictState.Archived -> TaskVerdictSeverity.Neutral
        TaskVerdictState.Finished -> TaskVerdictSeverity.Success
    }
}

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
)

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
        if (status == "queued") return TaskVerdictValue(
            TaskVerdictState.Queued, "Queued", "Waiting for a free slot",
        )
        if (status == "archived") return TaskVerdictValue(
            TaskVerdictState.Archived, "Archived", "No longer active",
        )
        val took = durationIfKnown(input.elapsedSeconds)
        return TaskVerdictValue(
            TaskVerdictState.Finished,
            took?.let { "Finished · $it" } ?: "Finished",
            "",
        )
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
            TaskVerdictState.Archived, TaskVerdictState.Finished -> TaskDetailAct.Output
        }
        if (preferred in acts) return preferred
        val from = TaskDetailAct.entries.indexOf(preferred)
        for (index in from - 1 downTo 0) if (TaskDetailAct.entries[index] in acts) return TaskDetailAct.entries[index]
        for (index in from + 1 until TaskDetailAct.entries.size) if (TaskDetailAct.entries[index] in acts) return TaskDetailAct.entries[index]
        return null
    }
}
