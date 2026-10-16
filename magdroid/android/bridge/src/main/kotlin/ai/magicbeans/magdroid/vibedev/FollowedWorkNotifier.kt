package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.bridge.R
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build

/**
 * One followed item's state, from a `work_item` FCM data message (every value
 * a string) or from the item on a `/work/live` read.
 */
data class FollowedWorkUpdate(
    val itemId: String,
    /** `coding_run | crew | work_task`. */
    val itemKind: String,
    val state: String,
    val title: String,
    val context: String,
    val step: String,
    val actionKind: String?,
    val actionId: String?,
    val deepLink: String?,
    /** The item ended; the server drops the route after this. */
    val done: Boolean,
    val eventTimestamp: Long,
) {
    companion object {
        const val KIND = "work_item"

        fun fromPush(data: Map<String, String>): FollowedWorkUpdate? {
            if (data["kind"] != KIND) return null
            fun text(key: String) = data[key]?.trim().orEmpty()
            val id = text("item_id").takeIf(String::isNotEmpty) ?: return null
            return FollowedWorkUpdate(
                itemId = id,
                itemKind = text("item_kind"),
                state = text("state").ifEmpty { "working" },
                title = text("title"),
                context = text("context"),
                step = text("step"),
                actionKind = text("action_kind").ifEmpty { null },
                actionId = text("action_id").ifEmpty { null },
                deepLink = text("deep_link").ifEmpty { null },
                done = text("done").toBooleanStrictOrNull() ?: false,
                eventTimestamp = text("event_timestamp").toLongOrNull() ?: 0,
            )
        }

        fun fromItem(item: WorkItem, nowMs: Long) = FollowedWorkUpdate(
            itemId = item.id,
            itemKind = item.kind,
            state = item.state,
            title = item.title,
            context = item.context.orEmpty(),
            step = item.step.orEmpty(),
            actionKind = item.action?.kind?.takeIf(String::isNotBlank),
            actionId = item.action?.id?.takeIf(String::isNotBlank),
            deepLink = item.deepLink,
            done = workStateEnded(item.state),
            eventTimestamp = nowMs,
        )
    }
}

/** What a followed item's notification says, separated from posting it. */
data class FollowedWorkCard(
    val title: String,
    val text: String,
    val subText: String?,
    val ongoing: Boolean,
    val timeoutMs: Long,
    /** Waits on a diff approval this phone can answer. */
    val approve: Boolean,
)

/** An ended item's card stays this long, then takes itself down. */
const val FOLLOWED_ENDED_TIMEOUT_MS = 10 * 60_000L

/**
 * A live item's card outlives its last update this long. Followed items are
 * pushed on change, not on a heartbeat, so this is long; a foreground read
 * ([CodingRunsNotifier.reconcile]) settles one that missed its end sooner.
 */
const val FOLLOWED_ACTIVE_TIMEOUT_MS = 2 * 60 * 60_000L

fun followedWorkCard(update: FollowedWorkUpdate): FollowedWorkCard {
    val ended = update.done || workStateEnded(update.state)
    val text = when {
        ended -> if (workStateEnded(update.state)) workStateLabel(update.state) else "Finished"
        update.state == "needs_you" -> "Needs you"
        else -> update.step.ifBlank { workStateLabel(update.state) }
    }
    return FollowedWorkCard(
        title = update.title.ifBlank { workKindLabel(update.itemKind) },
        text = text,
        subText = listOf(workKindLabel(update.itemKind), update.context).filter(String::isNotBlank).joinToString(" · "),
        ongoing = !ended,
        timeoutMs = if (ended) FOLLOWED_ENDED_TIMEOUT_MS else FOLLOWED_ACTIVE_TIMEOUT_MS,
        approve = !ended && update.actionKind == "approve_diff" && !update.actionId.isNullOrBlank(),
    )
}

/**
 * One ongoing notification per followed item (at most [WorkFollows.MAX]),
 * tagged with the item id under one notification id and grouped with the
 * "Magician at work" summary. Updated silently; an ended item shows its final
 * state, stops being ongoing and dismisses itself after
 * [FOLLOWED_ENDED_TIMEOUT_MS]. Ordering per item is the push service's.
 */
object FollowedWorkNotifier {
    private const val PREFERENCES = "magican_work_followed_cards_v1"
    /** Ids whose ongoing card is up. */
    private const val SHOWN = "shown"
    const val CHANNEL_ID = "magician.work.followed"
    const val NOTIFICATION_ID = 4717
    private val lock = Any()

    /** Post or update one item's card; false when it was not shown (over the cap). */
    fun apply(context: Context, update: FollowedWorkUpdate): Boolean {
        val app = context.applicationContext
        val card = followedWorkCard(update)
        return synchronized(lock) {
            val shown = shown(app)
            if (card.ongoing && update.itemId !in shown && shown.size >= WorkFollows.MAX) return@synchronized false
            post(app, update, card)
            save(app, if (card.ongoing) shown + update.itemId else shown - update.itemId)
            true
        }
    }

    fun hasShown(context: Context): Boolean = shown(context.applicationContext).isNotEmpty()

    /**
     * A foreground `/work/live` read: refresh each card that is up from its
     * item, take down the ones no longer followed, and put up a card for a
     * followed item still going that has none (followed elsewhere, or a lost
     * push).
     */
    fun reconcile(context: Context, live: WorkLive) {
        val app = context.applicationContext
        val now = System.currentTimeMillis()
        val items = (listOfNotNull(live.featured) + live.next + live.items).associateBy { it.id }
        val shown = synchronized(lock) { shown(app) }
        shown.forEach { id ->
            val item = items[id]
            when {
                item != null -> apply(app, FollowedWorkUpdate.fromItem(item, now))
                id !in live.followed -> cancel(app, id)
            }
        }
        live.followed.filter { it !in shown }.forEach { id ->
            items[id]?.takeIf { !workStateEnded(it.state) }?.let { apply(app, FollowedWorkUpdate.fromItem(it, now)) }
        }
    }

    fun cancel(context: Context, itemId: String) {
        val app = context.applicationContext
        synchronized(lock) { save(app, shown(app) - itemId) }
        runCatching { app.getSystemService(NotificationManager::class.java)?.cancel(itemId, NOTIFICATION_ID) }
    }

    /** The active machine changed: every card belonged to the old one. */
    fun clearAll(context: Context) {
        val app = context.applicationContext
        val shown = synchronized(lock) { shown(app).also { save(app, emptySet()) } }
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        shown.forEach { runCatching { manager.cancel(it, NOTIFICATION_ID) } }
    }

    private fun shown(app: Context): Set<String> =
        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getStringSet(SHOWN, null).orEmpty().toSet()

    private fun save(app: Context, ids: Set<String>) {
        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putStringSet(SHOWN, ids).apply()
    }

    private fun post(app: Context, update: FollowedWorkUpdate, card: FollowedWorkCard) {
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Followed work", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "One card for each run, crew or task you follow, until it ends."
                },
            )
        }
        val link = workItemLink(update.itemId, update.deepLink)
        val requestCode = NOTIFICATION_ID + update.itemId.hashCode()
        val builder = CodingRunsNotifier.builder(app, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(card.title)
            .setContentText(card.text)
            .setContentIntent(CodingRunsNotifier.openIntent(app, link, requestCode))
            .setOngoing(card.ongoing)
            .setAutoCancel(!card.ongoing)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setGroup(CodingRunsNotifier.GROUP)
        card.subText?.takeIf(String::isNotBlank)?.let { builder.setSubText(it) }
        if (card.approve) {
            update.actionId?.let { approval ->
                builder.addAction(
                    CodingRunsNotifier.action(
                        "Approve",
                        CodingRunsNotifier.broadcast(app, WorkActions.ACTION_APPROVE, update.itemId, approval, update.deepLink),
                        authenticate = true,
                    ),
                )
            }
        }
        if (card.ongoing) {
            builder.addAction(
                CodingRunsNotifier.action(
                    "Unfollow",
                    CodingRunsNotifier.broadcast(app, WorkActions.ACTION_UNFOLLOW, update.itemId, null, update.deepLink),
                ),
            )
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) builder.setTimeoutAfter(card.timeoutMs)
        runCatching { manager.notify(update.itemId, NOTIFICATION_ID, builder.build()) }
    }
}
