package ai.magicbeans.magdroid.realtime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

/** Resume, dedupe and resync, against both a hub-aware and a legacy server. */
class HubProtocolTest {

    private fun hello(epoch: String, seq: Long, resumed: Boolean) =
        """{"type":"hub.hello","epoch":"$epoch","seq":$seq,"resumed":$resumed}"""

    private fun stamped(epoch: String, seq: Long) =
        """{"event_type":"V3TaskUpdated","data":{"task_id":"t$seq"},"epoch":"$epoch","seq":$seq}"""

    private val legacy = """{"event_type":"V3TaskUpdated","data":{"task_id":"t"}}"""

    private fun List<HubOutput>.resyncs() = filterIsInstance<HubOutput.Resync>()
    private fun List<HubOutput>.delivered() = filterIsInstance<HubOutput.Deliver>()

    @Test
    fun `the first connection never resyncs`() {
        val protocol = HubProtocol()
        assertTrue(protocol.onOpen().isEmpty())
        assertTrue(protocol.onText(hello("b1", 10, resumed = false)).isEmpty())
        assertEquals(HubCursor("b1", 10), protocol.cursor)
    }

    @Test
    fun `a legacy server resyncs on every reconnect, without waiting for a hello`() {
        val protocol = HubProtocol()
        protocol.onOpen()
        assertEquals(1, protocol.onText(legacy).delivered().size)
        assertNull(protocol.resumeCursor())

        assertEquals(listOf(HubOutput.Resync("reconnect")), protocol.onOpen())
        // Unstamped events are never deduped.
        assertEquals(1, protocol.onText(legacy).delivered().size)
        assertEquals(1, protocol.onText(legacy).delivered().size)
    }

    @Test
    fun `a resumed reconnect replays without a resync and drops what it already has`() {
        val protocol = HubProtocol()
        protocol.onOpen()
        protocol.onText(hello("b1", 0, resumed = false))
        protocol.onText(stamped("b1", 1))
        protocol.onText(stamped("b1", 2))
        assertEquals(HubCursor("b1", 2), protocol.resumeCursor())

        assertTrue(protocol.onOpen().isEmpty())
        assertTrue(protocol.onText(hello("b1", 3, resumed = true)).isEmpty())
        // A replay overlapping what was delivered is dropped by (epoch, seq).
        assertTrue(protocol.onText(stamped("b1", 2)).isEmpty())
        assertEquals(1, protocol.onText(stamped("b1", 3)).delivered().size)
        assertTrue(protocol.onText(stamped("b1", 3)).isEmpty())
        assertEquals(HubCursor("b1", 3), protocol.cursor)
    }

    @Test
    fun `an unresumable reconnect resyncs once, not again for the server resync`() {
        val protocol = HubProtocol()
        protocol.onOpen()
        protocol.onText(hello("b1", 0, resumed = false))
        protocol.onText(stamped("b1", 5))

        protocol.onOpen()
        assertEquals(listOf(HubOutput.Resync("reconnect")), protocol.onText(hello("b1", 900, resumed = false)))
        assertTrue(protocol.onText("""{"type":"hub.resync","reason":"gap"}""").isEmpty())
        assertEquals(HubCursor("b1", 900), protocol.cursor)
        // After an event, a later server resync is a new gap.
        protocol.onText(stamped("b1", 901))
        assertEquals(
            listOf(HubOutput.Resync("lagged")),
            protocol.onText("""{"type":"hub.resync","reason":"lagged"}"""),
        )
    }

    @Test
    fun `a new epoch resyncs and restarts the seq space`() {
        val protocol = HubProtocol()
        protocol.onOpen()
        protocol.onText(hello("b1", 0, resumed = false))
        protocol.onText(stamped("b1", 50))

        protocol.onOpen()
        assertEquals(listOf(HubOutput.Resync("epoch")), protocol.onText(hello("b2", 3, resumed = false)))
        // seq 4 is below the old epoch's 50 but new in this one.
        assertEquals(1, protocol.onText(stamped("b2", 4)).delivered().size)
    }

    @Test
    fun `a hub-aware server that comes back legacy resyncs on its first event`() {
        val protocol = HubProtocol()
        protocol.onOpen()
        protocol.onText(hello("b1", 0, resumed = false))

        assertTrue(protocol.onOpen().isEmpty())
        val outputs = protocol.onText(legacy)
        assertEquals(1, outputs.resyncs().size)
        assertEquals(1, outputs.delivered().size)
        assertNull(protocol.resumeCursor())
    }

    @Test
    fun `the resume URL carries the cursor`() {
        assertEquals(
            "wss://mobile.example/api/magician/v2/realtime/ws",
            realtimeHubUrl("https://mobile.example/", null),
        )
        assertEquals(
            "ws://10.0.2.2:8080/api/magician/v2/realtime/ws?resume_epoch=boot+1%2F2&resume_seq=17",
            realtimeHubUrl("http://10.0.2.2:8080", HubCursor("boot 1/2", 17)),
        )
        assertEquals(
            "wss://bare.example/api/magician/v2/realtime/ws",
            realtimeHubUrl("bare.example", HubCursor("", 3)),
        )
    }

    @Test
    fun `backoff is jittered between 750 ms and 30 s`() {
        val random = Random(7)
        repeat(200) { attempt ->
            val wait = hubBackoffMs(attempt % 12, random)
            assertTrue("$wait", wait in MIN_BACKOFF_MS..MAX_BACKOFF_MS)
        }
        repeat(50) { assertTrue(hubBackoffMs(0, random) <= 1_000L) }
    }
}
