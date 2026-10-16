package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatHistoryContractTest {
    @Test
    fun `concurrent history provenance and queue snapshots survive decoding`() {
        val session = chatJson.decodeFromString(SessionSummary.serializer(), """{"id":"branch","history_lane":"automated","internal_voice":{"kind":"branch","parent_session_id":"parent"}}""")
        assertEquals("automated", session.historyLane)
        assertEquals("parent", session.internalVoice?.parentSessionId)
        val queue = chatJson.decodeFromString(ChatQueueSnapshot.serializer(), """{"active":true,"queued":[{"id":"q","text":"Follow up","attachment_ids":["file"]}]}""")
        assertTrue(queue.active)
        assertEquals(listOf("file"), queue.queued.single().attachmentIds)
    }


    @Test
    fun `original answer provenance survives wire projection and legacy links never choose a different reply`() {
        val copy = chatJson.decodeFromString(ChatMessageDto.serializer(), """{
            "id":"copy","session_id":"parent","direction":"assistant","created_at":42,
            "chat_turn_id":"request","content":{"type":"text","text":"Summary"},
            "context_origin":{"ui_thread_id":"ideas","session_id":"branch","request_id":"request","message_id":"saved"}
        }""")
        val link = copy.project(0).originalAnswer!!
        assertEquals("ideas", link.origin.uiThreadId)
        assertEquals("branch", link.origin.sessionId)
        val answer = ChatMessageDto(id = "saved", direction = "assistant", createdAt = 42, chatTurnId = "request")
        assertTrue(link.matches(answer))
        assertTrue(!link.matches(answer.copy(id = "other")))
        val legacy = link.copy(origin = link.origin.copy(messageId = null))
        assertTrue(legacy.matches(answer))
        assertTrue(!legacy.matches(answer.copy(createdAt = 43)))
        assertTrue(!legacy.matches(answer.copy(direction = "user")))
        assertNull(copy.copy(sessionId = "branch").project(0).originalAnswer)
    }

    @Test
    fun `exact linked task answer remains visible when a later status is coalesced`() {
        val original = ChatMessage(id = "original", fromUser = false, text = "", kind = MessageKind.TaskStatus,
            task = TaskStatusCard(taskId = "task", title = "Task", status = "completed"), linkedAnswerTarget = true)
        val latest = original.copy(id = "later", linkedAnswerTarget = false)
        assertEquals(listOf("original", "later"), coalesceTaskStatusMessages(listOf(original, latest)).map { it.id })
    }

    @Test
    fun `paged sessions retain lane lifecycle scope and server counts`() {
        val page = chatJson.decodeFromString(
            SessionList.serializer(),
            """{
              "sessions":[{
                "id":"s-1","principal":"me","workspace":"default",
                "agent_id":"personal-assistant","ui_thread_id":"ideas",
                "title":"Launch notes","status":"archived",
                "history_lane":"personal","is_default_session":false,
                "created_at":1720000000000,"updated_at":1720000001000
              }],
              "total":31,"limit":15,"offset":15
            }""",
        )

        assertEquals(31, page.total)
        assertEquals(15, page.limit)
        assertEquals(15, page.offset)
        assertEquals("ideas", page.sessions.single().uiThreadId)
        assertEquals("archived", page.sessions.single().status)
        assertEquals("personal", page.sessions.single().historyLane)
    }

    @Test
    fun `global search decodes both result kinds without applying browse lanes`() {
        val result = chatJson.decodeFromString(
            HistorySearchResponse.serializer(),
            """{
              "items":[
                {"kind":"session","history_lane":"personal","session":{"id":"s-1","ui_thread_id":"general","status":"active"}},
                {"kind":"thread","history_lane":"automated","thread":{"id":"daily-brief","name":"Daily brief","archived":false,"history_lane":"automated"}}
              ],
              "total":2,"limit":15,"offset":0
            }""",
        )

        assertEquals(listOf("session:s-1", "thread:daily-brief"), result.items.map { it.identifier })
        assertEquals("automated", result.items.last().historyLane)
        assertNull(result.items.last().session)
    }

    @Test
    fun `thread resolution prefers an active session found on a later page`() {
        val archived = SessionSummary(id = "old", status = "archived")
        val active = SessionSummary(id = "current", status = "active")

        val first = selectThreadSession(null, null, listOf(archived))
        val second = selectThreadSession(first.first, first.second, listOf(active))

        assertEquals("current", second.first?.identifier())
        assertEquals("old", second.second?.identifier())
    }

    @Test
    fun `history search limit counts code points without splitting emoji`() {
        val bounded = boundedHistoryQuery("🙂".repeat(121))
        assertEquals(120, bounded.codePointCount(0, bounded.length))
        assertTrue(bounded.endsWith("🙂"))
    }

    @Test
    fun `drawer creation and paging rules match ios`() {
        assertTrue(ChatHistoryState(historyLane = ChatHistoryLane.Personal).canCreate)
        assertTrue(!ChatHistoryState(historyLane = ChatHistoryLane.Automated).canCreate)
        assertTrue(!ChatHistoryState(appliedSearch = "launch").canCreate)

        val page = ChatHistoryState(total = 31, offset = 15)
        assertEquals(16, page.pageStart)
        assertEquals(30, page.pageEnd)
        assertTrue(page.canLoadPrevious)
        assertTrue(page.canLoadNext)
    }
}
