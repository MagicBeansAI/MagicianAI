package ai.magicbeans.magdroid.chat

import kotlin.math.floor

/*
 * The chat live task card's state: one card for a task's whole life, from
 * planning through approval to the run's end. Pure — no Compose, no clock of
 * its own (the view passes `nowMillis`).
 *
 * Android port of iOS `LiveTaskCardModel.swift` (itself a port of web
 * `chat/next/liveTask.ts` and the state rules `LiveTaskCard.svelte` derives).
 * Design §2 of `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`.
 */

enum class LiveTaskStatus {
    /**
     * Not started: `ready` (plan approved), `pending` (never planned) or
     * `rejected`. No run exists, so the card does not tick, offer Cancel or
     * read as working.
     */
    Queued,
    Running,
    Waiting,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

/** One run's state, the vocabulary of the turn-progress rows. */
enum class LiveTaskStepStatus { Running, Done, Failed, Waiting }

data class LiveTaskStep(val key: String, val label: String, val status: LiveTaskStepStatus)

/** One run's output blocks, as the card shows them. */
data class LiveTaskOutput(
    val key: String,
    /** Run label, only when the task has more than one run. */
    val label: String?,
    val sessionId: String?,
    val count: Int,
)

/**
 * One run of the task (web `ChatRenderTaskExecutionGroup`): its latest
 * update's status and ids, every update's time and the latest output count.
 */
data class LiveTaskRunInput(
    val id: String,
    val status: String? = null,
    val executionId: String? = null,
    val sessionId: String? = null,
    /** Epoch milliseconds, or epoch seconds (below 1e11). */
    val timestamps: List<Double> = emptyList(),
    val outputCount: Int = 0,
)

/** What the card reads: the latest `task_status_update`, the store's status when known, and the runs. */
data class LiveTaskCardInput(
    val messageId: String,
    val taskId: String? = null,
    val displayLabel: String? = null,
    /** The card message's status. */
    val status: String? = null,
    /** The task's status in the task store, when the host has it. */
    val storeStatus: String? = null,
    val summary: String? = null,
    val synthesisPending: Boolean = false,
    /** `full` or `partial`. */
    val completionKind: String? = null,
    val executionId: String? = null,
    /** Epoch milliseconds, or epoch seconds (below 1e11). */
    val createdAt: Double? = null,
    val outputCount: Int = 0,
    val runs: List<LiveTaskRunInput> = emptyList(),
) {
    companion object {
        /**
         * From the chat's [TaskStatusCard]. The chat falls back to the raw
         * task id as its title; the card's title rule never shows one.
         */
        fun from(
            task: TaskStatusCard,
            messageId: String,
            storeStatus: String? = null,
            createdAt: Double? = null,
            completionKind: String? = null,
            runs: List<LiveTaskRunInput> = emptyList(),
        ): LiveTaskCardInput = LiveTaskCardInput(
            messageId = messageId,
            taskId = task.taskId.takeIf { it != "unknown-task" },
            displayLabel = task.title,
            status = task.status,
            storeStatus = storeStatus,
            summary = task.summary,
            synthesisPending = task.synthesisPending,
            completionKind = completionKind,
            executionId = task.executionId,
            createdAt = createdAt,
            outputCount = task.outputs.size,
            runs = runs,
        )
    }
}

data class LiveTaskCardState(
    val id: String,
    /** The real task id (absent for malformed status messages). */
    val taskId: String?,
    /** Latest run's execution id, for Inspect. */
    val executionId: String?,
    val title: String,
    val status: LiveTaskStatus,
    /** The human status word; "Partially completed" for a partial success. */
    val verb: String,
    val terminal: Boolean,
    /** Plan approved and not started: the card can offer Run. */
    val startable: Boolean,
    /** One step per run, in order. */
    val steps: List<LiveTaskStep>,
    /** Non-terminal only, and only when the reason does not already say it. */
    val currentStep: String?,
    val startedAtMillis: Long?,
    val finishedAtMillis: Long?,
    /**
     * Why the task is where it is (markdown): the trimmed `summary`. Dropped
     * when the store's status replaced the message's.
     */
    val reason: String?,
    /** Per-run output counts, runs without outputs omitted. */
    val outputs: List<LiveTaskOutput>,
) {
    /** A task that has not started has no run to time, stop or stream. */
    val notStarted: Boolean get() = status == LiveTaskStatus.Queued

    /** The card's headline word. The same as [verb]; named for the view. */
    val headline: String get() = verb

    /** The quiet tag first in the meta row of an unstarted card. */
    val tagText: String? get() = if (notStarted) "Not started" else null

    /** "Needs you" beside the status of a task waiting on the reader. */
    val needsYou: Boolean get() = status == LiveTaskStatus.Waiting

    /** Elapsed time is shown (ticking or, once terminal, frozen). */
    val showsTimer: Boolean get() = startedAtMillis != null && !notStarted

    /** The elapsed time ticks: live, started, not unstarted. */
    val ticks: Boolean get() = showsTimer && !terminal

    /** Cancel is offered (the view also needs a handler). */
    val canCancel: Boolean get() = !terminal && !notStarted

    /** The live activity streams only while the task runs and has a real id. */
    val showsLiveActivity: Boolean get() = !terminal && !notStarted && taskId != null

    /** Not started or waiting for review: the host fetches the plan once when the card has no steps yet. */
    val wantsPlan: Boolean
        get() = !terminal && (notStarted || status == LiveTaskStatus.Waiting) && taskId != null

    fun showsPlanPreview(planStepCount: Int): Boolean = wantsPlan && planStepCount > 0

    /** `m:ss` / `h:mm:ss` from start to the end (terminal) or now; null when the card shows no time. */
    fun elapsedText(nowMillis: Long): String? {
        if (!showsTimer) return null
        val started = startedAtMillis ?: return null
        val end = finishedAtMillis ?: nowMillis
        return formatElapsed((end.toDouble() - started.toDouble()) / 1_000.0)
    }

    /**
     * The run list only when it says something: while running (the fallback
     * when there is no live activity), or a finished task whose runs ended
     * differently. Never a lone "Run · done".
     */
    fun showsRuns(liveActivityAvailable: Boolean): Boolean {
        if (liveActivityAvailable || steps.isEmpty()) return false
        if (steps.size == 1 && steps[0].status == LiveTaskStepStatus.Done) return false
        val distinct = steps.map { it.status }.toSet().size
        return !terminal || (steps.size >= 2 && distinct >= 2)
    }

    /** A finished task's outputs sit in the card body; a running task's stay in the details. */
    val outputsInBody: Boolean get() = terminal && outputs.isNotEmpty()

    val totalOutputCount: Int get() = outputs.sumOf { it.count }

    fun hiddenOutputCount(expanded: Boolean): Int =
        if (expanded) 0 else maxOf(0, totalOutputCount - OUTPUT_PREVIEW_LIMIT)

    /**
     * The card's rule over its own state. [activityHasContent]: the live
     * activity rendered something. [liveActivityAvailable] defaults to
     * [showsLiveActivity]. Once open it stays a toggle so it can close.
     */
    fun isExpandable(
        activityHasContent: Boolean,
        planStepCount: Int,
        reasonOverflows: Boolean,
        expanded: Boolean = false,
        liveActivityAvailable: Boolean? = null,
    ): Boolean {
        val activity = liveActivityAvailable ?: showsLiveActivity
        return expanded || isExpandable(
            hasSteps = activity && activityHasContent,
            hasRuns = showsRuns(activity),
            hasOutputs = !outputsInBody && outputs.isNotEmpty(),
            hasClampedReason = reasonOverflows,
            hasHiddenOutputs = hiddenOutputCount(expanded) > 0,
            hasPlanPreview = showsPlanPreview(planStepCount),
        )
    }

    companion object {
        /** Outputs a collapsed card shows before "+N more". */
        const val OUTPUT_PREVIEW_LIMIT = 2

        fun of(input: LiveTaskCardInput): LiveTaskCardState {
            val synthesisPending = input.synthesisPending
            val contentStatus = effectiveStatus(input.status, input.storeStatus)
            val partial = !synthesisPending && isPartialCompletion(contentStatus, input.completionKind)
            // A synthesizing task stays live even if its status already reads terminal.
            val terminal = !synthesisPending && (partial || isTerminal(contentStatus))
            val status = when {
                synthesisPending -> LiveTaskStatus.Running
                partial -> LiveTaskStatus.Partial
                else -> liveStatus(contentStatus)
            }
            val visualVerb = statusVerb(contentStatus, synthesisPending)
            val verb = if (partial) "Partially completed" else visualVerb
            val reason = if (contentStatus == input.status) text(input.summary) else null

            val total = input.runs.size
            val steps = input.runs.mapIndexed { index, run ->
                LiveTaskStep(run.id, runLabel(run, index, total), stepStatus(run.status))
            }

            // No progress fraction: runs are not a plan.
            var currentStep: String? = null
            if (!terminal) {
                val running = steps.lastOrNull { it.status == LiveTaskStepStatus.Running }
                currentStep = when {
                    synthesisPending -> visualVerb
                    // The reason already says what is happening; with several runs, still name the live one.
                    reason != null -> if (total > 1) running?.label else null
                    else -> running?.label ?: visualVerb
                }
            }

            val times = (listOf(input.createdAt) + input.runs.flatMap { it.timestamps })
                .mapNotNull { epochMillis(it) }

            val outputs = input.runs.mapIndexedNotNull { index, run ->
                if (run.outputCount <= 0) return@mapIndexedNotNull null
                LiveTaskOutput(
                    key = run.id,
                    label = if (total > 1) runLabel(run, index, total) else null,
                    sessionId = run.sessionId,
                    count = run.outputCount,
                )
            }
            val taskId = text(input.taskId)

            return LiveTaskCardState(
                id = taskId ?: input.messageId,
                taskId = taskId,
                executionId = text(input.runs.lastOrNull()?.executionId) ?: text(input.executionId),
                title = TaskTitleRules.labelOrShortIdTitle(input.displayLabel, taskId),
                status = status,
                verb = verb,
                terminal = terminal,
                startable = !synthesisPending && contentStatus == "ready",
                steps = steps,
                currentStep = currentStep,
                startedAtMillis = times.minOrNull(),
                finishedAtMillis = if (terminal) times.maxOrNull() else null,
                reason = reason,
                outputs = outputs,
            )
        }

        /** Expandable only when expanding reveals something. Cancel is not part of it. */
        fun isExpandable(
            hasSteps: Boolean,
            hasRuns: Boolean,
            hasOutputs: Boolean,
            hasClampedReason: Boolean,
            hasHiddenOutputs: Boolean,
            hasPlanPreview: Boolean,
        ): Boolean = hasSteps || hasRuns || hasOutputs || hasClampedReason || hasHiddenOutputs || hasPlanPreview

        /** `m:ss`, or `h:mm:ss` past an hour. */
        fun formatElapsed(seconds: Double): String {
            val total = if (seconds.isFinite()) maxOf(0, floor(seconds).toInt()) else 0
            val h = total / 3600
            val m = (total % 3600) / 60
            val s = total % 60
            val ss = if (s < 10) "0$s" else "$s"
            if (h > 0) return "$h:${if (m < 10) "0$m" else "$m"}:$ss"
            return "$m:$ss"
        }

        // Status rules (liveTask.ts / taskStatus.ts)

        /**
         * `archived` is final too: a card whose last status update never
         * arrived settles on the store's `archived` instead of reading
         * "Running" forever.
         */
        fun isTerminal(status: String?): Boolean =
            status == "completed" || status == "failed" || status == "cancelled" || status == "archived"

        /** `draft`: a plan waiting for the reader's review. */
        fun isWaiting(status: String?): Boolean {
            status ?: return false
            return status == "paused" || status == "draft" ||
                status.startsWith("waiting") || status.startsWith("awaiting")
        }

        fun isNotStarted(status: String?): Boolean =
            status == "ready" || status == "pending" || status == "rejected"

        fun isPartialCompletion(status: String?, completionKind: String?): Boolean =
            status == "partial_success" || (status == "completed" && completionKind == "partial")

        /**
         * The card's latest message, unless the task store knows the task is
         * not started while the message still says planning or draft.
         */
        fun effectiveStatus(message: String?, store: String?): String? {
            val behind = message == null || message == "planning" || message == "draft"
            return if (behind && isNotStarted(store)) store else message
        }

        fun liveStatus(status: String?): LiveTaskStatus = when {
            status == "completed" -> LiveTaskStatus.Completed
            status == "failed" -> LiveTaskStatus.Failed
            status == "cancelled" -> LiveTaskStatus.Cancelled
            // Settled but not a failure: muted, like a cancelled card.
            status == "archived" -> LiveTaskStatus.Cancelled
            isWaiting(status) -> LiveTaskStatus.Waiting
            isNotStarted(status) -> LiveTaskStatus.Queued
            else -> LiveTaskStatus.Running
        }

        fun stepStatus(status: String?): LiveTaskStepStatus = when {
            status == "completed" || status == "partial_success" -> LiveTaskStepStatus.Done
            status == "failed" || status == "cancelled" -> LiveTaskStepStatus.Failed
            isWaiting(status) -> LiveTaskStepStatus.Waiting
            else -> LiveTaskStepStatus.Running
        }

        /** The status word (web `taskStatusVisual(...).verb`). */
        fun statusVerb(status: String?, synthesisPending: Boolean): String {
            if (synthesisPending) return "Preparing final result"
            return when (status) {
                "created" -> "Created"
                "planning" -> "Planning"
                "ready" -> "Plan approved, not started"
                "pending" -> "Not started"
                "draft" -> "Plan ready for review"
                "rejected" -> "Plan rejected"
                "running", "in_progress" -> "Running"
                "paused" -> "Paused"
                "completed" -> "Completed"
                "failed" -> "Failed"
                "cancelled" -> "Cancelled"
                "archived" -> "Archived"
                else -> status ?: "Updated"
            }
        }

        /** Web `taskExecutionLabel`. */
        fun runLabel(run: LiveTaskRunInput, index: Int, total: Int): String {
            if (text(run.executionId) == null) return if (total > 1) "Primary run" else "Run"
            return if (total > 1) "Run ${index + 1}" else "Run"
        }

        /** Epoch milliseconds; values below 1e11 are seconds, larger ones are ms. */
        fun epochMillis(value: Double?): Long? {
            if (value == null || !value.isFinite() || value <= 0) return null
            return if (value < 1e11) (value * 1_000).toLong() else value.toLong()
        }

        private fun text(raw: String?): String? = raw?.trim()?.takeIf { it.isNotEmpty() }
    }
}

/**
 * The drawer's title rule, as the live card uses it. Port of iOS
 * `TaskTitleRules` (web `labelOrShortIdTitle` in `taskPanelTitle.ts`).
 */
object TaskTitleRules {
    const val TITLE_MAX_CHARS = 90

    /** `<agent-slug> / ` at the very start of a prompt: the routing prefix. */
    private val routingPrefix = Regex("""^[a-z0-9]+(?:[-_][a-z0-9]+)*\s+/\s+""")

    /** A leading meta sentence about how to run, not what to do. */
    private val metaSentence = Regex("""^(?:do\s+not|don['’]t|never)\b[^.!?\n]*[.!?]?\s*""", RegexOption.IGNORE_CASE)

    /** "Use (the) browser … to <goal>" keeps the goal; a bare "Use the browser." goes. */
    private val browserLead = Regex("""^use\s+(?:the\s+)?browser\b[^.!?\n]*?\bto\s+""", RegexOption.IGNORE_CASE)
    private val browserSentence = Regex("""^use\s+(?:the\s+)?browser\b(?:(?!\bto\s)[^.!?\n])*[.!?]\s*""", RegexOption.IGNORE_CASE)

    /** Words a surface prefixes onto an id to make a title out of it. */
    private val idPrefix = Regex("""^(?:run|task|execution|job)\s+""", RegexOption.IGNORE_CASE)
    private val shortIdPrefix = Regex("""^[a-z]+[_-](?=[a-z0-9])""", RegexOption.IGNORE_CASE)
    private val sentenceBreak = Regex("""(?<=[.!?])\s+""")
    private val trailingPunctuation = Regex("""[.!?…]+$""")
    private val whitespace = Regex("""\s+""")

    private fun trimmed(text: String?): String = text?.trim().orEmpty()

    /** Whether [text] is an identifier rather than prose. */
    fun looksLikeId(text: String?, id: String?): Boolean {
        val value = trimmed(text)
        if (value.isEmpty()) return false
        val ownId = trimmed(id)
        if (ownId.isNotEmpty() && value.contains(ownId)) return true
        val token = idPrefix.replace(value, "")
        if (token.any { it.isWhitespace() }) return false
        return token.length >= 8 && token.any { it.isDigit() }
    }

    fun isRoutedPrompt(text: String?): Boolean = routingPrefix.containsMatchIn(trimmed(text))

    private fun stripDirectives(text: String): String {
        var rest = routingPrefix.replace(trimmed(text), "")
        while (true) {
            val next = browserSentence.replace(metaSentence.replace(rest, ""), "")
            if (next == rest) break
            rest = next
        }
        return trimmed(rest)
    }

    private fun capAtWord(text: String, max: Int): String {
        if (text.length <= max) return text
        val cut = text.take(max)
        val space = cut.lastIndexOf(' ')
        if (space > 0) return cut.substring(0, space).trim() + "…"
        return cut.trim() + "…"
    }

    /** A human title from a prompt, or null when no prose is left. */
    fun titleFromPrompt(prompt: String?): String? {
        if (prompt.isNullOrEmpty()) return null
        var rest = stripDirectives(prompt)
        if (browserLead.containsMatchIn(rest)) {
            val goal = browserLead.replace(rest, "")
            rest = goal.take(1).uppercase() + goal.drop(1)
        }
        val firstLine = rest.split("\n").firstOrNull { trimmed(it).isNotEmpty() } ?: ""
        val firstSentence = sentenceBreak.replace(trimmed(firstLine), "\u0000").split("\u0000").firstOrNull() ?: ""
        val sentence = trimmed(whitespace.replace(trailingPunctuation.replace(firstSentence, ""), " "))
        if (sentence.isEmpty() || looksLikeId(sentence, null)) return null
        return capAtWord(sentence, TITLE_MAX_CHARS)
    }

    /** The drawer heading from a label and id alone. */
    fun deriveTitle(label: String?, id: String?): String? {
        val shown = trimmed(label)
        if (shown.isNotEmpty() && !looksLikeId(shown, id)) {
            if (!isRoutedPrompt(shown)) return shown
            titleFromPrompt(shown)?.let { return it }
        }
        return null
    }

    /** `task_2679cefd6f…` → `2679ce`. */
    fun shortTaskId(id: String?): String? {
        val value = trimmed(id)
        if (value.isEmpty()) return null
        return shortIdPrefix.replace(value, "").take(6)
    }

    /** The label through the title rules, else `Task · <short id>`, else `Task`. Never the raw id. */
    fun labelOrShortIdTitle(label: String?, id: String?): String {
        deriveTitle(label, id)?.takeIf { trimmed(it).isNotEmpty() }?.let { return it }
        shortTaskId(id)?.let { return "Task · $it" }
        return "Task"
    }

    /**
     * The short id when [title] is only the `Task · <short id>` stand-in
     * (iOS `LiveTaskCardView.idStandInShortId`), else null.
     */
    fun idStandInShortId(title: String?, id: String?): String? {
        val short = shortTaskId(id)?.takeIf { it.isNotEmpty() } ?: return null
        return short.takeIf { trimmed(title) == "Task · $short" }
    }

    /**
     * The heading the live card prints beside its short-id chip: `Task` when
     * the id only stands in for a title, so the id is never shown twice.
     */
    fun cardHeading(title: String, id: String?): String =
        if (idStandInShortId(title, id) != null) "Task" else title
}
