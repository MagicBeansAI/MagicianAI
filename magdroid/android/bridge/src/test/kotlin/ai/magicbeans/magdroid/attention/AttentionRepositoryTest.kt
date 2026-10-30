package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import ai.magicbeans.magdroid.chat.HitlResponseValue
import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.HttpRequestData
import io.ktor.http.HttpStatusCode
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class AttentionRepositoryTest {
    @Test fun `a retryable conflict mentioning already resolved does not dismiss the request`() = runBlocking {
        val repository = AttentionRepository({ first }, HttpClient(MockEngine {
            respond("""{"reason":"busy","message":"not already_resolved; retry"}""", HttpStatusCode.Conflict)
        }))
        try {
            try { repository.respond(ask().request(ask().metadata), HitlResponseValue.Confirmation(true)); fail("must retain request") }
            catch (expected: AttentionError) { assertTrue(expected.message.orEmpty().isNotBlank()) }
        } finally { repository.close() }
    }

    @Test fun `a JSON reply without acceptance is not an acknowledgement`() = runBlocking {
        val repository = AttentionRepository({ first }, HttpClient(MockEngine { respond("{}") }))
        try {
            try { repository.respond(ask().request(ask().metadata), HitlResponseValue.Confirmation(true)); fail("missing acknowledgement") }
            catch (expected: AttentionError) { assertTrue(expected.message.orEmpty().isNotBlank()) }
        } finally { repository.close() }
    }
    @Test fun `an exact request can open outside the loaded inbox page`() = runBlocking {
        var gets = 0
        val repository = AttentionRepository({ first }, HttpClient(MockEngine { request ->
            gets++
            assertTrue(request.url.toString().contains("/feed/attention/pause%2Fa%3A1"))
            respond("""{"id":"exact","status":"needs_action","metadata":{"source":"agentic","input_type":"form","pause_state_id":"pause/a:1","input_schema":{"questions":[{"id":"account","prompt":"Which account?"}]}}}""")
        }))
        try {
            val row = repository.pendingItem("pause/a:1")!!
            assertEquals("account", row.request(row.metadata).formQuestions.first().id)
            assertEquals(first, row.originConnection)
            assertEquals(1, gets)
        } finally { repository.close() }
    }
    private val first = MagicianConnectionSnapshot("https://first.example", mapOf("Authorization" to "Bearer first"))
    private fun ask(connection: MagicianConnectionSnapshot? = null) = AttentionItem(id = "ask", originConnection = connection,
        metadata = AttentionMetadata(source = "agentic", inputType = "confirmation", pauseStateId = "pause"))

    @Test fun `composer feed includes every actionable page and reads the ledger only once`() = runBlocking {
        var pages = 0
        var ledgers = 0
        val client = HttpClient(MockEngine { request ->
            assertEquals("Bearer first", request.headers["Authorization"])
            if (request.url.encodedPath.endsWith("user-requests")) {
                ledgers++
                respond("""{"requests":[]}""")
            } else {
                pages++
                when (request.url.parameters["requests_cursor"]) {
                    null -> respond("""{"requests":[{"id":"new","status":"needs_action"}],"pages":{"requests":{"next_cursor":"older"}}}""")
                    "older" -> respond("""{"requests":[{"id":"old","status":"needs_action"}],"pages":{}}""")
                    else -> error("unexpected cursor")
                }
            }
        })
        val repository = AttentionRepository({ first }, client)
        try {
            val feed = repository.pendingFeed()
            assertEquals(listOf("new", "old"), feed.requests.map { it.id })
            assertTrue(feed.requests.all { it.originConnection == first })
            assertFalse(attentionJson.encodeToString(AttentionItem.serializer(), feed.requests.first()).contains("Bearer"))
            assertEquals(2, pages); assertEquals(1, ledgers)
        } finally { repository.close() }
    }

    @Test fun `old approvals and approve-run followups cannot target a new connection`() = runBlocking {
        val current = first.copy(baseUrl = "https://second.example")
        var writes = 0
        val repository = AttentionRepository({ current }, HttpClient(MockEngine { writes++; respond("{}") }))
        try {
            for (action in listOf<suspend () -> Unit>(
                { repository.respond(ask(first).request(ask(first).metadata), HitlResponseValue.Confirmation(true)) },
                { repository.startRun("task", first) },
            )) {
                try { action(); fail("stale approval must be refused") }
                catch (expected: AttentionError) { assertTrue(expected.message.orEmpty().contains("Connection changed")) }
            }
            assertEquals(0, writes)
        } finally { repository.close() }
    }

    @Test fun `reasks and negative resume acknowledgements do not settle an ask`() = runBlocking {
        for (body in listOf("""{"status":"reask_required","resumed":false,"message":"Please give the missing account"}""",
            """{"accepted":true,"resumed":false,"reason":"not ready"}""")) {
            val repository = AttentionRepository({ first }, HttpClient(MockEngine { respond(body, HttpStatusCode.OK) }))
            try {
                try { repository.respond(ask().request(ask().metadata), HitlResponseValue.Confirmation(true)); fail("must retain ask") }
                catch (expected: AttentionError) { assertTrue(expected.message.orEmpty().isNotBlank()) }
            } finally { repository.close() }
        }
    }

    @Test fun `a successful legacy resume remains accepted`() = runBlocking {
        val repository = AttentionRepository({ first }, HttpClient(MockEngine { respond("""{"resumed":true}""") }))
        try { repository.respond(ask().request(ask().metadata), HitlResponseValue.Confirmation(true)) }
        finally { repository.close() }
    }

    @Test fun `a connection switch during response cannot settle the new connection's asks`() = runBlocking {
        var current = first
        val repository = AttentionRepository({ current }, HttpClient(MockEngine {
            current = first.copy(baseUrl = "https://second.example")
            respond("""{"accepted":true}""")
        }))
        try {
            try { repository.respond(ask(first).request(ask(first).metadata), HitlResponseValue.Confirmation(true)); fail("stale completion") }
            catch (expected: AttentionError) { assertTrue(expected.message.orEmpty().contains("Connection changed")) }
        } finally { repository.close() }
    }
}
