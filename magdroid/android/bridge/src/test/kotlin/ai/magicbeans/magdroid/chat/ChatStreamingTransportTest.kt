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
import ai.magicbeans.magdroid.realtime.FakeHubConnector
import ai.magicbeans.magdroid.realtime.RealtimeHub
import ai.magicbeans.magdroid.realtime.testHub
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import java.util.concurrent.atomic.AtomicInteger

/**
 * The send stream's body stays open: receiving before EOF is the contract.
 * Turn activity is a REST read plus the realtime hub.
 */
class ChatStreamingTransportTest {
    @Test
    fun `cancelled voice preference reads propagate cancellation instead of becoming load errors`() = runBlocking {
        val client = HttpClient(MockEngine { throw kotlinx.coroutines.CancellationException("voice panel closed") })
        val repository = repository(client)
        try {
            var cancelled = false
            try { repository.mediaPreferences() }
            catch (_: kotlinx.coroutines.CancellationException) { cancelled = true }
            org.junit.Assert.assertTrue("Panel cancellation must remain cancellation", cancelled)
        } finally { repository.close() }
    }

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

    /**
     * Turn activity is one REST read plus the app's realtime hub — no held
     * NDJSON stream per turn.
     */
    @Test
    fun `activity backfills by REST, then follows the turn on the hub`() = runBlocking {
        val connector = FakeHubConnector()
        val hubScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val client = HttpClient(MockEngine { request ->
            assertEquals("/api/magician/v2/chat/sessions/session/turns/turn/events", request.url.encodedPath)
            respond(
                """{"events":[${hitl("earlier", "Choose earlier")}]}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = repository(client, testHub(connector, hubScope))
        try {
            withTimeout(5_000) {
                val updates = Channel<List<ActivityRow>>(Channel.UNLIMITED)
                val follower = launch { repository.turnActivityStream("session", "turn").collect { updates.send(it) } }
                assertTrue(updates.receive().any { it.detail == "Choose earlier" })

                val socket = connector.connections.receive()
                socket.push(hitl("elsewhere", "Another turn", turn = "other-turn"))
                socket.push(hitl("later", "Choose later", turn = "turn"))
                val rows = updates.receive()
                assertTrue(rows.any { it.status == "waiting" && it.detail == "Choose later" })
                assertTrue(rows.none { it.detail == "Another turn" })
                follower.cancel()
            }
        } finally {
            hubScope.cancel()
            repository.close()
        }
    }

    /** The server maps a subscribed run's events to the turn without stamping it. */
    @Test
    fun `a run named by the turn streams into it without naming the turn`() = runBlocking {
        val connector = FakeHubConnector()
        val hubScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val client = HttpClient(MockEngine {
            respond(
                """{"events":[${hitl("started", "Started", turn = "turn", taskId = "task-7")}]}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = repository(client, testHub(connector, hubScope))
        try {
            withTimeout(5_000) {
                val updates = Channel<List<ActivityRow>>(Channel.UNLIMITED)
                val follower = launch { repository.turnActivityStream("session", "turn").collect { updates.send(it) } }
                updates.receive()
                val socket = connector.connections.receive()
                socket.push(hitl("unrelated", "Unrelated run", taskId = "task-8"))
                socket.push(hitl("linked", "From the run", taskId = "task-7"))
                val rows = updates.receive()
                assertTrue(rows.any { it.detail == "From the run" })
                assertTrue(rows.none { it.detail == "Unrelated run" })
                follower.cancel()
            }
        } finally {
            hubScope.cancel()
            repository.close()
        }
    }

    /** The current server cannot resume, so a reconnect re-reads the turn by REST. */
    @Test
    fun `a hub resync re-reads the turn by REST`() = runBlocking {
        val connector = FakeHubConnector()
        val hubScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val reads = AtomicInteger()
        val client = HttpClient(MockEngine {
            val events = if (reads.incrementAndGet() == 1) hitl("first", "First")
            else hitl("first", "First") + "," + hitl("missed", "Missed while away")
            respond(
                """{"events":[$events]}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = repository(client, testHub(connector, hubScope))
        try {
            withTimeout(5_000) {
                val updates = Channel<List<ActivityRow>>(Channel.UNLIMITED)
                val follower = launch { repository.turnActivityStream("session", "turn").collect { updates.send(it) } }
                updates.receive()
                connector.connections.receive().close()
                connector.connections.receive()
                assertTrue(updates.receive().any { it.detail == "Missed while away" })
                assertEquals(2, reads.get())
                follower.cancel()
            }
        } finally {
            hubScope.cancel()
            repository.close()
        }
    }

    private fun hitl(id: String, prompt: String, turn: String? = null, taskId: String? = null): String {
        val extra = listOfNotNull(
            turn?.let { "\"chat_turn_id\":\"$it\"" },
            taskId?.let { "\"task_id\":\"$it\"" },
        ).joinToString("") { ",$it" }
        return """{"event_type":"HitlRequested","data":{"event_id":"$id","correlation_id":"$id",""" +
            """"input_type":"choice","prompt":"$prompt"$extra}}"""
    }

    private fun repository(client: HttpClient, hub: RealtimeHub) = ChatRepository(
        client = client,
        baseUrl = { "https://mobile.example" },
        headers = { mapOf(HttpHeaders.Authorization to "Bearer test-device") },
        scope = { "owner" to "private" },
        realtimeHub = hub,
    )

    private fun repository(client: HttpClient) = ChatRepository(
        client = client,
        baseUrl = { "https://mobile.example" },
        headers = { mapOf(HttpHeaders.Authorization to "Bearer test-device") },
        scope = { "owner" to "private" },
    )
}
