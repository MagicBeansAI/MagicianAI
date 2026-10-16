package ai.magicbeans.magdroid.thinking

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The canonical map payload, decoded here.
 *
 * `magios/Magios/ThinkingMapCanonical/Fixtures` is a real recorded document —
 * the shape `GET /thinking-maps/{id}` actually returns, kept by the iOS
 * canonical suite. Nothing on this side read it, and five separate decode
 * faults lived behind that: edge endpoints under the wrong names, an edge kind
 * that matched nothing, tombstones ignored on both nodes and edges, and a
 * `connect` operation the server could only refuse.
 *
 * Every one of them was invisible. Absent keys default, defaults are plausible,
 * and a map with no connections looks like a map nobody has connected yet.
 */
class ThinkingMapWireFixtureTest {

    private fun fixture(name: String) = File(fixtureRoot(), name).readText()

    private fun map() = thinkingJson.decodeFromString(ThinkingMap.serializer(), fixture("map.json"))

    @Test
    fun `the canonical map decodes its identity and shape`() {
        val decoded = map()
        assertEquals("map-1", decoded.mapId)
        assertEquals("iOS thinking map migration", decoded.title)
        assertEquals("active", decoded.lifecycle)
        assertEquals(4L, decoded.revision)
        assertTrue("nodes should decode", decoded.nodes.isNotEmpty())
        assertTrue("edges should decode", decoded.edges.isNotEmpty())
    }

    /**
     * The endpoints. Read as `from_node_id`/`to_node_id` they came back empty,
     * so every connection was invisible: nothing to draw, nothing for Focus to
     * walk, and nothing for a disconnect to find.
     */
    @Test
    fun `an edge names both of its ends`() {
        val edge = map().edges.values.first()
        assertTrue("from_node must decode", edge.from.isNotBlank())
        assertTrue("to_node must decode", edge.to.isNotBlank())
        assertTrue("the ends must differ", edge.from != edge.to)
    }

    /** `related_to`, not `related` — the filter matched nothing for its whole life. */
    @Test
    fun `a plain relation decodes as the kind the server sends`() {
        val kinds = map().edgeList.mapNotNull { it.edgeKind }
        assertTrue("no edge kind was recognised: ${map().edgeList.map { it.kind }}", kinds.isNotEmpty())
        assertEquals(EdgeKind.RelatedTo, EdgeKind.from("related_to"))
        assertEquals(null, EdgeKind.from("related"))
    }

    /**
     * The document arrives whole, tombstones included — the server filters them
     * out of its previews and not out of this. Unread, a deleted thought stayed
     * on screen and survived a restart.
     */
    @Test
    fun `tombstoned nodes and edges are dropped from the live lists`() {
        val decoded = thinkingJson.decodeFromString(
            ThinkingMap.serializer(),
            """{"map_id":"m","title":"t","revision":1,
                "nodes":{
                  "live":{"node_id":"live","kind":"idea","label":"kept"},
                  "gone":{"node_id":"gone","kind":"idea","label":"deleted","tombstoned":true}},
                "edges":{
                  "e1":{"edge_id":"e1","from_node":"live","to_node":"live2","kind":"related_to"},
                  "e2":{"edge_id":"e2","from_node":"a","to_node":"b","kind":"related_to",
                        "tombstoned":true}}}""",
        )
        assertEquals(listOf("live"), decoded.nodeList.map { it.id })
        assertEquals(listOf("e1"), decoded.edgeList.map { it.id })
        // The raw maps keep everything: the filter belongs to the readers, and
        // a caller that wants the tombstone can still reach it.
        assertEquals(2, decoded.nodes.size)
        assertEquals(2, decoded.edges.size)
    }

    /** Focus walks `related_to` edges, in either direction, ignoring tombstones. */
    @Test
    fun `focus finds what a node is connected to`() {
        val decoded = thinkingJson.decodeFromString(
            ThinkingMap.serializer(),
            """{"map_id":"m","title":"t",
                "nodes":{
                  "a":{"node_id":"a","kind":"idea","label":"A"},
                  "b":{"node_id":"b","kind":"idea","label":"B"},
                  "c":{"node_id":"c","kind":"idea","label":"C"}},
                "edges":{
                  "e1":{"edge_id":"e1","from_node":"a","to_node":"b","kind":"related_to"},
                  "e2":{"edge_id":"e2","from_node":"c","to_node":"a","kind":"related_to"},
                  "e3":{"edge_id":"e3","from_node":"a","to_node":"c","kind":"supports"}}}""",
        )
        val related = focus(decoded, "a").related.map { it.id }.sorted()
        // b through the outgoing edge, c through the incoming one. The
        // `supports` edge is a different relation and is not a connection.
        assertEquals(listOf("b", "c"), related)
    }

    /** The clarification the fixture carries, which nothing here used to read. */
    @Test
    fun `the fixture's open clarification is surfaced`() {
        val open = map().openClarifications
        assertEquals(1, open.size)
        assertEquals("clar-1", open.single().id)
        assertNotNull(open.single().question)
        assertFalse(open.single().question.isBlank())
    }

    private fun fixtureRoot(): File {
        var directory: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (directory != null) {
            val candidate = File(directory, "magios/Magios/ThinkingMapCanonical/Fixtures")
            if (candidate.isDirectory) return candidate
            directory = directory.parentFile
        }
        error("could not locate the canonical map fixtures")
    }
}
