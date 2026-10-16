package ai.magicbeans.magdroid.tutor

import ai.magicbeans.magdroid.realtime.HubDelivery
import ai.magicbeans.magdroid.realtime.HubFilter
import ai.magicbeans.magdroid.realtime.RealtimeHub
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.takeWhile
import kotlinx.coroutines.launch

/**
 * The tutor's events, off the realtime bus.
 *
 * Actions do not come back on the chat stream. The backend routes them to
 * whichever surface the turn declared, and they arrive over the realtime bus —
 * which is why a client that only reads the chat SSE sees a lesson start and
 * then nothing happen.
 *
 * They arrive wrapped: `{"event_type":"AgentEvent","data":{"event":{…}}}`.
 * This class used to hand the raw frame to [TutorRun], which reads
 * `event_type`/`payload` at the top level — always `AgentEvent`, never a
 * tutor event — so a lesson never drew. The app's [RealtimeHub] unwraps
 * `AgentEvent` once for every subscriber, and the run gets the inner event.
 *
 * This class knows how to follow the hub and nothing about what a lesson is,
 * which is what lets the run be tested without a network.
 */
class TutorRealtime(private val context: Context) {

    private val hub: RealtimeHub by lazy { RealtimeHub.shared(context) }
    private var job: Job? = null

    /**
     * Follow [run] until stopped or finished.
     *
     * The hub reconnects on drop, because a lesson that stops drawing halfway
     * because the network blinked is indistinguishable to the owner from one
     * that crashed. The run keeps what it has already drawn across a reconnect;
     * a resync has no REST read to make, so it changes nothing here.
     */
    fun follow(scope: CoroutineScope, run: TutorRun) {
        job?.cancel()
        job = scope.launch {
            ai.magicbeans.magdroid.bridge.BridgeLog.info(TAG, "tutor realtime follow")
            hub.subscribe(HubFilter(eventTypes = listOf(TUTOR_PREFIX)))
                .takeWhile { !run.finished }
                .collect { delivery ->
                    if (delivery is HubDelivery.Event) run.apply(delivery.event.unwrapped)
                }
        }
    }

    suspend fun stop() {
        job?.cancelAndJoin()
        job = null
    }

    private companion object {
        const val TAG = "MagicianTutorRT"
        const val TUTOR_PREFIX = "tutor."
    }
}
