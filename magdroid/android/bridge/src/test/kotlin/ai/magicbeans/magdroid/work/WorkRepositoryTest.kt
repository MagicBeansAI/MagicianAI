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

    @Test fun quality_model_controls_preserve_defaults_and_bounded_independent_routes() {
        val fixture = Json.parseToJsonElement("""{
          "quality":{"profiles":[{"name":"actor-auto","family":"a"},{"name":"actor-high","family":"a"},{"name":"review-auto","family":"b"},{"name":"operation/actor","family":"a"}]},
          "catalog":{"profiles":[{"name":"operation/actor","model_selection":{"model_id":"a","thinking":"high"}}],"models":[
            {"id":"a","provider":"openai","label":"Draft","thinking":{"mode":"effort","levels":["low","high"],"default":"high"},"choices":{"auto":"chat-a"},"operation_choices":{"auto":"actor-auto","low":"unbounded","high":"actor-high"}},
            {"id":"b","provider":"gemini","label":"Review","thinking":{"mode":"effort","levels":["low","high"],"default":"low"},"choices":{"auto":"chat-b"},"operation_choices":{"auto":"review-auto","high":"unbounded-review"}}
          ]}}
        """).jsonObject
        val catalog = WorkQualityModels(fixture["quality"], fixture["catalog"])
        val models = catalog.models()
        assertEquals(mapOf("auto" to "actor-auto", "high" to "actor-high"), models.first().choices)
        assertEquals(listOf("high"), models.first().thinking.levels)
        assertEquals("high", catalog.selection("operation/actor", models)?.thinking)
        assertTrue(catalog.valid("operation/actor", models))
        val reviewers = catalog.models(catalog.family("operation/actor"))
        assertEquals(listOf("b"), reviewers.map { it.id })
        assertFalse(catalog.valid("actor-auto", reviewers))
        assertFalse(catalog.valid("unbounded-review", reviewers))
        assertTrue(catalog.valid("review-auto", reviewers))
        assertFalse(catalog.valid("retired-model", models))
        assertFalse(catalog.valid("review-auto", reviewers.map { it.copy(installed = false) }))
        val workload = WorkQualityModels(Json.parseToJsonElement("""{"profiles":[{"name":"operation/actor","family":"a"}]}"""), fixture["catalog"])
        assertEquals(mapOf("auto" to "operation/actor", "high" to "operation/actor"), workload.models().first().choices)
        assertEquals("high", workload.models().first().thinking.defaultLevel)
        assertEquals("auto", workload.selection("operation/actor", workload.models())?.thinking)
        val noJson = WorkQualityModels(Json.parseToJsonElement("""{"profiles":[{"name":"review-auto","family":"b","supports_json":false}]}"""), fixture["catalog"])
        assertEquals(listOf("b"), noJson.models().map { it.id })
        assertTrue(noJson.models("a").isEmpty())
        val proposal = WorkQualityDraft(actor = models.first().choices.getValue("high"), critic = reviewers.first().choices.getValue("auto")).proposal()
        assertEquals("actor-high", proposal.text("actor_profile"))
        assertEquals(List(3) { JsonPrimitive("review-auto") }, proposal["critic_profiles"])
    }

    private fun jsonReply(status: HttpStatusCode, body: String) = MockEngine { respond(body, status, headersOf(HttpHeaders.ContentType, "application/json")) }

    @Test fun program_document_reads_its_hash_and_tolerates_an_older_server() = runTest {
        val engine = MockEngine { request ->
            val body = if (request.url.encodedPath.endsWith("/new.md")) """{"name":"new.md","title":"New","content":"# New","sha256":"abc123","history":[]}"""
                else """{"name":"old.md","title":"Old","content":"# Old"}"""
            respond(body, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            assertEquals("abc123", client.program("new.md").sha256)
            val old = client.program("old.md")
            assertNull(old.sha256); assertEquals(emptyList<WorkProgramVersion>(), old.history)
        }
    }

    @Test fun save_program_puts_the_draft_against_the_opened_version() = runTest {
        val engine = MockEngine { request ->
            assertEquals(HttpMethod.Put, request.method)
            assertEquals("https://work.test/api/magician/v2/programs/facts%20v2.md", request.url.toString())
            val body = Json.parseToJsonElement((request.body as TextContent).text) as JsonObject
            assertEquals("# Facts\n  indented", body.text("content"))
            assertEquals("base-hash", body.text("base_sha256"))
            respond("""{"saved":true,"program":"facts v2.md","created":false,"unchanged":false,"history_snapshot":"facts v2.20261018.md","sha256":"next-hash"}""",
                HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            val saved = client.saveProgram("facts v2.md", "# Facts\n  indented", "base-hash")
            assertEquals(WorkProgramSaved("facts v2.md", created = false, unchanged = false, historySnapshot = "facts v2.20261018.md", sha256 = "next-hash"), saved)
        }
    }

    @Test fun save_program_conflict_is_typed_and_keeps_the_server_hash() = runTest {
        val engine = jsonReply(HttpStatusCode.Conflict, """{"error":"program_changed","message":"facts.md changed after it was opened.","current_sha256":"theirs"}""")
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            try { client.saveProgram("facts.md", "mine", "base"); fail("A changed program must not be overwritten") }
            catch (error: WorkProgramChangedError) {
                assertEquals(409, error.status)
                assertEquals("facts.md changed after it was opened.", error.message)
                assertEquals("theirs", error.currentSha256)
            }
        }
    }

    @Test fun save_program_refusals_report_the_server_words_and_blank_drafts_never_post() = runTest {
        var calls = 0
        val engine = MockEngine {
            calls++
            respond("""{"error":"grant_cannot_manage","message":"Only a signed-in owner session can make this change."}""",
                HttpStatusCode.Unauthorized, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        WorkRepository(MagicianConnectionSnapshot("https://work.test", emptyMap()), HttpClient(engine)).use { client ->
            try { client.saveProgram("facts.md", "   ", "base"); fail("A blank draft must not be saved") } catch (_: IllegalArgumentException) {}
            try { client.saveProgram("facts.md", "text", ""); fail("A draft without its base must not be saved") } catch (_: IllegalArgumentException) {}
            assertEquals(0, calls)
            try { client.saveProgram("facts.md", "text", "base"); fail("An agent credential must be refused") }
            catch (error: WorkRequestError) {
                assertFalse(error is WorkProgramChangedError)
                assertEquals(401, error.status)
                assertEquals("Only a signed-in owner session can make this change.", error.message)
            }
            assertEquals(1, calls)
        }
    }
}
