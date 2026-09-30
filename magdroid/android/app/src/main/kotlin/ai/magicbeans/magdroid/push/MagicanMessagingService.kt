package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.R
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.bridge.R as BridgeR
import ai.magicbeans.magdroid.glance.MagicanGlanceFocus
import ai.magicbeans.magdroid.glance.MagicanGlanceSnapshot
import ai.magicbeans.magdroid.glance.MagicanGlanceStore
import ai.magicbeans.magdroid.tasks.TaskProgressNotifier
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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/** Optional FCM seam. Builds without a deployment Firebase file remain valid. */
object AndroidMobilePushRegistration {
    private const val PREFERENCES = "magican_mobile_push_v1"
    private const val TOKEN = "fcm_token"
    private const val REGISTER_WORK = "magican-mobile-push-registration"
    internal const val INPUT_OPERATION = "operation"
    internal const val INPUT_TASK_ID = "task_id"
    internal const val INPUT_REVISION = "revision"
    internal const val OP_REGISTER_APPLICATION = "register_application"
    internal const val OP_REGISTER_TASK = "register_task"
    internal const val OP_REMOVE_TASK = "remove_task"

    fun ensure(context: Context) {
        val app = context.applicationContext
        if (!MagicianAccess.isConfigured(app) || FirebaseApp.getApps(app).isEmpty()) return
        // Reconcile the durable task leash even if Firebase keeps the existing
        // installation id and therefore has no fresh callback to deliver.
        TaskProgressNotifier.activeTaskId(app)
        // Registration completes asynchronously through onRegistered. Calling
        // it on startup also recovers if a previous server upload failed.
        runCatching { FirebaseMessaging.getInstance().register() }
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
        enqueue(app, REGISTER_WORK, OP_REGISTER_APPLICATION)
        TaskProgressNotifier.activeTaskId(app)?.let { taskID ->
            enqueue(app, taskWorkName(taskID), OP_REGISTER_TASK, taskID)
        }
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
    ) {
        val values = mutableListOf<Pair<String, Any?>>(INPUT_OPERATION to operation)
        taskID?.let { values += INPUT_TASK_ID to it }
        revision?.let { values += INPUT_REVISION to it }
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

    private fun taskWorkName(taskID: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(taskID.toByteArray())
        return "magican-mobile-push-task-${digest.take(12).joinToString("") { "%02x".format(it) }}"
    }

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
    ): Long = withContext(Dispatchers.IO) {
        val base = MagicianAccess.baseUrl(context).trimEnd('/')
        if (base.isBlank()) throw IllegalStateException("push registration has no runtime origin")
        val connection = URL("$base/api/magician/v2/devices/me/push")
            .openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "PUT"
            connection.connectTimeout = 12_000
            connection.readTimeout = 12_000
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            MagicianAccess.headers(context).forEach(connection::setRequestProperty)
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
                throw IllegalStateException("push registration rejected ($status)")
            }
            val responseBody = connection.inputStream.bufferedReader().use { it.readText() }
            JSONObject(responseBody).optLong("revision", 0).takeIf { it > 0 }
                ?: throw IOException("push registration response omitted its route revision")
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

    override fun onMessageReceived(message: RemoteMessage) {
        val data = message.data
        val eventTimestamp = pushEventTimestamp(data) ?: return
        if (!acceptInOrder(data, eventTimestamp)) return
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
                if (data["notify"] == "true") postAttention(data)
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
    }

    private fun acceptInOrder(
        data: Map<String, String>,
        timestamp: Long,
    ): Boolean = synchronized(pushOrderLock) {
        val kind = data["kind"]
        val key = pushOrderKey(kind, data["task_id"]) ?: return@synchronized false
        val store = getSharedPreferences("magican_push_order_v1", Context.MODE_PRIVATE)
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
        editor.apply()
        true
    }

    private fun postAttention(data: Map<String, String>) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return
        val manager = getSystemService(NotificationManager::class.java) ?: return
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
        val open = PendingIntent.getActivity(
            this,
            7710,
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
        manager.notify(ATTENTION_NOTIFICATION, builder.build())
    }

    private companion object {
        val pushOrderLock = Any()
        const val ATTENTION_CHANNEL = "magican.attention"
        const val ATTENTION_NOTIFICATION = 7711
    }
}

internal fun isNewerPushTimestamp(previous: Long, candidate: Long): Boolean =
    candidate > 0 && candidate > previous

internal fun pushEventTimestamp(data: Map<String, String>): Long? =
    data["event_timestamp"]?.toLongOrNull()?.takeIf { it > 0 }

internal fun pushOrderKey(kind: String?, taskID: String?): String? = when (kind) {
    "attention_requested", "attention_resolved" -> "attention"
    "task_progress" -> taskID?.trim()?.takeIf(String::isNotEmpty)?.let { "task:$it" }
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

internal fun taskRouteRemovalQuery(taskID: String, revision: Long): String =
    "kind=task_activity&task_id=${
        URLEncoder.encode(taskID, StandardCharsets.UTF_8.name())
    }&revision=$revision"

internal fun shouldReplaceTaskRouteRevision(current: Long, incoming: Long): Boolean =
    incoming > 0 && incoming > current

internal fun shouldClearTaskRouteRevision(current: Long, expected: Long): Boolean =
    expected > 0 && current == expected
