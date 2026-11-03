package ai.magicbeans.magdroid.chat

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

/** A thread's slug follows its name; links resolve to its fixed id, chips show its name. */
class ThreadDirectoryTest {
    @Before fun reset() = ThreadDirectory.clear()
    @After fun tearDown() = ThreadDirectory.clear()

    @Test fun `a listed thread resolves by id or slug and is labelled by name`() {
        ThreadDirectory.remember(listOf(UiThreadRecord(id = "thr_1", name = "Launch plan", slug = "launch-plan")))
        assertEquals("thr_1", ThreadDirectory.canonicalId("thr_1"))
        assertEquals("thr_1", ThreadDirectory.canonicalId("launch-plan"))
        assertNull(ThreadDirectory.canonicalId("old-name"))
        assertEquals("#Launch plan", ThreadDirectory.label("thr_1"))
        assertEquals("#unknown", ThreadDirectory.label("unknown"))
        assertEquals("#general", ThreadDirectory.label(null))
    }

    @Test fun `an unknown key is resolved once by the server and cached as an alias`() = runBlocking {
        var reads = 0
        val fetch: suspend (String) -> kotlinx.serialization.json.JsonElement = { path ->
            reads++
            assertEquals("/api/magician/v2/ui-threads/old-name", path)
            Json.parseToJsonElement("""{"thread":{"id":"thr_1","slug":"launch-plan","name":"Launch plan"}}""")
        }
        assertEquals("thr_1", ThreadDirectory.resolve("old-name", fetch))
        assertEquals("thr_1", ThreadDirectory.resolve("old-name", fetch))
        assertEquals(1, reads)
        assertEquals("#Launch plan", ThreadDirectory.label("thr_1"))
    }

    @Test fun `a failed lookup treats the key as the id`() = runBlocking {
        assertEquals("legacy", ThreadDirectory.resolve("legacy") { throw IllegalStateException("offline") })
    }

    @Test fun `create responses decode bare or enveloped`() {
        assertEquals("thr_2", ThreadDirectory.threadRecordOf(Json.parseToJsonElement("""{"id":"thr_2","name":"A"}"""))?.id)
        assertEquals("thr_3", ThreadDirectory.threadRecordOf(Json.parseToJsonElement("""{"thread":{"id":"thr_3","name":"B"}}"""))?.id)
        assertNull(ThreadDirectory.threadRecordOf(Json.parseToJsonElement("""{"error":"no"}""")))
    }

    @Test fun `switching machines forgets names and aliases`() {
        ThreadDirectory.useOrigin("https://one")
        ThreadDirectory.remember(listOf(UiThreadRecord(id = "thr_1", name = "One", slug = "one")))
        ThreadDirectory.useOrigin("https://two")
        assertNull(ThreadDirectory.canonicalId("one"))
        assertEquals("#thr_1", ThreadDirectory.label("thr_1"))
    }
}
