package ai.magicbeans.magdroid.access

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.HttpRequestData
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DeviceEnrollmentClientTest {
    private val link = DeviceEnrollmentLink(
        baseUrl = "https://mobile.example",
        enrollmentId = "abcdefghijklmnopqrstuvwx",
        secret = "abcdefghijklmnopqrstuvwxyzABCDEFGH123456789",
        clientKind = "android",
    )

    @Test fun `exchange verifies ordinary authenticated route before returning profile`() = runTest {
        val requests = mutableListOf<HttpRequestData>()
        val engine = MockEngine { request ->
            requests += request
            when (request.url.encodedPath) {
                "/api/magician/v2/devices/enrollment/exchange" -> respond(
                    content = grant(capabilities = "[\"mobile_client\"]"),
                    status = HttpStatusCode.OK,
                    headers = jsonHeaders,
                )
                "/api/magician/v2/devices/me" -> respond(
                    content = """{"device_id":"phone-1","principal":"owner","workspace":"private"}""",
                    status = HttpStatusCode.OK,
                    headers = jsonHeaders,
                )
                else -> error("unexpected request ${request.url}")
            }
        }
        val result = DeviceEnrollmentClient(HttpClient(engine), "phone-1", "Pixel")
            .exchange(link)

        assertEquals("https://mobile.example", result.baseUrl)
        assertEquals("outer-secret", result.cloudflareClientSecret)
        assertEquals(2, requests.size)
        assertNull(requests[0].headers[MagicianAccess.HEADER_CLIENT_ID])
        assertEquals("phone-1", requests[1].headers[MagicianAccess.HEADER_DEVICE_ID])
        assertEquals("Bearer device-token", requests[1].headers[MagicianAccess.HEADER_AUTHORIZATION])
        assertEquals("outer-id", requests[1].headers[MagicianAccess.HEADER_CLIENT_ID])
    }

    @Test fun `legacy Android rejects an automation grant and never probes`() = runTest {
        var calls = 0
        val engine = MockEngine {
            calls += 1
            respond(
                content = grant(capabilities = "[\"mobile_client\",\"device_automation\"]"),
                status = HttpStatusCode.OK,
                headers = jsonHeaders,
            )
        }

        runCatching {
            DeviceEnrollmentClient(HttpClient(engine), "phone-1", "Pixel").exchange(link)
        }.onSuccess { error("legacy Android accepted Apps automation authority") }
        assertEquals(1, calls)
    }

    @Test fun `failed probe keeps the candidate profile from returning`() = runTest {
        var calls = 0
        val engine = MockEngine {
            calls += 1
            if (calls == 1) {
                respond(grant("[\"mobile_client\"]"), HttpStatusCode.OK, jsonHeaders)
            } else {
                respond("forbidden", HttpStatusCode.Forbidden)
            }
        }

        val failure = runCatching {
            DeviceEnrollmentClient(HttpClient(engine), "phone-1", "Pixel").exchange(link)
        }.exceptionOrNull()
        assertEquals(
            "The new Magician connection could not be verified. Your previous connection was kept.",
            failure?.message,
        )
        assertEquals(2, calls)
    }

    @Test fun `Apps retry retains identity after a request may have reached Magician`() {
        assertEquals(
            PendingAppsEnrollmentDisposition.RetainForExactRetry,
            pendingAppsEnrollmentDisposition(
                requestMayHaveReachedServer = true,
                createdPendingThisAttempt = true,
                serverProvedUnspent = false,
            ),
        )
    }

    @Test fun `Apps retry abandons only definitely unsubmitted or explicit Gone work`() {
        assertEquals(
            PendingAppsEnrollmentDisposition.AbandonProvenUnspent,
            pendingAppsEnrollmentDisposition(
                requestMayHaveReachedServer = false,
                createdPendingThisAttempt = true,
                serverProvedUnspent = false,
            ),
        )
        assertEquals(
            PendingAppsEnrollmentDisposition.AbandonProvenUnspent,
            pendingAppsEnrollmentDisposition(
                requestMayHaveReachedServer = true,
                createdPendingThisAttempt = false,
                serverProvedUnspent = true,
            ),
        )
    }

    private fun grant(capabilities: String): String = """
        {
          "token":"device-token",
          "principal":"owner",
          "workspace":"private",
          "public_origin":"https://mobile.example",
          "client_kind":"android",
          "capabilities":$capabilities,
          "cloudflare_access":{"client_id":"outer-id","client_secret":"outer-secret"}
        }
    """.trimIndent()

    private val jsonHeaders = headersOf(HttpHeaders.ContentType, "application/json")
}
