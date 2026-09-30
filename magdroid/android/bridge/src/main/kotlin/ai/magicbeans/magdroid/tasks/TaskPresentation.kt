package ai.magicbeans.magdroid.tasks

import java.time.Instant
import java.time.ZoneId

/**
 * Pure presentation rules shared by the task list and task detail, ported from
 * the web Tasks surface (`NativeTasksSurface.svelte`, `UnifiedTaskPanel.svelte`,
 * `taskTimeline.ts`). Everything here is JVM-testable and has no Compose types.
 */
object TaskPresentation {
    private val FAILURE_WORDS = listOf("failed", "cancelled", "canceled", "stopped", "error")
    private val RECURRING_TAGS = setOf("recurring", "app_recurring")

    /**
     * Web `hasFinalResult`: named artifacts, a written summary, or a completed
     * task whose outcome is not itself a failure word.
     */
    fun hasFinalResult(
        status: String,
        completionSummary: String?,
        completionOutcome: String?,
        artifactNames: List<String>?,
    ): Boolean {
        if (!artifactNames.isNullOrEmpty()) return true
        if (!completionSummary.isNullOrBlank()) return true
        if (status.lowercase() == "completed") {
            val outcome = completionOutcome?.trim()?.lowercase().orEmpty()
            if (outcome.isNotEmpty() && FAILURE_WORDS.none { outcome.contains(it) }) return true
        }
        return false
    }

    /** True for a tag the ↻ Recurring chip already states. */
    fun isRecurringTag(tag: TaskTag): Boolean =
        tag.name.trim().lowercase() in RECURRING_TAGS || tag.id.trim().lowercase() == "app_recurring"

    fun isRecurring(cron: String?, tags: List<TaskTag>, hasRecurringSchedule: Boolean = false): Boolean =
        !cron.isNullOrBlank() || tags.any(::isRecurringTag) || hasRecurringSchedule

    /**
     * Plain-English cron description for the common five-field shapes the
     * schedule editor writes; anything else falls back to the raw expression.
     */
    fun describeCron(cron: String): String {
        val trimmed = cron.trim()
        if (trimmed.isEmpty()) return ""
        val fields = trimmed.split(Regex("\\s+"))
        if (fields.size != 5) return trimmed
        val (minute, hour, dayOfMonth, month, dayOfWeek) = fields
        if (month != "*") return trimmed
        val allDays = dayOfMonth == "*" && dayOfWeek == "*"

        stepOf(minute)?.let { n ->
            if (hour == "*" && allDays) return if (n == 1) "Every minute" else "Every $n minutes"
        }
        if (minute == "*" && hour == "*" && allDays) return "Every minute"
        val minuteValue = minute.toIntOrNull()?.takeIf { it in 0..59 } ?: return trimmed
        if (hour == "*" && allDays) {
            return if (minuteValue == 0) "Every hour" else "Every hour at :${pad(minuteValue)}"
        }
        stepOf(hour)?.let { n ->
            if (!allDays) return trimmed
            val every = if (n == 1) "Every hour" else "Every $n hours"
            return if (minuteValue == 0) every else "$every at :${pad(minuteValue)}"
        }
        val hourValue = hour.toIntOrNull()?.takeIf { it in 0..23 } ?: return trimmed
        val clock = "${pad(hourValue)}:${pad(minuteValue)}"
        return when {
            allDays -> "Daily at $clock"
            dayOfMonth == "*" && dayOfWeek == "1-5" -> "Weekdays at $clock"
            dayOfMonth == "*" -> dayName(dayOfWeek)?.let { "Weekly on $it at $clock" } ?: trimmed
            dayOfWeek == "*" -> dayOfMonth.toIntOrNull()?.takeIf { it in 1..31 }
                ?.let { "Monthly on day $it at $clock" } ?: trimmed
            else -> trimmed
        }
    }

    /** An app recurring behaviour's `interval_seconds`, phrased like a cron description. */
    fun describeInterval(seconds: Long?): String? {
        val value = seconds?.takeIf { it > 0 } ?: return null
        return when {
            value % 86_400 == 0L -> (value / 86_400).let { if (it == 1L) "Daily" else "Every $it days" }
            value % 3_600 == 0L -> (value / 3_600).let { if (it == 1L) "Every hour" else "Every $it hours" }
            value % 60 == 0L -> (value / 60).let { if (it == 1L) "Every minute" else "Every $it minutes" }
            else -> "Every ${value}s"
        }
    }

    private fun stepOf(field: String): Int? =
        field.removePrefix("*/").takeIf { field.startsWith("*/") }?.toIntOrNull()?.takeIf { it > 0 }

    private fun dayName(field: String): String? = when (field.lowercase()) {
        "0", "7", "sun" -> "Sunday"
        "1", "mon" -> "Monday"
        "2", "tue" -> "Tuesday"
        "3", "wed" -> "Wednesday"
        "4", "thu" -> "Thursday"
        "5", "fri" -> "Friday"
        "6", "sat" -> "Saturday"
        else -> null
    }

    private fun pad(value: Int): String = value.toString().padStart(2, '0')
}

/** Where an output file came from, matching web `TaskPanelFile.scope`. */
enum class TaskOutputScope(val wire: String, val title: String, val description: String) {
    Task(
        "task",
        "Task deliverables",
        "Stable task-level deliverables. These can be promoted or replaced across runs.",
    ),
    Execution("execution", "Direct outputs", "Files written directly by the selected execution."),
    Delegated("delegated", "Delegated outputs", "Files returned by work delegated from the selected execution."),
    Artifact("artifact", "Persisted artifacts", "File-backed evidence persisted during the selected execution.");

    companion object {
        /** Absent or unknown scope is a task deliverable, as on web (`scope ?? 'task'`). */
        fun fromWire(value: String?): TaskOutputScope =
            entries.firstOrNull { it.wire == value?.trim()?.lowercase() } ?: Task
    }
}

data class TaskOutputSection<T>(val scope: TaskOutputScope, val files: List<T>)

data class TaskOutputGroups<T>(
    val deliverables: List<T>,
    /** Non-empty intermediate sections, in Direct → Delegated → Persisted order. */
    val intermediates: List<TaskOutputSection<T>>,
) {
    val intermediateCount: Int get() = intermediates.sumOf { it.files.size }
}

object TaskOutputGrouping {
    fun <T> group(files: List<T>, scopeOf: (T) -> TaskOutputScope): TaskOutputGroups<T> {
        val deliverables = files.filter { scopeOf(it) == TaskOutputScope.Task }
        val intermediates = listOf(TaskOutputScope.Execution, TaskOutputScope.Delegated, TaskOutputScope.Artifact)
            .mapNotNull { scope ->
                files.filter { scopeOf(it) == scope }.takeIf(List<T>::isNotEmpty)?.let { TaskOutputSection(scope, it) }
            }
        return TaskOutputGroups(deliverables, intermediates)
    }
}

/** Web `ExecutionPanelDelegationGroup`: one delegated child in the parent's activity log. */
data class TaskDelegationGroup(
    val executionId: String,
    val agentId: String,
    val status: String,
    val entryCount: Int = 0,
    val parentExecutionId: String? = null,
    val startedAt: String? = null,
    val completedAt: String? = null,
)

sealed interface TaskTimelineSegment<out T> {
    data class Row<T>(val entry: T) : TaskTimelineSegment<T>
    data class Delegation<T>(val group: TaskDelegationGroup, val entries: List<T>) : TaskTimelineSegment<T>
}

data class TaskDelegationSpan(
    val startClock: String?,
    val endClock: String?,
    val duration: String?,
    val summary: String?,
)

enum class TaskTimelineMode(val wire: String, val title: String) {
    Grouped("grouped", "Grouped"), Chronological("chronological", "Chronological");

    companion object {
        fun fromWire(value: String?): TaskTimelineMode = entries.firstOrNull { it.wire == value } ?: Grouped
    }
}

object TaskTimeline {
    /**
     * Web `groupTimelineByDelegation`: one block per delegated child, placed at
     * its first row. Rows whose execution has no delegation entry stay plain.
     */
    fun <T> groupByDelegation(
        entries: List<T>,
        delegations: List<TaskDelegationGroup>,
        executionIdOf: (T) -> String?,
    ): List<TaskTimelineSegment<T>> {
        val groups = delegations.filter { it.executionId.isNotBlank() }.associateBy(TaskDelegationGroup::executionId)
        if (groups.isEmpty()) return entries.map { TaskTimelineSegment.Row(it) }
        val order = mutableListOf<Any>() // either a Row or an execution id placeholder
        val buckets = linkedMapOf<String, MutableList<T>>()
        for (entry in entries) {
            val id = executionIdOf(entry)
            if (id == null || id !in groups) {
                order += TaskTimelineSegment.Row(entry)
                continue
            }
            val bucket = buckets[id]
            if (bucket == null) {
                buckets[id] = mutableListOf(entry)
                order += DelegationSlot(id)
            } else {
                bucket += entry
            }
        }
        return order.map { slot ->
            @Suppress("UNCHECKED_CAST")
            when (slot) {
                is DelegationSlot -> TaskTimelineSegment.Delegation(groups.getValue(slot.id), buckets.getValue(slot.id).toList())
                else -> slot as TaskTimelineSegment<T>
            }
        }
    }

    private data class DelegationSlot(val id: String)

    /** Web `delegationSpan`, with `HH:mm` local clocks for a phone-width header. */
    fun delegationSpan(
        entryMillis: List<Long?>,
        group: TaskDelegationGroup?,
        zone: ZoneId = ZoneId.systemDefault(),
    ): TaskDelegationSpan {
        var start = parseMillis(group?.startedAt)
        var end = parseMillis(group?.completedAt)
        val recorded = entryMillis.filterNotNull().filter { it > 0 }
        if (recorded.isNotEmpty()) {
            if (start == null) start = recorded.first()
            if (end == null && group?.status?.lowercase() != "running") end = recorded.last()
        }
        val startClock = clock(start, zone)
        val endClock = clock(end, zone)
        val duration = if (start != null && end != null && end >= start) {
            TaskVerdict.durationIfKnown((end - start) / 1_000.0)
        } else null
        val summary = when {
            startClock != null && endClock != null && startClock != endClock && duration != null ->
                "$startClock – $endClock ($duration)"
            startClock != null && endClock != null && startClock != endClock -> "$startClock – $endClock"
            startClock != null -> if (group?.status?.lowercase() == "running") "started $startClock" else startClock
            else -> null
        }
        return TaskDelegationSpan(startClock, endClock, duration, summary)
    }

    /** History's "· 5m" suffix: the root execution's own duration, or null when either end is unknown. */
    fun executionDuration(startedAt: String?, endedAt: String?): String? {
        val start = parseMillis(startedAt) ?: return null
        val end = parseMillis(endedAt) ?: return null
        if (end < start) return null
        return TaskVerdict.durationIfKnown((end - start) / 1_000.0)
    }

    /** ISO-8601 or epoch seconds/millis, as the panel payload mixes both. */
    fun parseMillis(value: String?): Long? {
        val text = value?.trim()?.takeIf(String::isNotEmpty) ?: return null
        runCatching { Instant.parse(text).toEpochMilli() }.getOrNull()?.let { return it.takeIf { ms -> ms > 0 } }
        val raw = text.toDoubleOrNull()?.takeIf(Double::isFinite)?.takeIf { it > 0 } ?: return null
        return (if (raw > 10_000_000_000.0) raw else raw * 1_000.0).toLong()
    }

    private fun clock(millis: Long?, zone: ZoneId): String? {
        val value = millis?.takeIf { it > 0 } ?: return null
        val time = Instant.ofEpochMilli(value).atZone(zone)
        return "%02d:%02d".format(time.hour, time.minute)
    }
}
