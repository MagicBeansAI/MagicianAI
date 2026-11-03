package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/*
 * One row per call on the task detail's Run timeline. The execution panel's
 * `activity_log` records a model call as `llm.requested`, then `llm.routed`
 * (once per route the router picks, so again on a fallback), then
 * `llm.succeeded` / `llm.failed`; a tool call as `tool.started`, then
 * `tool.succeeded` / `tool.failed`. Drawn one row per event, every call read
 * twice and a finished call left a "running" row behind it.
 *
 * The rows of one call share an id: `metadata.llm_call_id` for a model call,
 * `metadata.call_id` for a tool call. Rows from a server that predates those
 * ids fold the way web `deriveTimeline` folds them: an `llm.requested` row is
 * the start of the next settled LLM row of the same execution. Tool rows
 * without an id stay one row each, as before.
 *
 * Pure — no Compose. A model call is titled as the chat Steps row titles
 * it ("Thinking with <model>"); tool rows keep the panel's "Using …",
 * "… returned" and "… failed", a failed one carrying why ([failureDetail]).
 */

/** One call's `activity_log` rows, folded into the one row the Run timeline draws. */
data class ActivityCallEntry(
    /** The row drawn in this call's place: the settled row once one landed, else its start. */
    val row: JsonObject,
    /** The call's start (`llm.requested` / `tool.started`), when the log holds it. */
    val start: JsonObject? = null,
    /** The latest route (`llm.routed`) the call took. */
    val routed: JsonObject? = null,
    /** The settled row (`llm.succeeded` / `llm.failed` / `tool.succeeded` / `tool.failed`). */
    val settled: JsonObject? = null,
) {
    /** The drawn row's `metadata.event_type`. */
    val eventType: String? get() = TaskActivityCalls.eventType(row)

    /** Nothing settled the call and its row says it is running. */
    val running: Boolean
        get() = settled == null &&
            TaskActivityCalls.text(row, "status")?.lowercase() in TaskActivityCalls.RUNNING_STATUSES

    /**
     * When the call began, for a live elapsed clock: the start's
     * `started_at_ms`, else when the start (or, without one, the first
     * route) was recorded.
     */
    val startedAtMillis: Long?
        get() = listOfNotNull(start, routed, row).firstNotNullOfOrNull { member ->
            TaskActivityCalls.metadataMillis(member, "started_at_ms")
                ?: TaskActivityCalls.instantMillis(member["created_at"])
                ?: TaskActivityCalls.instantMillis(member["timestamp"])
        }

    /**
     * The model this call names: the one that answered once it settled,
     * else the routed one, else the one the request asked for.
     */
    val model: String?
        get() = settled?.let { TaskActivityCalls.meta(it, "model") ?: TaskActivityCalls.meta(it, "harness_model") }
            ?: routed?.let { TaskActivityCalls.meta(it, "model") }
            ?: start?.let { TaskActivityCalls.meta(it, "model") }
            ?: TaskActivityCalls.meta(row, "model")
}

object TaskActivityCalls {
    /** The row status the panel writes for work in flight (web `FEED_STATUS.running`). */
    internal val RUNNING_STATUSES = setOf("running", "in_progress", "executing")

    private enum class Family { Llm, Tool, Context }
    private enum class Role { Start, Routed, Settled }

    private class Group(var start: Int? = null, var routed: Int? = null, var settled: Int? = null) {
        val shown: Int get() = settled ?: start ?: routed ?: 0
    }

    private fun roleOf(eventType: String?): Pair<Family, Role>? = when (eventType) {
        "context.retrieval.started" -> Family.Context to Role.Start
        "context.retrieval.completed" -> Family.Context to Role.Settled
        "llm.requested" -> Family.Llm to Role.Start
        "llm.routed" -> Family.Llm to Role.Routed
        "llm.succeeded", "llm.failed" -> Family.Llm to Role.Settled
        "tool.started", "tool.requested" -> Family.Tool to Role.Start
        "tool.succeeded", "tool.failed" -> Family.Tool to Role.Settled
        else -> null
    }

    /**
     * Fold [rows] (oldest first) into one entry per call; any other row is its
     * own entry. Entries keep the order of the row each draws.
     */
    fun fold(rows: List<JsonObject>): List<ActivityCallEntry> {
        val groups = mutableListOf<Group>()
        val single = mutableMapOf<Int, Int>() // group index → the lone non-call row it draws
        val byId = mutableMapOf<String, Int>()
        // An id-less `llm.requested` per execution, waiting for its settled row.
        val openRequest = mutableMapOf<String, Int>()

        fun place(group: Int, role: Role, index: Int) {
            val target = groups[group]
            when (role) {
                Role.Start -> if (target.start == null) target.start = index
                Role.Routed -> target.routed = index
                Role.Settled -> target.settled = index
            }
        }

        fun open(role: Role, index: Int): Int {
            groups.add(Group())
            place(groups.lastIndex, role, index)
            return groups.lastIndex
        }

        rows.forEachIndexed { index, row ->
            val (family, role) = roleOf(eventType(row)) ?: run {
                groups.add(Group())
                single[groups.lastIndex] = index
                return@forEachIndexed
            }
            val execution = meta(row, "execution_id") ?: text(row, "execution_id") ?: ""
            val callId = when (family) {
                Family.Llm -> meta(row, "llm_call_id")
                Family.Tool -> meta(row, "call_id")
                Family.Context -> meta(row, "phase_id")
            }
            if (callId != null) {
                val key = "${family.name}:$callId"
                val known = byId[key]
                if (known != null) {
                    place(known, role, index)
                    return@forEachIndexed
                }
                // A model call whose request predates the id: the open id-less
                // request of its execution is this call's start.
                val adopted = if (family == Family.Llm && role != Role.Start) openRequest[execution] else null
                val group = if (adopted != null) {
                    place(adopted, role, index)
                    adopted
                } else {
                    open(role, index)
                }
                byId[key] = group
                // Adopted, the request belongs to this id now.
                if (adopted != null) openRequest.remove(execution)
                return@forEachIndexed
            }
            if (family != Family.Llm) {
                open(role, index)
                return@forEachIndexed
            }
            when (role) {
                // A request no settled row answers stays its own row: a call
                // in flight, or one the log lost.
                Role.Start -> openRequest[execution] = open(role, index)
                Role.Routed -> openRequest[execution]?.let { place(it, role, index) } ?: open(role, index)
                Role.Settled -> {
                    val pending = openRequest.remove(execution)
                    if (pending != null) place(pending, role, index) else open(role, index)
                }
            }
        }

        return groups.mapIndexed { index, group -> index to group }
            .sortedBy { (index, group) -> single[index] ?: group.shown }
            .map { (index, group) ->
                single[index]?.let { return@map ActivityCallEntry(row = rows[it]) }
                ActivityCallEntry(
                    row = rows[group.shown],
                    start = group.start?.let(rows::get),
                    routed = group.routed?.let(rows::get),
                    settled = group.settled?.let(rows::get),
                )
            }
    }

    /**
     * The entry's title, in the chat Steps vocabulary for a model call:
     * "Thinking with <model>" (the capability once settled, when no model is
     * named), "Thinking…" while nothing has named a model yet.
     */
    fun title(entry: ActivityCallEntry): String {
        val row = entry.row
        val fallback = text(row, "title")
        val tool = listOfNotNull(entry.settled, entry.start, row).firstNotNullOfOrNull {
            meta(it, "detail")
        } ?: listOfNotNull(entry.settled, entry.start, row).firstNotNullOfOrNull {
            meta(it, "target") ?: meta(it, "tool_name")
        }
        return when (val event = entry.eventType) {
            "context.retrieval.started" -> "Preparing context"
            "context.retrieval.completed" -> when (meta(row, "status")) {
                "cancelled" -> "Context preparation cancelled"
                "partial" -> "Context prepared (partial)"
                else -> "Context prepared"
            }
            "tool.succeeded" -> tool?.let { "$it returned" } ?: fallback ?: "Tool returned"
            "tool.failed" -> tool?.let { "$it failed" } ?: fallback ?: "Tool failed"
            "tool.started", "tool.requested" -> tool?.let { "Using $it" } ?: fallback ?: "Using a tool"
            "llm.requested", "llm.routed" -> entry.model?.let { "Thinking with $it" } ?: "Thinking…"
            "llm.succeeded", "llm.failed" -> {
                val capability = listOfNotNull(entry.settled, entry.routed, entry.start)
                    .firstNotNullOfOrNull { meta(it, "capability") }
                (entry.model ?: capability)?.let { "Thinking with $it" }
                    ?: if (event == "llm.failed") "Model failed" else "Model responded"
            }
            else -> fallback ?: event?.replace('.', ' ') ?: "Activity"
        }
    }

    /**
     * The prefixes the runtime puts on a failed step's error, nested in either
     * order ("compiled-pack dispatch failed: step execution failed: …"); the
     * reason reads without them.
     */
    private val FAILURE_PREFIXES = listOf("step execution failed:", "compiled-pack dispatch failed:")

    /**
     * Why a failed tool call failed, for the line under its "… failed"
     * title: the settled row's `summary`, which the server panel projection
     * (`v3_adapter` `tool_failure_reason`) already stripped of the runtime's
     * prefixes; else its `metadata.error`, stripped here the same way. Whole:
     * the row clamps it to two lines and a tap reads the rest, as iOS does.
     * Null for any other row, and for a failure that names no reason.
     */
    fun failureDetail(entry: ActivityCallEntry): String? {
        if (entry.eventType != "tool.failed") return null
        val row = entry.settled ?: entry.row
        text(row, "summary")?.let { return it }
        return meta(row, "error")?.let(::stripFailurePrefixes)?.takeIf(String::isNotEmpty)
    }

    /** [error] without the runtime's failure prefixes, however nested, and the whitespace after each. */
    private fun stripFailurePrefixes(error: String): String {
        var reason = error.trim()
        while (true) {
            val prefix = FAILURE_PREFIXES.firstOrNull { reason.startsWith(it, ignoreCase = true) } ?: return reason
            reason = reason.substring(prefix.length).trimStart()
        }
    }

    internal fun eventType(row: JsonObject): String? = meta(row, "event_type")

    internal fun meta(row: JsonObject, key: String): String? {
        val metadata = row["metadata"] as? JsonObject ?: return null
        return text(metadata, key)
    }

    internal fun text(row: JsonObject, key: String): String? =
        (row[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }?.content?.trim()?.takeIf(String::isNotEmpty)

    internal fun metadataMillis(row: JsonObject, key: String): Long? =
        instantMillis((row["metadata"] as? JsonObject)?.get(key))

    internal fun instantMillis(value: kotlinx.serialization.json.JsonElement?): Long? =
        TaskTimeline.parseMillis((value as? JsonPrimitive)?.takeIf { it !is JsonNull }?.content)
}

/** A running row's live elapsed time, in the app's duration format (`12s`, `3m 4s`). */
object LiveElapsed {
    /** How often a running row's clock ticks. */
    const val TICK_MILLIS = 1_000L

    /** Elapsed since [startedAtMillis]; never negative, so device clock skew reads `0s`. */
    fun text(startedAtMillis: Long, nowMillis: Long): String? =
        TaskVerdict.durationIfKnown((nowMillis - startedAtMillis).coerceAtLeast(0L) / 1_000.0)
}
