package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.engine.mock.toByteArray
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class VibeDevRepositoryTest {
    private val json = headersOf(HttpHeaders.ContentType, "application/json")

    @Test
    fun `a run with an approval id is answered directly, as Attention answers a diff approval`() = runBlocking {
        var path = ""
        var body = ""
        val engine = MockEngine { request ->
            path = request.url.encodedPath
            body = String(request.body.toByteArray())
            assertEquals(HttpMethod.Post, request.method)
            assertEquals("Bearer t", request.headers[HttpHeaders.Authorization])
            respond("""{"accepted":true,"source":"diff_approval"}""", HttpStatusCode.OK, json)
        }
        val repository = VibeDevRepository(
            MagicianConnectionSnapshot("https://magician.test", mapOf(HttpHeaders.Authorization to "Bearer t")),
            HttpClient(engine),
            attention = { fail("the Attention feed is only the fallback"); error("unreachable") },
        )
        repository.answerDiffApproval(setOf("task_ask"), approve = false, approvalId = "ccp-6f1d")
        assertEquals("/api/magician/v2/hitl/ccp-6f1d/respond", path)
        val sent = Json.parseToJsonElement(body).jsonObject
        assertEquals("diff_approval", sent["source"]!!.jsonPrimitive.content)
        assertEquals("diff_approval", sent["input_type"]!!.jsonPrimitive.content)
        val value = sent["value"]!!.jsonObject
        assertEquals("choice", value["type"]!!.jsonPrimitive.content)
        assertEquals("reject", value["selected_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `an answer the server refuses is an error`() = runBlocking {
        val engine = MockEngine { respond("""{"accepted":false,"reason":"already_resolved"}""", HttpStatusCode.OK, json) }
        val repository = VibeDevRepository(MagicianConnectionSnapshot("https://magician.test", emptyMap()), HttpClient(engine))
        try {
            repository.answerDiffApproval(emptySet(), approve = true, approvalId = "ccp-1")
            fail("expected a refusal")
        } catch (error: VibeDevError) {
            assertTrue(error.message!!.contains("already answered"))
        }
    }

    @Test
    fun `a steer between turns is a conflict, not a failure`() = runBlocking {
        val engine = MockEngine { respond("""{"delivered":false}""", HttpStatusCode.Conflict, json) }
        val repository = VibeDevRepository(MagicianConnectionSnapshot("https://magician.test", emptyMap()), HttpClient(engine))
        assertEquals(VibeDevControlOutcome.BetweenTurns, repository.control("task_1", "steer", "hi"))
    }
}
