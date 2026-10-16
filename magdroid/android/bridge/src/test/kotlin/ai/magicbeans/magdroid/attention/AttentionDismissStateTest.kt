package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the list shows while a dismissal is in flight.
 *
 * The optimistic filter is state arithmetic, so it is tested as state — no
 * network, no view model scope. What matters is that a card leaves the moment
 * it is swiped, comes back if the server refuses, and never leaves twice.
 */
class AttentionDismissStateTest {

    private fun item(id: String, status: String = "failed") = AttentionItem(
        id = id,
        itemType = "task",
        title = "t-$id",
        status = status,
        createdAt = 1,
        updatedAt = 1,
    )

    private fun stateWith(vararg failed: AttentionItem) = AttentionUiState(
        lane = AttentionLane.Failed,
        feed = AttentionFeedResponse(failed = failed.toList()),
    )

    @Test
    fun `a card in flight leaves the list at once`() {
        val state = stateWith(item("a"), item("b"))
        assertEquals(listOf("a", "b"), state.items.map { it.id })

        val dismissing = state.copy(dismissing = setOf("a"))
        assertEquals(listOf("b"), dismissing.items.map { it.id })
    }

    /**
     * Restoring forgets one id rather than rebuilding the lane.
     *
     * The feed is untouched by an optimistic removal, which is what makes a
     * rollback a single-field edit instead of a re-fetch.
     */
    @Test
    fun `forgetting the id restores the row`() {
        val state = stateWith(item("a"), item("b")).copy(dismissing = setOf("a"))
        val restored = state.copy(dismissing = emptySet())
        assertEquals(listOf("a", "b"), restored.items.map { it.id })
    }

    @Test
    fun `dismissing every row leaves an empty lane, not a stale one`() {
        val state = stateWith(item("a")).copy(dismissing = setOf("a"))
        assertTrue(state.items.isEmpty())
    }

    /** An id that is no longer listed must not filter anything. */
    @Test
    fun `a stale in-flight id is harmless once the feed moves on`() {
        val state = AttentionUiState(
            lane = AttentionLane.Failed,
            feed = AttentionFeedResponse(failed = listOf(item("b"))),
            dismissing = setOf("a"),
        )
        assertEquals(listOf("b"), state.items.map { it.id })
    }

    /**
     * The filter follows the item across lanes.
     *
     * A failed card also appears in All, and dismissing it from one place while
     * it is still listed in the other would be the same card in two states.
     */
    @Test
    fun `a dismissed card is hidden in every lane that carried it`() {
        val state = AttentionUiState(
            lane = AttentionLane.All,
            feed = AttentionFeedResponse(
                failed = listOf(item("x")),
                requests = listOf(item("y", status = "needs_action")),
            ),
            dismissing = setOf("x"),
        )
        assertEquals(listOf("y"), state.items.map { it.id })
    }

    @Test
    fun `counts stay the server's until it says otherwise`() {
        // The lane label is not adjusted locally. A dismissal changes totals and
        // the badge, and guessing the new number here would disagree with the
        // refresh that follows a moment later.
        val state = AttentionUiState(
            lane = AttentionLane.Failed,
            feed = AttentionFeedResponse(
                counts = AttentionLaneCounts(failed = 2),
                failed = listOf(item("a"), item("b")),
            ),
            dismissing = setOf("a"),
        )
        assertEquals(1, state.items.size)
        assertEquals(2, state.count(AttentionLane.Failed))
    }

    // ── Bulk approval ────────────────────────────────────────────────────────

    private fun diffItem(id: String) = AttentionItem(
        id = id, itemType = "escalation", title = "change set $id", status = "needs_action",
        metadata = AttentionMetadata(inputSchema = AttentionInputSchema(type = "diff_approval")),
    )

    /** Only diff approvals. Nothing else is uniform enough to apply blind. */
    @Test
    fun `bulk approval selects diff approvals and nothing else`() {
        val state = AttentionUiState(
            lane = AttentionLane.All,
            feed = AttentionFeedResponse(
                escalations = listOf(diffItem("d1"), diffItem("d2")),
                requests = listOf(
                    AttentionItem(
                        id = "q", itemType = "escalation", title = "a question",
                        status = "needs_action",
                        metadata = AttentionMetadata(inputType = "text"),
                    ),
                ),
            ),
        )
        assertEquals(listOf("d1", "d2"), state.diffApprovals.map { it.id })
    }

    @Test
    fun `a failed change set is not offered for bulk approval`() {
        val state = AttentionUiState(
            lane = AttentionLane.Failed,
            feed = AttentionFeedResponse(
                failed = listOf(
                    AttentionItem(
                        id = "f", itemType = "failed", title = "broken", status = "failed",
                        metadata = AttentionMetadata(inputSchema = AttentionInputSchema(type = "diff_approval")),
                    ),
                ),
            ),
        )
        // Applying something already failed would post to a resolved id.
        assertTrue(state.diffApprovals.isEmpty())
    }

    /** A card being dismissed is off the list, so it is not bulk-approved either. */
    @Test
    fun `bulk approval respects the optimistic filter`() {
        val state = AttentionUiState(
            lane = AttentionLane.All,
            feed = AttentionFeedResponse(escalations = listOf(diffItem("d1"), diffItem("d2"))),
            dismissing = setOf("d1"),
        )
        assertEquals(listOf("d2"), state.diffApprovals.map { it.id })
    }

    @Test
    fun `undo is only offered while the card is still held`() {
        val held = stateWith(item("a")).copy(lastDismissed = item("a"))
        assertTrue(held.lastDismissed != null)
        // After a refresh the row is gone from the feed and there is nothing
        // local left to restore.
        val settled = held.copy(lastDismissed = null, dismissing = emptySet())
        assertFalse(settled.lastDismissed != null)
    }
}
