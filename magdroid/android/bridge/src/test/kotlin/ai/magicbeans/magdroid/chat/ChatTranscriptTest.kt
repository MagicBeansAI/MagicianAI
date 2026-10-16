package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatTranscriptTest {
    private val echo = optimisticUserMessage("local-user", "hello", false, "turn-1")
    private val placeholder = liveAssistantPlaceholder("local-reply", "turn-1").copy(
        text = "partial", activityRows = listOf(ActivityRow(label = "Answering", status = "running")),
    )
    private val user = ChatMessage("server-user", true, "hello", chatTurnId = "turn-1")
    private val reply = ChatMessage("server-reply", false, "answer", chatTurnId = "turn-1")
    private val done = ChatStreamEvent.Done(messages = listOf(user, reply))

    private fun taskMessage(
        id: String,
        taskId: String,
        status: String,
        chatTurnId: String? = null,
        executionId: String? = null,
        terminalLines: List<String> = emptyList(),
    ) = ChatMessage(
        id = id,
        fromUser = false,
        text = "",
        kind = MessageKind.TaskStatus,
        task = TaskStatusCard(
            taskId = taskId,
            title = taskId,
            status = status,
            executionId = executionId,
            terminalLines = terminalLines,
        ),
        chatTurnId = chatTurnId,
    )

    @Test
    fun `persisted task lifecycle renders only its latest terminal card`() {
        val messages = coalesceTaskStatusMessages(
            listOf(
                user,
                taskMessage("running", "task-1", "running", chatTurnId = "turn-1"),
                taskMessage("ready", "task-1", "ready", chatTurnId = "turn-1"),
                taskMessage("running-again", "task-1", "running", chatTurnId = "turn-1"),
                taskMessage("completed", "task-1", "completed"),
                reply,
            ),
        )

        // The card inherits turn-1 and so sits under the reply that turn produced.
        assertEquals(listOf(user.id, reply.id, "completed"), messages.map { it.id })
        assertEquals("completed", messages[2].task?.status)
        assertEquals("turn-1", messages[2].chatTurnId)
    }

    @Test
    fun `reload places a turn's task card after that turn's reply`() {
        val orphan = taskMessage("orphan", "task-2", "running")
        val nextUser = ChatMessage("user-2", true, "thanks", chatTurnId = "turn-2")
        val messages = coalesceTaskStatusMessages(
            listOf(
                user,
                taskMessage("card", "task-1", "running", chatTurnId = "turn-1"),
                orphan,
                reply,
                nextUser,
            ),
        )
        // A card without a turn id keeps its server position.
        assertEquals(listOf(user.id, "orphan", reply.id, "card", "user-2"), messages.map { it.id })
    }

    @Test
    fun `a resynced reply lands above its own turn's live task card`() {
        val card = taskMessage("card", "task-1", "running", chatTurnId = "turn-1")
        val messages = mergeRealtimeMessage(listOf(user, card), reply)
        assertEquals(listOf(user.id, reply.id, card.id), messages.map { it.id })
    }

    @Test
    fun `a card for the streaming turn stays below the live placeholder`() {
        val card = taskMessage("card", "task-1", "running", chatTurnId = "turn-1")
        val messages = mergeRealtimeMessage(listOf(echo, placeholder), card)
        assertEquals(listOf(echo.id, placeholder.id, card.id), messages.map { it.id })
    }

    @Test
    fun `a terminal transcript card settles its task's running step row`() {
        val rows = listOf(
            ActivityRow(label = "researcher running", status = "running", kind = "step", key = "task::task-1", taskId = "task-1"),
            ActivityRow(label = "Calling search", status = "running", kind = "tool", key = "system::tool::c"),
        )
        val withRows = reply.copy(activityRows = rows)
        val settled = settleTranscriptActivity(
            listOf(user, withRows, taskMessage("done", "task-1", "completed", chatTurnId = "turn-1")),
        )
        val strip = settled.first { it.id == reply.id }.activityRows
        assertEquals("done", strip[0].status)
        assertEquals("researcher completed", strip[0].label)
        assertEquals("done", strip[1].status)
        assertEquals("Settled when task finished", strip[1].detail)

        // A running latest card leaves the strip alone.
        val live = listOf(user, withRows, taskMessage("run", "task-1", "running", chatTurnId = "turn-1"))
        assertEquals(live, settleTranscriptActivity(live))
    }

    @Test
    fun `realtime terminal task replaces running card and keeps live details`() {
        val running = taskMessage(
            "running", "task-1", "running", chatTurnId = "turn-1",
            executionId = "exec-1", terminalLines = listOf("working"),
        )
        val completed = taskMessage("completed", "task-1", "completed")

        val messages = mergeRealtimeMessage(listOf(user, running), completed)

        assertEquals(listOf(user.id, completed.id), messages.map { it.id })
        assertEquals("exec-1", messages.last().task?.executionId)
        assertEquals(listOf("working"), messages.last().task?.terminalLines)
        assertEquals("turn-1", messages.last().chatTurnId)
    }

    @Test
    fun `socket before done replaces the echo and settles each canonical id once`() {
        var messages = listOf(echo, placeholder)
        messages = mergeRealtimeMessage(messages, user)
        messages = mergeRealtimeMessage(messages, reply)
        messages = settleChatMessages(messages, placeholder.id, done)
        assertEquals(listOf(user.id, reply.id), messages.map { it.id })
        assertEquals("answer", messages.last().text)
        assertEquals(placeholder.activityRows, messages.last().activityRows)
        assertFalse(messages.first().optimistic)
        assertFalse(messages.last().streaming)
        assertEquals(messages, settleChatMessages(messages, placeholder.id, done))
        assertEquals(messages, mergeRealtimeMessage(messages, reply))
    }

    @Test
    fun `done before socket produces the same transcript`() {
        val messages = settleChatMessages(listOf(echo, placeholder), placeholder.id, done)
        assertEquals(listOf(user.id, reply.id), messages.map { it.id })
        assertEquals(messages, mergeRealtimeMessage(mergeRealtimeMessage(messages, user), reply))
    }

    @Test
    fun `late canonical reply replaces a failed local placeholder for the same turn`() {
        val failed = placeholder.copy(
            streaming = false,
            failed = true,
            text = "The connection was interrupted.",
        )

        val messages = mergeRealtimeMessage(listOf(user, failed), reply)

        assertEquals(listOf(user.id, reply.id), messages.map { it.id })
        assertEquals("answer", messages.last().text)
        assertFalse(messages.last().failed)
    }

    @Test
    fun `attachment rows and other turns never replace the optimistic text`() {
        val attachment = user.copy(id = "file", kind = MessageKind.Attachment, text = "fixture.txt")
        val anotherTurn = user.copy(id = "other-user", chatTurnId = "turn-2")
        var messages = listOf(echo, placeholder)
        messages = mergeRealtimeMessage(messages, attachment)
        messages = mergeRealtimeMessage(messages, anotherTurn)
        assertTrue(messages.contains(echo))
        messages = mergeRealtimeMessage(messages, user)
        messages = settleChatMessages(messages, placeholder.id, done)
        assertEquals(listOf(user.id, reply.id, attachment.id, anotherTurn.id), messages.map { it.id })
    }

    @Test
    fun `cancellation and empty done preserve partial text`() {
        for (event in listOf(ChatStreamEvent.Done(), done.copy(cancelled = true))) {
            val messages = settleChatMessages(listOf(echo, placeholder), placeholder.id, event)
            assertEquals("partial", messages.last().text)
            assertFalse(messages.last().streaming)
            assertEquals(placeholder.activityRows, messages.last().activityRows)
        }
    }

    @Test
    fun `missing placeholder cannot insert a late response into another session`() {
        val messages = listOf(user.copy(id = "another-session-user"))
        assertEquals(messages, settleChatMessages(messages, placeholder.id, done))
    }

    @Test
    fun `settlement keeps composed cards and removes their early socket copies`() {
        val card = reply.copy(id = "task", kind = MessageKind.TaskStatus)
        val event = done.copy(messages = listOf(user, reply, card))
        val messages = settleChatMessages(listOf(echo, placeholder, reply, card), placeholder.id, event)
        assertEquals(listOf(user.id, reply.id, card.id), messages.map { it.id })
        assertEquals(MessageKind.TaskStatus, messages.last().kind)
    }

    private fun escalation(
        id: String,
        correlationId: String?,
        executionId: String? = "exec-1",
        resolved: Boolean = false,
        resolves: Boolean = false,
    ) = ChatMessage(
        id = id,
        fromUser = false,
        text = "",
        kind = MessageKind.Escalation,
        escalation = EscalationCard(
            question = "Send it?",
            options = listOf(EscalationOption(id = "yes", label = "Send")),
            correlationId = correlationId,
            executionId = executionId,
            resolved = resolved,
            answering = if (resolved) null else "yes",
        ),
        resolvesEscalation = if (resolves) EscalationRef(correlationId, executionId) else null,
    )

    @Test
    fun `a realtime resolution settles the open card instead of adding a second one`() {
        val open = escalation("ask", "corr-1")
        val resolution = escalation("ask-resolved", "corr-1", resolved = true, resolves = true)

        val messages = mergeRealtimeMessage(listOf(user, open), resolution)

        assertEquals(listOf(user.id, open.id), messages.map { it.id })
        val card = messages.last().escalation ?: error("expected the card")
        assertTrue(card.resolved)
        assertEquals(null, card.answering)
    }

    @Test
    fun `a history resolution settles its card and leaves sibling questions open`() {
        val first = escalation("ask-1", "corr-1")
        val sibling = escalation("ask-2", "corr-2")
        val resolution = escalation("ask-1-resolved", "corr-1", resolved = true, resolves = true)

        val messages = coalesceTaskStatusMessages(listOf(first, sibling, resolution))

        assertEquals(listOf(first.id, sibling.id), messages.map { it.id })
        assertTrue(messages[0].escalation!!.resolved)
        assertFalse(messages[1].escalation!!.resolved)
    }

    @Test
    fun `a resolution without a canonical id matches by execution`() {
        val open = escalation("ask", null, executionId = "exec-9")
        val resolution = ChatMessage(
            id = "note", fromUser = false, text = "Request resolved", system = true,
            resolvesEscalation = EscalationRef(null, "exec-9"),
        )

        val messages = mergeRealtimeMessage(listOf(open), resolution)

        assertEquals(listOf(open.id), messages.map { it.id })
        assertTrue(messages.single().escalation!!.resolved)
    }

    @Test
    fun `an unmatched resolution is appended as it stands`() {
        val other = escalation("ask", "corr-1")
        val resolution = escalation("late", "corr-9", resolved = true, resolves = true)

        val messages = mergeRealtimeMessage(listOf(other), resolution)

        assertEquals(listOf(other.id, resolution.id), messages.map { it.id })
        assertFalse(messages.first().escalation!!.resolved)
    }
}
