package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Graph family (plan 1.5): bounded model parsing and deterministic tiers. */
class MuijGraphModelsTest {
    private fun node(id: String, label: String = id, kind: String? = null, metadata: JsonObject? = null): JsonObject = buildJsonObject {
        put("id", id)
        put("label", label)
        kind?.let { put("kind", it) }
        metadata?.let { put("metadata", it) }
    }

    private fun edge(from: String, to: String, label: String? = null): JsonObject = buildJsonObject {
        put("from", from)
        put("to", to)
        label?.let { put("label", it) }
    }

    private fun graphComponent(
        nodes: List<JsonObject>,
        edges: List<JsonObject>,
        layout: String? = null,
        focus: String? = null,
        reveal: List<String> = emptyList(),
    ): MuijComponent = MuijComponent(
        id = "graph",
        type = "Graph",
        label = "Pipeline",
        props = buildJsonObject {
            put("nodes", JsonArray(nodes))
            put("edges", JsonArray(edges))
            layout?.let { put("layout", it) }
            focus?.let { put("focus_node_id", it) }
            if (reveal.isNotEmpty()) put("reveal_order", JsonArray(reveal.map { JsonPrimitive(it) }))
        },
        staticSnapshot = null,
        children = emptyList(),
    )

    @Test fun graph_model_parses_nodes_edges_layout_focus_and_reveal() {
        val component = graphComponent(
            nodes = listOf(
                node("root", label = "Root", kind = "core", metadata = buildJsonObject {
                    put("status", "active")
                    put("attempts", 2)
                }),
                node("leaf", label = "Leaf"),
            ),
            edges = listOf(edge("root", "leaf", label = "drives")),
            layout = "radial",
            focus = "root",
            reveal = listOf("root", "leaf"),
        )

        val model = component.graphModel
        assertEquals(listOf("root", "leaf"), model.nodes.map { it.id })
        assertEquals("core", model.nodes.first().kind)
        assertEquals(listOf(MuijGraphMetaEntry("attempts", "2"), MuijGraphMetaEntry("status", "active")), model.nodes.first().metadata)
        assertEquals(listOf(MuijGraphEdge("root", "leaf", "drives")), model.edges)
        assertEquals(MuijGraphLayout.RADIAL, model.layout)
        assertEquals("root", model.focusNodeId)
        assertEquals(listOf("root", "leaf"), model.revealOrder)
    }

    @Test fun graph_tiers_are_deterministic_and_cycles_share_a_final_tier() {
        val diamond = graphComponent(
            nodes = listOf(node("a"), node("b"), node("c"), node("d")),
            edges = listOf(edge("a", "b"), edge("a", "c"), edge("b", "d"), edge("c", "d")),
        ).graphModel
        val tier = { id: String -> diamond.nodes.first { it.id == id }.tier }
        assertEquals(0, tier("a"))
        assertEquals(1, tier("b"))
        assertEquals(1, tier("c"))
        assertEquals(2, tier("d"))

        val cyclic = graphComponent(
            nodes = listOf(node("entry"), node("loopA"), node("loopB")),
            edges = listOf(edge("entry", "loopA"), edge("loopA", "loopB"), edge("loopB", "loopA")),
        ).graphModel
        val cycleTier = { id: String -> cyclic.nodes.first { it.id == id }.tier }
        assertEquals(0, cycleTier("entry"))
        assertEquals(1, cycleTier("loopA"))
        assertEquals(1, cycleTier("loopB"))
    }

    @Test fun graph_skips_malformed_members_and_dangling_edges() {
        val model = graphComponent(
            nodes = listOf(node("keep"), node("keep")),
            edges = listOf(edge("keep", "ghost"), edge("keep", "keep")),
        ).graphModel

        assertEquals(listOf("keep"), model.nodes.map { it.id })
        assertEquals(listOf(MuijGraphEdge("keep", "keep")), model.edges)
    }

    @Test fun graph_admission_is_capped_at_the_bounded_sizes() {
        val nodes = (0..(MuijGraphModel.MaximumNodes + 5)).map { node("n$it") }
        val edges = (0..(MuijGraphModel.MaximumEdges + 5)).map { edge("n0", "n1") }
        val model = graphComponent(nodes, edges).graphModel

        assertEquals(MuijGraphModel.MaximumNodes, model.nodes.count())
        assertEquals(MuijGraphModel.MaximumEdges, model.edges.count())
    }

    @Test fun graph_unknown_layout_falls_back_to_layered_and_focus_must_be_declared() {
        assertEquals(MuijGraphLayout.LAYERED, graphComponent(listOf(node("only")), emptyList(), layout = "physics").graphModel.layout)
        assertEquals(MuijGraphLayout.LIST, graphComponent(listOf(node("only")), emptyList(), layout = "list").graphModel.layout)
        assertNull(graphComponent(listOf(node("only")), emptyList(), focus = "ghost").graphModel.focusNodeId)
        assertTrue(graphComponent(listOf(node("only")), emptyList(), reveal = listOf("only", "ghost")).graphModel.revealOrder == listOf("only"))
    }

    @Test fun graph_document_with_graph_component_still_parses_and_keeps_forward_compatibility() {
        val raw = todayJson.parseToJsonElement("""{
          "muij_version":"1.0","agent_id":"surface","layout":[
            {"id":"pipeline","component_type":"Graph","label":"Pipeline","props":{
              "layout":"list",
              "nodes":[{"id":"a","label":"Alpha"},{"id":"b","label":"Beta"}],
              "edges":[{"from":"a","to":"b"}],
              "focus_node_id":"a"
            }}
          ]
        }""")
        val document = (MuijDocument.parse(raw) as MuijParseResult.Valid).document
        val model = document.layout.single().graphModel
        assertEquals(MuijGraphLayout.LIST, model.layout)
        assertEquals(listOf("a", "b"), model.nodes.map { it.id })
        assertEquals("a", model.focusNodeId)
    }

    @Test fun graph_ids_compare_exactly_without_trimming() {
        // Ids compare exactly like the Rust validator (no trim): a node id
        // with surrounding spaces and an edge naming that same untrimmed id
        // is a Rust-valid document and must stay connected.
        val model = graphComponent(
            nodes = listOf(node(" a"), node("b")),
            edges = listOf(edge(" a", "b")),
        ).graphModel

        assertEquals(listOf(" a", "b"), model.nodes.map { it.id })
        assertEquals(listOf(MuijGraphEdge(" a", "b")), model.edges)
    }

    @Test fun graph_drops_nodes_whose_ids_exceed_the_id_cap() {
        val overlongId = "a".repeat(MuijGraphModel.MaximumNodeIdChars + 1)
        val model = graphComponent(
            nodes = listOf(node(overlongId), node("keep")),
            edges = listOf(edge(overlongId, "keep")),
        ).graphModel

        assertEquals(listOf("keep"), model.nodes.map { it.id })
        assertTrue(model.edges.isEmpty())
    }
}
