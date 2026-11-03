package ai.magicbeans.magdroid.chat

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.Url
import io.ktor.http.headersOf
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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

    @Test
    fun `a delete holds its row as Deleting until the server answers`() {
        val idle = ChatHistoryState()
        val deleting = idle.beginMutation(sessionMutationKey("s1"), HistoryMutation.Delete)!!
        assertEquals(HistoryMutation.Delete, deleting.sessionMutation("s1"))
        assertEquals("Deleting…", deleting.sessionMutation("s1")?.progressLabel())
        assertNull(deleting.sessionMutation("s2"))
        assertNull(deleting.threadMutation("s1"))
        // One change at a time: a second delete, of this row or another, is refused.
        assertNull(deleting.beginMutation(sessionMutationKey("s1"), HistoryMutation.Delete))
        assertNull(deleting.beginMutation(sessionMutationKey("s2"), HistoryMutation.Delete))

        val succeeded = deleting.settleMutation()
        assertNull(succeeded.mutationInFlight)
        assertNull(succeeded.sessionMutation("s1"))
        assertNull(succeeded.error)

        val failed = deleting.settleMutation("Magician is offline.")
        assertNull(failed.sessionMutation("s1"))
        assertEquals("Magician is offline.", failed.error)
    }

    @Test
    fun `history re-reads quietly only when a visible row's working state flips`() {
        val rows = setOf("s1", "s2")
        val idle = HistoryActivityPoll(rows, emptySet())
        val working = HistoryActivityPoll(rows, setOf("s1"))
        // The first poll is only a baseline.
        assertFalse(historyActivityChanged(null, working))
        assertFalse(historyActivityChanged(working, working))
        assertTrue(historyActivityChanged(idle, working))
        assertTrue(historyActivityChanged(working, idle))
        // Different rows mean the page itself was just re-read.
        assertFalse(historyActivityChanged(idle, HistoryActivityPoll(setOf("s3"), setOf("s3"))))
    }

    private fun thread(id: String) = UiThreadRecord(id = id, name = id)

    @Test
    fun `showing a thread's sessions picks Sessions, clears search and page`() {
        val shown = ChatHistoryState(
            activeTab = ChatHistoryTab.Threads,
            searchText = "x",
            appliedSearch = "x",
            offset = 15,
        ).showingSessionsOf(thread("plans"))
        assertEquals(ChatHistoryTab.Sessions, shown.activeTab)
        assertEquals("", shown.searchText)
        assertFalse(shown.searchActive)
        assertEquals(0, shown.offset)
        assertEquals("plans", shown.sessionThreadFilter?.id)
        // Another tap replaces the filter.
        assertEquals("ideas", shown.showingSessionsOf(thread("ideas")).sessionThreadFilter?.id)
    }

    @Test
    fun `a thread filter drops the lane from the sessions query`() {
        val personal = ChatHistoryState(historyLane = ChatHistoryLane.Personal)
        assertEquals(SessionsQuery(historyLane = ChatHistoryLane.Personal, uiThreadId = null), personal.sessionsQuery())
        val automated = ChatHistoryState(historyLane = ChatHistoryLane.Automated).showingSessionsOf(thread("plans"))
        assertEquals(SessionsQuery(historyLane = null, uiThreadId = "plans"), automated.sessionsQuery())
    }

    @Test
    fun `clearing the thread filter restores the lane`() {
        val cleared = ChatHistoryState(historyLane = ChatHistoryLane.Personal)
            .showingSessionsOf(thread("plans"))
            .withoutThreadFilter()
        assertNull(cleared.sessionThreadFilter)
        assertEquals(SessionsQuery(historyLane = ChatHistoryLane.Personal, uiThreadId = null), cleared.sessionsQuery())
    }

    @Test
    fun `general filters too`() {
        assertEquals("general", ChatHistoryState().showingSessionsOf(thread("general")).sessionsQuery().uiThreadId)
    }

    @Test
    fun `a thread-filtered Sessions list can always add a session`() {
        val automated = ChatHistoryState(historyLane = ChatHistoryLane.Automated)
        assertFalse(automated.canCreate)
        val filtered = automated.showingSessionsOf(thread("plans"))
        assertTrue(filtered.canCreate)
        assertFalse(filtered.copy(activeTab = ChatHistoryTab.Threads).canCreate)
        assertFalse(filtered.copy(appliedSearch = "launch").canCreate)
    }

    @Test
    fun `new session goes in the filtered thread and its lane`() {
        assertEquals("general" to ChatHistoryLane.Personal, ChatHistoryState().newSessionTarget())
        val automatedThread = UiThreadRecord(id = "daily-brief", name = "Daily brief", historyLane = "automated")
        assertEquals(
            "daily-brief" to ChatHistoryLane.Automated,
            ChatHistoryState().showingSessionsOf(automatedThread).newSessionTarget(),
        )
        assertEquals("plans" to ChatHistoryLane.Personal, ChatHistoryState().showingSessionsOf(thread("plans")).newSessionTarget())
        assertEquals(
            "general" to ChatHistoryLane.Personal,
            ChatHistoryState().showingSessionsOf(thread("plans")).withoutThreadFilter().newSessionTarget(),
        )
    }

    @Test
    fun `a thread's sessions are read with ui_thread_id and no lane`() = runBlocking {
        val requests = mutableListOf<Url>()
        val client = HttpClient(MockEngine { request ->
            requests += request.url
            respond(
                """{"sessions":[{"id":"s-1","ui_thread_id":"plans","history_lane":"automated"}],"total":1,"limit":15,"offset":0}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = ChatRepository(client, { "https://owner.test" }, { emptyMap() }, { "owner" to "workspace" })
        try {
            val query = ChatHistoryState().showingSessionsOf(thread("plans")).sessionsQuery()
            val page = repository.sessionsPage(historyLane = query.historyLane, uiThreadId = query.uiThreadId)
            assertEquals(listOf("s-1"), page.items.map { it.identifier() })
            val url = requests.single()
            assertTrue(url.encodedPath.endsWith("/chat/sessions"))
            assertEquals("plans", url.parameters["ui_thread_id"])
            assertNull(url.parameters["history_lane"])
        } finally { repository.close() }
    }

    @Test
    fun `quiet re-reads run at most once per poll interval`() {
        assertEquals(0L, quietRefreshDelay(null, 10_000))
        assertEquals(2_000L, quietRefreshDelay(9_000, 10_000))
        assertEquals(0L, quietRefreshDelay(5_000, 10_000))
        assertEquals(HISTORY_QUIET_REFRESH_INTERVAL_MS, quietRefreshDelay(10_000, 10_000))
    }
}
