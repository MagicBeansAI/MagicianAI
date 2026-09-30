package ai.magicbeans.magdroid.attention

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

/**
 * What is waiting on the owner, as `GET /feed/attention` returns it.
 *
 * Modelled from `FeedAttentionResponse` in `feed_api.rs` rather than from the
 * iOS client, because the server is the other party to this contract and iOS
 * is only another reader of it.
 *
 * The server's lanes are requests, approvals, escalations, failed and running.
 * "All" and "messages" exist on iOS but not here: the first is a merge this
 * client does itself, and the second is channel follow-ups, which are a
 * different endpoint and a later slice.
 */
val attentionJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

@Serializable
data class AttentionFeedResponse(
    val counts: AttentionLaneCounts = AttentionLaneCounts(),
    val totals: AttentionLaneTotals = AttentionLaneTotals(),
    val pages: AttentionLanePages = AttentionLanePages(),
    val requests: List<AttentionItem> = emptyList(),
    val approvals: List<AttentionItem> = emptyList(),
    val escalations: List<AttentionItem> = emptyList(),
    val failed: List<AttentionItem> = emptyList(),
    val running: List<AttentionItem> = emptyList(),
) {
    fun items(lane: AttentionLane): List<AttentionItem> = when (lane) {
        // Follow-ups are not feed items and are held separately.
        AttentionLane.Messages -> emptyList()
        AttentionLane.All -> merged()
        AttentionLane.Requests -> requests
        AttentionLane.Approvals -> approvals
        AttentionLane.Escalations -> escalations
        AttentionLane.Failed -> failed
    }

    /**
     * Every lane in one list, newest first, without duplicates.
     *
     * An item can arrive in more than one lane — an escalation that also needs
     * action — and showing it twice in "All" would make the count disagree with
     * what is on screen.
     */
    private fun merged(): List<AttentionItem> =
        (requests + approvals + escalations + failed)
            .distinctBy { it.id }
            .sortedByDescending { it.updatedAt }

    fun page(lane: AttentionLane): AttentionPageMeta = when (lane) {
        AttentionLane.Requests -> pages.requests
        AttentionLane.Approvals -> pages.approvals
        AttentionLane.Escalations -> pages.escalations
        AttentionLane.Failed -> pages.failed
        AttentionLane.Messages -> AttentionPageMeta()
        // "All" is assembled here, not paged by the server. It has more to show
        // whenever any lane it draws from does.
        AttentionLane.All -> AttentionPageMeta(
            hasMore = listOf(pages.requests, pages.approvals, pages.escalations, pages.failed)
                .any { it.hasMore },
        )
    }
}

@Serializable
data class AttentionLaneCounts(
    val requests: Long = 0,
    val approvals: Long = 0,
    val escalations: Long = 0,
    @SerialName("needs_action") val needsAction: Long = 0,
    val failed: Long = 0,
    val running: Long = 0,
)

/** True per-lane totals, independent of the page size that was returned. */
@Serializable
data class AttentionLaneTotals(
    val requests: Long = 0,
    val approvals: Long = 0,
    val escalations: Long = 0,
    val failed: Long = 0,
    val running: Long = 0,
)

@Serializable
data class AttentionPageMeta(
    val total: Int = 0,
    val limit: Int = 0,
    val cursor: String? = null,
    @SerialName("next_cursor") val nextCursor: String? = null,
    @SerialName("has_more") val hasMore: Boolean = false,
)

@Serializable
data class AttentionLanePages(
    val requests: AttentionPageMeta = AttentionPageMeta(),
    val approvals: AttentionPageMeta = AttentionPageMeta(),
    val escalations: AttentionPageMeta = AttentionPageMeta(),
    val failed: AttentionPageMeta = AttentionPageMeta(),
    val running: AttentionPageMeta = AttentionPageMeta(),
)

@Serializable
data class AttentionAction(
    val id: String = "",
    val label: String = "",
    @SerialName("action_type") val actionType: String? = null,
    val payload: JsonElement? = null,
)

@Serializable
data class AttentionItem(
    val id: String = "",
    @SerialName("item_type") val itemType: String = "",
    @SerialName("task_id") val taskId: String? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    val title: String = "",
    val summary: String? = null,
    val status: String = "",
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("updated_at") val updatedAt: Long = 0,
    val actions: List<AttentionAction> = emptyList(),
    /**
     * The free-form object carrying this item's HITL contract.
     *
     * Typed rather than left as raw JSON because every field in it is a
     * fallback in a chain — see [AttentionRequest]. Unknown keys are ignored,
     * so a subsystem stamping something new does not break the decode.
     */
    val metadata: AttentionMetadata? = null,
) {
    val needsAction: Boolean get() = status == "needs_action"
    val failed: Boolean get() = status == "failed"
}

/**
 * The lanes as tabs, in the order iOS shows them.
 *
 * `Running` is returned by the server but is not a lane here, matching iOS:
 * work in progress is not waiting on the owner, and putting it beside things
 * that are would dilute what the surface is for.
 */
enum class AttentionLane(val id: String, val label: String) {
    All("all", "All"),
    Requests("requests", "Requests"),
    Approvals("approvals", "Approvals"),
    Escalations("escalations", "Escalations"),
    Failed("failed", "Failed"),

    /**
     * Messages waiting on a reply.
     *
     * Not a feed lane — a different endpoint entirely. It sits here because it
     * answers the same question the others do, and deliberately does not appear
     * in All: a feed item is work Magician paused, a follow-up is a person
     * waiting, and merging them would make "everything that needs you" a list
     * with two meanings.
     */
    Messages("messages", "Messages"),
    ;

    companion object {
        fun from(id: String?): AttentionLane =
            entries.firstOrNull { it.id == id } ?: All
    }
}

/** How many a lane's tab should show. */
fun AttentionFeedResponse.laneCount(lane: AttentionLane): Long = when (lane) {
    AttentionLane.Requests -> totals.requests.orIfZero(counts.requests)
    AttentionLane.Approvals -> totals.approvals.orIfZero(counts.approvals)
    AttentionLane.Escalations -> totals.escalations.orIfZero(counts.escalations)
    AttentionLane.Failed -> totals.failed.orIfZero(counts.failed)
    // Counted from the follow-up page, which this response knows nothing about.
    AttentionLane.Messages -> 0
    // Deliberately the sum of the lanes rather than `needs_action`: that field
    // is the badge's input and excludes failed rows, so using it here would
    // make the All tab disagree with the list underneath it.
    AttentionLane.All ->
        totals.requests.orIfZero(counts.requests) +
            totals.approvals.orIfZero(counts.approvals) +
            totals.escalations.orIfZero(counts.escalations) +
            totals.failed.orIfZero(counts.failed)
}

/** `totals` is the truthful number; older builds only sent `counts`. */
private fun Long.orIfZero(fallback: Long): Long = if (this != 0L) this else fallback

/**
 * A request that has already been answered.
 *
 * A `HitlRequested` paired with its matching `HitlResolved`, which is how the
 * history view is assembled everywhere — there is no resolved-requests
 * resource, only the event log.
 */
data class ResolvedHitl(
    val correlationId: String,
    val prompt: String,
    val outcome: String,
    val decision: String?,
    val resolvedAtMs: Long,
)

/**
 * Resolved requests out of the HITL event backfill.
 *
 * Paired by correlation id: a resolution whose request fell outside the window
 * is dropped rather than shown with an empty question, because a history row
 * that cannot say what was asked is worse than one fewer row.
 *
 * Newest first, which is the order a history is read in.
 */
fun pairResolvedHitl(rawEventsJson: String): List<ResolvedHitl> {
    val root = runCatching {
        attentionJson.parseToJsonElement(rawEventsJson)
    }.getOrNull() ?: return emptyList()
    val array = when (root) {
        is kotlinx.serialization.json.JsonArray -> root
        is kotlinx.serialization.json.JsonObject ->
            root["events"] as? kotlinx.serialization.json.JsonArray ?: return emptyList()
        else -> return emptyList()
    }

    val prompts = mutableMapOf<String, String>()
    val resolutions = mutableListOf<ResolvedHitl>()

    array.forEach { element ->
        val event = element as? kotlinx.serialization.json.JsonObject ?: return@forEach
        fun str(source: kotlinx.serialization.json.JsonObject?, key: String) =
            (source?.get(key) as? kotlinx.serialization.json.JsonPrimitive)
                ?.takeIf { it.isString }?.content?.takeIf { it.isNotBlank() }
        val type = str(event, "event_type") ?: return@forEach
        // The payload rides under `data` when the bus wrapped it, else at the top.
        val payload = (event["data"] as? kotlinx.serialization.json.JsonObject) ?: event
        val correlationId = str(payload, "correlation_id")
            ?: str(payload, "pause_state_id")
            ?: return@forEach
        val at = (payload["timestamp"] as? kotlinx.serialization.json.JsonPrimitive)
            ?.longOrNullSafe() ?: 0L

        when (type) {
            "HitlRequested" ->
                prompts[correlationId] = str(payload, "question")
                    ?: str(payload, "prompt")
                    ?: ""

            "HitlResolved" -> resolutions += ResolvedHitl(
                correlationId = correlationId,
                prompt = "",
                // `responded` is the honest default: the pair proves it was
                // answered even when the event did not say how.
                outcome = str(payload, "outcome") ?: "responded",
                decision = str(payload, "decision"),
                resolvedAtMs = at,
            )
        }
    }

    return resolutions
        .mapNotNull { row -> prompts[row.correlationId]?.let { row.copy(prompt = it) } }
        .sortedByDescending { it.resolvedAtMs }
}

private fun kotlinx.serialization.json.JsonPrimitive.longOrNullSafe(): Long? =
    content.toLongOrNull() ?: content.toDoubleOrNull()?.toLong()
