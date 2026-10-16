package ai.magicbeans.magdroid.observe

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The room, on its way to Magician.
 *
 * A session is opened once, PCM goes up in numbered chunks against an upload
 * token, and the session is closed when capture stops. The same three calls iOS
 * makes, with the same paths and headers — an observation started on one client
 * and continued on another has to be the same session, not two.
 */
class ObservationUplink(context: Context) {

    private val appContext = context.applicationContext

    // encodeDefaults matters here the way it does for chatRequestJson: the
    // start request is ALL defaults (capture:"client", mic:true), and kotlinx
    // omits default-valued fields unless told otherwise. The omission sent
    // `{}`, the server opened a HOST capture instead, answered without an
    // upload token, and the client read that as "would not open" — while a
    // host observation leaked server-side on every attempt.
    private val json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = true
    }

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            // Generous: a chunk lost to a short timeout is a gap in the
            // recording, and the recording is the whole point.
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 15_000
        }
    }

    private fun base() = "${MagicianAccess.baseUrl(appContext)}/api/magician/v2"

    /**
     * Open a capture.
     *
     * `capture: "client"` says the audio comes from this device rather than
     * being pulled by the backend. A live session on the same thread is
     * returned rather than duplicated, which is what `reused` reports.
     */
    suspend fun start(title: String?, url: String? = null): ObservationSession? {
        val response = client.post("${base()}/meetings/listen") {
            scoped()
            contentType(ContentType.Application.Json)
            setBody(
                json.encodeToString(
                    StartRequest.serializer(),
                    StartRequest(
                        mic = true,
                        title = title?.takeIf { it.isNotBlank() },
                        url = url?.takeIf { it.isNotBlank() },
                    ),
                ),
            )
        }
        if (!response.status.isSuccess()) return null
        return runCatching {
            json.decodeFromString(ObservationSession.serializer(), response.bodyAsText())
        }.getOrNull()?.takeIf { it.sessionId.isNotBlank() && it.uploadToken.isNotBlank() }
    }

    /**
     * Send one chunk.
     *
     * Returns whether it landed. The caller decides what a failure means —
     * dropping a chunk is survivable, and stopping the whole capture because
     * one upload failed would lose far more than the gap.
     */
    suspend fun sendChunk(session: ObservationSession, seq: Long, pcm: ByteArray): Upload =
        runCatching {
            val response = client.post("${base()}/meetings/${session.sessionId}/audio") {
                scoped()
                parameter("channel", "mic")
                parameter("seq", seq)
                header("X-Upload-Token", session.uploadToken)
                contentType(ContentType("audio", "pcm"))
                setBody(pcm)
            }
            Upload.of(response.status.value)
            // A dropped connection is transient by definition: the server never
            // answered, so it never said the session was gone.
        }.getOrDefault(Upload.Transient)

    /** Close the capture. Best effort: the session also times out on its own. */
    /**
     * Open a screen observation narrating into the same meeting thread.
     *
     * Paired with the audio session rather than replacing it: the point is that
     * what was on screen and what was said land in one thread, so a meeting can
     * be read back as one thing.
     *
     * Best effort by design. If a host observation already holds the slot this
     * fails and the audio session carries on alone — a meeting that records
     * sound and not pictures is worth far more than one that refuses to start.
     */
    suspend fun startScreen(threadId: String, title: String?): ScreenObservation? =
        runCatching {
            val response = client.post("${base()}/screen/observe/client/start") {
                scoped()
                contentType(ContentType.Application.Json)
                setBody(
                    buildJsonObject {
                        put("thread", threadId)
                        title?.takeIf { it.isNotBlank() }?.let { put("title", it) }
                    }.toString(),
                )
            }
            if (!response.status.isSuccess()) null
            else json.decodeFromString(ScreenObservation.serializer(), response.bodyAsText())
        }.getOrNull()

    /**
     * Push one frame.
     *
     * A keyframe, not a stream: the server ingests stills and narrates from
     * them, and pushing video would be a different contract as well as a much
     * larger one to carry off a phone.
     */
    suspend fun sendFrame(observation: ScreenObservation, jpeg: ByteArray): Upload =
        runCatching {
            val response = client.post("${base()}/screen/observe/frame") {
                scoped()
                parameter("observe_id", observation.observeId)
                header("X-Upload-Token", observation.uploadToken)
                contentType(ContentType.Image.JPEG)
                setBody(jpeg)
            }
            Upload.of(response.status.value)
        }.getOrDefault(Upload.Transient)

    suspend fun stop(sessionId: String) {
        runCatching { client.post("${base()}/meetings/$sessionId/stop") { scoped() } }
    }

    /** Scope and Access, from the one place that knows how to build them. */
    private fun io.ktor.client.request.HttpRequestBuilder.scoped() {
        MagicianAccess.headers(appContext).forEach { (name, value) -> header(name, value) }
    }
}

@Serializable
internal data class StartRequest(
    val capture: String = "client",
    val mic: Boolean = true,
    val title: String? = null,
    val url: String? = null,
)

/**
 * What the server said about one upload.
 *
 * The distinction is load-bearing and was missing: a 410 is how a stop made
 * anywhere else reaches this device — iOS documents it as the cascade — and
 * collapsing it into "failed" meant the microphone carried on recording and
 * uploading into a session that had already ended.
 */
enum class Upload {
    Landed,

    /** Offline, a timeout, a 5xx. Keep going; the next one may land. */
    Transient,

    /** HTTP 410: the session is gone. Terminal — stop capturing. */
    Ended,
    ;

    companion object {
        fun of(status: Int): Upload = when {
            status == 410 -> Ended
            status in 200..299 -> Landed
            else -> Transient
        }
    }
}

/** A screen observation opened alongside an audio session. */
@Serializable
data class ScreenObservation(
    @SerialName("observe_id") val observeId: String = "",
    @SerialName("upload_token") val uploadToken: String = "",
)

@Serializable
data class ObservationSession(
    @SerialName("session_id") val sessionId: String = "",
    /** Echoed on every chunk. */
    @SerialName("upload_token") val uploadToken: String = "",
    @SerialName("thread_id") val threadId: String = "",
    /** True when a live session on this thread was handed back rather than a new one. */
    val reused: Boolean = false,
)
