package ai.magicbeans.magdroid.work

import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.*
import io.ktor.http.content.TextContent
import io.ktor.utils.io.ByteChannel
import io.ktor.utils.io.writeStringUtf8
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class WorkRepositoryTest {
    private fun rejectsOversizedOpenBody(declaredSize: Boolean) = runBlocking {
        val body = ByteChannel(autoFlush = true)
        body.writeStringUtf8("123456789")
        val engine = MockEngine {
            respond(body, HttpStatusCode.OK, if (declaredSize) headersOf(HttpHeaders.ContentLength, "9") else headersOf())
        }
        val repository = WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine))
        try {
            withTimeout(2_000) {
                try { repository.file("/api/magician/v2/relentless/runs/run/artifacts/hash/file", maximumBytes = 8); fail("Oversized file must fail before EOF") }
                catch (error: WorkRequestError) { assertTrue(error.message.orEmpty().contains("large")) }
            }
        } finally { body.close(); repository.close() }
    }

    @Test fun declared_oversized_file_is_rejected_before_the_response_finishes() = rejectsOversizedOpenBody(true)

    @Test fun file_without_a_length_is_rejected_at_the_ceiling_before_eof() = rejectsOversizedOpenBody(false)

    @Test fun exact_size_file_preserves_bytes_and_frozen_credentials() = runTest {
        val headers = mutableMapOf("Authorization" to "Bearer initial")
        val engine = MockEngine { request ->
            assertEquals("Bearer initial", request.headers[HttpHeaders.Authorization])
            respond("12345678", HttpStatusCode.OK, headersOf(HttpHeaders.ContentLength, "8"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", headers), HttpClient(engine)).use { repository ->
            headers["Authorization"] = "Bearer replacement"
            assertArrayEquals("12345678".toByteArray(), repository.file("/api/magician/v2/relentless/runs/run/artifacts/hash/file", maximumBytes = 8))
        }
    }

    @Test fun frozen_headers_encoded_identity_and_large_numbers_survive_definition_edit() = runTest {
        val headers = mutableMapOf("Authorization" to "Bearer first")
        val engine = MockEngine { request ->
            assertEquals("Bearer first", request.headers["Authorization"])
            assertEquals("https://work.test/api/magician/v2/agents/crew%2Fone%3F%23%25", request.url.toString())
            assertEquals("\"v1\"", request.headers["If-Match"])
            val body = Json.parseToJsonElement((request.body as TextContent).text) as JsonObject
            assertEquals(Long.MAX_VALUE, (body["unknown_budget"] as JsonPrimitive).long)
            respond("""{"definition":{"agent_id":"crew/one?#%","name":"Crew","unknown_budget":9223372036854775807},"etag":"\"v2\""}""",
                HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", headers), HttpClient(engine)).use { client ->
            headers["Authorization"] = "Bearer second"
            val result = client.patchMember("crew/one?#%", buildJsonObject { put("unknown_budget", Long.MAX_VALUE) }, "\"v1\"")
            assertEquals("\"v2\"", result.etag)
            assertEquals(Long.MAX_VALUE, (result.definition["unknown_budget"] as JsonPrimitive).long)
        }
    }

    @Test fun trigger_sends_json_and_missing_etag_never_writes() = runTest {
        var calls = 0
        val engine = MockEngine { request ->
            calls++
            assertTrue(request.url.toString().endsWith("/crew/trigger"))
            assertEquals("{}", (request.body as TextContent).text)
            respond("{}", HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            try { client.patchMember("crew", buildJsonObject {}, ""); fail("Missing version must be rejected") } catch (_: IllegalArgumentException) {}
            assertEquals(0, calls)
            client.actOnMember("crew", WorkRepository.AgentAction.Trigger)
            assertEquals(1, calls)
        }
    }

    @Test fun concurrent_edit_reports_refresh_instead_of_overwriting() = runTest {
        val engine = MockEngine { respond("""{"error":"stale"}""", HttpStatusCode.PreconditionFailed, headersOf(HttpHeaders.ContentType, "application/json")) }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            try { client.patchMember("crew", buildJsonObject { put("name", "New name") }, "old"); fail("Stale edit must fail") }
            catch (error: WorkRequestError) { assertEquals(412, error.status); assertTrue(error.message.orEmpty().contains("Refresh")) }
        }
    }

    @Test fun guided_quality_contract_keeps_review_quorum_local_handoff_and_integer_cost() {
        val value = WorkQualityDraft(goal = "Read a local brief", source = "Only supplied facts", rubric = "Clear and complete", actor = "actor", critic = "critic", dollars = 40).proposal()
        assertEquals("internal_draft", value.text("handoff_kind"))
        assertEquals("required", value.text("human_review"))
        assertEquals(3, (value["critic_profiles"] as JsonArray).size)
        val budget = value["budget"] as JsonObject
        assertEquals(40_000_000L, (budget["max_cost_microusd"] as JsonPrimitive).long)
        assertEquals(1800L, (budget["max_wall_clock_secs"] as JsonPrimitive).long)
        assertEquals("Only supplied facts", (value["sources"] as JsonObject).text("brief"))
        assertEquals(3, ((value["review"] as JsonObject)["score_quorum"] as JsonPrimitive).int)
    }
}
