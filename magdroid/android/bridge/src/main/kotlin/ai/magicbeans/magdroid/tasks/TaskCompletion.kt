package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive

/**
 * How a `completed` task delivered: the server's `completion_kind`
 * (`full` | `partial`) and `open_items` on V3 task rows and the task
 * detail's `state`. A completed + partial task finished with requirements
 * still open (often because something blocked it), so it reads
 * "Partly done" in the warning tone, never as a plain success.
 */
object TaskCompletion {
    const val PARTLY_DONE = "Partly done"
    private const val OPEN_ITEM_MAX_CHARS = 120

    /** `full` or `partial`; any other value or shape reads as absent. */
    fun kind(wire: JsonElement?): String? =
        (wire as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.lowercase()
            ?.takeIf { it == "full" || it == "partial" }

    /** The string entries of `open_items`, trimmed; anything else is dropped. */
    fun openItems(wire: JsonElement?): List<String> =
        (wire as? JsonArray).orEmpty().mapNotNull { item ->
            (item as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()?.takeIf(String::isNotEmpty)
        }

    fun isPartlyDone(status: String?, kind: String?): Boolean =
        status?.trim()?.lowercase() == "completed" && kind == "partial"

    /** "Open: <first item>" (one line, truncated), "· +N more" when there are others; null when none. */
    fun openLine(items: List<String>): String? {
        val first = items.firstOrNull() ?: return null
        val flat = first.replace(Regex("\\s+"), " ")
        val clipped = if (flat.length <= OPEN_ITEM_MAX_CHARS) flat else flat.take(OPEN_ITEM_MAX_CHARS - 1).trimEnd() + "…"
        val more = items.size - 1
        return "Open: $clipped" + if (more > 0) " · +$more more" else ""
    }
}
