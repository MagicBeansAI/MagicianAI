package ai.magicbeans.magdroid.chat

import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject

/** The session detail embeds the newest messages; this many is a full page. */
internal const val CHAT_EMBEDDED_HISTORY_LIMIT = 200

/** One scroll-to-top reads this many older messages, as the web does. */
internal const val CHAT_OLDER_PAGE_LIMIT = 50

/**
 * Whether the session detail left older messages behind.
 *
 * The server says so in `has_more`; an older server omits it, and a full
 * embedded page is then the best guess.
 */
internal fun resolveHistoryHasMore(serverHasMore: Boolean?, embeddedCount: Int): Boolean =
    serverHasMore ?: (embeddedCount >= CHAT_EMBEDDED_HISTORY_LIMIT)

/** `GET /chat/sessions/{id}/messages?before=…`: one page older than the cursor. */
data class OlderMessagesPage(val messages: List<ChatMessageDto>, val hasMore: Boolean)

/** An empty page ends paging whatever the flag says, so a stuck cursor cannot loop. */
internal fun parseOlderMessagesPage(body: String, limit: Int): OlderMessagesPage {
    val root = chatJson.parseToJsonElement(body).jsonObject
    val rows = chatJson.decodeFromJsonElement(ListSerializer(ChatMessageDto.serializer()), root.getValue("messages"))
    val serverHasMore = (root["has_more"] as? JsonPrimitive)?.booleanOrNull
    return OlderMessagesPage(rows, rows.isNotEmpty() && (serverHasMore ?: (rows.size >= limit)))
}

/**
 * Project an older page. A row without an id gets one scoped to its cursor,
 * so it cannot collide with the `history-N` fallback of the loaded page.
 */
internal fun projectOlderPage(rows: List<ChatMessageDto>, before: String): List<ChatMessage> =
    rows.mapIndexed { index, dto ->
        val projected = dto.project(index)
        if (dto.id == null) projected.copy(id = "history-before-$before-$index") else projected
    }

/**
 * Put an older page above the loaded transcript.
 *
 * Rows already loaded are dropped by id, and the whole transcript is
 * re-coalesced: a task's earlier lifecycle records fold into its loaded card,
 * and a loaded resolution settles the older question it answers.
 */
internal fun prependOlderMessages(loaded: List<ChatMessage>, older: List<ChatMessage>): List<ChatMessage> {
    val known = loaded.mapTo(HashSet()) { it.id }
    val fresh = older.filter { it.id !in known }.distinctBy { it.id }
    if (fresh.isEmpty()) return loaded
    return coalesceTaskStatusMessages(fresh + loaded)
}

/** One older page per trigger: only with more to read, and never over a load in flight. */
internal fun ChatUiState.canLoadOlderHistory(): Boolean =
    historyHasMore && !olderHistoryLoading && !loading && messages.isNotEmpty() && activeSessionId != null
