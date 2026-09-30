package ai.magicbeans.magdroid.tasks

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.firstOrNull
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * A running task's progress, as an ongoing notification — the Android shape
 * of iOS's task Live Activity. The notification is the live task surface;
 * the separate Home Screen widget remains a compact Today summary.
 *
 * Armed by the surfaces that dispatch a task and then leave the owner blind —
 * the keyboard is the canonical one: a task started from inside another app,
 * with no Magician screen anywhere to watch it. In-app dispatches are not
 * tracked here for the same reason iOS's Live Activity only arms on the
 * intent path: the task screen itself is already showing the run.
 *
 * One task at a time, like iOS's one `currentActivity`: a newer dispatch
 * supersedes the card of the older one rather than stacking a shade full of
 * spinners. Updates ride `ExecutionPanelDelta` off the shared realtime
 * socket, the settle signal is the overview's terminal status, and a
 * 20-minute watchdog (iOS's own cap) removes a card that can no longer be
 * trusted rather than leaving it claiming progress forever.
 */
object TaskProgressNotifier {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val stateLock = Any()
    private var tracking: Job? = null
    @Volatile private var trackedTaskId: String? = null

    /** One socket for the process, not one per dispatch. */
    @Volatile
    private var source: TasksDataSource? = null

    private fun events(app: Context): TasksDataSource = source ?: synchronized(stateLock) {
        source ?: TaskRepository(app).also { source = it }
    }

    /** Begin tracking one just-dispatched task. Safe to call from any thread. */
    fun track(context: Context, taskId: String, taskName: String) {
        val app = context.applicationContext
        synchronized(stateLock) {
            tracking?.cancel()
            val previousTaskId = activeTaskId(app)?.takeIf { it != taskId }
            trackedTaskId = taskId
            app.getSharedPreferences(ROUTE_PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .putString(ACTIVE_TASK_ID, taskId)
                .putLong(ACTIVE_TASK_STARTED_AT, System.currentTimeMillis())
                .putLong(ACTIVE_TASK_UPDATED_AT, 0)
                .apply()
            previousTaskId?.let { routeTask(app, it, register = false) }
            routeTask(app, taskId, register = true)
            post(app, taskId, TaskProgressCard(taskName, "Starting…", 0, done = false))
            tracking = scope.launch {
                val settled = withTimeoutOrNull(WATCHDOG_MS) {
                    events(app).taskEvents()
                        .mapNotNull { event ->
                            // The stream carries deltas for every run; only this
                            // task's move the card.
                            event.panel.takeIf {
                                event.eventType == "ExecutionPanelDelta" && event.taskId == taskId
                            }?.let { panel ->
                                taskProgressCard(panel, taskName)?.let { card ->
                                    event.eventTimestamp to card
                                }
                            }
                        }
                        .firstOrNull { (timestamp, card) ->
                            postIfTracked(app, taskId, timestamp, card) && card.done
                        }
                }
                // The watchdog, not the task, ended this. A card nothing will
                // ever update again is not progress, it is a lie in the shade.
                if (settled == null) {
                    expireIfTracked(app, taskId)
                } else {
                    finishTrackedTask(app, taskId)
                }
            }
        }
    }

    /** Update the same truthful progress card from a server push while the
     * realtime socket or app process is not active. */
    fun updateFromRemote(
        context: Context,
        taskId: String,
        title: String,
        status: String,
        stepCount: Int,
        done: Boolean,
        eventTimestamp: Long,
    ): Boolean {
        val app = context.applicationContext
        return synchronized(stateLock) {
            if (!shouldApplyRemote(activeTaskId(app), taskId)) return@synchronized false
            if (!markTaskEventIfNewerLocked(app, eventTimestamp)) return@synchronized false
            post(app, taskId, TaskProgressCard(title, status, stepCount.coerceAtLeast(0), done))
            if (done) {
                // Remote terminal delivery is authoritative. Do not leave the
                // realtime collector and its watchdog alive for another twenty
                // minutes after the visible activity has already settled.
                tracking?.cancel()
                tracking = null
                if (clearTrackedTaskLocked(app, taskId)) {
                    routeTask(app, taskId, register = false)
                }
            }
            true
        }
    }

    internal fun shouldApplyRemote(activeTaskId: String?, eventTaskId: String): Boolean =
        !activeTaskId.isNullOrBlank() && activeTaskId == eventTaskId

    fun activeTaskId(context: Context): String? {
        val app = context.applicationContext
        val preferences = app.getSharedPreferences(ROUTE_PREFERENCES, Context.MODE_PRIVATE)
        val taskId = preferences
            .getString(ACTIVE_TASK_ID, null)
            ?.trim()
            ?.takeIf(String::isNotEmpty)
            ?: return null
        val now = System.currentTimeMillis()
        var startedAt = preferences.getLong(ACTIVE_TASK_STARTED_AT, 0)
        if (startedAt <= 0) {
            // One bounded migration window for an activity written by the
            // pre-timestamp build; do not strand a currently running task on
            // upgrade, and do not preserve it indefinitely either.
            startedAt = now
            preferences.edit().putLong(ACTIVE_TASK_STARTED_AT, startedAt).apply()
        }
        if (isTaskTrackingFresh(startedAt, now)) return taskId

        preferences.edit()
            .remove(ACTIVE_TASK_ID)
            .remove(ACTIVE_TASK_STARTED_AT)
            .remove(ACTIVE_TASK_UPDATED_AT)
            .apply()
        routeTask(app, taskId, register = false)
        dismiss(app)
        return null
    }

    private fun finishTrackedTask(app: Context, taskId: String) {
        synchronized(stateLock) {
            if (clearTrackedTaskLocked(app, taskId)) {
                routeTask(app, taskId, register = false)
            }
        }
    }

    /** Returns whether this call owned and removed the active server route. */
    private fun clearTrackedTaskLocked(app: Context, taskId: String): Boolean {
        var removedActiveRoute = false
        if (activeTaskId(app) == taskId) {
            app.getSharedPreferences(ROUTE_PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .remove(ACTIVE_TASK_ID)
                .remove(ACTIVE_TASK_STARTED_AT)
                .remove(ACTIVE_TASK_UPDATED_AT)
                .apply()
            removedActiveRoute = true
        }
        if (trackedTaskId == taskId) {
            trackedTaskId = null
            tracking = null
        }
        return removedActiveRoute
    }

    private fun postIfTracked(
        app: Context,
        taskId: String,
        eventTimestamp: Long,
        card: TaskProgressCard,
    ): Boolean = synchronized(stateLock) {
        if (!shouldApplyRemote(activeTaskId(app), taskId) ||
            !markTaskEventIfNewerLocked(app, eventTimestamp)
        ) {
            false
        } else {
            post(app, taskId, card)
            true
        }
    }

    private fun markTaskEventIfNewerLocked(app: Context, eventTimestamp: Long): Boolean {
        val preferences = app.getSharedPreferences(ROUTE_PREFERENCES, Context.MODE_PRIVATE)
        val previous = preferences.getLong(ACTIVE_TASK_UPDATED_AT, 0)
        if (!isNewerTaskEvent(previous, eventTimestamp)) return false
        preferences.edit().putLong(ACTIVE_TASK_UPDATED_AT, eventTimestamp).apply()
        return true
    }

    private fun expireIfTracked(app: Context, taskId: String) {
        synchronized(stateLock) {
            if (!shouldApplyRemote(activeTaskId(app), taskId)) return
            dismiss(app)
            if (clearTrackedTaskLocked(app, taskId)) {
                routeTask(app, taskId, register = false)
            }
        }
    }

    private fun routeTask(app: Context, taskId: String, register: Boolean) {
        app.sendBroadcast(
            Intent(if (register) ACTION_REGISTER_TASK_ROUTE else ACTION_REMOVE_TASK_ROUTE)
                .setPackage(app.packageName)
                .putExtra(EXTRA_TASK_ID, taskId),
        )
    }

    private fun post(app: Context, taskId: String, card: TaskProgressCard) {
        val manager = app.getSystemService(NotificationManager::class.java) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID, "Task progress",
                    // Low: a progress card should sit quietly in the shade,
                    // not buzz on every step of a run the owner delegated.
                    NotificationManager.IMPORTANCE_LOW,
                ).apply {
                    description = "Shown while a task Magician was handed is running."
                },
            )
        }
        // Tapping the card lands on the task itself, through the same deep
        // link the app shortcuts use.
        val open = PendingIntent.getActivity(
            app, 0,
            Intent(Intent.ACTION_VIEW, Uri.parse("magican://task/${Uri.encode(taskId)}"))
                .setPackage(app.packageName)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(app, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(app)
        }
        builder
            .setContentTitle(card.title)
            .setContentText(card.status)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentIntent(open)
            .setOngoing(!card.done)
            .setAutoCancel(card.done)
        if (card.stepCount > 0 && !card.done) {
            builder.setSubText("${card.stepCount} ${if (card.stepCount == 1) "step" else "steps"}")
        }
        runCatching { manager.notify(NOTIFICATION_ID, builder.build()) }
    }

    private fun dismiss(app: Context) {
        runCatching {
            app.getSystemService(NotificationManager::class.java)?.cancel(NOTIFICATION_ID)
        }
    }

    private const val CHANNEL_ID = "magician.task.progress"
    private const val NOTIFICATION_ID = 4714
    const val ACTION_REGISTER_TASK_ROUTE = "ai.magicbeans.magdroid.action.REGISTER_TASK_PUSH"
    const val ACTION_REMOVE_TASK_ROUTE = "ai.magicbeans.magdroid.action.REMOVE_TASK_PUSH"
    const val EXTRA_TASK_ID = "task_id"
    private const val ROUTE_PREFERENCES = "magican_task_progress_v1"
    private const val ACTIVE_TASK_ID = "active_task_id"
    private const val ACTIVE_TASK_STARTED_AT = "active_task_started_at"
    private const val ACTIVE_TASK_UPDATED_AT = "active_task_updated_at"

    /** iOS's watchdog cap, kept identical: a lost task ends the card, not the battery. */
    internal const val WATCHDOG_MS = 20 * 60_000L
}

internal fun isTaskTrackingFresh(startedAtMs: Long, nowMs: Long): Boolean =
    startedAtMs > 0 && startedAtMs <= nowMs + 60_000L && nowMs - startedAtMs <= TaskProgressNotifier.WATCHDOG_MS

internal fun isNewerTaskEvent(previousTimestamp: Long, candidateTimestamp: Long): Boolean =
    candidateTimestamp > 0 && candidateTimestamp > previousTimestamp
