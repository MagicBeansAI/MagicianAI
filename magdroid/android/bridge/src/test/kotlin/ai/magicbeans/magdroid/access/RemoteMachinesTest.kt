package ai.magicbeans.magdroid.access

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.HttpRequestData
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class RemoteMachinesTest {
    private val body = """
        {"self":"macbookpro","machines":[
          {"name":"macbookpro","api_origin":"https://macbookpro.example.com",
           "ui_origin":"https://ui-macbookpro.example.com","is_self":true},
          {"name":"macstudio","api_origin":"https://MacStudio.example.com/",
           "ui_origin":"https://ui-macstudio.example.com","is_self":false},
          {"name":"bad","api_origin":"javascript:alert(1)","ui_origin":"https://x.example"},
          {"name":"plain","api_origin":"http://plain.example.com","ui_origin":"http://plain.example.com"}
        ]}
    """.trimIndent()

    private val snapshot = MagicianConnectionSnapshot(
        "https://macbookpro.example.com",
        mapOf(
            MagicianAccess.HEADER_DEVICE_ID to "magdroid-1",
            MagicianAccess.HEADER_AUTHORIZATION to "Bearer mbp-token",
            MagicianAccess.HEADER_CLIENT_ID to "mbp-id",
            MagicianAccess.HEADER_CLIENT_SECRET to "mbp-secret",
        ),
    )

    @Test fun `parse normalizes origins and drops unusable entries`() {
        val registry = requireNotNull(parseRemoteMachineRegistry(body))
        assertEquals("macbookpro", registry.self)
        assertEquals(listOf("macbookpro", "macstudio"), registry.machines.map { it.name })
        assertEquals("https://macstudio.example.com", registry.machines[1].apiOrigin)
        assertTrue(registry.machines[0].isSelf)
        assertEquals(null, parseRemoteMachineRegistry("not json"))
    }

    @Test fun `picker lists paired machines by registry name, then unpaired ones`() {
        val registry = requireNotNull(parseRemoteMachineRegistry(body))
        val paired = listOf(
            PairedMachine("https://macbookpro.example.com", "macbookpro.example.com", active = true),
            PairedMachine("https://old.example.com", "old.example.com", active = false),
        )
        val rows = machinePickerRows(paired, registry)

        assertEquals(listOf("macbookpro", "old.example.com", "macstudio"), rows.map { it.title })
        assertEquals(listOf(true, true, false), rows.map { it.paired })
        assertEquals(listOf(true, false, false), rows.map { it.active })
        assertEquals("https://ui-macstudio.example.com", rows[2].uiOrigin)
        // Without a registry (older machine, offline), paired rows fall back to the host.
        assertEquals(listOf("macbookpro.example.com"), machinePickerRows(paired.take(1), null).map { it.title })
    }

    @Test fun `a stored machine name titles its row and hides its registry twin`() {
        val registry = requireNotNull(parseRemoteMachineRegistry(body))
        val paired = listOf(
            PairedMachine("http://192.168.1.20:3002", "192.168.1.20:3002", active = true, machineName = "macbookpro"),
        )
        val rows = machinePickerRows(paired, registry)

        assertEquals(listOf("macbookpro", "macstudio"), rows.map { it.title })
        assertEquals(listOf(true, false), rows.map { it.paired })
        assertEquals(registry.machines[0].uiOrigin, rows[0].uiOrigin)
        assertEquals(listOf("macbookpro"), machinePickerRows(paired, null).map { it.title })
    }

    @Test fun `fetch sends the active machine's own credential`() = runTest {
        val requests = mutableListOf<HttpRequestData>()
        val engine = MockEngine { request ->
            requests += request
            respond(body, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        val registry = RemoteMachinesClient(HttpClient(engine)) { snapshot }.fetch()

        assertEquals(2, registry.machines.size)
        val request = requests.single()
        assertEquals("https://macbookpro.example.com/api/magician/v2/remote-machines", request.url.toString())
        assertEquals("Bearer mbp-token", request.headers[MagicianAccess.HEADER_AUTHORIZATION])
        assertEquals("mbp-id", request.headers[MagicianAccess.HEADER_CLIENT_ID])
        assertEquals("magdroid-1", request.headers[MagicianAccess.HEADER_DEVICE_ID])
    }

    @Test fun `an older machine without the endpoint lists nothing`() = runTest {
        val engine = MockEngine { respond("", HttpStatusCode.NotFound) }
        val registry = RemoteMachinesClient(HttpClient(engine)) { snapshot }.fetch()
        assertTrue(registry.machines.isEmpty())
    }

    @Test fun `fetch fails plainly without an active machine or on an error`() = runTest {
        val engine = MockEngine { respond("", HttpStatusCode.Unauthorized) }
        val failed = runCatching { RemoteMachinesClient(HttpClient(engine)) { snapshot }.fetch() }
        assertTrue(failed.exceptionOrNull() is IOException)
        val inactive = runCatching { RemoteMachinesClient(HttpClient(engine)) { null }.fetch() }
        assertTrue(inactive.exceptionOrNull() is IOException)
    }
}
