package ai.magicbeans.magdroid.vibedev

import ai.magicbeans.magdroid.realtime.HubDelivery
import ai.magicbeans.magdroid.realtime.HubFilter
import ai.magicbeans.magdroid.realtime.RealtimeHub
import android.content.Context
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * The realtime events that can change a VibeDev screen: task lifecycle,
 * coding runs and asks (an approval or a crew question). Screens re-read
 * over REST after a short debounce rather than patching from frames, as the
 * web stores do; a resync (the hub may have missed frames) re-reads too.
 */
object VibeDevLive {
    val FILTER = HubFilter(eventTypes = listOf("Task", "task.", "execution.", "delegation.results_ready",
        "coding.", "hitl.", "input.", "user_request.", "vibedev."))

    fun signals(context: Context): Flow<VibeDevSignal> =
        RealtimeHub.shared(context.applicationContext).subscribe(FILTER).map { delivery ->
            when (delivery) {
                is HubDelivery.Resync -> VibeDevSignal.Resync
                else -> VibeDevSignal.Event
            }
        }
}
