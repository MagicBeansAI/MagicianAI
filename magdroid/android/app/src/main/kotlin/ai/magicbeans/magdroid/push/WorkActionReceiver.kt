package ai.magicbeans.magdroid.push

import ai.magicbeans.magdroid.vibedev.WorkActions
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/**
 * App-layer adapter for the "Magician at work" notifications and follows:
 * Approve / Follow / Unfollow buttons on the summary and followed-item cards,
 * and the bridge's requests for a followed item's `work_item` push route.
 * Explicit-package broadcasts only (the receiver is not exported).
 */
class WorkActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val app = context.applicationContext
        val itemID = intent.getStringExtra(WorkActions.EXTRA_ITEM_ID)?.trim().orEmpty()
        val deepLink = intent.getStringExtra(WorkActions.EXTRA_DEEP_LINK)
        when (intent.action) {
            WorkActions.ACTION_REGISTER_ROUTE ->
                if (itemID.isNotEmpty()) AndroidMobilePushRegistration.registerWorkItem(app, itemID)
            WorkActions.ACTION_REMOVE_ROUTE ->
                if (itemID.isNotEmpty()) AndroidMobilePushRegistration.unregisterWorkItem(app, itemID)
            WorkActions.ACTION_APPROVE -> {
                val approvalID = intent.getStringExtra(WorkActions.EXTRA_ACTION_ID)?.trim().orEmpty()
                if (approvalID.isEmpty()) return
                runAsync { WorkActions.approve(app, approvalID, itemID.ifEmpty { null }, deepLink) }
            }
            WorkActions.ACTION_FOLLOW ->
                if (itemID.isNotEmpty()) runAsync { WorkActions.follow(app, itemID, deepLink) }
            WorkActions.ACTION_UNFOLLOW ->
                if (itemID.isNotEmpty()) runAsync { WorkActions.unfollow(app, itemID) }
        }
    }

    /** Run [block] past onReceive, inside the broadcast's handling window. */
    private fun runAsync(block: suspend () -> Unit) {
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                block()
            } finally {
                pending.finish()
            }
        }
    }
}
