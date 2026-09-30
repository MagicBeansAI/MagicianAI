package ai.magicbeans.magdroid.thinking

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

val thinkingJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

/**
 * Thinking maps, in the shape the server actually serialises.
 *
 * Modelled from `thinking_map/models.rs` and `store.rs`, not from the iOS
 * client. iOS keeps a *projection* — `id`, `title`, `detail`, `suggested` — over
 * a local `SyncStore` and never calls this REST surface at all, so following it
 * produced a decoder that matched nothing the server sends: `map_id` not `id`,
 * `label` not `title`, nodes as a keyed object rather than an array, and no
 * `snapshot` wrapper anywhere.
 *
 * Worth stating because the mistake was invisible: every field defaulted, so a
 * live map would have decoded to an empty one and the library would have looked
 * simply unused.
 */

/** Where an assertion came from. Decides whether a node is the owner's. */
enum class AssertionOrigin(val id: String) {
    OwnerSpoken("owner_spoken"),
    ParticipantSpoken("participant_spoken"),
    OwnerEdited("owner_edited"),
    ImportedSource("imported_source"),
    ModelInferred("model_inferred"),
    SystemDerived("system_derived"),
    Unknown(""),
    ;

    companion object {
        fun from(raw: String?): AssertionOrigin =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) } ?: Unknown
    }
}

/** How settled a node is. */
enum class EpistemicState(val id: String) {
    Provisional("provisional"),
    Asserted("asserted"),
    Confirmed("confirmed"),
    Contradicted("contradicted"),
    Rejected("rejected"),
    Resolved("resolved"),
    Superseded("superseded"),
    Unknown(""),
    ;

    companion object {
        fun from(raw: String?): EpistemicState =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) } ?: Unknown
    }
}

/**
 * What a node is.
 *
 * Open rather than closed: the server has grown these over time and a client
 * that refused an unfamiliar kind would drop nodes out of somebody's map.
 */
enum class ThinkingNodeKind(val id: String, val label: String) {
    Idea("idea", "Idea"),
    Question("question", "Question"),
    Risk("risk", "Risk"),
    Decision("decision", "Decision"),
    Action("action", "Action"),
    Fact("fact", "Fact"),
    Option("option", "Option"),
    Metric("metric", "Metric"),
    Assumption("assumption", "Assumption"),
    Evidence("evidence", "Evidence"),
    Group("group", "Group"),
    Other("", "Note"),
    ;

    companion object {
        fun from(raw: String?): ThinkingNodeKind =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) } ?: Other
    }
}

@Serializable
data class ThinkingNode(
    @SerialName("node_id") val nodeId: String = "",
    val kind: String = "",
    val label: String = "",
    @SerialName("detail_markdown") val detailMarkdown: String? = null,
    @SerialName("epistemic_state") val epistemicState: String = "",
    @SerialName("assertion_origin") val assertionOrigin: String = "",
    val confidence: Float = 0f,
    @SerialName("parent_id") val parentId: String? = null,
    /**
     * Deleted, and still on the map.
     *
     * `GET /thinking-maps/{id}` returns the document whole — the server filters
     * tombstones out of its own previews and not out of this. Unread, a node
     * the owner deleted stayed on screen until the app was restarted, and even
     * then came back.
     */
    val tombstoned: Boolean = false,
) {
    val id: String get() = nodeId
    val title: String get() = label
    val detail: String get() = detailMarkdown.orEmpty()
    val nodeKind: ThinkingNodeKind get() = ThinkingNodeKind.from(kind)

    /**
     * A node the agent produced, or one not yet settled.
     *
     * The same rule the server uses for its own preview: model-inferred *or*
     * still provisional. Two conditions, because a provisional owner thought is
     * also not yet something they have committed to.
     */
    val suggested: Boolean
        get() = AssertionOrigin.from(assertionOrigin) == AssertionOrigin.ModelInferred ||
            EpistemicState.from(epistemicState) == EpistemicState.Provisional
}

/**
 * How two nodes are joined. `#[serde(rename_all = "snake_case")]` on the server,
 * so the plain relation is `related_to` and not `related`.
 */
enum class EdgeKind(val id: String) {
    RelatedTo("related_to"),
    Supports("supports"),
    Contradicts("contradicts"),
    Answers("answers"),
    DependsOn("depends_on"),
    LeadsTo("leads_to"),
    AlternativeTo("alternative_to"),
    Measures("measures"),
    GroupedUnder("grouped_under"),
    ;

    companion object {
        fun from(raw: String?): EdgeKind? =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) }
    }
}

/**
 * A directed link between two nodes.
 *
 * The endpoints are `from_node` and `to_node` — not `from_node_id`/`to_node_id`,
 * which is what this read for its whole life. Every edge therefore decoded with
 * two empty ends, so no connection could be drawn, walked, or removed.
 */
@Serializable
data class ThinkingEdge(
    @SerialName("edge_id") val edgeId: String = "",
    @SerialName("from_node") val from: String = "",
    @SerialName("to_node") val to: String = "",
    val kind: String = "",
    @SerialName("assertion_origin") val assertionOrigin: String = "",
    /** Removed, but kept on the map. The payload is not filtered for us. */
    val tombstoned: Boolean = false,
) {
    val id: String get() = edgeId
    val edgeKind: EdgeKind? get() = EdgeKind.from(kind)
}

/** Where a clarification stands. `#[serde(rename_all = "snake_case")]` on the server. */
enum class ClarificationState(val id: String) {
    Open("open"),
    Answered("answered"),
    Deferred("deferred"),
    Dismissed("dismissed"),
    ;

    companion object {
        fun from(raw: String?): ClarificationState =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) } ?: Open
    }
}

/**
 * A question the agent asked about a node.
 *
 * The map carries these and this client never read them, so a question that
 * stopped the agent from doing anything useful simply never appeared — the map
 * looked idle rather than blocked.
 */
@Serializable
data class ThinkingClarification(
    @SerialName("clarification_id") val clarificationId: String = "",
    @SerialName("node_id") val nodeId: String = "",
    val question: String = "",
    val state: String = ClarificationState.Open.id,
    val answer: String? = null,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("resolved_at") val resolvedAt: String? = null,
) {
    val id: String get() = clarificationId
    val isOpen: Boolean get() = ClarificationState.from(state) == ClarificationState.Open
}

/** Where a restructure proposal stands. */
enum class ProposalState(val id: String) {
    Proposed("proposed"),
    Confirmed("confirmed"),
    Rejected("rejected"),
    Deferred("deferred"),
    ;

    companion object {
        fun from(raw: String?): ProposalState =
            entries.firstOrNull { it.id.equals(raw?.trim(), ignoreCase = true) } ?: Proposed
    }
}

/**
 * A restructuring the agent wants to make, waiting on the owner.
 *
 * `operations` is deliberately not decoded: the inner ops are the server's to
 * apply on confirm, and re-deriving them here would be a second interpretation
 * of the same list. What a reader needs is the rationale and which nodes move.
 */
@Serializable
data class RestructureProposal(
    @SerialName("proposal_id") val proposalId: String = "",
    val rationale: String = "",
    val state: String = ProposalState.Proposed.id,
    @SerialName("affected_node_ids") val affectedNodeIds: List<String> = emptyList(),
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("resolved_at") val resolvedAt: String? = null,
) {
    val id: String get() = proposalId
    val isOpen: Boolean get() = ProposalState.from(state) == ProposalState.Proposed
}

/**
 * One map, whole.
 *
 * `nodes` and `edges` arrive as objects keyed by id — a Rust `BTreeMap` — so
 * they decode as maps and the ordered lists are derived.
 */
@Serializable
data class ThinkingMap(
    @SerialName("map_id") val mapId: String = "",
    val title: String = "",
    val lifecycle: String = "",
    val revision: Long = 0,
    val nodes: Map<String, ThinkingNode> = emptyMap(),
    val edges: Map<String, ThinkingEdge> = emptyMap(),
    // Keyed by id, like nodes and edges. Both were sent on every map payload and
    // neither was read: a clarification the agent asked went unseen, and a
    // restructure proposal could not be answered because no proposal id ever
    // reached this client — `decideProposal` had nothing to decide about.
    val clarifications: Map<String, ThinkingClarification> = emptyMap(),
    val proposals: Map<String, RestructureProposal> = emptyMap(),
    @SerialName("view_state") val viewState: SharedViewState = SharedViewState(),
) {
    val id: String get() = mapId
    val displayTitle: String get() = title.ifBlank { "Untitled map" }
    val isArchived: Boolean get() = lifecycle.equals("archived", ignoreCase = true)

    /**
     * The live nodes. Tombstones are dropped here rather than at every reader,
     * because the map arrives with them and every surface would otherwise have
     * to remember — and one that forgot would show a deleted thought.
     */
    val nodeList: List<ThinkingNode> get() = nodes.values.filterNot { it.tombstoned }

    /** The live edges, for the same reason. */
    val edgeList: List<ThinkingEdge> get() = edges.values.filterNot { it.tombstoned }

    /**
     * Clarifications still waiting on an answer, oldest first.
     *
     * Only the open ones: an answered question is part of the record, not
     * something to ask again.
     */
    val openClarifications: List<ThinkingClarification>
        get() = clarifications.values
            .filter { it.state == ClarificationState.Open.id }
            .sortedBy { it.createdAt }

    /** Restructure proposals still awaiting a decision, oldest first. */
    val openProposals: List<RestructureProposal>
        get() = proposals.values
            .filter { it.state == ProposalState.Proposed.id }
            .sortedBy { it.createdAt }

    /** The node the shared view is focused on, when there is one. */
    val activeNodeId: String? get() = viewState.focusNodeId

    val activeThought: String
        get() = activeNodeId?.let { nodes[it]?.label }
            ?: nodeList.firstOrNull()?.label
            ?: "Unstarted idea"

    /** What the owner put there, as against what was inferred or provisional. */
    val capturedCount: Int get() = nodeList.count { !it.suggested }
    val questionCount: Int get() = nodeList.count { it.nodeKind == ThinkingNodeKind.Question }
    val actionCount: Int get() = nodeList.count { it.nodeKind == ThinkingNodeKind.Action }

    fun matches(query: String): Boolean {
        val needle = query.trim().lowercase()
        if (needle.isEmpty()) return true
        if (displayTitle.lowercase().contains(needle)) return true
        return nodeList.any {
            it.label.lowercase().contains(needle) || it.detail.lowercase().contains(needle)
        }
    }
}

@Serializable
data class SharedViewState(
    @SerialName("focus_node_id") val focusNodeId: String? = null,
)

/**
 * A row in the library.
 *
 * The list endpoint returns summaries, not whole maps — the node preview is a
 * bounded handful for the card, and the map itself is fetched on open.
 */
@Serializable
data class ThinkingMapSummary(
    @SerialName("map_id") val mapId: String = "",
    val title: String = "",
    val lifecycle: String = "",
    @SerialName("latest_revision") val latestRevision: Long = 0,
    @SerialName("updated_at") val updatedAt: String = "",
    @SerialName("node_preview") val nodePreview: NodePreview? = null,
) {
    val id: String get() = mapId
    val displayTitle: String get() = title.ifBlank { "Untitled map" }
    val isArchived: Boolean get() = lifecycle.equals("archived", ignoreCase = true)

    /**
     * The thought the card leads with.
     *
     * From the preview when the server sent one; otherwise nothing, because
     * inventing a headline from a title the owner already sees would just
     * repeat it.
     */
    val previewThought: String?
        get() = nodePreview?.nodes?.firstOrNull { !it.suggested }?.title
            ?: nodePreview?.nodes?.firstOrNull()?.title

    val previewCount: Int get() = nodePreview?.nodes?.size ?: 0

    fun matches(query: String): Boolean {
        val needle = query.trim().lowercase()
        if (needle.isEmpty()) return true
        if (displayTitle.lowercase().contains(needle)) return true
        return nodePreview?.nodes.orEmpty().any { it.title.lowercase().contains(needle) }
    }
}

@Serializable
data class NodePreview(
    val nodes: List<NodePreviewNode> = emptyList(),
)

@Serializable
data class NodePreviewNode(
    @SerialName("node_id") val nodeId: String = "",
    @SerialName("parent_id") val parentId: String? = null,
    val kind: String = "",
    /** The server computes this with the same rule the open map uses. */
    val suggested: Boolean = false,
    val title: String = "",
)
