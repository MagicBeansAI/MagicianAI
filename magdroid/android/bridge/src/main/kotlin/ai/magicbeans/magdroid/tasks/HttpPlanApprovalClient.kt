package ai.magicbeans.magdroid.tasks

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/*
 * The production [PlanApprovalClient]: the app's host and headers
 * ([MagicianAccess], the same identity chat and Tasks use), ktor CIO.
 * Android port of iOS `LivePlanApprovalClient`. The request shapes are pure
 * ([PlanApprovalHttp]) so they are tested without a socket.
 */

/** The plan-approval endpoints, as URLs under a host. Pure. */
object PlanApprovalHttp {
    /** `POST {host}/api/magician/v2/hitl/{correlationId}/respond`. */
    fun respondUrl(host: String, correlationId: String): String =
        "${base(host)}/api/magician/v2/hitl/${segment(correlationId)}/respond"

    /** `POST {host}/api/magician/v3/tasks/{taskId}/execute`. */
    fun executeUrl(host: String, taskId: String): String =
        "${base(host)}/api/magician/v3/tasks/${segment(taskId)}/execute"

    /** `POST {host}/api/magician/v3/tasks/{taskId}/plan/{approve|reject}?plan_id=`. */
    fun planDecisionUrl(host: String, taskId: String, approve: Boolean, planId: String): String =
        "${base(host)}/api/magician/v3/tasks/${segment(taskId)}/plan/${if (approve) "approve" else "reject"}" +
            "?plan_id=${segment(planId)}"

    /** A start that did not land, in words: the server's message, else the status. */
    fun startFailure(status: Int, body: JsonObject?): String? {
        if (status in 200..299) return null
        val message = body?.get("message").jText() ?: body?.get("error").jText() ?: body?.get("detail").jText()
        return message ?: "The run did not start (HTTP $status)."
    }

    fun parseBody(text: String): JsonObject? =
        runCatching { Json.parseToJsonElement(text) as? JsonObject }.getOrNull()

    private fun base(host: String): String {
        val trimmed = host.trim().trimEnd('/')
        if (trimmed.isEmpty()) throw IllegalStateException("No Magician host configured yet.")
        return trimmed
    }

    private fun segment(value: String): String =
        URLEncoder.encode(value, StandardCharsets.UTF_8.name()).replace("+", "%20")
}

class HttpPlanApprovalClient(
    private val client: HttpClient,
    private val baseUrl: () -> String,
    private val headers: () -> Map<String, String>,
) : PlanApprovalClient {

    override suspend fun respond(request: PlanApprovalRequest, confirmed: Boolean): PlanApprovalPost =
        try {
            val response = client.post(PlanApprovalHttp.respondUrl(baseUrl(), request.correlationId)) {
                headers().forEach { (name, value) -> header(name, value) }
                contentType(ContentType.Application.Json)
                setBody(PlanApprovalActions.respondPayload(request, confirmed).toString())
            }
            PlanApprovalActions.respondOutcome(response.status.value, PlanApprovalHttp.parseBody(response.bodyAsText()))
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            PlanApprovalPost.Failed(error.message ?: "The answer could not be sent.")
        }

    override suspend fun startRun(taskId: String) {
        val response = client.post(PlanApprovalHttp.executeUrl(baseUrl(), taskId)) {
            headers().forEach { (name, value) -> header(name, value) }
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        val failure = PlanApprovalHttp.startFailure(response.status.value, PlanApprovalHttp.parseBody(response.bodyAsText()))
        if (failure != null) throw IllegalStateException(failure)
    }

    companion object {
        fun forContext(context: Context): HttpPlanApprovalClient {
            val app = context.applicationContext
            return HttpPlanApprovalClient(
                client = HttpClient(CIO) {
                    install(HttpTimeout) {
                        requestTimeoutMillis = 60_000
                        socketTimeoutMillis = 60_000
                        connectTimeoutMillis = 20_000
                    }
                    expectSuccess = false
                },
                baseUrl = { MagicianAccess.baseUrl(app) },
                headers = { MagicianAccess.headers(app) },
            )
        }
    }
}
