package ai.magicbeans.magdroid.service

/**
 * Android 14 kills the process when [android.app.Service.startForeground]
 * posts a notification the user cannot see. The failure arrives later as
 * "Bad notification for startForeground", so a try/catch around the call does
 * not stop it. Callers skip the promotion and keep the accessibility
 * screenshot fallback.
 */
internal fun canPostForegroundServiceNotification(
    notificationsEnabled: Boolean,
    channelBlocked: Boolean,
): Boolean = notificationsEnabled && !channelBlocked
