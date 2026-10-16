package ai.magicbeans.magdroid.thinking

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * Operations against a map.
 *
 * `#[serde(tag = "op", rename_all = "snake_case")]` on the server, so the
 * discriminator is `op` and the variant names are snake_case. Built as JSON
 * rather than a sealed serializable because `detail_markdown` is an
 * `Option<Option<String>>` — three intents in one field — and no Kotlin type
 * expresses "absent", "explicitly null" and "set" without a wrapper that reads
 * worse than the builder does.
 */
object ThinkingOp {

    /**
     * Insert a node the owner just spoke.
     *
     * `asserted` and `owner_spoken` with full confidence: this is somebody
     * typing their own thought, not a model proposing one, and the distinction
     * is what `suggested` reads back out of.
     */
    fun addNode(
        nodeId: String,
        label: String,
        kind: ThinkingNodeKind,
        parentId: String?,
        detail: String? = null,
    ): JsonObject = buildJsonObject {
        put("op", "add_node")
        putJsonObject("node") {
            put("node_id", nodeId)
            put("kind", kind.id.ifEmpty { ThinkingNodeKind.Idea.id })
            put("label", label)
            // Mirrors iOS: the label doubles as detail when none was given, so
            // a captured thought is never stored with an empty body.
            put("detail_markdown", detail ?: label.ifEmpty { null })
            put("epistemic_state", EpistemicState.Asserted.id)
            put("assertion_origin", AssertionOrigin.OwnerSpoken.id)
            put("confidence", 1.0)
            parentId?.let { put("parent_id", it) }
        }
    }

    fun moveToParent(nodeId: String, parentId: String): JsonObject = buildJsonObject {
        put("op", "move_to_parent")
        put("node_id", nodeId)
        put("parent_id", parentId)
    }

    /**
     * Patch a node's text.
     *
     * `detail_markdown` is `Option<Option<String>>`: absent leaves it alone,
     * explicit null clears it, a string sets it. An empty edit clears rather
     * than storing "", because a blank body and no body are the same thing to a
     * reader and only one of them is honest.
     */
    fun updateNode(nodeId: String, label: String, detail: String): JsonObject = buildJsonObject {
        put("op", "update_node")
        put("node_id", nodeId)
        put("label", label)
        if (detail.isEmpty()) put("detail_markdown", JsonNull) else put("detail_markdown", detail)
    }

    fun setEpistemicState(nodeId: String, state: EpistemicState): JsonObject = buildJsonObject {
        put("op", "set_epistemic_state")
        put("node_id", nodeId)
        put("state", state.id)
    }

    fun setNodeKind(nodeId: String, kind: ThinkingNodeKind): JsonObject = buildJsonObject {
        put("op", "set_node_kind")
        put("node_id", nodeId)
        put("kind", kind.id)
    }

    fun tombstoneNode(nodeId: String): JsonObject = buildJsonObject {
        put("op", "tombstone_node")
        put("node_id", nodeId)
    }

    /**
     * Join two nodes.
     *
     * `Connect { edge: ThinkingEdge }` on the server — a whole edge nested under
     * `edge`, not the flat `edge_id`/`from_node_id`/`to_node_id` this used to
     * send. Every connection made from this client was refused, and the flat
     * spelling was wrong twice over: the endpoints are `from_node`/`to_node`,
     * and the default kind is `related_to`.
     *
     * `created_at`/`updated_at` are required by the struct and overwritten by
     * the reducer with the applied-at stamp, so the value sent only has to be
     * present and well formed.
     */
    fun connect(
        edgeId: String,
        from: String,
        to: String,
        kind: EdgeKind = EdgeKind.RelatedTo,
        at: String = "1970-01-01T00:00:00Z",
    ): JsonObject = buildJsonObject {
        put("op", "connect")
        putJsonObject("edge") {
            put("edge_id", edgeId)
            put("from_node", from)
            put("to_node", to)
            put("kind", kind.id)
            // The owner drew it, which is what makes it theirs rather than a
            // suggestion the map should show as provisional.
            put("assertion_origin", AssertionOrigin.OwnerSpoken.id)
            put("tombstoned", false)
            put("created_at", at)
            put("updated_at", at)
        }
    }

    /**
     * Remove a link between two nodes.
     *
     * `connect` shipped without its inverse, so a link drawn by hand or inferred
     * by the agent could never be taken back — the only way out of a wrong
     * connection was deleting one of the nodes it joined.
     */
    fun disconnect(edgeId: String): JsonObject = buildJsonObject {
        put("op", "disconnect")
        put("edge_id", edgeId)
    }

    /**
     * Answer, defer or dismiss a question the agent asked.
     *
     * `answer` is optional on the wire and only meaningful for
     * [ClarificationState.Answered] — deferring carries no text, and sending an
     * empty string would record that the owner answered with nothing.
     */
    fun resolveClarification(
        clarificationId: String,
        state: ClarificationState,
        answer: String? = null,
    ): JsonObject = buildJsonObject {
        put("op", "resolve_clarification")
        put("clarification_id", clarificationId)
        put("state", state.id)
        answer?.takeIf { it.isNotBlank() }?.let { put("answer", it) }
    }

    /**
     * Broadcast which node the viewers of this map are looking at.
     *
     * Shared rather than local: a map can be watched by more than the person
     * driving it, and a focus that stayed on this handset would leave everyone
     * else reading a different part of the same thought.
     */
    fun setSharedView(activeNodeId: String?): JsonObject = buildJsonObject {
        put("op", "set_shared_view")
        putJsonObject("view_state") {
            activeNodeId?.let { put("active_node", it) }
        }
    }
}

/**
 * The op sequences iOS builds, ported exactly.
 *
 * Pure and separate from the repository so the sequences can be checked without
 * a network — and they are sequences, not single ops, which is the part worth
 * holding: capturing a thought under a parent is two operations, and editing a
 * provisional one is two more.
 */
object ThinkingOps {

    /**
     * Capture a thought, optionally under the node in focus.
     *
     * The parent is set twice — once inside the node and again as a
     * `move_to_parent` — because that is what iOS emits and the reducer treats
     * the explicit move as the authority on placement.
     */
    fun addThought(
        nodeId: String,
        text: String,
        kind: ThinkingNodeKind = ThinkingNodeKind.Idea,
        activeId: String? = null,
    ): List<JsonObject> {
        val ops = mutableListOf(
            ThinkingOp.addNode(nodeId = nodeId, label = text, kind = kind, parentId = activeId),
        )
        if (activeId != null) ops += ThinkingOp.moveToParent(nodeId, activeId)
        return ops
    }

    /**
     * Edit a node's text.
     *
     * Editing a provisional node promotes it: once somebody has rewritten a
     * suggestion in their own words it is theirs, and leaving it provisional
     * would keep showing it as the agent's.
     */
    fun editNode(
        nodeId: String,
        title: String,
        detail: String,
        wasProvisional: Boolean,
    ): List<JsonObject> {
        val ops = mutableListOf(ThinkingOp.updateNode(nodeId, title, detail))
        if (wasProvisional) ops += ThinkingOp.setEpistemicState(nodeId, EpistemicState.Asserted)
        return ops
    }

    /** Accept a suggestion as the owner's own. */
    fun acceptSuggestion(nodeId: String): List<JsonObject> =
        listOf(ThinkingOp.setEpistemicState(nodeId, EpistemicState.Asserted))

    /**
     * Reject a suggestion.
     *
     * `rejected`, not a tombstone: the map keeps that the agent proposed it and
     * the owner said no, which is worth more than the node quietly vanishing.
     */
    fun rejectSuggestion(nodeId: String): List<JsonObject> =
        listOf(ThinkingOp.setEpistemicState(nodeId, EpistemicState.Rejected))

    fun deleteNode(nodeId: String): List<JsonObject> = listOf(ThinkingOp.tombstoneNode(nodeId))

    fun connect(edgeId: String, from: String, to: String): List<JsonObject> =
        listOf(ThinkingOp.connect(edgeId, from, to))

    fun disconnect(edgeId: String): List<JsonObject> = listOf(ThinkingOp.disconnect(edgeId))

    /** Answer a question the agent asked about a node. */
    fun answerClarification(clarificationId: String, answer: String): List<JsonObject> =
        listOf(
            ThinkingOp.resolveClarification(
                clarificationId,
                ClarificationState.Answered,
                answer,
            ),
        )

    /**
     * Put a question aside without answering it.
     *
     * Deferred rather than dismissed: the owner is saying "not now", and
     * recording that as "never" would lose a question worth returning to.
     */
    fun deferClarification(clarificationId: String): List<JsonObject> =
        listOf(ThinkingOp.resolveClarification(clarificationId, ClarificationState.Deferred))

    /** Say the question does not apply, and stop being asked it. */
    fun dismissClarification(clarificationId: String): List<JsonObject> =
        listOf(ThinkingOp.resolveClarification(clarificationId, ClarificationState.Dismissed))
}
