package ai.magicbeans.magdroid.tasks

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test

/** Records every request; answers from a script keyed by method + path. */
internal class StubPlanTransport : PlanInspectorTransport {
    val responses = mutableMapOf<String, PlanInspectorResponse>()
    val errors = mutableMapOf<String, Exception>()
    val requests = mutableListOf<PlanInspectorRequest>()

    fun on(method: String, path: String, status: Int = 200, body: Any? = emptyMap<String, Any>()) {
        responses["$method $path"] = PlanInspectorResponse(status, planJson(body))
    }

    override suspend fun send(request: PlanInspectorRequest): PlanInspectorResponse {
        requests.add(request)
        val key = "${request.method} ${request.path}"
        errors[key]?.let { throw it }
        return responses[key] ?: PlanInspectorResponse(404, planJson(mapOf("error" to "unscripted $key")))
    }

    val calls: List<String> get() = requests.map { "${it.method} ${it.path}" }
}

/**
 * Request building and response reading for the Plan Inspector endpoints.
 * One-for-one port of iOS `PlanInspectorClientTests` (web
 * `ExecutionPlanInspector.svelte`, `taskStore.ts` approve/reject).
 */
class PlanInspectorRequestsTest {
    private val base = "/api/magician/v3/tasks/task_1/plan"
    private lateinit var transport: StubPlanTransport
    private lateinit var client: PlanInspectorClient

    @Before fun setUp() {
        transport = StubPlanTransport()
        client = PlanInspectorClient("task_1", transport)
    }

    companion object {
        val graphJSON: Map<String, Any?> = mapOf(
            "steps" to listOf(mapOf("id" to "a", "task" to "A", "tool" to null, "parameters" to emptyMap<String, Any>(),
                "expected_outputs" to emptyList<Any>(), "confidence" to 0.9, "metadata" to emptyMap<String, Any>())),
            "edges" to emptyList<Any>(), "unresolved_inputs" to emptyList<Any>(), "confidence" to 0.9,
            "provenance" to mapOf("strategy" to "S", "generator" to null, "notes" to null),
        )
    }

    @Test fun encodes_the_task_id_as_a_path_segment() {
        val odd = PlanInspectorClient("a b/c", transport)
        assertEquals("/api/magician/v3/tasks/a%20b%2Fc/plan", odd.planPath)
    }

    @Test fun load_plan_reads_id_status_graph_and_pending_questions() = runBlocking<Unit> {
        transport.on("GET", base, body = mapOf("plan" to mapOf(
            "plan_id" to "plan_9", "status" to "draft", "plan_graph" to graphJSON,
            "pending_questions" to listOf(mapOf("id" to "q1", "question_text" to "Who?", "urgency" to 0.5,
                "options" to listOf(mapOf("value" to "me", "label" to "Me"), mapOf("label" to "no value")))),
        )))
        val record = client.loadPlan()
        assertEquals("plan_9", record?.planId)
        assertEquals(PlanStatus.Draft, record?.status)
        assertEquals(listOf("a"), record?.graph?.steps?.map { it.id })
        assertEquals(listOf("me"), record?.pendingQuestions?.first()?.options?.map { it.value })
        assertEquals(listOf("GET $base"), transport.calls)
    }

    @Test fun a_missing_plan_is_null_not_an_error() = runBlocking<Unit> {
        transport.on("GET", base, status = 404, body = mapOf("error" to "task_plan_not_found:task_1"))
        assertNull(client.loadPlan())
    }

    @Test fun server_errors_carry_the_servers_message() = runBlocking<Unit> {
        transport.on("GET", base, status = 500, body = mapOf("message" to "store offline"))
        try {
            client.loadPlan()
            fail("expected an error")
        } catch (error: PlanInspectorException) {
            assertEquals("store offline", error.message)
        }
    }

    @Test fun save_puts_the_plan_and_a_blank_label_as_null() = runBlocking<Unit> {
        val graph = PlanGraph.decode(planJson(graphJSON))!!
        val request = client.savePlanRequest(graph, "  ")
        assertEquals("PUT", request.method)
        assertEquals(base, request.path)
        assertEquals(JsonNull, request.body.jObj()?.get("label"))
        assertEquals("a", request.body.jObj()?.get("plan").jObj()?.get("steps").jArr()?.first().jObj()?.get("id").jStr())
        assertEquals(JsonPrimitive("v2"), client.savePlanRequest(graph, " v2 ").body.jObj()?.get("label"))

        transport.on("PUT", base, body = mapOf("plan" to mapOf("plan_id" to "p", "status" to "draft", "plan_graph" to graphJSON)))
        val saved = client.savePlan(graph, null)
        assertEquals("p", saved.planId)
    }

    @Test fun versions_ask_for_twenty_and_parse() = runBlocking<Unit> {
        transport.on("GET", "$base/versions", body = mapOf("versions" to listOf(
            mapOf("epoch_ms" to 1_700_000_000_000L, "label" to null, "step_count" to 3, "confidence" to 0.5),
            mapOf("epoch_ms" to 1_600_000_000_000L, "label" to "Before", "step_count" to 1),
        ), "has_more" to true))
        val page = client.versions()
        assertEquals(listOf("limit" to "20"), transport.requests.first().query)
        assertEquals(listOf(1_700_000_000_000L, 1_600_000_000_000L), page.versions.map { it.epochMs })
        assertEquals(listOf("Saved version", "Before"), page.versions.map { it.title })
        assertTrue(page.hasMore)
    }

    @Test fun version_preview_and_restore_paths() = runBlocking<Unit> {
        transport.on("GET", "$base/versions/42", body = mapOf("version" to mapOf("epoch_ms" to 42, "plan_graph" to graphJSON)))
        transport.on("POST", "$base/versions/42/restore", body = mapOf("plan" to mapOf("plan_id" to "p", "status" to "draft")))
        val preview = client.version(42)
        assertEquals(1, preview?.steps?.size)
        client.restore(42)
        assertEquals(listOf("GET $base/versions/42", "POST $base/versions/42/restore"), transport.calls)
    }

    @Test fun answering_a_question_posts_the_canonical_hitl_respond() {
        val request = client.answerRequest("q 1", "Jordan")
        assertEquals("/api/magician/v2/hitl/q%201/respond", request.path)
        assertEquals(planJson(mapOf(
            "source" to "clarification",
            "value" to mapOf("type" to "text", "value" to "Jordan"),
            "channel" to "android",
            "task_id" to "task_1",
        )), request.body)
    }

    @Test fun an_already_answered_question_counts_as_done() = runBlocking<Unit> {
        transport.on("POST", "/api/magician/v2/hitl/q1/respond", status = 409, body = mapOf("reason" to "already_resolved"))
        client.answer("q1", "x")
        transport.on("POST", "/api/magician/v2/hitl/q2/respond", status = 400, body = mapOf("message" to "bad answer"))
        try {
            client.answer("q2", "x")
            fail("expected an error")
        } catch (error: PlanInspectorException) {
            assertEquals("bad answer", error.message)
        }
    }

    @Test fun decisions_name_the_plan_id_and_run_posts_execute() {
        val approve = client.decisionRequest("approve", "plan 9")
        assertEquals("POST", approve.method)
        assertEquals("$base/approve", approve.path)
        assertEquals(listOf("plan_id" to "plan 9"), approve.query)
        assertEquals("$base/reject", client.decisionRequest("reject", "p").path)
        assertEquals("$base/replan", client.replanRequest().path)
        assertEquals("/api/magician/v3/tasks/task_1/execute", client.executeRequest().path)
        assertEquals("/api/magician/v3/tasks/task_1/execution-panel", client.executionPanelRequest().path)
    }

    @Test fun pane_endpoints_parse() = runBlocking<Unit> {
        transport.on("GET", "$base/analysis", body = mapOf("query_analysis" to null))
        transport.on("GET", "$base/slots", body = mapOf("slot_graph_snapshot" to mapOf(
            "execution_id" to "e", "slots" to listOf(
                mapOf("id" to "t::who", "slot_type" to "entity", "value" to "Jo", "confidence" to 0.9,
                    "provenance" to listOf(mapOf("source" to "UserReply"))),
                mapOf("id" to "t::when", "slot_type" to mapOf("Custom" to "x"), "value" to null, "confidence" to 0.1),
                mapOf("id" to "t::what", "slot_type" to "Entity", "value" to "Brief", "confidence" to 0.5),
            ),
            "confidence_summary" to mapOf("overall" to 0.5, "min_critical_slot" to 0.1, "unresolved_slots" to listOf("t::when")),
            "slot_trigger_mappings" to emptyList<Any>(), "confidence_boost_results" to emptyMap<String, Any>(),
        )))
        transport.on("GET", "$base/attempts", body = mapOf("strategy_attempts" to listOf(
            mapOf("strategy_type" to "MCTS", "failure_reason" to "timeout", "exploration_result" to mapOf("confidence" to 0.3)),
        )))
        transport.on("GET", "$base/clarifications", body = mapOf("clarification_history" to listOf(
            mapOf("question_id" to "q", "question_text" to "Q?", "stage" to "waiting_user",
                "slot_updates" to listOf(mapOf("slot_id" to "s"))),
        )))

        assertNull(client.analysis())
        val slots = client.slots()
        assertEquals(listOf("entity", "custom"), slots?.groups?.map { it.type })
        assertEquals(0.7, slots?.groups?.first()?.averageConfidence ?: 0.0, 0.0001)
        assertEquals("who", slots?.slots?.first()?.shortId)
        assertEquals(listOf("t::when"), slots?.unresolvedSlots)
        assertNull(slots?.triggerMappings)
        assertNull(slots?.boostResults)
        val attempts = client.attempts()
        assertEquals("MCTS #1", attempts.first().title)
        assertEquals(0.3, attempts.first().confidence!!, 0.0)
        val history = client.clarifications()
        assertEquals("Waiting User", history.first().stageLabel)
        assertEquals(1, history.first().slotUpdates.size)
    }

    private fun assertInvalid(expected: PlanManualResumeInvalid, form: PlanManualResume) {
        try {
            form.payload()
            fail("expected $expected")
        } catch (error: PlanManualResumeInvalid) {
            assertEquals(expected, error)
        }
    }

    @Test fun manual_resume_validates_like_the_web() {
        var form = PlanManualResume()
        assertEquals(JsonObject(emptyMap()), form.payload())
        form = form.copy(updatedConfidence = "1.5")
        assertInvalid(PlanManualResumeInvalid.Confidence, form)
        form = form.copy(updatedConfidence = "0.8", slotsJson = """{"slot_id":"a"}""")
        assertInvalid(PlanManualResumeInvalid.SlotsNotArray, form)
        form = form.copy(slotsJson = """[{"slot_id":"a"}]""")
        assertInvalid(PlanManualResumeInvalid.SlotMissingFields(1), form)
        form = form.copy(slotsJson = "[nope")
        assertInvalid(PlanManualResumeInvalid.SlotsNotJson, form)
        form = form.copy(
            slotsJson = """[{"slot_id":" a ","slot_type":"entity","value":{"n":1},"confidence":0.9}]""",
            handledQuestion = " q1 ",
        )
        val payload = form.payload()
        assertEquals(JsonPrimitive("q1"), payload["handled_question"])
        assertEquals(0.8, payload["updated_confidence"].jNum()!!, 0.0)
        val slot = payload["slots"].jArr()!!.single().jObj()!!
        assertEquals("a", slot["slot_id"].jStr())
        assertEquals("entity", slot["slot_type"].jStr())
        assertEquals(1.0, slot["value"].jObj()?.get("n").jNum()!!, 0.0)
        assertEquals(0.9, slot["confidence"].jNum()!!, 0.0)
        assertEquals("$base/clarifications/resume", client.resumeRequest(form).path)
    }
}
