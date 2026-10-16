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
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class HttpPlanInspectorTransportTest {
    private val seen = mutableListOf<HttpRequestData>()

    private fun transport(
        base: String = "https://magic.example/",
        handler: suspend MockRequestHandleScope.(HttpRequestData) -> HttpResponseData = {
            respond("""{"ok":true}""", HttpStatusCode.OK)
        },
    ) = HttpPlanInspectorTransport(
        client = HttpClient(MockEngine { request -> seen.add(request); handler(request) }) { expectSuccess = false },
        baseUrl = { base },
        headers = { mapOf("X-Device-Id" to "phone-1", "Authorization" to "Bearer tok") },
    )

    @Test
    fun `GET plan goes to the host plus the encoded path with the app headers`() = runBlocking {
        val client = PlanInspectorClient("task one/2", transport())
        val response = transport().send(client.loadPlanRequest())
        val request = seen.single()
        assertEquals("GET", request.method.value)
        assertEquals("https://magic.example/api/magician/v3/tasks/task%20one%2F2/plan", request.url.toString())
        assertEquals("phone-1", request.headers["X-Device-Id"])
        assertEquals("Bearer tok", request.headers["Authorization"])
        assertEquals(200, response.status)
        assertEquals(JsonPrimitive(true), response.body!!.jsonObject["ok"])
    }

    @Test
    fun `decision query is percent-encoded and the method kept`() = runBlocking {
        val client = PlanInspectorClient("t1", transport())
        transport().send(client.decisionRequest("approve", "plan a&b"))
        val request = seen.single()
        assertEquals("POST", request.method.value)
        assertEquals("https://magic.example/api/magician/v3/tasks/t1/plan/approve?plan_id=plan%20a%26b", request.url.toString())
        assertEquals("plan a&b", request.url.parameters["plan_id"])
    }

    @Test
    fun `PUT plan sends the JSON body as application json`() = runBlocking {
        val graph = PlanGraph.decode(Json.parseToJsonElement(
            """{"steps":[{"id":"a","task":"Read","tool":"files__read","confidence":0.9}],"edges":[]}""",
        ))!!
        val client = PlanInspectorClient("t1", transport())
        transport().send(client.savePlanRequest(graph, " v2 "))
        val request = seen.single()
        assertEquals("PUT", request.method.value)
        val content = request.body as TextContent
        assertEquals("application/json", content.contentType.toString().substringBefore(';'))
        val body = Json.parseToJsonElement(content.text).jsonObject
        assertEquals(JsonPrimitive("v2"), body["label"])
        assertTrue(body["plan"]!!.jsonObject.containsKey("steps"))
    }

    @Test
    fun `a 404 passes through for the client to read as no plan`() = runBlocking {
        val sent = transport { respond("", HttpStatusCode.NotFound) }
        assertNull(PlanInspectorClient("t1", sent).loadPlan())
    }

    @Test
    fun `a failure that is not JSON keeps its text as the message`() = runBlocking {
        val sent = transport { respond("Bad gateway from tunnel", HttpStatusCode.BadGateway) }
        try {
            PlanInspectorClient("t1", sent).approve("p1")
            fail("expected a PlanInspectorException")
        } catch (error: PlanInspectorException) {
            assertEquals(502, error.status)
            assertEquals("Bad gateway from tunnel", error.message)
        }
    }

    @Test
    fun `no configured host fails before any request`() = runBlocking {
        try {
            transport(base = "  ").send(PlanInspectorRequest("GET", "/api/x"))
            fail("expected a PlanInspectorException")
        } catch (error: PlanInspectorException) {
            assertEquals(0, error.status)
        }
        assertTrue(seen.isEmpty())
    }

    @Test
    fun `body reading`() {
        assertNull(HttpPlanInspectorTransport.body(200, "  "))
        assertNull(HttpPlanInspectorTransport.body(200, "not json"))
        assertEquals(JsonObject(mapOf("message" to JsonPrimitive("oops"))), HttpPlanInspectorTransport.body(500, "oops\n"))
        assertEquals(
            "https://h/api/p?limit=20",
            HttpPlanInspectorTransport.url("https://h/", PlanInspectorRequest("GET", "/api/p", listOf("limit" to "20"))),
        )
    }
}
