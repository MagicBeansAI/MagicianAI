package ai.magicbeans.magdroid.thinking

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Map notices and interpretation progress, off the realtime bus.
 *
 * These assert the SERVER's frame shape — `RuntimeTransportEvent` serializes
 * with `tag = "event_type", content = "data"`, so the fields ride under
 * `data`. The first version of this file asserted a `payload` wrapping the bus
 * never sent, and passed: the parser and its tests agreed with each other and
 * both were wrong, so `ThinkingMapUpdated` never routed and the board only
 * moved on the poll. A wire test that does not copy the wire is how that
 * happens.
 */
class ThinkingMapRealtimeTest {

    @Test
    fun `an update notice carries the map and its new revision`() {
        val notice = parseMapNotice(
            """{"event_type":"ThinkingMapUpdated","data":{"map_id":"m1","principal":"p","workspace":"w","revision":7,"timestamp":1}}""",
        )
        assertEquals(ThinkingMapNotice("m1", 7), notice)
    }

    /** Flat fields still route — tolerance, not the contract. */
    @Test
    fun `a flat notice routes as a fallback`() {
        val flat = parseMapNotice(
            """{"event_type":"ThinkingMapUpdated","map_id":"m1","revision":9}""",
        )
        assertEquals(ThinkingMapNotice("m1", 9), flat)
    }

    /**
     * Every other event on the bus belongs to another surface. Re-reading the
     * map on a chat token would be a request per streamed word.
     */
    @Test
    fun `other events are not map notices`() {
        assertNull(parseMapNotice("""{"event_type":"ChatTokenReceived","data":{"map_id":"m1"}}"""))
        assertNull(parseMapNotice("""{"event_type":"TaskCompleted","data":{"map_id":"m1"}}"""))
        assertNull(parseMapNotice("""{"map_id":"m1","revision":3}"""))
    }

    /** A notice with no map names nothing and must not be acted on. */
    @Test
    fun `a notice without a map id is ignored`() {
        assertNull(parseMapNotice("""{"event_type":"ThinkingMapUpdated","data":{"revision":4}}"""))
        assertNull(parseMapNotice("""{"event_type":"ThinkingMapUpdated","data":{"map_id":""}}"""))
    }

    /**
     * A missing revision reads as zero, which the caller's gate then treats as
     * "not newer" — a malformed notice costs a skipped refresh, never a
     * re-fetch loop.
     */
    @Test
    fun `a notice without a revision reads as zero`() {
        assertEquals(
            0L,
            parseMapNotice("""{"event_type":"ThinkingMapUpdated","data":{"map_id":"m"}}""")?.revision,
        )
    }

    @Test
    fun `rubbish on the socket is ignored rather than thrown`() {
        assertNull(parseMapNotice("not json"))
        assertNull(parseMapNotice(""))
        assertNull(parseMapNotice("[1,2,3]"))
        assertNull(parseInterpretProgress("not json"))
        assertNull(parseInterpretProgress("[1,2,3]"))
    }

    // ── Interpretation progress ──────────────────────────────────────────────

    @Test
    fun `a progress event carries the run, the stage, and the count`() {
        val event = parseInterpretProgress(
            """{"event_type":"ThinkingMapInterpretProgress","data":{"map_id":"m1","principal":"p","workspace":"w","utterance_id":"u1","stage":"preparing","node_count":34,"timestamp":1}}""",
        )
        assertEquals(
            ThinkingInterpretProgress("m1", "u1", ThinkingProgress.Preparing, 34),
            event,
        )
    }

    @Test
    fun `every narrated stage parses to its state`() {
        listOf(
            "preparing" to ThinkingProgress.Preparing,
            "loading_context" to ThinkingProgress.LoadingContext,
            "facilitating" to ThinkingProgress.Facilitating,
            "parsing" to ThinkingProgress.Parsing,
            "shaping" to ThinkingProgress.Shaping,
            "idle" to ThinkingProgress.Idle,
        ).forEach { (wire, stage) ->
            val event = parseInterpretProgress(
                """{"event_type":"ThinkingMapInterpretProgress","data":{"map_id":"m","utterance_id":"u","stage":"$wire"}}""",
            )
            assertEquals(stage, event?.stage)
        }
    }

    /**
     * A stage this build has never heard of still parses — with a null stage,
     * so the caller keeps its current line. Dropping the event would make every
     * stage the server learns to narrate look like a dropped frame on old
     * builds.
     */
    @Test
    fun `an unknown stage parses with a null stage rather than being dropped`() {
        val event = parseInterpretProgress(
            """{"event_type":"ThinkingMapInterpretProgress","data":{"map_id":"m","utterance_id":"u","stage":"grounding"}}""",
        )
        assertEquals(ThinkingInterpretProgress("m", "u", null, null), event)
    }

    /**
     * Progress without a run id cannot be claimed by anyone and is dropped:
     * the id is the only thing separating this screen's run from an ambient
     * auto-map on the same board.
     */
    @Test
    fun `progress without an utterance id or map id is ignored`() {
        assertNull(
            parseInterpretProgress(
                """{"event_type":"ThinkingMapInterpretProgress","data":{"map_id":"m","stage":"parsing"}}""",
            ),
        )
        assertNull(
            parseInterpretProgress(
                """{"event_type":"ThinkingMapInterpretProgress","data":{"utterance_id":"u","stage":"parsing"}}""",
            ),
        )
    }

    /** The two event kinds must never claim each other's frames. */
    @Test
    fun `notices and progress do not cross-parse`() {
        val noticeFrame =
            """{"event_type":"ThinkingMapUpdated","data":{"map_id":"m1","revision":7}}"""
        val progressFrame =
            """{"event_type":"ThinkingMapInterpretProgress","data":{"map_id":"m1","utterance_id":"u","stage":"shaping"}}"""
        assertNull(parseInterpretProgress(noticeFrame))
        assertNull(parseMapNotice(progressFrame))
    }
}
