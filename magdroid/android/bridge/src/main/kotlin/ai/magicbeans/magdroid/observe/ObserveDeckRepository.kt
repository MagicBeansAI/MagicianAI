package ai.magicbeans.magdroid.observe

import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.net.CarriesFailure
import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.Failures
import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import kotlinx.serialization.KSerializer

/** A deck read that failed, carrying a classified [Failure] for the block. */
class ObserveDeckError(override val failure: Failure) : Exception(failure.headline), CarriesFailure {
    constructor(message: String) : this(Failure(FailureKind.Unknown, message, "", retryable = true))
}

/**
 * The read-mostly lanes of the Observe deck: recent captures, the web & account
 * sources (view-only here), and the meeting/listening audio profiles.
 *
 * Each call is its own request and fails on its own, so one slow or refused
 * lane never blanks the others — the same separation the meetings repository
 * keeps between active and upcoming.
 */
class ObserveDeckRepository(context: Context) {

    private val app = context.applicationContext

    private val client = HttpClient(CIO) {
        install(HttpTimeout) {
            requestTimeoutMillis = 30_000
            connectTimeoutMillis = 20_000
        }
    }

    suspend fun recent(): List<RecentMeeting> = parseRecentMeetings(text("/meetings", "recent captures"))

    suspend fun channels(): List<ChannelAssistChannel> =
        decode(text("/channel-assist/channels", "mail & chat channels"), ChannelAssistChannelsResponse.serializer(), "mail & chat channels")
            .channels

    suspend fun calendarStatus(): CalendarObserveStatus =
        decode(text("/observe/calendar/status", "calendar observation"), CalendarObserveStatus.serializer(), "calendar observation")

    suspend fun enabledSubscriptions(limit: Int = 20): ObservationSubscriptionPage = decode(
        text("/observe/subscriptions", "continuous sources") {
            parameter("enabled", true)
            parameter("limit", limit)
        },
        ObservationSubscriptionPage.serializer(),
        "continuous sources",
    )

    suspend fun ambientStatus(): AmbientStatus =
        decode(text("/ambient/status", "browser tabs"), AmbientStatus.serializer(), "browser tabs")

    suspend fun catchUp(): CatchUpStatus =
        decode(text("/observe/catch-up", "startup catch-up"), CatchUpEnvelope.serializer(), "startup catch-up").status

    /** The account's chosen profile per surface. */
    suspend fun surfaceProfiles(): Map<String, String> =
        parseSurfaceProfiles(text("/media/preferences", "audio preferences"))
            ?: throw ObserveDeckError(Failures.garbled("audio preferences"))

    /** Profiles, stages and configured defaults. */
    suspend fun audioCatalog(): RealtimeVoiceCatalog =
        decode(text("/media/providers", "audio profiles"), RealtimeVoiceCatalog.serializer(), "audio profiles")

    /** Save one surface's profile; returns the server's saved profile map. */
    suspend fun saveSurfaceProfile(surface: String, profileId: String?): Map<String, String> {
        val response = client.put("${base()}/media/preferences") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(surfaceProfilePatch(surface, profileId))
        }
        if (!response.status.isSuccess()) {
            throw ObserveDeckError(Failures.ofStatus(response.status.value, "audio preferences"))
        }
        return parseSurfaceProfiles(response.bodyAsText())
            ?: throw ObserveDeckError(Failures.garbled("audio preferences"))
    }

    private suspend fun text(
        path: String,
        doing: String,
        block: HttpRequestBuilder.() -> Unit = {},
    ): String {
        val response = client.get("${base()}$path") {
            authorize()
            block()
        }
        if (!response.status.isSuccess()) {
            throw ObserveDeckError(Failures.ofStatus(response.status.value, doing))
        }
        return response.bodyAsText()
    }

    private fun <T> decode(raw: String, serializer: KSerializer<T>, doing: String): T =
        runCatching { deckJson.decodeFromString(serializer, raw) }
            .getOrElse { throw ObserveDeckError(Failures.garbled(doing)) }

    private fun HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app).trimEnd('/')
        if (host.isEmpty()) throw ObserveDeckError("No Magician host configured yet.")
        return "$host/api/magician/v2"
    }

    fun close() = client.close()
}
