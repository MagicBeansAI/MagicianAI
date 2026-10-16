package ai.magicbeans.magdroid.push

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

/** What Settings should do for the notification grant remote alerts need. */
internal enum class NotificationPrompt {
    /** Alerts can be posted. Settings refreshes the FCM registration. */
    Granted,

    /** Android will still show the runtime dialog. */
    Request,

    /** The runtime dialog will not appear. Settings opens the system page. */
    OpenSettings,
}

/**
 * Remembers that this install has already been through the notification
 * dialog. The key matches Observe's permission memory so one answer is shared.
 */
internal object NotificationPermissionMemory {
    const val PERMISSION = "android.permission.POST_NOTIFICATIONS"
    private const val STORE = "magdroid.observe"

    fun asked(context: Context): Boolean = runCatching {
        context.getSharedPreferences(STORE, Context.MODE_PRIVATE)
            .getBoolean("asked:$PERMISSION", false)
    }.getOrDefault(false)

    fun markAsked(context: Context) {
        runCatching {
            context.getSharedPreferences(STORE, Context.MODE_PRIVATE)
                .edit()
                .putBoolean("asked:$PERMISSION", true)
                .apply()
        }
    }
}

internal fun notificationsAllowed(context: Context): Boolean {
    if (!NotificationManagerCompat.from(context).areNotificationsEnabled()) return false
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return true
    return ContextCompat.checkSelfPermission(
        context,
        NotificationPermissionMemory.PERMISSION,
    ) == android.content.pm.PackageManager.PERMISSION_GRANTED
}

internal fun Context.hostActivity(): Activity? {
    var current: Context? = this
    while (current is ContextWrapper) {
        if (current is Activity) return current
        current = current.baseContext
    }
    return null
}

/**
 * Launch asks only before the owner has answered. A later denial is offered
 * again from Settings, which still uses the dialog while Android allows it.
 */
internal fun shouldRequestNotificationPermissionOnLaunch(
    granted: Boolean,
    runtimePromptAvailable: Boolean,
    askedBefore: Boolean,
): Boolean = runtimePromptAvailable && !granted && !askedBefore

internal fun notificationPrompt(
    granted: Boolean,
    runtimePromptAvailable: Boolean,
    askedBefore: Boolean,
    showRationale: Boolean,
): NotificationPrompt = when {
    granted -> NotificationPrompt.Granted
    runtimePromptAvailable && (!askedBefore || showRationale) -> NotificationPrompt.Request
    else -> NotificationPrompt.OpenSettings
}

internal fun notificationPromptFor(context: Context): NotificationPrompt {
    val activity = context.hostActivity()
    val rationale = activity?.let {
        ActivityCompat.shouldShowRequestPermissionRationale(
            it,
            NotificationPermissionMemory.PERMISSION,
        )
    } ?: false
    return notificationPrompt(
        granted = notificationsAllowed(context),
        runtimePromptAvailable = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU,
        askedBefore = NotificationPermissionMemory.asked(context),
        showRationale = rationale,
    )
}

internal fun openNotificationSettings(context: Context) {
    val opened = runCatching {
        context.startActivity(
            Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }.isSuccess
    if (opened) return
    runCatching {
        context.startActivity(
            Intent(
                Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                Uri.fromParts("package", context.packageName, null),
            ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    }
}
