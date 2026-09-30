package ai.magicbeans.magdroid.voice

import java.io.IOException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ConcurrentVoiceTest {
    private val json = Json { ignoreUnknownKeys = true }
    private fun row(id: String) = VoiceRequest(id, "parent-$id", "branch-$id", "Topic $id", "completed", "pending", "Answer $id")
    private class Fixture {
        val json = Json { ignoreUnknownKeys = true }
        var rows = listOf(VoiceRequest("a", "parent-a", "branch-a", "Topic a", "completed", "pending", "Answer a"))
        var revision = 0L
        var output: VoiceOutput? = null
        var busy = false
        var eligible = true
        var automatic = true
        var now = 100_000L
        var onClaim: () -> Unit = {}
        val commands = mutableListOf<JsonObject>()
        val paths = mutableListOf<String>()
        var started: (() -> Unit)? = null
        var finished: ((SpeechOutcome) -> Unit)? = null
        var plays = 0
        val coordinator = ConcurrentVoiceCoordinator(
            CoroutineScope(Dispatchers.Unconfined),
            request = { path, body ->
                paths += path
                if (body != null) {
                    commands += body
                    when (body["action"]?.jsonPrimitive?.content) {
                        "acquire" -> output = VoiceOutput(body["device_id"]!!.jsonPrimitive.content, body["interaction_id"]!!.jsonPrimitive.content, 1)
                        "claim" -> onClaim()
                        "playback" -> if (body["event"]!!.jsonPrimitive.content == "completed") rows = rows.map { it.copy(deliveryStatus = "played") }
                    }
                }
                if (body?.containsKey("submission_id") == true) json.encodeToJsonElement(rows.first()).jsonObject
                else json.encodeToJsonElement(VoiceRequestSnapshot(++revision, rows, output)).jsonObject
            },
            eligible = { eligible }, outputBusy = { busy },
            play = { _, start, finish -> plays++; started = start; finished = finish },
            stopPlayback = { finished?.invoke(SpeechOutcome.Cancelled) }, focusChanged = {}, now = { now }, automaticPlayback = { automatic },
        )
        fun events() = commands.mapNotNull { it["event"]?.jsonPrimitive?.content }
    }

    @Test fun `panel lifetime follows active unread and selected context`() {
        val ready = row("a")
        assertTrue(ready.visible(null))
        val read = ready.copy(readAt = 42)
        assertFalse(read.visible(null)); assertTrue(read.visible("a"))
        assertTrue(read.copy(pendingTasks = listOf("task")).visible(null))
        assertFalse(ready.copy(deliveryStatus = "played").visible(null))
        assertTrue(ready.copy(deliveryStatus = "played").visible("a"))
        assertFalse(read.copy(deliveryStatus = "dismissed").visible("a"))
    }
    @Test fun `read result stays quiet but explicit replay works`() = runBlocking {
        val f = Fixture(); f.rows = f.rows.map { it.copy(readAt = 42) }; f.coordinator.activate()
        f.coordinator.tick(); assertEquals(0, f.plays)
        f.coordinator.replay("a"); f.coordinator.tick(); assertEquals(1, f.plays)
    }
    @Test fun `dismissed selection clears after already captured input settles`() = runBlocking {
        val f = Fixture(); f.coordinator.select(row("a")); f.coordinator.captureStarted()
        f.rows = f.rows.map { it.copy(deliveryStatus = "dismissed") }; f.coordinator.tick()
        assertNull(f.coordinator.state.value.focus)
        assertEquals("branch-a", f.coordinator.target("parent-a").second)
        f.coordinator.captureStopped(); f.coordinator.inputSettled()
        assertNull(f.coordinator.target("parent-a").second)
    }
    @Test fun `ready result waits for foreground audio and only completes on device callback`() = runBlocking {
        val f = Fixture(); f.coordinator.activate(); f.busy = true
        f.coordinator.tick(); assertEquals(0, f.plays)
        f.busy = false; f.coordinator.tick(); assertEquals(1, f.plays)
        assertFalse(f.events().contains("completed")); assertNull(f.coordinator.state.value.focus)
        f.started!!.invoke(); yield()
        assertEquals("a", f.coordinator.state.value.focus?.id)
        assertTrue(f.events().contains("started")); assertFalse(f.events().contains("completed"))
        f.finished!!.invoke(SpeechOutcome.Completed); yield()
        assertTrue(f.events().contains("completed"))
    }
    @Test fun `capture opening during claim rejects audio without cancelling work`() = runBlocking {
        val f = Fixture(); f.coordinator.activate(); f.onClaim = { f.coordinator.captureStarted() }
        f.coordinator.tick()
        assertEquals(0, f.plays); assertTrue(f.events().contains("rejected"))
        assertTrue(f.paths.none { it.endsWith("/cancel") })
    }
    @Test fun `interruption keeps work and records interrupted delivery`() = runBlocking {
        val f = Fixture(); f.coordinator.activate(); f.coordinator.tick(); f.started!!.invoke(); yield()
        f.coordinator.captureStarted(); yield()
        assertTrue(f.events().contains("interrupted")); assertFalse(f.events().contains("completed"))
        assertTrue(f.paths.none { it.endsWith("/cancel") })
    }
    @Test fun `capture freezes topic even when another topic is selected`() {
        val f = Fixture(); f.coordinator.select(row("a")); f.coordinator.captureStarted()
        f.coordinator.select(row("b"))
        assertEquals("parent-a" to "branch-a", f.coordinator.target("parent-b"))
        f.coordinator.captureStopped(); f.coordinator.inputSettled()
        assertEquals("parent-b" to "branch-b", f.coordinator.target("parent-b"))
    }
    @Test fun `hidden device does not claim or start audio`() = runBlocking {
        val f = Fixture(); f.coordinator.activate(); f.eligible = false; f.coordinator.tick()
        assertEquals(0, f.plays); assertTrue(f.commands.isEmpty())
    }
    @Test fun `manual read works with automatic speech disabled`() = runBlocking {
        val f = Fixture(); f.automatic = false; f.coordinator.activate()
        f.coordinator.tick(); assertEquals(0, f.plays)
        f.coordinator.replay("a"); f.coordinator.tick(); assertEquals(1, f.plays)
        f.started!!.invoke(); yield(); assertTrue(f.events().contains("started"))
    }
    @Test fun `another device owns output without causing claim errors`() = runBlocking {
        val f = Fixture(); f.output = VoiceOutput("other", "other", 2, f.now + 30_000)
        f.coordinator.activate(); f.coordinator.tick()
        assertEquals(0, f.plays); assertTrue(f.commands.isEmpty()); assertNull(f.coordinator.state.value.error)
    }
    @Test fun `lost admission acknowledgement reuses exact key`() = runBlocking {
        val bodies = mutableListOf<JsonObject>()
        val coordinator = ConcurrentVoiceCoordinator(
            this, { _, body ->
                bodies += body!!
                if (bodies.size == 1) throw IOException("lost acknowledgement")
                json.encodeToJsonElement(row("a")).jsonObject
            }, { false }, { false }, { _, _, _ -> }, {}, {},
        )
        coordinator.submit("parent-a", "Question", JsonObject(emptyMap()))
        assertEquals(2, bodies.size); assertEquals(bodies[0], bodies[1])
    }
    @Test fun `typed parallel send stays in its chat without releasing pending voice input`() = runBlocking {
        val f = Fixture(); f.coordinator.select(row("a")); f.coordinator.captureStarted(); f.coordinator.captureStopped()
        f.coordinator.select(row("b"))
        f.coordinator.submit("typed-parent", "Typed question", JsonObject(emptyMap()))
        assertTrue(f.paths.contains("/chat/sessions/typed-parent/voice/requests"))
        assertFalse(f.commands.first().containsKey("context_session_id"))
        assertEquals("parent-a" to "branch-a", f.coordinator.target("typed-parent"))
        f.now += 1000; f.coordinator.tick(); assertEquals(0, f.plays)
        f.coordinator.inputSettled(); f.now += 1000; f.coordinator.tick(); assertEquals(1, f.plays)
        assertTrue(f.paths.none { it.endsWith("/cancel") })
    }

}
