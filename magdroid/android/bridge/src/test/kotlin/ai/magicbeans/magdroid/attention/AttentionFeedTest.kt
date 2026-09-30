package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The attention feed, against `FeedAttentionResponse` in `feed_api.rs`.
 *
 * Decoded from JSON shaped like the server's, not round-tripped through these
 * types: the server is the other party and a field named wrong here decodes to
 * a default that looks like an empty inbox, which is the one failure this
 * surface must not have.
 */
class AttentionFeedTest {

    private fun feed(json: String): AttentionFeedResponse =
        attentionJson.decodeFromString(AttentionFeedResponse.serializer(), json)

    private fun item(id: String, updated: Long, status: String = "needs_action") =
        """{"id":"$id","item_type":"approval","title":"t-$id","status":"$status",
            "created_at":1,"updated_at":$updated}"""

    @Test
    fun `lanes decode with their items and counts`() {
        val f = feed(
            """{"counts":{"requests":2,"approvals":1,"escalations":0,
                          "needs_action":3,"failed":1,"running":4},
                "totals":{"requests":9,"approvals":1,"escalations":0,"failed":1,"running":4},
                "requests":[${item("r1", 30)}],
                "approvals":[${item("a1", 20)}],
                "escalations":[],
                "failed":[${item("f1", 10, "failed")}],
                "running":[]}""",
        )
        assertEquals(1, f.items(AttentionLane.Requests).size)
        assertEquals("r1", f.items(AttentionLane.Requests).single().id)
        assertTrue(f.items(AttentionLane.Failed).single().failed)
        assertTrue(f.items(AttentionLane.Approvals).single().needsAction)
        // `totals` is truthful where present; `counts` is the fallback for
        // builds that predate it.
        assertEquals(9, f.laneCount(AttentionLane.Requests))
        assertEquals(1, f.laneCount(AttentionLane.Failed))
    }

    /**
     * All is a merge this client does, newest first.
     *
     * Running is excluded on purpose: work in progress is not waiting on
     * anybody, and listing it beside things that are dilutes the surface.
     */
    @Test
    fun `all merges the waiting lanes newest first and leaves running out`() {
        val f = feed(
            """{"requests":[${item("r1", 10)}],
                "approvals":[${item("a1", 30)}],
                "escalations":[${item("e1", 20)}],
                "failed":[],
                "running":[${item("run1", 99)}]}""",
        )
        assertEquals(listOf("a1", "e1", "r1"), f.items(AttentionLane.All).map { it.id })
    }

    /** An item in two lanes is one row, or the count disagrees with the list. */
    @Test
    fun `all shows an item once even when two lanes carry it`() {
        val f = feed(
            """{"requests":[${item("x", 10)}],
                "escalations":[${item("x", 10)}],
                "approvals":[],"failed":[],"running":[]}""",
        )
        assertEquals(1, f.items(AttentionLane.All).size)
    }

    @Test
    fun `all counts every waiting lane rather than needs_action`() {
        val f = feed(
            """{"counts":{"requests":2,"approvals":3,"escalations":1,
                          "needs_action":6,"failed":4,"running":0},
                "requests":[],"approvals":[],"escalations":[],"failed":[],"running":[]}""",
        )
        // needs_action is the badge's input and excludes failed rows. Using it
        // here would print a number the list underneath cannot match.
        assertEquals(10, f.laneCount(AttentionLane.All))
    }

    // ── Paging ───────────────────────────────────────────────────────────────

    @Test
    fun `a lane reports its own cursor and whether more exists`() {
        val f = feed(
            """{"pages":{"requests":{"total":40,"limit":25,"next_cursor":"c1","has_more":true},
                         "approvals":{"total":1,"limit":25,"has_more":false},
                         "escalations":{"total":0,"limit":25,"has_more":false},
                         "failed":{"total":0,"limit":25,"has_more":false},
                         "running":{"total":0,"limit":25,"has_more":false}},
                "requests":[],"approvals":[],"escalations":[],"failed":[],"running":[]}""",
        )
        assertTrue(f.page(AttentionLane.Requests).hasMore)
        assertEquals("c1", f.page(AttentionLane.Requests).nextCursor)
        assertFalse(f.page(AttentionLane.Approvals).hasMore)
    }

    /** All has more whenever anything it draws from does. */
    @Test
    fun `all has more when any lane it merges has more`() {
        val f = feed(
            """{"pages":{"requests":{"has_more":false},
                         "approvals":{"has_more":false},
                         "escalations":{"has_more":true},
                         "failed":{"has_more":false},
                         "running":{"has_more":false}},
                "requests":[],"approvals":[],"escalations":[],"failed":[],"running":[]}""",
        )
        assertTrue(f.page(AttentionLane.All).hasMore)
    }

    // ── Robustness ───────────────────────────────────────────────────────────

    /**
     * An empty body must not look like a populated inbox, or vice versa.
     *
     * Every field defaults, so a response missing a lane reads as that lane
     * being empty rather than failing the whole decode and blanking the screen.
     */
    @Test
    fun `a sparse response decodes to an empty inbox`() {
        val f = feed("""{"counts":{"requests":0,"approvals":0,"escalations":0,
                                    "needs_action":0,"failed":0,"running":0}}""")
        AttentionLane.entries.forEach { assertTrue(f.items(it).isEmpty()) }
        assertEquals(0, f.laneCount(AttentionLane.All))
        assertFalse(f.page(AttentionLane.All).hasMore)
    }

    @Test
    fun `unknown fields do not break the decode`() {
        val f = feed(
            """{"requests":[{"id":"r","item_type":"task","title":"t","status":"needs_action",
                             "created_at":1,"updated_at":2,"something_new":{"a":1}}],
                "approvals":[],"escalations":[],"failed":[],"running":[],
                "a_whole_new_lane":[]}""",
        )
        assertEquals("r", f.items(AttentionLane.Requests).single().id)
    }

    @Test
    fun `lane ids round trip and an unknown one falls back to all`() {
        AttentionLane.entries.forEach { assertEquals(it, AttentionLane.from(it.id)) }
        // `messages` resolved to All until follow-ups had a lane of their own;
        // it now resolves to itself, which is what this line is here to notice.
        assertEquals(AttentionLane.Messages, AttentionLane.from("messages"))
        assertEquals(AttentionLane.All, AttentionLane.from("running"))
        assertEquals(AttentionLane.All, AttentionLane.from(null))
    }
}
