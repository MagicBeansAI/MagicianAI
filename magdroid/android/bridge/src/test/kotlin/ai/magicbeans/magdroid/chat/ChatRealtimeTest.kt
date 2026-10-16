package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Reading the shared realtime bus.
 *
 * Every surface publishes onto `/realtime/ws`, so most frames arriving here
 * belong to something else. Returning null for them is the normal case; the
 * tests that matter are the ones where a wrong answer would either show another
 * principal's message or silently drop our own.
 */
class ChatRealtimeTest {

    private fun parse(raw: String) = parseChatRealtimeEvent(raw, "anonymous", "default")

    // ── Scope ────────────────────────────────────────────────────────────────

    /** Showing another principal's message would be a data leak, not a bug. */
    @Test
    fun `an event for another principal is not ours`() {
        val raw = """{"event_type":"MessageCompleted",
            "data":{"principal":"someone-else","workspace":"default","chat_turn_id":"t1"}}"""
        assertNull(parse(raw))
    }

    @Test
    fun `an event for another workspace is not ours`() {
        val raw = """{"event_type":"MessageCompleted",
            "data":{"principal":"anonymous","workspace":"other","chat_turn_id":"t1"}}"""
        assertNull(parse(raw))
    }

    /**
     * An event that names nobody predates the scope fields.
     *
     * Dropping it would make this client go quiet against an older backend,
     * which is a worse failure than showing an event that was probably ours.
     */
    @Test
    fun `an event with no scope is allowed through`() {
        val raw = """{"event_type":"MessageCompleted","data":{"chat_turn_id":"t1"}}"""
        val event = parse(raw)
        assertTrue(event is ChatRealtimeEvent.TurnCompleted)
    }

    // ── Frames that are not ours ─────────────────────────────────────────────

    @Test
    fun `unrelated events and junk are ignored`() {
        assertNull(parse("""{"event_type":"TaskCreated","data":{}}"""))
        assertNull(parse("""{"event_type":"ShellOutputChunk","data":{"text":"hi"}}"""))
        assertNull(parse("not json"))
        assertNull(parse("""{"data":{}}"""))
        // Retired in H6.3/H7.3 — the canonical HitlRequested replaced it, and
        // reviving a listener for it would resurrect a dead flow.
        assertNull(parse("""{"event_type":"V3PlanningClarificationNeeded","data":{}}"""))
    }

    // ── Messages ─────────────────────────────────────────────────────────────

    @Test
    fun `a message from another surface projects into the transcript`() {
        val raw = """{"event_type":"ChatMessageReceived","data":{
            "session_id":"s1",
            "message":{"id":"m1","direction":"assistant","chat_turn_id":"t9",
                       "content":{"type":"text","text":"from the web"}}}}"""
        val event = parse(raw) as ChatRealtimeEvent.MessageReceived
        assertEquals("s1", event.sessionId)
        assertEquals("m1", event.message.id)
        assertEquals("from the web", event.message.content.text)
        assertEquals("t9", event.message.chatTurnId)
    }

    @Test
    fun `a message that will not decode is dropped, not thrown`() {
        val raw = """{"event_type":"ChatMessageReceived","data":{"message":"not-an-object"}}"""
        assertNull(parse(raw))
    }

    @Test
    fun `the session falls back to the message when the envelope omits it`() {
        val raw = """{"event_type":"ChatMessageReceived","data":{
            "message":{"id":"m2","session_id":"s2","direction":"user",
                       "content":{"type":"text","text":"hi"}}}}"""
        val event = parse(raw) as ChatRealtimeEvent.MessageReceived
        assertEquals("s2", event.sessionId)
    }

    // ── Turn completion ──────────────────────────────────────────────────────

    @Test
    fun `completion accepts either turn id spelling`() {
        val a = parse("""{"event_type":"MessageCompleted","data":{"chat_turn_id":"t1"}}""")
        assertEquals("t1", (a as ChatRealtimeEvent.TurnCompleted).chatTurnId)
        // Older emitters used `turn_id`; both are the same fact.
        val b = parse("""{"event_type":"MessageCompleted","data":{"turn_id":"t2"}}""")
        assertEquals("t2", (b as ChatRealtimeEvent.TurnCompleted).chatTurnId)
    }

    // ── Activity ─────────────────────────────────────────────────────────────

    /**
     * The frame carries `{principal, workspace, task_id?, execution_id?,
     * state, timestamp}` and nothing else. An earlier arm read `rows` /
     * `activity_rows` off it and could therefore never fire — the "What
     * happened" section went unfed for its whole life. Rows come from the
     * canonical turn-events endpoint; this pins that the parser no longer
     * pretends otherwise.
     */
    @Test
    fun `a panel delta is not a chat event — rows never rode it`() {
        val raw = """{"event_type":"ExecutionPanelDelta","data":{
            "principal":"anonymous","workspace":"default","task_id":"t1",
            "state":{"overview":{"task_id":"t1","status":"running"}},"timestamp":1}}"""
        assertNull(parse(raw))
    }

    @Test
    fun `planning started names its task`() {
        val raw = """{"event_type":"V3PlanningStarted",
            "data":{"task_id":"task-1","task_title":"Book a table"}}"""
        val event = parse(raw) as ChatRealtimeEvent.PlanningStarted
        assertEquals("task-1", event.taskId)
        assertEquals("Book a table", event.title)
    }

    // ── Shell output ─────────────────────────────────────────────────────────

    /**
     * Parsed from the wire's real shape: a `data` string of newline-joined
     * output. iOS's chat decoder expects a `lines` array here and has never
     * received one — the mismatch is recorded in the parity register, and this
     * parser is written against the frame the backend actually sends.
     */
    @Test
    fun `a shell chunk splits its batched lines like the backend does`() {
        val raw = """{"event_type":"ShellOutputChunk","data":{
            "execution_id":"exec-1","step_id":"step-1","step_index":0,
            "command":"cargo build","stream":"stdout",
            "data":"Compiling magician\n\n   done\n","sequence":0,
            "is_final":false,"timestamp":1}}"""
        val event = parse(raw) as ChatRealtimeEvent.ShellOutput
        assertEquals("exec-1", event.executionId)
        // Interior blank kept (real output); trailing-newline artifact dropped.
        assertEquals(listOf("Compiling magician", "", "   done"), event.lines)
        assertTrue(!event.isFinal)
    }

    @Test
    fun `an empty chunk is not an event unless it is the final one`() {
        val empty = """{"event_type":"ShellOutputChunk","data":{
            "execution_id":"exec-1","data":"","sequence":3,"is_final":false}}"""
        assertNull(parse(empty))
        val final = """{"event_type":"ShellOutputChunk","data":{
            "execution_id":"exec-1","data":"","sequence":4,"is_final":true,"exit_code":0}}"""
        val event = parse(final) as ChatRealtimeEvent.ShellOutput
        assertTrue(event.isFinal)
        assertTrue(event.lines.isEmpty())
    }

    @Test
    fun `a shell chunk for another principal is not ours`() {
        val raw = """{"event_type":"ShellOutputChunk","data":{
            "execution_id":"exec-1","principal":"someone-else","data":"x"}}"""
        assertNull(parse(raw))
    }

    // ── Routing a chunk to its card ──────────────────────────────────────────

    private fun taskMessage(id: String, executionId: String?) = ChatMessage(
        id = id, fromUser = false, text = "",
        kind = MessageKind.TaskStatus,
        task = TaskStatusCard(
            taskId = "task-$id", title = "t", status = "running",
            executionId = executionId,
        ),
    )

    @Test
    fun `a chunk lands on the card that owns its execution`() {
        val messages = listOf(
            taskMessage("a", executionId = "exec-1"),
            taskMessage("b", executionId = "exec-2"),
        )
        assertEquals(0, shellTargetIndex(messages, "exec-1"))
        assertEquals(1, shellTargetIndex(messages, "exec-2"))
    }

    /**
     * The first chunks of a run can arrive before the card has learned its
     * execution id — iOS falls back to the newest task card, and so does this.
     */
    @Test
    fun `an unclaimed chunk falls back to the newest task card`() {
        val messages = listOf(
            taskMessage("a", executionId = null),
            ChatMessage(id = "text", fromUser = false, text = "an answer"),
            taskMessage("b", executionId = null),
        )
        assertEquals(2, shellTargetIndex(messages, "exec-9"))
    }

    @Test
    fun `a transcript with no task card drops the chunk`() {
        val messages = listOf(ChatMessage(id = "text", fromUser = true, text = "hi"))
        assertEquals(-1, shellTargetIndex(messages, "exec-1"))
    }
}
