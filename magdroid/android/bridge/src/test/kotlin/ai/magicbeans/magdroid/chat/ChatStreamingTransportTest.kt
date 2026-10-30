package ai.magicbeans.magdroid.chat

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.plugins.HttpTimeout
import io.ktor.http.HttpHeaders
import io.ktor.http.headersOf
import io.ktor.http.content.TextContent
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import io.ktor.utils.io.ByteChannel
import io.ktor.utils.io.writeStringUtf8
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.toList
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
    fun `model catalog and transport use typed selections without profile aliases`() = runBlocking {
        val posted = mutableListOf<kotlinx.serialization.json.JsonObject>()
        val client = HttpClient(MockEngine { request ->
            when {
                request.url.encodedPath.endsWith("/chat/models") -> {
                    assertEquals("pi", request.url.parameters["engine"])
                    respond("""{"models":[{"id":"custom/model","label":"Custom model","choices":{"auto":"opaque-auto","high":"opaque-high"}}],"default_selection":{"model_id":"custom/model","thinking":"auto"}}""",
                        headers = headersOf(HttpHeaders.ContentType, "application/json"))
                }
                else -> {
                    posted += Json.parseToJsonElement((request.body as TextContent).text).jsonObject
                    if (request.url.encodedPath.endsWith("/queue")) respond("""{"queued":{"id":"queued-typed"}}""", headers = headersOf(HttpHeaders.ContentType, "application/json"))
                    else respond("event: token\ndata: {\"text\":\"Answer\"}\n\n", headers = headersOf(HttpHeaders.ContentType, "text/event-stream"))
                }
            }
        })
        val repository = repository(client)
        try {
            val rows = repository.models("pi").rows()
            val selection = rows.single { it.name == "opaque-high" }.modelSelection
            repository.send("session", "Stream", "turn", modelSelection = selection, harnessEngine = "pi", harnessModel = "default").first()
            assertEquals("queued-typed", repository.enqueue("session", SendMessageRequest("Queued", "queue-turn", modelSelection = selection, harnessEngine = "pi")))
            assertEquals(2, posted.size)
            for (body in posted) {
                assertEquals("\"pi\"", body["harness_engine"].toString())
                assertEquals("\"custom/model\"", body["model_selection"]?.jsonObject?.get("model_id").toString())
                assertEquals("\"high\"", body["model_selection"]?.jsonObject?.get("thinking").toString())
                assertTrue(!body.containsKey("profile") && !body.containsKey("harness_model"))
            }
        } finally { repository.close() }
    }

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

    /** The server may keep the turn (`continue_on_disconnect`); the end says it was a drop. */
    @Test
    fun `a stream that closes without done is marked dropped`() = runBlocking {
        val client = HttpClient(MockEngine {
            respond(
                "event: token\ndata: {\"text\":\"Half\"}\n\n",
                headers = headersOf(HttpHeaders.ContentType, "text/event-stream"),
            )
        })
        val repository = repository(client)
        try {
            val events = withTimeout(3_000) { repository.send("session", "Hello", "turn").toList() }
            assertEquals(ChatStreamEvent.Token("Half"), events.first())
            assertEquals(ChatStreamEvent.Done(dropped = true), events.last())
        } finally {
            repository.close()
        }
    }

    @Test
    fun `a session read carries its latest turn state`() = runBlocking {
        val client = HttpClient(MockEngine { request ->
            assertEquals("/api/magician/v2/chat/sessions/session-1", request.url.encodedPath)
            respond(
                """{"session":{"id":"session-1"},"messages":[{"id":"m1","direction":"user",
                "content":{"type":"text","text":"research this"},"chat_turn_id":"turn-1"}],
                "turn_state":{"chat_turn_id":"turn-1","status":"running","started_at":42}}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = repository(client)
        try {
            val snapshot = repository.conversationSnapshot("session-1")
            assertEquals(listOf("m1"), snapshot.messages.map { it.id })
            assertTrue(snapshot.turnState.supported)
            val state = snapshot.turnState.state!!
            assertEquals("turn-1", state.chatTurnId)
            assertTrue(state.running)
            assertEquals("session-1", state.sessionId)
        } finally {
            repository.close()
        }
    }

    @Test
    fun `a queue read carries the latest turn state`() = runBlocking {
        val client = HttpClient(MockEngine { request ->
            assertEquals("/api/magician/v2/chat/sessions/session-1/queue", request.url.encodedPath)
            respond(
                """{"queued":[],"active":false,"session_id":"session-1","turn_state":{"chat_turn_id":"turn-1",
                "status":"stopped","started_at":1,"ended_at":2,"reason":"user_stop"}}""",
                headers = headersOf(HttpHeaders.ContentType, "application/json"),
            )
        })
        val repository = repository(client)
        try {
            val queue = repository.queueSnapshot("session-1")
            assertTrue(queue.turnState.state!!.stoppedByUser)
            assertEquals(2L, queue.turnState.state!!.endedAtMs)
        } finally {
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
