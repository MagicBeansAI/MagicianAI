package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatSessionActionsTest {
    private val active = SessionSummary(id = "session-1", title = "Current")
    private val other = SessionSummary(id = "session-2", title = "Other")
    private val message = ChatMessage(id = "message-1", fromUser = false, text = "Hello")
    private val attachment = StagedAttachment(localId = "file-1", name = "notes.txt")

    @Test
    fun `clear removes only transcript state and preserves the session and composer`() {
        val state = ChatUiState(
            messages = listOf(message),
            draft = "unfinished thought",
            activeSessionId = active.id,
            sessions = listOf(active, other),
            attachments = listOf(attachment),
            sending = true,
            queuedPosition = 2,
            sessionActionInFlight = ChatSessionAction.Clear,
        )

        val cleared = state.afterSessionAction(ChatSessionAction.Clear, active.id)

        assertTrue(cleared.messages.isEmpty())
        assertEquals(active.id, cleared.activeSessionId)
        assertEquals("unfinished thought", cleared.draft)
        assertEquals(listOf(active, other), cleared.sessions)
        assertEquals(listOf(attachment), cleared.attachments)
        assertFalse(cleared.sending)
        assertNull(cleared.queuedPosition)
        assertNull(cleared.sessionActionInFlight)
    }

    @Test
    fun `archive and delete stop projecting the removed session`() {
        listOf(ChatSessionAction.Archive, ChatSessionAction.Delete).forEach { action ->
            val state = ChatUiState(
                messages = listOf(message),
                draft = "belongs to the old chat",
                activeSessionId = active.id,
                sessions = listOf(active, other),
                attachments = listOf(attachment),
                queuedPosition = 2,
                turnFromVoice = true,
                autoSendIn = 2,
                sessionActionInFlight = action,
            )

            val settled = state.afterSessionAction(action, active.id)

            assertNull(settled.activeSessionId)
            assertTrue(settled.messages.isEmpty())
            assertTrue(settled.draft.isEmpty())
            assertTrue(settled.attachments.isEmpty())
            assertNull(settled.queuedPosition)
            assertFalse(settled.turnFromVoice)
            assertNull(settled.autoSendIn)
            assertEquals(listOf(other), settled.sessions)
            assertNull(settled.sessionActionInFlight)
        }
    }

    @Test
    fun `session list carries the backend default-session protection`() {
        val decoded = chatJson.decodeFromString(
            SessionList.serializer(),
            """{"sessions":[{"id":"general","status":"active","is_default_session":true}]}""",
        ).sessions.single()

        assertEquals("active", decoded.status)
        assertTrue(decoded.isDefaultSession)
    }
}
