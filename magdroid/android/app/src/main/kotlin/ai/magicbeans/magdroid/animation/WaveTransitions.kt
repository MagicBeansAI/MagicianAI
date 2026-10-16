package ai.magicbeans.magdroid.animation

import android.app.Activity
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.core.app.ActivityOptionsCompat

/**
 * Helper for applying wave-themed activity transitions
 */
object WaveTransitions {

    /**
     * Apply wave enter/exit transitions to an activity
     */
    fun applyWaveTransitions(activity: Activity, enterResId: Int, exitResId: Int) {
        activity.overrideOpen(enterResId, exitResId)
    }

    /**
     * Create activity options with wave animations
     */
    @RequiresApi(Build.VERSION_CODES.LOLLIPOP)
    fun createWaveTransitionOptions(activity: Activity): ActivityOptionsCompat {
        // For now, use basic fade animation
        // Can be enhanced with custom scene transitions
        return ActivityOptionsCompat.makeBasic()
    }

    /**
     * Apply wave entrance animation when activity starts
     */
    fun applyWaveEntrance(activity: Activity, waveEnterResId: Int) {
        activity.overrideOpen(waveEnterResId, 0)
    }

    /**
     * Apply wave exit animation when activity finishes
     */
    fun applyWaveExit(activity: Activity, waveExitResId: Int) {
        activity.overrideClose(0, waveExitResId)
    }
}

/**
 * Activity transitions, on both sides of API 34.
 *
 * `overridePendingTransition` was replaced by `overrideActivityTransition`,
 * which needs to be told whether the transition is an open or a close — the old
 * call inferred it from when it was made. That is why these are two functions
 * rather than one with a flag: the caller already knows which it is, and a
 * wrong guess plays the closing animation on the way in.
 */
internal fun Activity.overrideOpen(enterResId: Int, exitResId: Int) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        overrideActivityTransition(Activity.OVERRIDE_TRANSITION_OPEN, enterResId, exitResId)
    } else {
        @Suppress("DEPRECATION")
        overridePendingTransition(enterResId, exitResId)
    }
}

internal fun Activity.overrideClose(enterResId: Int, exitResId: Int) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        overrideActivityTransition(Activity.OVERRIDE_TRANSITION_CLOSE, enterResId, exitResId)
    } else {
        @Suppress("DEPRECATION")
        overridePendingTransition(enterResId, exitResId)
    }
}
