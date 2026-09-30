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
 * Ported from `ThinkingGraphView` in `ThinkingMapPrototypeView.swift`: depth
 * decides the horizontal band, position within the band decides the vertical.
 * Nothing is stored — the server keeps no coordinates — so the layout is
 * derived, and both clients must derive it the same way or the same map looks
 * like two different shapes.
 *
 * Normalised rather than in pixels, because the phone that draws this is a
 * different width from the iPad that does, and the arithmetic should not have
 * to know.
 *
 * Pure, so the placement can be checked without a canvas — including the cases
 * that make it fall over: a cycle, a missing parent, a single node.
 */
fun graphLayout(map: ThinkingMap): GraphLayout {
    val nodes = map.nodeList
    if (nodes.isEmpty()) return GraphLayout(emptyList(), emptyList())

    val byId = nodes.associateBy { it.id }
    val depths = nodes.associate { it.id to depthOf(it, byId) }
    // At least one, so a flat map still divides rather than dividing by zero.
    val maxDepth = maxOf(1, depths.values.maxOrNull() ?: 1)

    val points = mutableListOf<GraphPoint>()
    depths.entries
        .groupBy({ it.value }, { it.key })
        .toSortedMap()
        .forEach { (level, ids) ->
            // Sorted so the same map draws the same way twice; the server sends
            // a keyed object and iteration order is not a promise.
            val ordered = ids.sorted()
            ordered.forEachIndexed { index, id ->
                val node = byId[id] ?: return@forEachIndexed
                points += GraphPoint(
                    node = node,
                    x = level.toFloat() / maxDepth.toFloat(),
                    // Spread across the band with a gap at each end, which is
                    // what `count + 1` buys — nodes never touch the edges.
                    y = (index + 1).toFloat() / (ordered.size + 1).toFloat(),
                )
            }
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

/**
 * How far a node sits from its root.
 *
 * Stops on a node already seen, so a cycle resolves to a finite depth instead
 * of climbing forever. A map with a broken link still draws.
 */
private fun depthOf(node: ThinkingNode, byId: Map<String, ThinkingNode>): Int {
    var depth = 0
    val seen = mutableSetOf(node.id)
    var cursor = node.parentId?.let(byId::get)
    while (cursor != null && seen.add(cursor.id)) {
        depth += 1
        cursor = cursor.parentId?.let(byId::get)
    }
    return depth
}
