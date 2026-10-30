package ai.magicbeans.magdroid.tasks

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

/*
 * The task detail screen's act rules: which acts it has, which opens, the
 * order they render in, and the one line each carries while closed. Pure.
 *
 * Android port of iOS `TaskActsModel.swift` (web `actOrder` in
 * `taskCapabilities.ts`, `actSummaries.ts`, `outputKindOf` in
 * `taskPanelModel.ts`). `deriveActs` / `defaultOpenAct` already live in
 * [TaskVerdict]; [TaskActs] re-exposes them so a view reads every act rule
 * from one place.
 */

/** Which acts the task has (iOS `ActCapabilities`). */
data class ActCapabilities(
    /** False when the task was never planned, not when the plan is empty. */
    val hasPlanAct: Boolean,
    /** False when there is no execution, not when it has yet to move. */
    val hasRunAct: Boolean,
    /** True with zero files: the act renders and summarises `no output`. */
    val hasOutputAct: Boolean,
)

object TaskActs {
    fun deriveActs(caps: ActCapabilities): List<TaskDetailAct> =
        TaskVerdict.acts(caps.hasPlanAct, caps.hasRunAct, caps.hasOutputAct)

    fun defaultOpenAct(
        state: TaskVerdictState,
        acts: List<TaskDetailAct>,
        attention: TaskAttentionSource?,
    ): TaskDetailAct? = TaskVerdict.defaultOpenAct(state, acts, attention)

    /**
     * The act that leads the column, or null to keep lifecycle order. Result
     * first: a finished task's reader came for its output. A live or
     * stopped-short run leads with Run. Waiting and queued keep lifecycle
     * order.
     */
    fun lead(state: TaskVerdictState): TaskDetailAct? = when (state) {
        TaskVerdictState.Waiting, TaskVerdictState.Queued -> null
        TaskVerdictState.Failed, TaskVerdictState.Stalled, TaskVerdictState.Running,
        TaskVerdictState.Paused, TaskVerdictState.Cancelled -> TaskDetailAct.Run
        TaskVerdictState.Archived, TaskVerdictState.Finalizing, TaskVerdictState.Finished,
        TaskVerdictState.PartlyDone -> TaskDetailAct.Output
    }

    /** [acts] (lifecycle order) with the state's leading act moved to the front. A permutation of its input. */
    fun actOrder(state: TaskVerdictState, acts: List<TaskDetailAct>): List<TaskDetailAct> {
        val lead = lead(state)
        if (lead == null || lead !in acts) return acts
        return listOf(lead) + acts.filter { it != lead }
    }
}

/** The plan's status (web `TaskPlanStatus`). */
enum class TaskPlanStatus(val wire: String, val copy: String) {
    Planning("planning", "being planned"),
    Eliciting("eliciting", "gathering what it needs before planning"),
    Draft("draft", "draft, not yet approved"),
    Approved("approved", "approved"),
    Rejected("rejected", "rejected, so it will not run"),
    Failed("failed", "planning failed");

    companion object {
        /** Tolerant: an unknown status is null (the summary then says nothing). */
        fun fromWire(value: String?): TaskPlanStatus? {
            val raw = value?.trim()?.lowercase() ?: return null
            return entries.firstOrNull { it.wire == raw }
        }
    }
}

data class PlanSummaryInput(
    /** Null when the task has a plan act but no status the client recognises. */
    val status: TaskPlanStatus?,
    val approvedAtMillis: Long?,
    /** The asks blocking the plan, in the order the act body lists them. */
    val questions: List<String>,
    val nowMillis: Long,
)

data class RunSummaryInput(
    /** Steps recorded so far. */
    val steps: Int,
    val retries: Int,
    /** Seconds; null, negative or non-finite render no duration. */
    val elapsedSeconds: Double?,
    /** 1-based; non-null while the run is live, swapping the count for the position. */
    val currentStep: Int?,
    val totalSteps: Int?,
    /** Timeline entries recorded. */
    val events: Int,
)

/** What kind of file, for the output summary's plural. */
enum class OutputKind(val plural: String) {
    Document("documents"),
    Image("images"),
    Other("other files"),
}

data class ActOutputFile(val name: String, val kind: OutputKind)

object ActSummaries {
    /** A waiting question outranks the approval; an approved plan says when; an unknown status says nothing. */
    fun planSummary(input: PlanSummaryInput): String {
        if (input.questions.isNotEmpty()) {
            val count = input.questions.size
            val waiting = "$count ${if (count == 1) "question" else "questions"} waiting"
            val asked = input.questions[0].trim()
            return if (asked.isEmpty()) waiting else "$waiting — \"$asked\""
        }
        if (input.status == TaskPlanStatus.Approved) {
            approvalTime(input.approvedAtMillis, input.nowMillis)?.let { return "approved $it" }
        }
        return input.status?.copy ?: ""
    }

    /**
     * Live, the position replaces the step count; retries, events and the
     * duration still apply. Empty segments drop out; nothing at all is
     * `not started`.
     */
    fun runSummary(input: RunSummaryInput): String {
        val progress = if (input.currentStep == null) {
            count(input.steps, "step", "steps")
        } else {
            TaskVerdict.stepPhrase(input.currentStep, input.totalSteps)
        }
        val line = listOfNotNull(
            progress,
            count(input.retries, "retry", "retries"),
            count(input.events, "event", "events"),
            TaskVerdict.durationIfKnown(input.elapsedSeconds),
        ).filter { it.isNotEmpty() }.joinToString(" · ")
        return line.ifEmpty { "not started" }
    }

    /** `report.md`, `report.md and notes.md`, `report.md and 2 images`. */
    fun outputSummary(files: List<ActOutputFile>): String {
        val first = files.firstOrNull() ?: return "no output"
        if (files.size == 1) return first.name
        if (files.size == 2) return "${first.name} and ${files[1].name}"
        val rest = files.drop(1)
        val kinds = rest.map { it.kind }.toSet()
        val plural = if (kinds.size == 1) rest.first().kind.plural else OutputKind.Other.plural
        return "${first.name} and ${rest.size} $plural"
    }

    /** The Output act's line over task deliverables, by basename and kind. */
    fun outputSummaryOfFiles(files: List<TaskOutputFile>): String =
        outputSummary(files.map { ActOutputFile(it.displayName, outputKind(it.mediaType, it.relativePath)) })

    private val documentMediaTypes = setOf(
        "application/pdf", "application/msword", "application/rtf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    )
    private val documentExtensions = setOf("md", "markdown", "txt", "pdf", "html", "htm", "doc", "docx", "rtf")
    private val imageExtensions = setOf("png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif")

    /** The mime is authoritative when present; the extension is the fallback. */
    fun outputKind(mediaType: String?, path: String): OutputKind {
        val mime = mediaType?.trim()?.lowercase().orEmpty()
        if (mime.isNotEmpty()) {
            if (mime.startsWith("image/")) return OutputKind.Image
            if (mime.startsWith("text/") || mime in documentMediaTypes) return OutputKind.Document
            return OutputKind.Other
        }
        val base = path.substringAfterLast('/')
        val dot = base.lastIndexOf('.')
        if (dot <= 0) return OutputKind.Other
        val ext = base.substring(dot + 1).lowercase()
        if (ext in imageExtensions) return OutputKind.Image
        if (ext in documentExtensions) return OutputKind.Document
        return OutputKind.Other
    }

    /**
     * Web `formatRelativeTime`: `now` under a minute (a future instant clamps
     * to it), then `Nm`, `Nh`, `Nd`, and a short date after a week.
     */
    fun compactRelativeTime(atMillis: Long?, nowMillis: Long, zone: ZoneId = ZoneId.systemDefault()): String {
        val at = atMillis ?: return ""
        val delta = maxOf(0L, nowMillis - at) / 1_000.0
        if (delta < 60) return "now"
        if (delta < 3_600) return "${(delta / 60).toInt()}m"
        if (delta < 86_400) return "${(delta / 3_600).toInt()}h"
        if (delta < 7 * 86_400) return "${(delta / 86_400).toInt()}d"
        return DateTimeFormatter.ofPattern("MMM d", Locale.getDefault()).format(Instant.ofEpochMilli(at).atZone(zone))
    }

    /** When the plan was approved, phrased to follow the word `approved`. */
    private fun approvalTime(approvedAtMillis: Long?, nowMillis: Long): String? {
        val relative = compactRelativeTime(approvedAtMillis, nowMillis)
        if (relative.isEmpty()) return null
        if (relative == "now") return "just now"
        return if (Regex("^\\d+[mhd]$").matches(relative)) "$relative ago" else "on $relative"
    }

    /** Null rather than `0 steps`, so a segment with nothing to report drops out. */
    private fun count(n: Int, singular: String, plural: String): String? =
        if (n <= 0) null else "$n ${if (n == 1) singular else plural}"
}
