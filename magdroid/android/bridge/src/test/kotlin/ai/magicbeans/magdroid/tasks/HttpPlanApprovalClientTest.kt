package ai.magicbeans.magdroid.tasks

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.MockRequestHandleScope
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.HttpRequestData
import io.ktor.client.request.HttpResponseData
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.TextContent
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** The production plan-approval client's requests, over a mock engine. */
class HttpPlanApprovalClientTest {
    private val seen = mutableListOf<HttpRequestData>()

    private fun client(
        handler: suspend MockRequestHandleScope.(HttpRequestData) -> HttpResponseData = {
            respond("""{"accepted":true}""", HttpStatusCode.OK)
        },
    ) = HttpPlanApprovalClient(
        client = HttpClient(MockEngine { request -> seen.add(request); handler(request) }) { expectSuccess = false },
        baseUrl = { "https://magic.example/" },
        headers = { mapOf("Authorization" to "Bearer tok") },
    )

    private val request = PlanApprovalRequest(correlationId = "plan 1/a", taskId = "task-1", executionId = "exec-9")

    @Test
    fun `urls are encoded under the host`() {
        assertEquals(
            "https://magic.example/api/magician/v2/hitl/plan%201%2Fa/respond",
            PlanApprovalHttp.respondUrl("https://magic.example/", "plan 1/a"),
        )
        assertEquals(
            "https://magic.example/api/magician/v3/tasks/t%201/execute",
            PlanApprovalHttp.executeUrl("https://magic.example", "t 1"),
        )
        assertEquals(
            "https://magic.example/api/magician/v3/tasks/t1/plan/reject?plan_id=p%26q",
            PlanApprovalHttp.planDecisionUrl("https://magic.example", "t1", approve = false, planId = "p&q"),
        )
        try {
            PlanApprovalHttp.executeUrl("  ", "t1")
            fail("an empty host must not build a URL")
        } catch (_: IllegalStateException) {
        }
    }

    @Test
    fun `respond posts the confirmation with the app headers`() = runBlocking {
        val posted = client().respond(request, confirmed = true)
        assertEquals(PlanApprovalPost.Ok, posted)
        val sent = seen.single()
        assertEquals("POST", sent.method.value)
        assertEquals("https://magic.example/api/magician/v2/hitl/plan%201%2Fa/respond", sent.url.toString())
        assertEquals("Bearer tok", sent.headers["Authorization"])
        val body = Json.parseToJsonElement((sent.body as TextContent).text).jsonObject
        assertEquals(JsonPrimitive("plan_approval"), body["source"])
        assertEquals(JsonPrimitive("task-1"), body["task_id"])
        assertEquals(JsonPrimitive("exec-9"), body["execution_id"])
        assertEquals(JsonPrimitive(true), body["value"]!!.jsonObject["confirmed"])
    }

    @Test
    fun `a refused or already answered respond reads through respondOutcome`() = runBlocking {
        val refused = client { respond("""{"accepted":false,"message":"scope mismatch"}""", HttpStatusCode.OK) }
            .respond(request, confirmed = false)
        assertEquals(PlanApprovalPost.Failed("scope mismatch"), refused)
        val answered = client { respond("""{"reason":"already_resolved"}""", HttpStatusCode.Conflict) }
            .respond(request, confirmed = true)
        assertTrue(answered is PlanApprovalPost.Failed)
    }

    @Test fun `already answered approve and run never executes a task`() = runBlocking {
        val outcome = PlanApprovalActions.approveAndRun(request,
            client { respond("""{"reason":"already_resolved"}""", HttpStatusCode.Conflict) })
        assertTrue(outcome is PlanApprovalOutcome.Failed)
        assertEquals(1, seen.size)
    }

    @Test fun `a plan client cannot follow an approval onto a new server`() = runBlocking {
        var host = "https://first.example"
        val http = HttpClient(MockEngine { request -> seen.add(request); respond("""{"accepted":true}""") })
        val bound = HttpPlanApprovalClient(http, { host }, { emptyMap() })
        host = "https://second.example"
        try {
            val outcome = PlanApprovalActions.approveAndRun(request, bound)
            assertTrue(outcome is PlanApprovalOutcome.Failed)
            assertTrue(seen.isEmpty())
        } finally { http.close() }
    }

    @Test
    fun `approve and run starts the run after the approval lands`() = runBlocking {
        val outcome = PlanApprovalActions.approveAndRun(request, client())
        assertEquals(PlanApprovalOutcome.ApprovedAndStarted, outcome)
        assertEquals(
            listOf(
                "https://magic.example/api/magician/v2/hitl/plan%201%2Fa/respond",
                "https://magic.example/api/magician/v3/tasks/task-1/execute",
            ),
            seen.map { it.url.toString() },
        )
    }

    @Test
    fun `a start that fails keeps the approval and says why`() = runBlocking {
        val outcome = PlanApprovalActions.approveAndRun(request, client { data ->
            if (data.url.encodedPath.endsWith("/execute")) respond("""{"message":"Task is busy"}""", HttpStatusCode.Conflict)
            else respond("""{"accepted":true}""", HttpStatusCode.OK)
        })
        assertTrue(outcome.isApproved)
        assertEquals(PlanApprovalOutcome.ApprovedRunNotStarted("Task is busy"), outcome)
        assertEquals("Approved — the run did not start. Task is busy", outcome.message)
    }
}
