package ai.magicbeans.magdroid.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * The live half of the Attention badge, against `PendingHitlTracker.swift`.
 *
 * Two rules carry this, and both exist because the other clients hit the bug
 * first: only the canonical `HitlRequested` counts, and the id is searched
 * key-first rather than layer-first. Get either wrong and the badge climbs and
 * never comes down.
 */
class PendingHitlTrackerTest {

    @Before fun clear() = PendingHitlTracker.reset()

    private fun requested(id: String) =
        """{"event_type":"HitlRequested","payload":{"correlation_id":"$id"}}"""

    @Test
    fun `a request raises the count and its resolution lowers it`() {
        PendingHitlTracker.apply(requested("c1"))
        assertEquals(1, PendingHitlTracker.count.value)

        PendingHitlTracker.apply("""{"event_type":"HitlResolved","payload":{"correlation_id":"c1"}}""")
        assertEquals(0, PendingHitlTracker.count.value)
    }

    /** Deduped by id: the same request seen twice is still one request. */
    @Test
    fun `the same request twice counts once`() {
        PendingHitlTracker.apply(requested("c1"))
        PendingHitlTracker.apply(requested("c1"))
        assertEquals(1, PendingHitlTracker.count.value)
    }

    /**
     * The legacy twins are co-emitted for the *same* pause and keyed on
     * `pause_state_id`. Counting them double-counts the badge and strands an
     * entry no `HitlResolved` will ever clear — a badge that only climbs.
     */
    @Test
    fun `legacy twins of the same pause are not counted`() {
        PendingHitlTracker.apply(requested("c1"))
        PendingHitlTracker.apply(
            """{"event_type":"AgenticWaitingForUser","payload":{"pause_state_id":"p1"}}""",
        )
        PendingHitlTracker.apply(
            """{"event_type":"UserRequestPending","payload":{"pause_state_id":"p1"}}""",
        )
        assertEquals(1, PendingHitlTracker.count.value)
    }

    /**
     * Key-first, not layer-first. A `correlation_id` buried under
     * `data.event.payload` still beats a `pause_state_id` at the top, because
     * the dedup contract is the id and not where it was written.
     */
    @Test
    fun `the correlation id wins wherever it is nested`() {
        PendingHitlTracker.apply(
            """{"event_type":"HitlRequested","pause_state_id":"p1",
                "data":{"event":{"payload":{"correlation_id":"c9"}}}}""",
        )
        assertEquals(1, PendingHitlTracker.count.value)
        // Resolved by that same id, proving which key was stored.
        PendingHitlTracker.apply("""{"event_type":"HitlResolved","payload":{"correlation_id":"c9"}}""")
        assertEquals(0, PendingHitlTracker.count.value)
    }

    /** `id` counts only on the legacy `data.request` layer, where it means this. */
    @Test
    fun `a bare id is read only from the legacy request payload`() {
        PendingHitlTracker.apply(
            """{"event_type":"HitlRequested","data":{"request":{"id":"r1"}}}""",
        )
        assertEquals(1, PendingHitlTracker.count.value)

        PendingHitlTracker.reset()
        // A top-level `id` is some other object's identity, not a correlation.
        PendingHitlTracker.apply("""{"event_type":"HitlRequested","id":"not-a-correlation"}""")
        assertEquals(0, PendingHitlTracker.count.value)
    }

    @Test
    fun `every resolution spelling clears the entry`() {
        listOf("HitlResolved", "UserRequestResolved", "approval.resolved", "x.responded", "x.expired", "x.cancelled", "x.dismissed")
            .forEach { type ->
                PendingHitlTracker.reset()
                PendingHitlTracker.apply(requested("c1"))
                PendingHitlTracker.apply("""{"event_type":"$type","payload":{"correlation_id":"c1"}}""")
                assertEquals("$type should clear", 0, PendingHitlTracker.count.value)
            }
    }

    /** The feed is authoritative; live frames only move the set between fetches. */
    @Test
    fun `seeding replaces the set`() {
        PendingHitlTracker.apply(requested("stale"))
        PendingHitlTracker.seed(listOf("a", "b", ""))
        assertEquals(2, PendingHitlTracker.count.value)
        assertFalse(PendingHitlTracker.contains("stale"))
        assertTrue(PendingHitlTracker.contains("a"))
    }

    /**
     * An optimistic drop that failed is undone by restoring only that id —
     * re-seeding would discard requests that arrived while it was in flight.
     */
    @Test
    fun `a failed response restores only its own id`() {
        PendingHitlTracker.seed(listOf("a"))
        PendingHitlTracker.drop("a")
        assertEquals(0, PendingHitlTracker.count.value)

        PendingHitlTracker.apply(requested("arrived-meanwhile"))
        PendingHitlTracker.restore("a")
        assertEquals(2, PendingHitlTracker.count.value)
    }

    @Test
    fun `bus bookkeeping and rubbish are ignored`() {
        PendingHitlTracker.apply("""{"event_type":"__events_replay","payload":{"correlation_id":"c1"}}""")
        PendingHitlTracker.apply("""{"event_type":"HitlRequested"}""")
        PendingHitlTracker.apply("not json")
        PendingHitlTracker.apply("")
        assertEquals(0, PendingHitlTracker.count.value)
    }
}
