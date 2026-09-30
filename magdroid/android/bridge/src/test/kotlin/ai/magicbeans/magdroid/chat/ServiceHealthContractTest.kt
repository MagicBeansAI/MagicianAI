package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Android reads the same single aggregated health contract as iOS. */
class ServiceHealthContractTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `aggregated root health projects all three service rows`() {
        val body = json.decodeFromString(
            ServiceHealthBody.serializer(),
            """{"status":"ok","version":"0.6.1126","magician":"healthy",
                "magicutor_status":"offline","tauri_status":"ready"}""",
        )
        val stack = body.stack()
        assertTrue(stack.magician.reachable)
        assertEquals("0.6.1126", stack.magician.version)
        assertFalse(stack.magicutor.reachable)
        assertEquals("Offline", stack.magicutor.detail)
        assertTrue(stack.desktop.reachable)
    }

    @Test
    fun `missing dependency status is unknown rather than falsely offline`() {
        val stack = ServiceHealthBody(version = "1").stack()
        assertTrue(stack.magician.reachable)
        assertEquals("Not reported", stack.magicutor.detail)
        assertEquals("Not reported", stack.desktop.detail)
    }

    // ── Service versions ─────────────────────────────────────────────────────

    /**
     * The sibling versions the About list shows.
     *
     * Read opportunistically, as iOS reads them: the endpoint has sent more than
     * one spelling, and a version missing from the body means that service is
     * not deployed rather than that it is unwell.
     */
    @Test
    fun `sibling service versions are carried through`() {
        val stack = chatJson.decodeFromString(
            ServiceHealthBody.serializer(),
            """{"version":"0.6.9","magician":"healthy",
                "magicutor_status":"healthy","magicutor_version":"0.4.1",
                "tauri_status":"healthy","tauri_version":"0.2.0",
                "supervisor_version":"0.1.7"}""",
        ).stack()

        assertEquals("0.6.9", stack.magician.version)
        assertEquals("0.4.1", stack.magicutor.version)
        assertEquals("0.2.0", stack.desktop.version)
        assertEquals("0.1.7", stack.supervisorVersion)
    }

    @Test
    fun `the alternate supervisor spelling is accepted`() {
        val stack = chatJson.decodeFromString(
            ServiceHealthBody.serializer(),
            """{"version":"1","supervisor":"0.1.7"}""",
        ).stack()
        assertEquals("0.1.7", stack.supervisorVersion)
    }

    /**
     * A body with no sibling versions reports none, rather than reporting them
     * as broken.
     *
     * The About list shows a row only when a version came back — a row reading
     * "—" for a service that is simply not deployed says something is wrong
     * with it, which is a different claim from saying nothing.
     */
    @Test
    fun `absent versions stay absent`() {
        val stack = chatJson.decodeFromString(
            ServiceHealthBody.serializer(),
            """{"version":"1","magician":"healthy"}""",
        ).stack()
        assertNull(stack.magicutor.version)
        assertNull(stack.desktop.version)
        assertNull(stack.supervisorVersion)
    }

    /**
     * A version is not a health check.
     *
     * The supervisor reports one and no status, so it is carried as a bare
     * string — forcing it into a ServiceHealth would render "Offline" for a
     * service the endpoint never claimed to be watching.
     */
    @Test
    fun `a supervisor version does not imply a supervisor status`() {
        val stack = chatJson.decodeFromString(
            ServiceHealthBody.serializer(),
            """{"version":"1","supervisor_version":"0.1.7"}""",
        ).stack()
        assertEquals("0.1.7", stack.supervisorVersion)
        // Nothing was said about magicutor or the desktop gateway either.
        assertFalse(stack.magicutor.reachable)
        assertEquals("Not reported", stack.magicutor.detail)
    }
}
