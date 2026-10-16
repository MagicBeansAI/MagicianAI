package ai.magicbeans.magdroid.thinking

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A graph read as an outline.
 *
 * Every interesting case here is a malformed graph — a deleted parent, a cycle,
 * a node that is its own parent. The map is somebody's thinking and none of it
 * may be dropped on the way to the screen, however broken the links are.
 */
class ThinkingOutlineTest {

    private fun node(id: String, parent: String? = null, title: String = id) =
        ThinkingNode(nodeId = id, parentId = parent, kind = "idea", label = title,
            assertionOrigin = "owner_spoken", epistemicState = "asserted")

    private fun rowsOf(vararg nodes: ThinkingNode) =
        outline(ThinkingMap(mapId = "m", nodes = nodes.associateBy { it.id }))

    @Test
    fun `children sit under their parent, deeper`() {
        val rows = rowsOf(
            node("root"),
            node("a", parent = "root"),
            node("b", parent = "a"),
        )
        assertEquals(listOf("root", "a", "b"), rows.map { it.node.id })
        assertEquals(listOf(0, 1, 2), rows.map { it.depth })
    }

    @Test
    fun `siblings are ordered by title`() {
        val rows = rowsOf(
            node("root"),
            node("z", parent = "root", title = "Zebra"),
            node("a", parent = "root", title = "Apple"),
        )
        assertEquals(listOf("root", "a", "z"), rows.map { it.node.id })
    }

    /**
     * A node whose parent was deleted is still the owner's.
     *
     * Showing it at the top level keeps it in the map; dropping it would lose
     * a thought because something above it went away.
     */
    @Test
    fun `an orphan is promoted rather than dropped`() {
        val rows = rowsOf(node("root"), node("lost", parent = "deleted-long-ago"))
        assertEquals(2, rows.size)
        assertTrue(rows.all { it.depth == 0 })
        assertTrue(rows.any { it.node.id == "lost" })
    }

    /** A cycle must terminate, and must not swallow the nodes in it. */
    @Test
    fun `a cycle is broken and every node still appears`() {
        val rows = rowsOf(
            node("a", parent = "c"),
            node("b", parent = "a"),
            node("c", parent = "b"),
        )
        assertEquals(3, rows.size)
        assertEquals(setOf("a", "b", "c"), rows.map { it.node.id }.toSet())
    }

    @Test
    fun `a node that is its own parent is a root, not a loop`() {
        val rows = rowsOf(node("self", parent = "self"))
        assertEquals(1, rows.size)
        assertEquals(0, rows.single().depth)
    }

    /** Each node is placed once, at the first depth it is reached. */
    @Test
    fun `no node is listed twice`() {
        val rows = rowsOf(
            node("root"),
            node("shared", parent = "root"),
            node("deep", parent = "shared"),
        )
        assertEquals(rows.size, rows.map { it.node.id }.distinct().size)
    }

    @Test
    fun `an empty map outlines to nothing`() {
        assertTrue(outline(ThinkingMap()).isEmpty())
    }

    @Test
    fun `several roots are all kept, in order`() {
        val rows = rowsOf(node("one"), node("two"), node("three"))
        assertEquals(listOf("one", "two", "three"), rows.map { it.node.id })
        assertTrue(rows.all { it.depth == 0 })
    }

    /**
     * A suggestion keeps its flag through the outline.
     *
     * The screen marks it, because a map that blurs the agent's thoughts with
     * the owner's misrepresents what they decided.
     */
    @Test
    fun `suggested nodes survive the walk marked`() {
        val rows = rowsOf(
            node("root"),
            ThinkingNode(
                nodeId = "s", parentId = "root", kind = "question",
                label = "Did we consider…?", assertionOrigin = "model_inferred",
            ),
        )
        assertTrue(rows.single { it.node.id == "s" }.node.suggested)
    }
}
