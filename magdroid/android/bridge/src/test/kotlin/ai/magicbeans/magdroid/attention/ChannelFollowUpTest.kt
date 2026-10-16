package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Messages waiting on a reply.
 *
 * A different endpoint and a different shape from the feed. These hold the line
 * on the two things that are easy to get wrong: which resolutions a card may
 * offer, and the fact that "All" must never absorb them.
 */
class ChannelFollowUpTest {

    private fun page(json: String) =
        attentionJson.decodeFromString(ChannelFollowUpPage.serializer(), json)

    @Test
    fun `a page decodes with its cursor`() {
        val p = page(
            """{"items":[{"annotation_id":"a1","provider":"gmail","account_alias":"work",
                          "thread_id":"t1","lane":"needs_reply","subject":"Invoice?",
                          "sender":"sam@example.com","created_at":10,"review_required":false}],
                "next_cursor":"c1","total":9}""",
        )
        assertEquals("a1", p.items.single().id)
        assertEquals("Invoice?", p.items.single().displayTitle)
        assertTrue(p.hasMore)
        assertEquals(9, p.total)
    }

    @Test
    fun `no cursor means no more`() {
        assertFalse(page("""{"items":[],"total":0}""").hasMore)
        assertFalse(page("""{"items":[],"next_cursor":"","total":0}""").hasMore)
    }

    /**
     * Acknowledge means "handled, without opening it".
     *
     * Only honest when the distillation was confident enough not to need
     * checking, which is exactly what `review_required` states.
     */
    @Test
    fun `acknowledge is withheld when the card wants eyes on it`() {
        val needsReview = ChannelFollowUp(annotationId = "a", reviewRequired = true)
        val confident = ChannelFollowUp(annotationId = "b", reviewRequired = false)
        assertFalse(needsReview.canAcknowledge)
        assertTrue(confident.canAcknowledge)
    }

    /** A card with no subject still needs something readable at the top. */
    @Test
    fun `the headline falls back through subject, summary, then sender`() {
        assertEquals(
            "Subject line",
            ChannelFollowUp(subject = "Subject line", summary = "s", sender = "x").displayTitle,
        )
        assertEquals(
            "A summary",
            ChannelFollowUp(subject = "  ", summary = "A summary", sender = "x").displayTitle,
        )
        assertEquals(
            "Message from sam@example.com",
            ChannelFollowUp(sender = "sam@example.com").displayTitle,
        )
        assertEquals(
            "Message from work",
            ChannelFollowUp(accountAlias = "work").displayTitle,
        )
    }

    /**
     * The four actions are path segments on the server.
     *
     * An enum rather than free strings, because a typo would be a 404 that
     * reads as a card which simply will not resolve.
     */
    @Test
    fun `the resolutions are the four the server accepts`() {
        assertEquals(
            listOf("useful", "acknowledge", "snooze", "dismiss"),
            FollowUpAction.entries.map { it.id },
        )
    }

    // ── The lane must not leak into the feed ─────────────────────────────────

    /**
     * A feed item is work Magician paused; a follow-up is a person waiting.
     *
     * Merging them would make "everything that needs you" a list with two
     * meanings, so Messages holds no feed items and All holds no follow-ups.
     */
    @Test
    fun `messages carries no feed items and all carries no messages`() {
        val feed = AttentionFeedResponse(
            requests = listOf(AttentionItem(id = "r", itemType = "task", title = "t")),
        )
        assertTrue(feed.items(AttentionLane.Messages).isEmpty())
        assertEquals(listOf("r"), feed.items(AttentionLane.All).map { it.id })
        // And the feed cannot count a lane it does not hold.
        assertEquals(0, feed.laneCount(AttentionLane.Messages))
    }

    @Test
    fun `the messages tab counts what is visible, not what the feed knows`() {
        val state = AttentionUiState(
            lane = AttentionLane.Messages,
            feed = AttentionFeedResponse(counts = AttentionLaneCounts(needsAction = 7)),
            followUps = listOf(
                ChannelFollowUp(annotationId = "a"),
                ChannelFollowUp(annotationId = "b"),
            ),
        )
        assertEquals(2, state.count(AttentionLane.Messages))
    }

    /** A card being resolved leaves at once, like a dismissed feed row. */
    @Test
    fun `a resolving card is hidden while it is in flight`() {
        val state = AttentionUiState(
            followUps = listOf(
                ChannelFollowUp(annotationId = "a"),
                ChannelFollowUp(annotationId = "b"),
            ),
            resolving = setOf("a"),
        )
        assertEquals(listOf("b"), state.visibleFollowUps.map { it.id })
        assertEquals(1, state.count(AttentionLane.Messages))
    }

    @Test
    fun `messages paging follows its own cursor`() {
        val more = AttentionUiState(lane = AttentionLane.Messages, followUpsCursor = "c1")
        assertTrue(more.hasMore)
        val done = AttentionUiState(lane = AttentionLane.Messages, followUpsCursor = null)
        assertFalse(done.hasMore)
    }
}
