package ai.magicbeans.magdroid.observe

import kotlinx.serialization.json.Json
import org.junit.Assert.assertTrue
import org.junit.Test

/** The local Listen action must retain its calendar identity on the wire. */
class ObservationStartRequestTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun `scheduled listen carries title url and explicit client capture defaults`() {
        val body = json.encodeToString(
            StartRequest.serializer(),
            StartRequest(title = "Design review", url = "https://meet.google.com/abc"),
        )

        assertTrue(body.contains("\"capture\":\"client\""))
        assertTrue(body.contains("\"mic\":true"))
        assertTrue(body.contains("\"title\":\"Design review\""))
        assertTrue(body.contains("\"url\":\"https://meet.google.com/abc\""))
    }
}
