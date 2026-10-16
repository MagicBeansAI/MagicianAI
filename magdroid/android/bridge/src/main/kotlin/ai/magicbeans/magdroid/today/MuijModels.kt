package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull

/** Validated, read-only GAUI/MUIJ model used by published mobile surfaces. */
data class MuijDocument(
    val version: String,
    val agentId: String,
    val layout: List<MuijComponent>,
) {
    companion object {
        const val MaximumDepth = 32
        const val MaximumTopLevelComponents = 500
        const val MaximumTotalComponents = 2_000

        fun parse(raw: JsonElement): MuijParseResult {
            val envelope = raw as? JsonObject ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidEnvelope)
            val version = envelope.string("muij_version")?.trim()
                ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidEnvelope)
            val agentId = envelope.string("agent_id")?.trim()
                ?.takeIf(String::isNotEmpty)
                ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidEnvelope)
            val layout = envelope["layout"] as? JsonArray
                ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidEnvelope)
            if (version != "1.0") return MuijParseResult.Invalid(MuijInvalidReason.UnsupportedVersion(version))
            if (layout.size > MaximumTopLevelComponents) return MuijParseResult.Invalid(MuijInvalidReason.TooManyComponents)

            val stack = ArrayDeque<Pair<JsonElement, Int>>()
            layout.forEach { stack.addLast(it to 0) }
            val seen = mutableSetOf<String>()
            var count = 0
            while (stack.isNotEmpty()) {
                val (candidate, depth) = stack.removeLast()
                if (depth >= MaximumDepth) return MuijParseResult.Invalid(MuijInvalidReason.NestingTooDeep)
                val component = candidate as? JsonObject
                    ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent("unknown"))
                val id = component.string("id")?.trim()?.takeIf(String::isNotEmpty)
                    ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent("unknown"))
                val type = component.string("component_type")?.trim()?.takeIf(String::isNotEmpty)
                    ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent(id))
                component.string("label")
                    ?: return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent(id))
                if (!seen.add(id)) return MuijParseResult.Invalid(MuijInvalidReason.DuplicateId(id))
                if (component["props"] != null && component["props"] !is JsonObject) {
                    return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent(id))
                }
                if (component["children"] != null && component["children"] !is JsonArray) {
                    return MuijParseResult.Invalid(MuijInvalidReason.InvalidComponent(id))
                }
                count += 1
                if (count > MaximumTotalComponents) return MuijParseResult.Invalid(MuijInvalidReason.TooManyComponents)
                (component["children"] as? JsonArray)?.forEach { stack.addLast(it to depth + 1) }
            }

            return MuijParseResult.Valid(MuijDocument(
                version = version,
                agentId = agentId,
                layout = layout.mapNotNull { MuijComponent.decode(it, 0) },
            ))
        }
    }
}

data class MuijComponent(
    val id: String,
    val type: String,
    val label: String,
    val props: JsonObject,
    val staticSnapshot: JsonElement?,
    val children: List<MuijComponent>,
) {
    companion object {
        internal fun decode(raw: JsonElement, depth: Int): MuijComponent? {
            if (depth >= MuijDocument.MaximumDepth) return null
            val value = raw as? JsonObject ?: return null
            val id = value.string("id") ?: return null
            val type = value.string("component_type") ?: return null
            return MuijComponent(
                id = id,
                type = type,
                label = value.string("label").orEmpty(),
                props = value["props"] as? JsonObject ?: JsonObject(emptyMap()),
                staticSnapshot = value["static_snapshot"],
                children = (value["children"] as? JsonArray).orEmpty().mapNotNull { decode(it, depth + 1) },
            )
        }
    }

    fun string(key: String, fallback: String = ""): String = props.string(key) ?: fallback
    fun number(key: String): Double? = (props[key] as? JsonPrimitive)?.doubleOrNull
    fun boolean(key: String, fallback: Boolean = false): Boolean {
        val value = props[key] as? JsonPrimitive ?: return fallback
        value.booleanOrNull?.let { return it }
        value.doubleOrNull?.let { return it != 0.0 }
        return when (value.contentOrNull?.trim()?.lowercase()) {
            "true", "1", "yes", "on" -> true
            "false", "0", "no", "off", "" -> false
            else -> fallback
        }
    }

    val displayLabel: String get() = string("title", string("label", label))
    val tableRows: List<JsonObject> get() = arrayProp("rows").ifEmpty {
        (staticSnapshot as? JsonArray).orEmpty()
    }.mapNotNull { it as? JsonObject }.take(200)

    val tableColumns: List<MuijTableColumn> get() {
        val explicit = arrayProp("columns").mapNotNull { raw ->
            when (raw) {
                is JsonPrimitive -> raw.contentOrNull?.let { MuijTableColumn(it, it) }
                is JsonObject -> {
                    val key = raw.string("key") ?: raw.string("label") ?: return@mapNotNull null
                    MuijTableColumn(key, raw.string("label") ?: key)
                }
                else -> null
            }
        }.distinctBy(MuijTableColumn::key).take(12)
        if (explicit.isNotEmpty()) return explicit
        return tableRows.firstOrNull()?.keys?.sorted()?.take(12)?.map { MuijTableColumn(it, it) }.orEmpty()
    }

    val metricValue: String get() {
        string("value").takeIf(String::isNotEmpty)?.let { return it }
        val record = (staticSnapshot as? JsonArray)?.firstOrNull() as? JsonObject ?: return "—"
        val field = string("valueField").takeIf(String::isNotEmpty) ?: record.keys.sorted().firstOrNull() ?: return "—"
        return record[field].compactDisplay()
    }

    val chartData: List<MuijChartPoint> get() {
        val raw = arrayProp("data").ifEmpty { (staticSnapshot as? JsonArray).orEmpty() }
        val labelField = string("labelField", string("xField", "label"))
        val valueField = string("valueField", string("yField", "value"))
        return raw.take(30).mapIndexedNotNull { index, item ->
            val record = item as? JsonObject ?: return@mapIndexedNotNull null
            val number = (record[valueField] as? JsonPrimitive)?.doubleOrNull
                ?.takeIf(Double::isFinite) ?: return@mapIndexedNotNull null
            MuijChartPoint(index, record[labelField].compactDisplay().takeIf(String::isNotBlank) ?: "Item ${index + 1}", number)
        }
    }

    val graphModel: MuijGraphModel
        get() {
            val seen = mutableSetOf<String>()
            val nodes = mutableListOf<MuijGraphNode>()
            for (raw in arrayProp("nodes")) {
                if (nodes.size >= MuijGraphModel.MaximumNodes) break
                val record = raw as? JsonObject ?: continue
                // Ids compare exactly like the Rust validator (no trim), so a
                // valid document's edges always resolve; the id-length cap
                // drops over-long ids like every other cap violation. Every
                // cap counts Unicode code points (mirroring Svelte's
                // codePointLength and the Rust validator's chars()), not
                // UTF-16 units.
                val id = record.string("id")
                    ?.takeIf {
                        it.isNotEmpty() &&
                            it.codePointCount(0, it.length) <= MuijGraphModel.MaximumNodeIdChars
                    }
                    ?: continue
                if (!seen.add(id)) continue
                val label = record.string("label")
                if (label != null && label.codePointCount(0, label.length) > MuijGraphModel.MaximumTextChars) continue
                val metaObject = record["metadata"] as? JsonObject
                val metadata = metaObject
                    ?.keys?.sorted()
                    ?.mapNotNull { key ->
                        if (key.codePointCount(0, key.length) > MuijGraphModel.MaximumMetadataKeyChars) return@mapNotNull null
                        val value = metaObject[key].compactDisplay()
                        if (value.codePointCount(0, value.length) > MuijGraphModel.MaximumTextChars) null else MuijGraphMetaEntry(key, value)
                    }
                    ?.take(MuijGraphModel.MaximumMetadataEntries)
                    .orEmpty()
                nodes += MuijGraphNode(
                    id = id,
                    label = label ?: id,
                    kind = record.string("kind")
                        ?.takeIf {
                            it.isNotEmpty() &&
                                it.codePointCount(0, it.length) <= MuijGraphModel.MaximumTextChars
                        },
                    metadata = metadata,
                )
            }

            val ids = nodes.mapTo(mutableSetOf()) { it.id }
            val edges = mutableListOf<MuijGraphEdge>()
            for (raw in arrayProp("edges")) {
                if (edges.size >= MuijGraphModel.MaximumEdges) break
                val record = raw as? JsonObject ?: continue
                val from = record.string("from")?.takeIf(String::isNotEmpty) ?: continue
                val to = record.string("to")?.takeIf(String::isNotEmpty) ?: continue
                if (from !in ids || to !in ids) continue
                edges += MuijGraphEdge(
                    from,
                    to,
                    record.string("label")
                        ?.takeIf {
                            it.isNotEmpty() &&
                                it.codePointCount(0, it.length) <= MuijGraphModel.MaximumTextChars
                        },
                )
            }

            val layout = when (string("layout")) {
                "radial" -> MuijGraphLayout.RADIAL
                "list" -> MuijGraphLayout.LIST
                else -> MuijGraphLayout.LAYERED
            }
            val focus = (string("focus_node_id").ifEmpty { string("focusNodeId") })
                .takeIf { it.isNotEmpty() && it in ids }
            val reveal = (props["reveal_order"] as? JsonArray ?: props["revealOrder"] as? JsonArray)
                ?.mapNotNull { (it as? JsonPrimitive)?.contentOrNull }
                ?.filter { it in ids }
                ?.take(MuijGraphModel.MaximumNodes)
                .orEmpty()

            return MuijGraphModel(
                nodes = MuijGraphModel.assignTiers(nodes, edges),
                edges = edges,
                layout = layout,
                focusNodeId = focus,
                revealOrder = reveal,
            )
        }

    fun arrayProp(key: String): List<JsonElement> = (props[key] as? JsonArray).orEmpty()
}

data class MuijTableColumn(val key: String, val label: String)
data class MuijChartPoint(val id: Int, val label: String, val value: Double)

/** Deterministic graph layouts admitted by the MUIJ `Graph` family. */
enum class MuijGraphLayout { LAYERED, RADIAL, LIST }

data class MuijGraphMetaEntry(val key: String, val value: String)

data class MuijGraphNode(
    val id: String,
    val label: String,
    val kind: String? = null,
    val metadata: List<MuijGraphMetaEntry> = emptyList(),
    val tier: Int = 0,
)

data class MuijGraphEdge(val from: String, val to: String, val label: String? = null)

/**
 * Bounded, fail-soft graph model behind the native renderer. Mirrors the
 * Rust validator caps (200 nodes / 400 edges); malformed members and
 * dangling edges are skipped so one hostile document degrades to a smaller
 * graph instead of failing the whole briefing.
 */
data class MuijGraphModel(
    val nodes: List<MuijGraphNode>,
    val edges: List<MuijGraphEdge>,
    val layout: MuijGraphLayout,
    val focusNodeId: String? = null,
    val revealOrder: List<String> = emptyList(),
) {
    companion object {
        const val MaximumNodes = 200
        const val MaximumEdges = 400
        const val MaximumNodeIdChars = 128
        const val MaximumTextChars = 200
        const val MaximumMetadataEntries = 8
        const val MaximumMetadataKeyChars = 64

        /** Deterministic topological tiers (Kahn); cycle members share the final tier. */
        fun assignTiers(nodes: List<MuijGraphNode>, edges: List<MuijGraphEdge>): List<MuijGraphNode> {
            val position = nodes.indices.associate { nodes[it].id to it }
            val incoming = IntArray(nodes.size)
            val outgoing = mutableMapOf<Int, MutableList<Int>>()
            for (edge in edges) {
                val from = position[edge.from] ?: continue
                val to = position[edge.to] ?: continue
                if (from == to) continue
                incoming[to] += 1
                outgoing.getOrPut(from) { mutableListOf() }.add(to)
            }
            val tiers = IntArray(nodes.size)
            var frontier = nodes.indices.filter { incoming[it] == 0 }.sorted()
            var tier = 0
            val placed = mutableSetOf<Int>()
            while (frontier.isNotEmpty()) {
                for (index in frontier) {
                    tiers[index] = tier
                    placed += index
                }
                val next = mutableListOf<Int>()
                for (index in frontier) {
                    for (target in outgoing[index].orEmpty()) {
                        if (incoming[target] > 0) {
                            incoming[target] -= 1
                            if (incoming[target] == 0 && target !in placed && target !in next) next += target
                        }
                    }
                }
                frontier = next.sorted()
                tier += 1
            }
            for (index in tiers.indices) if (index !in placed) tiers[index] = tier
            return nodes.mapIndexed { index, node -> node.copy(tier = tiers[index]) }
        }
    }
}

sealed interface MuijParseResult {
    data class Valid(val document: MuijDocument) : MuijParseResult
    data class Invalid(val reason: MuijInvalidReason) : MuijParseResult
}

sealed interface MuijInvalidReason {
    val message: String
    data object InvalidEnvelope : MuijInvalidReason { override val message = "This briefing has an invalid dashboard document." }
    data class UnsupportedVersion(val version: String) : MuijInvalidReason { override val message = "Dashboard version $version is not supported yet." }
    data object TooManyComponents : MuijInvalidReason { override val message = "This dashboard is too large to render safely." }
    data object NestingTooDeep : MuijInvalidReason { override val message = "This dashboard is nested too deeply to render safely." }
    data class DuplicateId(val id: String) : MuijInvalidReason { override val message = "This dashboard contains a duplicate component ($id)." }
    data class InvalidComponent(val id: String) : MuijInvalidReason { override val message = "Dashboard component $id is malformed." }
}

data class MuijPresentationRow(val key: String, val value: String)

fun JsonElement?.compactDisplay(): String = when (this) {
    null, JsonNull -> "—"
    is JsonPrimitive -> booleanOrNull?.let { if (it) "Yes" else "No" }
        ?: contentOrNull.orEmpty().let { content ->
            // Number rendering matches web/Swift ("2", not "2.0"): strip the
            // trailing ".0" a whole double carries in its JSON content. String
            // primitives keep their exact text.
            if (!isString && content.endsWith(".0")) content.dropLast(2) else content
        }
    is JsonArray -> "$size items"
    is JsonObject -> "$size fields"
    else -> "—"
}

fun JsonElement.presentationRows(limit: Int = 100): List<MuijPresentationRow> = when (this) {
    is JsonObject -> keys.sorted().take(limit).map { MuijPresentationRow(it.humanized(), get(it).compactDisplay()) }
    is JsonArray -> take(limit).mapIndexed { index, value -> MuijPresentationRow("Item ${index + 1}", value.compactDisplay()) }
    else -> listOf(MuijPresentationRow("", compactDisplay()))
}

private fun JsonObject.string(key: String): String? = (get(key) as? JsonPrimitive)?.contentOrNull
private fun String.humanized(): String = replace('_', ' ').replace('-', ' ').split(' ').joinToString(" ") {
    it.replaceFirstChar(Char::uppercaseChar)
}
