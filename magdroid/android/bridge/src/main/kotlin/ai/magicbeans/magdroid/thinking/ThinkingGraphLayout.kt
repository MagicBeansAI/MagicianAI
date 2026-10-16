package ai.magicbeans.magdroid.thinking

/** A node placed on the canvas, in 0..1 space. */
data class GraphPoint(
    val node: ThinkingNode,
    /** Fractions of the drawing area, so the layout survives any size. */
    val x: Float,
    val y: Float,
)

/** A branch edge, already resolved to two placed ends. */
data class GraphLink(
    val from: GraphPoint,
    val to: GraphPoint,
)

data class GraphLayout(
    val points: List<GraphPoint>,
    val links: List<GraphLink>,
)

/**
 * Where each node sits when the map is drawn.
 *
 * A tree by `parentId`: depth decides the horizontal band, and within the band
 * each parent sits centred beside its own children, siblings in creation
 * order. Web (`treeLayout.ts`) and iOS (`ThinkingGraphLayout`) place nodes by
 * the same rule — they draw it top-down, this draws it left to right — so one
 * map is one shape on every client. Edges are drawn but never place anything:
 * the tree is authored (the interpreter sets `parent_id`), not guessed.
 *
 * Normalised rather than in pixels, because the phone that draws this is a
 * different width from the iPad that does, and the arithmetic should not have
 * to know.
 *
 * Pure, so the placement can be checked without a canvas — including the cases
 * that make it fall over: a cycle, a missing parent, a single node.
 */
fun graphLayout(map: ThinkingMap): GraphLayout {
    val nodes = map.nodeList.sortedWith(compareBy({ it.createdAt }, { it.id }))
    if (nodes.isEmpty()) return GraphLayout(emptyList(), emptyList())

    val ids = nodes.mapTo(mutableSetOf()) { it.id }
    // A parent counts only when it is another node on this map.
    fun parentOf(node: ThinkingNode): String? =
        node.parentId?.takeIf { it != node.id && it in ids }
    val children = nodes.filter { parentOf(it) != null }.groupBy({ parentOf(it)!! }, { it.id })

    // Breadth in sibling slots, depth in bands. A leaf takes the next slot; a
    // parent sits midway between its first and last child.
    val slot = mutableMapOf<String, Float>()
    val depth = mutableMapOf<String, Int>()
    var nextSlot = 0f
    fun place(id: String, level: Int) {
        depth[id] = level
        val kids = children[id].orEmpty().filter { it !in depth }
        kids.forEach { place(it, level + 1) }
        val placed = kids.mapNotNull { slot[it] }
        if (placed.isEmpty()) {
            slot[id] = nextSlot
            nextSlot += 1f
        } else {
            slot[id] = (placed.first() + placed.last()) / 2f
        }
    }
    var trees = 0
    fun plant(id: String) {
        // A little air between unrelated trees.
        if (trees > 0) nextSlot += TREE_GAP
        place(id, 0)
        trees += 1
    }
    nodes.filter { parentOf(it) == null }.forEach { plant(it.id) }
    // Whatever is still unplaced hangs off a parent cycle and no root reaches
    // it; its earliest node heads a tree rather than vanishing.
    nodes.forEach { if (it.id !in depth) plant(it.id) }

    // At least one, so a flat map still divides rather than dividing by zero.
    val maxDepth = maxOf(1, depth.values.maxOrNull() ?: 0)
    val points = nodes.map { node ->
        GraphPoint(
            node = node,
            x = depth.getValue(node.id).toFloat() / maxDepth.toFloat(),
            // A gap at each end, which is what `+ 1` buys — nodes never touch
            // the edges. `nextSlot` is one past the last leaf.
            y = (slot.getValue(node.id) + 1f) / (nextSlot + 1f),
        )
    }

    val placed = points.associateBy { it.node.id }
    // Every live edge, whatever it means. This filtered for `branch`, which is
    // not one of the server's edge kinds at all — so the canvas drew nodes and
    // never once drew a line between them.
    //
    // Not narrowed to `related_to` the way Focus is: Focus answers "what else
    // is this about", where the canvas is showing the shape of the map, and a
    // `supports` or `contradicts` link is part of that shape.
    val links = map.edgeList
        .mapNotNull { edge ->
            val from = placed[edge.from] ?: return@mapNotNull null
            val to = placed[edge.to] ?: return@mapNotNull null
            GraphLink(from, to)
        }

    return GraphLayout(points, links)
}

/** Extra room between two unrelated trees, as a fraction of a sibling slot. */
private const val TREE_GAP = 0.3f
