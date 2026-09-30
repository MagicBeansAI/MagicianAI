package ai.magicbeans.magdroid.thinking

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the owner is told when the facilitator fails.
 *
 * The retryable split is the part that earns a test: a timeout is worth trying
 * again and a rejection is not, and somebody who cannot tell will either hammer
 * it or abandon something that would have worked on the second go.
 */
class ThinkingIntelligenceTest {

    @Test
    fun `a timeout is worth another go`() {
        val fallback = classifyThinkingFailure(java.net.SocketTimeoutException("read timed out"))
        assertTrue(fallback.canRetry)
        assertTrue(fallback.message.contains("too long"))
    }

    @Test
    fun `an unreachable backend says so and can be retried`() {
        listOf(
            java.net.UnknownHostException("no such host"),
            java.net.ConnectException("refused"),
        ).forEach {
            val fallback = classifyThinkingFailure(it)
            assertTrue(fallback.canRetry)
            assertTrue(fallback.message.contains("could not be reached"))
        }
    }

    @Test
    fun `a cancellation explains itself rather than reading as a failure`() {
        val fallback = classifyThinkingFailure(
            kotlinx.coroutines.CancellationException("cancelled"),
        )
        assertTrue(fallback.canRetry)
        assertTrue(fallback.message.contains("cancelled"))
    }

    /**
     * A rejection is not retryable.
     *
     * Something refused the request rather than failing to deliver it, so the
     * same request will be refused again — and offering Retry would be a button
     * that cannot work.
     */
    @Test
    fun `a rejection is not offered a retry`() {
        val fallback = classifyThinkingFailure(ThinkingMapError("The facilitator refused that."))
        assertFalse(fallback.canRetry)
        assertEquals("The facilitator refused that.", fallback.message)
    }

    /** A conflict is somebody else's edit, not a facilitator problem. */
    @Test
    fun `a conflict is reported as a conflict and can be retried`() {
        val fallback = classifyThinkingFailure(
            ThinkingMapConflict("This map changed while you were editing it."),
        )
        assertTrue(fallback.canRetry)
        assertTrue(fallback.message.contains("changed while you were editing"))
    }

    @Test
    fun `a failure with no message still says something`() {
        val fallback = classifyThinkingFailure(RuntimeException())
        assertTrue(fallback.message.isNotBlank())
        assertTrue(fallback.statusLabel.isNotBlank())
        assertTrue(fallback.provenance.isNotBlank())
    }

    /**
     * The full stage vocabulary, now that the server narrates it.
     *
     * This test used to pin the count at TWO, because five of iOS's seven
     * states were assigned nowhere and porting a readout wired to nothing is a
     * mistake this codebase has had to undo before. The server now emits every
     * stage here from a real step of `interpret()` — the count grows because
     * the emission exists, which is the only reason it was ever allowed to.
     */
    @Test
    fun `progress covers the six server stages, each with something to say`() {
        assertEquals(6, ThinkingProgress.entries.size)
        ThinkingProgress.entries.forEach {
            assertTrue(it.label.isNotBlank())
            assertTrue(it.detail().isNotBlank())
        }
    }

    /** The wire spellings are the server's `InterpretStage::wire_name` set. */
    @Test
    fun `every server stage spelling maps to exactly one state`() {
        val wire = mapOf(
            "idle" to ThinkingProgress.Idle,
            "preparing" to ThinkingProgress.Preparing,
            "loading_context" to ThinkingProgress.LoadingContext,
            "facilitating" to ThinkingProgress.Facilitating,
            "parsing" to ThinkingProgress.Parsing,
            "shaping" to ThinkingProgress.Shaping,
        )
        wire.forEach { (spelling, stage) ->
            assertEquals(stage, ThinkingProgress.fromWire(spelling))
        }
        // A stage this build predates is null, never a guess: the client keeps
        // its current line, which is what lets the vocabulary grow server-first.
        assertEquals(null, ThinkingProgress.fromWire("grounding"))
        assertEquals(null, ThinkingProgress.fromWire(""))
    }

    /**
     * Preparing says how many thoughts are being read when the count arrived.
     * The number is what makes a several-second wait feel like motion.
     */
    @Test
    fun `preparing composes the node count into its detail`() {
        assertEquals(
            "Reading 34 thoughts and the active branch…",
            ThinkingProgress.Preparing.detail(34),
        )
        assertEquals(
            "Reading 1 thought and the active branch…",
            ThinkingProgress.Preparing.detail(1),
        )
        // No count (socket raced, or an empty board): the generic line, never
        // "Reading 0 thoughts" — a zero reads as a bug, not a board.
        assertEquals("Reading the board and the active branch…", ThinkingProgress.Preparing.detail(null))
        assertEquals("Reading the board and the active branch…", ThinkingProgress.Preparing.detail(0))
        // Other stages ignore the count; it is preparing's number.
        assertEquals(ThinkingProgress.Shaping.detail(), ThinkingProgress.Shaping.detail(34))
    }

    /**
     * The strip-routing gate. Each clause refuses a real hazard; together they
     * mean the strip only ever narrates the run this screen started.
     */
    @Test
    fun `progress applies only to this screen's own in-flight run`() {
        val event = ThinkingInterpretProgress(
            mapId = "m1",
            utteranceId = "u1",
            stage = ThinkingProgress.Facilitating,
            nodeCount = null,
        )
        assertTrue(interpretProgressApplies(event, openMapId = "m1", inFlightUtteranceId = "u1"))

        // Another map's run, another run on this map, or no run at all.
        assertFalse(interpretProgressApplies(event, openMapId = "m2", inFlightUtteranceId = "u1"))
        assertFalse(interpretProgressApplies(event, openMapId = "m1", inFlightUtteranceId = "u2"))
        assertFalse(interpretProgressApplies(event, openMapId = "m1", inFlightUtteranceId = null))
        assertFalse(interpretProgressApplies(event, openMapId = null, inFlightUtteranceId = "u1"))

        // A stage this build has never heard of: keep the current line.
        assertFalse(
            interpretProgressApplies(
                event.copy(stage = null),
                openMapId = "m1",
                inFlightUtteranceId = "u1",
            ),
        )

        // The terminal idle is the HTTP response's to apply — a realtime idle
        // can outrun the response body, and clearing early shows "Ready" over
        // a run still being applied.
        assertFalse(
            interpretProgressApplies(
                event.copy(stage = ThinkingProgress.Idle),
                openMapId = "m1",
                inFlightUtteranceId = "u1",
            ),
        )
    }
}
