package ai.magicbeans.magdroid.tasks

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Builds a JSON value from Kotlin literals for the plan tests (the iOS tests'
 * `[String: Any]` fixtures): maps, lists, strings, numbers, booleans, null.
 */
internal fun planJson(value: Any?): JsonElement = when (value) {
    null -> JsonNull
    is JsonElement -> value
    is String -> JsonPrimitive(value)
    is Number -> JsonPrimitive(value)
    is Boolean -> JsonPrimitive(value)
    is Map<*, *> -> JsonObject(value.entries.associate { (k, v) -> k as String to planJson(v) })
    is List<*> -> JsonArray(value.map { planJson(it) })
    else -> error("unsupported $value")
}

internal fun planObj(vararg pairs: Pair<String, Any?>): JsonObject = planJson(mapOf(*pairs)) as JsonObject

/**
 * The plan graph helpers behind the Plan Inspector. One-for-one port of iOS
 * `PlanGraphModelTests` (web `plangraph.ts` `isPlanAcyclic`, and
 * `PlanGraphView.svelte` levels, `topoSortIds`, `getAvailableDeps`,
 * `wouldCreateCycle`, edit ops, `rebuildEdges`, `requestSave`).
 */
class PlanGraphTest {
    private fun step(id: String, extra: Map<String, Any?> = emptyMap()): Map<String, Any?> =
        mapOf<String, Any?>(
            "id" to id, "task" to "Task $id", "tool" to "tool_$id", "parameters" to emptyMap<String, Any>(),
            "expected_outputs" to emptyList<Any>(), "confidence" to 0.8, "metadata" to emptyMap<String, Any>(),
        ) + extra

    private fun graph(
        steps: List<Map<String, Any?>>,
        edges: List<Triple<String, String, String>>,
        extra: Map<String, Any?> = emptyMap(),
    ): PlanGraph {
        val obj = mapOf<String, Any?>(
            "steps" to steps,
            "edges" to edges.map { mapOf("from" to it.first, "to" to it.second, "reason" to it.third) },
            "unresolved_inputs" to emptyList<Any>(), "confidence" to 0.7,
            "provenance" to mapOf("strategy" to "MCTS", "generator" to null, "notes" to null),
        ) + extra
        return PlanGraph.decode(planJson(obj))!!
    }

    /** a → b, a → c, b → d, c → d; e alone. Steps listed out of order. */
    private val diamond: PlanGraph
        get() = graph(
            listOf(step("d"), step("c"), step("b"), step("a"), step("e")),
            listOf(Triple("a", "b", "first"), Triple("a", "c", "second"), Triple("b", "d", "join b"), Triple("c", "d", "join c")),
        )

    // Decoding

    @Test fun decodes_tolerantly_when_fields_are_missing() {
        val decoded = PlanGraph.decode(planJson(mapOf("steps" to listOf(mapOf("task" to "No id"), mapOf("id" to "x")))))!!
        assertEquals(listOf("step_1", "x"), decoded.steps.map { it.id })
        assertEquals("", decoded.steps[1].task)
        assertNull(decoded.steps[1].tool)
        assertEquals(0.0, decoded.steps[1].confidence, 0.0)
        assertTrue(decoded.edges.isEmpty())
        assertNull(decoded.confidence)
        assertNull(PlanGraph.decode(planJson("not an object")))
    }

    @Test fun dependencies_come_from_edges_and_fill_in_from_a_steps_own_depends_on() {
        val decoded = graph(
            listOf(step("a"), step("b"), step("c", mapOf("depends_on" to listOf("a", "ghost")))),
            listOf(Triple("b", "c", "edge")),
        )
        assertEquals(listOf("b", "a"), decoded.step("c")?.dependsOn)
    }

    // Levels and order

    @Test fun level_is_one_plus_the_deepest_dependency() {
        assertEquals(mapOf("a" to 0, "b" to 1, "c" to 1, "d" to 2, "e" to 0), diamond.levels)
    }

    @Test fun waterfall_places_each_step_once_its_dependencies_are_placed() {
        // Web topoSortIds: roots in plan order, children depth first.
        assertEquals(listOf("a", "c", "b", "d", "e"), diamond.waterfallOrder)
        assertEquals(listOf(0, 1, 1, 2, 0), diamond.waterfall.map { it.second })
    }

    @Test fun a_cycle_still_lists_every_step_and_keeps_levels_finite() {
        val cyclic = graph(
            listOf(step("a"), step("b"), step("c")),
            listOf(Triple("a", "b", ""), Triple("b", "c", ""), Triple("c", "b", "")),
        )
        assertFalse(cyclic.isAcyclic)
        assertEquals(setOf("a", "b", "c"), cyclic.waterfallOrder.toSet())
        assertEquals(3, cyclic.waterfallOrder.size)
        assertTrue(cyclic.levels.values.all { it in 0 until 10 })
        assertTrue(diamond.isAcyclic)
    }

    // Cycle-safe dependencies

    @Test fun candidates_exclude_self_existing_dependencies_and_transitive_dependents() {
        val chain = graph(listOf(step("a"), step("b"), step("c"), step("d")),
            listOf(Triple("a", "b", ""), Triple("b", "c", "")))
        assertEquals(listOf("d"), chain.dependencyCandidates("a").map { it.id })
        assertEquals(listOf("a", "d"), chain.dependencyCandidates("c").map { it.id })
        assertEquals(emptyList<String>(), chain.dependencyCandidates("missing").map { it.id })
    }

    @Test fun every_candidate_keeps_the_graph_acyclic() {
        val base = diamond
        for (target in base.steps) {
            for (candidate in base.dependencyCandidates(target.id)) {
                val copy = base.copy()
                assertTrue(copy.setDependency(target.id, candidate.id, true))
                assertTrue("${target.id} → ${candidate.id} made a cycle", copy.isAcyclic)
            }
        }
    }

    @Test fun set_dependency_refuses_a_cycle_a_self_loop_and_a_duplicate() {
        val chain = graph(listOf(step("a"), step("b"), step("c")), listOf(Triple("a", "b", ""), Triple("b", "c", "")))
        assertTrue(chain.wouldCreateCycle("a", "c"))
        assertFalse(chain.setDependency("a", "c", true))
        assertFalse(chain.setDependency("a", "a", true))
        assertFalse(chain.setDependency("b", "a", true))
        assertTrue(chain.setDependency("c", "a", true))
        assertEquals(listOf("b", "a"), chain.step("c")?.dependsOn)
        assertTrue(chain.setDependency("c", "b", false))
        assertEquals(listOf("a"), chain.step("c")?.dependsOn)
        assertTrue(chain.isAcyclic)
    }

    // Edit operations

    @Test fun add_step_uses_epoch_millis_and_the_web_defaults() {
        val plan = diamond
        val now = 1_700_000_000_123L
        val id = plan.addStep(now)
        assertEquals("step_1700000000123", id)
        val added = plan.step(id)!!
        assertEquals("New step", added.task)
        assertEquals(0.5, added.confidence, 0.0)
        assertNull(added.tool)
        assertTrue(added.isStart)
        assertEquals(id, plan.steps.last().id)
        assertEquals("step_1700000000123_2", plan.addStep(now))
    }

    @Test fun remove_step_drops_it_and_every_dependency_on_it() {
        val plan = diamond
        plan.removeStep("b")
        assertNull(plan.step("b"))
        assertEquals(listOf("c"), plan.step("d")?.dependsOn)
        assertFalse(plan.saveEdges().any { it.from == "b" || it.to == "b" })
        plan.removeSteps(setOf(0))
        assertNull(plan.step("d"))
    }

    @Test fun move_steps_reorders_like_array_move() {
        val plan = graph(listOf(step("a"), step("b"), step("c"), step("d")), emptyList())
        plan.moveSteps(setOf(0), 3)
        assertEquals(listOf("b", "c", "a", "d"), plan.steps.map { it.id })
        plan.moveSteps(setOf(3), 0)
        assertEquals(listOf("d", "b", "c", "a"), plan.steps.map { it.id })
    }

    @Test fun make_start_clears_dependencies_and_sort_by_waterfall_reorders() {
        val plan = diamond
        plan.makeStart("d")
        assertTrue(plan.step("d")!!.isStart)
        plan.sortByWaterfall()
        assertEquals(plan.waterfallOrder, plan.steps.map { it.id })
    }

    @Test fun update_step_keeps_identity() {
        val plan = diamond
        plan.updateStep("a") { it.copy(id = "renamed", task = "Changed", confidence = 0.35) }
        assertEquals("Changed", plan.step("a")?.task)
        assertEquals(0.35, plan.step("a")!!.confidence, 0.0)
        assertNull(plan.step("renamed"))
    }

    // Save payload

    @Test fun save_rebuilds_edges_from_dependencies_keeping_old_reasons() {
        val plan = diamond
        assertTrue(plan.setDependency("e", "d", true))
        plan.removeStep("c")
        assertEquals(
            listOf(PlanEdge("b", "d", "join b"), PlanEdge("a", "b", "first"), PlanEdge("d", "e", "dependency")),
            plan.saveEdges(),
        )
    }

    @Test fun save_preserves_every_field_the_server_sent() {
        val plan = graph(
            listOf(
                step("a", mapOf("success_criteria" to "done",
                    "parameters" to mapOf("limit" to 5, "flag" to true, "name" to "x"),
                    "providing_agent_id" to "researcher", "readiness" to "strong")),
                step("b", mapOf("depends_on" to listOf("a"))),
            ),
            listOf(Triple("a", "b", "needs a")),
            extra = mapOf(
                "unresolved_inputs" to listOf(mapOf("parameter" to "who", "step_id" to "b")),
                "session_policy" to mapOf("mode" to "shared"), "planning_metadata" to mapOf("v" to 2),
                "confidence" to 0.61,
            ),
        )
        plan.updateStep("a") { it.copy(task = "Edited", tool = "  ") }
        val payload = plan.savePayload()

        assertEquals(planJson(mapOf("mode" to "shared")), payload["session_policy"])
        assertEquals(planJson(mapOf("v" to 2)), payload["planning_metadata"])
        assertEquals(1, payload["unresolved_inputs"].jArr()?.size)
        assertEquals(0.61, payload["confidence"].jNum()!!, 0.0)
        assertEquals("MCTS", payload["provenance"].jObj()?.get("strategy").jStr())

        val steps = payload["steps"].jArr()!!
        val a = steps[0].jObj()!!
        assertEquals("Edited", a["task"].jStr())
        assertEquals(JsonNull, a["tool"])
        assertEquals("done", a["success_criteria"].jStr())
        assertEquals("strong", a["readiness"].jStr())
        assertEquals("researcher", a["providing_agent_id"].jStr())
        assertEquals(5.0, a["parameters"].jObj()?.get("limit").jNum()!!, 0.0)
        assertEquals(true, a["parameters"].jObj()?.get("flag").jBool())
        assertNull(a["depends_on"])
        assertEquals(planJson(listOf("a")), steps[1].jObj()?.get("depends_on"))
        assertEquals(planJson(listOf(mapOf("from" to "a", "to" to "b", "reason" to "needs a"))), payload["edges"])

        // Survives a JSON round trip with types intact.
        val text = Json.encodeToString(JsonElement.serializer(), payload)
        val reparsed = PlanGraph.decode(PlanJson.parse(text))!!
        assertEquals(5.0, reparsed.step("a")?.parameters?.first { it.key == "limit" }?.value.jNum()!!, 0.0)
        assertEquals(listOf("a"), reparsed.step("b")?.dependsOn)
    }

    @Test fun a_new_step_saves_the_fields_the_server_requires() {
        val plan = PlanGraph.empty()
        val id = plan.addStep(1_000L)
        val saved = plan.savePayload()["steps"].jArr()!![0].jObj()!!
        assertEquals(id, saved["id"].jStr())
        assertEquals(JsonArray(emptyList()), saved["expected_outputs"])
        assertEquals(JsonObject(emptyMap()), saved["metadata"])
        assertEquals(JsonObject(emptyMap()), saved["parameters"])
        assertEquals(0.5, saved["confidence"].jNum()!!, 0.0)
        assertNotNull(plan.savePayload()["provenance"].jObj()?.get("strategy"))
    }

    @Test fun parameter_edits_keep_a_non_string_type_when_the_text_is_json() {
        assertEquals(12.0, PlanParameter.valueFromText("12", JsonPrimitive(5)).jNum()!!, 0.0)
        assertEquals(JsonPrimitive("1x"), PlanParameter.valueFromText("1x", JsonPrimitive(5)))
        assertEquals(JsonPrimitive("12"), PlanParameter.valueFromText("12", JsonPrimitive("a")))
        assertEquals(JsonPrimitive("hi"), PlanParameter.valueFromText("hi", null))
    }

    // Layout

    @Test fun layout_puts_each_level_on_its_own_row() {
        val layout = PlanGraphLayout(diamond)
        assertEquals(3, layout.layers.size)
        val pitch = layout.metrics.nodeHeight + layout.metrics.verticalGap
        for ((index, layer) in layout.layers.withIndex()) {
            for (id in layer) assertEquals(layout.positions["a"]!!.y + index * pitch, layout.positions[id]!!.y, 1e-9)
        }
        assertEquals(setOf("a", "e"), layout.layers[0].toSet())
        assertEquals(listOf("d"), layout.layers[2])
        assertTrue(layout.size.width > layout.metrics.nodeWidth * 2)
    }

    @Test fun children_are_ordered_by_their_parents_mean_position() {
        // Roots p, q, r left to right; x hangs off r, y off p, z off p and q.
        val plan = graph(
            listOf(step("p"), step("q"), step("r"), step("x"), step("y"), step("z")),
            listOf(Triple("r", "x", ""), Triple("p", "y", ""), Triple("p", "z", ""), Triple("q", "z", "")),
        )
        val layout = PlanGraphLayout(plan)
        assertEquals(listOf("p", "q", "r"), layout.layers[0])
        assertEquals(listOf("y", "z", "x"), layout.layers[1])
        val xs = layout.layers[1].map { layout.positions[it]!!.x }
        assertEquals(xs.sorted(), xs)
    }

    @Test fun fit_scale_is_clamped_and_never_enlarges() {
        val layout = PlanGraphLayout(diamond)
        assertEquals(1.0, layout.fitScale(PlanSize(10_000.0, 10_000.0)), 0.0)
        assertEquals(0.3, layout.fitScale(PlanSize(10.0, 10.0)), 0.0)
        assertEquals(3.0, PlanGraphLayout.clampScale(9.0), 0.0)
        assertEquals(PlanSize(0.0, 0.0), PlanGraphLayout(PlanGraph.empty()).size)
    }

    @Test fun segments_run_from_parent_bottom_to_child_top() {
        val layout = PlanGraphLayout(diamond)
        val segments = layout.segments(diamond)
        assertEquals(4, segments.size)
        for (segment in segments) assertTrue(segment.from.y < segment.to.y)
    }

    @Test fun confidence_bands() {
        assertEquals(PlanConfidenceBand.High, PlanConfidenceBand.of(0.7))
        assertEquals(PlanConfidenceBand.Medium, PlanConfidenceBand.of(0.69))
        assertEquals(PlanConfidenceBand.Medium, PlanConfidenceBand.of(0.4))
        assertEquals(PlanConfidenceBand.Low, PlanConfidenceBand.of(0.39))
    }
}
