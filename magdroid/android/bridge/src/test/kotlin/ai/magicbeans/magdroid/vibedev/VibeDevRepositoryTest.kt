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
    fun `project references and nightly schedules use the web run contract`() = runBlocking {
        val posted = mutableListOf<kotlinx.serialization.json.JsonObject>()
        val engine = MockEngine { request ->
            posted += Json.parseToJsonElement(String(request.body.toByteArray())).jsonObject
            respond("""{"task_id":"scheduled"}""", HttpStatusCode.OK, json)
        }
        val repository = VibeDevRepository(MagicianConnectionSnapshot("https://magician.test", emptyMap()), HttpClient(engine))
        val attachment = ai.magicbeans.magdroid.chat.StagedAttachment("local", "design.png", "remote", uploading = false, mimeType = "image/png")
        val request = VibeDevRunRequest("Build this", "project", "autopilot", "one", attachments = listOf(attachment),
            attachmentSessionId = "project-chat", nightly = true, costBudgetUsd = 3.0)
        repository.startRun(request)
        val sent = posted.last()
        assertEquals("project-chat", sent["attachment_session_id"]!!.jsonPrimitive.content)
        val references = sent["attachments"] as kotlinx.serialization.json.JsonArray
        assertEquals("remote", references.single().jsonObject["attachment_id"]!!.jsonPrimitive.content)
        assertEquals("0 2 * * *", sent["schedule"]!!.jsonObject["kind"]!!.jsonObject["Cron"]!!.jsonObject["expression"]!!.jsonPrimitive.content)
        assertEquals("false", sent["auto_apply"]!!.jsonPrimitive.content)
        repository.startRun(request.copy(parentTaskId = "existing"))
        assertTrue(!posted.last().containsKey("schedule"))
        repository.startRun(request.copy(mode = "discuss"))
        assertTrue(!posted.last().containsKey("schedule"))
        assertEquals("cto", posted.last()["agent_id"]!!.jsonPrimitive.content)
    }

    @Test
    fun `history uses server cursor and decodes original folder metadata`() = runBlocking {
        val cursor = "2026-10-22T00:00:00Z|r"
        val engine = MockEngine { request ->
            assertEquals("/api/magician/v2/vibedev/runs", request.url.encodedPath)
            assertEquals("20", request.url.parameters["limit"])
            assertEquals(cursor, request.url.parameters["cursor"])
            respond("""{"runs":[{"run_id":"r","live_task_id":"turn","project_id":"p","work_location":"original","repo_path":"/owner/app","needs_you_count":2}],"has_more":false}""", HttpStatusCode.OK, json)
        }
        val repository = VibeDevRepository(MagicianConnectionSnapshot("https://magician.test", emptyMap()), HttpClient(engine))
        val page = repository.runHistory(20, cursor)
        assertEquals("original", page.runs.single().workLocation)
        assertEquals("/owner/app", page.runs.single().repoPath)
        assertEquals(2, page.runs.single().needsYouCount)
        assertTrue(!page.hasMore)
    }

    @Test
    fun `coding models preserve engine and effort on actual run requests without profile fields`() = runBlocking {
        val engines = listOf("pi", "claude_code", "codex_app_server", "grok_acp", "agy_cli")
        val modelId = { engine: String -> if (engine == "pi") "provider/model" else "native/$engine/provider/model" }
        val models = engines.joinToString(",") { engine ->
            """{"id":"${modelId(engine)}","engine":"$engine","label":"Model","choices":{"auto":"$engine/auto","high":"$engine/high"}}"""
        }
        val statuses = engines.joinToString(",") { """{"id":"$it","selectable":true,"readiness":"ready"}""" }
        val payload = """{"models":[$models],"engines":[$statuses],"default_selection":{"kind":"model","engine":"pi","model_id":"provider/model","thinking":"high"}}"""
        val posted = mutableListOf<kotlinx.serialization.json.JsonObject>()
        val engine = MockEngine { request ->
            if (request.method == HttpMethod.Get) {
                assertEquals("/api/magician/v2/coding/models", request.url.encodedPath)
                respond(payload, HttpStatusCode.OK, json)
            } else {
                assertEquals("/api/magician/v2/vibedev/runs", request.url.encodedPath)
                posted += Json.parseToJsonElement(String(request.body.toByteArray())).jsonObject
                respond("""{"task_id":"started"}""", HttpStatusCode.OK, json)
            }
        }
        val repository = VibeDevRepository(MagicianConnectionSnapshot("https://magician.test", emptyMap()), HttpClient(engine))
        val rows = repository.codingProfiles()
        assertEquals("pi/high", rows.single { it.isDefault }.id)
        for (selectedEngine in engines) {
            for (thinking in listOf("auto", "high")) {
                val selection = codingModelSelection(rows, "$selectedEngine/$thinking")
                repository.startRun(VibeDevRunRequest("Build", "project", clientRunId = "id-$selectedEngine-$thinking", codingChoice = selection))
                val sent = posted.last()
                val choice = sent["coding_choice"]!!.jsonObject
                assertEquals("model", choice["kind"]!!.jsonPrimitive.content)
                assertEquals(selectedEngine, choice["engine"]!!.jsonPrimitive.content)
                assertEquals(modelId(selectedEngine), choice["model_id"]!!.jsonPrimitive.content)
                assertEquals(thinking, choice["thinking"]!!.jsonPrimitive.content)
                assertTrue(!sent.containsKey("coding_profile_id"))
                assertTrue(!choice.containsKey("profile_id"))
            }
        }
        try {
            codingModelSelection(rows, "retired-profile")
            fail("an unavailable explicit selection must not become the default")
        } catch (_: IllegalArgumentException) { }
        assertEquals(10, posted.size)
    }

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
