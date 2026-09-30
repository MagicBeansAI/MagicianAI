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

        assertEquals(listOf(user.id, "completed", reply.id), messages.map { it.id })
        assertEquals("completed", messages[1].task?.status)
        assertEquals("turn-1", messages[1].chatTurnId)
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
}
