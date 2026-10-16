package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.realtime.HubEvent
import ai.magicbeans.magdroid.realtime.HubFrame
import ai.magicbeans.magdroid.realtime.parseHubFrame
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Which hub events belong to one chat turn. */
class ChatTurnLinkTest {

    private fun event(text: String): HubEvent = (parseHubFrame(text) as HubFrame.Event).event
    private fun json(text: String): JsonObject = chatJson.parseToJsonElement(text).jsonObject

    @Test
    fun `an event naming the turn belongs, one naming another turn does not`() {
        val link = ChatTurnLink("turn")
        assertTrue(link.owns(event("""{"event_type":"MessageCompleted","data":{"chat_turn_id":"turn"}}""")))
        assertFalse(link.owns(event("""{"event_type":"MessageCompleted","data":{"chat_turn_id":"other"}}""")))
        // Server-stamped refs count the same as the payload.
        assertTrue(link.owns(event("""{"event_type":"X","data":{},"refs":{"chat_turn_id":"turn"}}""")))
    }

    @Test
    fun `a run the turn named is followed without the turn id`() {
        val link = ChatTurnLink("turn")
        val unstamped = event("""{"event_type":"AgenticToolCall","data":{"execution_id":"e1"}}""")
        assertFalse(link.owns(unstamped))

        link.owns(event("""{"event_type":"V3PlanningStarted","data":{"chat_turn_id":"turn","task_id":"t1","execution_id":"e1"}}"""))
        assertTrue(link.owns(unstamped))
        assertTrue(link.owns(event("""{"event_type":"V3TaskUpdated","data":{"task_id":"t1"}}""")))
        // A run that names a different turn stays that turn's.
        assertFalse(link.owns(event("""{"event_type":"V3TaskUpdated","data":{"task_id":"t1","chat_turn_id":"other"}}""")))
    }

    @Test
    fun `an agentic action start rides the run like its completion`() {
        val link = ChatTurnLink("turn")
        link.owns(event("""{"event_type":"V3PlanningStarted","data":{"chat_turn_id":"turn","execution_id":"e1"}}"""))
        assertTrue(link.owns(event("""{"event_type":"AgenticActionStarted","data":{"execution_id":"e1","call_id":"c1"}}""")))
        assertTrue(link.owns(event("""{"event_type":"AgenticActionExecuted","data":{"execution_id":"e1","call_id":"c1"}}""")))
        assertFalse(link.owns(event("""{"event_type":"AgenticActionStarted","data":{"execution_id":"e2","call_id":"c1"}}""")))
    }

    @Test
    fun `backfilled events teach the link too`() {
        val link = ChatTurnLink("turn")
        link.learn(json("""{"event_type":"AgentEvent","data":{"event":{"event_type":"x","payload":{"chat_turn_id":"turn","task_id":"t9"}}}}"""))
        assertTrue(link.owns(event("""{"event_type":"ExecutionPanelDelta","data":{"task_id":"t9"}}""")))
    }
}
