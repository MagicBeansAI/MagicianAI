package ai.magicbeans.magdroid.realtime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Frames as both servers send them: the current one (no `seq`, no `refs`)
 * and the hub-aware one (stamped, plus `hub.*` control frames).
 */
class HubFramesTest {

    private fun event(text: String): HubEvent =
        (parseHubFrame(text) as HubFrame.Event).event

    @Test
    fun `a legacy frame without seq still parses as an event`() {
        val parsed = event(
            """{"event_type":"ExecutionPanelDelta","data":{"task_id":"t1","execution_id":"e1","state":{}}}""",
        )
        assertEquals("ExecutionPanelDelta", parsed.eventType)
        assertNull(parsed.seq)
        assertNull(parsed.epoch)
        assertNull(parsed.categories)
        assertNull(parsed.userRelevant)
        assertEquals("t1", parsed.refs.taskId)
        assertEquals("e1", parsed.refs.executionId)
    }

    @Test
    fun `a stamped frame carries seq, epoch, categories, relevance and refs`() {
        val parsed = event(
            """{"event_type":"MessageCompleted","data":{"session_id":"payload-session"},""" +
                """"seq":42,"epoch":"boot-1","categories":["chat","task"],"user_relevant":true,""" +
                """"refs":{"chat_turn_id":"turn-1","session_id":"s1"}}""",
        )
        assertEquals(42L, parsed.seq)
        assertEquals("boot-1", parsed.epoch)
        assertEquals(listOf("chat", "task"), parsed.categories)
        assertEquals(true, parsed.userRelevant)
        assertEquals("turn-1", parsed.refs.chatTurnId)
        // Server refs win over the payload.
        assertEquals("s1", parsed.refs.sessionId)
    }

    @Test
    fun `refs fall back to the AgentEvent payload and metadata`() {
        val parsed = event(
            """{"event_type":"AgentEvent","data":{"event":{"event_type":"agentic.step","agent_id":"a1",""" +
                """"payload":{"chat_turn_id":"turn-9","task_id":"t9"}}}}""",
        )
        assertEquals("turn-9", parsed.refs.chatTurnId)
        assertEquals("t9", parsed.refs.taskId)
        assertEquals("a1", parsed.refs.agentId)

        val nested = event(
            """{"event_type":"ProgressEvent","data":{"message":{"metadata":{"chat_turn_id":"turn-m"}}}}""",
        )
        assertEquals("turn-m", nested.refs.chatTurnId)

        val deep = event(
            """{"event_type":"Custom","data":{"blob":{"inner":[{"chat_turn_id":"turn-deep"}]}}}""",
        )
        assertEquals("turn-deep", deep.refs.chatTurnId)
    }

    /** The tutor bug: the frame's own event_type is always AgentEvent. */
    @Test
    fun `an AgentEvent is unwrapped for subscribers`() {
        val parsed = event(
            """{"event_type":"AgentEvent","data":{"event":{"event_type":"tutor.draw.shape",""" +
                """"payload":{"shape":{"type":"rect"}}}}}""",
        )
        assertEquals("AgentEvent", parsed.eventType)
        assertEquals("tutor.draw.shape", parsed.agentEventType)
        assertEquals(parsed.agentEvent, parsed.unwrapped)
        assertTrue(parsed.unwrapped.containsKey("payload"))
    }

    @Test
    fun `a non AgentEvent is its own unwrapped form`() {
        val parsed = event("""{"event_type":"ThinkingMapUpdated","data":{"map_id":"m"}}""")
        assertNull(parsed.agentEvent)
        assertEquals(parsed.root, parsed.unwrapped)
    }

    @Test
    fun `control frames use type, never event_type`() {
        assertEquals(
            HubFrame.Hello(epoch = "boot-1", seq = 7, resumed = true),
            parseHubFrame("""{"type":"hub.hello","epoch":"boot-1","seq":7,"resumed":true}"""),
        )
        assertEquals(HubFrame.Resync("lagged"), parseHubFrame("""{"type":"hub.resync","reason":"lagged"}"""))
        assertEquals(HubFrame.Control("ui.interaction.ack"), parseHubFrame("""{"type":"ui.interaction.ack"}"""))
        assertNull(parseHubFrame("not json"))
        assertNull(parseHubFrame("""["array"]"""))
        assertNull(parseHubFrame("""{"neither":true}"""))
    }

    @Test
    fun `an event whose body has a type field is still an event`() {
        val frame = parseHubFrame("""{"event_type":"HitlRequested","type":"x","data":{}}""")
        assertTrue(frame is HubFrame.Event)
    }
}
