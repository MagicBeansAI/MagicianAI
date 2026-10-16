package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.R
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.access.MagicianConnectionSnapshot
import ai.magicbeans.magdroid.bridge.R as BridgeR
import ai.magicbeans.magdroid.glance.MagicanGlanceFocus
import ai.magicbeans.magdroid.glance.MagicanGlanceSnapshot
import ai.magicbeans.magdroid.glance.MagicanGlanceStore
import ai.magicbeans.magdroid.tasks.TaskProgressNotifier
import ai.magicbeans.magdroid.vibedev.CodingRunsNotifier
import ai.magicbeans.magdroid.vibedev.CodingRunsSource
import ai.magicbeans.magdroid.vibedev.CodingRunsSummary
import ai.magicbeans.magdroid.vibedev.FollowedWorkNotifier
import ai.magicbeans.magdroid.vibedev.FollowedWorkUpdate
import ai.magicbeans.magdroid.vibedev.WorkFollows
import ai.magicbeans.magdroid.widget.MagicanGlanceWidget
import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.content.ContextCompat
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URLEncoder
import java.net.URL
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

/** Optional FCM seam. Builds without a deployment Firebase file remain valid. */
object AndroidMobilePushRegistration {
    private const val PREFERENCES = "magican_mobile_push_v1"
    private const val TOKEN = "fcm_token"
    private const val REGISTER_WORK = "magican-mobile-push-registration"
    private const val LAST_APPLICATION_ROUTE = "last_application_route"
    private const val LAST_APPLICATION_ROUTE_AT = "last_application_route_at"
    internal const val INPUT_OPERATION = "operation"
    internal const val INPUT_TASK_ID = "task_id"
    internal const val INPUT_REVISION = "revision"
    internal const val OP_REGISTER_APPLICATION = "register_application"
    internal const val OP_REGISTER_TASK = "register_task"
    internal const val OP_REMOVE_TASK = "remove_task"
    internal const val OP_REGISTER_CODING_RUNS = "register_coding_runs"
    internal const val OP_REMOVE_CODING_RUNS = "remove_coding_runs"
    internal const val OP_RETIRE_CODING_RUNS = "retire_coding_runs"
    internal const val OP_REGISTER_WORK_ITEM = "register_work_item"
    internal const val OP_REMOVE_WORK_ITEM = "remove_work_item"
    internal const val INPUT_ORIGIN = "origin"
    private const val CODING_RUNS_WORK = "magican-mobile-push-coding-runs"
    /** Before [CODING_RUNS_ROUTE_ORIGINS]: the one machine last registered with. */
    private const val CODING_RUNS_ROUTE_ORIGIN = "coding_runs_route_origin"
    /** Every machine that may hold this phone's `coding_runs` route. */
    private const val CODING_RUNS_ROUTE_ORIGINS = "coding_runs_route_origins"
    private const val CODING_RUNS_REFUSALS_PREFIX = "coding_runs_refusals_"
    /** The machine the coding-runs card and its push clock belong to. */
    private const val CODING_RUNS_MACHINE = "coding_runs_machine"
    private val machineWatch = AtomicBoolean(false)
    /** Best-effort route removals that must not wait for WorkManager. */
    private val cleanupScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * Make sure the server holds this phone's push route.
     *
     * Called on launch, on every foreground, after pairing / re-auth, when
     * notifications are granted, and from Settings' "Refresh registration".
     * Firebase only calls [MagicanMessagingService.onRegistered] when the
     * installation's registration changes, so a route the server dropped
     * (an unpair, a scope retirement, a provider rejection) was never sent
     * again while the id stayed the same. The stored id is now re-uploaded
     * here too — the PUT is an idempotent upsert — at most once per
     * [APPLICATION_ROUTE_REFRESH_MS] unless [force] (launch, pairing, an
     * explicit refresh) asks for it now.
     */
    fun ensure(context: Context, force: Boolean = false) {
        val app = context.applicationContext
        watchActiveMachine(app)
        // A switch this process has not handled yet re-registers in full.
        val forced = adoptCodingRunsMachine(app) || force
        if (!MagicianAccess.isConfigured(app) || FirebaseApp.getApps(app).isEmpty()) return
        // Reconcile the durable task leash even if Firebase keeps the existing
        // installation id and therefore has no fresh callback to deliver.
        TaskProgressNotifier.activeTaskId(app)
        // Registration completes asynchronously through onRegistered when the
        // id is new or changed. A throttled foreground check with an id in
        // hand skips the Firebase round trip and only re-sends the route.
        val stored = storedToken(app)
        if (forced || stored == null) runCatching { FirebaseMessaging.getInstance().register() }
        stored?.let { token -> reuploadApplicationRoute(app, token, forced) }
        retireStaleCodingRunsRoute(app)
    }

    /**
     * A `coding_runs` route left on a machine that is no longer active is
     * removed with that machine's own credential, while this phone still
     * holds it. Otherwise both machines push into the one card and fight.
     * A machine already forgotten has no credential left; nothing is sent
     * (forgetting one sends its removal first: [retireBeforeForget]).
     */
    internal fun retireStaleCodingRunsRoute(app: Context) {
        codingRunsRoutesToRetire(codingRunsRouteOrigins(app), MagicianAccess.activeOrigin(app))
            .forEach { enqueue(app, "$CODING_RUNS_WORK-retire-${digest(it)}", OP_RETIRE_CODING_RUNS, origin = it) }
    }

    @Synchronized
    internal fun codingRunsRouteOrigins(context: Context): Set<String> {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val legacy = preferences.getString(CODING_RUNS_ROUTE_ORIGIN, null)
        return (preferences.getStringSet(CODING_RUNS_ROUTE_ORIGINS, null).orEmpty() + listOfNotNull(legacy))
            .filterTo(mutableSetOf(), String::isNotBlank)
    }

    /**
     * Record that [origin] may hold this phone's route. Recorded before the
     * PUT, and kept per machine, so a route a cancelled worker or a switch
     * mid-PUT left behind is still found and retired. True when this call
     * added the record (it was not there before).
     */
    @Synchronized
    internal fun rememberCodingRunsRoute(context: Context, origin: String?): Boolean {
        if (origin.isNullOrBlank()) return false
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val origins = codingRunsRouteOrigins(context)
        preferences.edit()
            .putStringSet(CODING_RUNS_ROUTE_ORIGINS, origins + origin)
            .remove(CODING_RUNS_ROUTE_ORIGIN)
            .apply()
        return origin !in origins
    }

    /** Forget the route recorded on [origin]. */
    @Synchronized
    internal fun forgetCodingRunsRoute(context: Context, origin: String?) {
        if (origin == null) return
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val origins = codingRunsRouteOrigins(context)
        if (origin !in origins) return
        preferences.edit()
            .putStringSet(CODING_RUNS_ROUTE_ORIGINS, origins - origin)
            .remove(CODING_RUNS_ROUTE_ORIGIN)
            .remove("$CODING_RUNS_REFUSALS_PREFIX${digest(origin)}")
            .apply()
    }

    /**
     * The phone is about to forget [origin]. Its `coding_runs` route is
     * removed now, with a credential copied before the forget and held only
     * in memory (never in WorkManager data): afterwards nothing can authorize
     * the DELETE, and the machine would keep pushing its summary into the
     * card. The pushes carry no machine identity, so dropping them for a
     * window after a switch would also drop the new machine's; the DELETE is
     * the fix. Best effort: a failure (offline, the process dying) leaves the
     * route until that machine prunes it.
     */
    fun retireBeforeForget(context: Context, origin: String) {
        val app = context.applicationContext
        val snapshot = MagicianAccess.connectionSnapshot(app, origin)
        if (snapshot == null) {
            forgetCodingRunsRoute(app, origin)
            return
        }
        cleanupScope.launch {
            runCatching { removeCodingRuns(snapshot.baseUrl, snapshot.headers) }
            forgetCodingRunsRoute(app, origin)
        }
    }

    /**
     * Follow [MagicianAccess.activeMachine] for the whole process, as the
     * realtime hub and the task notifier do, so every switch (Settings, a QR
     * pairing, forgetting the active machine) resets the coding-runs card
     * and re-registers, not only the ones Settings makes.
     */
    fun watchActiveMachine(context: Context) {
        val app = context.applicationContext
        if (!machineWatch.compareAndSet(false, true)) return
        cleanupScope.launch {
            MagicianAccess.activeMachine.collect { onActiveMachineChanged(app) }
        }
    }

    /**
     * When the active machine is not the one the coding-runs card belongs
     * to, the card and its ordering clocks start over; true when they did.
     * Kept on disk, so a switch the process did not live to see still
     * resets on the next start.
     */
    @Synchronized
    private fun adoptCodingRunsMachine(app: Context): Boolean {
        val active = MagicianAccess.activeOrigin(app).orEmpty()
        val preferences = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val previous = preferences.getString(CODING_RUNS_MACHINE, null)
        if (previous == active) return false
        preferences.edit().putString(CODING_RUNS_MACHINE, active).apply()
        if (!isCodingRunsMachineSwitch(previous, active)) return false
        CodingRunsNotifier.reset(app)
        // Follows are per machine: the new one's list arrives on its next read.
        WorkFollows.reset(app)
        FollowedWorkNotifier.clearAll(app)
        MagicanMessagingService.forgetCodingRunsOrder(app)
        return true
    }

    /** Enqueue the application-route PUT unless the same route was sent recently. */
    private fun reuploadApplicationRoute(app: Context, token: String, force: Boolean) {
        val routeKey = "${MagicianAccess.baseUrl(app).trim().trimEnd('/')}|$token"
        val preferences = app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val now = System.currentTimeMillis()
        if (!shouldReuploadApplicationRoute(
                lastRouteKey = preferences.getString(LAST_APPLICATION_ROUTE, null),
                lastAtMs = preferences.getLong(LAST_APPLICATION_ROUTE_AT, 0),
                routeKey = routeKey,
                nowMs = now,
                force = force,
            )
        ) return
        preferences.edit()
            .putString(LAST_APPLICATION_ROUTE, routeKey)
            .putLong(LAST_APPLICATION_ROUTE_AT, now)
            .apply()
        enqueue(app, REGISTER_WORK, OP_REGISTER_APPLICATION)
        // The coding-runs summary rides its own route, refreshed with this one,
        // and so does each followed item's `work_item` route.
        if (CodingRunsNotifier.isEnabled(app)) enqueue(app, CODING_RUNS_WORK, OP_REGISTER_CODING_RUNS)
        WorkFollows.ids(app).forEach { enqueue(app, workItemWorkName(it), OP_REGISTER_WORK_ITEM, it) }
    }

    /**
     * "Show Magician at work in the notification shade". On registers the
     * `coding_runs` route so the server pushes the summary while the app is
     * closed; off removes it and takes the card down.
     */
    fun setCodingRunsEnabled(context: Context, enabled: Boolean) {
        val app = context.applicationContext
        CodingRunsNotifier.setEnabled(app, enabled)
        if (!MagicianAccess.isConfigured(app) || FirebaseApp.getApps(app).isEmpty()) return
        if (enabled) {
            if (storedToken(app) != null) enqueue(app, CODING_RUNS_WORK, OP_REGISTER_CODING_RUNS)
            else runCatching { FirebaseMessaging.getInstance().register() }
        } else {
            enqueue(app, CODING_RUNS_WORK, OP_REMOVE_CODING_RUNS)
        }
    }

    /**
     * The active Magician machine changed. Upload this phone's stored push
     * route to the newly active machine (Firebase keeps the same installation
     * id, so no fresh callback would arrive), and remove the previous
     * machine's `coding_runs` route with the previous machine's credential
     * ([retireStaleCodingRunsRoute], from [ensure]). The coding-runs card and
     * its ordering clocks belonged to the old machine, so they start over
     * ([adoptCodingRunsMachine]). Settings calls this directly and the
     * process-wide [watchActiveMachine] collector calls it for every switch.
     */
    fun onActiveMachineChanged(context: Context) {
        val app = context.applicationContext
        watchActiveMachine(app)
        // A credential change on the same machine is not a switch; the
        // collector and Settings both call here, and only the first resets.
        if (!adoptCodingRunsMachine(app)) return
        ensure(app, force = true)
    }

    fun acceptInstallationId(context: Context, installationId: String) {
        val app = context.applicationContext
        if (installationId.isBlank() || !MagicianAccess.isConfigured(app)) return
        app.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .edit()
            // Keep the existing key so upgrades do not strand an already
            // queued worker; the value is now an FCM-registered FID.
            .putString(TOKEN, installationId)
            .apply()
        reuploadApplicationRoute(app, installationId, force = true)
        TaskProgressNotifier.activeTaskId(app)?.let { taskID ->
            enqueue(app, taskWorkName(taskID), OP_REGISTER_TASK, taskID)
        }
    }

    /**
     * A followed item's `work_item` route: this phone's FCM id with the item
     * id in `task_id`. The server pushes the item's state on each change and
     * drops the route once it ends.
     */
    fun registerWorkItem(context: Context, itemID: String) {
        val app = context.applicationContext
        val normalized = itemID.trim()
        if (normalized.isEmpty() || !MagicianAccess.isConfigured(app) ||
            FirebaseApp.getApps(app).isEmpty()
        ) return
        // Without an id yet, acceptInstallationId registers every followed item.
        if (storedToken(app) != null) {
            enqueue(app, workItemWorkName(normalized), OP_REGISTER_WORK_ITEM, normalized)
        } else {
            runCatching { FirebaseMessaging.getInstance().register() }
        }
    }

    /** Unfollowed: drop the item's route (every generation of it). */
    fun unregisterWorkItem(context: Context, itemID: String) {
        val app = context.applicationContext
        val normalized = itemID.trim()
        if (normalized.isEmpty() || !MagicianAccess.isConfigured(app)) return
        enqueue(app, workItemWorkName(normalized), OP_REMOVE_WORK_ITEM, normalized)
    }

    fun registerTask(context: Context, taskID: String) {
        val app = context.applicationContext
        val normalized = taskID.trim()
        if (normalized.isEmpty() || !MagicianAccess.isConfigured(app) ||
            FirebaseApp.getApps(app).isEmpty()
        ) return
        storedToken(app)?.let {
            enqueue(app, taskWorkName(normalized), OP_REGISTER_TASK, normalized)
        }
        runCatching { FirebaseMessaging.getInstance().register() }
    }

    fun unregisterTask(context: Context, taskID: String) {
        val app = context.applicationContext
        val normalized = taskID.trim()
        if (normalized.isEmpty() || !MagicianAccess.isConfigured(app)) return
        // Deleting without the generation returned by PUT can erase a route
        // created by a newer lifecycle of the same task. If registration is
        // still in flight, its worker observes that the task is no longer
        // active and performs this exact-revision cleanup itself.
        val revision = storedTaskRevision(app, normalized) ?: return
        enqueue(app, taskWorkName(normalized), OP_REMOVE_TASK, normalized, revision)
    }

    private fun enqueue(
        context: Context,
        name: String,
        operation: String,
        taskID: String? = null,
        revision: Long? = null,
        origin: String? = null,
    ) {
        val values = mutableListOf<Pair<String, Any?>>(INPUT_OPERATION to operation)
        taskID?.let { values += INPUT_TASK_ID to it }
        revision?.let { values += INPUT_REVISION to it }
        origin?.let { values += INPUT_ORIGIN to it }
        val input = workDataOf(*values.toTypedArray())
        WorkManager.getInstance(context).enqueueUniqueWork(
            name,
            ExistingWorkPolicy.REPLACE,
            OneTimeWorkRequestBuilder<MobilePushRegistrationWorker>()
                .setInputData(input)
                .setConstraints(
                    Constraints.Builder()
                        .setRequiredNetworkType(NetworkType.CONNECTED)
                        .build(),
                )
                .build(),
        )
    }

    private fun taskWorkName(taskID: String): String = "magican-mobile-push-task-${digest(taskID)}"

    private fun workItemWorkName(itemID: String): String = "magican-mobile-push-work-item-${digest(itemID)}"

    private fun digest(value: String): String =
        MessageDigest.getInstance("SHA-256").digest(value.toByteArray()).take(12).joinToString("") { "%02x".format(it) }

    private fun taskRevisionKey(taskID: String): String = "task_revision:${taskWorkName(taskID)}"

    @Synchronized
    internal fun storedTaskRevision(context: Context, taskID: String): Long? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .getLong(taskRevisionKey(taskID), 0)
            .takeIf { it > 0 }

    @Synchronized
    internal fun rememberTaskRevision(context: Context, taskID: String, revision: Long) {
        if (revision <= 0) return
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val key = taskRevisionKey(taskID)
        if (shouldReplaceTaskRouteRevision(preferences.getLong(key, 0), revision)) {
            preferences.edit().putLong(key, revision).apply()
        }
    }

    @Synchronized
    internal fun forgetTaskRevision(
        context: Context,
        taskID: String,
        expectedRevision: Long,
    ) {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val key = taskRevisionKey(taskID)
        if (shouldClearTaskRouteRevision(preferences.getLong(key, 0), expectedRevision)) {
            preferences.edit().remove(key).apply()
        }
    }

    internal fun storedToken(context: Context): String? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .getString(TOKEN, null)
            ?.trim()
            ?.takeIf(String::isNotEmpty)

    internal suspend fun upload(
        context: Context,
        token: String,
        kind: String,
        taskID: String? = null,
        /** One read of the target machine; the active machine's when null. */
        machine: MagicianConnectionSnapshot? = null,
    ): Long = withContext(Dispatchers.IO) {
        val base = (machine?.baseUrl ?: MagicianAccess.baseUrl(context)).trimEnd('/')
        if (base.isBlank()) throw IllegalStateException("push registration has no runtime origin")
        val connection = URL("$base/api/magician/v2/devices/me/push")
            .openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "PUT"
            connection.connectTimeout = 12_000
            connection.readTimeout = 12_000
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            (machine?.headers ?: MagicianAccess.headers(context)).forEach(connection::setRequestProperty)
            val body = JSONObject()
                .put("platform", "fcm")
                .put("kind", kind)
                .put("environment", "production")
                .put("token", token)
                .apply { if (taskID != null) put("task_id", taskID) }
                .toString()
            connection.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
            val status = connection.responseCode
            if (status !in 200..299) {
                // Failure-body diagnostics must not change retry semantics. A
                // broken/empty error stream on a permanent 4xx is still a 4xx,
                // not an IOException that should wake WorkManager again.
                val errorBody = runCatching {
                    connection.errorStream
                        ?.bufferedReader()
                        ?.use { it.readText() }
                        .orEmpty()
                }.getOrDefault("")
                if (shouldRetryPushRegistration(status, errorBody)) {
                    throw IOException("push registration temporarily unavailable ($status)")
                }
                throw PushRegistrationRejected(status)
            }
            val responseBody = connection.inputStream.bufferedReader().use { it.readText() }
            JSONObject(responseBody).optLong("revision", 0).takeIf { it > 0 }
                ?: throw IOException("push registration response omitted its route revision")
        } finally {
            connection.disconnect()
        }
    }

    /** Drop this device's `coding_runs` route (every generation of it). */
    internal suspend fun removeCodingRuns(context: Context) {
        val (origin, snapshot) = MagicianAccess.activeConnection(context) ?: return
        removeCodingRuns(snapshot.baseUrl, snapshot.headers)
        forgetCodingRunsRoute(context, origin)
    }

    /**
     * Drop the `coding_runs` route this phone left on [origin], a machine
     * that is no longer active, with that machine's own credential. A
     * permanent refusal ([codingRunsRouteRemoval]) settles it too: the
     * record is forgotten, or every [ensure] would re-enqueue the same
     * DELETE for that machine to refuse again.
     */
    internal suspend fun retireCodingRuns(context: Context, origin: String) {
        val snapshot = MagicianAccess.connectionSnapshot(context, origin)
        if (snapshot != null) {
            try {
                removeCodingRuns(snapshot.baseUrl, snapshot.headers)
            } catch (refused: CodingRunsRemovalRefused) {
                // Kept (and asked again on a later ensure) until refused often
                // enough to be a real unpairing, not the wrong instance.
                if (noteCodingRunsRefusal(context, origin) < MAX_CODING_RUNS_REFUSALS) throw refused
            }
        }
        forgetCodingRunsRoute(context, origin)
    }

    /** Count one more refused retire for [origin]; returns the count. */
    @Synchronized
    internal fun noteCodingRunsRefusal(context: Context, origin: String): Int {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val key = "$CODING_RUNS_REFUSALS_PREFIX${digest(origin)}"
        val count = preferences.getInt(key, 0) + 1
        preferences.edit().putInt(key, count).apply()
        return count
    }

    private suspend fun removeCodingRuns(baseUrl: String, headers: Map<String, String>) = withContext(Dispatchers.IO) {
        val base = baseUrl.trim().trimEnd('/')
        if (base.isBlank()) return@withContext
        val connection = URL("$base/api/magician/v2/devices/me/push?$CODING_RUNS_ROUTE_REMOVAL_QUERY")
            .openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "DELETE"
            connection.connectTimeout = 12_000
            connection.readTimeout = 12_000
            headers.forEach(connection::setRequestProperty)
            val status = connection.responseCode
            when (codingRunsRouteRemoval(status)) {
                CodingRunsRouteRemoval.Done -> Unit
                CodingRunsRouteRemoval.Retry ->
                    throw IOException("push route removal temporarily unavailable ($status)")
                CodingRunsRouteRemoval.Refused -> throw CodingRunsRemovalRefused(status)
                CodingRunsRouteRemoval.Rejected ->
                    throw IllegalStateException("push route removal rejected ($status)")
            }
        } finally {
            connection.disconnect()
        }
    }

    internal suspend fun removeTask(context: Context, taskID: String, revision: Long) =
        withContext(Dispatchers.IO) {
            val base = MagicianAccess.baseUrl(context).trimEnd('/')
            if (base.isBlank()) return@withContext
            val connection = URL(
                "$base/api/magician/v2/devices/me/push?${taskRouteRemovalQuery(taskID, revision)}",
            ).openConnection() as HttpURLConnection
            try {
                connection.requestMethod = "DELETE"
                connection.connectTimeout = 12_000
                connection.readTimeout = 12_000
                MagicianAccess.headers(context).forEach(connection::setRequestProperty)
                val status = connection.responseCode
                if (status !in 200..299) {
                    if (status == 408 || status == 429 || status >= 500) {
                        throw IOException("push route removal temporarily unavailable ($status)")
                    }
                    throw IllegalStateException("push route removal rejected ($status)")
                }
                forgetTaskRevision(context, taskID, revision)
            } finally {
                connection.disconnect()
            }
        }

    /** Drop this device's `work_item` route for one item; a permanent 4xx counts as gone. */
    internal suspend fun removeWorkItem(context: Context, itemID: String): Unit =
        withContext(Dispatchers.IO) {
            val base = MagicianAccess.baseUrl(context).trim().trimEnd('/')
            if (base.isBlank()) return@withContext
            val connection = URL("$base/api/magician/v2/devices/me/push?${workItemRouteRemovalQuery(itemID)}")
                .openConnection() as HttpURLConnection
            try {
                connection.requestMethod = "DELETE"
                connection.connectTimeout = 12_000
                connection.readTimeout = 12_000
                MagicianAccess.headers(context).forEach(connection::setRequestProperty)
                val status = connection.responseCode
                if (status == 408 || status == 429 || status >= 500) {
                    throw IOException("push route removal temporarily unavailable ($status)")
                }
                Unit
            } finally {
                connection.disconnect()
            }
        }
}

class MobilePushRegistrationWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        if (!MagicianAccess.isConfigured(applicationContext)) return Result.success()
        val operation = inputData.getString(AndroidMobilePushRegistration.INPUT_OPERATION)
            ?: AndroidMobilePushRegistration.OP_REGISTER_APPLICATION
        val taskID = inputData.getString(AndroidMobilePushRegistration.INPUT_TASK_ID)
        val revision = inputData.getLong(AndroidMobilePushRegistration.INPUT_REVISION, 0)
            .takeIf { it > 0 }
        return try {
            when (operation) {
                AndroidMobilePushRegistration.OP_REGISTER_CODING_RUNS -> {
                    val token = AndroidMobilePushRegistration.storedToken(applicationContext)
                        ?: return Result.success()
                    // Turned off while this waited for a network: remove instead.
                    if (CodingRunsNotifier.isEnabled(applicationContext)) {
                        // One read names both the machine and its credential,
                        // and the target is recorded before the PUT: a worker
                        // cancelled after the server stored the route (REPLACE
                        // on a switch) or a switch mid-PUT still leaves a
                        // record for the retire to find.
                        val (origin, connection) = MagicianAccess.activeConnection(applicationContext)
                            ?: return Result.success()
                        val recorded = AndroidMobilePushRegistration.rememberCodingRunsRoute(applicationContext, origin)
                        try {
                            AndroidMobilePushRegistration.upload(
                                applicationContext, token, CodingRunsSummary.KIND, machine = connection,
                            )
                        } catch (rejected: PushRegistrationRejected) {
                            // Refused outright, so nothing was stored there: the
                            // record this attempt added would only send a DELETE
                            // that machine refuses too. One an earlier PUT left
                            // stays; its retire settles a refusal itself.
                            if (recorded) AndroidMobilePushRegistration.forgetCodingRunsRoute(applicationContext, origin)
                            throw rejected
                        } finally {
                            // Switched while this ran (cancelled or not): the
                            // retire goes after this PUT, not before it.
                            if (origin != MagicianAccess.activeOrigin(applicationContext)) {
                                AndroidMobilePushRegistration.retireStaleCodingRunsRoute(applicationContext)
                            }
                        }
                    } else {
                        AndroidMobilePushRegistration.removeCodingRuns(applicationContext)
                    }
                }
                AndroidMobilePushRegistration.OP_REMOVE_CODING_RUNS -> {
                    if (!CodingRunsNotifier.isEnabled(applicationContext)) {
                        AndroidMobilePushRegistration.removeCodingRuns(applicationContext)
                    }
                }
                AndroidMobilePushRegistration.OP_RETIRE_CODING_RUNS -> {
                    val origin = inputData.getString(AndroidMobilePushRegistration.INPUT_ORIGIN)
                    // Switched back before this ran: the route is current again.
                    if (!origin.isNullOrBlank() &&
                        shouldRetireCodingRunsRoute(origin, MagicianAccess.activeOrigin(applicationContext))
                    ) {
                        AndroidMobilePushRegistration.retireCodingRuns(applicationContext, origin)
                    }
                }
                AndroidMobilePushRegistration.OP_REGISTER_WORK_ITEM -> {
                    val token = AndroidMobilePushRegistration.storedToken(applicationContext)
                        ?: return Result.success()
                    val itemID = taskID?.takeIf(String::isNotBlank) ?: return Result.failure()
                    // Unfollowed while this waited for a network: remove instead.
                    if (WorkFollows.isFollowed(applicationContext, itemID)) {
                        AndroidMobilePushRegistration.upload(applicationContext, token, FollowedWorkUpdate.KIND, itemID)
                    } else {
                        AndroidMobilePushRegistration.removeWorkItem(applicationContext, itemID)
                    }
                }
                AndroidMobilePushRegistration.OP_REMOVE_WORK_ITEM -> {
                    val itemID = taskID?.takeIf(String::isNotBlank) ?: return Result.success()
                    if (!WorkFollows.isFollowed(applicationContext, itemID)) {
                        AndroidMobilePushRegistration.removeWorkItem(applicationContext, itemID)
                    }
                }
                AndroidMobilePushRegistration.OP_REMOVE_TASK -> {
                    if (!taskID.isNullOrBlank() && revision != null) {
                        AndroidMobilePushRegistration.removeTask(
                            applicationContext,
                            taskID,
                            revision,
                        )
                    }
                }
                AndroidMobilePushRegistration.OP_REGISTER_TASK,
                AndroidMobilePushRegistration.OP_REGISTER_APPLICATION -> {
                    val token = AndroidMobilePushRegistration.storedToken(applicationContext)
                        ?: return Result.success()
                    val boundTaskID = if (
                        operation == AndroidMobilePushRegistration.OP_REGISTER_TASK
                    ) {
                        taskID?.takeIf(String::isNotBlank) ?: return Result.failure()
                    } else {
                        null
                    }
                    if (boundTaskID != null &&
                        TaskProgressNotifier.activeTaskId(applicationContext) != boundTaskID
                    ) {
                        // A prior attempt may have uploaded successfully and
                        // failed only during its exact cleanup. Resume removal
                        // from the durable revision instead of creating another
                        // route generation on every WorkManager retry.
                        AndroidMobilePushRegistration.storedTaskRevision(
                            applicationContext,
                            boundTaskID,
                        )?.let { orphanRevision ->
                            AndroidMobilePushRegistration.removeTask(
                                applicationContext,
                                boundTaskID,
                                orphanRevision,
                            )
                        }
                        return Result.success()
                    }
                    val registeredRevision = AndroidMobilePushRegistration.upload(
                        applicationContext,
                        token,
                        if (operation == AndroidMobilePushRegistration.OP_REGISTER_TASK) {
                            "task_activity"
                        } else {
                            "application"
                        },
                        boundTaskID,
                    )
                    if (boundTaskID != null) {
                        AndroidMobilePushRegistration.rememberTaskRevision(
                            applicationContext,
                            boundTaskID,
                            registeredRevision,
                        )
                        if (TaskProgressNotifier.activeTaskId(applicationContext) != boundTaskID) {
                            // The task settled while PUT was in flight. Remove
                            // exactly the generation this worker just created.
                            AndroidMobilePushRegistration.removeTask(
                                applicationContext,
                                boundTaskID,
                                registeredRevision,
                            )
                        }
                    }
                }
                else -> return Result.failure()
            }
            Result.success()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: IOException) {
            Result.retry()
        } catch (_: Exception) {
            Result.failure()
        }
    }
}

class MagicanMessagingService : FirebaseMessagingService() {
    override fun onRegistered(installationId: String) {
        AndroidMobilePushRegistration.acceptInstallationId(applicationContext, installationId)
    }

    /**
     * The legacy registration token rotated. This app registers by Firebase
     * Installation ID ([onRegistered]) and never uploads the legacy token —
     * mixing the two would overwrite the route the server sends to. A rotation
     * still means Firebase's view of this install changed, so re-register and
     * re-upload the stored route now.
     */
    @Deprecated("Firebase superseded this callback with onRegistered; kept as a re-registration trigger.")
    override fun onNewToken(token: String) {
        AndroidMobilePushRegistration.ensure(applicationContext, force = true)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        // Any push is a chance to take down ask notifications whose window
        // closed while nothing ran — below API 26 nothing else times them out.
        AttentionNotificationExpiry.sweep(this)
        val data = message.data
        val eventTimestamp = pushEventTimestamp(data) ?: return
        if (!acceptInOrder(data, eventTimestamp)) return
        var answerable = false
        when (data["kind"]) {
            "attention_requested" -> {
                val previous = MagicanGlanceStore.load(this)
                MagicanGlanceStore.save(
                    this,
                    previous.copy(
                        generatedAt = eventTimestamp,
                        focus = MagicanGlanceFocus.NeedsYou,
                        title = "Needs your input",
                        subtitle = "Open Attention to keep work moving",
                        needsYouCount = previous.needsYouCount.coerceAtLeast(1),
                        taskId = null,
                    ),
                )
                // The generic notification goes up first; an `answer_mode` ask
                // is then fetched and, when it is safe, re-posted answerable.
                if (data["notify"] == "true") answerable = postAttention(data) && data["answer_mode"] != null
            }

            // Answered on another surface, or its notification window closed
            // (the ask itself stays open in Attention): withdraw this ask's
            // notification so the phone never offers a settled or stale
            // question. `attention_expired` was dropped before it reached
            // here, so on API < 26 an expired ask stayed up indefinitely.
            "attention_resolved", "attention_expired" -> {
                val correlationID = data["correlation_id"].orEmpty()
                if (correlationID.isNotBlank()) {
                    getSystemService(NotificationManager::class.java)
                        ?.cancel(correlationID, ATTENTION_NOTIFICATION)
                    AttentionNotificationExpiry.forget(this, correlationID)
                }
            }

            // The "Magician at work" summary (runs, crews and Work tasks),
            // still on the `coding_runs` route: the ongoing card, a brief
            // just-ended card, or nothing once nothing is going.
            "coding_runs" -> {
                CodingRunsSummary.fromPush(data)?.let { CodingRunsNotifier.apply(this, it, CodingRunsSource.Push) }
                return
            }

            // One followed item: its own ongoing card until it ends.
            FollowedWorkUpdate.KIND -> {
                FollowedWorkUpdate.fromPush(data)?.let { FollowedWorkNotifier.apply(this, it) }
                return
            }

            "task_progress" -> {
                val taskID = data["task_id"].orEmpty()
                val title = data["title"].orEmpty().ifBlank { "Work in progress" }
                val status = data["status"].orEmpty().ifBlank { "Working…" }
                val done = data["done"]?.toBooleanStrictOrNull() ?: false
                val steps = data["step_count"]?.toIntOrNull()?.coerceAtLeast(0) ?: 0
                if (taskID.isBlank() || !TaskProgressNotifier.updateFromRemote(
                        this,
                        taskID,
                        title,
                        status,
                        steps,
                        done,
                        eventTimestamp,
                    )
                ) return
                val previous = MagicanGlanceStore.load(this)
                if (previous.focus != MagicanGlanceFocus.NeedsYou && !done) {
                    MagicanGlanceStore.save(
                        this,
                        MagicanGlanceSnapshot(
                            generatedAt = eventTimestamp,
                            focus = MagicanGlanceFocus.ActiveWork,
                            title = title,
                            subtitle = status,
                            needsYouCount = previous.needsYouCount,
                            activeWorkCount = previous.activeWorkCount.coerceAtLeast(1),
                            taskId = taskID.takeIf(String::isNotBlank),
                        ),
                    )
                }
            }
        }
        MagicanGlanceWidget.refreshVisible(this)
        if (shouldRequestCanonicalGlanceRefresh(data["kind"], data["done"])) {
            MagicanGlanceWidget.requestRefresh(this)
        }
        // Last, so the widget is not held behind the ask fetch. This runs on
        // FCM's worker thread inside its handling window.
        if (answerable) AttentionAnswerNotifications.enrich(this, data)
    }

    private fun acceptInOrder(
        data: Map<String, String>,
        timestamp: Long,
    ): Boolean = synchronized(pushOrderLock) {
        val kind = data["kind"]
        val key = pushOrderKey(kind, data["task_id"], data["correlation_id"], data["item_id"])
            ?: return@synchronized false
        val store = getSharedPreferences(PUSH_ORDER, Context.MODE_PRIVATE)
        val previous = store.getLong(key, 0)
        if (!isNewerPushTimestamp(previous, timestamp)) return@synchronized false
        val editor = store.edit().putLong(key, timestamp)
        if (kind == "task_progress") {
            // Ordering is per task: two concurrent runs must not suppress one
            // another. Keep the newest bounded journal entries as terminal
            // tombstones so a delayed update cannot resurrect completed work.
            store.all
                .asSequence()
                .filter { (candidate, value) ->
                    candidate.startsWith("task:") && candidate != key && value is Long
                }
                .sortedByDescending { (_, value) -> value as Long }
                .drop(127)
                .forEach { (candidate, _) -> editor.remove(candidate) }
        }
        if (kind == FollowedWorkUpdate.KIND) {
            // Per item too; followed items are few, so a short journal.
            store.all
                .asSequence()
                .filter { (candidate, value) ->
                    candidate.startsWith("work_item:") && candidate != key && value is Long
                }
                .sortedByDescending { (_, value) -> value as Long }
                .drop(31)
                .forEach { (candidate, _) -> editor.remove(candidate) }
        }
        editor.apply()
        true
    }

    /** Post the generic notification for an ask; true when it was posted. */
    private fun postAttention(data: Map<String, String>): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return false
        val manager = getSystemService(NotificationManager::class.java) ?: return false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    ATTENTION_CHANNEL,
                    "Needs your attention",
                    NotificationManager.IMPORTANCE_HIGH,
                ).apply {
                    description = "Questions or approvals waiting for you in Magican."
                },
            )
        }
        val deepLink = data["deep_link"] ?: "magican://attention"
        // One notification per ask: tagged with its correlation id, so a
        // newer ask no longer replaces an older one and `attention_resolved`
        // can cancel exactly this one. The request code follows the tag so
        // each notification keeps its own deep link.
        val correlationID = data["correlation_id"].orEmpty()
        val open = PendingIntent.getActivity(
            this,
            7710 + correlationID.hashCode(),
            Intent(Intent.ACTION_VIEW, Uri.parse(deepLink))
                .setPackage(packageName)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, ATTENTION_CHANNEL)
        } else {
            @Suppress("DEPRECATION") Notification.Builder(this)
        }
        builder
            .setSmallIcon(BridgeR.drawable.ic_notification)
            .setContentTitle("Magican needs your input")
            .setContentText("Open Attention to keep your work moving.")
            .setContentIntent(open)
            .setAutoCancel(true)
        // Time-bounded: the ask's notification dismisses itself at
        // `expires_at` (its deadline, else the server's TTL) so unanswered
        // asks do not pile up; the ask stays open in Attention. One already
        // past it is not posted at all.
        val remaining = attentionTimeoutMillis(data["expires_at"], System.currentTimeMillis())
        if (remaining == 0L) return false
        if (remaining != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            builder.setTimeoutAfter(remaining)
        }
        // Remembered on every API level: below 26 there is no timeout, so the
        // sweep on the next push or app start is what takes it down.
        if (remaining != null && correlationID.isNotBlank()) {
            data["expires_at"]?.toLongOrNull()?.let {
                AttentionNotificationExpiry.remember(this, correlationID, it)
            }
        }
        // A plain confirmation ask can be answered from the notification.
        val source = data["source"].orEmpty()
        if (data["actions"] == "approve_reject" && correlationID.isNotBlank() && source.isNotBlank()) {
            builder.addAction(attentionAction(correlationID, source, deepLink, AttentionActionReceiver.ACTION_APPROVE, "Approve"))
            builder.addAction(attentionAction(correlationID, source, deepLink, AttentionActionReceiver.ACTION_REJECT, "Reject"))
        }
        manager.notify(correlationID.ifBlank { null }, ATTENTION_NOTIFICATION, builder.build())
        return true
    }

    private fun attentionAction(
        correlationID: String,
        source: String,
        deepLink: String,
        action: String,
        title: String,
    ): Notification.Action {
        val intent = Intent(this, AttentionActionReceiver::class.java)
            .setAction(action)
            .putExtra(AttentionActionReceiver.EXTRA_CORRELATION_ID, correlationID)
            .putExtra(AttentionActionReceiver.EXTRA_SOURCE, source)
            .putExtra(AttentionActionReceiver.EXTRA_DEEP_LINK, deepLink)
        // Request code per ask and per button, so each keeps its own extras.
        val pending = PendingIntent.getBroadcast(
            this,
            (correlationID + action).hashCode(),
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = Notification.Action.Builder(null as android.graphics.drawable.Icon?, title, pending)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            // Answering from the lock screen requires unlocking the device.
            builder.setAuthenticationRequired(true)
        }
        return builder.build()
    }

    companion object {
        private val pushOrderLock = Any()
        private const val PUSH_ORDER = "magican_push_order_v1"

        /** The next machine's coding-runs pushes owe nothing to the last one's clock. */
        internal fun forgetCodingRunsOrder(context: Context) = synchronized(pushOrderLock) {
            val store = context.getSharedPreferences(PUSH_ORDER, Context.MODE_PRIVATE)
            val editor = store.edit().remove(pushOrderKey(CodingRunsSummary.KIND, null))
            store.all.keys.filter { it.startsWith("work_item:") }.forEach { editor.remove(it) }
            editor.apply()
        }
        private const val ATTENTION_CHANNEL = "magican.attention"
        private const val ATTENTION_NOTIFICATION = 7711
        /** The id every attention notification uses (tagged per ask). */
        internal const val ATTENTION_NOTIFICATION_ID = ATTENTION_NOTIFICATION
        /** The channel every attention notification posts on. */
        internal const val ATTENTION_CHANNEL_ID = ATTENTION_CHANNEL
    }
}

/**
 * When each posted ask notification stops being worth showing.
 *
 * `setTimeoutAfter` exists only from API 26, and the backend's silent
 * `attention_expired` can be throttled or lost. Each posted ask's `expires_at`
 * is kept here by its notification tag, and [sweep] (on every push and on app
 * start) cancels the ones whose time has passed. The asks stay in Attention.
 */
internal object AttentionNotificationExpiry {
    private const val PREFERENCES = "magican_attention_expiry_v1"

    @Synchronized
    fun remember(context: Context, tag: String, expiresAtMillis: Long) {
        if (tag.isBlank() || expiresAtMillis <= 0) return
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .edit().putLong(tag, expiresAtMillis).apply()
    }

    @Synchronized
    fun forget(context: Context, tag: String) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
            .edit().remove(tag).apply()
    }

    @Synchronized
    fun sweep(context: Context, nowMillis: Long = System.currentTimeMillis()) {
        val store = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val stored = store.all.mapNotNull { (tag, value) -> (value as? Long)?.let { tag to it } }.toMap()
        val expired = expiredAttentionTags(stored, nowMillis)
        if (expired.isEmpty()) return
        val manager = context.getSystemService(NotificationManager::class.java)
        val editor = store.edit()
        expired.forEach { tag ->
            manager?.cancel(tag, MagicanMessagingService.ATTENTION_NOTIFICATION_ID)
            editor.remove(tag)
        }
        editor.apply()
    }
}

/** The tags whose stored expiry has passed. */
internal fun expiredAttentionTags(stored: Map<String, Long>, nowMillis: Long): List<String> =
    stored.filter { (tag, expiresAt) -> tag.isNotBlank() && expiresAt in 1..nowMillis }.keys.sorted()

internal fun isNewerPushTimestamp(previous: Long, candidate: Long): Boolean =
    candidate > 0 && candidate > previous

/** Milliseconds an ask's notification has left: `null` when the push names
 * no expiry, `0` when it is already past it. */
internal fun attentionTimeoutMillis(expiresAt: String?, nowMillis: Long): Long? {
    val expiry = expiresAt?.toLongOrNull()?.takeIf { it > 0 } ?: return null
    return (expiry - nowMillis).coerceAtLeast(0)
}

internal fun pushEventTimestamp(data: Map<String, String>): Long? =
    data["event_timestamp"]?.toLongOrNull()?.takeIf { it > 0 }

/**
 * The ordering stream a push belongs to. Attention is ordered per ask: one key
 * for every ask let an older `attention_resolved` of ask A be dropped as stale
 * behind a newer `attention_requested` of ask B, leaving A's notification up.
 * `attention_expired` shares its ask's lane; returning null for it dropped the
 * push before it was read.
 */
internal fun pushOrderKey(kind: String?, taskID: String?, correlationID: String? = null, itemID: String? = null): String? =
    when (kind) {
        "attention_requested", "attention_resolved", "attention_expired" ->
            correlationID?.trim()?.takeIf(String::isNotEmpty)?.let { "attention:$it" } ?: "attention"
        "task_progress" -> taskID?.trim()?.takeIf(String::isNotEmpty)?.let { "task:$it" }
        "coding_runs" -> "coding_runs"
        // A followed item is ordered on its own: one item's update never
        // suppresses another's.
        "work_item" -> itemID?.trim()?.takeIf(String::isNotEmpty)?.let { "work_item:$it" }
        else -> null
    }

/** A host without provider credentials is a deployment state, not a transient
 * network failure. Retrying it forever makes WorkManager wake the phone for a
 * route the current server process can never deliver. */
internal fun shouldRetryPushRegistration(status: Int, errorBody: String): Boolean =
    (status == 408 || status == 429 || status >= 500) &&
        !errorBody.contains("mobile_push_provider_not_configured")

internal fun shouldRequestCanonicalGlanceRefresh(kind: String?, done: String?): Boolean =
    kind == "attention_requested" || kind == "attention_resolved" ||
        (kind == "task_progress" && done == "true")

/** Every generation of this device's coding-runs route. */
internal const val CODING_RUNS_ROUTE_REMOVAL_QUERY = "kind=coding_runs"

/** The machine refused a push-route PUT outright; retrying cannot change that. */
internal class PushRegistrationRejected(val status: Int) :
    IllegalStateException("push registration rejected ($status)")

/** What a `coding_runs` route DELETE's status means for the phone's record of it. */
internal enum class CodingRunsRouteRemoval {
    /** Removed, or the machine will never accept it: forget the record. */
    Done,
    /** Timeout, throttle or server error: try again later. */
    Retry,
    /**
     * Not authorized (401, 403). Usually the machine unpaired this phone, but
     * behind a tunnel shared by two instances the other one answers this way
     * while the real machine still holds the route: asked again a few times
     * ([MAX_CODING_RUNS_REFUSALS]) before the record is let go.
     */
    Refused,
    /** Anything else (a redirect the connection did not follow): a failure. */
    Rejected,
}

/** Times a retire may be refused (401/403) before its record is forgotten. */
internal const val MAX_CODING_RUNS_REFUSALS = 5

/** A `coding_runs` route DELETE refused with 401/403. */
internal class CodingRunsRemovalRefused(val status: Int) :
    IllegalStateException("push route removal refused ($status)")

/**
 * Other than 401/403 ([CodingRunsRouteRemoval.Refused]), a 4xx besides 408
 * and 429 is permanent: the machine has no such route or endpoint (404, 410)
 * or is an older server that refuses `kind=coding_runs` (400). Retrying cannot
 * succeed, so the removal counts as done and its record is forgotten.
 */
internal fun codingRunsRouteRemoval(status: Int): CodingRunsRouteRemoval = when {
    status in 200..299 -> CodingRunsRouteRemoval.Done
    status == 408 || status == 429 || status >= 500 -> CodingRunsRouteRemoval.Retry
    status == 401 || status == 403 -> CodingRunsRouteRemoval.Refused
    status in 400..499 -> CodingRunsRouteRemoval.Done
    else -> CodingRunsRouteRemoval.Rejected
}

/**
 * Whether the coding-runs route recorded on [routeOrigin] belongs to a machine
 * other than the active one and must be removed there.
 */
internal fun shouldRetireCodingRunsRoute(routeOrigin: String?, activeOrigin: String?): Boolean =
    !routeOrigin.isNullOrBlank() && routeOrigin != activeOrigin

/** Every recorded route on a machine other than the active one, sorted. */
internal fun codingRunsRoutesToRetire(recorded: Collection<String>, activeOrigin: String?): List<String> =
    recorded.filter { shouldRetireCodingRunsRoute(it, activeOrigin) }.sorted()

/**
 * Whether the coding-runs card, last kept for [previous], now belongs to a
 * different machine ("" for none). No record yet (first run with it) is not
 * a switch: the card is the current machine's.
 */
internal fun isCodingRunsMachineSwitch(previous: String?, active: String): Boolean =
    previous != null && previous != active

internal fun workItemRouteRemovalQuery(itemID: String): String =
    "kind=work_item&task_id=${URLEncoder.encode(itemID, StandardCharsets.UTF_8.name())}"

internal fun taskRouteRemovalQuery(taskID: String, revision: Long): String =
    "kind=task_activity&task_id=${
        URLEncoder.encode(taskID, StandardCharsets.UTF_8.name())
    }&revision=$revision"

internal fun shouldReplaceTaskRouteRevision(current: Long, incoming: Long): Boolean =
    incoming > 0 && incoming > current

internal fun shouldClearTaskRouteRevision(current: Long, expected: Long): Boolean =
    expected > 0 && current == expected

/** How often a foregrounded app re-sends an unchanged application route. */
internal const val APPLICATION_ROUTE_REFRESH_MS = 15 * 60 * 1_000L

/**
 * Whether to re-PUT the application push route. Always when forced, when the
 * route (host + id) differs from the last one sent, or once the last send is
 * older than [intervalMs] (a clock that moved backwards counts as stale).
 */
internal fun shouldReuploadApplicationRoute(
    lastRouteKey: String?,
    lastAtMs: Long,
    routeKey: String,
    nowMs: Long,
    force: Boolean,
    intervalMs: Long = APPLICATION_ROUTE_REFRESH_MS,
): Boolean = force || lastRouteKey != routeKey || lastAtMs <= 0 || nowMs < lastAtMs || nowMs - lastAtMs >= intervalMs
