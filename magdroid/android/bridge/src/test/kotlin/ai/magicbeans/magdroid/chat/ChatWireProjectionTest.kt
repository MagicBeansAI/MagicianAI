package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The chat wire, as `magician/src/magician_v2/chat/models.rs` writes it.
 *
 * These fixtures are hand-built from the Rust `ChatMessage` and its
 * `#[serde(tag = "type", rename_all = "snake_case")]` content enum, not captured
 * from a running server — which is the point. The client parsed `role` and a
 * string `content` for its whole life; both are wrong, and nothing here caught
 * it because nothing here read the wire.
 */
class ChatWireProjectionTest {
    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    private fun parse(body: String): ChatSessionDetail =
        json.decodeFromString(ChatSessionDetail.serializer(), body)

    /**
     * The session is nested and messages sit beside it. Reading the id off the
     * top level found nothing.
     */
    @Test
    fun `session detail carries the session under its own key`() {
        val detail = parse(
            """
            {
              "session": {
                "id": "sess-1",
                "principal": "anonymous",
                "workspace": "default",
                "agent_id": "magician",
                "ui_thread_id": "general",
                "title": "Groceries",
                "status": "active",
                "created_at": 1754800000000,
                "updated_at": 1754800500000
              },
              "messages": []
            }
            """,
        )
        assertEquals("sess-1", detail.session.identifier())
        assertEquals("Groceries", detail.session.title)
    }

    /** `direction`, not `role` — the field the client used does not exist. */
    @Test
    fun `direction decides who spoke`() {
        val detail = parse(
            """
            {
              "session": {"id": "s"},
              "messages": [
                {"id": "m1", "session_id": "s", "direction": "user",
                 "content": {"type": "text", "text": "hello"}, "created_at": 1754800000000},
                {"id": "m2", "session_id": "s", "direction": "assistant",
                 "content": {"type": "text", "text": "hi"}, "created_at": 1754800001000},
                {"id": "m3", "session_id": "s", "direction": "system",
                 "content": {"type": "text", "text": "session resumed"}, "created_at": 1754800002000}
              ]
            }
            """,
        )
        val messages = detail.messages.mapIndexed { index, dto -> dto.project(index) }
        assertTrue(messages[0].fromUser)
        assertEquals("hello", messages[0].text)
        assertTrue(!messages[1].fromUser && !messages[1].system)
        assertTrue(messages[2].system)
    }

    /**
     * `created_at` is millis. Modelling it as a string failed the whole
     * response, not just the timestamp.
     */
    @Test
    fun `timestamps are numbers`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "user",
                           "content": {"type": "text", "text": "x"},
                           "created_at": 1754800000000}]}
            """,
        )
        assertEquals(1754800000000L, detail.messages.single().createdAt as Long)
    }

    @Test
    fun `a task status update becomes a task card, not a bubble`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{
               "id": "m", "session_id": "s", "direction": "system",
               "created_at": 1754800000000,
               "content": {
                 "type": "task_status_update",
                 "task_id": "task-7",
                 "status": "running",
                 "display_label": "Summarise the quarter",
                 "execution_id": "exec-3",
                 "ui_thread_id": "general",
                 "summary": "Reading the filings",
                 "output_files": [{"type": "file", "filename": "q3.pdf"}],
                 "synthesis_pending": false
               }}]}
            """,
        )
        val message = detail.messages.single().project(0)
        assertEquals(MessageKind.TaskStatus, message.kind)
        assertTrue(message.system)
        val task = message.task ?: error("expected a task card")
        assertEquals("Summarise the quarter", task.title)
        assertEquals("Running", task.verb())
        assertTrue(task.running)
        assertTrue(!task.terminal)
        assertEquals("q3.pdf", task.outputs.single().display())
        // The card is the content; leaving text behind would draw an empty
        // bubble beside it.
        assertEquals("", message.text)
    }

    /**
     * A terminal status that is still synthesizing says so, rather than
     * claiming to be finished while the result is still being written.
     */
    @Test
    fun `a synthesizing task says it is preparing the result`() {
        val task = TaskStatusCard(
            taskId = "t", title = "T", status = "completed", synthesisPending = true,
        )
        assertEquals("Preparing final result", task.verb())
        assertTrue(task.running)
    }

    @Test
    fun `a failed task is marked failed, not merely terminal`() {
        val task = TaskStatusCard(taskId = "t", title = "T", status = "error")
        assertEquals("Failed", task.verb())
        assertTrue(task.terminal)
        assertTrue(task.failed)
    }

    /** An unrecognised status is still shown, spelled like a sentence. */
    @Test
    fun `an unknown status is rendered rather than dropped`() {
        assertEquals(
            "Awaiting review",
            TaskStatusCard(taskId = "t", title = "T", status = "awaiting_review").verb(),
        )
    }

    @Test
    fun `an escalation becomes an answerable card`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{
               "id": "m", "session_id": "s", "direction": "assistant",
               "created_at": 1754800000000,
               "content": {
                 "type": "escalation",
                 "execution_id": "exec-1",
                 "pause_state_id": "pause-9",
                 "escalation_type": "confirmation",
                 "input_type": "confirmation",
                 "question": "Send the email now?",
                 "options": [
                   {"id": "yes", "label": "Send it"},
                   {"id": "no", "label": "Hold", "description": "Leave it in drafts"}
                 ]
               }}]}
            """,
        )
        val message = detail.messages.single().project(0)
        assertEquals(MessageKind.Escalation, message.kind)
        val card = message.escalation ?: error("expected an escalation card")
        assertEquals("Send the email now?", card.question)
        assertEquals(2, card.options.size)
        assertTrue(!card.resolved)
        // No correlation_id or request_id here, so the pause id is what an
        // answer has to be posted against.
        assertEquals("pause-9", card.correlationId)
    }

    /** A correlation id outranks the pause id when the backend sends both. */
    @Test
    fun `the correlation id is preferred for answering`() {
        val content = MessageContent(
            type = "escalation",
            correlationId = "corr-1",
            requestId = "req-1",
            pauseStateId = "pause-1",
        )
        assertEquals("corr-1", content.hitlCorrelationId())
    }

    /** Answered escalations stay in the transcript, marked as answered. */
    @Test
    fun `a resolved escalation keeps its question`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{
               "id": "m", "session_id": "s", "direction": "assistant",
               "created_at": 1,
               "content": {
                 "type": "escalation_resolved",
                 "execution_id": "e", "pause_state_id": "p",
                 "escalation_type": "confirmation",
                 "question": "Send the email now?",
                 "options": [{"id": "yes", "label": "Send it"}]
               }}]}
            """,
        )
        val card = detail.messages.single().project(0).escalation
            ?: error("expected an escalation card")
        assertTrue(card.resolved)
        assertEquals("Send the email now?", card.question)
    }

    /**
     * A choice-shaped pause with no options cannot be answered, so it falls
     * back to a plain row rather than drawing a card with no way out of it.
     */
    @Test
    fun `a confirmation without options is not drawn as a card`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "assistant", "created_at": 1,
               "content": {"type": "escalation", "execution_id": "e", "pause_state_id": "p",
                           "escalation_type": "confirmation", "input_type": "confirmation",
                           "question": "Apply this?", "options": []}}]}
            """,
        )
        val message = detail.messages.single().project(0)
        assertEquals(MessageKind.Text, message.kind)
        assertNull(message.escalation)
    }

    @Test
    fun `a form without options is still a card`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "assistant", "created_at": 1,
               "content": {"type": "escalation", "execution_id": "e", "pause_state_id": "p",
                           "escalation_type": "clarification", "input_type": "form",
                           "question": "A few things", "options": []}}]}
            """,
        )
        val card = detail.messages.single().project(0).escalation
            ?: error("expected an escalation card")
        assertEquals("form", card.inputType)
        assertEquals("A few things", card.question)
        assertTrue(card.options.isEmpty())
    }

    @Test
    fun `guidance without options is still a card`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "assistant", "created_at": 1,
               "content": {"type": "escalation", "execution_id": "e", "pause_state_id": "p",
                           "escalation_type": "guidance", "input_type": "guidance",
                           "question": "What now?", "options": []}}]}
            """,
        )
        val card = detail.messages.single().project(0).escalation
            ?: error("expected an escalation card")
        assertEquals("guidance", card.inputType)
    }

    @Test
    fun `an attachment shows its name and a readable size`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "user", "created_at": 1,
               "content": {"type": "attachment", "filename": "notes.pdf",
                           "mime_type": "application/pdf", "size": 2097152}}]}
            """,
        )
        val message = detail.messages.single().project(0)
        assertEquals(MessageKind.Attachment, message.kind)
        assertEquals("notes.pdf", message.attachment?.first)
        assertEquals("2.0 MB", message.attachment?.second)
    }

    @Test
    fun `a rich tool result is introduced by its summary and keeps its blocks`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "session_id": "s", "direction": "assistant", "created_at": 1,
               "content": {"type": "rich_tool_result", "tool_name": "web_search",
                           "summary": "Found three sources",
                           "content_blocks": [{"type": "text", "text": "first"},
                                              {"type": "file", "label": "report.md"}]}}]}
            """,
        )
        val message = detail.messages.single().project(0)
        assertEquals("Found three sources", message.text)
        assertEquals(listOf("first", "report.md"), message.outputs.map { it.display() })
    }

    /**
     * The backend adds content types faster than a client ships. An unknown one
     * must cost its own card, never the rest of the conversation.
     */
    @Test
    fun `an unknown content type still renders its text`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [
               {"id": "m1", "session_id": "s", "direction": "assistant", "created_at": 1,
                "content": {"type": "some_future_thing", "text": "still readable", "novel_field": 7}},
               {"id": "m2", "session_id": "s", "direction": "user", "created_at": 2,
                "content": {"type": "text", "text": "and this survives"}}
             ]}
            """,
        )
        val messages = detail.messages.mapIndexed { index, dto -> dto.project(index) }
        assertEquals(2, messages.size)
        assertEquals("still readable", messages[0].text)
        assertEquals("and this survives", messages[1].text)
    }

    /** Optional fields are genuinely optional — most turns carry almost none. */
    @Test
    fun `a minimal message parses`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "direction": "user", "content": {"type": "text", "text": "hi"}}]}
            """,
        )
        val dto = detail.messages.single()
        val message = dto.project(0)
        assertEquals("hi", message.text)
        assertTrue(message.fromUser)
        // Absent, not zero — an older message with no timestamp must not date
        // itself to 1970.
        assertNull(dto.createdAt)
    }

    /** A voice-originated turn is flagged on both halves of the exchange. */
    @Test
    fun `voice origin survives the projection`() {
        val detail = parse(
            """
            {"session": {"id": "s"},
             "messages": [{"id": "m", "direction": "user", "voice_origin": true,
                           "content": {"type": "text", "text": "remind me at six"}}]}
            """,
        )
        assertTrue(detail.messages.single().project(0).voiceOrigin)
    }

    /** Sessions list with millisecond timestamps, not strings. */
    @Test
    fun `the session list parses millisecond timestamps`() {
        val list = json.decodeFromString(
            SessionList.serializer(),
            """{"sessions": [{"id": "s1", "title": "Groceries",
                              "ui_thread_id": "general", "updated_at": 1754800500000}]}""",
        )
        val session = list.sessions.single()
        assertEquals(1754800500000L, session.updatedAt as Long)
        assertEquals("General", session.threadLabel())
    }

    /** A named thread shows its own name rather than the default label. */
    @Test
    fun `a named thread keeps its name`() {
        assertEquals("kitchen-rebuild", SessionSummary(uiThreadId = "kitchen-rebuild").threadLabel())
    }

    /** Creating a session answers with it wrapped, which is where the id is. */
    @Test
    fun `a created session is read through its envelope`() {
        val envelope = json.decodeFromString(
            ChatSessionEnvelope.serializer(),
            """{"session": {"id": "sess-new", "ui_thread_id": "general", "status": "active"}}""",
        )
        assertEquals("sess-new", envelope.session.identifier())
    }
}
