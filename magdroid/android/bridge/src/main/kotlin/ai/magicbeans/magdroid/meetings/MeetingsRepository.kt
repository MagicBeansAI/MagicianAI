package ai.magicbeans.magdroid.meetings

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Named once, so every sentence about these reads alike. */
private const val ACTIVE = "active meetings"
private const val CALENDAR = "your calendar"

/**
 * Why a meetings call failed.
 *
 * Reads carry a classified [Failure]; joining and ending stay sentences,
 * because those are refusals rather than transport faults.
 */
class MeetingsError(override val failure: Failure) :
    Exception(failure.headline), CarriesFailure {

    constructor(message: String) : this(
        Failure(FailureKind.Unknown, message, "", retryable = true),
    )
}

/**
 * Meetings Magician is in, and ones it could join.
 *
 * Active and upcoming are separate calls because they are separate endpoints,
 * and deliberately so on the server: a slow calendar CLI must never stall the
 * sessions listing. Keeping them apart here means the same failure stays
 * contained rather than blanking both halves of the screen.
 */
class MeetingsRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            // Upcoming reaches a calendar through a CLI, which is the slow one.
            requestTimeoutMillis = 45_000
            connectTimeoutMillis = 20_000
        }
    }

    suspend fun active(): List<ActiveMeeting> {
        val response = client.get("${base()}/meetings/active") { authorize() }
        if (!response.status.isSuccess()) {
            throw MeetingsError(Failures.ofStatus(response.status.value, ACTIVE))
        }
        return runCatching {
            meetingsJson.decodeFromString(
                ActiveMeetingsResponse.serializer(), response.bodyAsText(),
            ).all()
        }.getOrElse { throw MeetingsError(Failures.garbled(ACTIVE)) }
    }

    /**
     * The transcript so far, from the meeting's own chat session.
     *
     * A poll rather than a stream, matching iOS: the lines land in the session
     * as ordinary messages and there is no transcript socket to subscribe to.
     * Bounded at two hundred, which is what iOS asks for — a long meeting is
     * read from its thread, not scrolled here.
     */
    /**
     * The thread's chat session, resolved once and remembered.
     *
     * Transcript lines do not live under the meeting's own session id — the
     * server's transcript sink posts them into the chat session that BELONGS
     * TO THE THREAD, resolved through the same get-or-create the sink itself
     * uses. This client used to poll `/chat/sessions/{meeting-session}/
     * messages`, which answers 200 with an empty list forever; the live
     * transcript panel had never shown a line. Found on-device in pass 6.
     */
    private val threadSessions = java.util.concurrent.ConcurrentHashMap<String, String>()

    suspend fun transcript(threadId: String): List<TranscriptLine> {
        val chatSession = threadSessions[threadId]
            ?: resolveThreadSession(threadId)?.also { threadSessions[threadId] = it }
            ?: return emptyList()
        val response = client.get("${base()}/chat/sessions/$chatSession/messages") {
            authorize()
            parameter("limit", TRANSCRIPT_LIMIT)
        }
        if (!response.status.isSuccess()) return emptyList()
        return parseTranscriptLines(response.bodyAsText())
    }

    private suspend fun resolveThreadSession(threadId: String): String? = runCatching {
        val response = client.get("${base()}/chat/active") {
            authorize()
            parameter("ui_thread_id", threadId)
            // The automated lane, exactly as the server's own transcript sink
            // resolves it — the personal lane would answer with the owner's
            // current conversation instead.
            parameter("history_lane", "automated")
        }
        if (!response.status.isSuccess()) return null
        val root = meetingsJson.parseToJsonElement(response.bodyAsText())
        ((root as? kotlinx.serialization.json.JsonObject)
            ?.get("session") as? kotlinx.serialization.json.JsonObject)
            ?.get("id")?.let { id ->
                (id as? kotlinx.serialization.json.JsonPrimitive)?.content
            }
            ?.takeIf { it.isNotBlank() }
    }.getOrNull()

    /** `refresh` busts the server's cache, for the manual refresh button. */
    suspend fun upcoming(refresh: Boolean = false): UpcomingMeetingsResponse {
        val response = client.get("${base()}/meetings/upcoming") {
            authorize()
            if (refresh) parameter("refresh", true)
        }
        if (!response.status.isSuccess()) {
            throw MeetingsError(Failures.ofStatus(response.status.value, CALENDAR))
        }
        return runCatching {
            meetingsJson.decodeFromString(
                UpcomingMeetingsResponse.serializer(), response.bodyAsText(),
            )
        }.getOrElse { throw MeetingsError(Failures.garbled(CALENDAR)) }
    }

    /** Send the attendee bot into a call. */
    suspend fun join(url: String, title: String? = null) {
        val response = client.post("${base()}/meetings/join") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(
                buildJsonObject {
                    put("url", url.trim())
                    title?.trim()?.takeIf { it.isNotEmpty() }?.let { put("title", it) }
                }.toString(),
            )
        }
        if (!response.status.isSuccess()) {
            throw MeetingsError("Magician could not join that meeting.")
        }
    }

    suspend fun stop(sessionId: String) {
        val response = client.post("${base()}/meetings/$sessionId/stop") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody("{}")
        }
        if (!response.status.isSuccess()) throw MeetingsError("Could not end that meeting.")
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw MeetingsError("No Magician host configured yet.")
        return "$host/api/magician/v2"
    }

    fun close() = client.close()

    private companion object {
        /** What iOS asks for. A longer meeting is read from its thread. */
        const val TRANSCRIPT_LIMIT = 200
    }
}
