package ai.magicbeans.magdroid.chat

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.headersOf
import io.ktor.http.content.TextContent
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ExecutionModesTest {
    @Test
    fun `mode writes use owner endpoints and explicit durations without starting work`() = runBlocking {
        val writes = mutableListOf<JsonObject>()
        val client = HttpClient(MockEngine { request ->
            assertEquals("Bearer owner-device", request.headers[HttpHeaders.Authorization])
            assertEquals("PUT", request.method.value)
            val body = Json.parseToJsonElement((request.body as TextContent).text).jsonObject
            writes += body
            val response = when (request.url.encodedPath) {
                "/api/magician/v2/chat/sessions/chat-1/god-mode" -> """{"enabled":true,"override":null,"default":true}"""
                "/api/magician/v2/settings/super-god-mode" -> """{"active":false,"enabled":false,"until_ms":null,"remaining_secs":null}"""
                else -> error("Unexpected execution request: ${request.url}")
            }
            respond(response, headers = headersOf(HttpHeaders.ContentType, "application/json"))
        })
        val repository = ChatRepository(client, { "https://owner.test" },
            { mapOf(HttpHeaders.Authorization to "Bearer owner-device") }, { "owner" to "workspace" })
        try {
            assertTrue(repository.chatGodMode("chat-1", true, update = true).enabled)
            repository.chatGodMode("chat-1", null, update = true)
            repository.superGodMode(SuperGodModeDuration.Hour, update = true)
            repository.superGodMode(SuperGodModeDuration.UntilOff, update = true)
            repository.superGodMode(null, update = true)
            assertEquals(JsonPrimitive(true), writes[0]["enabled"])
            assertEquals(JsonNull, writes[1]["enabled"])
            assertEquals(JsonPrimitive("1h"), writes[2]["duration"])
            assertEquals(JsonPrimitive("forever"), writes[3]["duration"])
            assertEquals(buildJsonObject { put("enabled", false) }, writes[4])
        } finally { repository.close() }
    }

    @Test
    fun `the one hour state expires locally and a malformed state never defaults to enabled`() {
        val timed = SuperGodModeState(active = true, enabled = true, untilMs = 1000)
        assertTrue(timed.activeAt(999)); assertFalse(timed.activeAt(1000))
        assertTrue(SuperGodModeState(active = true, enabled = true).activeAt(Long.MAX_VALUE))
        assertTrue(runCatching { chatJson.decodeFromString(SuperGodModeState.serializer(), """{"enabled":true}""") }.isFailure)
    }
}
