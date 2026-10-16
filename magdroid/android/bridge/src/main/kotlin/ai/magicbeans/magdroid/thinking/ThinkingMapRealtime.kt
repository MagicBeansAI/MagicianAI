package ai.magicbeans.magdroid.thinking

import ai.magicbeans.magdroid.realtime.HubDelivery
import ai.magicbeans.magdroid.realtime.HubFilter
import ai.magicbeans.magdroid.realtime.RealtimeHub
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

/**
 * A notice that a map moved on.
 *
 * The backend sends the id and the new revision and no payload, deliberately —
 * one authoritative fetch path stays the only way the map is read.
 */
data class ThinkingMapNotice(val mapId: String, val revision: Long)

/**
 * One stage of an owner-triggered interpretation, off the realtime bus.
 *
 * [stage] is null when the server named a stage this build has never heard of
 * — the vocabulary is allowed to grow server-first, and the honest response to
 * a stranger is to keep the current line, not to guess. [utteranceId] is what
 * lets a client ignore progress for a run it did not start: the bus is shared,
 * and two maps thinking at once must not drive each other's strip.
 */
data class ThinkingInterpretProgress(
    val mapId: String,
    val utteranceId: String,
    val stage: ThinkingProgress?,
    val nodeCount: Int?,
)

/**
 * Reads `ThinkingMapUpdated` off the realtime bus.
 *
 * The map was the one live surface on this client that never went live. Chat,
 * tasks, Today and the tutor all read the realtime bus; an open map did not, so
 * an agent adding nodes in the background, a consolidation finishing, or
 * anybody else editing simply never appeared. The board sat still and looked
 * finished.
 *
 * Push is an accelerator and not the correctness path, matching iOS: the notice
 * carries no map, and the caller re-fetches. A dropped socket costs seconds of
 * staleness, never a wrong board.
 */
class ThinkingMapRealtime(private val context: Context) {

    private val hub: RealtimeHub by lazy { RealtimeHub.shared(context) }
    private var job: Job? = null

    /**
     * Follow one map until stopped, calling [onNotice] for each update to it,
     * [onProgress] for each interpretation stage narrated against it, and
     * [onResync] when the realtime hub may have missed notices (re-fetch).
     *
     * Scoped to a single map id because a handset watching one board has no use
     * for notices about the rest of the library, and re-fetching on them would
     * cost a request per edit anybody made anywhere. The hub filters to the two
     * map event types, so chat tokens never reach this parser.
     */
    fun follow(
        scope: CoroutineScope,
        mapId: String,
        onNotice: (ThinkingMapNotice) -> Unit,
        onProgress: (ThinkingInterpretProgress) -> Unit = {},
        onResync: () -> Unit = {},
    ) {
        job?.cancel()
        // Off the main dispatcher: the caller is a ViewModel whose scope runs
        // Main.immediate. The callbacks stay thread-safe by construction: both
        // write a MutableStateFlow or launch back onto the caller's scope.
        job = scope.launch(Dispatchers.Default) {
            hub.subscribe(HubFilter(eventTypes = listOf(EVENT_TYPE, PROGRESS_EVENT_TYPE))).collect { delivery ->
                when (delivery) {
                    is HubDelivery.Resync -> onResync()
                    is HubDelivery.Event -> {
                        val root = delivery.event.root
                        when (delivery.event.eventType) {
                            EVENT_TYPE -> mapNoticeFrom(eventBody(root))?.let { notice ->
                                if (notice.mapId == mapId) onNotice(notice)
                            }
                            PROGRESS_EVENT_TYPE -> interpretProgressFrom(eventBody(root))?.let { progress ->
                                if (progress.mapId == mapId) onProgress(progress)
                            }
                        }
                    }
                }
            }
        }
    }

    suspend fun stop() {
        job?.cancelAndJoin()
        job = null
    }
}

/**
 * One frame, read as a map notice.
 *
 * Free of the socket so the routing can be tested by handing it event JSON,
 * which is the part worth testing — holding a WebSocket open is Ktor's job.
 * (The socket loop parses each frame once and dispatches on `event_type`; this
 * string form is the testable seam and the one-off path.)
 */
fun parseMapNotice(raw: String, json: Json = Json { ignoreUnknownKeys = true }): ThinkingMapNotice? {
    val root = parsedFrame(raw, EVENT_TYPE, json) ?: return null
    return mapNoticeFrom(eventBody(root))
}

/**
 * One frame, read as an interpretation stage.
 *
 * A recognisable event with an unknown stage still parses — [stage] comes back
 * null and the caller keeps its current line. Dropping the whole event instead
 * would make every future stage the server learns to narrate look like a
 * dropped frame on old builds.
 */
fun parseInterpretProgress(
    raw: String,
    json: Json = Json { ignoreUnknownKeys = true },
): ThinkingInterpretProgress? {
    val root = parsedFrame(raw, PROGRESS_EVENT_TYPE, json) ?: return null
    return interpretProgressFrom(eventBody(root))
}

private fun parsedFrame(raw: String, eventType: String, json: Json): JsonObject? {
    val root = runCatching { json.parseToJsonElement(raw) as? JsonObject }.getOrNull() ?: return null
    val type = (root["event_type"] as? JsonPrimitive)?.contentOrNullSafe()
    return if (type == eventType) root else null
}

/**
 * The event's body, from an already-parsed frame.
 *
 * The server serializes `RuntimeTransportEvent` with `tag = "event_type",
 * content = "data"`, so the fields ride under `data`. This parser used to read
 * `payload`-or-root — a shape the bus never sent — and its own tests asserted
 * that invention back at it, so `ThinkingMapUpdated` never actually routed and
 * the board only moved on the poll. The root fallback is kept for tolerance;
 * `data` is the wire.
 */
private fun eventBody(root: JsonObject): JsonObject = (root["data"] as? JsonObject) ?: root

private fun mapNoticeFrom(body: JsonObject): ThinkingMapNotice? {
    val mapId = (body["map_id"] as? JsonPrimitive)?.contentOrNullSafe() ?: return null
    val revision = (body["revision"] as? JsonPrimitive)?.longOrNull ?: 0L
    return ThinkingMapNotice(mapId, revision)
}

private fun interpretProgressFrom(body: JsonObject): ThinkingInterpretProgress? {
    val mapId = (body["map_id"] as? JsonPrimitive)?.contentOrNullSafe() ?: return null
    val utteranceId = (body["utterance_id"] as? JsonPrimitive)?.contentOrNullSafe() ?: return null
    val stage = (body["stage"] as? JsonPrimitive)?.contentOrNullSafe() ?: return null
    val nodeCount = (body["node_count"] as? JsonPrimitive)?.longOrNull?.toInt()
    return ThinkingInterpretProgress(
        mapId = mapId,
        utteranceId = utteranceId,
        stage = ThinkingProgress.fromWire(stage),
        nodeCount = nodeCount,
    )
}

private const val EVENT_TYPE = "ThinkingMapUpdated"
private const val PROGRESS_EVENT_TYPE = "ThinkingMapInterpretProgress"

private fun JsonPrimitive.contentOrNullSafe(): String? = content.takeIf { it.isNotBlank() }
