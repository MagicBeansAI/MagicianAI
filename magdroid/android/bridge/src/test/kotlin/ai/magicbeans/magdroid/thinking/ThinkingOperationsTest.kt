package ai.magicbeans.magdroid.thinking

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Operations, against `thinking_map/operations.rs`.
 *
 * The enum is `#[serde(tag = "op", rename_all = "snake_case")]`, so the
 * discriminator and every field name here are the server's to define. A wrong
 * one is a rejected envelope, and the owner's thought is simply not recorded.
 *
 * The sequences mirror `CanonicalThinkingMapBackend.swift`. They are sequences
 * and not single ops on purpose — capture under a parent is two, and editing a
 * provisional node is two — and getting the second one wrong is the failure
 * that shows up later as a suggestion that never became the owner's.
 */
class ThinkingOperationsTest {

    @Test
    fun `add_node carries an owner-spoken, asserted node`() {
        val op = ThinkingOp.addNode(
            nodeId = "n1", label = "Tiered pricing",
            kind = ThinkingNodeKind.Idea, parentId = null,
        )
        assertEquals("add_node", op["op"]!!.jsonPrimitive.content)
        val node = op["node"]!!.jsonObject
        assertEquals("n1", node["node_id"]!!.jsonPrimitive.content)
        assertEquals("idea", node["kind"]!!.jsonPrimitive.content)
        assertEquals("Tiered pricing", node["label"]!!.jsonPrimitive.content)
        // Owner-spoken and asserted is what makes this *not* a suggestion when
        // it is read back.
        assertEquals("owner_spoken", node["assertion_origin"]!!.jsonPrimitive.content)
        assertEquals("asserted", node["epistemic_state"]!!.jsonPrimitive.content)
        assertEquals(1.0, node["confidence"]!!.jsonPrimitive.content.toDouble(), 0.001)
        assertNull(node["parent_id"])
    }

    /** The label doubles as detail, so a captured thought has a body. */
    @Test
    fun `add_node falls back to the label for detail`() {
        val node = ThinkingOp.addNode("n", "A thought", ThinkingNodeKind.Idea, null)["node"]!!
            .jsonObject
        assertEquals("A thought", node["detail_markdown"]!!.jsonPrimitive.content)

        val explicit = ThinkingOp.addNode("n", "A thought", ThinkingNodeKind.Idea, null, "Detail")
        assertEquals(
            "Detail",
            explicit["node"]!!.jsonObject["detail_markdown"]!!.jsonPrimitive.content,
        )
    }

    /**
     * Capturing under a node is two operations.
     *
     * The parent rides inside the node *and* as an explicit move, matching
     * iOS — the reducer treats the move as the authority on placement, and
     * sending only the field would leave the node where the server put it.
     */
    @Test
    fun `capturing under a parent emits the move as well`() {
        val ops = ThinkingOps.addThought(nodeId = "new", text = "Next", activeId = "parent")
        assertEquals(2, ops.size)
        assertEquals("add_node", ops[0]["op"]!!.jsonPrimitive.content)
        assertEquals("parent", ops[0]["node"]!!.jsonObject["parent_id"]!!.jsonPrimitive.content)
        assertEquals("move_to_parent", ops[1]["op"]!!.jsonPrimitive.content)
        assertEquals("new", ops[1]["node_id"]!!.jsonPrimitive.content)
        assertEquals("parent", ops[1]["parent_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `capturing a root is one operation`() {
        val ops = ThinkingOps.addThought(nodeId = "root", text = "First", activeId = null)
        assertEquals(1, ops.size)
        assertNull(ops[0]["node"]!!.jsonObject["parent_id"])
    }

    /**
     * `detail_markdown` is `Option<Option<String>>`.
     *
     * Explicit null clears; a string sets. An empty edit must clear rather than
     * store "", because a blank body and no body read the same and only one is
     * honest about what is there.
     */
    @Test
    fun `an emptied detail is cleared, not stored blank`() {
        val cleared = ThinkingOp.updateNode("n", "Title", "")
        assertEquals(JsonNull, cleared["detail_markdown"])

        val set = ThinkingOp.updateNode("n", "Title", "Body")
        assertEquals("Body", set["detail_markdown"]!!.jsonPrimitive.content)
    }

    /**
     * Editing a provisional node promotes it.
     *
     * Once somebody has rewritten a suggestion in their own words it is theirs;
     * leaving it provisional would keep presenting it as the agent's.
     */
    @Test
    fun `editing a provisional node also asserts it`() {
        val promoted = ThinkingOps.editNode("n", "T", "D", wasProvisional = true)
        assertEquals(2, promoted.size)
        assertEquals("update_node", promoted[0]["op"]!!.jsonPrimitive.content)
        assertEquals("set_epistemic_state", promoted[1]["op"]!!.jsonPrimitive.content)
        assertEquals("asserted", promoted[1]["state"]!!.jsonPrimitive.content)

        val plain = ThinkingOps.editNode("n", "T", "D", wasProvisional = false)
        assertEquals(1, plain.size)
    }

    /**
     * Rejecting is not deleting.
     *
     * The map keeps that the agent proposed it and the owner said no, which is
     * worth more than the node quietly disappearing — and a tombstone would
     * lose exactly that.
     */
    @Test
    fun `rejecting records a refusal rather than removing the node`() {
        val rejected = ThinkingOps.rejectSuggestion("n").single()
        assertEquals("set_epistemic_state", rejected["op"]!!.jsonPrimitive.content)
        assertEquals("rejected", rejected["state"]!!.jsonPrimitive.content)

        val accepted = ThinkingOps.acceptSuggestion("n").single()
        assertEquals("asserted", accepted["state"]!!.jsonPrimitive.content)

        val deleted = ThinkingOps.deleteNode("n").single()
        assertEquals("tombstone_node", deleted["op"]!!.jsonPrimitive.content)
    }

    @Test
    fun `connect nests a whole edge, as the server reads it`() {
        val op = ThinkingOps.connect("e1", "a", "b").single()
        assertEquals("connect", op["op"]!!.jsonPrimitive.content)
        // `Connect { edge: ThinkingEdge }`. Sent flat — `edge_id` and friends at
        // the top level — the envelope was refused and no link was ever made.
        assertNull("the fields belong under `edge`", op["edge_id"])
        val edge = op["edge"]!!.jsonObject
        assertEquals("e1", edge["edge_id"]!!.jsonPrimitive.content)
        // `from_node`/`to_node`, not the `_id` spellings.
        assertEquals("a", edge["from_node"]!!.jsonPrimitive.content)
        assertEquals("b", edge["to_node"]!!.jsonPrimitive.content)
        // `related_to` is the server's snake_case name; `related` is nothing.
        assertEquals("related_to", edge["kind"]!!.jsonPrimitive.content)
        // Required by the struct, and the owner drew this rather than a model.
        assertEquals("owner_spoken", edge["assertion_origin"]!!.jsonPrimitive.content)
        assertEquals(false, edge["tombstoned"]!!.jsonPrimitive.content.toBoolean())
        assertTrue(edge["created_at"]!!.jsonPrimitive.content.isNotBlank())
        assertTrue(edge["updated_at"]!!.jsonPrimitive.content.isNotBlank())
    }

    @Test
    fun `set_node_kind uses the server's snake_case names`() {
        val op = ThinkingOp.setNodeKind("n", ThinkingNodeKind.Assumption)
        assertEquals("set_node_kind", op["op"]!!.jsonPrimitive.content)
        assertEquals("assumption", op["kind"]!!.jsonPrimitive.content)
    }

    /** An unclassified kind must not be sent as an empty string. */
    @Test
    fun `an unknown kind falls back to idea when capturing`() {
        val node = ThinkingOp.addNode("n", "t", ThinkingNodeKind.Other, null)["node"]!!.jsonObject
        assertEquals("idea", node["kind"]!!.jsonPrimitive.content)
    }

    /**
     * A link can be taken back.
     *
     * `connect` shipped without its inverse, so the only way out of a wrong
     * connection was deleting a node that was not the problem.
     */
    @Test
    fun `disconnect names the edge to remove`() {
        val op = ThinkingOps.disconnect("e1").single()
        assertEquals("disconnect", op["op"]!!.jsonPrimitive.content)
        assertEquals("e1", op["edge_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `answering a clarification carries the answer and the answered state`() {
        val op = ThinkingOps.answerClarification("c1", "Because the tier caps at 40").single()
        assertEquals("resolve_clarification", op["op"]!!.jsonPrimitive.content)
        assertEquals("c1", op["clarification_id"]!!.jsonPrimitive.content)
        assertEquals("answered", op["state"]!!.jsonPrimitive.content)
        assertEquals("Because the tier caps at 40", op["answer"]!!.jsonPrimitive.content)
    }

    /**
     * Deferring is not answering with nothing. An empty `answer` would record
     * that the owner replied and said nothing, which is a different claim from
     * "not now" — and the field is optional on the wire precisely so it can be
     * left off.
     */
    @Test
    fun `deferring and dismissing carry no answer`() {
        val deferred = ThinkingOps.deferClarification("c1").single()
        assertEquals("deferred", deferred["state"]!!.jsonPrimitive.content)
        assertNull(deferred["answer"])

        val dismissed = ThinkingOps.dismissClarification("c1").single()
        assertEquals("dismissed", dismissed["state"]!!.jsonPrimitive.content)
        assertNull(dismissed["answer"])
    }

    /** A blank answer is not sent as an answer either. */
    @Test
    fun `a blank answer is omitted rather than sent empty`() {
        val op = ThinkingOp.resolveClarification("c1", ClarificationState.Answered, "   ")
        assertNull(op["answer"])
    }

    /** The focus broadcast nests its node under `view_state`, as the server reads it. */
    @Test
    fun `set_shared_view nests the active node`() {
        val op = ThinkingOp.setSharedView("n7")
        assertEquals("set_shared_view", op["op"]!!.jsonPrimitive.content)
        assertEquals("n7", op["view_state"]!!.jsonObject["active_node"]!!.jsonPrimitive.content)

        // Clearing focus sends the state without a node rather than a null one.
        val cleared = ThinkingOp.setSharedView(null)
        assertTrue(cleared["view_state"]!!.jsonObject.isEmpty())
    }

    @Test
    fun `every operation names itself with the op discriminator`() {
        val all = listOf(
            ThinkingOp.addNode("n", "l", ThinkingNodeKind.Idea, null),
            ThinkingOp.moveToParent("n", "p"),
            ThinkingOp.updateNode("n", "l", "d"),
            ThinkingOp.setEpistemicState("n", EpistemicState.Asserted),
            ThinkingOp.setNodeKind("n", ThinkingNodeKind.Risk),
            ThinkingOp.tombstoneNode("n"),
            ThinkingOp.connect("e", "a", "b"),
            ThinkingOp.disconnect("e"),
            ThinkingOp.resolveClarification("c", ClarificationState.Answered, "yes"),
            ThinkingOp.setSharedView("n"),
        )
        assertTrue(all.all { it["op"]!!.jsonPrimitive.content.isNotBlank() })
        // snake_case, as the enum is renamed.
        assertTrue(all.none { it["op"]!!.jsonPrimitive.content.any(Char::isUpperCase) })
    }
}
