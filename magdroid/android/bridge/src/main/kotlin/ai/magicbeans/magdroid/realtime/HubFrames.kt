package ai.magicbeans.magdroid.realtime

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull

internal val hubJson: Json = Json { ignoreUnknownKeys = true; isLenient = true }

/**
 * The ids an event is about, the same set the server stamps as `refs`.
 *
 * Every field is filled from the frame's `refs` when the server sent them and
 * otherwise read out of the payload, so a filter behaves the same against a
 * server that stamps and one that predates the hub.
 */
data class HubRefs(
    val taskId: String? = null,
    val executionId: String? = null,
    val chatTurnId: String? = null,
    val agentId: String? = null,
    val sessionId: String? = null,
) {
    companion object {
        /**
         * Server `refs` first, then the payload.
         *
         * The payload lookup tries the envelope layers the variants actually
         * use (`data`, `data.event`, `data.event.payload`, `data.message`,
         * `metadata` on each) before a breadth-first scan of `data`. The scan is
         * the server chat-turn sink's own rule — a `chat_turn_id` can land
         * anywhere — and breadth-first so the shallowest copy wins.
         */
        fun from(root: JsonObject): HubRefs {
            val stamped = root["refs"] as? JsonObject
            val data = root["data"] as? JsonObject
            fun pick(name: String): String? =
                stamped?.text(name) ?: data?.let { payloadField(it, name) }
            return HubRefs(
                taskId = pick("task_id"),
                executionId = pick("execution_id"),
                chatTurnId = pick("chat_turn_id"),
                agentId = pick("agent_id"),
                sessionId = pick("session_id"),
            )
        }
    }
}

/**
 * One forwarded bus event.
 *
 * [raw] is the frame exactly as it arrived, for parsers that already take a
 * string; [root] is the same frame parsed once by the hub. [agentEvent] is the
 * `AgentEvent` variant's inner envelope (`data.event`), unwrapped so no
 * subscriber has to — the tutor once read `event_type` off the outer frame,
 * which is always `AgentEvent`, and never drew.
 *
 * [seq], [epoch], [categories] and [userRelevant] are null on a server that
 * does not stamp them yet.
 */
class HubEvent(
    val raw: String,
    val root: JsonObject,
    val eventType: String,
    val seq: Long?,
    val epoch: String?,
    val categories: List<String>?,
    val userRelevant: Boolean?,
    val refs: HubRefs,
) {
    /** The variant's body (`data`), empty when the frame carried none. */
    val data: JsonObject get() = root["data"] as? JsonObject ?: EMPTY

    /** `data.event` of an `AgentEvent`; null for every other variant. */
    val agentEvent: JsonObject? =
        if (eventType == AGENT_EVENT) (root["data"] as? JsonObject)?.get("event") as? JsonObject else null

    /** The inner `event_type` of an `AgentEvent`, e.g. `tutor.draw.shape`. */
    val agentEventType: String? get() = agentEvent?.text("event_type")

    /** The unwrapped event when there is one, else the frame itself. */
    val unwrapped: JsonObject get() = agentEvent ?: root

    override fun toString(): String = "HubEvent($eventType, epoch=$epoch, seq=$seq)"

    companion object {
        const val AGENT_EVENT = "AgentEvent"
        private val EMPTY = JsonObject(emptyMap())
    }
}

/** What one text frame is. */
sealed class HubFrame {
    data class Event(val event: HubEvent) : HubFrame()

    /** First frame of a connection on a hub-aware server. */
    data class Hello(val epoch: String, val seq: Long, val resumed: Boolean) : HubFrame()

    /** The server says this client missed events; every subscriber re-reads. */
    data class Resync(val reason: String) : HubFrame()

    /** Any other `type` frame: replies to client commands, pongs. Not events. */
    data class Control(val type: String) : HubFrame()
}

/**
 * Read one socket frame, or null when it is not JSON or not an object.
 *
 * Control frames carry `type` and never `event_type`; that is the whole
 * discriminator, so an event that happens to carry a `type` field in its own
 * body is still an event.
 */
fun parseHubFrame(text: String): HubFrame? {
    val root = runCatching { hubJson.parseToJsonElement(text) as? JsonObject }.getOrNull() ?: return null
    val eventType = root.text("event_type")
    if (eventType == null) {
        return when (val type = root.text("type") ?: return null) {
            HELLO -> HubFrame.Hello(
                epoch = root.text("epoch").orEmpty(),
                seq = (root["seq"] as? JsonPrimitive)?.longOrNull ?: 0L,
                resumed = (root["resumed"] as? JsonPrimitive)?.booleanOrNull ?: false,
            )
            RESYNC -> HubFrame.Resync(root.text("reason") ?: "unknown")
            else -> HubFrame.Control(type)
        }
    }
    return HubFrame.Event(
        HubEvent(
            raw = text,
            root = root,
            eventType = eventType,
            seq = (root["seq"] as? JsonPrimitive)?.longOrNull,
            epoch = root.text("epoch"),
            categories = (root["categories"] as? JsonArray)?.mapNotNull { it.textValue() },
            userRelevant = (root["user_relevant"] as? JsonPrimitive)?.booleanOrNull,
            refs = HubRefs.from(root),
        ),
    )
}

internal const val HELLO = "hub.hello"
internal const val RESYNC = "hub.resync"

private fun payloadField(data: JsonObject, name: String): String? {
    val event = data["event"] as? JsonObject
    val layers = listOf(
        data,
        event,
        event?.get("payload") as? JsonObject,
        data["message"] as? JsonObject,
        data["metadata"] as? JsonObject,
        event?.get("metadata") as? JsonObject,
        (event?.get("payload") as? JsonObject)?.get("metadata") as? JsonObject,
    )
    layers.forEach { layer -> layer?.text(name)?.let { return it } }
    return breadthFirst(data, name)
}

private fun breadthFirst(start: JsonElement, name: String): String? {
    val queue = ArrayDeque<JsonElement>()
    queue.addLast(start)
    var visited = 0
    // Bounded: a frame carrying a whole document must not cost a phone a
    // full walk of it for every subscriber.
    while (queue.isNotEmpty() && visited < MAX_SCAN_NODES) {
        visited++
        when (val node = queue.removeFirst()) {
            is JsonObject -> {
                node.text(name)?.let { return it }
                node.values.forEach { if (it is JsonObject || it is JsonArray) queue.addLast(it) }
            }
            is JsonArray -> node.forEach { if (it is JsonObject || it is JsonArray) queue.addLast(it) }
            else -> Unit
        }
    }
    return null
}

private const val MAX_SCAN_NODES = 512

internal fun JsonObject.text(name: String): String? = this[name].textValue()

internal fun JsonElement?.textValue(): String? {
    val primitive = this as? JsonPrimitive ?: return null
    if (primitive is JsonNull || !primitive.isString) return null
    return primitive.contentOrNull?.takeIf { it.isNotBlank() }
}
