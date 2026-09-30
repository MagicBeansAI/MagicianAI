package ai.magicbeans.magdroid.tutor

import ai.magicbeans.magdroid.access.MagicianAccess
import android.content.Context
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.client.request.header
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.net.URLEncoder

/**
 * The tutor's events, off the realtime bus.
 *
 * Actions do not come back on the chat stream. The backend routes them to
 * whichever surface the turn declared, and they arrive over
 * `/realtime/ws` — which is why a client that only reads the chat SSE sees a
 * lesson start and then nothing happen.
 *
 * Frames go straight to [TutorRun]. This class knows how to hold a socket open
 * and nothing about what a lesson is, which is what lets the run be tested
 * without a network and the socket be replaced without touching the parser.
 */
class TutorRealtime(private val context: Context) {

    private val client = HttpClient(CIO) { install(WebSockets) }
    private var job: Job? = null

    /**
     * Follow [run] until stopped.
     *
     * Reconnects on drop, because a lesson that stops drawing halfway because
     * the network blinked is indistinguishable to the owner from one that
     * crashed. The run keeps what it has already drawn across a reconnect.
     */
    fun follow(scope: CoroutineScope, run: TutorRun) {
        job?.cancel()
        job = scope.launch {
            var attempt = 0
            while (isActive && !run.finished) {
                val ok = runCatching { listen(run) }.isSuccess
                if (!isActive || run.finished) break
                // Backed off, and jittered by the attempt so two surfaces
                // reconnecting do not do it in lockstep.
                attempt = if (ok) 0 else (attempt + 1).coerceAtMost(MAX_ATTEMPT)
                delay(BASE_BACKOFF_MS shl attempt)
            }
        }
    }

    private suspend fun listen(run: TutorRun) {
        val url = "${socketBase()}/realtime/ws"
        client.webSocket(
            urlString = url,
            request = {
                MagicianAccess.headers(context).forEach { (name, value) -> header(name, value) }
            },
        ) {
            ai.magicbeans.magdroid.bridge.BridgeLog.info(TAG, "tutor realtime open")
            for (frame in incoming) {
                val text = (frame as? Frame.Text)?.readText() ?: continue
                run.apply(text)
                if (run.finished) break
            }
        }
    }

    suspend fun stop() {
        job?.cancelAndJoin()
        job = null
    }

    /**
     * The socket URL, from the same base the rest of the client uses.
     *
     * Derived rather than configured separately: two places to set the host is
     * two places for them to disagree, and the failure is a lesson that starts
     * and silently never draws.
     */
    private fun socketBase(): String =
        MagicianAccess.baseUrl(context)
            .replace("https://", "wss://")
            .replace("http://", "ws://") + "/api/magician/v2"

    private fun encoded(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

    private companion object {
        const val TAG = "MagicianTutorRT"
        const val BASE_BACKOFF_MS = 1_000L
        const val MAX_ATTEMPT = 5
    }
}
