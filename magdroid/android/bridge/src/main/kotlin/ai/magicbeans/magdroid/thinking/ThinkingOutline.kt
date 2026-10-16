package ai.magicbeans.magdroid.thinking

/** One node in reading order, with how deep it sits. */
data class OutlineRow(
    val node: ThinkingNode,
    val depth: Int,
)

/**
 * A map as an outline instead of a canvas.
 *
 * A phone shows roughly six nodes at once, which is too few to navigate a graph
 * by panning but plenty to read a structure. Parent/child already carries that
 * structure, so this walks it rather than inventing a layout.
 *
 * Pure, because the interesting cases are all malformed graphs — a parent that
 * was deleted, a cycle, a node that is its own ancestor — and none of them need
 * a screen to reproduce.
 */
fun outline(map: ThinkingMap): List<OutlineRow> {
    val nodes = map.nodeList
    if (nodes.isEmpty()) return emptyList()

    val byId = nodes.associateBy { it.id }
    val children = nodes
        .filter { it.parentId != null && byId.containsKey(it.parentId) && it.parentId != it.id }
        .groupBy { it.parentId!! }

    // A root is anything with no parent, plus anything whose parent is missing.
    // Orphans are shown at the top level rather than dropped: a node the owner
    // wrote is not less real because the thing above it was deleted.
    val roots = nodes.filter { it.parentId == null || !byId.containsKey(it.parentId) || it.parentId == it.id }

    val rows = mutableListOf<OutlineRow>()
    val seen = mutableSetOf<String>()

    fun walk(node: ThinkingNode, depth: Int) {
        // A cycle would otherwise recurse until the stack gives out. Each node
        // is placed once, at the first depth it is reached.
        if (!seen.add(node.id)) return
        rows += OutlineRow(node, depth)
        children[node.id].orEmpty()
            .sortedBy { it.title.lowercase() }
            .forEach { walk(it, depth + 1) }
    }

    roots.forEach { walk(it, 0) }

    // Anything a cycle kept out of the walk still belongs to the map. Appended
    // flat rather than omitted, for the same reason orphans are kept.
    nodes.filterNot { it.id in seen }.forEach { rows += OutlineRow(it, 0) }
    return rows
}
