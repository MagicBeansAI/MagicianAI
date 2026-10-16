package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.tasks.TaskProgressNotifier
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** App-layer adapter from the generic task tracker to the optional FCM seam. */
class TaskActivityPushRouteReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val taskID = intent.getStringExtra(TaskProgressNotifier.EXTRA_TASK_ID)
            ?.trim()
            ?.takeIf(String::isNotEmpty)
            ?: return
        when (intent.action) {
            TaskProgressNotifier.ACTION_REGISTER_TASK_ROUTE ->
                AndroidMobilePushRegistration.registerTask(context, taskID)
            TaskProgressNotifier.ACTION_REMOVE_TASK_ROUTE ->
                AndroidMobilePushRegistration.unregisterTask(context, taskID)
        }
    }
}
