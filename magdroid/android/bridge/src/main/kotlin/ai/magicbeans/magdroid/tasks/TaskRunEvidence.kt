package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.*
import java.time.Instant

/** Shared work-event rules. Polls and heartbeats never keep a stalled run healthy. */
object TaskRunEvidence {
    private val progressEvents = setOf(
        "llm.requested", "llm.first_token", "llm.succeeded", "llm.failed",
        "reasoning.start", "reasoning.content", "reasoning.end", "tool.started", "tool.requested",
        "tool.succeeded", "tool.failed", "tool.call.started", "tool.call.finished", "tool.call.failed",
        "step.started", "step.completed", "step.failed", "step.skipped", "step.delegated",
        "child.spawned", "child.completed", "child.failed", "child.output_available",
        "sub_goal.requested", "sub_goal.outcome", "execution.started", "execution.resumed",
        "LLMRequestSent", "LLMResponseReceived", "AgenticActionStarted", "AgenticActionExecuted",
    )
    fun isProgress(type: String, meta: JsonObject = JsonObject(emptyMap())): Boolean {
        if (type !in progressEvents) return false
        val correlation = meta["correlation"] as? JsonObject
        return !(type in setOf("llm.succeeded", "llm.failed", "LLMResponseReceived") &&
            TaskUsageModel.number(meta["provider_attempt_count"] ?: correlation?.get("provider_attempt_count")) == 0.0 &&
            (meta["response_kind"] as? JsonPrimitive)?.contentOrNull != "harness_aggregate")
    }
    fun executionId(panel: JsonObject): String? {
        val overview = panel["overview"] as? JsonObject
        val selected = (panel["debug"] as? JsonObject)?.get("selected_execution") as? JsonObject
        return text(overview?.get("execution_id")) ?: text(selected?.get("execution_id"))
            ?: text((selected?.get("state") as? JsonObject)?.get("execution_id"))
    }
    fun instant(value: JsonElement?): Long? {
        val raw = value as? JsonPrimitive ?: return null
        val millis = if (!raw.isString) raw.longOrNull else runCatching { Instant.parse(raw.content).toEpochMilli() }.getOrNull()
        return millis?.takeIf { it > 0 }
    }
    fun latest(vararg values: Long?): Long? = values.filterNotNull().filter { it > 0 }.maxOrNull()
    fun progressAt(panel: JsonObject?, executionId: String?): Long? {
        if (panel == null || executionId == null || TaskRunEvidence.executionId(panel) != executionId) return null
        val run = panel["run"] as? JsonObject
        var latest: Long? = null
        val rows = (run?.get("activity_log") as? JsonArray).orEmpty() + (run?.get("recent_activity") as? JsonArray).orEmpty()
        for (element in rows) {
            val row = element as? JsonObject ?: continue
            val meta = row["metadata"] as? JsonObject ?: continue
            if (isProgress(text(meta["event_type"]).orEmpty(), meta)) latest = TaskRunEvidence.latest(latest, instant(row["created_at"]))
        }
        for (element in ((panel["debug"] as? JsonObject)?.get("timeline") as? JsonArray).orEmpty()) {
            val row = element as? JsonObject ?: continue
            val type = text(row["title"]).orEmpty().lowercase().replace(Regex("[ _]+"), ".")
            if (isProgress(type)) latest = TaskRunEvidence.latest(latest, instant(row["timestamp"]))
        }
        return latest
    }
    fun history(body: JsonObject, executionId: String): Map<String, TaskCallUsage> {
        val columns = (body["columns"] as? JsonArray).orEmpty().map { text(it) }
        return (body["rows"] as? JsonArray).orEmpty().mapNotNull { element ->
            val row = element as? JsonArray ?: return@mapNotNull null
            fun value(key: String): JsonElement? = columns.indexOf(key).takeIf { it >= 0 }?.let { row.getOrNull(it) }
            val id = text(value("llm_call_id"))?.takeIf(String::isNotBlank) ?: return@mapNotNull null
            val input = TaskUsageModel.number(value("input_tokens"))
            val output = TaskUsageModel.number(value("output_tokens"))
            id to TaskCallUsage(if (input == null && output == null) null else ((input ?: 0.0) + (output ?: 0.0)).toInt(),
                TaskUsageModel.number(value("cost_usd")), executionId)
        }.toMap()
    }
    private fun text(value: JsonElement?): String? = (value as? JsonPrimitive)?.contentOrNull
}

/** Preserve work already observed for this root when an older REST projection arrives. */
fun TaskDetailBundle.withTaskProgress(previous: TaskDetailBundle? = null): TaskDetailBundle {
    val record = (task?.get("task") as? JsonObject) ?: task
    val state = record?.get("state") as? JsonObject
    fun text(key: String) = (state?.get(key) as? JsonPrimitive)?.contentOrNull
    val root = text("active_root_execution_id") ?: text("latest_root_execution_id") ?: panel?.let(TaskRunEvidence::executionId)
    val at = TaskRunEvidence.latest(TaskRunEvidence.instant(state?.get("last_progress_at")),
        TaskRunEvidence.progressAt(panel, root), observedProgressAt?.takeIf { progressExecutionId == root },
        previous?.observedProgressAt?.takeIf { previous.progressExecutionId == root })
    return copy(observedProgressAt = at, progressExecutionId = root)
}
