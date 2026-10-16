package ai.magicbeans.magdroid.bridge

import android.content.Context
import android.util.Log
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/**
 * Restarts the bridge after the OS has quietly stopped it.
 *
 * The client's own reconnect loop is coroutine `delay()`, and Doze and App
 * Standby suspend exactly that. A phone left on a desk overnight enters Doze,
 * the socket dies, the retry that would have fixed it never fires, and the
 * bridge is simply gone until someone opens the app. Nothing reports an error,
 * because from the client's point of view it is still waiting to retry.
 *
 * This is the same failure the browser extension hit: its `setInterval`
 * reconnect died with the MV3 service worker, which is why `background.js` needs
 * a `chrome.alarms` backstop. Different platform, identical shape — a timer
 * cannot be the thing that recovers from the timer being killed.
 *
 * WorkManager survives Doze, app death and reboot, so it is the one scheduler
 * that can. It only nudges: the client still owns connecting, and this only
 * asks it to try again.
 */
class BridgeWatchdogWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        val service = ai.magicbeans.magdroid.service.MagdroidAccessibilityService.instance
        if (service == null) {
            // The accessibility service is not running at all, which the owner
            // controls in system settings. Nothing to recover, and retrying
            // would just burn wakeups.
            Log.d(TAG, "Accessibility service not running; nothing to nudge")
            return Result.success()
        }
        if (!ai.magicbeans.magdroid.access.MagicianAccess.isConfigured(applicationContext)) {
            Log.d(TAG, "Bridge not configured; nothing to nudge")
            return Result.success()
        }
        if (service.isBridgeConnected()) {
            return Result.success()
        }
        Log.i(TAG, "Bridge is down after a quiet period; asking it to reconnect")
        service.restartMagicianBridge()
        return Result.success()
    }

    companion object {
        private const val TAG = "BridgeWatchdog"
        private const val WORK_NAME = "magdroid-bridge-watchdog"

        /**
         * Fifteen minutes is WorkManager's floor for periodic work.
         *
         * That is a long time to be disconnected, and it is the point: this is a
         * backstop for the case where the fast path is dead, not a substitute
         * for it. The client still reconnects in seconds when it is alive to do
         * so.
         */
        private const val INTERVAL_MINUTES = 15L

        fun schedule(context: Context) {
            val request = PeriodicWorkRequestBuilder<BridgeWatchdogWorker>(
                INTERVAL_MINUTES, TimeUnit.MINUTES,
            )
                // No point waking to reconnect with no network.
                .setConstraints(
                    Constraints.Builder()
                        .setRequiredNetworkType(NetworkType.CONNECTED)
                        .build(),
                )
                .build()

            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                WORK_NAME,
                // KEEP, not REPLACE: replacing on every service start resets the
                // period, so a service that restarts often would mean the
                // watchdog never actually runs.
                ExistingPeriodicWorkPolicy.KEEP,
                request,
            )
        }

        fun cancel(context: Context) {
            WorkManager.getInstance(context).cancelUniqueWork(WORK_NAME)
        }
    }
}
