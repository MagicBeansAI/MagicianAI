package ai.magicbeans.magdroid.preferences

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

/**
 * The account's UI preferences, as `/api/magician/v2/ui/preferences` stores
 * them: the resolved theme variant (`sky-dark`, …) and, when a mobile surface
 * wrote it, the appearance mode. `saved == false` means the account holds no
 * preference yet, so a surface keeps its own choice (and may seed the account
 * with it, exactly as the web does).
 */
data class UiThemePreferences(
    val theme: String,
    val appearance: String?,
    val saved: Boolean,
)

class UiPreferencesError(message: String) : Exception(message)

/**
 * `GET`/`PUT /api/magician/v2/ui/preferences` — the same account-scoped theme
 * the web reads and writes, so a theme picked on any surface follows the
 * account. Pure transport; the app module's [ai.magicbeans.magdroid.ui.ThemeSync]
 * owns when to read and write.
 */
class UiPreferencesClient(context: Context) {
    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 15_000
            connectTimeoutMillis = 10_000
        }
    }

    suspend fun fetch(): UiThemePreferences {
        val response = client.get(url()) { authorized() }
        val body = response.bodyAsText()
        if (!response.status.isSuccess()) {
            throw UiPreferencesError("ui preferences fetch ${response.status.value}")
        }
        return parse(body)
    }

    suspend fun push(theme: String, appearance: String?) {
        val response = client.put(url()) {
            authorized()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("theme", theme)
                    appearance?.let { put("appearance", it) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) {
            throw UiPreferencesError("ui preferences save ${response.status.value}")
        }
    }

    private fun parse(body: String): UiThemePreferences {
        val root = runCatching { Json.parseToJsonElement(body).jsonObject }.getOrNull()
            ?: throw UiPreferencesError("unreadable ui preferences")
        return UiThemePreferences(
            theme = (root["theme"] as? JsonPrimitive)?.takeIf { it.isString }?.content ?: "sorbet",
            appearance = (root["appearance"] as? JsonPrimitive)?.takeIf { it.isString }?.content,
            saved = (root["saved"] as? JsonPrimitive)?.booleanOrNull == true,
        )
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorized() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun url(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw UiPreferencesError("No Magician host configured yet.")
        return "$host/api/magician/v2/ui/preferences"
    }

    fun close() = client.close()
}
