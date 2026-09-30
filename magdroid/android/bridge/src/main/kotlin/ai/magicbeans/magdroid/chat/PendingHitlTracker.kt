package ai.magicbeans.magdroid.chat

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * How many requests are waiting on the owner, live.
 *
 * A port of `PendingHitlTracker.swift`, itself a port of the web
 * `pendingHitlStore`. The Attention badge fed this a hard zero and leaned on
 * the feed's durable `needs_action` instead, so a just-raised request was one
 * refresh late — the badge told the owner about it only after the next fetch.
 *
 * Deduped by correlation id, which is the contract. Two rules carry the weight
 * and both come from bugs the other clients already hit:
 *
 * Only the canonical `HitlRequested` counts. The legacy twins
 * (`AgenticWaitingForUser`, `UserRequestPending`, …) are co-emitted for the
 * *same* pause and keyed on `pause_state_id`, so counting them double-counts
 * the badge and strands an entry no `HitlResolved` will ever clear — the
 * monotonically-climbing badge the web comments warn about.
 *
 * The key is looked up key-first across every nesting layer, not layer-first:
 * `correlation_id` anywhere beats `pause_state_id` at the top, because the
 * dedup contract is the id, not where it happened to be written.
 */
object PendingHitlTracker {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    private val ids = mutableSetOf<String>()
    private val _count = MutableStateFlow(0)
    val count: StateFlow<Int> = _count.asStateFlow()

    /** Ingest one realtime frame. */
    @Synchronized
    fun apply(eventText: String) {
        val root = runCatching { json.parseToJsonElement(eventText) as? JsonObject }.getOrNull() ?: return
        val eventType = (root["event_type"] as? JsonPrimitive)?.contentOrNull().orEmpty()
        // `__events_…` are the bus's own bookkeeping frames, not requests.
        if (eventType.isEmpty() || eventType.startsWith("__events_")) return
        val key = correlationKey(root) ?: return

        if (isResolutionEvent(eventType)) {
            mutate { it.remove(key) }
            return
        }
        if (eventType != "HitlRequested") return
        mutate { it.add(key) }
    }

    /**
     * Replace the baseline from the authoritative feed.
     *
     * Live events move the set between fetches and the next fetch re-seeds it,
     * so a missed frame costs one refresh rather than a permanently wrong badge.
     */
    @Synchronized
    fun seed(correlationIds: List<String>) {
        val fresh = correlationIds.filter { it.isNotBlank() }.toSet()
        mutate { set ->
            set.clear()
            set.addAll(fresh)
        }
    }

    /** Drop a just-answered request so the badge ticks down without waiting. */
    @Synchronized
    fun drop(correlationId: String) {
        if (correlationId.isBlank()) return
        mutate { it.remove(correlationId) }
    }

    @Synchronized
    fun contains(correlationId: String): Boolean = correlationId in ids

    /**
     * Undo an optimistic [drop] whose request then failed.
     *
     * Deliberately the inverse of `drop` rather than a re-seed, so requests
     * that arrived while the response was in flight are not thrown away.
     */
    @Synchronized
    fun restore(correlationId: String) {
        if (correlationId.isBlank()) return
        mutate { it.add(correlationId) }
    }

    @Synchronized
    fun reset() = mutate { it.clear() }

    private fun mutate(change: (MutableSet<String>) -> Unit) {
        change(ids)
        val size = ids.size
        if (_count.value != size) _count.value = size
    }

    /**
     * The id this event is about, searched key-first across every layer.
     *
     * `id` is accepted only on the `data.request` layer — it is the legacy
     * `UserRequest` payload's own field, and elsewhere `id` means something
     * else entirely.
     */
    internal fun correlationKey(root: JsonObject): String? {
        val data = root["data"] as? JsonObject
        val dataEvent = data?.get("event") as? JsonObject
        val dataEventPayload = dataEvent?.get("payload") as? JsonObject
        val payload = root["payload"] as? JsonObject
        val dataRequest = data?.get("request") as? JsonObject

        val layers = listOf(
            root to false,
            data to false,
            dataEvent to false,
            dataEventPayload to false,
            payload to false,
            dataRequest to true,
        )
        val keys = listOf(
            "correlation_id", "pause_state_id", "approval_id",
            "clarification_id", "request_id", "id",
        )
        for (key in keys) {
            for ((layer, isDataRequest) in layers) {
                if (layer == null) continue
                if (key == "id" && !isDataRequest) continue
                val value = (layer[key] as? JsonPrimitive)?.contentOrNull()
                if (!value.isNullOrBlank()) return value
            }
        }
        return null
    }

    internal fun isResolutionEvent(eventType: String): Boolean {
        if (eventType == "HitlResolved" || eventType == "UserRequestResolved") return true
        return eventType.endsWith(".resolved") ||
            eventType.endsWith(".responded") ||
            eventType.endsWith(".expired") ||
            eventType.endsWith(".cancelled") ||
            eventType.endsWith(".dismissed")
    }

    private fun JsonPrimitive.contentOrNull(): String? = if (isString) content else content
}
