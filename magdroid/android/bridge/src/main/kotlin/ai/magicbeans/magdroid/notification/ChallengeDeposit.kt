package ai.magicbeans.magdroid.notification

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
import io.ktor.http.isSuccess
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * The trusted device-to-custody handoff (secure HITL plan §6.2, P6).
 *
 * A code this phone read from a notification answers the pending ask the
 * same way the owner would from the Attention sheet: `POST
 * /hitl/{correlation}/respond` over the device's own paired credential. The
 * code therefore never rides an MCP tool result, a replay cache or a model's
 * context; what the bridge returns to the runtime is status alone. First
 * response wins on the server — a person who typed the code a moment earlier
 * simply makes this a no-op.
 */
class ChallengeDeposit(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 20_000
            connectTimeoutMillis = 10_000
        }
    }

    sealed class Outcome {
        object Deposited : Outcome()
        object AlreadyResolved : Outcome()
        data class Refused(val reason: String) : Outcome()
    }

    suspend fun answer(correlationId: String, source: String, code: String): Outcome {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) return Outcome.Refused("no Magician host configured")
        val response = try {
            client.post("$host/api/magician/v2/hitl/$correlationId/respond") {
                MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
                contentType(ContentType.Application.Json)
                setBody(
                    buildJsonObject {
                        put("source", source)
                        putJsonObject("value") {
                            put("type", "password")
                            put("value", code)
                        }
                        put("input_type", "otp")
                        put("channel", CHANNEL)
                    }.toString(),
                )
            }
        } catch (error: Exception) {
            return Outcome.Refused("the answer could not be sent")
        }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) {
            return if (response.status.value == 404 || response.status.value == 409 || response.status.value == 410) {
                Outcome.AlreadyResolved
            } else {
                Outcome.Refused("Magician answered HTTP ${response.status.value}")
            }
        }
        // `accepted:false` arrives with 200 and a reason — an ask already
        // answered, or one belonging to another scope.
        val accepted = runCatching { Json.parseToJsonElement(body).jsonObject }.getOrNull()
        val ok = accepted?.get("accepted")?.jsonPrimitive?.content?.equals("true", ignoreCase = true) ?: true
        if (ok) return Outcome.Deposited
        val reason = accepted?.get("reason")?.jsonPrimitive?.content ?: "refused"
        return if (reason.contains("already", ignoreCase = true)) Outcome.AlreadyResolved else Outcome.Refused(reason)
    }

    fun close() = client.close()

    companion object {
        /** The channel the runtime records the answer under. */
        const val CHANNEL = "android_notification"
    }
}
