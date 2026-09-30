package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.meetings.ActiveMeeting
import ai.magicbeans.magdroid.meetings.UpcomingMeeting
import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The four Command Deck views. The KPI cards are the only switchers, exactly as
 * on the web `/observe?pane=…`.
 */
enum class ObservePane(val wire: String) {
    Now("now"),
    Sources("sources"),
    Audio("audio"),
    Notes("notes"),
    ;

    companion object {
        fun fromWire(raw: String?): ObservePane? {
            val value = raw?.trim()?.lowercase() ?: return null
            return entries.firstOrNull { it.wire == value }
        }

        /** `pane` out of `magican://observe?pane=sources`; null when absent or unknown. */
        fun fromDeepLink(raw: String?): ObservePane? {
            val uri = raw?.let { runCatching { java.net.URI(it) }.getOrNull() } ?: return null
            val query = uri.rawQuery ?: return null
            return query.split('&').firstNotNullOfOrNull { part ->
                val pieces = part.split('=', limit = 2)
                if (pieces[0] == "pane") fromWire(pieces.getOrNull(1)) else null
            }
        }
    }
}

/** One-shot handoff of a deep-linked pane into Observe. */
object ObservePaneRequests {
    private val _pane = MutableStateFlow<ObservePane?>(null)
    val pane: StateFlow<ObservePane?> = _pane.asStateFlow()

    fun request(pane: ObservePane?) {
        if (pane != null) _pane.value = pane
    }

    fun consume(pane: ObservePane) {
        if (_pane.value == pane) _pane.value = null
    }
}

/** The selected view, remembered per device (default Now). */
internal object ObservePaneMemory {
    private const val STORE = "magdroid.observe"
    private const val KEY = "pane"

    fun load(context: Context): ObservePane =
        ObservePane.fromWire(
            runCatching {
                context.getSharedPreferences(STORE, Context.MODE_PRIVATE).getString(KEY, null)
            }.getOrNull(),
        ) ?: ObservePane.Now

    fun save(context: Context, pane: ObservePane) {
        runCatching {
            context.getSharedPreferences(STORE, Context.MODE_PRIVATE).edit().putString(KEY, pane.wire).apply()
        }
    }
}

/** What renders, top to bottom, for a pane. The contract the parity test pins. */
internal enum class ObserveDeckSection {
    CommandHeader,
    Kpis,
    Live,
    Launchpad,
    Upcoming,
    Widgets,
    Recent,
    ThisPhone,
    WebAccounts,
    AudioProfiles,
    PublishedNotes,
    AudioNotes,
}

internal fun observeDeckSections(pane: ObservePane, hasLive: Boolean): List<ObserveDeckSection> = buildList {
    add(ObserveDeckSection.CommandHeader)
    add(ObserveDeckSection.Kpis)
    // Live sits ABOVE the views: a recording must be visible whichever view is open.
    if (hasLive) add(ObserveDeckSection.Live)
    when (pane) {
        ObservePane.Now -> {
            add(ObserveDeckSection.Launchpad)
            add(ObserveDeckSection.Upcoming)
            add(ObserveDeckSection.Widgets)
            add(ObserveDeckSection.Recent)
        }
        ObservePane.Sources -> {
            add(ObserveDeckSection.ThisPhone)
            add(ObserveDeckSection.WebAccounts)
        }
        ObservePane.Audio -> add(ObserveDeckSection.AudioProfiles)
        ObservePane.Notes -> {
            add(ObserveDeckSection.PublishedNotes)
            add(ObserveDeckSection.AudioNotes)
        }
    }
}

/**
 * Captures live right now: every server session, plus the in-app capture when
 * the server list does not already carry it (it lags the phone by a poll).
 */
internal fun observeActiveCaptureCount(
    serverActive: List<ActiveMeeting>,
    localLive: Boolean,
    localSessionId: String?,
): Int {
    val localCounted = localSessionId != null && serverActive.any { it.sessionId == localSessionId }
    return serverActive.size + if (localLive && !localCounted) 1 else 0
}

internal fun liveCalendarMeetings(upcoming: List<UpcomingMeeting>): Int = upcoming.count { it.liveNow }

private fun plural(n: Int, word: String) = "$n $word${if (n == 1) "" else "s"}"

/** The web deck's header status line, word for word. */
internal fun observeCaptureStatusLine(activeCaptures: Int, liveMeetings: Int): String = when {
    activeCaptures > 0 -> "${plural(activeCaptures, "capture")} live"
    liveMeetings > 0 -> "${plural(liveMeetings, "meeting")} live — nothing capturing"
    else -> "Quiet — nothing capturing"
}

internal fun nowKpiSub(activeCaptures: Int, liveMeetings: Int): String = when {
    activeCaptures > 0 -> "${plural(activeCaptures, "live capture")}"
    liveMeetings > 0 -> "${plural(liveMeetings, "live meeting")}"
    else -> "Live captures & meetings"
}

/** Two surfaces once preferences load; a dash when they cannot be read. */
internal fun audioKpiValue(preferencesLoaded: Boolean): String = if (preferencesLoaded) "2" else "—"

/** Recent captures when known, else published notes, else a dash. */
internal fun notesKpiValue(recentCount: Int?, publishedTotal: Int?): String =
    (recentCount ?: publishedTotal)?.toString() ?: "—"

internal fun kpiAccessibilityLabel(label: String, value: String, sub: String, selected: Boolean): String =
    buildString {
        append(label.replace("&", "and"))
        append(", ")
        append(value)
        if (sub.isNotBlank()) append(", ").append(sub.replace("&", "and"))
        if (selected) append(", selected")
    }

/** Capture launchpad tiles. */
internal enum class LaunchTile(val title: String, val subtitle: String) {
    Listen("Listen", "Audio & mic capture"),
    JoinAgent("Join as agent", "Agent attendee"),
    ShareScreen("Share screen", "Screen + audio journal"),
    Brainstorm("Brainstorm", "Idea space"),
}

/** Listen is hidden while this phone is already capturing, as the old card was. */
internal fun launchpadTiles(localLive: Boolean): List<LaunchTile> =
    LaunchTile.entries.filterNot { localLive && it == LaunchTile.Listen }

/** One open form at a time; tapping the open tile closes it. */
internal fun toggleLaunchTile(open: LaunchTile?, tapped: LaunchTile): LaunchTile? =
    if (open == tapped) null else tapped

private fun normalizedMeetingUrl(value: String?): String? {
    val trimmed = value?.trim()?.takeIf(String::isNotEmpty) ?: return null
    val withScheme = if (Regex("^[a-zA-Z][a-zA-Z0-9+.-]*://").containsMatchIn(trimmed)) trimmed else "https://$trimmed"
    return runCatching {
        val uri = java.net.URI(withScheme)
        val host = uri.host?.lowercase()?.removePrefix("www.") ?: return@runCatching null
        val path = (uri.path.orEmpty().trimEnd('/').ifEmpty { "/" }).lowercase()
        "$host$path"
    }.getOrNull() ?: trimmed.lowercase().replace(Regex("^[a-z][a-z0-9+.-]*://"), "")
        .replace(Regex("[?#].*$"), "").trimEnd('/')
}

private fun normalizedMeetingTitle(value: String?): String? =
    value?.trim()?.lowercase()?.replace(Regex("\\s+"), " ")?.takeIf(String::isNotEmpty)

/**
 * The active capture an Upcoming row represents — ported from the web's
 * `findActiveSessionForUpcoming`: URLs are authoritative; titles only match
 * when one side has no URL, so same-titled meetings with different links stay
 * actionable.
 */
internal fun activeSessionForUpcoming(event: UpcomingMeeting, sessions: List<ActiveMeeting>): ActiveMeeting? {
    val eventUrl = normalizedMeetingUrl(event.meetUrl)
    if (eventUrl != null) {
        sessions.firstOrNull { normalizedMeetingUrl(it.url) == eventUrl }?.let { return it }
    }
    val eventTitle = normalizedMeetingTitle(event.title) ?: return null
    return sessions.firstOrNull { session ->
        normalizedMeetingTitle(session.title) == eventTitle &&
            (eventUrl == null || normalizedMeetingUrl(session.url) == null)
    }
}

/** Runtime-permission state as the Sources view words it. */
internal enum class PermissionStatus(val label: String) {
    Granted("Allowed"),
    NotAsked("Not asked yet"),
    Denied("Denied"),
    Blocked("Blocked — change it in system settings"),
}

/**
 * Android cannot say "never asked" directly: a permission that was never
 * requested and one denied for good both show no rationale. The asked flag we
 * keep ourselves separates them.
 */
internal fun permissionStatus(granted: Boolean, askedBefore: Boolean, showRationale: Boolean): PermissionStatus = when {
    granted -> PermissionStatus.Granted
    showRationale -> PermissionStatus.Denied
    askedBefore -> PermissionStatus.Blocked
    else -> PermissionStatus.NotAsked
}

/** "Just now", "12 min ago", "3 h ago", else a short date. */
internal fun relativeWhen(epochMillis: Long, nowMillis: Long = System.currentTimeMillis()): String {
    if (epochMillis <= 0) return ""
    val delta = (nowMillis - epochMillis).coerceAtLeast(0)
    val minutes = delta / 60_000
    return when {
        minutes < 1 -> "just now"
        minutes < 60 -> "$minutes min ago"
        minutes < 24 * 60 -> "${minutes / 60} h ago"
        else -> java.time.Instant.ofEpochMilli(epochMillis).atZone(java.time.ZoneId.systemDefault())
            .format(java.time.format.DateTimeFormatter.ofPattern("d MMM, h:mm a"))
    }
}
