package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The answered-requests history, paired from the HITL event backfill.
 *
 * There is no resolved-requests resource anywhere — the history is the event
 * log, paired by correlation id. iOS assembles it the same way.
 */
class ResolvedHitlTest {

    private fun events(vararg rows: String) = """{"events":[${rows.joinToString(",")}]}"""

    private fun requested(id: String, question: String, at: Long) =
        """{"event_type":"HitlRequested","data":{"correlation_id":"$id",
            "question":"$question","timestamp":$at}}"""

    private fun resolved(id: String, outcome: String = "responded", at: Long = 2, decision: String? = null) =
        """{"event_type":"HitlResolved","data":{"correlation_id":"$id","outcome":"$outcome",
            "timestamp":$at${decision?.let { ""","decision":"$it"""" } ?: ""}}}"""

    @Test
    fun `a request and its resolution become one row`() {
        val rows = pairResolvedHitl(
            events(requested("c1", "Ship the pricing change?", 1), resolved("c1", "approved", 2, "yes")),
        )
        assertEquals(1, rows.size)
        assertEquals("c1", rows.single().correlationId)
        assertEquals("Ship the pricing change?", rows.single().prompt)
        assertEquals("approved", rows.single().outcome)
        assertEquals("yes", rows.single().decision)
    }

    /**
     * A resolution whose request fell outside the fetched window is dropped. A
     * history row that cannot say what was asked is worse than one fewer row.
     */
    @Test
    fun `a resolution with no request in the window is dropped`() {
        val rows = pairResolvedHitl(events(resolved("orphan")))
        assertTrue(rows.isEmpty())
    }

    /** A request still waiting is not history — it belongs in the lanes. */
    @Test
    fun `an unanswered request is not history`() {
        val rows = pairResolvedHitl(events(requested("c1", "Still waiting", 1)))
        assertTrue(rows.isEmpty())
    }

    @Test
    fun `history reads newest first`() {
        val rows = pairResolvedHitl(
            events(
                requested("old", "Older question", 1), resolved("old", at = 10),
                requested("new", "Newer question", 2), resolved("new", at = 20),
            ),
        )
        assertEquals(listOf("new", "old"), rows.map { it.correlationId })
    }

    /** The pair proves it was answered even when the event did not say how. */
    @Test
    fun `a resolution without an outcome still reads as answered`() {
        val rows = pairResolvedHitl(events(requested("c1", "Q", 1), resolved("c1", outcome = "")))
        assertEquals("responded", rows.single().outcome)
        assertNull(rows.single().decision)
    }

    /** The payload rides under `data` or at the top, depending on the wrapper. */
    @Test
    fun `a flat event pairs the same as a wrapped one`() {
        val rows = pairResolvedHitl(
            events(
                """{"event_type":"HitlRequested","correlation_id":"c1","question":"Q","timestamp":1}""",
                """{"event_type":"HitlResolved","correlation_id":"c1","outcome":"denied","timestamp":2}""",
            ),
        )
        assertEquals("denied", rows.single().outcome)
    }

    @Test
    fun `rubbish yields nothing rather than throwing`() {
        assertTrue(pairResolvedHitl("not json").isEmpty())
        assertTrue(pairResolvedHitl("").isEmpty())
        assertTrue(pairResolvedHitl("""{"events":[]}""").isEmpty())
    }
}
