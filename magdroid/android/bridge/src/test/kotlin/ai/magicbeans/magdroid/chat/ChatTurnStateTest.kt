package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A turn sent with `continue_on_disconnect` outlives the phone's stream; the
 * server's `turn_state` decides what its bubble shows after a drop or a relaunch.
 */
class ChatTurnStateTest {
    private val user = ChatMessage("user-1", true, "research this", chatTurnId = "turn-1", createdAtMs = 1_000)
    private val reply = ChatMessage("reply-1", false, "Here it is", chatTurnId = "turn-1")
    private val steps = listOf(ActivityRow(label = "Calling search", status = "running"))
    private val preview = liveAssistantPlaceholder("local-reply", "turn-1").copy(activityRows = steps)

    private fun state(status: ChatTurnStatus, reason: String? = null, turn: String = "turn-1") =
        ChatTurnState(turn, status, startedAtMs = 2_000, endedAtMs = 3_000, reason = reason)

    private fun body(raw: String) = chatJson.parseToJsonElement(raw).jsonObject

    // ── Wire ─────────────────────────────────────────────────────────────────

    @Test
    fun `the snapshot field is read as the server writes it`() {
        val report = ChatTurnStateReport.from(
            body(
                """{"messages":[],"turn_state":{"chat_turn_id":"turn-1","status":"stopped",
                "started_at":10,"ended_at":20,"reason":"turn_stalled"}}""",
            ),
            sessionId = "session-1",
        )
        assertTrue(report.supported)
        val parsed = report.state!!
        assertEquals("turn-1", parsed.chatTurnId)
        assertEquals(ChatTurnStatus.Stopped, parsed.status)
        assertEquals(10L, parsed.startedAtMs)
        assertEquals(20L, parsed.endedAtMs)
        assertEquals("session-1", parsed.sessionId)
        assertTrue(parsed.stoppedByWatchdog)
        assertFalse(parsed.stoppedByUser)
    }

    @Test
    fun `null means no turn known, absence means an older server`() {
        val none = ChatTurnStateReport.from(body("""{"messages":[],"turn_state":null}"""), "s")
        assertTrue(none.supported)
        assertNull(none.state)

        val old = ChatTurnStateReport.from(body("""{"messages":[]}"""), "s")
        assertFalse(old.supported)
    }

    @Test
    fun `a malformed or unknown state is no state`() {
        assertNull(parseChatTurnState(body("""{"chat_turn_id":"t","status":"paused"}""")))
        assertNull(parseChatTurnState(body("""{"status":"running"}""")))
        val stop = parseChatTurnState(body("""{"chat_turn_id":"t","status":"stopped","started_at":1}"""))!!
        assertTrue("A stop without a reason is the owner's", stop.stoppedByUser)
    }

    // ── Running ──────────────────────────────────────────────────────────────

    @Test
    fun `a running turn reopened shows a live bubble after its message`() {
        val earlier = ChatMessage("old", false, "Earlier", chatTurnId = "turn-0")
        val messages = applyChatTurnState(listOf(earlier, user), state(ChatTurnStatus.Running))

        assertEquals(listOf("old", "user-1", "live-turn:turn-1"), messages.map { it.id })
        val live = messages.last()
        assertTrue(live.streaming)
        assertTrue(live.optimistic)
        assertEquals("turn-1", live.chatTurnId)
        assertEquals(2_000L, live.createdAtMs)
        // Replaying the state (event, then snapshot) changes nothing.
        assertEquals(messages, applyChatTurnState(messages, state(ChatTurnStatus.Running)))
    }

    @Test
    fun `a dropped stream's failed bubble goes live again while the server runs it`() {
        val failed = preview.copy(streaming = false, failed = true, text = "Connection reset")
        val messages = applyChatTurnState(listOf(user, failed), state(ChatTurnStatus.Running))

        val live = messages.single { !it.fromUser }
        assertTrue(live.streaming)
        assertFalse(live.failed)
        assertEquals("", live.text)
        assertEquals(steps, live.activityRows)
    }

    @Test
    fun `running never mounts a second bubble beside a persisted reply`() {
        val messages = listOf(user, reply)
        assertEquals(messages, applyChatTurnState(messages, state(ChatTurnStatus.Running)))
    }

    // ── Completed ────────────────────────────────────────────────────────────

    @Test
    fun `the reply replaces a bubble a clean stream drop left behind`() {
        // A socket that closes without `done` leaves the placeholder neither
        // streaming nor failed. The reply must replace it, not land under it.
        val dropped = preview.copy(streaming = false)
        val messages = mergeRealtimeMessage(listOf(user, dropped), reply)

        assertEquals(listOf("user-1", "reply-1"), messages.map { it.id })
        assertEquals(steps, messages.last().activityRows)
        assertFalse(messages.last().optimistic)
    }

    @Test
    fun `a completed turn keeps its reply and drops leftover previews`() {
        val messages = applyChatTurnState(listOf(user, reply, preview), state(ChatTurnStatus.Completed))
        assertEquals(listOf("user-1", "reply-1"), messages.map { it.id })
        assertTrue(hasServerReply(messages, "turn-1"))
    }

    @Test
    fun `a completed turn whose reply is still on its way stops looking live`() {
        val messages = applyChatTurnState(listOf(user, preview), state(ChatTurnStatus.Completed))
        assertFalse(messages.last().streaming)
        assertFalse(hasServerReply(messages, "turn-1"))
    }

    // ── Stopped ──────────────────────────────────────────────────────────────

    @Test
    fun `a turn the owner stopped reads Stopped and keeps its steps`() {
        val messages = applyChatTurnState(
            listOf(user, preview), state(ChatTurnStatus.Stopped, ChatTurnState.USER_STOP_REASON),
        )

        assertEquals(listOf("user-1", turnNoticeId("turn-1")), messages.map { it.id })
        val line = messages.last()
        assertEquals(TURN_STOPPED_NOTICE, line.text)
        assertTrue(line.system)
        assertFalse(line.streaming)
        assertEquals(steps, line.activityRows)
        assertEquals(messages, applyChatTurnState(messages, state(ChatTurnStatus.Stopped)))
    }

    @Test
    fun `a stopped partial answer keeps its words and the line follows`() {
        val partial = preview.copy(text = "Half an ans")
        val messages = applyChatTurnState(listOf(user, partial), state(ChatTurnStatus.Stopped))

        assertEquals(listOf("user-1", "local-reply", turnNoticeId("turn-1")), messages.map { it.id })
        assertEquals("Half an ans", messages[1].text)
        assertFalse(messages[1].streaming)
        // The answer, not the line, keeps the steps.
        val rows = listOf(ActivityRow(label = "Done", status = "done"))
        val owned = ChatUiState(messages = messages).withTurnActivity("turn-1", rows).messages
        assertEquals(rows, owned[1].activityRows)
        assertEquals(emptyList<ActivityRow>(), owned[2].activityRows)
    }

    @Test
    fun `a stopped turn reloaded with no bubble gets its line after the message`() {
        val messages = applyChatTurnState(listOf(user), state(ChatTurnStatus.Stopped))
        assertEquals(listOf("user-1", turnNoticeId("turn-1")), messages.map { it.id })
    }

    @Test
    fun `a watchdog stop is told by its persisted notice, not a line`() {
        val notice = ChatMessage("notice-1", false, "This turn stalled and was stopped.", chatTurnId = "turn-1")
        val messages = applyChatTurnState(
            listOf(user, preview, notice), state(ChatTurnStatus.Stopped, "turn_stalled"),
        )
        assertEquals(listOf("user-1", "notice-1"), messages.map { it.id })

        // Before the notice arrives nothing is invented; the bubble just stops.
        val waiting = applyChatTurnState(listOf(user, preview), state(ChatTurnStatus.Stopped, "turn_wall_clock_exceeded"))
        assertEquals(listOf("user-1", "local-reply"), waiting.map { it.id })
        assertFalse(waiting.last().streaming)
    }

    @Test
    fun `a reply that beat the Stop replaces the Stopped line`() {
        val stopped = applyChatTurnState(listOf(user, preview), state(ChatTurnStatus.Stopped))
        val answered = applyChatTurnState(mergeRealtimeMessage(stopped, reply), state(ChatTurnStatus.Completed))
        assertEquals(listOf("user-1", "reply-1"), answered.map { it.id })
    }

    // ── Failed and interrupted ───────────────────────────────────────────────

    @Test
    fun `a failed turn shows a failed line`() {
        val messages = applyChatTurnState(listOf(user, preview), state(ChatTurnStatus.Failed, "error"))
        val line = messages.last()
        assertEquals(TURN_FAILED_NOTICE, line.text)
        assertTrue(line.failed)
        assertFalse(line.system)

        val interrupted = applyChatTurnState(listOf(user, preview), state(ChatTurnStatus.Failed, "interrupted"))
        assertEquals(TURN_INTERRUPTED_NOTICE, interrupted.last().text)
    }

    @Test
    fun `a failure the stream already worded keeps its words`() {
        val worded = preview.copy(streaming = false, failed = true, text = "Your input exceeds the context window")
        val messages = applyChatTurnState(listOf(user, worded), state(ChatTurnStatus.Failed, "error"))
        assertEquals(listOf("user-1", "local-reply"), messages.map { it.id })
        assertEquals("Your input exceeds the context window", messages.last().text)
    }

    @Test
    fun `after a server restart the unanswered latest turn reads interrupted`() {
        val messages = markInterruptedTurn(listOf(reply.copy(id = "r0", chatTurnId = "turn-0"), user))
        val line = messages.last()
        assertEquals(turnNoticeId("turn-1"), line.id)
        assertEquals(TURN_INTERRUPTED_NOTICE, line.text)
        assertTrue(line.failed)
        assertEquals(messages, markInterruptedTurn(messages))

        // A late reply still replaces the line.
        val answered = mergeRealtimeMessage(messages, reply)
        assertEquals(listOf("r0", "user-1", "reply-1"), answered.map { it.id })
    }

    @Test
    fun `after a restart a live bubble with nothing in it is not left spinning`() {
        val messages = markInterruptedTurn(listOf(user, preview))
        assertEquals(listOf("user-1", turnNoticeId("turn-1")), messages.map { it.id })
        assertFalse(messages.any { it.streaming })
    }

    @Test
    fun `answered, card-answered, voice and unsent turns are not interrupted`() {
        val answered = listOf(user, reply)
        assertEquals(answered, markInterruptedTurn(answered))

        val card = ChatMessage(
            "task", false, "", kind = MessageKind.TaskStatus,
            task = TaskStatusCard(taskId = "t", title = "Research", status = "running"),
        )
        assertEquals(listOf(user, card), markInterruptedTurn(listOf(user, card)))

        val voice = user.copy(chatTurnId = "voice-turn-1")
        assertEquals(listOf(voice), markInterruptedTurn(listOf(voice)))

        val echo = optimisticUserMessage("echo", "hi", false, "turn-2")
        assertEquals(listOf(echo), markInterruptedTurn(listOf(echo)))
    }

    @Test
    fun `realtime voice turns are left to the call surface`() {
        assertTrue(isRealtimeVoiceTurn("voice-turn-abc"))
        assertTrue(isRealtimeVoiceTurn("voice-request-abc"))
        assertFalse(isRealtimeVoiceTurn("2b1c-uuid"))
    }
}
