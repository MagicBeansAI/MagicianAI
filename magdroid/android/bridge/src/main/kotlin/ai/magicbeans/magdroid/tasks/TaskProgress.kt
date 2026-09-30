package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * One task's progress, digested from an `ExecutionPanelDelta` for the ongoing
 * notification — the Android counterpart of what iOS's `BackgroundEngine`
 * reads into its Live Activity, field for field: the newest activity line is
 * the status, the feed length is the step count, and the overview's status
 * decides whether the run is over.
 */
data class TaskProgressCard(
    val title: String,
    val status: String,
    val stepCount: Int,
    val done: Boolean,
)

/**
 * Digest a panel state. Null when the delta carries no overview — a frame
 * that says nothing about the task should change nothing on the card.
 */
fun taskProgressCard(panel: JsonObject?, fallbackTitle: String): TaskProgressCard? {
    val overview = panel?.get("overview") as? JsonObject ?: return null
    val run = panel["run"] as? JsonObject
    // The full log when the delta carries one, the recent window otherwise —
    // the same nil-coalescing iOS applies, so both cards count alike.
    val activityLog = run?.get("activity_log") as? JsonArray
    val feed = activityLog
        ?: (run?.get("recent_activity") as? JsonArray)
        ?: JsonArray(emptyList())
    // The full log is chronological (oldest -> newest), while the legacy
    // recent window is newest-first. Treating both arrays alike makes a
    // push/socket fallback regress to the oldest visible step.
    val latest = (if (activityLog != null) feed.lastOrNull() else feed.firstOrNull())
        as? JsonObject
    val moment = latest?.text("title") ?: latest?.text("content")

    // Mirrors the backend's `TaskStatus::is_terminal`. An unknown status is
    // treated as non-terminal: a run we don't understand is better left
    // showing progress than declared finished.
    val state = overview.text("status").orEmpty()
    val status = when (state) {
        "completed" -> "Done."
        "failed" -> "Did not finish."
        "cancelled" -> "Cancelled."
        else -> moment ?: "Working…"
    }
    return TaskProgressCard(
        title = overview.text("title")?.takeIf { it.isNotBlank() } ?: fallbackTitle,
        status = status,
        stepCount = feed.size,
        done = state == "completed" || state == "failed" || state == "cancelled",
    )
}

private fun JsonObject.text(key: String): String? =
    (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.content
