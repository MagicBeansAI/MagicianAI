package ai.magicbeans.magdroid.observe

import ai.magicbeans.magdroid.voice.AudioSurfaceProfile
import ai.magicbeans.magdroid.voice.NativeAudioStage
import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import ai.magicbeans.magdroid.voice.audioProfileLabel
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * Wire shapes behind the Observe Command Deck, decoded leniently.
 *
 * Every one of these mirrors what the web deck reads
 * (`ui/unified-ui/src/lib/observe/`, `channelAssistStore.ts`,
 * `observeConnectorStore.ts`, `media/preferences.ts`). Unknown fields are
 * ignored and every field defaults, because a block on a phone that fails to
 * decode because the server grew a field is a block that silently goes blank.
 */
val deckJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    coerceInputValues = true
    isLenient = true
}

// ── Recent captures ────────────────────────────────────────────────────────

/** One row of `GET /meetings` → `recent`. */
@Serializable
data class RecentMeeting(
    @SerialName("thread_id") val threadId: String = "",
    @SerialName("session_id") val sessionId: String = "",
    val title: String? = null,
    @SerialName("agent_id") val agentId: String? = null,
    /** Chat-session `updated_at`: epoch millis today; seconds tolerated. */
    @SerialName("updated_at") val updatedAt: Long = 0,
    val mode: String? = null,
) {
    val displayTitle: String get() = title?.takeIf(String::isNotBlank) ?: threadId

    /** Normalised to millis so an older seconds-based row still sorts right. */
    val updatedAtMillis: Long
        get() = if (updatedAt in 1 until RECENT_SECONDS_CUTOFF) updatedAt * 1000 else updatedAt
}

/** Anything below this is seconds (it is the year 5138 in seconds). */
private const val RECENT_SECONDS_CUTOFF = 100_000_000_000L

/** The web's Recent list is capped at thirty, newest first. */
const val RECENT_CAPTURES_MAX = 30

/**
 * `recent` out of `GET /meetings`, newest first, at most [max].
 *
 * Decoded per row: one malformed row drops that row, not the list.
 */
fun parseRecentMeetings(raw: String, max: Int = RECENT_CAPTURES_MAX): List<RecentMeeting> {
    val root = runCatching { deckJson.parseToJsonElement(raw) as? JsonObject }.getOrNull()
        ?: return emptyList()
    val rows = root["recent"] as? JsonArray ?: return emptyList()
    return rows.mapNotNull { element ->
        runCatching { deckJson.decodeFromJsonElement(RecentMeeting.serializer(), element) }.getOrNull()
    }
        .filter { it.threadId.isNotBlank() }
        .sortedByDescending { it.updatedAtMillis }
        .take(max)
}

// ── Web & account sources (view-only on the phone) ────────────────────────

/** One Mail & chat account from `GET /channel-assist/channels`. */
@Serializable
data class ChannelAssistChannel(
    val provider: String = "",
    @SerialName("provider_display") val providerDisplay: String? = null,
    @SerialName("account_alias") val accountAlias: String = "",
    val display: String = "",
    val lane: String = "",
    val connected: Boolean = false,
    val enabled: Boolean = false,
    @SerialName("thread_count") val threadCount: Int = 0,
    @SerialName("message_count") val messageCount: Int = 0,
    val purposes: List<String> = emptyList(),
) {
    val providerLabel: String
        get() = providerDisplay?.takeIf(String::isNotBlank)
            ?: provider.replace('_', ' ').replaceFirstChar(Char::uppercase)
    val accountLabel: String get() = display.ifBlank { accountAlias }
    val hasVerificationCodes: Boolean get() = "verification_codes" in purposes
}

@Serializable
data class ChannelAssistChannelsResponse(
    val channels: List<ChannelAssistChannel> = emptyList(),
)

/** `GET /observe/calendar/status`. */
@Serializable
data class CalendarObserveStatus(
    val enabled: Boolean = false,
    val accounts: List<String> = emptyList(),
    val frequency: String? = null,
    val time: String? = null,
    @SerialName("total_synced") val totalSynced: Long = 0,
    @SerialName("last_sync_at") val lastSyncAt: String? = null,
) {
    /** "Daily at 07:00", "Hourly", … */
    val scheduleLabel: String
        get() {
            val freq = frequency?.takeIf(String::isNotBlank)
                ?.replace('-', ' ')?.replace('_', ' ')
                ?.replaceFirstChar(Char::uppercase)
                ?: "Daily"
            val at = time?.takeIf(String::isNotBlank)
            return if (at != null && !freq.equals("Hourly", ignoreCase = true)) "$freq at $at" else freq
        }
}

/** One continuous source from `GET /observe/subscriptions`. */
@Serializable
data class ObservationSubscription(
    @SerialName("subscription_id") val id: String = "",
    @SerialName("source_id") val sourceId: String = "",
    @SerialName("display_name") val displayName: String = "",
    val category: String = "",
    val enabled: Boolean = true,
    val cadence: String = "",
    @SerialName("next_run_at_ms") val nextRunAtMs: Long? = null,
    @SerialName("last_success_at_ms") val lastSuccessAtMs: Long? = null,
    @SerialName("last_run_started_at_ms") val lastRunStartedAtMs: Long? = null,
    @SerialName("consecutive_failures") val consecutiveFailures: Int = 0,
) {
    /** The web's own three words for the same state. */
    val stateLabel: String
        get() = when {
            !enabled -> "Paused"
            consecutiveFailures > 0 -> "Retrying"
            else -> "Listening"
        }
    val providerLabel: String get() = category.ifBlank { sourceId }
}

@Serializable
data class ObservationSubscriptionPage(
    val items: List<ObservationSubscription> = emptyList(),
    val total: Int = 0,
)

/** `GET /ambient/status` — the browser-tabs collector. */
@Serializable
data class AmbientStatus(
    val enabled: Boolean = false,
    val paired: Boolean = false,
    @SerialName("total_signals") val totalSignals: Long = 0,
    @SerialName("accepted_today") val acceptedToday: Long? = null,
    @SerialName("pages_today") val pagesToday: Long? = null,
    @SerialName("last_signal_at") val lastSignalAt: String? = null,
)

@Serializable
data class CatchUpStatus(
    val phase: String = "",
    @SerialName("admitted_items") val admittedItems: Int = 0,
    @SerialName("processed_items") val processedItems: Int = 0,
    @SerialName("remaining_items") val remainingItems: Int = 0,
)

/** `GET /observe/catch-up` wraps the status in an envelope. */
@Serializable
data class CatchUpEnvelope(val status: CatchUpStatus = CatchUpStatus())

/** "Active · 12 of 40 items processed". */
fun catchUpSummary(status: CatchUpStatus): String {
    val phase = when (status.phase.lowercase()) {
        "waiting" -> "Waiting for startup"
        "active" -> "Catching up"
        "completed" -> "Caught up"
        "disabled" -> "Off"
        "expired" -> "Window expired"
        "" -> "Unknown"
        else -> status.phase.replaceFirstChar(Char::uppercase)
    }
    return if (status.admittedItems > 0) {
        "$phase · ${status.processedItems} of ${status.admittedItems} items processed"
    } else {
        phase
    }
}

/** One line for the browser-tabs block. */
fun ambientSummary(status: AmbientStatus): String = buildList {
    add(if (status.enabled) "On" else "Off")
    if (!status.paired) add("no browser paired")
    status.acceptedToday?.let { add("$it signals today") } ?: add("${status.totalSignals} signals")
    status.lastSignalAt?.takeIf(String::isNotBlank)?.let { add("last capture $it") }
}.joinToString(" · ")

/**
 * The Sources-on KPI, counted exactly as the web counts it: channels (any one
 * enabled counts once) + calendar + browser tabs + every enabled continuous
 * subscription. An unknown lane contributes nothing rather than guessing.
 */
fun enabledSourcesCount(
    channels: List<ChannelAssistChannel>?,
    calendarEnabled: Boolean?,
    ambientEnabled: Boolean?,
    enabledSubscriptions: Int?,
): Int = listOf(
    channels?.any { it.enabled } == true,
    calendarEnabled == true,
    ambientEnabled == true,
).count { it } + (enabledSubscriptions ?: 0).coerceAtLeast(0)

// ── Audio profiles (meeting + listening STT) ──────────────────────────────

/** The two transcription surfaces the Observe deck configures. */
enum class ObserveAudioSurface(val wire: String, val label: String, val hint: String) {
    Meeting("meeting", "Meeting", "Calls the agent attends or a meeting you listen to"),
    Listening("listening", "Listening", "Rooms captured by a phone or the Mac microphone"),
}

/**
 * `surface_profiles` out of `GET /media/preferences`, trimmed, blanks dropped.
 * Null when the body cannot be read at all.
 */
fun parseSurfaceProfiles(raw: String): Map<String, String>? {
    val root = runCatching { deckJson.parseToJsonElement(raw) as? JsonObject }.getOrNull() ?: return null
    val profiles = root["surface_profiles"] as? JsonObject ?: return emptyMap()
    return profiles.mapNotNull { (surface, value) ->
        (value as? JsonPrimitive)?.takeIf { it.isString }?.content?.trim()
            ?.takeIf(String::isNotEmpty)?.let { surface to it }
    }.toMap()
}

/**
 * The PUT body for choosing one surface's profile, byte-for-byte what the web
 * `SurfaceAudioProfileControl.changeProfile` sends: the profile (blank means
 * "use the configured default") plus a clear of every per-stage override,
 * because stage choices made for the previous profile do not apply to the new
 * one. Nothing else is carried — never echo settings this client does not own.
 */
fun surfaceProfilePatch(surface: String, profileId: String?): String = buildJsonObject {
    putJsonObject("surface_profiles") { put(surface, profileId?.trim().orEmpty()) }
    putJsonObject("surface_stage_options") {
        putJsonObject(surface) { NativeAudioStage.entries.forEach { put(it.wire, "") } }
    }
}.toString()

/** A choosable profile for one surface. */
data class AudioProfileChoice(val id: String, val label: String, val description: String)

/** What a profile does, in the stage words the web uses. */
fun audioProfileDescription(profile: AudioSurfaceProfile): String {
    val stages = NativeAudioStage.entries.filter { profile.stage(it).enabled }.map { it.label }
    val boundary = profile.turnBoundary?.takeIf(String::isNotBlank)?.replace('_', ' ')
    return (stages + listOfNotNull(boundary?.let { "turns: $it" })).joinToString(" · ")
        .ifBlank { "No stages configured" }
}

/** The profiles offered for [surface], sorted by label like the web select. */
fun audioProfileChoices(catalog: RealtimeVoiceCatalog, surface: ObserveAudioSurface): List<AudioProfileChoice> =
    catalog.audioProfiles.entries
        .filter { it.value.surface == surface.wire }
        .map { (id, profile) -> AudioProfileChoice(id, audioProfileLabel(id), audioProfileDescription(profile)) }
        .sortedBy { it.label.lowercase() }

/** Optimistic local apply of a profile choice; blank clears to the default. */
fun applySurfaceProfile(current: Map<String, String>, surface: String, profileId: String?): Map<String, String> {
    val id = profileId?.trim().orEmpty()
    return if (id.isEmpty() || id.equals("auto", true) || id.equals("default", true)) {
        current - surface
    } else {
        current + (surface to id)
    }
}
