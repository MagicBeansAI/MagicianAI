package ai.magicbeans.magdroid.chat

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.headersOf
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatHistoryPagingTest {
    private fun text(id: String, fromUser: Boolean = false) = ChatMessage(id, fromUser, "text $id")

    private fun task(id: String, taskId: String, status: String, startedAt: Long) = ChatMessage(
        id = id, fromUser = false, text = "", kind = MessageKind.TaskStatus,
        task = TaskStatusCard(taskId = taskId, title = "Build", status = status, startedAtMs = startedAt, updatedAtMs = startedAt),
    )

    @Test
    fun `has_more decodes when present and falls back to a full page when absent`() {
        val current = chatJson.decodeFromString(ChatSessionDetail.serializer(), """{"session":{"id":"s"},"messages":[],"has_more":true}""")
        assertEquals(true, current.hasMore)
        assertTrue(resolveHistoryHasMore(current.hasMore, 0))
        assertFalse(resolveHistoryHasMore(false, CHAT_EMBEDDED_HISTORY_LIMIT))

        val older = chatJson.decodeFromString(ChatSessionDetail.serializer(), """{"session":{"id":"s"},"messages":[]}""")
        assertNull(older.hasMore)
        assertTrue(resolveHistoryHasMore(older.hasMore, CHAT_EMBEDDED_HISTORY_LIMIT))
        assertFalse(resolveHistoryHasMore(older.hasMore, CHAT_EMBEDDED_HISTORY_LIMIT - 1))
    }

    @Test
    fun `an older page ends on an empty page whatever its flag says`() {
        val page = parseOlderMessagesPage("""{"messages":[{"id":"m1","direction":"user","content":{"type":"text","text":"hi"}}],"has_more":true}""", 50)
        assertEquals(listOf("m1"), page.messages.map { it.id })
        assertTrue(page.hasMore)
        assertFalse(parseOlderMessagesPage("""{"messages":[],"has_more":true}""", 50).hasMore)
        // No flag: a short page is the last one.
        assertFalse(parseOlderMessagesPage("""{"messages":[{"id":"m1"}]}""", 50).hasMore)
    }

    @Test
    fun `older page prepends above the loaded transcript and drops rows already loaded`() {
        val loaded = listOf(text("m3", fromUser = true), text("m4"))
        val merged = prependOlderMessages(loaded, listOf(text("m1", fromUser = true), text("m2"), text("m3", fromUser = true)))
        assertEquals(listOf("m1", "m2", "m3", "m4"), merged.map { it.id })
        assertTrue(prependOlderMessages(loaded, emptyList()) === loaded)
        assertTrue(prependOlderMessages(loaded, listOf(text("m4"))) === loaded)
    }

    @Test
    fun `an older lifecycle record folds into the loaded task card`() {
        val loaded = listOf(text("m3", fromUser = true), task("t-done", "task-1", "completed", startedAt = 300))
        val merged = prependOlderMessages(loaded, listOf(task("t-running", "task-1", "running", startedAt = 100)))
        val card = merged.single { it.kind == MessageKind.TaskStatus }
        assertEquals("t-done", card.id)
        assertEquals("completed", card.task?.status)
        // The card's clock starts at the first update ever seen.
        assertEquals(100L, card.task?.startedAtMs)
    }

    @Test
    fun `rows without ids get a cursor-scoped id`() {
        val rows = listOf(ChatMessageDto(direction = "user", content = MessageContent(text = "old")))
        assertEquals("history-before-m9-0", projectOlderPage(rows, "m9").single().id)
    }

    @Test
    fun `one older page per trigger, only with more to read`() {
        val ready = ChatUiState(messages = listOf(text("m1")), loading = false, activeSessionId = "s", historyHasMore = true)
        assertTrue(ready.canLoadOlderHistory())
        assertFalse(ready.copy(olderHistoryLoading = true).canLoadOlderHistory())
        assertFalse(ready.copy(historyHasMore = false).canLoadOlderHistory())
        assertFalse(ready.copy(loading = true).canLoadOlderHistory())
        assertFalse(ready.copy(messages = emptyList()).canLoadOlderHistory())
    }

    @Test
    fun `snapshot carries has_more and the raw cursor, and older pages read before it`() = runBlocking {
        val requests = mutableListOf<String>()
        val client = HttpClient(MockEngine { request ->
            requests += request.url.toString()
            val body = when (request.url.encodedPath) {
                "/api/magician/v2/chat/sessions/s1" -> """{"session":{"id":"s1"},"messages":[{"id":"m5","direction":"user","content":{"type":"text","text":"hi"}}],"has_more":true}"""
                "/api/magician/v2/chat/sessions/s1/messages" -> """{"messages":[{"id":"m4","direction":"assistant","content":{"type":"text","text":"older"}}],"has_more":false}"""
                else -> error("Unexpected request: ${request.url}")
            }
            respond(body, headers = headersOf(HttpHeaders.ContentType, "application/json"))
        })
        val repository = ChatRepository(client, { "https://owner.test" }, { emptyMap() }, { "owner" to "workspace" })
        try {
            val snapshot = repository.conversationSnapshot("s1")
            assertTrue(snapshot.hasMore)
            assertEquals("m5", snapshot.oldestMessageId)
            val page = repository.olderMessages("s1", snapshot.oldestMessageId!!)
            assertEquals(listOf("m4"), page.messages.map { it.id })
            assertFalse(page.hasMore)
            assertTrue(requests.last().contains("before=m5"))
            assertTrue(requests.last().contains("limit=$CHAT_OLDER_PAGE_LIMIT"))
        } finally { repository.close() }
    }
}
