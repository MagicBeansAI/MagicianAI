package ai.magicbeans.magdroid.chat

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.plugins.HttpTimeout
import io.ktor.http.HttpHeaders
import io.ktor.http.headersOf
import io.ktor.utils.io.ByteChannel
import io.ktor.utils.io.writeStringUtf8
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The server deliberately keeps each body open: receiving before EOF is the contract. */
class ChatStreamingTransportTest {
    @Test
    fun `reply stream can wait for a person beyond the ordinary request budget`() = runBlocking {
        val body = ByteChannel(autoFlush = true)
        val client = HttpClient(MockEngine {
            respond(body, headers = headersOf(HttpHeaders.ContentType, "text/event-stream"))
        }) { install(HttpTimeout) { requestTimeoutMillis = 100 } }
        val repository = repository(client)
        val producer = launch {
            delay(500)
            body.writeStringUtf8("event: token\ndata: {\"text\":\"Answered\"}\n\n")
        }
        try {
            assertEquals(ChatStreamEvent.Token("Answered"), withTimeout(3_000) {
                repository.send("session", "Question", "turn").first()
            })
        } finally {
            producer.cancel()
            body.close()
            repository.close()
        }
    }

    @Test
    fun `activity stream can stay open beyond the ordinary request budget`() = runBlocking {
        val body = ByteChannel(autoFlush = true)
        val client = HttpClient(MockEngine {
            respond(body, headers = headersOf(HttpHeaders.ContentType, "application/x-ndjson"))
        }) { install(HttpTimeout) { requestTimeoutMillis = 100 } }
        val repository = repository(client)
        val producer = launch {
            delay(500)
            body.writeStringUtf8("""{"event_type":"HitlRequested","data":{"correlation_id":"later","input_type":"choice","prompt":"Choose later"}}""" + "\n")
        }
        try {
            val rows = withTimeout(3_000) {
                repository.turnActivityStream("session", "turn").first { it.isNotEmpty() }
            }
            assertTrue(rows.any { it.status == "waiting" && it.detail == "Choose later" })
        } finally {
            producer.cancel()
            body.close()
            repository.close()
        }
    }

    @Test
    fun `a reply token arrives while the response body is still open`() = runBlocking {
        val body = ByteChannel(autoFlush = true)
        body.writeStringUtf8("event: token\ndata: {\"text\":\"First words\"}\n\n")
        val client = HttpClient(MockEngine { request ->
            assertEquals("/api/magician/v2/chat/sessions/session/messages/stream", request.url.encodedPath)
            assertEquals("Bearer test-device", request.headers[HttpHeaders.Authorization])
            respond(body, headers = headersOf(HttpHeaders.ContentType, "text/event-stream"))
        })
        val repository = repository(client)
        try {
            val first = withTimeout(3_000) {
                repository.send("session", "Hello", "turn").first()
            }
            assertEquals(ChatStreamEvent.Token("First words"), first)
        } finally {
            body.close()
            repository.close()
        }
    }

    @Test
    fun `a pending question arrives before the activity stream closes`() = runBlocking {
        val body = ByteChannel(autoFlush = true)
        body.writeStringUtf8(
            """{"event_type":"HitlRequested","data":{"correlation_id":"choice-1","input_type":"choice","prompt":"Choose the test color"}}""" + "\n",
        )
        val client = HttpClient(MockEngine { request ->
            assertEquals("/api/magician/v2/chat/sessions/session/turns/turn/events/stream", request.url.encodedPath)
            assertEquals("Bearer test-device", request.headers[HttpHeaders.Authorization])
            respond(body, headers = headersOf(HttpHeaders.ContentType, "application/x-ndjson"))
        })
        val repository = repository(client)
        try {
            val rows = withTimeout(3_000) {
                repository.turnActivityStream("session", "turn").first { it.isNotEmpty() }
            }
            assertTrue(rows.any { it.status == "waiting" })
            assertEquals("Choose the test color", rows.first().detail)
        } finally {
            body.close()
            repository.close()
        }
    }

    private fun repository(client: HttpClient) = ChatRepository(
        client = client,
        baseUrl = { "https://mobile.example" },
        headers = { mapOf(HttpHeaders.Authorization to "Bearer test-device") },
        scope = { "owner" to "private" },
    )
}
