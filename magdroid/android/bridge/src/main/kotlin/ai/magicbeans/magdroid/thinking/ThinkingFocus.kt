package ai.magicbeans.magdroid.thinking

/**
 * One node, and the ways out of it.
 *
 * This is how the map is actually navigated — iOS focuses a single node with
 * its ancestry above, its branches below, and whatever it is connected to
 * sideways. The outline reads a whole map at once; this walks it.
 *
 * Pure for the same reason the outline is: the interesting inputs are broken
 * graphs, and none of them need a screen.
 */
data class ThinkingFocus(
    /** Root first, ending at the focused node itself. */
    val ancestry: List<ThinkingNode>,
    val node: ThinkingNode?,
    /** Where to go next. Captured before suggested, oldest first. */
    val branches: List<ThinkingNode>,
    /** Joined by a `related` edge rather than by parentage. */
    val related: List<ThinkingNode>,
) {
    val isEmpty: Boolean get() = node == null
}

/**
 * Focus a node, falling back to the map's own active node.
 *
 * A map whose nodes have loaded before a selection publishes must still render
 * something, so an absent selection resolves to the active node and then to the
 * first — the same resolution the map row uses for its headline.
 */
fun focus(map: ThinkingMap, nodeId: String? = null): ThinkingFocus {
    val byId = map.nodeList.associateBy { it.id }
    val active = nodeId?.let(byId::get)
        ?: map.activeNodeId?.let(byId::get)
        ?: map.nodeList.firstOrNull()
        ?: return ThinkingFocus(emptyList(), null, emptyList(), emptyList())

    return ThinkingFocus(
        ancestry = ancestryOf(active, byId),
        node = active,
        branches = branchesOf(active.id, map.nodeList),
        related = relatedTo(active.id, map),
    )
}

/**
 * The path from the root down to this node.
 *
 * Walks parents and stops on a node already seen, so a cycle ends the climb
 * instead of hanging it. Reversed at the end because a breadcrumb reads
 * outermost-first.
 */
private fun ancestryOf(node: ThinkingNode, byId: Map<String, ThinkingNode>): List<ThinkingNode> {
    val path = mutableListOf<ThinkingNode>()
    val visited = mutableSetOf<String>()
    var cursor: ThinkingNode? = node
    while (cursor != null && visited.add(cursor.id)) {
        path += cursor
        cursor = cursor.parentId?.let(byId::get)
    }
    return path.reversed()
}

/**
 * Children, with what the owner captured ahead of what was suggested.
 *
 * The order is the point: a branch somebody wrote themselves should not sit
 * below one the agent proposed.
 */
private fun branchesOf(id: String, nodes: List<ThinkingNode>): List<ThinkingNode> =
    nodes.filter { it.parentId == id && it.id != id }
        .sortedWith(compareBy({ it.suggested }, { it.title.lowercase() }))

/** Nodes joined by a `related` edge, in either direction. */
private fun relatedTo(id: String, map: ThinkingMap): List<ThinkingNode> {
    val byId = map.nodeList.associateBy { it.id }
    return map.edgeList
        // `related_to`, the server's own snake_case spelling. Filtering on
        // "related" matched nothing the backend has ever sent.
        .filter { it.edgeKind == EdgeKind.RelatedTo }
        .mapNotNull { edge ->
            when (id) {
                edge.from -> edge.to
                edge.to -> edge.from
                else -> null
            }
        }
        // A node is not related to itself, and one edge should not list the
        // same neighbour twice.
        .filter { it != id }
        .distinct()
        .mapNotNull(byId::get)
}
