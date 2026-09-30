package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.forms.formData
import io.ktor.client.request.forms.submitFormWithBinaryData
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.Headers
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.net.URLEncoder

/** A typed failure that is safe to show in the Android voice UI. */
class VoiceMediaError(message: String, val retryable: Boolean = true) : Exception(message)

@Serializable
private data class SynthesisRequest(
    val text: String,
    @SerialName("audio_profile") val audioProfile: String? = null,
    @SerialName("audio_stage_options") val audioStageOptions: Map<String, String> = emptyMap(),
)

@Serializable
private data class TranscriptionResponse(
    val transcript: String? = null,
    val text: String? = null,
)

@Serializable
data class AudioNoteReceipt(
    @SerialName("note_id") val noteId: String,
    val provider: String,
    @SerialName("captured_at") val capturedAt: String,
    @SerialName("note_path") val notePath: String,
    @SerialName("audio_path") val audioPath: String,
    val bytes: Long,
)

/**
 * The native Android adapter for Magician's existing media contracts.
 *
 * It owns no preference or UI state. Scope and authentication always come
 * from [MagicianAccess], so Dictation, spoken replies, the keyboard and future
 * voice surfaces cannot accidentally address different owners.
 */
class BackendVoiceClient(
    context: Context,
    private val client: HttpClient = defaultClient(),
) {
    private val app = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val prefs = VoicePrefs.get(app)

    suspend fun synthesize(text: String): ByteArray {
        val clean = text.trim()
        if (clean.isEmpty()) throw VoiceMediaError("There is no reply to speak.", retryable = false)
        val response = client.post("${base()}/media/tts/synthesize") {
            authorize()
            contentType(ContentType.Application.Json)
            val profile = prefs.audioProfiles.value[NativeAudioSurface.Dictation]
            val tts = prefs.audioStageOptions.value[NativeAudioSurface.Dictation to NativeAudioStage.Tts]
            setBody(json.encodeToString(
                SynthesisRequest.serializer(),
                SynthesisRequest(
                    text = clean,
                    audioProfile = profile,
                    audioStageOptions = tts?.let { mapOf(NativeAudioStage.Tts.wire to it) }.orEmpty(),
                ),
            ))
        }
        if (!response.status.isSuccess()) throw response.failure("Backend voice is unavailable.")
        return response.body<ByteArray>().takeIf(ByteArray::isNotEmpty)
            ?: throw VoiceMediaError("Backend voice returned no audio.")
    }

    suspend fun transcribe(wav: ByteArray, filename: String = "dictation.wav"): String {
        if (wav.size <= WavPcm.HeaderBytes) {
            throw VoiceMediaError("No speech was recorded.", retryable = false)
        }
        val profile = prefs.audioProfiles.value[NativeAudioSurface.Dictation]
        val stage = prefs.audioStageOptions.value[
            NativeAudioSurface.Dictation to NativeAudioStage.RecordingStt
        ]
        val requestUrl = buildString {
            append("${base()}/media/stt/transcribe")
            profile?.let { append("?profile=${encoded(it)}") }
            stage?.let {
                append(if (profile == null) "?" else "&")
                append("stage_option=${encoded("${NativeAudioStage.RecordingStt.wire}:$it")}")
            }
        }
        val response = client.submitFormWithBinaryData(
            url = requestUrl,
            formData = formData {
                append("file", wav, Headers.build {
                    append(HttpHeaders.ContentType, "audio/wav")
                    append(HttpHeaders.ContentDisposition, "filename=\"$filename\"")
                })
            },
        ) { authorize() }
        if (!response.status.isSuccess()) throw response.failure("Backend transcription is unavailable.")
        val decoded = runCatching {
            json.decodeFromString(TranscriptionResponse.serializer(), response.bodyAsText())
        }.getOrElse { throw VoiceMediaError("Backend transcription returned an unreadable response.") }
        return (decoded.transcript ?: decoded.text).orEmpty().trim().takeIf(String::isNotEmpty)
            ?: throw VoiceMediaError("No speech was recognized.", retryable = false)
    }

    suspend fun uploadAudioNote(record: PendingAudioNote, wav: ByteArray): AudioNoteReceipt {
        val response = client.submitFormWithBinaryData(
            url = "${base()}/notes/audio",
            formData = formData {
                append("note_id", record.id)
                append("captured_at", record.capturedAt)
                append("source_surface", record.sourceSurface)
                append("duration_ms", record.durationMs.toString())
                record.transcript?.takeIf(String::isNotBlank)?.let { append("transcript", it) }
                append("file", wav, Headers.build {
                    append(HttpHeaders.ContentType, "audio/wav")
                    append(HttpHeaders.ContentDisposition, "filename=\"dictation-${record.id}.wav\"")
                })
            },
        ) { authorize() }
        if (!response.status.isSuccess()) {
            val retryable = response.status.value == 408 || response.status.value == 425 ||
                response.status.value == 429 || response.status.value >= 500
            throw response.failure("Audio Note could not be saved.", retryable)
        }
        return runCatching {
            json.decodeFromString(AudioNoteReceipt.serializer(), response.bodyAsText())
        }.getOrElse { throw VoiceMediaError("Audio Notes returned an unreadable receipt.") }
            .also { receipt ->
                if (receipt.noteId != record.id || receipt.bytes != wav.size.toLong()) {
                    throw VoiceMediaError("Audio Notes returned a receipt for different recording bytes.")
                }
            }
    }

    fun close() = client.close()

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app)
        if (host.isBlank()) throw VoiceMediaError("No Magician host is configured.", retryable = false)
        return "$host/api/magician/v2"
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun encoded(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

    private suspend fun HttpResponse.failure(
        fallback: String,
        retryable: Boolean = status.value == 408 || status.value == 425 ||
            status.value == 429 || status.value >= 500,
    ): VoiceMediaError {
        val detail = runCatching { bodyAsText() }.getOrDefault("")
        val message = Regex("\"message\"\\s*:\\s*\"([^\"]+)\"")
            .find(detail)?.groupValues?.getOrNull(1)?.trim().orEmpty()
        return VoiceMediaError(message.ifEmpty { "$fallback (HTTP ${status.value})" }, retryable)
    }

    companion object {
        private fun defaultClient() = HttpClient(CIO) {
            install(HttpTimeout) {
                connectTimeoutMillis = 20_000
                requestTimeoutMillis = 180_000
                socketTimeoutMillis = 180_000
            }
        }
    }
}

/** Pure PCM16 mono WAV encoding shared by cloud dictation and tests. */
object WavPcm {
    const val HeaderBytes = 44

    fun encode(pcm16le: ByteArray, sampleRate: Int = 16_000): ByteArray {
        require(sampleRate > 0) { "sampleRate must be positive" }
        require(pcm16le.size % 2 == 0) { "PCM16 byte count must be even" }
        val output = ByteArray(HeaderBytes + pcm16le.size)
        fun ascii(offset: Int, value: String) = value.toByteArray(Charsets.US_ASCII).copyInto(output, offset)
        fun little(offset: Int, value: Int, bytes: Int) {
            repeat(bytes) { index -> output[offset + index] = (value ushr (index * 8)).toByte() }
        }
        ascii(0, "RIFF")
        little(4, 36 + pcm16le.size, 4)
        ascii(8, "WAVE")
        ascii(12, "fmt ")
        little(16, 16, 4)
        little(20, 1, 2)
        little(22, 1, 2)
        little(24, sampleRate, 4)
        little(28, sampleRate * 2, 4)
        little(32, 2, 2)
        little(34, 16, 2)
        ascii(36, "data")
        little(40, pcm16le.size, 4)
        pcm16le.copyInto(output, HeaderBytes)
        return output
    }
}
