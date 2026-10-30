package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The composer stack's rules: ask dock, planner dock, queue row, auto-send, presence, archived. */
class ComposerStackTest {
    @Test fun `review-bearing transcript approvals retain the focused form before feed hydration`() {
        val schema = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }.decodeFromString(ChatInputSchema.serializer(),
            """{"type":"confirmation","approval_preview":{"title":"Review the change"}}""")
        assertTrue(schema.requiresReview)
        val card = EscalationCard("Allow?", emptyList(), "ask", "run", false, inputType = "confirmation", requiresReview = schema.requiresReview)
        assertEquals("Review and decide →", ComposerAskDock.handoff(card))
    }
    private fun ask(id: String, resolved: Boolean = false, inputType: String? = "choice", type: String? = "user_input", sensitive: SensitiveSpec? = null) =
        ChatMessage(
            id = id, fromUser = false, text = "", kind = MessageKind.Escalation,
            escalation = EscalationCard(
                question = "Which one?", options = emptyList(), correlationId = "c-$id", executionId = null,
                resolved = resolved, inputType = inputType, escalationType = type, sensitive = sensitive,
            ),
        )

    private fun task(id: String, taskId: String, status: String, store: String? = null) = ChatMessage(
        id = id, fromUser = false, text = "", kind = MessageKind.TaskStatus,
        task = TaskStatusCard(taskId = taskId, title = "Plan $taskId", status = status, storeStatus = store),
    )

    @Test
    fun `the ask dock lists open asks only, oldest first`() {
        val targets = ComposerAskDock.targets(
            listOf(ask("a"), ChatMessage("t", false, "hi"), ask("b", resolved = true), ask("c")),
        )
        assertEquals(listOf("a", "c"), targets.map { it.messageId })
    }

    @Test
    fun `the active ask stays put until it leaves, and steps wrap`() {
        val targets = ComposerAskDock.targets(listOf(ask("a"), ask("b"), ask("c")))
        assertEquals("b", ComposerAskDock.activeId(targets, "b"))
        assertEquals("a", ComposerAskDock.activeId(targets, "gone"))
        assertEquals("c", ComposerAskDock.step(targets, "a", -1))
        assertEquals("b", ComposerAskDock.step(targets, "a", 1))
        assertEquals("3 of 3", ComposerAskDock.position(targets, "c"))
        assertNull(ComposerAskDock.position(targets.take(1), "a"))
    }

    @Test
    fun `secrets grants and forms hand off, plain asks answer inline`() {
        fun card(m: ChatMessage) = m.escalation!!
        assertEquals("Enter it securely →", ComposerAskDock.handoff(card(ask("p", inputType = "password"))))
        assertEquals("Review and decide →", ComposerAskDock.handoff(card(ask("t", inputType = "confirmation", type = "tool_authorization"))))
        assertEquals("Answer in Attention →", ComposerAskDock.handoff(card(ask("f", inputType = "form"))))
        assertEquals("Enter it securely →", ComposerAskDock.handoff(card(ask("s", inputType = "text", sensitive = SensitiveSpec()))))
        assertNull(ComposerAskDock.handoff(card(ask("c", inputType = "choice"))))
        assertTrue(ComposerAskDock.isPlanApproval(card(ask("plan", inputType = "confirmation", type = "plan_approval"))))
        assertFalse(ComposerAskDock.isPlanApproval(card(ask("x", inputType = "confirmation", type = "confirmation"))))
    }

    @Test
    fun `the planner dock lists each task's latest card that is a drafted plan, newest first`() {
        val tasks = PlannerDock.candidates(
            listOf(
                task("1", "task_a", "draft"),
                task("2", "task_b", "draft"),
                task("3", "task_a", "running"),
                task("4", "task_c", "planning", store = "draft"),
            ),
        )
        assertEquals(listOf("task_c", "task_b"), tasks.map { it.taskId })
        assertEquals("1 item", PlannerDock.countLabel(1, 1))
        assertEquals("3 items across 2 tasks", PlannerDock.countLabel(3, 2))
    }

    @Test
    fun `queue rows are numbered and timed`() {
        val now = 10_000_000L
        assertEquals("#1", ComposerRows.queueItemLabel(0, null, now))
        assertEquals("#2 · just now", ComposerRows.queueItemLabel(1, now - 5_000, now))
        assertEquals("#3 · 2m ago", ComposerRows.queueItemLabel(2, now - 120_000, now))
        assertEquals("3h ago", ComposerRows.relative(now - 3 * 3_600_000L, now))
        val queued = chatJson.decodeFromString(QueuedTextMessage.serializer(), """{"id":"q","text":"hi","queued_at":1759300000.5}""")
        assertEquals(1759300000500L, queued.queuedAtMillis)
        assertNull(chatJson.decodeFromString(QueuedTextMessage.serializer(), """{"id":"q"}""").queuedAtMillis)
    }

    @Test
    fun `the auto-send bar drains over three seconds`() {
        assertEquals(1f, ComposerRows.autoSendFraction(3))
        assertEquals(1f / 3, ComposerRows.autoSendFraction(1), 0.001f)
        assertEquals(0f, ComposerRows.autoSendFraction(-1))
    }

    @Test
    fun `presence ranks needs-you over working over thinking over idle`() {
        assertEquals(ChatPresence.Idle, ChatPresenceRules.state(false, false, false, false))
        assertEquals(ChatPresence.Thinking, ChatPresenceRules.state(false, false, false, liveTurn = true))
        assertEquals(ChatPresence.Working, ChatPresenceRules.state(false, streaming = true, toolsRunning = false, liveTurn = true))
        assertEquals(ChatPresence.Working, ChatPresenceRules.state(false, false, toolsRunning = true, liveTurn = false))
        assertEquals(ChatPresence.NeedsYou, ChatPresenceRules.state(hasPendingAsk = true, streaming = true, toolsRunning = true, liveTurn = true))
    }

    @Test
    fun `presence reads the chat state`() {
        val user = ChatMessage("u", true, "hi")
        assertEquals(ChatPresence.Idle, ChatPresenceRules.derive(listOf(user), sending = false, serverRunning = false))
        assertEquals(ChatPresence.Thinking, ChatPresenceRules.derive(listOf(user), sending = true, serverRunning = false))
        val empty = ChatMessage("r", false, "", streaming = true)
        assertEquals(ChatPresence.Thinking, ChatPresenceRules.derive(listOf(user, empty), sending = false, serverRunning = false))
        val tool = empty.copy(activity = listOf("web_search"))
        assertEquals(ChatPresence.Working, ChatPresenceRules.derive(listOf(user, tool), sending = false, serverRunning = false))
        val text = empty.copy(text = "Here")
        assertEquals(ChatPresence.Working, ChatPresenceRules.derive(listOf(user, text), sending = false, serverRunning = false))
        assertEquals(ChatPresence.Working, ChatPresenceRules.derive(listOf(user), sending = false, serverRunning = true))
        // A settled reply is not live, whatever its activity was.
        val done = text.copy(streaming = false, activity = listOf("web_search"))
        assertEquals(ChatPresence.Idle, ChatPresenceRules.derive(listOf(user, done), sending = false, serverRunning = false))
        assertEquals(ChatPresence.NeedsYou, ChatPresenceRules.derive(listOf(user, text, ask("a")), sending = true, serverRunning = true))
        assertEquals("Magician needs you", ChatPresenceRules.label(ChatPresence.NeedsYou))
        assertEquals("Mags is idle", ChatPresenceRules.label(ChatPresence.Idle, assistant = "Mags"))
    }

    @Test
    fun `an archived session is read-only`() {
        assertTrue(ChatReadOnly.isArchived(SessionSummary(id = "s", status = "archived")))
        assertFalse(ChatReadOnly.isArchived(SessionSummary(id = "s", status = "active")))
        assertFalse(ChatReadOnly.isArchived(null))
    }
}
