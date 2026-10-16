package ai.magicbeans.magdroid.chat

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** The two independent history collections exposed by Magician. */
enum class ChatHistoryTab(val wire: String, val title: String) {
    Sessions("sessions", "Sessions"),
    Threads("threads", "Threads"),
}

/** User conversation history versus product-generated activity. */
enum class ChatHistoryLane(val wire: String, val title: String) {
    Personal("personal", "Personal"),
    Automated("automated", "Automated"),
}

@Serializable
data class UiThreadRecord(
    val principal: String = "",
    val workspace: String = "",
    val id: String = "",
    val name: String = "",
    val archived: Boolean = false,
    @SerialName("sort_order") val sortOrder: Long = 0,
    @SerialName("memory_summary") val memorySummary: String? = null,
    @SerialName("memory_updated_at") val memoryUpdatedAt: Long? = null,
    @SerialName("history_lane") val historyLane: String? = null,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("updated_at") val updatedAt: Long = 0,
) {
    val isGeneral: Boolean get() = id == "general"
}

@Serializable
data class UiThreadListResponse(
    val threads: List<UiThreadRecord> = emptyList(),
    val total: Int = threads.size,
    val limit: Int = threads.size,
    val offset: Int = 0,
)

@Serializable
data class HistorySearchItem(
    val kind: String = "",
    @SerialName("history_lane") val historyLane: String = ChatHistoryLane.Personal.wire,
    val session: SessionSummary? = null,
    val thread: UiThreadRecord? = null,
) {
    val identifier: String
        get() = session?.let { "session:${it.identifier()}" }
            ?: thread?.let { "thread:${it.id}" }
            ?: "invalid:$kind"
}

@Serializable
data class HistorySearchResponse(
    val items: List<HistorySearchItem> = emptyList(),
    val total: Int = 0,
    val limit: Int = 0,
    val offset: Int = 0,
)

data class HistoryPage<T>(
    val items: List<T>,
    val total: Int,
    val limit: Int,
    val offset: Int,
)

enum class HistoryMutation { Archive, Restore, Delete }

/**
 * Pick the session a thread opens, independently of UI and networking.
 *
 * An active session wins even when an archived row appeared on an earlier
 * page. The caller therefore scans the whole bounded result before falling
 * back, matching iOS and avoiding resurrecting an old transcript by accident.
 */
internal fun selectThreadSession(
    active: SessionSummary?,
    archivedFallback: SessionSummary?,
    page: List<SessionSummary>,
): Pair<SessionSummary?, SessionSummary?> {
    val selectedActive = active ?: page.firstOrNull { it.status == "active" }
    val selectedFallback = archivedFallback ?: page.firstOrNull()
    return selectedActive to selectedFallback
}

internal fun boundedHistoryQuery(value: String): String = buildString {
    val iterator = value.codePoints().iterator()
    var count = 0
    while (iterator.hasNext() && count < 120) {
        appendCodePoint(iterator.nextInt())
        count++
    }
}
