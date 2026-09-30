package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The SSE frames `chat_api.rs` emits, verbatim.
 *
 * The event names are a closed vocabulary on the server — `token`,
 * `tool_call`, `error`, `reasoning_*`, `tool_call_*`, `done` — so these
 * fixtures are copied from the `format!` calls that build them. The client
 * previously listened for `structured_response` and `activity`, which the
 * server has never sent, and treated `done` as a bare signal when it carries
 * the entire turn.
 */
class ChatStreamFrameTest {

    @Test
    fun `a token frame carries its text`() {
        val event = decodeStreamFrame("token", """{"text": "Hello"}""")
        assertEquals("Hello", (event as ChatStreamEvent.Token).text)
    }

    /** Reasoning arrives under `delta`, not `text`. */
    @Test
    fun `a reasoning delta reads the delta field`() {
        val event = decodeStreamFrame("reasoning_delta", """{"index": 0, "delta": "thinking"}""")
        assertEquals("thinking", (event as ChatStreamEvent.ReasoningDelta).text)
    }

    @Test
    fun `a tool call names the tool`() {
        val event = decodeStreamFrame(
            "tool_call",
            """{"id": "c1", "name": "web_search", "arguments_chunk": "{", "status": "executing"}""",
        )
        assertEquals("web_search", (event as ChatStreamEvent.ToolCall).name)
    }

    /** The finer-grained lifecycle event carries the same fact under another key. */
    @Test
    fun `tool call start names the tool too`() {
        val event = decodeStreamFrame("tool_call_start", """{"call_id": "c1", "tool_name": "read_file"}""")
        assertEquals("read_file", (event as ChatStreamEvent.ToolCall).name)
    }

    /** An unnamed tool is still worth announcing — the pause has a cause. */
    @Test
    fun `a nameless tool call still announces itself`() {
        val event = decodeStreamFrame("tool_call", """{"id": "c1"}""")
        assertEquals("a tool", (event as ChatStreamEvent.ToolCall).name)
    }

    @Test
    fun `an error frame reads the error field`() {
        val event = decodeStreamFrame("error", """{"error": "the provider refused"}""")
        assertEquals("the provider refused", (event as ChatStreamEvent.Failed).message)
    }

    /**
     * Events this build has no use for must be ignored, not treated as the end
     * of the turn — the server adds lifecycle deltas independently of clients.
     */
    @Test
    fun `unknown and unused events are ignored`() {
        for (name in listOf("reasoning_start", "tool_call_args_delta", "tool_call_end", "some_future_event")) {
            assertTrue("$name should be ignored", decodeStreamFrame(name, "{}") is ChatStreamEvent.Ignored)
        }
    }

    /**
     * The whole point of `done`: it carries the persisted turn, including the
     * composed presentation the token stream cannot express.
     */
    @Test
    fun `done carries the settled turn`() {
        val event = decodeStreamFrame(
            "done",
            """
            {
              "assistant_message": {
                "id": "a1", "session_id": "s", "direction": "assistant",
                "created_at": 1754800000000,
                "content": {"type": "text", "text": "Here is the summary"},
                "presentation": {"title": "Q3", "blocks": []}
              },
              "messages": [
                {"id": "a1", "session_id": "s", "direction": "assistant", "created_at": 1754800000000,
                 "content": {"type": "text", "text": "Here is the summary"},
                 "presentation": {"title": "Q3", "blocks": []}},
                {"id": "a2", "session_id": "s", "direction": "assistant", "created_at": 1754800000001,
                 "content": {"type": "task_status_update", "task_id": "t1", "status": "running",
                             "display_label": "Crunch the numbers"}}
              ],
              "session_title": "Quarterly review",
              "cancelled": false
            }
            """,
        ) as ChatStreamEvent.Done

        assertEquals(2, event.messages.size)
        assertEquals("Here is the summary", event.messages[0].text)
        assertEquals("Q3", event.messages[0].structured?.title)
        // The task card the turn produced arrives live, not only after a reload.
        assertEquals(MessageKind.TaskStatus, event.messages[1].kind)
        assertEquals("Crunch the numbers", event.messages[1].task?.title)
        assertEquals("Quarterly review", event.sessionTitle)
        assertFalse(event.cancelled)
    }

    /** Older turns send only the assistant message; it is still the answer. */
    @Test
    fun `done falls back to the assistant message alone`() {
        val event = decodeStreamFrame(
            "done",
            """
            {"assistant_message": {"id": "a1", "direction": "assistant",
              "content": {"type": "text", "text": "Just this"}}}
            """,
        ) as ChatStreamEvent.Done
        assertEquals(listOf("Just this"), event.messages.map { it.text })
    }

    /** The reply appears in both fields; it must not be shown twice. */
    @Test
    fun `done does not duplicate the reply`() {
        val event = decodeStreamFrame(
            "done",
            """
            {"assistant_message": {"id": "a1", "direction": "assistant",
              "content": {"type": "text", "text": "Once"}},
             "messages": [{"id": "a1", "direction": "assistant",
              "content": {"type": "text", "text": "Once"}}]}
            """,
        ) as ChatStreamEvent.Done
        assertEquals(1, event.messages.size)
    }

    /** A queued send never ran, and says where it landed. */
    @Test
    fun `a queued turn reports its position`() {
        val event = decodeStreamFrame(
            "done",
            """{"queued": {"id": "q1", "position": 2}, "messages": []}""",
        ) as ChatStreamEvent.Done
        assertEquals(2, event.queuedPosition)
        assertTrue(event.messages.isEmpty())
    }

    @Test
    fun `a cancelled turn is marked cancelled`() {
        val event = decodeStreamFrame("done", """{"cancelled": true}""") as ChatStreamEvent.Done
        assertTrue(event.cancelled)
    }

    /**
     * A `done` this build cannot read still ends the turn. Leaving the bubble
     * streaming forever is worse than losing the final payload.
     */
    @Test
    fun `a malformed done still ends the turn`() {
        val event = decodeStreamFrame("done", "not json at all")
        assertTrue(event is ChatStreamEvent.Done)
        assertTrue((event as ChatStreamEvent.Done).messages.isEmpty())
    }

    /** An empty `done` is normal for a turn with nothing to reconcile. */
    @Test
    fun `an empty done is harmless`() {
        val event = decodeStreamFrame("done", "{}") as ChatStreamEvent.Done
        assertTrue(event.messages.isEmpty())
        assertEquals(null, event.sessionTitle)
        assertEquals(null, event.queuedPosition)
    }

    /** A blank generated title is not a title. */
    @Test
    fun `a blank session title is dropped`() {
        val event = decodeStreamFrame("done", """{"session_title": "   "}""") as ChatStreamEvent.Done
        assertEquals(null, event.sessionTitle)
    }

    /** The send body is serialized, so quotes in a profile name cannot break it. */
    @Test
    fun `the send body escapes what it carries`() {
        val body = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(
                text = """he said "hello" \ then left""",
                chatTurnId = "turn-1",
                profile = """odd"name""",
                attachmentIds = listOf("a1", "a2"),
            ),
        )
        // The constant fields must be *present*, not merely defaulted back on
        // decode — that is what made them invisible in the first place.
        assertTrue("source_surface missing", body.contains("\"source_surface\""))
        assertTrue("chat_turn_id missing", body.contains("\"chat_turn_id\""))
        // Round-trips, which hand-built JSON would not have.
        val parsed = chatJson.decodeFromString(SendMessageRequest.serializer(), body)
        assertEquals("""he said "hello" \ then left""", parsed.text)
        assertEquals("""odd"name""", parsed.profile)
        assertEquals(listOf("a1", "a2"), parsed.attachmentIds)
        assertEquals("android", parsed.sourceSurface)
        assertEquals("turn-1", parsed.chatTurnId)
    }

    /**
     * A spoken turn says so on the wire.
     *
     * `turnFromVoice` was tracked in view state from the day dictation landed
     * and never reached the request, so every dictated turn arrived at the
     * backend looking typed — and a reply that could have been spoken back was
     * not. The assertion is on the serialized key, because a default that
     * decodes correctly can still be absent from what was sent.
     */
    @Test
    fun `a dictated turn is stamped voice_origin`() {
        val spoken = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "hi", chatTurnId = "t", voiceOrigin = true),
        )
        assertTrue("voice_origin missing", spoken.contains("\"voice_origin\":true"))
        assertTrue(chatJson.decodeFromString(SendMessageRequest.serializer(), spoken).voiceOrigin)

        val typed = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "hi", chatTurnId = "t"),
        )
        assertFalse(chatJson.decodeFromString(SendMessageRequest.serializer(), typed).voiceOrigin)
    }

    @Test
    fun `a durable mobile turn asks the server to survive disconnect`() {
        val durable = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(
                text = "finish after process loss",
                chatTurnId = "turn-durable",
                continueOnDisconnect = true,
            ),
        )

        assertTrue(
            "continue_on_disconnect missing",
            durable.contains("\"continue_on_disconnect\":true"),
        )
        assertTrue(
            chatJson.decodeFromString(SendMessageRequest.serializer(), durable)
                .continueOnDisconnect,
        )
    }

    @Test
    fun `a selected chat harness and profile travel together`() {
        val request = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(
                text = "hello",
                chatTurnId = "turn-pi",
                profile = "frontier",
                harnessEngine = "pi",
                harnessModel = "default",
            ),
        )
        assertTrue(request.contains("\"harness_engine\":\"pi\""))
        assertTrue(request.contains("\"harness_model\":\"default\""))
        assertTrue(request.contains("\"profile\":\"frontier\""))
    }

    @Test
    fun `plan and accept modes are named on the wire`() {
        val plan = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "hi", chatTurnId = "t", mode = ChatComposerMode.Plan.wire),
        )
        assertTrue("plan missing", plan.contains("\"mode\":\"plan\""))
        val accept = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "hi", chatTurnId = "t", mode = ChatComposerMode.AcceptInScope.wire),
        )
        assertTrue("accept_in_scope missing", accept.contains("\"mode\":\"accept_in_scope\""))
        val ask = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "hi", chatTurnId = "t"),
        )
        assertFalse("ask must be omitted", ask.contains("\"mode\""))
    }

    @Test
    fun `do remembers accept while plan is active`() {
        var state = ChatUiState()
        assertEquals(ChatComposerMode.Ask, state.composerMode)
        assertEquals(ChatDoPermission.Ask, state.composerDoPermission)

        state = state.withComposerMode(ChatComposerMode.AcceptInScope)
        assertEquals(ChatDoPermission.AcceptInScope, state.composerDoPermission)

        state = state.withComposerMode(ChatComposerMode.Plan)
        assertEquals(ChatComposerMode.Plan, state.composerMode)
        assertEquals(ChatDoPermission.AcceptInScope, state.composerDoPermission)

        state = state.withComposerMode(state.composerDoPermission.mode)
        assertEquals(ChatComposerMode.AcceptInScope, state.composerMode)
    }

    @Test
    fun `a live response owns canonical steps before it has text`() {
        val placeholder = liveAssistantPlaceholder(replyId = "reply-1", chatTurnId = "turn-1")
        assertTrue(placeholder.streaming)
        assertEquals("", placeholder.text)
        assertEquals("turn-1", placeholder.chatTurnId)

        val user = optimisticUserMessage(id = "user-1", text = "research this", voiceOrigin = false)
        val otherTurn = ChatMessage(
            id = "reply-0", fromUser = false, text = "Earlier", chatTurnId = "turn-0",
        )
        val rows = listOf(ActivityRow(label = "Running step: research", status = "running"))
        val projected = ChatUiState(messages = listOf(otherTurn, user, placeholder))
            .withTurnActivity(chatTurnId = "turn-1", rows = rows)

        assertEquals(emptyList<ActivityRow>(), projected.messages[0].activityRows)
        assertEquals(emptyList<ActivityRow>(), projected.messages[1].activityRows)
        assertEquals(rows, projected.messages[2].activityRows)
        assertTrue(projected.messages[2].streaming)
        assertEquals("", projected.messages[2].text)
    }

    @Test
    fun `only the latest text response owns a turns steps`() {
        val earlier = ChatMessage(
            id = "earlier", fromUser = false, text = "Starting", chatTurnId = "turn-1",
        )
        val task = ChatMessage(
            id = "task", fromUser = false, text = "", chatTurnId = "turn-1",
            kind = MessageKind.TaskStatus,
            task = TaskStatusCard(taskId = "task-1", title = "Research", status = "completed"),
        )
        val answer = ChatMessage(
            id = "answer", fromUser = false, text = "Finished", chatTurnId = "turn-1",
        )
        val rows = listOf(ActivityRow(label = "Opened the site", status = "completed"))

        val projected = ChatUiState(messages = listOf(earlier, task, answer))
            .withTurnActivity(chatTurnId = "turn-1", rows = rows)

        assertTrue(projected.messages[0].activityRows.isEmpty())
        assertTrue(projected.messages[1].activityRows.isEmpty())
        assertEquals(rows, projected.messages[2].activityRows)
    }
}
