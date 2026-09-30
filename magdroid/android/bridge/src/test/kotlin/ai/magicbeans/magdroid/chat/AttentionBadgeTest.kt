package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The Attention badge count, ported from the web's `resolveAttentionBadgeCount`
 * — the same rule iOS follows.
 *
 * Pending HITL and the feed's needs-action count are two projections of the
 * same work, so the larger wins rather than both being counted. Failed rows
 * are separate and add once. Three surfaces disagreeing about how much is
 * waiting is worse than any one of them being briefly stale.
 */
class AttentionBadgeTest {
    private val json = Json { ignoreUnknownKeys = true }

    /** The overlap is the whole point: 3 and 3 is three things, not six. */
    @Test
    fun `overlapping projections are not added together`() {
        assertEquals(3, resolveAttentionBadgeCount(pendingHitl = 3, needsAction = 3, failed = 0))
    }

    /** Whichever projection has seen more is the one to trust. */
    @Test
    fun `the larger projection wins`() {
        assertEquals(5, resolveAttentionBadgeCount(pendingHitl = 5, needsAction = 2, failed = 0))
        assertEquals(5, resolveAttentionBadgeCount(pendingHitl = 2, needsAction = 5, failed = 0))
    }

    /** Failed rows are not HITL, so they add on top exactly once. */
    @Test
    fun `failures add once`() {
        assertEquals(7, resolveAttentionBadgeCount(pendingHitl = 4, needsAction = 4, failed = 3))
    }

    @Test
    fun `nothing waiting is no badge`() {
        assertEquals(0, resolveAttentionBadgeCount(0, 0, 0))
    }

    /** A negative count is a bug upstream, not a reason to subtract. */
    @Test
    fun `negative inputs cannot reduce the count`() {
        assertEquals(2, resolveAttentionBadgeCount(pendingHitl = -5, needsAction = 0, failed = 2))
        assertEquals(3, resolveAttentionBadgeCount(pendingHitl = 3, needsAction = 0, failed = -9))
    }

    /** The badge reads two fields off the attention feed; the lanes are not its business. */
    @Test
    fun `the feed's counts decode`() {
        val feed = json.decodeFromString(
            AttentionFeed.serializer(),
            """{"counts": {"requests": 2, "approvals": 1, "escalations": 0,
                           "needs_action": 3, "failed": 1, "running": 7}}""",
        )
        assertEquals(3, feed.counts.needsAction)
        assertEquals(1, feed.counts.failed)
        assertEquals(4, resolveAttentionBadgeCount(0, feed.counts.needsAction, feed.counts.failed))
    }

    /** A feed with no counts block is zero, not a failure. */
    @Test
    fun `a countless feed is simply empty`() {
        val feed = json.decodeFromString(AttentionFeed.serializer(), "{}")
        assertEquals(0, resolveAttentionBadgeCount(0, feed.counts.needsAction, feed.counts.failed))
    }
}
