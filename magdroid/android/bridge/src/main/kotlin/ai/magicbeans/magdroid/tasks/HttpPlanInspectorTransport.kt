package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.header
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/*
 * The Plan Inspector's production transport: the same host, Cloudflare Access
 * pair and bridge token as Tasks and chat (`MagicianAccess`), so the inspector
 * is never a second login. Request values come from `PlanInspectorClient`;
 * this only turns them into HTTP and the answer back into JSON.
 */
class HttpPlanInspectorTransport(
    private val client: HttpClient,
    private val baseUrl: () -> String,
    private val headers: () -> Map<String, String>,
) : PlanInspectorTransport {

    override suspend fun send(request: PlanInspectorRequest): PlanInspectorResponse {
        val base = baseUrl().trim().trimEnd('/')
        if (base.isEmpty()) throw PlanInspectorException(0, "No Magician host configured yet.")
        val response = client.request(url(base, request)) {
            method = HttpMethod.parse(request.method.uppercase())
            headers().forEach { (name, value) -> header(name, value) }
            header("Accept", "application/json")
            request.body?.let {
                contentType(ContentType.Application.Json)
                setBody(it.toString())
            }
        }
        val status = response.status.value
        return PlanInspectorResponse(status, body(status, response.bodyAsText()))
    }

    companion object {
        /** The app's transport: [MagicianAccess] host and headers, read per request. */
        fun forContext(context: Context): HttpPlanInspectorTransport {
            val app = context.applicationContext
            return HttpPlanInspectorTransport(
                client = HttpClient(CIO) {
                    install(HttpTimeout) {
                        requestTimeoutMillis = 120_000
                        socketTimeoutMillis = 120_000
                        connectTimeoutMillis = 20_000
                    }
                    expectSuccess = false
                },
                baseUrl = { MagicianAccess.baseUrl(app) },
                headers = { MagicianAccess.headers(app) },
            )
        }

        /**
         * The full URL: host + the client's already-encoded path + the query,
         * each name and value percent-encoded as a path segment would be.
         */
        fun url(base: String, request: PlanInspectorRequest): String {
            val query = request.query.joinToString("&") { (name, value) ->
                PlanInspectorClient.encodeSegment(name) + "=" + PlanInspectorClient.encodeSegment(value)
            }
            return base.trimEnd('/') + request.path + if (query.isEmpty()) "" else "?$query"
        }

        /**
         * The answer as JSON: null when empty; a 2xx that is not JSON is null;
         * a failure that is not JSON keeps its text as `message`, so the
         * screen shows what the server said.
         */
        fun body(status: Int, text: String): JsonElement? {
            if (text.isBlank()) return null
            // Strict: kotlinx reads a bare word (`oops`) as a literal.
            val parsed = PlanJson.parse(text)
            if (parsed != null) return parsed
            if (status in 200..299) return null
            return JsonObject(mapOf("message" to JsonPrimitive(text.trim().take(500))))
        }
    }
}
