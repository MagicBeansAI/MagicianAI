package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonObject

/*
 * The task detail screen's pure rules (design §3 of
 * `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`).
 * Android port of iOS `TaskDetailLayout` / `TaskDetailMenu`
 * (`TaskDetailScreenModel.swift`). The Compose screen reads only these plus
 * the shared models ([TaskActs], [ActSummaries], [PlanSteps], [PlanProgress],
 * [TaskOutputGrouping], [TaskVerdict], [TaskUsageModel]). No Compose here.
 */

/** A lifecycle action in the detail screen's ⋯ menu. */
enum class TaskDetailAction(val title: String) {
    Run("Run now"),
    Plan("Plan"),
    ApprovePlan("Approve plan"),
    RejectPlan("Reject plan"),
    Replan("Replan"),
    RetrySynthesis("Retry output synthesis"),
    Reset("Reset to Ready"),
    MarkComplete("Mark complete"),
    MarkNotDone("Mark not done"),
    CancelTask("Cancel task"),
    Delete("Delete task"),
}

/** What the menu reads off the task. */
data class TaskDetailMenuInput(
    val status: String,
    val planStatus: String?,
    val hasPlan: Boolean,
    /** An active root run (live): no Run / Plan / Reset while it lives. */
    val live: Boolean,
    val synthesisFailed: Boolean,
    val synthesisPending: Boolean,
)

object TaskDetailRules {
    const val POLL_INTERVAL_MILLIS = 4_000L

    /** Planning, running or executing. */
    fun isActive(status: String?): Boolean =
        status?.trim()?.lowercase() in setOf("planning", "running", "executing", "in_progress")

    /**
     * Poll `/execution-panel` only while the screen is resumed (visible, app
     * in the foreground) and the task is planning or running (design §3
     * "Live updates"); backgrounding stops it.
     */
    fun shouldPoll(resumed: Boolean, status: String?, planStatus: String?): Boolean =
        resumed && (isActive(status) || planStatus?.trim()?.lowercase() == "planning")

    /**
     * The act open when the screen appears: an entry point's focus when that
     * act exists, else the state's default ([TaskActs.defaultOpenAct]).
     */
    fun initialOpenAct(
        acts: List<TaskDetailAct>,
        state: TaskVerdictState,
        attention: TaskAttentionSource?,
        focus: TaskDetailAct?,
    ): TaskDetailAct? {
        if (focus != null && focus in acts) return focus
        return TaskActs.defaultOpenAct(state, acts, attention)
    }

    /** Tapping an open card closes it; tapping a closed one opens it (one open at a time). */
    fun toggle(open: TaskDetailAct?, tapped: TaskDetailAct): TaskDetailAct? = if (open == tapped) null else tapped

    /** The Plan act keeps its summary while open (its body lists steps, never the plan's state). */
    fun keepsSummaryWhenOpen(act: TaskDetailAct): Boolean = act == TaskDetailAct.Plan

    /** The first paragraph of the description, Markdown markers stripped; null when empty. */
    fun brief(description: String?): String? {
        val paragraph = description.orEmpty().split("\n\n").firstOrNull().orEmpty()
            .replace("#", "").replace("**", "").replace("\n", " ").trim()
        return paragraph.ifEmpty { null }
    }

    /** `fixture-deta…`: the id chip's text. */
    fun shortId(id: String): String = if (id.length > 14) id.take(12) + "…" else id

    /** `launch-brief-fo…ile-app-final.md`: a long name keeps its start and its extension end. */
    fun middleEllipsize(name: String, maxChars: Int = 34): String {
        if (name.length <= maxChars || maxChars < 5) return name
        val keep = maxChars - 1
        val head = (keep + 1) / 2
        val tail = keep - head
        return name.take(head) + "…" + name.takeLast(tail)
    }

    /** A deliverable reads inline: an image, or text / markdown / JSON / CSV / log / YAML. */
    fun previewable(mediaType: String?, path: String): Boolean {
        if (ActSummaries.outputKind(mediaType, path) == OutputKind.Image) return true
        val ext = path.substringAfterLast('/').substringAfterLast('.', "").lowercase()
        val mime = mediaType?.trim()?.lowercase().orEmpty()
        return ext in setOf("md", "markdown", "txt", "json", "csv", "log", "yaml", "yml") ||
            mime.startsWith("text/") || mime == "application/json"
    }

    fun isImage(mediaType: String?, path: String): Boolean =
        ActSummaries.outputKind(mediaType, path) == OutputKind.Image

    /** The deliverable previewed in place: the first previewable one. */
    fun <T> defaultPreview(files: List<T>, mediaType: (T) -> String?, path: (T) -> String): T? =
        files.firstOrNull { previewable(mediaType(it), path(it)) }

    /** `Markdown`, `Image`, `PDF`, `JSON`, `Text`, `File`: line 1's kind word. */
    fun kindLabel(mediaType: String?, path: String): String {
        val ext = path.substringAfterLast('/').substringAfterLast('.', "").lowercase()
        val mime = mediaType?.trim()?.lowercase().orEmpty()
        return when {
            ext in setOf("md", "markdown") || mime == "text/markdown" -> "Markdown"
            mime.startsWith("image/") || ActSummaries.outputKind(mediaType, path) == OutputKind.Image -> "Image"
            ext == "pdf" || mime == "application/pdf" -> "PDF"
            ext == "json" || mime == "application/json" -> "JSON"
            ext in setOf("html", "htm") || mime == "text/html" -> "HTML"
            ext in setOf("txt", "csv", "log", "yaml", "yml") || mime.startsWith("text/") -> "Text"
            else -> "File"
        }
    }

    /** `912 bytes`, `5 KB`, `1.2 MB`. */
    fun formatSize(bytes: Long?): String? {
        val size = bytes ?: return null
        if (size < 0) return null
        return when {
            size < 1_000 -> "$size bytes"
            size < 1_000_000 -> "${Math.round(size / 1_000.0)} KB"
            else -> String.format(java.util.Locale.US, "%.1f MB", size / 1_000_000.0)
        }
    }

    /** `Markdown · 5 KB`: an output row's trailing meta. */
    fun fileMeta(mediaType: String?, path: String, sizeBytes: Long?): String =
        listOfNotNull(kindLabel(mediaType, path), formatSize(sizeBytes)).joinToString(" · ")

    /** What Copy path puts on the clipboard: the absolute path when sent, else the task-relative one. */
    fun copyPath(file: TaskOutputFile): String = file.absolutePath ?: file.relativePath

    /**
     * `plan_graph.steps[].task` (else `metadata.description`, else
     * `Step <id>`), in graph order — web `extractPlanStepsFromPlanGraph`.
     */
    fun planGraphSteps(plan: JsonObject?): List<String> {
        val graph = (plan?.get("plan").jObj() ?: plan)?.get("plan_graph").jObj() ?: return emptyList()
        return graph["steps"].jObjects().orEmpty().map { step ->
            step["task"].jText()
                ?: step["metadata"].jObj()?.get("description").jText()
                ?: "Step ${step["id"].jStr().orEmpty()}"
        }
    }

    /** The Plan act's steps: the plan graph's, else the run's plan-step rows, else the run's steps. */
    fun planSteps(graphSteps: List<String>, stepRows: List<PlanStepRow>, runSteps: List<String>): List<String> = when {
        graphSteps.isNotEmpty() -> graphSteps
        stepRows.isNotEmpty() -> stepRows.map { it.name }
        else -> runSteps
    }

    /** A run with an end time, or a terminal status, has ended. */
    fun runHasEnded(status: String?, endedAt: String?): Boolean {
        if (!endedAt.isNullOrBlank()) return true
        return status?.trim()?.lowercase() in setOf(
            "completed", "complete", "done", "succeeded", "failed", "error",
            "cancelled", "canceled", "timed_out", "timeout",
        )
    }

    /** Planning activity is a live list while planning, then folds. */
    fun planningActivityIsLive(planStatus: String?): Boolean = planStatus?.trim()?.lowercase() == "planning"

    /** A draft plan offers Reject · Approve · Approve & Run in the Plan act. */
    fun showsDraftActions(planStatus: String?, status: String?): Boolean =
        planStatus?.trim()?.lowercase() == "draft" &&
            status?.trim()?.lowercase() !in setOf("completed", "failed", "cancelled", "canceled")

    /** The Run act line: the live plan-step summary, else the run counts; usage closes it. */
    fun runActSummary(
        stepRows: List<PlanStepRow>,
        live: Boolean,
        input: RunSummaryInput,
        usage: TaskUsage?,
    ): String {
        val base = if (stepRows.isNotEmpty() && live) PlanSteps.summary(stepRows) else ActSummaries.runSummary(input)
        return TaskUsageFormat.runSummary(base, usage)
    }

    /** `Plan · approved 15m ago`, or `Plan` when the summary says nothing. */
    fun planChipText(summary: String): String = if (summary.isEmpty()) "Plan" else "Plan · $summary"

    /**
     * Which lifecycle actions the ⋯ menu offers (iOS `TaskDetailMenu.actions`),
     * so moving to act cards loses none of the old header buttons.
     */
    fun menuActions(input: TaskDetailMenuInput): List<TaskDetailAction> {
        val status = input.status.trim().lowercase()
        val plan = input.planStatus?.trim()?.lowercase()
        val actions = mutableListOf<TaskDetailAction>()
        if (!input.live) {
            if (plan == "approved" && status == "ready") {
                actions.add(TaskDetailAction.Run)
            } else if (status in setOf("pending", "ready") && plan !in setOf("planning", "eliciting", "draft")) {
                actions.add(TaskDetailAction.Run)
            }
            if (status == "pending" && !input.hasPlan) actions.add(TaskDetailAction.Plan)
        }
        if (plan == "draft" || plan == "eliciting") {
            actions.add(TaskDetailAction.ApprovePlan)
            actions.add(TaskDetailAction.RejectPlan)
        }
        if (input.hasPlan && plan != "planning") actions.add(TaskDetailAction.Replan)
        if (input.synthesisFailed && !input.synthesisPending) actions.add(TaskDetailAction.RetrySynthesis)
        if (status in setOf("paused", "failed", "cancelled", "canceled") && !input.live) actions.add(TaskDetailAction.Reset)
        if (status == "completed") {
            actions.add(TaskDetailAction.MarkNotDone)
        } else if (status !in setOf("running", "planning", "paused", "executing", "queued")) {
            actions.add(TaskDetailAction.MarkComplete)
        }
        if (status !in setOf("completed", "cancelled", "canceled")) actions.add(TaskDetailAction.CancelTask)
        actions.add(TaskDetailAction.Delete)
        return actions
    }

    /** The compact title bar shows once the header card has scrolled away. */
    fun compactTitleVisible(firstVisibleItemIndex: Int, firstVisibleItemOffset: Int, headerHeight: Int): Boolean =
        firstVisibleItemIndex > 0 || (headerHeight > 0 && firstVisibleItemOffset >= headerHeight)
}
