package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.statement.bodyAsText
import io.ktor.http.isSuccess
import java.net.URLEncoder
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class AudioNoteItem(
    @SerialName("note_id") val id: String,
    val provider: String,
    @SerialName("used_fallback") val usedFallback: Boolean = false,
    @SerialName("captured_at") val capturedAt: String,
    @SerialName("source_surface") val sourceSurface: String,
    val transcript: String? = null,
    @SerialName("duration_ms") val durationMs: Long? = null,
    @SerialName("mime_type") val mimeType: String,
    @SerialName("note_path") val notePath: String,
    @SerialName("audio_path") val audioPath: String,
    val bytes: Long,
)

@Serializable
data class AudioNotePage(
    val items: List<AudioNoteItem> = emptyList(),
    val offset: Int = 0,
    val limit: Int = 20,
    val total: Int = 0,
    @SerialName("has_more") val hasMore: Boolean = false,
)

/**
 * Scoped access to the same Audio Notes collection used by iOS.
 *
 * Search and paging stay server-authoritative. Recording bytes are fetched
 * only when Play is pressed and never cached beyond the app cache directory.
 */
class AudioNotesRepository private constructor(
    context: Context,
    private val client: HttpClient,
) {
    constructor(context: Context) : this(context, defaultClient())
    private val app = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }

    suspend fun list(offset: Int, limit: Int = 20, query: String = ""): AudioNotePage {
        val response = client.get("${base()}/notes/audio") {
            authorize()
            parameter("offset", offset.coerceAtLeast(0))
            parameter("limit", limit.coerceIn(1, 100))
            query.trim().takeIf(String::isNotEmpty)?.let { parameter("q", it) }
        }
        if (!response.status.isSuccess()) throw response.failure("Audio Notes could not be loaded.")
        return runCatching {
            json.decodeFromString(AudioNotePage.serializer(), response.bodyAsText())
        }.getOrElse { throw VoiceMediaError("Audio Notes returned an unreadable list.") }
    }

    suspend fun recording(noteId: String): ByteArray {
        val response = client.get("${base()}/notes/audio/${encoded(noteId)}/recording") { authorize() }
        if (!response.status.isSuccess()) throw response.failure("The recording could not be loaded.")
        return response.body<ByteArray>().takeIf(ByteArray::isNotEmpty)
            ?: throw VoiceMediaError("The saved recording is empty.", retryable = false)
    }

    suspend fun delete(noteId: String) {
        val response = client.delete("${base()}/notes/audio/${encoded(noteId)}") { authorize() }
        if (response.status.value != 204 && response.status.value != 404) {
            throw response.failure("The Audio Note could not be deleted.")
        }
    }

    fun close() = client.close()

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app)
        if (host.isBlank()) {
            throw VoiceMediaError("No Magician host is configured. Open Settings to connect this phone.", false)
        }
        return "$host/api/magician/v2"
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private suspend fun io.ktor.client.statement.HttpResponse.failure(fallback: String): VoiceMediaError {
        val detail = runCatching { bodyAsText() }.getOrDefault("")
        val server = Regex("\"(?:message|error)\"\\s*:\\s*\"([^\"]+)\"")
            .find(detail)?.groupValues?.getOrNull(1)?.trim().orEmpty()
        val message = when (status.value) {
            401, 403 -> "Magician refused access. Check the connection credentials in Settings."
            404 -> "That Audio Note no longer exists."
            else -> server.ifEmpty { "$fallback (HTTP ${status.value})" }
        }
        return VoiceMediaError(message, status.value == 408 || status.value == 425 ||
            status.value == 429 || status.value >= 500)
    }

    private fun encoded(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

    companion object {
        private fun defaultClient() = HttpClient(CIO) {
            install(HttpTimeout) {
                connectTimeoutMillis = 20_000
                requestTimeoutMillis = 120_000
                socketTimeoutMillis = 120_000
            }
        }
    }
}
