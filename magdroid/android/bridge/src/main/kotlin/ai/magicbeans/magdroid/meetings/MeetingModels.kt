package ai.magicbeans.magdroid.meetings

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

val meetingsJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
}

/**
 * A meeting Magician is in right now.
 *
 * Two modes the server distinguishes and this client must not flatten:
 * `passive` is the phone listening to a room, `attendee` is a bot that joined a
 * call by URL. Ending one is a different act from ending the other, and showing
 * them identically would make "stop" ambiguous.
 */
@Serializable
data class ActiveMeeting(
    @SerialName("session_id") val sessionId: String = "",
    val mode: String = "",
    val status: String = "",
    @SerialName("thread_id") val threadId: String? = null,
    val title: String? = null,
    val url: String? = null,
    val mic: Boolean? = null,
    val paused: Boolean = false,
    @SerialName("latest_summary") val latestSummary: String? = null,
) {
    val isAttendee: Boolean get() = mode == "attendee"
    val displayTitle: String
        get() = title?.takeIf { it.isNotBlank() }
            ?: url?.takeIf { it.isNotBlank() }
            ?: if (isAttendee) "Meeting" else "This room"
}

@Serializable
data class ActiveMeetingsResponse(
    // `active` is what the endpoint sends TODAY (verified against the live
    // response on-device in pass 6); the other two are the names it has used
    // before. Reading only the old pair decoded silently to empty — defaults
    // plus ignoreUnknownKeys swallow a renamed envelope without a sound — so
    // the "Now" section had never once rendered a live meeting.
    val active: List<ActiveMeeting> = emptyList(),
    val meetings: List<ActiveMeeting> = emptyList(),
    val sessions: List<ActiveMeeting> = emptyList(),
) {
    /** The server has used all three names; whichever is filled is the list. */
    fun all(): List<ActiveMeeting> = active.ifEmpty { meetings.ifEmpty { sessions } }
}

/**
 * A calendar meeting that has not started yet.
 *
 * Its own endpoint on purpose — a slow calendar CLI must never stall the
 * sessions listing, and the same separation is kept here so one failing does
 * not blank the other.
 */
@Serializable
data class UpcomingMeeting(
    @SerialName("event_id") val eventId: String? = null,
    val title: String = "",
    val start: String? = null,
    val end: String? = null,
    @SerialName("meet_url") val meetUrl: String? = null,
    @SerialName("live_now") val liveNow: Boolean = false,
    val account: String? = null,
) {
    val id: String get() = eventId ?: "$title|${start.orEmpty()}"

    /** Only something with a link can be joined. */
    val isJoinable: Boolean get() = !meetUrl.isNullOrBlank()
}

@Serializable
data class UpcomingMeetingsResponse(
    /** Current server envelope. */
    val events: List<UpcomingMeeting> = emptyList(),
    val errors: List<UpcomingMeetingError> = emptyList(),
    /** Historical aliases retained for old servers. */
    val meetings: List<UpcomingMeeting> = emptyList(),
    val upcoming: List<UpcomingMeeting> = emptyList(),
    /** Historical aggregate error. */
    val error: String? = null,
) {
    fun all(): List<UpcomingMeeting> = events.ifEmpty { meetings.ifEmpty { upcoming } }

    fun errorSummary(): String? = when {
        errors.isNotEmpty() -> "Some calendars couldn't be read — check your Google sign-in."
        !error.isNullOrBlank() -> error
        else -> null
    }
}

@Serializable
data class UpcomingMeetingError(
    val account: String? = null,
    val error: String = "",
)

/**
 * One line of a meeting transcript.
 *
 * [speaker] is null when the line arrived without an attributable prefix, which
 * is a different thing from an empty name and is why it is not defaulted.
 */
data class TranscriptLine(val id: String, val speaker: String?, val text: String)

/**
 * Transcript lines out of a chat session's messages.
 *
 * The meeting's transcript lands in its chat session rather than in a
 * transcript resource, so this reads the session and keeps only what the
 * capture wrote — `source_surface == "meeting-transcript"`. Without that filter
 * an assistant reply in the same session would be read back as something
 * somebody said in the room.
 *
 * A leading `"Name: "` is split off as the speaker, guarded to the first forty
 * characters so a colon deep in a sentence does not turn half a sentence into a
 * name. Ported from `MeetingsAPI.swift`, that guard included.
 */
fun parseTranscriptLines(rawMessagesJson: String): List<TranscriptLine> {
    val root = runCatching {
        meetingsJson.parseToJsonElement(rawMessagesJson) as? kotlinx.serialization.json.JsonObject
    }.getOrNull() ?: return emptyList()
    val messages = root["messages"] as? kotlinx.serialization.json.JsonArray ?: return emptyList()

    return messages.mapNotNull { element ->
        val message = element as? kotlinx.serialization.json.JsonObject ?: return@mapNotNull null
        fun str(key: String) = (message[key] as? kotlinx.serialization.json.JsonPrimitive)
            ?.takeIf { it.isString }?.content
        if (str("source_surface") != "meeting-transcript") return@mapNotNull null
        // `content` is the chat message object `{type, text}` on the live wire
        // (verified on-device in pass 6); reading it as a bare string dropped
        // every line. The string branch stays for any older shape.
        val body = when (val content = message["content"]) {
            is kotlinx.serialization.json.JsonObject ->
                (content["text"] as? kotlinx.serialization.json.JsonPrimitive)
                    ?.takeIf { it.isString }?.content
            is kotlinx.serialization.json.JsonPrimitive ->
                content.takeIf { it.isString }?.content
            else -> null
        }?.trim().orEmpty()
        if (body.isEmpty()) return@mapNotNull null
        // The body doubles as the identity when the message carried none, so a
        // repeated poll still de-duplicates by content rather than by position.
        val id = str("id")?.takeIf { it.isNotBlank() } ?: body

        val separator = body.indexOf(": ")
        if (separator in 0..MAX_SPEAKER_PREFIX) {
            TranscriptLine(id, body.substring(0, separator), body.substring(separator + 2))
        } else {
            TranscriptLine(id, null, body)
        }
    }
}

/** How far into a line a `": "` may sit and still be a speaker's name. */
private const val MAX_SPEAKER_PREFIX = 40
