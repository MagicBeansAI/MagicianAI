package ai.magicbeans.magdroid.realtime

import ai.magicbeans.magdroid.tutor.TutorRun
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The hub end to end, over a fake socket. */
class RealtimeHubTest {

    private val hubScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val connector = FakeHubConnector()

    @After
    fun tearDown() {
        hubScope.cancel()
    }

    private fun CoroutineScope.collectInto(
        hub: RealtimeHub,
        filter: HubFilter = HubFilter.ALL,
    ): Channel<HubDelivery> {
        val out = Channel<HubDelivery>(Channel.UNLIMITED)
        // Undispatched, so the subscription is registered before this returns
        // and no frame the test pushes next can miss it.
        launch(Dispatchers.Default, start = CoroutineStart.UNDISPATCHED) {
            hub.subscribe(filter).collect { out.send(it) }
        }
        return out
    }

    @Test
    fun `one socket serves every subscriber, each through its own filter`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope)
            val tasks = collectInto(hub, HubFilter(eventTypes = listOf("V3Task")))
            val maps = collectInto(hub, HubFilter(eventTypes = listOf("ThinkingMap")))
            val socket = connector.connections.receive()

            socket.push("""{"event_type":"ThinkingMapUpdated","data":{"map_id":"m"}}""")
            socket.push("""{"event_type":"V3TaskUpdated","data":{"task_id":"t"}}""")

            assertEquals("V3TaskUpdated", (tasks.receive() as HubDelivery.Event).event.eventType)
            assertEquals("ThinkingMapUpdated", (maps.receive() as HubDelivery.Event).event.eventType)
            assertEquals(1, connector.urls.size)
            assertEquals("wss://mobile.example/api/magician/v2/realtime/ws", connector.urls.single())
            coroutineContext.cancelChildren()
        }
    }

    /** The current server sends no hello: every reconnect is a resync. */
    @Test
    fun `a reconnect to a legacy server resyncs every subscriber`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope)
            val deliveries = collectInto(hub)
            val first = connector.connections.receive()
            first.push("""{"event_type":"V3TaskUpdated","data":{}}""")
            assertTrue(deliveries.receive() is HubDelivery.Event)

            first.close()
            connector.connections.receive()
            assertEquals(HubDelivery.Resync("reconnect"), deliveries.receive())
            // No resume on a server that never stamped a cursor.
            assertTrue(connector.urls.all { !it.contains("resume_") })
            coroutineContext.cancelChildren()
        }
    }

    @Test
    fun `a reconnect to a hub-aware server resumes from the last seq`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope)
            val deliveries = collectInto(hub)
            val first = connector.connections.receive()
            first.push("""{"type":"hub.hello","epoch":"boot-1","seq":10,"resumed":false}""")
            first.push("""{"event_type":"V3TaskUpdated","data":{},"epoch":"boot-1","seq":11}""")
            assertEquals(11L, (deliveries.receive() as HubDelivery.Event).event.seq)

            first.close()
            val second = connector.connections.receive()
            assertEquals(
                "wss://mobile.example/api/magician/v2/realtime/ws?resume_epoch=boot-1&resume_seq=11",
                connector.urls.last(),
            )
            second.push("""{"type":"hub.hello","epoch":"boot-1","seq":12,"resumed":true}""")
            second.push("""{"event_type":"V3TaskUpdated","data":{},"epoch":"boot-1","seq":11}""")
            second.push("""{"event_type":"V3TaskUpdated","data":{},"epoch":"boot-1","seq":12}""")
            // The duplicate 11 is dropped and no resync is owed.
            assertEquals(12L, (deliveries.receive() as HubDelivery.Event).event.seq)
            coroutineContext.cancelChildren()
        }
    }

    @Test
    fun `the keepalive is the app ping`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope, keepaliveMs = 20)
            collectInto(hub)
            val socket = connector.connections.receive()
            while (socket.sent.isEmpty()) delay(10)
            assertEquals("""{"type":"ping"}""", socket.sent.first())
            coroutineContext.cancelChildren()
        }
    }

    /**
     * The tutor bug: the socket wraps tutor events in `AgentEvent`, and the
     * run read the outer `event_type`. Through the hub it gets the inner one.
     */
    @Test
    fun `a wrapped tutor event reaches the run unwrapped and draws`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope)
            val deliveries = collectInto(hub, HubFilter(eventTypes = listOf("tutor.")))
            val socket = connector.connections.receive()
            val wrapped = """{"event_type":"AgentEvent","data":{"event":{"event_type":"tutor.draw.shape",""" +
                """"agent_id":"tutor","payload":{"shape":{"type":"rect","x":1,"y":2,"w":3,"h":4}}}}}"""
            socket.push("""{"event_type":"ChatTokenDelta","data":{"token":"hi"}}""")
            socket.push(wrapped)

            val event = (deliveries.receive() as HubDelivery.Event).event
            val run = TutorRun()
            // The raw frame is what the old socket handed the run: nothing drew.
            assertEquals(false, TutorRun().apply(event.raw))
            assertTrue(run.apply(event.unwrapped))
            assertEquals(listOf("rect"), run.shapes.map { it.type })
            coroutineContext.cancelChildren()
        }
    }

    @Test
    fun `a server resync reaches every subscriber, filtered or not`() = runBlocking {
        withTimeout(5_000) {
            val hub = testHub(connector, hubScope)
            val narrow = collectInto(hub, HubFilter(taskId = "nothing-matches"))
            val socket = connector.connections.receive()
            socket.push("""{"type":"hub.hello","epoch":"b","seq":0,"resumed":false}""")
            socket.push("""{"type":"hub.resync","reason":"lagged"}""")
            assertEquals(HubDelivery.Resync("lagged"), narrow.receive())
            coroutineContext.cancelChildren()
        }
    }
}
