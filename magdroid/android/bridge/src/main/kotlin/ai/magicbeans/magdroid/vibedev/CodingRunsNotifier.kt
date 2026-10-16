package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.bridge.R
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/**
 * The "Magician at work" summary the shade shows: counts plus one featured
 * item, the Android shape of the iOS summary Live Activity. The server ranks
 * coding runs, VibeDev crews and Work tasks (`work_summary`) and sends the
 * result on the `coding_runs` route; a Magician that predates it sends only
 * the coding runs' [headline] and [detail], which still work alone.
 */
data class CodingRunsSummary(
    val working: Int,
    val needsYou: Int,
    val done: Int,
    val failed: Int,
    val headline: String,
    val detail: String,
    /** Something is going or waits on the owner. */
    val active: Boolean,
    val eventTimestamp: Long,
    /** The featured item; null from an older Magician. */
    val featured: WorkSummaryLine? = null,
    val featuredContext: String = "",
    val featuredStep: String = "",
    /** `approve_diff | answer` on the featured item, with its id. */
    val actionKind: String? = null,
    val actionId: String? = null,
    /** The featured item's link (`magican://vibe` from an older Magician). */
    val deepLink: String? = null,
    /** The next two items by rank. */
    val next: List<WorkSummaryLine> = emptyList(),
    /** Items beyond the featured one and [next]. */
    val more: Int = 0,
) {
    companion object {
        const val KIND = "coding_runs"

        /**
         * An FCM data message (every value a string). `done` there is the
         * delivery flag for "not active"; the count rides `done_count`.
         */
        fun fromPush(data: Map<String, String>): CodingRunsSummary? {
            if (data["kind"] != KIND) return null
            val timestamp = data["event_timestamp"]?.toLongOrNull()?.takeIf { it > 0 } ?: return null
            fun count(key: String) = data[key]?.trim()?.toIntOrNull()?.coerceAtLeast(0) ?: 0
            fun text(key: String) = data[key]?.trim().orEmpty()
            val working = count("working")
            val needsYou = count("needs_you")
            return CodingRunsSummary(
                working = working,
                needsYou = needsYou,
                done = count("done_count"),
                failed = count("failed"),
                headline = text("headline"),
                detail = text("detail"),
                active = data["active"]?.trim()?.toBooleanStrictOrNull() ?: (working + needsYou > 0),
                eventTimestamp = timestamp,
                featured = text("featured_id").takeIf(String::isNotEmpty)?.let { id ->
                    WorkSummaryLine(id, text("featured_kind"), text("featured_state"), text("featured_title"))
                },
                featuredContext = text("featured_context"),
                featuredStep = text("featured_step"),
                actionKind = text("action_kind").ifEmpty { null },
                actionId = text("action_id").ifEmpty { null },
                deepLink = text("deep_link").ifEmpty { null },
                next = parseNext(data["next"]),
                more = count("more"),
            )
        }

        /** `next`: a JSON string of `[{"id","kind","state","title"}]`. */
        internal fun parseNext(raw: String?): List<WorkSummaryLine> {
            val array = raw?.takeIf(String::isNotBlank)
                ?.let { runCatching { vibeDevJson.parseToJsonElement(it) }.getOrNull() } as? JsonArray ?: return emptyList()
            return array.mapNotNull { element ->
                val item = element as? JsonObject ?: return@mapNotNull null
                fun text(key: String) = (item[key] as? JsonPrimitive)?.contentOrNull?.trim().orEmpty()
                text("id").takeIf(String::isNotEmpty)?.let { WorkSummaryLine(it, text("kind"), text("state"), text("title")) }
            }.take(2)
        }

        /** The same summary from an in-app `/work/live` read. */
        fun fromWork(live: WorkLive, nowMs: Long): CodingRunsSummary {
            val counts = live.counts
            val featured = live.featured
            val next = live.next.take(2)
            fun line(item: WorkItem) = WorkSummaryLine(item.id, item.kind, item.state, item.title)
            return CodingRunsSummary(
                working = counts.working,
                needsYou = counts.needsYou,
                done = counts.done,
                failed = counts.failed,
                headline = featured?.let { listOf(it.title, it.context.orEmpty()).filter(String::isNotBlank).joinToString(" · ") }.orEmpty(),
                detail = featured?.let { if (it.state == "needs_you") "Needs you" else it.step.orEmpty() }.orEmpty(),
                active = counts.working + counts.needsYou > 0,
                eventTimestamp = nowMs,
                featured = featured?.let(::line),
                featuredContext = featured?.context.orEmpty(),
                featuredStep = featured?.step.orEmpty(),
                actionKind = featured?.action?.kind?.takeIf(String::isNotBlank),
                actionId = featured?.action?.id?.takeIf(String::isNotBlank),
                deepLink = featured?.deepLink,
                next = next.map(::line),
                more = (live.items.size - (if (featured != null) 1 else 0) - next.size).coerceAtLeast(0),
            )
        }

        /** The coding runs alone, from `/vibedev/runs/live` (server `summarize`). */
        fun fromLive(runs: List<VibeDevLiveRun>, nowMs: Long): CodingRunsSummary {
            fun count(vararg states: VibeDevRunState) = runs.count { it.state in states }
            val working = count(VibeDevRunState.Working, VibeDevRunState.Paused)
            val needsYou = count(VibeDevRunState.NeedsYou)
            val top = runs.firstOrNull { it.state == VibeDevRunState.NeedsYou }
                ?: runs.firstOrNull { it.state == VibeDevRunState.Working }
                ?: runs.firstOrNull()
            val headline = top?.let { run ->
                val label = run.lane?.let { "$it lane" } ?: run.title
                if (run.projectName.isEmpty()) label else "$label · ${run.projectName}"
            }.orEmpty()
            val detail = top?.let { run ->
                if (run.state == VibeDevRunState.NeedsYou) "Needs you" else run.step ?: run.summary ?: ""
            }.orEmpty()
            return CodingRunsSummary(
                working = working,
                needsYou = needsYou,
                done = count(VibeDevRunState.Done),
                failed = count(VibeDevRunState.Failed),
                headline = headline,
                detail = detail,
                active = working + needsYou > 0,
                eventTimestamp = nowMs,
            )
        }
    }
}

/** A button on the summary notification. */
enum class WorkCardAction(val label: String) { Approve("Approve"), Open("Open"), Follow("Follow") }

/** What the notification says, separated from posting it so it is tested. */
data class CodingRunsCard(
    val title: String,
    val text: String,
    /** The counts line, e.g. "3 running · 1 needs you". */
    val counts: String?,
    val ongoing: Boolean,
    /** Auto-dismiss after this long; null stays until replaced. */
    val timeoutMs: Long?,
    /** The expanded list (`InboxStyle`): featured, next two, "+N more", counts. */
    val lines: List<String> = emptyList(),
    /** At most three. */
    val actions: List<WorkCardAction> = emptyList(),
)

/**
 * How long a non-ongoing card for work that just ended stays: the server's
 * "just done / just failed" window, after which its next summary drops it.
 */
const val CODING_RUNS_FINISHED_TIMEOUT_MS = 2 * 60_000L

/**
 * How long an active card outlives its last post. The server re-sends an
 * unchanged active summary at least every 10 minutes, so a card this old
 * missed its end (a lost push, a server restart) and takes itself down.
 */
const val CODING_RUNS_ACTIVE_TIMEOUT_MS = 30 * 60_000L

fun codingRunsCounts(summary: CodingRunsSummary): String? = buildList {
    if (summary.working > 0) add("${summary.working} running")
    if (summary.needsYou > 0) add("${summary.needsYou} needs you")
    if (summary.failed > 0) add("${summary.failed} failed")
    if (summary.done > 0) add("${summary.done} done")
}.takeIf { it.isNotEmpty() }?.joinToString(" · ")

/**
 * The card for [summary], or null when there is nothing to show (nothing is
 * active and nothing just ended), which takes the card down. [followed] hides
 * Follow on an item already followed.
 */
fun codingRunsCard(summary: CodingRunsSummary, followed: Set<String> = emptySet()): CodingRunsCard? {
    val counts = codingRunsCounts(summary)
    val featured = summary.featured
    val title = featured?.title?.takeIf(String::isNotBlank) ?: summary.headline.takeIf(String::isNotBlank)
    if (!summary.active) {
        // The brief "just done / just failed" state; nothing to say ends it.
        title ?: return null
        return CodingRunsCard(
            title = title,
            text = featured?.let { workStateLabel(it.state) } ?: counts ?: "Finished",
            counts = counts,
            ongoing = false,
            timeoutMs = CODING_RUNS_FINISHED_TIMEOUT_MS,
            actions = if (featured != null) listOf(WorkCardAction.Open) else emptyList(),
        )
    }
    val text = when {
        featured?.state == "needs_you" -> "Needs you"
        featured != null && summary.featuredStep.isNotBlank() -> summary.featuredStep
        else -> summary.detail.ifBlank { counts ?: "Working" }
    }
    val lines = if (featured == null) emptyList() else buildList {
        add(listOf(title ?: workKindLabel(featured.kind), summary.featuredContext).filter(String::isNotBlank).joinToString(" · ") + " — " + text)
        summary.next.forEach { add("${it.title.ifBlank { workKindLabel(it.kind) }} — ${workStateLabel(it.state)}") }
        if (summary.more > 0) add("+${summary.more} more")
        counts?.let(::add)
    }.take(5)
    val actions = if (featured == null) emptyList() else buildList {
        if (summary.actionKind == "approve_diff" && !summary.actionId.isNullOrBlank()) add(WorkCardAction.Approve)
        add(WorkCardAction.Open)
        if (featured.id !in followed && !workStateEnded(featured.state)) add(WorkCardAction.Follow)
    }
    return CodingRunsCard(
        title = title ?: "Magician at work",
        text = text,
        counts = counts,
        ongoing = true,
        timeoutMs = CODING_RUNS_ACTIVE_TIMEOUT_MS,
        lines = lines,
        actions = actions.take(3),
    )
}

/** Whether to raise the "Needs you" heads-up: more need the owner than last shown. */
fun shouldAlertNeedsYou(summary: CodingRunsSummary, lastNeedsYou: Int): Boolean =
    summary.active && summary.needsYou > lastNeedsYou

/** Where a summary came from; each source keeps its own clock. */
enum class CodingRunsSource { Push, App }

/**
 * What to do with a summary, given what the shade shows now: post the card,
 * take it down, or leave it.
 *
 * Pushes carry the server's clock and in-app reads the phone's, so a summary
 * is ordered only against earlier ones from the same source ([lastSameSource]);
 * across sources the one received last wins. An inactive summary only
 * replaces a card that was showing work, so a quiet refresh never posts a
 * finished card out of nowhere.
 */
enum class CodingRunsAction { Post, Cancel, Ignore }

/**
 * Whether the shade shows a live card: one was posted active, its staleness
 * window has not run out, and (when the system can say) it is still in the
 * shade — the owner may have dismissed it, or a reboot cleared it. Without
 * this an inactive summary after a server restart posted a finished card
 * over nothing.
 */
fun codingRunsShowingActive(activeUntilMs: Long, nowMs: Long, cardPresent: Boolean?): Boolean =
    activeUntilMs > nowMs && cardPresent != false

fun codingRunsAction(
    summary: CodingRunsSummary,
    enabled: Boolean,
    lastSameSource: Long,
    showingActive: Boolean,
): CodingRunsAction = when {
    !enabled -> CodingRunsAction.Cancel
    summary.eventTimestamp <= lastSameSource -> CodingRunsAction.Ignore
    summary.active -> CodingRunsAction.Post
    showingActive -> CodingRunsAction.Post
    else -> CodingRunsAction.Ignore
}

/**
 * The ongoing "Magician at work" notification: the Android shape of the iOS
 * summary Live Activity, driven by `coding_runs` FCM data messages while the
 * app is closed and by the app's own `/work/live` reads while it is open.
 * One card in a quiet channel, a newer-event gate, the featured item as title,
 * counts as subtext, up to five lines expanded, and Approve / Open / Follow.
 * An active card times out [CODING_RUNS_ACTIVE_TIMEOUT_MS] after its last post
 * and is re-checked on every foreground ([reconcile]), so a missed end cannot
 * leave it up. A rise in needs-you also raises one heads-up in its own
 * high-importance channel; the summary itself only ever updates silently.
 *
 * Android 16 Live Updates (a promoted ongoing notification with a status-bar
 * chip) need compileSdk 36; this build compiles against 34, so the summary is
 * a plain ongoing notification on every version.
 */
object CodingRunsNotifier {
    private const val PREFERENCES = "magican_vibedev_runs_v1"
    private const val ENABLED = "show_in_shade"
    private const val LAST_PUSH_TIMESTAMP = "last_push_timestamp"
    private const val LAST_APP_TIMESTAMP = "last_app_timestamp"
    /** When the posted active card goes stale; 0 when none is up. */
    private const val ACTIVE_UNTIL = "active_until"
    /** Before [ACTIVE_UNTIL]: a flag for a card posted with no timeout. */
    private const val LEGACY_SHOWING_ACTIVE = "showing_active"
    /** The needs-you count of the last summary shown, for the heads-up. */
    private const val LAST_NEEDS_YOU = "last_needs_you"
    /** Kept from the "Coding runs" card, so an owner's channel settings carry over. */
    const val CHANNEL_ID = "magician.vibedev.runs"
    const val NOTIFICATION_ID = 4715
    const val NEEDS_YOU_CHANNEL_ID = "magician.work.needs_you"
    const val NEEDS_YOU_NOTIFICATION_ID = 4716
    /** A one-off note on the summary channel: an action from a button failed. */
    private const val NOTICE_NOTIFICATION_ID = 4718
    /** The summary and the followed items' cards group together. */
    const val GROUP = "magician.work"
    const val DEEP_LINK = "magican://vibe"
    private val lock = Any()

    /** "Show Magician at work in the notification shade"; on unless turned off. */
    fun isEnabled(context: Context): Boolean =
        context.applicationContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getBoolean(ENABLED, true)

    /** Turning it off takes the card down; the caller drops the push route. */
    fun setEnabled(context: Context, enabled: Boolean) {
        val app = context.applicationContext
        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putBoolean(ENABLED, enabled).apply()
        if (!enabled) dismiss(app)
    }

    /** Apply a summary from a push or an in-app read; true when the shade changed. */
    fun apply(context: Context, summary: CodingRunsSummary, source: CodingRunsSource): Boolean {
        val app = context.applicationContext
        val preferences = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val watermark = if (source == CodingRunsSource.Push) LAST_PUSH_TIMESTAMP else LAST_APP_TIMESTAMP
        return synchronized(lock) {
            val action = codingRunsAction(
                summary,
                enabled = preferences.getBoolean(ENABLED, true),
                lastSameSource = preferences.getLong(watermark, 0),
                showingActive = showingActive(app),
            )
            when (action) {
                CodingRunsAction.Ignore -> false
                CodingRunsAction.Cancel -> { dismiss(app); false }
                CodingRunsAction.Post -> {
                    val card = codingRunsCard(summary, WorkFollows.ids(app))
                    val alert = shouldAlertNeedsYou(summary, preferences.getInt(LAST_NEEDS_YOU, 0))
                    preferences.edit()
                        .putLong(watermark, summary.eventTimestamp)
                        .putLong(ACTIVE_UNTIL, if (summary.active && card != null) System.currentTimeMillis() + (card.timeoutMs ?: 0) else 0)
                        .putInt(LAST_NEEDS_YOU, if (summary.active) summary.needsYou else 0)
                        .apply()
                    if (card == null) dismiss(app) else post(app, card, summary)
                    when {
                        alert -> postNeedsYou(app, summary)
                        summary.needsYou == 0 -> cancel(app, NEEDS_YOU_NOTIFICATION_ID)
                    }
                    true
                }
            }
        }
    }

    /**
     * Whether a live card is up. A card past its staleness window is taken
     * down here too: below API 26 nothing else times it out.
     */
    private fun showingActive(app: Context): Boolean {
        val preferences = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        if (preferences.contains(LEGACY_SHOWING_ACTIVE)) {
            // A card from before the staleness window never times out: take
            // it down; the server's next heartbeat posts a fresh one.
            val legacy = preferences.getBoolean(LEGACY_SHOWING_ACTIVE, false)
            preferences.edit().remove(LEGACY_SHOWING_ACTIVE).apply()
            if (legacy) dismiss(app)
        }
        val activeUntil = preferences.getLong(ACTIVE_UNTIL, 0)
        if (activeUntil == 0L) return false
        val present = runCatching {
            app.getSystemService(NotificationManager::class.java)?.activeNotifications?.any { it.id == NOTIFICATION_ID }
        }.getOrNull()
        val showing = codingRunsShowingActive(activeUntil, System.currentTimeMillis(), present)
        if (!showing) dismiss(app)
        return showing
    }

    /**
     * On launch and foreground: a card that says work is going, or a followed
     * item's card, is checked against one live read, so a missed end clears
     * as soon as the app is opened rather than when the staleness window runs
     * out.
     */
    fun reconcile(context: Context) {
        val app = context.applicationContext
        val due = synchronized(lock) {
            app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getBoolean(ENABLED, true) && showingActive(app)
        } || FollowedWorkNotifier.hasShown(app) || WorkFollows.ids(app).isNotEmpty()
        if (due) refresh(app)
    }

    /**
     * Read the summary now and apply it ([CodingRunsSource.App]): the Now
     * screen after each live-runs read ([fallbackRuns], used when this
     * Magician has no `/work/live`), and after a notification button acted.
     * One read at a time; a request during one reads once more after it.
     */
    fun refresh(context: Context, fallbackRuns: List<VibeDevLiveRun>? = null) {
        val app = context.applicationContext
        pendingFallback = fallbackRuns
        refreshAgain.set(true)
        if (!refreshing.compareAndSet(false, true)) return
        reconcileScope.launch {
            do {
                try {
                    while (refreshAgain.getAndSet(false)) {
                        readSummary(app, pendingFallback)?.let { apply(app, it, CodingRunsSource.App) }
                    }
                } finally {
                    refreshing.set(false)
                }
                // A request that came in as this one finished is not lost.
            } while (refreshAgain.get() && refreshing.compareAndSet(false, true))
        }
    }

    /**
     * One read of the summary: `/work/live` (which also brings the followed
     * list and the followed items' state), or the coding runs alone from a
     * Magician that predates it. Null when neither could be read.
     */
    internal suspend fun readSummary(app: Context, fallbackRuns: List<VibeDevLiveRun>?): CodingRunsSummary? {
        val source = runCatching { VibeDevConnection.source(app) }.getOrNull() ?: return null
        val now = System.currentTimeMillis()
        if (now >= workLiveMissingUntil) {
            val read = runCatching { source.workLive() }
            read.getOrNull()?.let { live ->
                WorkFollows.sync(app, live.followed)
                FollowedWorkNotifier.reconcile(app, live)
                return CodingRunsSummary.fromWork(live, now)
            }
            when {
                // A 404: an older Magician; ask again later, not on every read.
                read.isSuccess -> workLiveMissingUntil = now + WORK_LIVE_RETRY_MS
                // Offline or refused: no runs read now could do better.
                fallbackRuns == null -> return null
            }
        }
        val runs = fallbackRuns ?: runCatching { source.liveRuns().runs }.getOrNull() ?: return null
        return CodingRunsSummary.fromLive(runs, now)
    }

    private val refreshing = AtomicBoolean(false)
    private val refreshAgain = AtomicBoolean(false)
    @Volatile private var pendingFallback: List<VibeDevLiveRun>? = null
    @Volatile private var workLiveMissingUntil = 0L
    private const val WORK_LIVE_RETRY_MS = 10 * 60_000L
    private val reconcileScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * The active machine changed: the card and both clocks belonged to the
     * old one, and the new machine's server clock owes nothing to them.
     */
    fun reset(context: Context) {
        val app = context.applicationContext
        synchronized(lock) {
            app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit()
                .remove(LAST_PUSH_TIMESTAMP)
                .remove(LAST_APP_TIMESTAMP)
                .remove(LAST_NEEDS_YOU)
                .apply()
            workLiveMissingUntil = 0L
            dismiss(app)
            cancel(app, NEEDS_YOU_NOTIFICATION_ID)
        }
    }

    /** The waiting item was answered from a button: its heads-up goes. */
    fun clearNeedsYou(context: Context) = cancel(context.applicationContext, NEEDS_YOU_NOTIFICATION_ID)

    /** A short note on the quiet channel (a button's action failed); tap opens [link]. */
    fun postNotice(context: Context, title: String, text: String, link: String) {
        val app = context.applicationContext
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        ensureSummaryChannel(manager)
        val builder = builder(app, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(openIntent(app, link, NOTICE_NOTIFICATION_ID))
            .setAutoCancel(true)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) builder.setTimeoutAfter(NOTICE_TIMEOUT_MS)
        runCatching { manager.notify(NOTICE_NOTIFICATION_ID, builder.build()) }
    }

    private const val NOTICE_TIMEOUT_MS = 2 * 60_000L

    private fun post(app: Context, card: CodingRunsCard, summary: CodingRunsSummary) {
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        ensureSummaryChannel(manager)
        val featuredId = summary.featured?.id
        val link = if (summary.featured == null) DEEP_LINK else workItemLink(featuredId, summary.deepLink)
        val builder = builder(app, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(card.title)
            .setContentText(card.text)
            .setContentIntent(openIntent(app, link, NOTIFICATION_ID))
            .setOngoing(card.ongoing)
            .setAutoCancel(!card.ongoing)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setGroup(GROUP)
        card.counts?.let { builder.setSubText(it) }
        if (card.lines.isNotEmpty()) {
            builder.setStyle(Notification.InboxStyle().also { style -> card.lines.forEach { style.addLine(it) } })
        }
        card.actions.forEach { button ->
            val intent = when (button) {
                WorkCardAction.Open -> openIntent(app, link, NOTIFICATION_ID + 100)
                WorkCardAction.Approve -> summary.actionId?.let { broadcast(app, WorkActions.ACTION_APPROVE, featuredId, it, summary.deepLink) }
                WorkCardAction.Follow -> featuredId?.let { broadcast(app, WorkActions.ACTION_FOLLOW, it, null, summary.deepLink) }
            } ?: return@forEach
            builder.addAction(action(button.label, intent, authenticate = button == WorkCardAction.Approve))
        }
        if (card.timeoutMs != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) builder.setTimeoutAfter(card.timeoutMs)
        runCatching { manager.notify(NOTIFICATION_ID, builder.build()) }
    }

    /** One heads-up naming the item that now needs the owner; tap opens it. */
    @Suppress("DEPRECATION") // Priority and defaults alert below API 26, where channels do not exist.
    private fun postNeedsYou(app: Context, summary: CodingRunsSummary) {
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(NEEDS_YOU_CHANNEL_ID, "Needs you", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "Once each time a run, crew or task you have going starts waiting on you."
                },
            )
        }
        val featured = summary.featured?.takeIf { it.state == "needs_you" }
        val name = featured?.title?.takeIf(String::isNotBlank) ?: summary.headline.takeIf(String::isNotBlank)
        val link = if (summary.featured == null) DEEP_LINK else workItemLink(summary.featured.id, summary.deepLink)
        val builder = builder(app, NEEDS_YOU_CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(name ?: "Magician needs you")
            .setContentText(if (summary.needsYou > 1) "Needs you · ${summary.needsYou} waiting" else "Needs you")
            .setContentIntent(openIntent(app, link, NEEDS_YOU_NOTIFICATION_ID))
            .setAutoCancel(true)
            .setCategory(Notification.CATEGORY_REMINDER)
        if (featured != null) summary.featuredContext.takeIf(String::isNotBlank)?.let { builder.setSubText(it) }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            builder.setPriority(Notification.PRIORITY_HIGH)
            builder.setDefaults(Notification.DEFAULT_ALL)
        }
        if (featured != null && summary.actionKind == "approve_diff" && !summary.actionId.isNullOrBlank()) {
            builder.addAction(
                action("Approve", broadcast(app, WorkActions.ACTION_APPROVE, featured.id, summary.actionId, summary.deepLink), authenticate = true),
            )
        }
        runCatching { manager.notify(NEEDS_YOU_NOTIFICATION_ID, builder.build()) }
    }

    private fun ensureSummaryChannel(manager: NotificationManager) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Magician at work", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "What your runs, crews and tasks are doing, while any is going or waits on you."
                },
            )
        }
    }

    internal fun builder(app: Context, channel: String): Notification.Builder =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(app, channel)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(app)
        }

    internal fun openIntent(app: Context, link: String, requestCode: Int): PendingIntent = PendingIntent.getActivity(
        app, requestCode,
        Intent(Intent.ACTION_VIEW, Uri.parse(link))
            .setPackage(app.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** A button answered by the app's `WorkActionReceiver`; its own request code per action and item. */
    internal fun broadcast(app: Context, action: String, itemId: String?, actionId: String?, deepLink: String?): PendingIntent {
        val intent = Intent(action)
            .setPackage(app.packageName)
            .putExtra(WorkActions.EXTRA_ITEM_ID, itemId)
            .putExtra(WorkActions.EXTRA_ACTION_ID, actionId)
            .putExtra(WorkActions.EXTRA_DEEP_LINK, deepLink)
        return PendingIntent.getBroadcast(
            app, (action + itemId.orEmpty()).hashCode(), intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    internal fun action(label: String, intent: PendingIntent, authenticate: Boolean = false): Notification.Action {
        val builder = Notification.Action.Builder(null as android.graphics.drawable.Icon?, label, intent)
        // Approving from the lock screen requires unlocking the device.
        if (authenticate && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) builder.setAuthenticationRequired(true)
        return builder.build()
    }

    private fun cancel(app: Context, id: Int) {
        runCatching { app.getSystemService(NotificationManager::class.java)?.cancel(id) }
    }

    private fun dismiss(app: Context) {
        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putLong(ACTIVE_UNTIL, 0).apply()
        cancel(app, NOTIFICATION_ID)
    }
}
