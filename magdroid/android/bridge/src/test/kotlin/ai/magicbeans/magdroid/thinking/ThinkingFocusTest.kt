package ai.magicbeans.magdroid.thinking

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Walking a map one node at a time.
 *
 * Mirrors `ancestry`, `children(of:)` and `relatedNodes(to:)` in
 * `ThinkingMapModel.swift`. The orderings are not cosmetic — a branch the owner
 * wrote must not sit below one the agent proposed — and the malformed cases
 * must terminate, because a map is somebody's thinking and a hang loses it.
 */
class ThinkingFocusTest {

    private fun node(
        id: String,
        parent: String? = null,
        title: String = id,
        suggested: Boolean = false,
    ) = ThinkingNode(
        nodeId = id, parentId = parent, kind = "idea", label = title,
        // `suggested` is derived, not sent: model-inferred or provisional.
        assertionOrigin = if (suggested) "model_inferred" else "owner_spoken",
        epistemicState = "asserted",
    )

    private fun snapshot(
        nodes: List<ThinkingNode>,
        edges: List<ThinkingEdge> = emptyList(),
        active: String? = null,
    ) = ThinkingMap(
        mapId = "m", title = "m",
        // Keyed objects, as the server serialises a Rust BTreeMap.
        nodes = nodes.associateBy { it.id },
        edges = edges.associateBy { it.id },
        viewState = SharedViewState(focusNodeId = active),
    )

    // ── Ancestry ─────────────────────────────────────────────────────────────

    @Test
    fun `ancestry runs root first and ends at the focused node`() {
        val f = focus(
            snapshot(listOf(node("root"), node("mid", "root"), node("leaf", "mid"))),
            "leaf",
        )
        assertEquals(listOf("root", "mid", "leaf"), f.ancestry.map { it.id })
        assertEquals("leaf", f.node?.id)
    }

    /** A cycle above the focus must end the climb, not hang it. */
    @Test
    fun `a cycle in the ancestry terminates`() {
        val f = focus(
            snapshot(listOf(node("a", "c"), node("b", "a"), node("c", "b"))),
            "a",
        )
        assertTrue(f.ancestry.isNotEmpty())
        assertEquals(f.ancestry.size, f.ancestry.map { it.id }.distinct().size)
    }

    @Test
    fun `a root node has only itself above it`() {
        val f = focus(snapshot(listOf(node("root"))), "root")
        assertEquals(listOf("root"), f.ancestry.map { it.id })
    }

    // ── Branches ─────────────────────────────────────────────────────────────

    /**
     * Captured before suggested.
     *
     * A branch somebody wrote themselves should not sit below one the agent
     * proposed — the order is the client saying whose thought came first.
     */
    @Test
    fun `branches put what the owner captured ahead of suggestions`() {
        val f = focus(
            snapshot(
                listOf(
                    node("root"),
                    node("s", "root", title = "Aaa suggested", suggested = true),
                    node("c", "root", title = "Zzz captured"),
                ),
            ),
            "root",
        )
        assertEquals(listOf("c", "s"), f.branches.map { it.id })
    }

    @Test
    fun `a node is never its own branch`() {
        val f = focus(snapshot(listOf(node("self", parent = "self"))), "self")
        assertTrue(f.branches.isEmpty())
    }

    @Test
    fun `a leaf has no branches`() {
        val f = focus(snapshot(listOf(node("root"), node("leaf", "root"))), "leaf")
        assertTrue(f.branches.isEmpty())
    }

    // ── Related ──────────────────────────────────────────────────────────────

    @Test
    fun `related edges are read in both directions`() {
        val f = focus(
            snapshot(
                nodes = listOf(node("a"), node("b"), node("c")),
                edges = listOf(
                    ThinkingEdge(edgeId = "e1", from = "a", to = "b", kind = EdgeKind.RelatedTo.id),
                    ThinkingEdge(edgeId = "e2", from = "c", to = "a", kind = EdgeKind.RelatedTo.id),
                ),
            ),
            "a",
        )
        assertEquals(setOf("b", "c"), f.related.map { it.id }.toSet())
    }

    /** Only `related` edges. Parentage is already the branch list. */
    @Test
    fun `other edge kinds are not connections`() {
        val f = focus(
            snapshot(
                nodes = listOf(node("a"), node("b")),
                edges = listOf(ThinkingEdge(edgeId = "e", from = "a", to = "b", kind = "supports")),
            ),
            "a",
        )
        assertTrue(f.related.isEmpty())
    }

    @Test
    fun `a self edge and a duplicate neighbour are both ignored`() {
        val f = focus(
            snapshot(
                nodes = listOf(node("a"), node("b")),
                edges = listOf(
                    ThinkingEdge(edgeId = "e1", from = "a", to = "a", kind = EdgeKind.RelatedTo.id),
                    ThinkingEdge(edgeId = "e2", from = "a", to = "b", kind = EdgeKind.RelatedTo.id),
                    ThinkingEdge(edgeId = "e3", from = "b", to = "a", kind = EdgeKind.RelatedTo.id),
                ),
            ),
            "a",
        )
        assertEquals(listOf("b"), f.related.map { it.id })
    }

    /** An edge to a node that no longer exists points at nothing. */
    @Test
    fun `a dangling edge is dropped`() {
        val f = focus(
            snapshot(
                nodes = listOf(node("a")),
                edges = listOf(ThinkingEdge(edgeId = "e", from = "a", to = "gone", kind = EdgeKind.RelatedTo.id)),
            ),
            "a",
        )
        assertTrue(f.related.isEmpty())
    }

    // ── Resolution ───────────────────────────────────────────────────────────

    /**
     * A map whose nodes arrive before a selection must still render.
     *
     * No explicit focus falls to the map's active node, then to the first —
     * the same resolution the library row uses for its headline.
     */
    @Test
    fun `focus resolves to the active node, then the first`() {
        val nodes = listOf(node("one"), node("two"))
        assertEquals("two", focus(snapshot(nodes, active = "two")).node?.id)
        assertEquals("one", focus(snapshot(nodes)).node?.id)
    }

    @Test
    fun `an empty map focuses nothing`() {
        val f = focus(ThinkingMap())
        assertNull(f.node)
        assertTrue(f.isEmpty)
        assertTrue(f.ancestry.isEmpty())
    }

    /** An id that is not in the map falls back rather than showing nothing. */
    @Test
    fun `an unknown focus id falls back to the active node`() {
        val f = focus(snapshot(listOf(node("a"), node("b")), active = "b"), "deleted")
        assertEquals("b", f.node?.id)
    }
}
