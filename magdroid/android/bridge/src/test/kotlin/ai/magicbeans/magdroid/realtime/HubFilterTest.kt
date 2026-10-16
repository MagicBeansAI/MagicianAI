package ai.magicbeans.magdroid.realtime

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HubFilterTest {

    private fun event(text: String): HubEvent = (parseHubFrame(text) as HubFrame.Event).event

    private val tutor = event(
        """{"event_type":"AgentEvent","data":{"event":{"event_type":"tutor.draw.shape","payload":{}}}}""",
    )
    private val legacyTask = event(
        """{"event_type":"V3TaskUpdated","data":{"task_id":"t1","execution_id":"e1"}}""",
    )
    private val stamped = event(
        """{"event_type":"MessageCompleted","data":{},"seq":1,"epoch":"b",""" +
            """"categories":["chat"],"user_relevant":false,"refs":{"chat_turn_id":"turn-1"}}""",
    )

    @Test
    fun `an empty filter matches everything`() {
        assertTrue(HubFilter.ALL.matches(tutor))
        assertTrue(HubFilter.ALL.matches(legacyTask))
    }

    @Test
    fun `event types prefix match the outer and the AgentEvent inner type`() {
        assertTrue(HubFilter(eventTypes = listOf("tutor.")).matches(tutor))
        assertTrue(HubFilter(eventTypes = listOf("AgentEvent")).matches(tutor))
        assertTrue(HubFilter(eventTypes = listOf("V3Task")).matches(legacyTask))
        assertFalse(HubFilter(eventTypes = listOf("tutor.")).matches(legacyTask))
    }

    @Test
    fun `ids match refs, falling back to the payload`() {
        assertTrue(HubFilter(taskId = "t1").matches(legacyTask))
        assertTrue(HubFilter(executionId = "e1").matches(legacyTask))
        assertFalse(HubFilter(taskId = "other").matches(legacyTask))
        assertTrue(HubFilter(chatTurnId = "turn-1").matches(stamped))
        assertFalse(HubFilter(chatTurnId = "turn-1").matches(legacyTask))
    }

    @Test
    fun `server classifications filter stamped frames and pass legacy ones`() {
        assertTrue(HubFilter(categories = listOf("chat", "today")).matches(stamped))
        assertFalse(HubFilter(categories = listOf("today")).matches(stamped))
        assertTrue(HubFilter(categories = listOf("today")).matches(legacyTask))

        assertFalse(HubFilter(userRelevant = true).matches(stamped))
        assertTrue(HubFilter(userRelevant = false).matches(stamped))
        assertTrue(HubFilter(userRelevant = true).matches(legacyTask))
    }

    @Test
    fun `the predicate runs after the fields`() {
        assertTrue(HubFilter(predicate = { it.refs.taskId == "t1" }).matches(legacyTask))
        assertFalse(HubFilter(taskId = "t1", predicate = { false }).matches(legacyTask))
    }
}
