package ai.magicbeans.magdroid.attention

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * A message waiting on a reply, from `GET /channel-assist/follow-ups`.
 *
 * A different thing from an [AttentionItem] and a different endpoint. Feed items
 * are work Magician paused; these are messages a person sent that nobody has
 * answered. They share a surface because they share the question — what is
 * waiting on me — and nothing else, which is why the Messages lane is the only
 * one that carries them and "All" deliberately does not.
 */
@Serializable
data class ChannelFollowUp(
    @SerialName("annotation_id") val annotationId: String = "",
    @SerialName("candidate_id") val candidateId: String? = null,
    val provider: String = "",
    @SerialName("account_alias") val accountAlias: String = "",
    @SerialName("account_email") val accountEmail: String? = null,
    @SerialName("thread_id") val threadId: String = "",
    val lane: String = "",
    val label: String? = null,
    val reason: String? = null,
    val confidence: Double? = null,
    val subject: String? = null,
    val sender: String? = null,
    val summary: String? = null,
    @SerialName("received_at") val receivedAt: Long? = null,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("open_url") val openUrl: String? = null,
    val state: String? = null,
    /**
     * Whether a human has to look before this can be acknowledged.
     *
     * Acknowledging says "handled" without opening it, which is only honest
     * when the distillation was confident enough not to need checking.
     */
    @SerialName("review_required") val reviewRequired: Boolean = false,
    @SerialName("source_family") val sourceFamily: String? = null,
    @SerialName("available_actions") val availableActions: List<ChannelActionDescriptor> = emptyList(),
) {
    val id: String get() = annotationId

    /** Acknowledge is hidden when the item still wants eyes on it. */
    val canAcknowledge: Boolean get() = !reviewRequired

    /** What to show as the headline when the subject is missing. */
    val displayTitle: String
        get() = subject?.takeIf { it.isNotBlank() }
            ?: summary?.takeIf { it.isNotBlank() }
            ?: "Message from ${sender ?: accountAlias}"
}

@Serializable
data class ChannelActionDescriptor(
    val id: String = "",
    val label: String = "",
    val kind: String? = null,
)

@Serializable
data class ChannelFollowUpPage(
    val items: List<ChannelFollowUp> = emptyList(),
    @SerialName("next_cursor") val nextCursor: String? = null,
    val total: Int = 0,
) {
    val hasMore: Boolean get() = !nextCursor.isNullOrBlank()
}

/**
 * The resolutions a follow-up offers.
 *
 * Named rather than free strings because each is a path segment on the server —
 * `POST /channel-assist/annotations/{id}/{action}` — and a typo would be a 404
 * that looks like a card that would not resolve.
 */
enum class FollowUpAction(val id: String, val label: String) {
    Useful("useful", "Useful"),
    Acknowledge("acknowledge", "Acknowledge"),
    Snooze("snooze", "Snooze"),
    Dismiss("dismiss", "Dismiss"),
}
