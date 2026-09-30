package ai.magicbeans.magdroid.thinking

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Where nodes land when the map is drawn.
 *
 * Ported from `ThinkingGraphView`: depth decides the horizontal band, position
 * within the band the vertical. Nothing is stored server-side, so both clients
 * derive this — and if they derive it differently the same map is two different
 * shapes, which is the kind of divergence nobody notices until they compare
 * screenshots.
 */
class ThinkingGraphLayoutTest {

    private fun node(id: String, parent: String? = null) = ThinkingNode(
        nodeId = id, parentId = parent, kind = "idea", label = id,
        assertionOrigin = "owner_spoken", epistemicState = "asserted",
    )

    private fun mapOfNodes(vararg nodes: ThinkingNode, edges: List<ThinkingEdge> = emptyList()) =
        ThinkingMap(
            mapId = "m",
            nodes = nodes.associateBy { it.id },
            edges = edges.associateBy { it.id },
        )

    @Test
    fun `depth runs left to right`() {
        val layout = graphLayout(
            mapOfNodes(node("root"), node("mid", "root"), node("leaf", "mid")),
        )
        val byId = layout.points.associateBy { it.node.id }
        assertEquals(0f, byId["root"]!!.x, 0.001f)
        assertEquals(0.5f, byId["mid"]!!.x, 0.001f)
        assertEquals(1f, byId["leaf"]!!.x, 0.001f)
    }

    /**
     * Nodes never touch the top or bottom edge.
     *
     * `count + 1` is what buys the gap — with two nodes they sit at a third and
     * two thirds, not at 0 and 1 where half of each would be off-canvas.
     */
    @Test
    fun `a level is spread with a gap at each end`() {
        val layout = graphLayout(
            mapOfNodes(node("root"), node("a", "root"), node("b", "root")),
        )
        val level1 = layout.points.filter { it.x > 0f }.sortedBy { it.y }
        assertEquals(2, level1.size)
        assertEquals(1f / 3f, level1[0].y, 0.001f)
        assertEquals(2f / 3f, level1[1].y, 0.001f)
    }

    /** A flat map still divides, rather than dividing by zero. */
    @Test
    fun `a single node is placed without a crash`() {
        val layout = graphLayout(mapOfNodes(node("only")))
        assertEquals(1, layout.points.size)
        assertEquals(0f, layout.points.single().x, 0.001f)
        assertEquals(0.5f, layout.points.single().y, 0.001f)
    }

    @Test
    fun `siblings with no depth all sit in the same band`() {
        val layout = graphLayout(mapOfNodes(node("a"), node("b"), node("c")))
        assertTrue(layout.points.all { it.x == 0f })
        assertEquals(3, layout.points.map { it.y }.distinct().size)
    }

    /** A cycle must resolve to a finite depth, or the layout never returns. */
    @Test
    fun `a cycle does not hang the layout`() {
        val layout = graphLayout(
            mapOfNodes(node("a", "c"), node("b", "a"), node("c", "b")),
        )
        assertEquals(3, layout.points.size)
        assertTrue(layout.points.all { it.x.isFinite() && it.y.isFinite() })
    }

    @Test
    fun `a node whose parent is gone is treated as a root`() {
        val layout = graphLayout(mapOfNodes(node("orphan", "deleted")))
        assertEquals(0f, layout.points.single().x, 0.001f)
    }

    // ── Edges ────────────────────────────────────────────────────────────────

    /**
     * Every live edge is drawn, whatever it means.
     *
     * This asserted that only `branch` edges became links — and `branch` is not
     * one of the server's edge kinds, so the canvas drew nodes and never a
     * single line between them. Narrowing it the way Focus narrows is also
     * wrong: Focus answers "what else is this about", the canvas is showing the
     * shape of the map, and a `supports` link is part of that shape.
     */
    @Test
    fun `every live edge becomes a link`() {
        val layout = graphLayout(
            mapOfNodes(
                node("a"), node("b", "a"),
                edges = listOf(
                    ThinkingEdge(edgeId = "e1", from = "a", to = "b", kind = EdgeKind.RelatedTo.id),
                    ThinkingEdge(edgeId = "e2", from = "a", to = "b", kind = EdgeKind.Supports.id),
                ),
            ),
        )
        assertEquals(2, layout.links.size)
    }

    /** A removed edge is not drawn, and the payload still carries it. */
    @Test
    fun `a tombstoned edge is not drawn`() {
        val layout = graphLayout(
            mapOfNodes(
                node("a"), node("b", "a"),
                edges = listOf(
                    ThinkingEdge(edgeId = "e1", from = "a", to = "b", kind = EdgeKind.RelatedTo.id),
                    ThinkingEdge(
                        edgeId = "e2", from = "a", to = "b",
                        kind = EdgeKind.RelatedTo.id, tombstoned = true,
                    ),
                ),
            ),
        )
        assertEquals(1, layout.links.size)
    }

    /** An edge to a node that is not placed cannot be drawn. */
    @Test
    fun `a dangling edge is dropped rather than drawn to nowhere`() {
        val layout = graphLayout(
            mapOfNodes(
                node("a"),
                edges = listOf(
                    ThinkingEdge(edgeId = "e", from = "a", to = "missing", kind = EdgeKind.RelatedTo.id),
                ),
            ),
        )
        assertTrue(layout.links.isEmpty())
    }

    @Test
    fun `an empty map lays out to nothing`() {
        val layout = graphLayout(ThinkingMap())
        assertTrue(layout.points.isEmpty())
        assertTrue(layout.links.isEmpty())
    }

    /**
     * The same map draws the same way twice.
     *
     * The server sends nodes as a keyed object and iteration order is not a
     * promise, so the placement is sorted — without it the shape would shuffle
     * between reads of identical data.
     */
    @Test
    fun `placement is stable across identical maps`() {
        val nodes = arrayOf(node("z", "root"), node("a", "root"), node("root"))
        val first = graphLayout(mapOfNodes(*nodes)).points.map { it.node.id to it.y }
        val again = graphLayout(mapOfNodes(*nodes)).points.map { it.node.id to it.y }
        assertEquals(first, again)
    }
}
