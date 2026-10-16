package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlin.math.max
import kotlin.math.min

/*
 * The task plan graph the Plan Inspector shows and edits — Android port of
 * iOS `PlanGraphModel.swift` (itself the port of web `plangraph.ts` and the
 * graph helpers inside `PlanGraphView.svelte`: levels, waterfall order,
 * cycle-safe dependency editing, `requestSave`'s edge rebuild). Pure: no
 * Compose, no networking.
 *
 * Nothing the server sent is dropped on save. The Rust `PlanGraph`/`PlanStep`
 * carry more than this app knows (`session_policy`, `planning_metadata`,
 * `success_criteria`, `readiness`, …) and parameters are JSON values, not
 * strings. Every step and the graph keep the object they were decoded from,
 * and the save payload is that object with only the edited fields written
 * over it. JSON values are kotlinx `JsonElement`s, so numbers keep their
 * literal form through a round trip.
 */

/** JSON text helpers for editors and pills. */
object PlanJson {
    private val compactJson = Json
    private val prettyJson = Json { prettyPrint = true }

    /** Parse JSON text (any fragment); null when it is not JSON. */
    fun parse(text: String): JsonElement? =
        runCatching { Json.parseToJsonElement(text) }.getOrNull()?.takeIf { strict(it) }

    /** kotlinx accepts bare words (`1x`) as literals; strict JSON does not. */
    private fun strict(value: JsonElement): Boolean = when (value) {
        is JsonObject -> value.values.all { strict(it) }
        is JsonArray -> value.all { strict(it) }
        is JsonNull -> true
        is JsonPrimitive -> value.isString || value.content == "true" || value.content == "false" ||
            value.content.toDoubleOrNull() != null
    }

    /** One-line text for a pill or an editor: strings bare, the rest as JSON. */
    fun displayText(value: JsonElement): String = when {
        value is JsonPrimitive && value.isString -> value.content
        value is JsonNull -> "null"
        else -> compact(value)
    }

    /** Compact JSON with sorted keys. */
    fun compact(value: JsonElement): String = compactJson.encodeToString(JsonElement.serializer(), sorted(value))

    /** Indented JSON with sorted keys. */
    fun pretty(value: JsonElement): String = prettyJson.encodeToString(JsonElement.serializer(), sorted(value))

    private fun sorted(value: JsonElement): JsonElement = when (value) {
        is JsonObject -> JsonObject(value.entries.sortedBy { it.key }.associate { it.key to sorted(it.value) })
        is JsonArray -> JsonArray(value.map { sorted(it) })
        else -> value
    }
}

/** One parameter of a step, in display order. The value keeps its JSON type. */
data class PlanParameter(val key: String, val value: JsonElement) {
    val id: String get() = key

    companion object {
        /**
         * Parse editor text back to a value: a parameter that was not a string
         * keeps its JSON type when the text is valid JSON; anything else is a
         * string.
         */
        fun valueFromText(text: String, previous: JsonElement?): JsonElement {
            if (previous != null && previous.jStr() == null) PlanJson.parse(text)?.let { return it }
            return JsonPrimitive(text)
        }
    }
}

/** One metadata entry (the server types metadata as string → string). */
data class PlanKeyValue(val key: String, val value: String) {
    val id: String get() = key
}

data class PlanEdge(val from: String, val to: String, val reason: String)

data class PlanProvenance(
    val strategy: String? = null,
    val generator: String? = null,
    val notes: String? = null,
)

data class PlanStep(
    val id: String,
    val task: String,
    val tool: String?,
    val parameters: List<PlanParameter>,
    val expectedOutputs: List<String>,
    val confidence: Double,
    val metadata: List<PlanKeyValue>,
    val providingAgentId: String?,
    val stepHash: String?,
    /** The steps this one waits for — read from the edges; saved back as edges. */
    val dependsOn: List<String>,
    /** The object this step was decoded from (empty for a new step). */
    val raw: JsonObject = JsonObject(emptyMap()),
) {
    val hasResolvedTool: Boolean get() = !(tool ?: "").trim().isEmpty()
    val isDelegated: Boolean get() = !providingAgentId.isNullOrEmpty()
    val isStart: Boolean get() = dependsOn.isEmpty()

    /**
     * The step object to save: the received object with the editable fields
     * written over it. `depends_on` mirrors the rebuilt edges (omitted when
     * empty, as the server serialises it).
     */
    fun savePayload(): JsonObject {
        val obj = raw.toMutableMap()
        obj["id"] = JsonPrimitive(id)
        obj["task"] = JsonPrimitive(task)
        val trimmedTool = tool?.trim() ?: ""
        obj["tool"] = if (trimmedTool.isEmpty()) JsonNull else JsonPrimitive(trimmedTool)
        obj["parameters"] = JsonObject(parameters.associate { it.key to it.value })
        obj["expected_outputs"] = JsonArray(expectedOutputs.map { JsonPrimitive(it) })
        obj["confidence"] = JsonPrimitive(confidence)
        obj["metadata"] = JsonObject(metadata.associate { it.key to JsonPrimitive(it.value) })
        if (dependsOn.isEmpty()) obj.remove("depends_on")
        else obj["depends_on"] = JsonArray(dependsOn.map { JsonPrimitive(it) })
        return JsonObject(obj)
    }

    companion object {
        /** A new step, as the web's Add step makes it. */
        fun new(id: String): PlanStep = PlanStep(
            id = id, task = "New step", tool = null, parameters = emptyList(), expectedOutputs = emptyList(),
            confidence = 0.5, metadata = emptyList(), providingAgentId = null, stepHash = null, dependsOn = emptyList(),
        )
    }
}

data class PlanSummary(
    val steps: Int,
    val edges: Int,
    val unresolved: Int,
    val confidence: Double?,
    val strategy: String?,
)

/**
 * The confidence band a node's fill and a step's badge use (web
 * `getNodeColor`): high ≥ 0.7, medium ≥ 0.4, else low.
 */
enum class PlanConfidenceBand {
    High, Medium, Low;

    companion object {
        fun of(confidence: Double): PlanConfidenceBand = when {
            confidence >= 0.7 -> High
            confidence >= 0.4 -> Medium
            else -> Low
        }
    }
}

/**
 * The plan graph. Value-like: the lists are immutable and reassigned by the
 * edit operations, so `copy()` gives an independent graph (the iOS struct's
 * value semantics) and equality compares content.
 */
data class PlanGraph(
    var steps: List<PlanStep>,
    var edges: List<PlanEdge>,
    /** Opaque: shown as a count, saved back as received. */
    var unresolvedInputs: List<JsonElement>,
    var confidence: Double?,
    var provenance: PlanProvenance,
    /** The graph object as received. */
    var raw: JsonObject,
) {
    fun step(id: String): PlanStep? = steps.firstOrNull { it.id == id }

    /** Header chips: steps · edges · unresolved · confidence · strategy. */
    val summary: PlanSummary
        get() = PlanSummary(
            steps = steps.size,
            edges = steps.sumOf { it.dependsOn.size },
            unresolved = unresolvedInputs.size,
            confidence = confidence,
            strategy = provenance.strategy?.takeIf { it.isNotEmpty() },
        )

    // Dependencies

    /** Steps that wait for `id` (outgoing edges). */
    fun dependents(id: String): List<String> = steps.filter { id in it.dependsOn }.map { it.id }

    private fun depsMap(): Map<String, List<String>> {
        val map = LinkedHashMap<String, List<String>>()
        for (step in steps) if (step.id !in map) map[step.id] = step.dependsOn
        return map
    }

    /** No cycle among the dependencies (web `isPlanAcyclic`). */
    val isAcyclic: Boolean
        get() {
            val deps = depsMap()
            val done = HashSet<String>()
            val onPath = HashSet<String>()
            fun cycle(id: String): Boolean {
                if (id in onPath) return true
                if (id in done) return false
                onPath.add(id)
                val found = (deps[id] ?: emptyList()).any { cycle(it) }
                onPath.remove(id)
                done.add(id)
                return found
            }
            return steps.none { cycle(it.id) }
        }

    /**
     * Making `dep` a dependency of `stepId` would close a cycle: `dep`
     * already waits (transitively) for `stepId`. Web `wouldCreateCycle`.
     */
    fun wouldCreateCycle(stepId: String, dependingOn: String): Boolean {
        if (stepId == dependingOn) return true
        val visited = HashSet<String>()
        val stack = ArrayDeque(listOf(dependingOn))
        while (stack.isNotEmpty()) {
            val next = stack.removeLast()
            if (next == stepId) return true
            if (!visited.add(next)) continue
            val step = step(next) ?: continue
            stack.addAll(step.dependsOn)
        }
        return false
    }

    /**
     * Steps `stepId` may start depending on: not itself, not one it already
     * depends on, and none of its transitive dependents. Web
     * `getAvailableDeps`, in plan order.
     */
    fun dependencyCandidates(stepId: String): List<PlanStep> {
        val step = step(stepId) ?: return emptyList()
        val dependents = HashSet<String>()
        val frontier = ArrayDeque(listOf(stepId))
        while (frontier.isNotEmpty()) {
            val current = frontier.removeLast()
            for (other in steps) {
                if (current in other.dependsOn && dependents.add(other.id)) frontier.addLast(other.id)
            }
        }
        val existing = step.dependsOn.toSet()
        return steps.filter { it.id != stepId && it.id !in existing && it.id !in dependents }
    }

    /** The reason recorded on the edge `from → to`, if any. */
    fun reason(from: String, to: String): String? = edges.firstOrNull { it.from == from && it.to == to }?.reason

    // Levels and waterfall order

    /**
     * Each step's level: 0 for a start step, else 1 + the deepest of its
     * dependencies. A cycle is cut where it closes (web `calculateStepLevel`).
     */
    val levels: Map<String, Int>
        get() {
            val deps = depsMap()
            val memo = HashMap<String, Int>()
            fun level(id: String, path: Set<String>): Int {
                memo[id]?.let { return it }
                if (id in path) return 0
                val known = (deps[id] ?: emptyList()).filter { deps.containsKey(it) }
                if (known.isEmpty()) {
                    memo[id] = 0
                    return 0
                }
                val next = path + id
                val value = 1 + (known.maxOfOrNull { level(it, next) } ?: 0)
                memo[id] = value
                return value
            }
            val result = LinkedHashMap<String, Int>()
            for (step in steps) result[step.id] = level(step.id, emptySet())
            return result
        }

    /**
     * The waterfall's order: a step is placed as soon as all its dependencies
     * are, children visited shallowest first; whatever a cycle strands is
     * appended in plan order. Port of web `topoSortIds`.
     */
    val waterfallOrder: List<String>
        get() {
            val ids = steps.map { it.id }
            val known = ids.toSet()
            val deps = HashMap<String, List<String>>()
            for (step in steps) if (step.id !in deps) deps[step.id] = step.dependsOn.filter { it in known }
            val levels = this.levels
            val children = HashMap<String, MutableList<String>>()
            for (step in steps) {
                for (dep in step.dependsOn) if (dep in known) children.getOrPut(dep) { mutableListOf() }.add(step.id)
            }
            val ordered = mutableListOf<String>()
            val visited = HashSet<String>()
            fun visit(id: String) {
                if (!visited.add(id)) return
                ordered.add(id)
                val sortedChildren = (children[id] ?: emptyList<String>()).withIndex()
                    .sortedWith(compareBy({ levels[it.value] ?: 0 }, { it.index }))
                    .map { it.value }
                for (child in sortedChildren) {
                    if (child !in visited && (deps[child] ?: emptyList()).all { it in visited }) visit(child)
                }
            }
            for (id in ids) if ((deps[id] ?: emptyList()).isEmpty()) visit(id)
            var remaining = ids.filter { it !in visited }
            while (remaining.isNotEmpty()) {
                val before = visited.size
                for (id in remaining) if ((deps[id] ?: emptyList()).all { it in visited }) visit(id)
                remaining = ids.filter { it !in visited }
                if (visited.size == before) {
                    for (id in remaining) if (visited.add(id)) ordered.add(id)
                    break
                }
            }
            return ordered
        }

    /** Steps in waterfall order with their levels. */
    val waterfall: List<Pair<PlanStep, Int>>
        get() {
            val levels = this.levels
            return waterfallOrder.mapNotNull { id -> step(id)?.let { it to (levels[id] ?: 0) } }
        }

    // Editing

    /**
     * Add a step (`step_<epochms>`, confidence 0.5, no tool, no dependencies)
     * at the end. Returns its id.
     */
    fun addStep(nowMillis: Long = System.currentTimeMillis()): String {
        val base = "step_$nowMillis"
        var id = base
        var suffix = 2
        while (step(id) != null) {
            id = "${base}_$suffix"
            suffix += 1
        }
        steps = steps + PlanStep.new(id)
        return id
    }

    /** Remove a step and every dependency on it. */
    fun removeStep(id: String) {
        steps = steps.filter { it.id != id }.map { step ->
            if (id in step.dependsOn) step.copy(dependsOn = step.dependsOn.filter { it != id }) else step
        }
    }

    fun removeSteps(offsets: Set<Int>) {
        val ids = offsets.mapNotNull { steps.getOrNull(it)?.id }
        ids.forEach { removeStep(it) }
    }

    /** Reorder (Steps edit mode drag). Same semantics as Swift `Array.move`. */
    fun moveSteps(fromOffsets: Set<Int>, toOffset: Int) {
        val source = fromOffsets.sorted().filter { it in steps.indices }
        val moving = source.map { steps[it] }
        if (moving.isEmpty()) return
        val before = fromOffsets.count { it < toOffset }
        val rest = steps.toMutableList()
        for (index in source.sortedDescending()) rest.removeAt(index)
        val insertAt = min(max(toOffset - before, 0), rest.size)
        rest.addAll(insertAt, moving)
        steps = rest
    }

    /**
     * Add (`true`) or remove a dependency. Adding one that would close a
     * cycle, or that already exists, changes nothing and returns false.
     */
    fun setDependency(stepId: String, on: String, enabled: Boolean): Boolean {
        val index = steps.indexOfFirst { it.id == stepId }
        if (index < 0) return false
        val current = steps[index]
        val updated = if (enabled) {
            if (step(on) == null || on in current.dependsOn || wouldCreateCycle(stepId, on)) return false
            current.copy(dependsOn = current.dependsOn + on)
        } else {
            if (on !in current.dependsOn) return false
            current.copy(dependsOn = current.dependsOn.filter { it != on })
        }
        steps = steps.toMutableList().also { it[index] = updated }
        return true
    }

    /** Make a step a start step: clear its dependencies. */
    fun makeStart(stepId: String) {
        val index = steps.indexOfFirst { it.id == stepId }
        if (index < 0) return
        steps = steps.toMutableList().also { it[index] = it[index].copy(dependsOn = emptyList()) }
    }

    /** Replace a step's editable fields (its id and raw object are kept). */
    fun updateStep(stepId: String, change: (PlanStep) -> PlanStep) {
        val index = steps.indexOfFirst { it.id == stepId }
        if (index < 0) return
        val original = steps[index]
        val changed = change(original).copy(id = original.id, raw = original.raw)
        steps = steps.toMutableList().also { it[index] = changed }
    }

    /**
     * Re-sort steps into waterfall order (entering edit mode, and after a
     * dependency change — web `sortEditableStepsByLevel`).
     */
    fun sortByWaterfall() {
        val rank = HashMap<String, Int>()
        waterfallOrder.forEachIndexed { index, id -> if (id !in rank) rank[id] = index }
        steps = steps.withIndex()
            .sortedWith(compareBy({ rank[it.value.id] ?: Int.MAX_VALUE }, { it.index }))
            .map { it.value }
    }

    // Save payload

    /**
     * Edges rebuilt from the steps' dependencies, in step order; each keeps
     * the reason its old edge had, else "dependency" (web `rebuildEdges`).
     */
    fun saveEdges(): List<PlanEdge> = steps.flatMap { step ->
        step.dependsOn.map { dep ->
            PlanEdge(dep, step.id, reason(dep, step.id)?.takeIf { it.isNotEmpty() } ?: "dependency")
        }
    }

    /**
     * The `plan_graph` object to PUT: the graph as received, with steps and
     * edges written over it. `unresolved_inputs`, `confidence`, `provenance`
     * and every unknown field are carried through.
     */
    fun savePayload(): JsonObject {
        val obj = raw.toMutableMap()
        obj["steps"] = JsonArray(steps.map { it.savePayload() })
        obj["edges"] = JsonArray(saveEdges().map {
            JsonObject(mapOf("from" to JsonPrimitive(it.from), "to" to JsonPrimitive(it.to), "reason" to JsonPrimitive(it.reason)))
        })
        if ("unresolved_inputs" !in obj) obj["unresolved_inputs"] = JsonArray(unresolvedInputs)
        if ("confidence" !in obj) obj["confidence"] = JsonPrimitive(confidence ?: 0.0)
        if ("provenance" !in obj) {
            obj["provenance"] = JsonObject(mapOf(
                "strategy" to JsonPrimitive(provenance.strategy ?: "manual"),
                "generator" to (provenance.generator?.let { JsonPrimitive(it) } ?: JsonNull),
                "notes" to (provenance.notes?.let { JsonPrimitive(it) } ?: JsonNull),
            ))
        }
        return JsonObject(obj)
    }

    companion object {
        fun empty(): PlanGraph = PlanGraph(
            steps = emptyList(), edges = emptyList(), unresolvedInputs = emptyList(), confidence = null,
            provenance = PlanProvenance(), raw = JsonObject(emptyMap()),
        )

        /**
         * Decode a `plan_graph` object. Missing fields take empty values; null
         * when the value is not an object.
         */
        fun decode(json: JsonElement?): PlanGraph? {
            val obj = json.jObj() ?: return null
            val edges = obj["edges"].jArr().orEmpty().mapNotNull { value ->
                val edge = value.jObj() ?: return@mapNotNull null
                val from = edge["from"].jStr() ?: return@mapNotNull null
                val to = edge["to"].jStr() ?: return@mapNotNull null
                PlanEdge(from, to, edge["reason"].jStr() ?: "")
            }
            val seen = HashSet<String>()
            val decoded = mutableListOf<PlanStep>()
            obj["steps"].jArr().orEmpty().forEachIndexed { index, value ->
                val stepObject = value.jObj() ?: return@forEachIndexed
                var id = stepObject["id"].jStr() ?: ""
                if (id.isEmpty() || id in seen) id = "step_${index + 1}"
                seen.add(id)
                decoded.add(PlanStep(
                    id = id,
                    task = stepObject["task"].jStr() ?: "",
                    tool = stepObject["tool"].jStr(),
                    parameters = stepObject["parameters"].jObj().orEmpty().entries.sortedBy { it.key }
                        .map { PlanParameter(it.key, it.value) },
                    expectedOutputs = stepObject["expected_outputs"].jArr().orEmpty().mapNotNull { it.jStr() },
                    confidence = stepObject["confidence"].jNum() ?: 0.0,
                    metadata = stepObject["metadata"].jObj().orEmpty().entries.sortedBy { it.key }
                        .map { PlanKeyValue(it.key, PlanJson.displayText(it.value)) },
                    providingAgentId = stepObject["providing_agent_id"].jStr(),
                    stepHash = stepObject["step_hash"].jStr(),
                    dependsOn = emptyList(),
                    raw = stepObject,
                ))
            }
            val ids = decoded.map { it.id }.toSet()
            // Edges are the truth (the web reads only them); a step's own
            // `depends_on` fills in only dependencies the edges do not name.
            val steps = decoded.map { step ->
                val deps = mutableListOf<String>()
                for (edge in edges) if (edge.to == step.id && edge.from !in deps) deps.add(edge.from)
                for (dep in step.raw["depends_on"].jArr().orEmpty().mapNotNull { it.jStr() }) {
                    if (dep in ids && dep != step.id && dep !in deps) deps.add(dep)
                }
                step.copy(dependsOn = deps)
            }
            val provenanceObject = obj["provenance"].jObj()
            return PlanGraph(
                steps = steps,
                edges = edges,
                unresolvedInputs = obj["unresolved_inputs"].jArr().orEmpty().toList(),
                confidence = obj["confidence"].jNum(),
                provenance = PlanProvenance(
                    strategy = provenanceObject?.get("strategy").jStr(),
                    generator = provenanceObject?.get("generator").jStr(),
                    notes = provenanceObject?.get("notes").jStr(),
                ),
                raw = obj,
            )
        }
    }
}

// Layered layout

data class PlanPoint(val x: Double, val y: Double)
data class PlanSize(val width: Double, val height: Double)

/** One drawable edge: bottom-centre of the parent to top-centre of the child. */
data class PlanSegment(val edge: PlanEdge, val from: PlanPoint, val to: PlanPoint)

/** Where each node sits on the graph canvas: a top-to-bottom layered DAG. */
class PlanGraphLayout(graph: PlanGraph, val metrics: Metrics = Metrics()) {
    data class Metrics(
        val nodeWidth: Double = 168.0,
        val nodeHeight: Double = 76.0,
        val horizontalGap: Double = 28.0,
        val verticalGap: Double = 56.0,
        val padding: Double = 24.0,
    )

    /** Node centres, in canvas units. */
    val positions: Map<String, PlanPoint>
    val size: PlanSize
    /** Node ids per layer, left to right. */
    val layers: List<List<String>>

    /*
     * Level = 1 + the deepest dependency (0 for a start step). Layer 0 keeps
     * waterfall order; every later layer is ordered by the barycenter (mean x)
     * of each node's parents. Ties keep waterfall order.
     */
    init {
        val levels = graph.levels
        val order = graph.waterfallOrder
        val rank = HashMap<String, Int>()
        order.forEachIndexed { index, id -> if (id !in rank) rank[id] = index }
        val depth = (levels.values.maxOrNull() ?: -1) + 1
        val built = MutableList(max(depth, 0)) { mutableListOf<String>() }
        for (id in order) built[levels[id] ?: 0].add(id)

        val pitchX = metrics.nodeWidth + metrics.horizontalGap
        val pitchY = metrics.nodeHeight + metrics.verticalGap
        val widest = built.maxOfOrNull { it.size } ?: 0
        val span = max(widest - 1, 0) * pitchX
        val x = HashMap<String, Double>()
        for (index in built.indices) {
            if (index > 0) {
                fun barycenter(id: String): Double? {
                    val parents = (graph.step(id)?.dependsOn ?: emptyList()).mapNotNull { x[it] }
                    return if (parents.isEmpty()) null else parents.sum() / parents.size
                }
                val entries = built[index].mapIndexed { offset, id -> Triple(id, offset, barycenter(id)) }
                val sortedEntries = entries.sortedWith { lhs, rhs ->
                    val left = lhs.third
                    val right = rhs.third
                    if (left != null && right != null && left != right) left.compareTo(right)
                    else (rank[lhs.first] ?: lhs.second).compareTo(rank[rhs.first] ?: rhs.second)
                }
                built[index] = sortedEntries.map { it.first }.toMutableList()
            }
            val count = built[index].size
            val start = (span - max(count - 1, 0) * pitchX) / 2
            built[index].forEachIndexed { slot, id -> x[id] = start + slot * pitchX }
        }
        val placed = LinkedHashMap<String, PlanPoint>()
        val originX = metrics.padding + metrics.nodeWidth / 2
        val originY = metrics.padding + metrics.nodeHeight / 2
        built.forEachIndexed { index, layer ->
            for (id in layer) placed[id] = PlanPoint(originX + (x[id] ?: 0.0), originY + index * pitchY)
        }
        positions = placed
        layers = built.map { it.toList() }
        size = if (built.isEmpty()) PlanSize(0.0, 0.0) else PlanSize(
            span + metrics.nodeWidth + metrics.padding * 2,
            (built.size - 1) * pitchY + metrics.nodeHeight + metrics.padding * 2,
        )
    }

    /** Edges as drawable segments: bottom-centre of the parent to top-centre of the child. */
    fun segments(graph: PlanGraph): List<PlanSegment> = graph.saveEdges().mapNotNull { edge ->
        val from = positions[edge.from] ?: return@mapNotNull null
        val to = positions[edge.to] ?: return@mapNotNull null
        val half = metrics.nodeHeight / 2
        PlanSegment(edge, PlanPoint(from.x, from.y + half), PlanPoint(to.x, to.y - half))
    }

    /**
     * The zoom that fits the whole graph in `viewport`, clamped to 0.3–3× and
     * never above 1× (a small graph stays at its natural size).
     */
    fun fitScale(viewport: PlanSize): Double {
        if (size.width <= 0 || size.height <= 0 || viewport.width <= 0 || viewport.height <= 0) return 1.0
        return clampScale(minOf(viewport.width / size.width, viewport.height / size.height, 1.0))
    }

    companion object {
        const val MIN_SCALE = 0.3
        const val MAX_SCALE = 3.0
        fun clampScale(scale: Double): Double = min(max(scale, MIN_SCALE), MAX_SCALE)
    }
}
