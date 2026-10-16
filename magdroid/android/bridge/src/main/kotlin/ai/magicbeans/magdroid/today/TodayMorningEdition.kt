package ai.magicbeans.magdroid.today

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull
import java.time.Instant
import java.time.LocalDate
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.abs
import kotlin.math.roundToLong

/*
 * Pure pieces of the Morning Edition Today page (web `MorningEdition.svelte`,
 * `TodayRealtimeWire.svelte`, `TodayNewspaperLedger.svelte`,
 * `TodayTriageDeck.svelte`). Kept free of Android and Compose so the JVM tests
 * pin the exact strings and thresholds the web contract uses.
 */

// ---------------------------------------------------------------- Masthead

/** Volume I is the publication's first year. */
const val MORNING_EDITION_INCEPTION_YEAR = 2023

/** Standard subtractive Roman numerals; anything below 1 reads as I. */
fun romanNumeral(value: Int): String {
    var remaining = value.coerceAtLeast(1)
    val builder = StringBuilder()
    for ((letter, amount) in ROMAN_LOOKUP) {
        while (remaining >= amount) {
            builder.append(letter)
            remaining -= amount
        }
    }
    return builder.toString()
}

private val ROMAN_LOOKUP = listOf(
    "M" to 1000, "CM" to 900, "D" to 500, "CD" to 400, "C" to 100, "XC" to 90,
    "L" to 50, "XL" to 40, "X" to 10, "IX" to 9, "V" to 5, "IV" to 4, "I" to 1,
)

/** 1-based day of the local calendar year. */
fun morningEditionIssue(date: LocalDate): Int = date.dayOfYear

fun morningEditionVolume(date: LocalDate): String =
    romanNumeral(date.year - MORNING_EDITION_INCEPTION_YEAR + 1)

/** The three masthead meta items, in display order. */
/** Masthead dateline, left of the volume: `MONDAY, SEPTEMBER 28, 2026`. */
fun mastheadDateline(date: LocalDate): String =
    date.format(DateTimeFormatter.ofPattern("EEEE, MMMM d, yyyy", Locale.US)).uppercase(Locale.US)

/** Masthead volume, right of the dateline: `VOL. IV · NO. 271`. */
fun mastheadVolume(date: LocalDate): String =
    "VOL. ${morningEditionVolume(date)} · NO. ${morningEditionIssue(date)}"

// ---------------------------------------------------------------- Realtime wire

enum class TodayWireKind(val wire: String, val label: String) {
    Event("event", "EVENT"),
    Insight("insight", "INSIGHT"),
    Activity("activity", "ACTIVITY"),
}

enum class TodayWireSeverity { Info, Warn, Error, Success }

/** One line on the realtime wire. Only real feed, agent and stream records. */
data class TodayWireItem(
    val id: String,
    val kind: TodayWireKind,
    val title: String,
    val summary: String,
    val timestamp: Long,
    val badge: String,
    val severity: TodayWireSeverity = TodayWireSeverity.Info,
    val taskId: String? = null,
    val threadId: String? = null,
)

/** Web's drawer filter tabs; `null` kind is All. */
enum class TodayWireFilter(val label: String, val kind: TodayWireKind?) {
    All("All", null), Events("Events", TodayWireKind.Event),
    Insights("Insights", TodayWireKind.Insight), Activity("Activity", TodayWireKind.Activity),
}

const val TODAY_WIRE_MAX_ITEMS = 50
const val TODAY_WIRE_DISPLAY = 5
const val TODAY_WIRE_FEED_LIMIT = 15
const val TODAY_WIRE_TICKER_MS = 4_500L

/** `0/24H`, `412/24H`, `1.5K/24H`, `35K/24H`, `2.4M/24H`. */
fun formatCompact24h(count: Long): String {
    if (count <= 0) return "0/24H"
    fun compact(value: Double, suffix: String): String =
        if (value >= 10) "${value.roundToLong()}$suffix/24H"
        else "${String.format(Locale.US, "%.1f", value).removeSuffix(".0")}$suffix/24H"
    return when {
        count >= 1_000_000 -> compact(count / 1_000_000.0, "M")
        count >= 1_000 -> compact(count / 1_000.0, "K")
        else -> "$count/24H"
    }
}

private val WIRE_ACRONYMS = setOf("llm", "hitl", "ui", "api", "id", "cli", "sse", "db")
private val CAMEL_BOUNDARY = Regex("(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")

internal fun wireTitleWord(word: String): String {
    if (word.isEmpty()) return ""
    val lower = word.lowercase()
    if (lower in WIRE_ACRONYMS) return lower.uppercase()
    return lower.replaceFirstChar(Char::uppercase)
}

private fun wireWords(value: String): String = value.replace('_', ' ')
    .replace(CAMEL_BOUNDARY, " ")
    .split(Regex("\\s+"))
    .filter(String::isNotBlank)
    .joinToString(" ", transform = ::wireTitleWord)

/**
 * `event.task.status_changed` → `Task: Status Changed`; `llm_call` → `LLM Call`.
 * PascalCase transport names (`TaskStatusChanged`) are split into words too.
 */
fun humanizeEventType(raw: String): String {
    val cleaned = raw.trim().replace(Regex("^event\\.", RegexOption.IGNORE_CASE), "").trim()
    if (cleaned.isEmpty()) return "System event"
    val parts = cleaned.split('.').filter(String::isNotBlank)
    if (parts.size > 1) return "${wireWords(parts.first())}: ${wireWords(parts.drop(1).joinToString(" "))}"
    return wireWords(cleaned).ifEmpty { "System event" }
}

private val INSIGHT_FEED_TYPES = setOf("learning_insight", "learning_candidate", "agent_learning")

/** A `/v2/feed` row as an insight or activity wire line. */
fun normalizeFeedWireItem(item: TodayActivityItem, nowMs: Long = System.currentTimeMillis()): TodayWireItem {
    val insight = item.itemType in INSIGHT_FEED_TYPES
    val taskId = item.taskId?.trim()?.takeIf(String::isNotEmpty)
    return TodayWireItem(
        id = "feed-${item.id}",
        kind = if (insight) TodayWireKind.Insight else TodayWireKind.Activity,
        title = item.title.trim().ifEmpty { if (insight) "Distilled Memory" else "Fleet Activity" },
        summary = item.summary?.trim()?.takeIf(String::isNotEmpty)
            ?: taskId?.let { "Task $it" } ?: "Feed insight recorded",
        timestamp = item.updatedAt.takeIf { it > 0 } ?: item.createdAt.takeIf { it > 0 } ?: nowMs,
        badge = item.itemType.replace('_', ' '),
        severity = when (item.status) {
            "failed" -> TodayWireSeverity.Error
            "done" -> TodayWireSeverity.Success
            else -> TodayWireSeverity.Info
        },
        taskId = taskId,
        threadId = item.threadId?.trim()?.takeIf(String::isNotEmpty),
    )
}

/** One `/v2/agents/updates` event. Parsed by hand: its fields vary by kind. */
data class TodayAgentUpdate(
    val id: String,
    val agentId: String?,
    val kind: String,
    val ts: Long,
    val threadId: String? = null,
    val error: String? = null,
    val focusArea: String? = null,
    val reason: String? = null,
    val outcome: String? = null,
)

fun parseAgentUpdates(root: JsonElement): List<TodayAgentUpdate> {
    val events = (root as? JsonObject)?.get("events") as? JsonArray ?: return emptyList()
    return events.mapNotNull { element ->
        val record = element as? JsonObject ?: return@mapNotNull null
        val id = record["id"].text()?.takeIf(String::isNotBlank) ?: return@mapNotNull null
        val kind = record["kind"].text()?.takeIf(String::isNotBlank) ?: return@mapNotNull null
        TodayAgentUpdate(
            id = id,
            agentId = record["agent_id"].text()?.takeIf(String::isNotBlank),
            kind = kind,
            ts = timestampOf(record["ts"]) ?: 0,
            threadId = record["thread_id"].text()?.takeIf(String::isNotBlank),
            error = record["error"].text()?.takeIf(String::isNotBlank),
            focusArea = record["focus_area"].text()?.takeIf(String::isNotBlank),
            reason = record["reason"].text()?.takeIf(String::isNotBlank),
            outcome = record["outcome"].text()?.takeIf(String::isNotBlank),
        )
    }
}

private fun agentLabel(agentId: String): String =
    agentId.split(Regex("[-_]+")).filter(String::isNotBlank).joinToString(" ", transform = ::wireTitleWord)

fun normalizeAgentUpdate(update: TodayAgentUpdate, nowMs: Long = System.currentTimeMillis()): TodayWireItem {
    val kind = update.kind.replace('_', ' ')
    return TodayWireItem(
        id = "agent-update-${update.id}",
        kind = TodayWireKind.Activity,
        title = "${update.agentId?.let(::agentLabel)?.ifEmpty { null } ?: "Fleet Agent"}: $kind",
        summary = update.error ?: update.focusArea ?: update.reason ?: "Autonomous agent cycle logged",
        timestamp = update.ts.takeIf { it > 0 } ?: nowMs,
        badge = kind,
        severity = if (update.outcome == "failed") TodayWireSeverity.Error else TodayWireSeverity.Info,
        threadId = update.threadId,
    )
}

/**
 * Transport chatter that is not a system event worth a wire line: keepalives
 * and per-token streaming deltas would otherwise flood the ticker and inflate
 * the 24h count with frames the `events` table never records.
 */
private val WIRE_EPHEMERAL_MARKERS = listOf("heartbeat", "ping", "pong", "delta", "token", "connected", "typing")

private fun JsonObject?.firstText(vararg keys: String): String? {
    if (this == null) return null
    return keys.firstNotNullOfOrNull { key -> this[key].text()?.trim()?.takeIf(String::isNotEmpty) }
}

private fun timestampOf(value: JsonElement?): Long? {
    val primitive = value as? JsonPrimitive ?: return null
    primitive.longOrNull?.takeIf { it > 0 }?.let { return it }
    primitive.doubleOrNull?.takeIf { it > 0 }?.let { return it.toLong() }
    val text = primitive.text()?.trim().orEmpty()
    if (text.isEmpty() || primitive.isString.not()) return null
    return runCatching { Instant.parse(text).toEpochMilli() }.getOrNull()
}

/**
 * A realtime websocket frame as an `event` wire line, or null when the frame is
 * not JSON or is transport chatter. The title prefers a payload title and
 * otherwise humanizes the event type; the summary prefers summary, message,
 * then error.
 */
fun normalizeRealtimeWireEvent(raw: String, nowMs: Long = System.currentTimeMillis()): TodayWireItem? {
    val root = runCatching { todayJson.parseToJsonElement(raw) as? JsonObject }.getOrNull() ?: return null
    val outerType = root.firstText("event_type", "type") ?: return null
    val data = root["data"] as? JsonObject
    // `RuntimeTransportEvent` wraps most runtime events as
    // `{event_type:"AgentEvent", data:{event:{event_type, agent_id, payload}}}`;
    // the meaningful type, agent and payload live on the inner event.
    val inner = data?.get("event") as? JsonObject
    val type = if (outerType == "AgentEvent") inner.firstText("event_type") ?: outerType else outerType
    val lowerType = type.lowercase()
    if (lowerType.startsWith("__") || WIRE_EPHEMERAL_MARKERS.any(lowerType::contains)) return null
    val payload = (inner?.get("payload") as? JsonObject)
        ?: (data?.get("payload") as? JsonObject) ?: (root["payload"] as? JsonObject)
    val agent = (inner.firstText("agent_id") ?: data.firstText("agent_id") ?: root.firstText("agent_id"))
        ?.let(::agentLabel)?.takeIf(String::isNotBlank)
    val error = payload.firstText("error") ?: data.firstText("error") ?: root.firstText("error")
    val outcome = (payload.firstText("outcome") ?: data.firstText("outcome"))?.lowercase()
    val taskId = root.firstText("task_id") ?: data.firstText("task_id") ?: inner.firstText("task_id") ?: payload.firstText("task_id")
    val threadId = root.firstText("thread_id", "ui_thread_id") ?: data.firstText("thread_id", "ui_thread_id")
        ?: payload.firstText("thread_id", "ui_thread_id")
    val timestamp = listOf(
        root["timestamp_ms"], root["timestamp"], data?.get("timestamp_ms"), data?.get("timestamp"),
        inner?.get("timestamp_ms"), inner?.get("timestamp"),
        payload?.get("timestamp_ms"), payload?.get("timestamp"),
    ).firstNotNullOfOrNull(::timestampOf) ?: nowMs
    val eventId = root.firstText("event_id", "id") ?: data.firstText("event_id", "id") ?: payload.firstText("event_id")
    val baseTitle = payload.firstText("title") ?: data.firstText("title") ?: humanizeEventType(type)
    return TodayWireItem(
        id = "event-${eventId ?: "$type-$timestamp-${raw.hashCode()}"}",
        kind = TodayWireKind.Event,
        title = if (agent != null && !baseTitle.contains(agent, ignoreCase = true)) "$agent: $baseTitle" else baseTitle,
        summary = payload.firstText("summary") ?: data.firstText("summary")
            ?: payload.firstText("message") ?: data.firstText("message") ?: root.firstText("message")
            ?: error ?: taskId?.let { "Task $it" } ?: "",
        timestamp = timestamp,
        badge = humanizeEventType(type).substringBefore(':').lowercase(),
        severity = when {
            error != null || outcome == "failed" || outcome == "failure" -> TodayWireSeverity.Error
            outcome == "completed" || outcome == "done" || outcome == "success" -> TodayWireSeverity.Success
            else -> TodayWireSeverity.Info
        },
        taskId = taskId,
        threadId = threadId,
    )
}

/** Newest first, de-duplicated by id (the newer copy wins), capped at 50. */
fun mergeWireItems(existing: List<TodayWireItem>, incoming: List<TodayWireItem>): List<TodayWireItem> {
    val byId = LinkedHashMap<String, TodayWireItem>()
    existing.forEach { byId[it.id] = it }
    incoming.forEach { byId[it.id] = it }
    return byId.values.sortedByDescending(TodayWireItem::timestamp).take(TODAY_WIRE_MAX_ITEMS)
}

fun filterWireItems(items: List<TodayWireItem>, filter: TodayWireFilter): List<TodayWireItem> =
    items.filter { filter.kind == null || it.kind == filter.kind }.take(TODAY_WIRE_DISPLAY)

/** Web's chip counts: All counts the visible five, the kinds count everything held. */
fun wireFilterCount(items: List<TodayWireItem>, filter: TodayWireFilter): Int =
    if (filter.kind == null) items.take(TODAY_WIRE_DISPLAY).size else items.count { it.kind == filter.kind }

fun parseEventCount24h(root: JsonElement): Long? {
    val rows = (root as? JsonObject)?.get("rows") as? JsonArray ?: return null
    val first = (rows.firstOrNull() as? JsonArray)?.firstOrNull() as? JsonPrimitive ?: return null
    return first.longOrNull ?: first.doubleOrNull?.toLong()
}

fun eventCount24hSql(nowMs: Long): String =
    "SELECT COUNT(*) AS total_24h FROM events WHERE epoch_ms(timestamp) >= ${nowMs - 86_400_000L}"

/** `just now` under 45s, then minutes, hours and days (web wire). */
fun wireTimeAgo(timestamp: Long, nowMs: Long = System.currentTimeMillis()): String {
    val seconds = ((nowMs - timestamp).coerceAtLeast(0)) / 1_000
    if (seconds < 45) return "just now"
    val minutes = seconds / 60
    if (minutes < 60) return "${minutes}m ago"
    val hours = minutes / 60
    if (hours < 24) return "${hours}h ago"
    return "${hours / 24}d ago"
}

// ---------------------------------------------------------------- Ledger

/** `$X.XX`; from $100 up the cents are noise and drop. */
fun formatSpend(value: Double): String =
    if (value >= 100) "$${String.format(Locale.US, "%.0f", value)}" else "$${String.format(Locale.US, "%.2f", value)}"

/** `formatSpend` split into its dollars and `.cents` parts for the big figure. */
fun splitSpend(value: Double): Pair<String, String> {
    val digits = formatSpend(value).removePrefix("$")
    return digits.substringBefore('.') to digits.substringAfter('.', "").let { if (it.isEmpty()) "" else ".$it" }
}

private const val CURRENCY_DELTA_NOISE_USD = 0.005

/** `+$0.42`, `−$1.10` (U+2212), `new today`, or empty when nothing moved. */
fun formatSpendDelta(today: Double, yesterday: Double): String {
    if (today == 0.0 && yesterday == 0.0) return ""
    if (yesterday == 0.0 && today > 0) return "new today"
    val diff = today - yesterday
    if (abs(diff) < CURRENCY_DELTA_NOISE_USD) return ""
    val sign = if (diff > 0) "+" else "−"
    return "$sign$${String.format(Locale.US, "%.2f", abs(diff))}"
}

enum class SpendTone { Good, Bad, Neutral }

/** Inverted polarity: spending more than yesterday is the watch-out direction. */
fun spendTone(today: Double, yesterday: Double): SpendTone {
    if (today == 0.0 && yesterday == 0.0) return SpendTone.Neutral
    if (yesterday == 0.0 && today > 0) return SpendTone.Bad
    val diff = today - yesterday
    if (abs(diff) < CURRENCY_DELTA_NOISE_USD) return SpendTone.Neutral
    return if (diff > 0) SpendTone.Bad else SpendTone.Good
}

fun spendDeltaLine(today: Double, yesterday: Double): String {
    val delta = formatSpendDelta(today, yesterday)
    return if (delta.isEmpty()) "steady vs ${formatSpend(yesterday)} yday" else "$delta vs ${formatSpend(yesterday)} yday"
}

/** Web prints a whole share bare and anything else at two decimals. */
fun topProviderShareLabel(shareFraction: Double): String {
    val pct = shareFraction * 100
    return if (pct % 1.0 == 0.0) pct.toLong().toString() else String.format(Locale.US, "%.2f", pct)
}

fun hourLabel(hour: Int): String = when {
    hour == 0 -> "12a"
    hour == 12 -> "12p"
    hour > 12 -> "${hour - 12}p"
    else -> "${hour}a"
}

fun hourlySpendTooltip(hour: Int, spend: Double, calls: Int): String =
    "${hourLabel(hour)}: $${String.format(Locale.US, "%.2f", spend)} ($calls ${if (calls == 1) "call" else "calls"})"

/** Average model cost per call today, or null before the first call. */
fun avgCostPerCall(spendToday: Double, callsToday: Int): Double? =
    if (callsToday > 0) spendToday / callsToday else null

/** `$0.0015` under a cent, `$0.25` otherwise, `—` when unknown (web formatPerCall). */
fun formatPerCall(value: Double?): String = when {
    value == null -> "—"
    value < 0.01 -> "$" + String.format(Locale.US, "%.4f", value)
    else -> "$" + String.format(Locale.US, "%.2f", value)
}

/** The hour with the most spend as `3p`, or null when nothing was spent. */
fun peakSpendHour(hourlySpend: List<Double>): String? {
    var best = -1; var bestValue = 0.0
    hourlySpend.forEachIndexed { hour, value -> if (value > bestValue) { bestValue = value; best = hour } }
    return if (best < 0) null else hourLabel(best)
}

/** One-line task row for the State of Operations slide. */
data class TodayTaskLine(val id: String, val title: String, val status: String, val updatedAtMs: Long)

/** The [limit] most recently updated `/v3/tasks` rows, newest first. */
fun recentTaskLines(rows: JsonArray, limit: Int = 20): List<TodayTaskLine> = rows.mapNotNull { element ->
    val row = element as? JsonObject ?: return@mapNotNull null
    val id = row["id"].text()?.takeIf(String::isNotBlank) ?: return@mapNotNull null
    val updated = (row["updated_at"].text() ?: row["created_at"].text())
        ?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() } ?: 0L
    TodayTaskLine(
        id = id,
        title = row["title"].text()?.trim()?.takeIf(String::isNotEmpty) ?: "Untitled task",
        status = row["status"].text().orEmpty(),
        updatedAtMs = updated,
    )
}.sortedByDescending(TodayTaskLine::updatedAtMs).take(limit)

/** Compact age for one-line rows: `now`, `4m`, `2h`, `3d`. */
fun taskShortAge(epochMs: Long, nowMs: Long = System.currentTimeMillis()): String {
    if (epochMs <= 0) return ""
    val minutes = ((nowMs - epochMs).coerceAtLeast(0)) / 60_000
    return when {
        minutes < 1 -> "now"
        minutes < 60 -> "${minutes}m"
        minutes < 48 * 60 -> "${minutes / 60}h"
        else -> "${minutes / (24 * 60)}d"
    }
}

/** Operations carousel: auto-advance every 8 s, held 15 s after the reader navigates. */
const val OPS_CAROUSEL_ADVANCE_MS = 8_000L
const val OPS_CAROUSEL_MANUAL_PAUSE_MS = 15_000L

/** The slide after [current], wrapping to the first. */
fun nextCarouselIndex(current: Int, count: Int): Int = if (count <= 0) 0 else (current + 1) % count

/** Auto-advance may move only when not dragging, not held after manual navigation, and motion is allowed. */
fun carouselMayAdvance(nowMs: Long, pausedUntilMs: Long, dragging: Boolean, reduceMotion: Boolean): Boolean =
    !dragging && !reduceMotion && nowMs >= pausedUntilMs

/** `/v3/tasks` status buckets for the fleet pie. */
data class TodayTaskBuckets(val completed: Int = 0, val failed: Int = 0, val inFlight: Int = 0)

fun taskBuckets(rows: JsonArray): TodayTaskBuckets {
    var completed = 0; var failed = 0; var inFlight = 0
    rows.forEach { element ->
        when ((element as? JsonObject)?.get("status").text()) {
            "completed" -> completed++
            "failed" -> failed++
            "running", "paused", "planning" -> inFlight++
        }
    }
    return TodayTaskBuckets(completed, failed, inFlight)
}

data class TodayFleetYield(
    val succeeded: Int, val failed: Int, val inFlight: Int,
    val succeededPct: Int, val failedPct: Int, val inFlightPct: Int,
) {
    val total: Int get() = succeeded + failed + inFlight
}

/** Succeeded takes the larger of the status bucket and pulse's completed-today count. */
fun todayFleetYield(buckets: TodayTaskBuckets, pulseCompletedToday: Int): TodayFleetYield {
    val succeeded = maxOf(buckets.completed, pulseCompletedToday)
    val total = succeeded + buckets.failed + buckets.inFlight
    if (total <= 0) return TodayFleetYield(succeeded, buckets.failed, buckets.inFlight, 0, 0, 0)
    val succeededPct = Math.round(succeeded * 100.0 / total).toInt()
    val failedPct = Math.round(buckets.failed * 100.0 / total).toInt()
    return TodayFleetYield(
        succeeded, buckets.failed, buckets.inFlight,
        succeededPct, failedPct, (100 - succeededPct - failedPct).coerceAtLeast(0),
    )
}

data class TodayAgentCounts(val total: Int = 0, val enabled: Int = 0, val active: Int = 0)

/** `/v2/agents` → `{agents:[{status, disabled?}]}`; system agents are not the crew. */
fun parseAgentCounts(root: JsonElement): TodayAgentCounts {
    val agents = when (root) {
        is JsonObject -> root["agents"] as? JsonArray
        is JsonArray -> root
        else -> null
    } ?: return TodayAgentCounts()
    var enabled = 0; var active = 0
    agents.forEach { element ->
        val record = element as? JsonObject ?: return@forEach
        val status = record["status"].text()
        val disabled = (record["disabled"] as? JsonPrimitive)?.booleanOrNull == true
        if (!disabled && status != "disabled") enabled++
        if (status == "running" || status == "triggered") active++
    }
    return TodayAgentCounts(agents.size, enabled, active)
}

// ---------------------------------------------------------------- State of the Crew

/** Crew window: the last 24 hours, not the calendar day. */
const val CREW_WINDOW_MS = 86_400_000L
/** `/v3/tasks` pages walked newest-first until they leave the window. */
const val CREW_TASK_PAGE_LIMIT = 100
const val CREW_TASK_MAX_PAGES = 5

data class TodayCrewAgent(val id: String, val name: String, val status: String)
data class TodayCrewUsage(val agentId: String, val calls: Int, val costUsd: Double, val okCalls: Int)
data class TodayCrewTask(val agentId: String, val status: String, val updatedAtMs: Long)

data class TodayCrewMember(
    val id: String,
    val name: String,
    val active: Boolean,
    val disabled: Boolean,
    val costUsd: Double,
    val calls: Int,
    val okCalls: Int,
    val done: Int,
    val failed: Int,
) {
    /** done ÷ (done + failed), or null with no finished tasks. */
    val successPct: Int? get() = crewPercent(done, done + failed)
    /** Successful model calls ÷ calls, or null with no calls. */
    val reliabilityPct: Int? get() = crewPercent(okCalls, calls)
}

data class TodayCrew(val members: List<TodayCrewMember>, val activeNow: Int, val total: Int) {
    val costUsd: Double get() = members.sumOf(TodayCrewMember::costUsd)
    val tasksDone: Int get() = members.sumOf(TodayCrewMember::done)
    val reliabilityPct: Int? get() = crewPercent(members.sumOf(TodayCrewMember::okCalls), members.sumOf(TodayCrewMember::calls))
}

fun crewPercent(part: Int, whole: Int): Int? = if (whole <= 0) null else Math.round(part * 100.0 / whole).toInt()

/** `/v2/agents` rows as crew agents; the display name is `definition.name`. */
fun parseCrewAgents(root: JsonElement): List<TodayCrewAgent> {
    val agents = when (root) {
        is JsonObject -> root["agents"] as? JsonArray
        is JsonArray -> root
        else -> null
    } ?: return emptyList()
    return agents.mapNotNull { element ->
        val record = element as? JsonObject ?: return@mapNotNull null
        val definition = record["definition"] as? JsonObject
        val id = (definition?.get("agent_id") ?: record["agent_id"] ?: record["id"]).text()?.takeIf(String::isNotBlank)
            ?: return@mapNotNull null
        val disabled = (record["disabled"] as? JsonPrimitive)?.booleanOrNull == true
        TodayCrewAgent(
            id = id,
            name = definition?.get("name").text()?.trim()?.takeIf(String::isNotEmpty) ?: id,
            status = if (disabled) "disabled" else record["status"].text().orEmpty(),
        )
    }
}

/** Per-agent model use in the window; same real-call rule as the spend pulse. */
fun crewUsageSql(sinceMs: Long): String =
    "SELECT agent_id, COUNT(*) AS calls, COALESCE(SUM(cost_usd), 0) AS cost_usd, " +
        "SUM(CASE WHEN success THEN 1 ELSE 0 END) AS ok_calls, AVG(latency_ms) AS avg_latency_ms " +
        "FROM llm_calls WHERE timestamp_ms >= $sinceMs AND NULLIF(TRIM(agent_id), '') IS NOT NULL " +
        "AND (COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate') " +
        "GROUP BY agent_id ORDER BY cost_usd DESC"

/** Column-name-based parse; numbers may arrive as strings. */
fun parseCrewUsage(columns: List<String>, rows: List<JsonArray>): List<TodayCrewUsage> {
    fun at(row: JsonArray, name: String): JsonElement? = columns.indexOf(name).takeIf { it in row.indices }?.let(row::get)
    fun num(value: JsonElement?): Double = (value as? JsonPrimitive)?.let { it.doubleOrNull ?: it.contentOrNull?.toDoubleOrNull() } ?: 0.0
    return rows.mapNotNull { row ->
        val agent = at(row, "agent_id").text()?.takeIf(String::isNotBlank) ?: return@mapNotNull null
        TodayCrewUsage(agent, num(at(row, "calls")).toInt(), num(at(row, "cost_usd")), num(at(row, "ok_calls")).toInt())
    }
}

/** `/v3/tasks` rows (newest first) inside the window, as crew tasks. */
fun crewTasks(rows: JsonArray, sinceMs: Long): List<TodayCrewTask> = rows.mapNotNull { element ->
    val row = element as? JsonObject ?: return@mapNotNull null
    val agent = row["agent_id"].text()?.takeIf(String::isNotBlank) ?: return@mapNotNull null
    val updated = row["updated_at"].text()?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() } ?: return@mapNotNull null
    if (updated < sinceMs) null else TodayCrewTask(agent, row["status"].text().orEmpty(), updated)
}

/** Walk on only while the page's oldest row is still inside the window and there is a next page. */
fun crewTaskPageContinues(rows: JsonArray, sinceMs: Long, nextCursor: String?): Boolean {
    if (nextCursor.isNullOrBlank()) return false
    val oldest = (rows.lastOrNull() as? JsonObject)?.get("updated_at").text()
        ?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() } ?: return false
    return oldest >= sinceMs
}

/**
 * Agents with calls or tasks in the window, plus any active now. Active
 * first, then cost, then tasks done.
 */
fun assembleCrew(agents: List<TodayCrewAgent>, usage: List<TodayCrewUsage>, tasks: List<TodayCrewTask>): TodayCrew {
    val usageBy = usage.associateBy(TodayCrewUsage::agentId)
    val tasksBy = tasks.groupBy(TodayCrewTask::agentId)
    val known = agents.associateBy(TodayCrewAgent::id)
    val ids = LinkedHashSet<String>().apply { addAll(agents.map(TodayCrewAgent::id)); addAll(usageBy.keys); addAll(tasksBy.keys) }
    val members = ids.mapNotNull { id ->
        val agent = known[id]
        val used = usageBy[id]
        val own = tasksBy[id].orEmpty()
        val workingTask = own.any { it.status in setOf("running", "paused", "planning") }
        val active = agent?.status in setOf("running", "triggered") || workingTask
        val done = own.count { it.status == "completed" || it.status == "done" }
        val failed = own.count { it.status == "failed" }
        if (!active && used == null && own.isEmpty()) return@mapNotNull null
        TodayCrewMember(
            id = id, name = agent?.name ?: id, active = active, disabled = agent?.status == "disabled",
            costUsd = used?.costUsd ?: 0.0, calls = used?.calls ?: 0, okCalls = used?.okCalls ?: 0,
            done = done, failed = failed,
        )
    }.sortedWith(compareByDescending<TodayCrewMember> { it.active }.thenByDescending { it.costUsd }.thenByDescending { it.done })
    return TodayCrew(members, activeNow = members.count(TodayCrewMember::active), total = agents.size)
}

// ---------------------------------------------------------------- Reading room

/** Broadsheet tabs: channel follow-ups or Worth a look, one server page at a time. */
enum class TodayBroadsheetTab(val wire: String) {
    ForYou("for_you"), Worth("worth");
    companion object {
        fun fromWire(value: String?): TodayBroadsheetTab = entries.firstOrNull { it.wire == value } ?: ForYou
    }
}

/** Web's broadsheet page size (FOLLOW_UP_LIMIT / WORTH_LIMIT). */
const val TODAY_BROADSHEET_PAGE_SIZE = 5

/** One server-side page of a broadsheet tab. [loaded] is false until the first answer. */
data class TodayBroadsheetPage<T>(
    val page: Int = 1,
    val items: List<T> = emptyList(),
    val total: Int = 0,
    val loaded: Boolean = false,
    val loading: Boolean = false,
    val error: String? = null,
) {
    val pageCount: Int get() = broadsheetPageCount(total)
    val startItem: Int get() = if (total == 0 || items.isEmpty()) 0 else (page - 1) * TODAY_BROADSHEET_PAGE_SIZE + 1
    val endItem: Int get() = if (startItem == 0) 0 else minOf(total, startItem + items.size - 1)
    val hasPrevious: Boolean get() = page > 1
    val hasNext: Boolean get() = page < pageCount
}

/** Optimistic removal from a broadsheet page; the total drops with it. */
fun <T> TodayBroadsheetPage<T>.without(id: String, idOf: (T) -> String): TodayBroadsheetPage<T> =
    if (items.none { idOf(it) == id }) this
    else copy(items = items.filterNot { idOf(it) == id }, total = (total - 1).coerceAtLeast(0))

fun broadsheetPageCount(total: Int, size: Int = TODAY_BROADSHEET_PAGE_SIZE): Int =
    maxOf(1, (total + size - 1) / size)

/** `1–5 of 30`, or `0 of 0` for an empty tab (web ServerPager). */
fun broadsheetRangeLabel(page: TodayBroadsheetPage<*>): String =
    if (page.startItem == 0) "0 of ${page.total}" else "${page.startItem}–${page.endItem} of ${page.total}"

enum class TodayReadingRoomMode(val wire: String) {
    Deck("deck"), Broadsheet("broadsheet");
    companion object {
        fun fromWire(value: String?): TodayReadingRoomMode = entries.firstOrNull { it.wire == value } ?: Deck
    }
}

enum class TodayDeckLane { Dispatch, ReadingRoom }

enum class TodayDeckTab(val label: String) {
    All("All"), ForYou("For You"), Worth("Worth a Look");
    fun includes(card: TodayDeckCard): Boolean = when (this) {
        All -> true
        ForYou -> card.lane == TodayDeckLane.Dispatch
        Worth -> card.lane == TodayDeckLane.ReadingRoom
    }
}

enum class TodayDeckAction { Useful, Acknowledge, Dismiss, Primary }

/** A Morning Brief card: a channel follow-up or a worth-a-look card. */
data class TodayDeckCard(
    val id: String,
    val lane: TodayDeckLane,
    val title: String,
    val category: String,
    val summary: String,
    val sender: String? = null,
    val primaryLabel: String,
    val followUp: ChannelFollowUp? = null,
    val worth: ResurfacingCard? = null,
)

fun deckCard(followUp: ChannelFollowUp): TodayDeckCard = TodayDeckCard(
    id = deckCardId(followUp),
    lane = TodayDeckLane.Dispatch,
    title = followUp.subject?.trim()?.takeIf(String::isNotEmpty) ?: "Untitled Message",
    category = dispatchCategory(followUp),
    summary = followUp.summary?.trim()?.takeIf(String::isNotEmpty)
        ?: followUp.reason?.trim()?.takeIf(String::isNotEmpty) ?: "No preview available",
    sender = followUp.sender?.trim()?.takeIf(String::isNotEmpty),
    primaryLabel = "Do it",
    followUp = followUp,
)

fun deckCard(worth: ResurfacingCard): TodayDeckCard = TodayDeckCard(
    id = deckCardId(worth),
    lane = TodayDeckLane.ReadingRoom,
    title = worth.sourceTitle.trim().ifEmpty { worth.line.trim() }.ifEmpty { "Resurfaced Note" },
    category = readingRoomCategory(worth),
    summary = worth.summary.trim().ifEmpty { worth.whyNow.trim() }.ifEmpty { "Resurfaced for your attention" },
    primaryLabel = "Open",
    worth = worth,
)

fun deckCardId(followUp: ChannelFollowUp): String = "followup:${followUp.id}"
fun deckCardId(worth: ResurfacingCard): String = "worth:${worth.id}"

fun dispatchCategory(followUp: ChannelFollowUp): String =
    "DISPATCH · ${followUp.provider.trim().uppercase().ifEmpty { "CORRESPONDENCE" }}"

fun readingRoomCategory(worth: ResurfacingCard): String =
    "READING ROOM · ${worth.sourceKind.replace('_', ' ').trim().uppercase().ifEmpty { "NOTE" }}"

/** Two dispatches, then one reading-room card, until both run out. */
fun interleaveDeck(followUps: List<ChannelFollowUp>, worth: List<ResurfacingCard>): List<TodayDeckCard> {
    val result = mutableListOf<TodayDeckCard>()
    var f = 0; var w = 0
    while (f < followUps.size || w < worth.size) {
        repeat(2) { if (f < followUps.size) result += deckCard(followUps[f++]) }
        if (w < worth.size) result += deckCard(worth[w++])
    }
    return result
}

fun readingRoomCountLabel(total: Int): String = if (total > 50) "50+" else total.coerceAtLeast(0).toString()

/** Deck stays flowing: fetch the next page once two or fewer cards remain. */
fun deckNeedsMore(visibleRemaining: Int, hasMoreFollowUps: Boolean, hasMoreWorth: Boolean): Boolean =
    visibleRemaining <= 2 && (hasMoreFollowUps || hasMoreWorth)

// ---------------------------------------------------------------- Deliverables

const val TODAY_DELIVERED_PAGE = 6
const val TODAY_DIGEST_PAGE = 6

fun deliverableDateline(item: TodayItem): String =
    "FILED · ${item.sourceKind.replace('_', ' ').uppercase()}"

fun briefingMeta(briefing: TodayBriefing, nowMs: Long = System.currentTimeMillis()): String = listOfNotNull(
    briefing.sourceAgentId?.trim()?.takeIf(String::isNotEmpty)?.let { "By $it" },
    briefing.surface.taskId?.trim()?.takeIf(String::isNotEmpty)?.let { "Task $it" },
    runCatching { Instant.parse(briefing.surface.publishedAt).toEpochMilli() }.getOrNull()
        ?.let { todayRelativeTime(it, nowMs) },
).joinToString(" · ")
