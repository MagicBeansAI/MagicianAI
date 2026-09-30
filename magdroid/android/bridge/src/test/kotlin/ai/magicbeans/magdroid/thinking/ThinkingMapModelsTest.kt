package ai.magicbeans.magdroid.thinking

import kotlinx.serialization.builtins.ListSerializer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Thinking maps, against what `thinking_maps_api.rs` actually serialises.
 *
 * These decode the server's own shape — `map_id`, `label`, `detail_markdown`,
 * nodes as an object keyed by id. An earlier version of this file asserted the
 * iOS client's projection instead (`id`, `title`, `detail`, a `snapshot`
 * wrapper), which iOS builds locally over a sync store and never receives from
 * this endpoint.
 *
 * That version passed. Every field defaulted, so a real map would have decoded
 * to an empty one and the library would have looked merely unused — which is
 * why these now decode literal server JSON rather than round-tripping our own
 * types.
 */
class ThinkingMapModelsTest {

    private fun map(json: String) =
        thinkingJson.decodeFromString(ThinkingMap.serializer(), json)

    @Test
    fun `a map decodes from the canonical shape`() {
        val m = map(
            """{"schema_version":1,"map_id":"m1","principal":"anonymous","workspace":"default",
                "title":"Pricing","revision":7,"lifecycle":"active",
                "view_state":{"focus_node_id":"n2"},
                "nodes":{
                  "n1":{"node_id":"n1","kind":"idea","label":"Tiered pricing",
                        "detail_markdown":"per seat","epistemic_state":"asserted",
                        "assertion_origin":"owner_spoken","confidence":0.9},
                  "n2":{"node_id":"n2","parent_id":"n1","kind":"question",
                        "label":"What do we lose?","epistemic_state":"asserted",
                        "assertion_origin":"model_inferred","confidence":0.4}},
                "edges":{
                  "e1":{"edge_id":"e1","from_node":"n1","to_node":"n2","kind":"related_to"}}}""",
        )
        assertEquals("m1", m.id)
        assertEquals("Pricing", m.displayTitle)
        assertEquals(7, m.revision)
        assertEquals(2, m.nodeList.size)
        assertEquals(1, m.edgeList.size)
        // The endpoints, which decoded empty while this asserted the `_id`
        // spellings the server has never sent.
        assertEquals("n1", m.edgeList.single().from)
        assertEquals("n2", m.edgeList.single().to)
        // The focused node comes from view_state, not a snapshot field.
        assertEquals("What do we lose?", m.activeThought)
    }

    /**
     * `suggested` is derived here, not sent.
     *
     * Model-inferred *or* still provisional, which is the rule the server uses
     * for its own preview. Two conditions, because a provisional owner thought
     * is also not something they have committed to.
     */
    @Test
    fun `suggested follows origin and epistemic state`() {
        val inferred = ThinkingNode(nodeId = "a", assertionOrigin = "model_inferred", epistemicState = "asserted")
        val provisional = ThinkingNode(nodeId = "b", assertionOrigin = "owner_spoken", epistemicState = "provisional")
        val settled = ThinkingNode(nodeId = "c", assertionOrigin = "owner_spoken", epistemicState = "asserted")
        assertTrue(inferred.suggested)
        assertTrue(provisional.suggested)
        assertFalse(settled.suggested)
    }

    @Test
    fun `counts separate what the owner settled from the rest`() {
        val m = map(
            """{"map_id":"m","title":"t","nodes":{
                 "1":{"node_id":"1","kind":"idea","label":"a",
                      "assertion_origin":"owner_spoken","epistemic_state":"asserted"},
                 "2":{"node_id":"2","kind":"question","label":"b",
                      "assertion_origin":"owner_spoken","epistemic_state":"asserted"},
                 "3":{"node_id":"3","kind":"question","label":"c",
                      "assertion_origin":"model_inferred","epistemic_state":"asserted"},
                 "4":{"node_id":"4","kind":"action","label":"d",
                      "assertion_origin":"owner_spoken","epistemic_state":"asserted"}}}""",
        )
        assertEquals(3, m.capturedCount)
        assertEquals(2, m.questionCount)
        assertEquals(1, m.actionCount)
    }

    /** A kind this build has never seen still shows, rather than vanishing. */
    @Test
    fun `an unknown node kind degrades to a note`() {
        val m = map(
            """{"map_id":"m","title":"t","nodes":{
                 "n":{"node_id":"n","kind":"constraint","label":"Budget is fixed"}}}""",
        )
        assertEquals(ThinkingNodeKind.Other, m.nodeList.single().nodeKind)
        assertEquals("Budget is fixed", m.nodeList.single().title)
    }

    @Test
    fun `enums match regardless of case, and unknown values do not throw`() {
        assertEquals(ThinkingNodeKind.Idea, ThinkingNodeKind.from("Idea"))
        assertEquals(ThinkingNodeKind.Evidence, ThinkingNodeKind.from("EVIDENCE"))
        assertEquals(AssertionOrigin.ModelInferred, AssertionOrigin.from("model_inferred"))
        assertEquals(AssertionOrigin.Unknown, AssertionOrigin.from("from_the_future"))
        assertEquals(EpistemicState.Provisional, EpistemicState.from("provisional"))
        assertEquals(EpistemicState.Unknown, EpistemicState.from(null))
    }

    @Test
    fun `an empty map is unstarted, and an untitled one still has a name`() {
        val m = map("""{"map_id":"m","title":"","nodes":{}}""")
        assertEquals("Unstarted idea", m.activeThought)
        assertEquals("Untitled map", m.displayTitle)
        assertEquals(0, m.capturedCount)
    }

    /** The focused node may be gone; the first node is the honest fallback. */
    @Test
    fun `a dangling focus id falls back to the first node`() {
        val m = map(
            """{"map_id":"m","title":"t","view_state":{"focus_node_id":"missing"},
                "nodes":{"n1":{"node_id":"n1","kind":"idea","label":"Still here"}}}""",
        )
        assertEquals("Still here", m.activeThought)
    }

    @Test
    fun `search reaches the title, node labels and node detail`() {
        val m = map(
            """{"map_id":"m","title":"Pricing","nodes":{
                 "n":{"node_id":"n","kind":"idea","label":"Tiered","detail_markdown":"per seat"}}}""",
        )
        assertTrue(m.matches("pric"))
        assertTrue(m.matches("TIERED"))
        assertTrue(m.matches("per seat"))
        assertFalse(m.matches("nothing like this"))
        assertTrue(m.matches("   "))
    }

    // ── The library ──────────────────────────────────────────────────────────

    /** `list_maps` returns summaries in a bare array, not whole maps. */
    @Test
    fun `the library decodes summaries with their preview`() {
        val summaries = thinkingJson.decodeFromString(
            ListSerializer(ThinkingMapSummary.serializer()),
            """[{"map_id":"m1","title":"Pricing","lifecycle":"active",
                 "latest_revision":12,"updated_at":"2026-08-11T09:00:00Z",
                 "node_preview":{"nodes":[
                    {"node_id":"n1","kind":"idea","suggested":false,"title":"Tiered pricing"},
                    {"node_id":"n2","kind":"question","suggested":true,"title":"What do we lose?"}]}},
                {"map_id":"m2","title":"","lifecycle":"archived",
                 "latest_revision":1,"updated_at":"2026-08-01T09:00:00Z"}]""",
        )
        assertEquals(2, summaries.size)
        // The card leads with a settled thought rather than a suggestion.
        assertEquals("Tiered pricing", summaries[0].previewThought)
        assertEquals(2, summaries[0].previewCount)
        assertFalse(summaries[0].isArchived)

        // A summary with no preview invents nothing.
        assertNull(summaries[1].previewThought)
        assertEquals(0, summaries[1].previewCount)
        assertEquals("Untitled map", summaries[1].displayTitle)
        assertTrue(summaries[1].isArchived)
    }

    /**
     * Clarifications and proposals ride on every map payload and neither was
     * read. A question the agent asked never appeared, so the map looked idle
     * when it was actually blocked; and no proposal id ever reached this client,
     * which left `decideProposal` with nothing it could ever decide about.
     *
     * Both arrive keyed by id — a Rust `BTreeMap` — like nodes and edges.
     */
    @Test
    fun `clarifications and proposals decode from the map payload`() {
        val decoded = map(
            """{"map_id":"m","title":"Pricing","revision":4,
                "nodes":{},"edges":{},
                "clarifications":{
                  "c1":{"clarification_id":"c1","node_id":"n1",
                        "question":"Which tier caps first?","state":"open",
                        "created_at":"2026-08-11T09:00:00Z"},
                  "c2":{"clarification_id":"c2","node_id":"n2","question":"Answered one",
                        "state":"answered","answer":"the middle tier",
                        "created_at":"2026-08-11T08:00:00Z",
                        "resolved_at":"2026-08-11T08:30:00Z"}},
                "proposals":{
                  "p1":{"proposal_id":"p1","rationale":"Group the pricing thoughts",
                        "state":"proposed","affected_node_ids":["n1","n2"],
                        "created_at":"2026-08-11T09:05:00Z"},
                  "p2":{"proposal_id":"p2","rationale":"Already decided",
                        "state":"rejected","created_at":"2026-08-11T07:00:00Z"}}}""",
        )

        assertEquals(2, decoded.clarifications.size)
        val open = decoded.openClarifications.single()
        assertEquals("c1", open.id)
        assertEquals("Which tier caps first?", open.question)
        assertEquals("n1", open.nodeId)
        assertTrue(open.isOpen)

        val answered = decoded.clarifications.getValue("c2")
        assertEquals("the middle tier", answered.answer)
        assertFalse(answered.isOpen)

        assertEquals(2, decoded.proposals.size)
        val pending = decoded.openProposals.single()
        assertEquals("p1", pending.id)
        assertEquals("Group the pricing thoughts", pending.rationale)
        assertEquals(listOf("n1", "n2"), pending.affectedNodeIds)
    }

    /** A map with neither reads as having nothing pending, not as broken. */
    @Test
    fun `a map without clarifications or proposals has none open`() {
        val decoded = map("""{"map_id":"m","title":"t","nodes":{},"edges":{}}""")
        assertTrue(decoded.openClarifications.isEmpty())
        assertTrue(decoded.openProposals.isEmpty())
    }

    /** An unrecognised state must not read as open and re-ask a settled question. */
    @Test
    fun `an unknown clarification state does not count as open`() {
        assertEquals(ClarificationState.Open, ClarificationState.from("open"))
        assertEquals(ClarificationState.Deferred, ClarificationState.from("deferred"))
        val decoded = map(
            """{"map_id":"m","title":"t","nodes":{},"edges":{},
                "clarifications":{"c":{"clarification_id":"c","node_id":"n",
                  "question":"q","state":"withdrawn","created_at":"z"}}}""",
        )
        assertTrue(decoded.openClarifications.isEmpty())
    }

    @Test
    fun `unknown fields do not break either decode`() {
        assertEquals(
            "a",
            map(
                """{"map_id":"m","title":"t","brand_new":{"x":1},
                    "nodes":{"n":{"node_id":"n","kind":"idea","label":"a","future":2}},
                    "clarifications":{},"proposals":{}}""",
            ).nodeList.single().title,
        )
    }
}
