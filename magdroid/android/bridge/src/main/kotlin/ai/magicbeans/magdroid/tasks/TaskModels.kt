package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId

@Serializable
data class TaskTag(
    val id: String = "",
    val name: String = "",
    val color: String? = null,
)

@Serializable
data class TaskPendingQuestion(
    val question: String? = null,
    val text: String? = null,
    val prompt: String? = null,
) {
    fun display(): String? = question ?: text ?: prompt
}

/** The tolerant flat row returned by both V3 task lanes. */
@Serializable
data class TaskV3(
    val id: String = "",
    val title: String = "",
    val description: String = "",
    val status: String = "pending",
    @SerialName("agent_id") val agentId: String = "",
    @SerialName("ui_thread_id") val uiThreadId: String = "general",
    val priority: String? = null,
    @SerialName("due_date") val dueDate: String? = null,
    val tags: List<TaskTag> = emptyList(),
    @SerialName("depends_on") val dependsOn: List<String> = emptyList(),
    val schedule: JsonElement? = null,
    @SerialName("is_blocked") val isBlocked: Boolean = false,
    @SerialName("current_step_title") val currentStepTitle: String? = null,
    @SerialName("current_substep_title") val currentSubstepTitle: String? = null,
    @SerialName("completion_summary") val completionSummary: String? = null,
    @SerialName("completion_outcome") val completionOutcome: String? = null,
    @SerialName("completion_artifact_names") val completionArtifactNames: List<String>? = null,
    /** App recurring behaviour schedule; only some internal payloads carry it. */
    @SerialName("recurring_schedule") val recurringSchedule: JsonElement? = null,
    @SerialName("has_plan") val hasPlan: Boolean = false,
    @SerialName("plan_status") val planStatus: String? = null,
    @SerialName("latest_plan_id") val latestPlanId: String? = null,
    @SerialName("pending_question") val pendingQuestion: TaskPendingQuestion? = null,
    @SerialName("chat_session_id") val chatSessionId: String? = null,
    @SerialName("synthesis_pending") val synthesisPending: Boolean = false,
    @SerialName("synthesis_failed_execution_id") val synthesisFailedExecutionId: String? = null,
    val lifecycle: String? = null,
    @SerialName("monitor_revision") val monitorRevision: Int = 0,
    @SerialName("created_by") val createdBy: String? = null,
    @SerialName("active_root_execution_id") val activeRootExecutionId: String? = null,
    @SerialName("latest_root_execution_id") val latestRootExecutionId: String? = null,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
) {
    private val normalizedStatus: String get() = status.lowercase()

    val statusLabel: String
        get() = when (normalizedStatus) {
            "pending" -> "Pending"
            "planning" -> "Planning"
            "ready" -> "Ready"
            "running", "executing" -> "Running"
            "paused" -> "Paused"
            "completed" -> "Completed"
            "failed" -> "Failed"
            "cancelled", "canceled" -> "Cancelled"
            "deferred" -> "Deferred"
            else -> status.replace('_', ' ').replaceFirstChar { it.uppercase() }
        }

    val activityLine: String?
        get() = when {
            normalizedStatus in setOf("running", "planning", "executing") ->
                currentSubstepTitle ?: currentStepTitle ?: "Working…"
            normalizedStatus == "completed" && !completionSummary.isNullOrBlank() -> completionSummary
            description.isBlank() -> null
            else -> description
        }

    val priorityLabel: String?
        get() = priority?.lowercase()?.takeIf { it in setOf("p1", "p2", "p3", "p4") }?.uppercase()

    val needsAnswer: Boolean get() = pendingQuestion != null
    val planAwaitsReview: Boolean
        get() = hasPlan && latestPlanId != null && planStatus in setOf(null, "draft", "eliciting")

    val activeExecutionIdForControls: String?
        get() = activeRootExecutionId?.trim()?.takeIf {
            it.isNotEmpty() && normalizedStatus in setOf("queued", "running", "planning", "paused", "executing")
        }

    val canMarkCompleteManually: Boolean
        get() = normalizedStatus !in setOf("queued", "running", "planning", "paused", "executing")

    val canPublishToNotes: Boolean
        get() = normalizedStatus in setOf("completed", "failed", "cancelled", "canceled")

    val isInternal: Boolean
        get() = lifecycle in setOf("internal", "ephemeral_owned_by_chat", "internal_debug")

    val lifecycleLabel: String
        get() = when {
            !isInternal -> "persistent"
            createdBy == "__system__" -> "debug"
            !chatSessionId.isNullOrBlank() -> "chat"
            else -> "internal"
        }

    val synthesisFailed: Boolean get() = !synthesisFailedExecutionId.isNullOrBlank()
    val canConvertToMonitor: Boolean get() = !isInternal && monitorRevision == 0

    /** Web `hasFinalResult`: the card offers "Result" and opens the Output act on it. */
    val hasFinalResult: Boolean
        get() = TaskPresentation.hasFinalResult(status, completionSummary, completionOutcome, completionArtifactNames)

    /** A task that has run at least once — the only kind a Reset has anything to reset. */
    val hasExecution: Boolean
        get() = !latestRootExecutionId.isNullOrBlank() || !activeRootExecutionId.isNullOrBlank()

    /** Paused always; failed/cancelled only once an execution exists (web `taskActions`). */
    val canResetToReady: Boolean
        get() = normalizedStatus == "paused" ||
            (normalizedStatus in setOf("failed", "cancelled", "canceled") && hasExecution)

    /**
     * Card actions, primary first. Mirrors web `taskActions`; a Result that is
     * not already the primary is appended once as a secondary action.
     */
    val visibleActions: List<TaskCardAction>
        get() {
            val base = baseCardActions()
            return if (hasFinalResult && TaskCardAction.Result !in base) base + TaskCardAction.Result else base
        }

    val primaryCardAction: TaskCardAction? get() = visibleActions.firstOrNull()

    private fun baseCardActions(): List<TaskCardAction> {
        if (planStatus == "planning") return listOf(TaskCardAction.ViewPlan)
        if (planStatus == "eliciting") {
            return listOf(if (needsAnswer) TaskCardAction.AnswerQuestion else TaskCardAction.ViewPlan)
        }
        if (planStatus == "draft") return listOf(TaskCardAction.ReviewPlan)
        if (planStatus == "approved" && normalizedStatus == "ready") return listOf(TaskCardAction.RunPlan)
        return when (normalizedStatus) {
            "pending" -> listOf(TaskCardAction.Preplan, TaskCardAction.RunNow)
            "ready" -> listOf(TaskCardAction.RunNow)
            "paused" -> listOf(
                if (needsAnswer) TaskCardAction.ViewQuestion else TaskCardAction.ViewExecution,
                TaskCardAction.Reset,
            )
            "failed", "cancelled", "canceled" -> buildList {
                if (hasExecution) add(TaskCardAction.Reset)
                else if (planStatus != null) add(TaskCardAction.ReviewPlan)
                add(TaskCardAction.PublishToNotes)
            }
            "completed" -> buildList {
                if (hasFinalResult) add(TaskCardAction.Result)
                add(TaskCardAction.PublishToNotes)
            }
            else -> emptyList()
        }
    }

    val leadingSwipeActions: List<TaskSwipeAction>
        get() = when (normalizedStatus) {
            "completed" -> listOf(TaskSwipeAction.MarkNotDone)
            "failed", "cancelled", "canceled", "paused" -> emptyList()
            else -> if (canMarkCompleteManually) listOf(TaskSwipeAction.MarkComplete) else emptyList()
        }

    val trailingSwipeActions: List<TaskSwipeAction>
        get() = if (normalizedStatus in setOf("completed", "failed", "cancelled", "canceled")) {
            listOf(TaskSwipeAction.Delete)
        } else {
            listOf(TaskSwipeAction.Cancel, TaskSwipeAction.Delete)
        }

    val scheduleCron: String?
        get() = scheduleObject()?.get("kind")?.asObject()
            ?.get("Cron")?.asObject()?.get("expression")?.asString()

    val scheduleTimezone: String?
        get() = scheduleObject()?.get("timezone")?.asString()
            ?: scheduleObject()?.get("kind")?.asObject()?.get("Cron")?.asObject()?.get("timezone")?.asString()

    val scheduleRetentionMaxRecords: Int?
        get() = scheduleObject()?.get("execution_history_retention")?.asObject()
            ?.get("max_records")?.jsonPrimitive?.content?.toIntOrNull()

    val scheduleRetentionMaxDays: Int?
        get() = scheduleObject()?.get("execution_history_retention")?.asObject()
            ?.get("max_age_days")?.jsonPrimitive?.content?.toIntOrNull()

    val isRecurring: Boolean
        get() = TaskPresentation.isRecurring(scheduleCron, tags, recurringSchedule is JsonObject)

    /** Human schedule for the ↻ chip's secondary text; null when only a tag says "recurring". */
    val recurringDescription: String?
        get() = scheduleCron?.let(TaskPresentation::describeCron)?.takeIf(String::isNotBlank)
            ?: (recurringSchedule as? JsonObject)?.get("interval_seconds")?.asString()?.toLongOrNull()
                ?.let(TaskPresentation::describeInterval)

    /** Tags minus the plain "recurring" marker the ↻ chip already states. */
    val displayTags: List<TaskTag>
        get() = if (isRecurring) tags.filterNot(TaskPresentation::isRecurringTag) else tags

    val keptScheduleSummary: String
        get() = scheduleCron?.let { cron ->
            scheduleTimezone?.takeIf { it.isNotBlank() }?.let { "Cron $cron ($it)" } ?: "Cron $cron"
        } ?: "unscheduled"

    fun updatedInstant(): Instant? = runCatching { Instant.parse(updatedAt) }.getOrNull()

    private fun scheduleObject(): JsonObject? = (schedule as? JsonObject)
}

enum class TaskCardAction {
    ViewPlan, AnswerQuestion, ReviewPlan, RunPlan, Preplan, RunNow,
    ViewExecution, ViewQuestion, Reset, Result, PublishToNotes,
}

enum class TaskSwipeAction { MarkComplete, MarkNotDone, Reset, Cancel, Delete }

enum class TaskLane(val title: String) {
    Tasks("Tasks"), Monitors("Monitors"), Internal("Internal")
}

enum class TaskFilter(val wire: String, val title: String) {
    All("all", "All"), Inbox("inbox", "Inbox"), Today("today", "Today"),
    Overdue("overdue", "Overdue"), Running("running", "Running"), Completed("completed", "Completed");

    fun matches(task: TaskV3, todayIso: String): Boolean = when (this) {
        All -> task.status.lowercase() != "completed"
        Inbox -> task.tags.isEmpty() && task.status.lowercase() == "pending"
        Today -> task.dueDate?.startsWith(todayIso) == true
        Overdue -> !task.dueDate.isNullOrBlank() && task.dueDate < todayIso && task.status.lowercase() != "completed"
        Running -> task.status.lowercase() in setOf("running", "paused")
        Completed -> task.status.lowercase() == "completed"
    }
}

enum class TaskSortField(val title: String) {
    Updated("Updated"), Created("Created"), Title("Title"), Agent("Agent"), Status("Status")
}

enum class TaskLoadState { Idle, Loading, Loaded, Failed }

@Serializable
data class TaskListPagination(
    val total: Int = 0,
    val limit: Int = 0,
    val offset: Int = 0,
    @SerialName("has_more") val hasMore: Boolean = false,
)

@Serializable
data class TaskListResponse(
    val tasks: List<TaskV3> = emptyList(),
    val pagination: TaskListPagination? = null,
    val counts: Map<String, Int>? = null,
)

data class AgentOption(val id: String, val name: String)

data class TaskCreateDraft(
    val title: String,
    val description: String = "",
    val agentId: String = "personal-assistant",
    val threadId: String = "general",
    val priority: String? = null,
    val dueDate: String? = null,
    val tagNames: List<String> = emptyList(),
    val outputMode: String = "accumulate",
    val dependsOn: List<String> = emptyList(),
    val schedule: JsonObject? = null,
)

@Serializable
data class ExecutionControlState(
    @SerialName("execution_id") val executionId: String = "",
    @SerialName("waiting_state") val waitingState: String = "",
    @SerialName("paused_from_state") val pausedFromState: String? = null,
    @SerialName("pause_kind") val pauseKind: String? = null,
    val active: Boolean = false,
    @SerialName("can_pause") val canPause: Boolean = false,
    @SerialName("can_resume") val canResume: Boolean = false,
    @SerialName("can_steer") val canSteer: Boolean = false,
    @SerialName("can_cancel") val canCancel: Boolean = false,
)

enum class ExecutionControlAction(val wire: String) {
    Pause("pause"), Resume("resume"), Steer("steer"), Cancel("cancel")
}

/** The five task-detail reads are independent; one missing section must not hide the rest. */
data class TaskDetailBundle(
    val task: JsonObject? = null,
    val panel: JsonObject? = null,
    val outputs: JsonObject? = null,
    val details: JsonObject? = null,
    val plan: JsonObject? = null,
    val unavailableSections: Set<String> = emptySet(),
)

/** A scoped realtime hint. Panel deltas carry a full run projection snapshot. */
data class TaskRealtimeEvent(
    val eventType: String,
    val taskId: String? = null,
    val executionId: String? = null,
    val panel: JsonObject? = null,
    val eventTimestamp: Long = 0,
)

@Serializable
data class MonitorListItem(
    @SerialName("task_id") val taskId: String = "",
    val title: String = "",
    val objective: String = "",
    val state: String = "active",
    @SerialName("cadence_summary") val cadenceSummary: String = "unscheduled",
    @SerialName("monitor_revision") val monitorRevision: Int = 0,
    @SerialName("last_run_at") val lastRunAt: String? = null,
    @SerialName("last_run_status") val lastRunStatus: String = "never_ran",
    @SerialName("next_run_at") val nextRunAt: String? = null,
    val health: String = "ok",
)

@Serializable
data class MonitorListPage(
    val items: List<MonitorListItem> = emptyList(),
    @SerialName("next_cursor") val nextCursor: String? = null,
    val limit: Int = 50,
    val total: Int? = null,
    val offset: Int? = null,
)

@Serializable
data class MonitorSources(
    val urls: List<String> = emptyList(),
    val domains: List<String> = emptyList(),
    @SerialName("authenticated_sources") val authenticatedSources: List<String> = emptyList(),
)

@Serializable
data class MonitorSpec(
    @SerialName("schema_version") val schemaVersion: Int = 1,
    val objective: String,
    @SerialName("query_seeds") val querySeeds: List<String> = emptyList(),
    val sources: MonitorSources = MonitorSources(),
    @SerialName("include_rules") val includeRules: List<String> = emptyList(),
    @SerialName("exclude_rules") val excludeRules: List<String> = emptyList(),
    @SerialName("match_mode") val matchMode: String = "balanced",
    @SerialName("notification_policy") val notificationPolicy: String = "material_changes",
    @SerialName("notify_initial_baseline") val notifyInitialBaseline: Boolean = false,
)

@Serializable
data class MonitorStateBlock(
    val status: String = "active",
    @SerialName("schedule_fire_count") val scheduleFireCount: Int = 0,
)

@Serializable
data class MonitorDetail(
    @SerialName("task_id") val taskId: String = "",
    val title: String = "",
    val spec: MonitorSpec,
    @SerialName("monitor_revision") val monitorRevision: Int = 0,
    val schedule: JsonObject? = null,
    val state: MonitorStateBlock = MonitorStateBlock(),
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
    val tags: List<String> = emptyList(),
)

@Serializable
data class MonitorFinding(
    @SerialName("stable_key") val stableKey: String = "",
    val title: String = "",
    @SerialName("canonical_url") val canonicalUrl: String? = null,
    val source: String = "",
    @SerialName("observed_at") val observedAt: String = "",
    @SerialName("published_at") val publishedAt: String? = null,
    val summary: String = "",
    @SerialName("why_it_matters") val whyItMatters: String = "",
    val entities: List<String> = emptyList(),
    val evidence: List<MonitorEvidence> = emptyList(),
    @SerialName("content_fingerprint") val contentFingerprint: String = "",
    val classification: String = "unchanged",
)

@Serializable
data class MonitorEvidence(
    val kind: String = "",
    val value: String = "",
    val url: String? = null,
)

@Serializable
data class MonitorSourceOutcome(
    val source: String = "",
    val status: String = "ok",
    val complete: Boolean = true,
    @SerialName("items_scanned") val itemsScanned: Int = 0,
    val note: String? = null,
)

@Serializable
data class MonitorAccessProblem(
    val source: String = "",
    val kind: String = "",
    val message: String = "",
    val since: String = "",
)

@Serializable
data class MonitorNotification(
    val policy: String = "material_changes",
    val emitted: Boolean = false,
    val channel: String = "",
    @SerialName("dedupe_key") val dedupeKey: String = "",
)

@Serializable
data class MonitorUpdate(
    @SerialName("update_id") val updateId: String = "",
    @SerialName("monitor_task_id") val monitorTaskId: String = "",
    @SerialName("monitor_revision") val monitorRevision: Int = 0,
    @SerialName("execution_id") val executionId: String = "",
    @SerialName("occurred_at") val occurredAt: String = "",
    val status: String = "unchanged",
    @SerialName("change_fingerprint") val changeFingerprint: String? = null,
    val headline: String = "",
    val summary: String = "",
    val findings: List<MonitorFinding> = emptyList(),
    val notification: MonitorNotification = MonitorNotification(),
)

@Serializable
data class MonitorRunCounts(
    val scanned: Int = 0,
    val new: Int = 0,
    val updated: Int = 0,
    val unchanged: Int = 0,
    @SerialName("possibly_removed") val possiblyRemoved: Int = 0,
)

@Serializable
data class MonitorRun(
    @SerialName("monitor_task_id") val monitorTaskId: String = "",
    @SerialName("execution_id") val executionId: String = "",
    @SerialName("monitor_revision") val monitorRevision: Int = 0,
    @SerialName("started_at") val startedAt: String = "",
    @SerialName("completed_at") val completedAt: String = "",
    val status: String = "unchanged",
    @SerialName("complete_scan") val completeScan: Boolean = false,
    @SerialName("source_outcomes") val sourceOutcomes: List<MonitorSourceOutcome> = emptyList(),
    val counts: MonitorRunCounts = MonitorRunCounts(),
    val findings: List<MonitorFinding> = emptyList(),
    @SerialName("run_fingerprint") val runFingerprint: String = "",
    @SerialName("change_fingerprint") val changeFingerprint: String? = null,
    @SerialName("access_problem") val accessProblem: MonitorAccessProblem? = null,
)

@Serializable
data class MonitorFeedbackRecord(
    @SerialName("feedback_id") val feedbackId: String = "",
    @SerialName("update_id") val updateId: String = "",
    val verdict: String = "",
    val note: String? = null,
    @SerialName("recorded_at") val recordedAt: String = "",
)

@Serializable
data class ItemsPage<T>(
    val items: List<T> = emptyList(),
    @SerialName("next_cursor") val nextCursor: String? = null,
    val limit: Int = 50,
)

data class MonitorDetailBundle(
    val detail: MonitorDetail,
    val updates: List<MonitorUpdate> = emptyList(),
    val runs: List<MonitorRun> = emptyList(),
    val feedbackByUpdate: Map<String, String> = emptyMap(),
)

data class MonitorDraft(
    val title: String = "",
    val objective: String = "",
    val urls: List<String> = emptyList(),
    val domains: List<String> = emptyList(),
    val authenticatedSources: List<String> = emptyList(),
    val querySeeds: List<String> = emptyList(),
    val includeRules: List<String> = emptyList(),
    val excludeRules: List<String> = emptyList(),
    val matchMode: String = "balanced",
    val notificationPolicy: String = "material_changes",
    val notifyInitialBaseline: Boolean = false,
    val cron: String? = "0 9 * * *",
    val timezone: String = ZoneId.systemDefault().id,
) {
    fun spec(): MonitorSpec = MonitorSpec(
        objective = objective.trim(),
        querySeeds = querySeeds.cleaned(),
        sources = MonitorSources(urls.cleaned(), domains.cleaned(), authenticatedSources.cleaned()),
        includeRules = includeRules.cleaned(),
        excludeRules = excludeRules.cleaned(),
        matchMode = matchMode,
        notificationPolicy = notificationPolicy,
        notifyInitialBaseline = notifyInitialBaseline,
    )

    fun validationError(): String? = when {
        objective.trim().isEmpty() -> "Describe what to monitor."
        objective.trim().length > 2_000 -> "Keep the objective under 2,000 characters."
        urls.cleaned().isEmpty() && domains.cleaned().isEmpty() && querySeeds.cleaned().isEmpty() ->
            "Add at least one URL, domain, or search phrase."
        urls.cleaned().any { runCatching { java.net.URI(it) }.getOrNull()?.let { u ->
            u.host.isNullOrBlank() || u.scheme?.lowercase() !in setOf("http", "https")
        } != false } -> "One of the URLs is not a valid http(s) URL."
        urls.cleaned().size > 100 -> "Keep it to 100 URLs or fewer."
        listOf(querySeeds, domains, authenticatedSources, includeRules, excludeRules).any { values ->
            values.cleaned().any { it.length > 500 }
        } -> "Keep each list entry under 500 characters."
        listOf(querySeeds, domains, authenticatedSources, includeRules, excludeRules).any { it.cleaned().size > 50 } ->
            "Keep each list to 50 entries or fewer."
        cron != null && cron.trim().split(Regex("\\s+")).size != 5 ->
            "Schedule must be a five-field cron expression."
        else -> null
    }
}

fun todayIso(zoneId: ZoneId = ZoneId.systemDefault()): String = LocalDate.now(zoneId).toString()

private fun JsonElement.asObject(): JsonObject? = this as? JsonObject
private fun JsonElement.asString(): String? = if (this is JsonNull) null else runCatching { jsonPrimitive.content }.getOrNull()
private fun List<String>.cleaned(): List<String> = map(String::trim).filter(String::isNotEmpty).distinct()
