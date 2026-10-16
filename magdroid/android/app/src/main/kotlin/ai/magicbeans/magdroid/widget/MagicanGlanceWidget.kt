package ai.magicbeans.magdroid.widget

import ai.magicbeans.magdroid.R
import ai.magicbeans.magdroid.glance.MagicanGlanceFocus
import ai.magicbeans.magdroid.glance.MagicanGlanceSnapshot
import ai.magicbeans.magdroid.glance.MagicanGlanceStore
import ai.magicbeans.magdroid.today.TodayRepository
import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.widget.RemoteViews
import androidx.work.CoroutineWorker
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.CancellationException
import java.util.concurrent.TimeUnit

class MagicanGlanceWidget : AppWidgetProvider() {
    override fun onEnabled(context: Context) {
        super.onEnabled(context)
        schedulePeriodic(context)
        requestRefresh(context)
    }

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        ids.forEach { manager.updateAppWidget(it, render(context, MagicanGlanceStore.load(context))) }
        requestRefresh(context)
    }

    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        if (intent.action == ACTION_REFRESH) {
            refreshVisible(context)
        }
    }

    override fun onDisabled(context: Context) {
        super.onDisabled(context)
        WorkManager.getInstance(context).cancelUniqueWork(PERIODIC_WORK)
        WorkManager.getInstance(context).cancelUniqueWork(REFRESH_WORK)
    }

    companion object {
        const val ACTION_REFRESH = "ai.magicbeans.magdroid.action.REFRESH_GLANCE"
        private const val PERIODIC_WORK = "magican-glance-periodic"
        private const val REFRESH_WORK = "magican-glance-refresh"

        fun requestRefresh(context: Context) {
            WorkManager.getInstance(context).enqueueUniqueWork(
                REFRESH_WORK,
                ExistingWorkPolicy.KEEP,
                OneTimeWorkRequestBuilder<MagicanGlanceWorker>()
                    .setConstraints(networkConstraints())
                    .build(),
            )
        }

        fun refreshVisible(context: Context) {
            val app = context.applicationContext
            val manager = AppWidgetManager.getInstance(app)
            val component = ComponentName(app, MagicanGlanceWidget::class.java)
            val snapshot = MagicanGlanceStore.load(app)
            manager.getAppWidgetIds(component).forEach {
                manager.updateAppWidget(it, render(app, snapshot))
            }
        }

        private fun schedulePeriodic(context: Context) {
            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                PERIODIC_WORK,
                ExistingPeriodicWorkPolicy.KEEP,
                PeriodicWorkRequestBuilder<MagicanGlanceWorker>(15, TimeUnit.MINUTES)
                    .setConstraints(networkConstraints())
                    .build(),
            )
        }

        private fun networkConstraints(): Constraints = Constraints.Builder()
            .setRequiredNetworkType(NetworkType.CONNECTED)
            .build()

        private fun render(context: Context, snapshot: MagicanGlanceSnapshot): RemoteViews {
            val views = RemoteViews(context.packageName, R.layout.widget_magican_glance)
            views.setTextViewText(R.id.glanceTitle, snapshot.title)
            views.setTextViewText(R.id.glanceSubtitle, snapshot.subtitle)
            views.setTextViewText(
                R.id.glanceEyebrow,
                when (snapshot.focus) {
                    MagicanGlanceFocus.NeedsYou -> "${snapshot.needsYouCount} NEEDS YOU"
                    MagicanGlanceFocus.ActiveWork -> "${snapshot.activeWorkCount} ACTIVE"
                    MagicanGlanceFocus.Ready -> "MAGICAN"
                },
            )
            views.setImageViewResource(
                R.id.glanceStateIcon,
                when (snapshot.focus) {
                    MagicanGlanceFocus.NeedsYou -> android.R.drawable.ic_dialog_alert
                    MagicanGlanceFocus.ActiveWork -> android.R.drawable.stat_notify_sync
                    MagicanGlanceFocus.Ready -> android.R.drawable.ic_btn_speak_now
                },
            )
            views.setOnClickPendingIntent(R.id.glanceRoot, deepLink(context, snapshot.destination, 6101))
            views.setOnClickPendingIntent(R.id.glanceTalk, deepLink(context, "magican://talk", 6102))
            return views
        }

        private fun deepLink(context: Context, value: String, requestCode: Int): PendingIntent {
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse(value))
                .setPackage(context.packageName)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            return PendingIntent.getActivity(
                context,
                requestCode,
                intent,
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
        }
    }
}

class MagicanGlanceWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result = try {
        val today = withTimeout(15_000) { TodayRepository(applicationContext).today() }
        MagicanGlanceStore.save(applicationContext, MagicanGlanceSnapshot.reduce(today))
        MagicanGlanceWidget.refreshVisible(applicationContext)
        Result.success()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        // The last truthful snapshot is preferable to a fabricated empty day.
        if (shouldRetryGlanceRefresh(runAttemptCount)) Result.retry() else Result.success()
    }
}

internal fun shouldRetryGlanceRefresh(runAttemptCount: Int): Boolean = runAttemptCount == 0
