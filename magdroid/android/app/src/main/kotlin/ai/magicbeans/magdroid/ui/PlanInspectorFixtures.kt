package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.BuildConfig
import ai.magicbeans.magdroid.tasks.PlanInspectorRequest
import ai.magicbeans.magdroid.tasks.PlanInspectorResponse
import ai.magicbeans.magdroid.tasks.PlanInspectorRoute
import ai.magicbeans.magdroid.tasks.PlanInspectorTransport
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import kotlinx.coroutines.delay
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/*
 * DEBUG-only canned Plan Inspector data, for device screenshots and checks
 * without touching a real plan (port of iOS `PlanInspectorFixtures.swift`).
 * Ids: `fixture-plan-draft` (a draft with a pending question) and
 * `fixture-plan-running` (approved, run in progress with live marks). Nothing
 * here reaches the network; Save, Approve and Restore change only the fixture.
 *
 * Open one on a debug build, from any screen under a [PlanInspectorHost]:
 *   adb shell setprop debug.magdroid.plan_fixture "fixture-plan-draft steps"
 * then bring the app to the foreground (the second word is the optional view:
 * graph, steps, steps-edit, step-sheet, step-sheet-read, history,
 * history-preview, clarifications, slots, attempts, analysis). Clear it with
 * `adb shell setprop debug.magdroid.plan_fixture ""`.
 */
internal object PlanInspectorFixtures {
    private const val PROPERTY = "debug.magdroid.plan_fixture"
    private var launch: Pair<String, String?>? = null

    fun transportFor(taskId: String): PlanInspectorTransport? =
        if (BuildConfig.DEBUG && taskId.startsWith("fixture-plan-")) FixtureTransport(running = taskId == "fixture-plan-running") else null

    /** The view a fixture launch asked for, once. */
    fun launchState(taskId: String): String? {
        val current = launch ?: return null
        if (current.first != taskId) return null
        launch = null
        return current.second
    }

    /** The debug property's route, when set on a debug build. */
    fun pendingRoute(): PlanInspectorRoute? {
        if (!BuildConfig.DEBUG) return null
        val value = runCatching {
            Class.forName("android.os.SystemProperties").getMethod("get", String::class.java).invoke(null, PROPERTY) as? String
        }.getOrNull()?.trim().orEmpty()
        if (value.isEmpty()) return null
        val parts = value.split(Regex("\\s+"))
        val id = parts[0].takeIf { it.startsWith("fixture-plan-") } ?: return null
        launch = id to parts.getOrNull(1)
        return PlanInspectorRoute(
            taskId = id, title = "Quarterly sales brief",
            planDocument = "# Quarterly sales brief\n\n1. Gather the exports\n2. Pull the CRM pipeline\n3. Compare and draft",
        )
    }

    fun graphJson(withMarket: Boolean = true): JsonObject {
        val steps = buildList {
            add(step("gather", "Gather the last quarter's sales exports", "files__read", 0.92, """{"folder":"Reports/Q3","limit":50}"""))
            add(step("crm", "Pull pipeline changes from the CRM", "crm__query", 0.74, """{"since":"2026-07-01"}"""))
            if (withMarket) add(step("market", "Scan competitor pricing news", null, 0.38, "{}", agent = "web-researcher"))
            add(step("analyze", "Compare revenue against pipeline movement", "python__run", 0.61, """{"notebook":"q3.ipynb","charts":true,"seed":7}"""))
            add(step("draft", "Draft the quarterly brief", "docs__write", 0.81, "{}"))
            add(step("review", "Check numbers and send for review", "mail__send", 0.55, "{}"))
        }
        val edges = listOf(
            Triple("gather", "analyze", "needs the exports"),
            Triple("crm", "analyze", "needs pipeline data"),
            Triple("market", "draft", "context for the narrative"),
            Triple("analyze", "draft", "charts and findings"),
            Triple("draft", "review", "the brief to check"),
        ).filter { withMarket || it.first != "market" }
            .joinToString(",") { """{"from":"${it.first}","to":"${it.second}","reason":"${it.third}"}""" }
        return Json.parseToJsonElement(
            """{"steps":[${steps.joinToString(",")}],"edges":[$edges],
            "unresolved_inputs":[{"step_id":"review","parameter":"recipient","prompt":"Who reviews it?"}],
            "confidence":0.72,"provenance":{"strategy":"AtomicComposition","generator":"planner","notes":null},
            "session_policy":{"mode":"fixture"}}""",
        ) as JsonObject
    }

    private fun step(id: String, task: String, tool: String?, confidence: Double, parameters: String, agent: String? = null): String =
        """{"id":"$id","task":"$task","tool":${tool?.let { "\"$it\"" } ?: "null"},"parameters":$parameters,
        "expected_outputs":["$id result"],"confidence":$confidence,"metadata":{"rationale":"Fixture step"},
        "success_criteria":"Done when $id has output"${agent?.let { ",\"providing_agent_id\":\"$it\"" } ?: ""}}"""

    private class FixtureTransport(private val running: Boolean) : PlanInspectorTransport {
        private var graph: JsonElement = graphJson()
        private var status = if (running) "approved" else "draft"

        override suspend fun send(request: PlanInspectorRequest): PlanInspectorResponse {
            delay(150)
            val path = request.path
            val now = System.currentTimeMillis()
            fun ok(json: String) = PlanInspectorResponse(200, Json.parseToJsonElement(json))
            return when {
                path.endsWith("/execution-panel") -> {
                    val marks = listOf("completed", "completed", "completed", "in_progress", "pending", "pending")
                    val ids = listOf("gather", "crm", "market", "analyze", "draft", "review")
                    val rows = if (running) ids.mapIndexed { i, id ->
                        """{"number":${i + 1},"name":"$id","status":"${marks[i]}","step_id":"$id"}"""
                    }.joinToString(",") else ""
                    ok("""{"debug":{"selected_execution":{"status":"${if (running) "running" else "completed"}","step_statuses":[$rows]}}}""")
                }
                path.endsWith("/plan/versions") -> ok(
                    """{"versions":[{"epoch_ms":${now - 3_600_000},"label":"Before adding market scan","step_count":5,"confidence":0.68},
                    {"epoch_ms":${now - 86_400_000},"label":null,"step_count":4,"confidence":0.6}],"has_more":false}""",
                )
                path.contains("/plan/versions/") && request.method == "GET" ->
                    ok("""{"version":{"epoch_ms":1,"plan_graph":${graphJson(withMarket = false)}}}""")
                path.endsWith("/plan/clarifications") -> ok(
                    """{"clarification_history":[{"question_id":"q-format","question_text":"Should the brief be a doc or slides?",
                    "stage":"answered","answered_at":${now - 600_000},
                    "slot_updates":[{"slot_id":"task::format","slot_type":"modifier","value":"doc","confidence":0.9,"received_at":0}]}]}""",
                )
                path.endsWith("/plan/slots") -> ok(
                    """{"slot_graph_snapshot":{"execution_id":"planexec_fixture","slots":[
                    {"id":"task::period","slot_type":"temporal","value":"Q3 2026","confidence":0.95,"provenance":[{"source":"LlmPrimary","timestamp":0}]},
                    {"id":"task::audience","slot_type":"entity","value":"Leadership team","confidence":0.7,"provenance":[]},
                    {"id":"task::reviewer","slot_type":"entity","value":null,"confidence":0.2,"provenance":[]},
                    {"id":"task::format","slot_type":"modifier","value":"doc","confidence":0.9,"provenance":[{"source":"UserReply","timestamp":0}]}],
                    "confidence_summary":{"overall":0.69,"min_critical_slot":0.2,"unresolved_slots":["task::reviewer"]},
                    "clarified_task":{"goal":"Quarterly brief","constraints":["Q3 only"]}}}""",
                )
                path.endsWith("/plan/analysis") ->
                    ok("""{"query_analysis":{"intent":"report","complexity":"multi_step","domains":["sales","finance"]}}""")
                path.endsWith("/plan/attempts") -> ok(
                    """{"strategy_attempts":[{"strategy_type":"AtomicComposition","attempt_number":1,"failure_reason":null,
                    "attempted_at":$now,"exploration_result":{"confidence":0.72,"nodes":14}}]}""",
                )
                path.endsWith("/plan") && request.method == "PUT" -> {
                    (request.body as? JsonObject)?.get("plan")?.let { graph = it }
                    ok("""{"plan":${record()}}""")
                }
                path.endsWith("/plan/approve") -> { status = "approved"; ok("""{"plan":${record()}}""") }
                path.endsWith("/plan/reject") -> { status = "rejected"; ok("""{"plan":${record()}}""") }
                path.endsWith("/execute") || path.endsWith("/respond") || path.endsWith("/resume") || path.endsWith("/restore") ->
                    ok("""{"plan":${record()},"workflow_resumed":true}""")
                path.endsWith("/plan") -> ok("""{"plan":${record()}}""")
                else -> PlanInspectorResponse(404, JsonObject(mapOf("error" to JsonPrimitive("not_found"))))
            }
        }

        private fun record(): String {
            val questions = if (status == "draft") """[{"id":"q-reviewer","question_text":"Who should review the brief before it goes out?",
                "options":[{"value":"cfo","label":"The CFO"},{"value":"team","label":"My team"}],
                "source_slot_id":"task::reviewer","urgency":0.8}]""" else "[]"
            return """{"plan_id":"plan-fixture","task_id":"fixture","status":"$status","plan_graph":$graph,"pending_questions":$questions}"""
        }
    }
}

/** DEBUG: opens a fixture inspector when `debug.magdroid.plan_fixture` is set (see [PlanInspectorFixtures]). */
@Composable
internal fun PlanInspectorDebugLauncher(open: (PlanInspectorRoute) -> Unit) {
    if (!BuildConfig.DEBUG) return
    LaunchedEffect(Unit) { PlanInspectorFixtures.pendingRoute()?.let(open) }
}
