package ai.magicbeans.magdroid.realtime

import ai.magicbeans.magdroid.access.MagicianAccess
import android.app.Activity
import android.app.Application
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Bundle
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.client.request.header
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.transform
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** What a subscriber receives: an event, or word that it missed some. */
sealed class HubDelivery {
    data class Event(val event: HubEvent) : HubDelivery()

    /**
     * Events were missed — a reconnect without resume, a server `hub.resync`,
     * or this subscriber falling behind its buffer. Re-read state by REST.
     */
    data class Resync(val reason: String) : HubDelivery()
}

/** One open socket, reduced to what the hub needs. */
interface HubSocket {
    /** The next text frame, or null once the socket has closed. */
    suspend fun receive(): String?
    suspend fun send(text: String)
}

/** Opens the realtime socket; the seam the tests replace. */
fun interface HubConnector {
    /** Connect, run [session] while the socket is open, return when it closes. */
    suspend fun connect(url: String, headers: Map<String, String>, session: suspend (HubSocket) -> Unit)
}

/** The production connector: one Ktor WebSocket. */
class KtorHubConnector(private val client: HttpClient) : HubConnector {
    override suspend fun connect(
        url: String,
        headers: Map<String, String>,
        session: suspend (HubSocket) -> Unit,
    ) {
        client.webSocket(
            urlString = url,
            request = { headers.forEach { (name, value) -> header(name, value) } },
        ) {
            val ktor = this
            session(object : HubSocket {
                override suspend fun receive(): String? {
                    while (true) {
                        val frame = ktor.incoming.receiveCatching().getOrNull() ?: return null
                        if (frame is Frame.Text) return frame.readText()
                    }
                }

                override suspend fun send(text: String) {
                    ktor.outgoing.send(Frame.Text(text))
                }
            })
        }
    }
}

/**
 * The app's one realtime connection.
 *
 * Every surface used to open its own socket to `/realtime/ws` — chat, tasks,
 * the task notification, Today, the thinking map, the tutor — and each one
 * received the whole scope's stream only to keep a sliver of it. Now there is
 * one, owned here, and surfaces [subscribe] with a [HubFilter].
 *
 * - Reconnects forever with capped full jitter (750 ms to 30 s), and at once
 *   when the app returns to the foreground or the network comes back
 *   ([reconnectNow]).
 * - Resumes from the last `(epoch, seq)` on a server that stamps them, and
 *   dedupes a replay by the same pair.
 * - Tells every subscriber to resync when it may have missed events. Against
 *   a server that predates the hub that is every reconnect.
 * - Sends `{"type":"ping"}` as a keepalive.
 * - Holds the socket only while somebody subscribes, plus a short linger so a
 *   screen change does not drop and reopen it.
 *
 * Each subscriber has its own bounded buffer. One that falls behind gets a
 * `Resync("lagged")` instead of stalling the socket for everyone else.
 */
class RealtimeHub internal constructor(
    private val connector: HubConnector,
    private val baseUrl: () -> String,
    private val headers: () -> Map<String, String>,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
    private val backoff: (attempt: Int) -> Long = { hubBackoffMs(it) },
    private val keepaliveMs: Long = KEEPALIVE_MS,
    private val lingerMs: Long = LINGER_MS,
) {
    /** For a caller that owns its own HTTP client (tests, the chat repository). */
    constructor(
        client: HttpClient,
        baseUrl: () -> String,
        headers: () -> Map<String, String>,
    ) : this(KtorHubConnector(client), baseUrl, headers)

    private val lock = Any()
    private val subscribers = LinkedHashSet<Subscriber>()
    private var connection: Job? = null
    private var linger: Job? = null
    private val wake = Channel<Unit>(Channel.CONFLATED)

    private val _connected = MutableStateFlow(false)

    /** Whether the socket is open right now. */
    val connected: StateFlow<Boolean> = _connected.asStateFlow()

    /**
     * Deliveries matching [filter], for as long as the flow is collected.
     *
     * The socket opens on the first subscriber and closes a little after the
     * last one leaves.
     */
    fun subscribe(filter: HubFilter = HubFilter.ALL): Flow<HubDelivery> = flow {
        val subscriber = Subscriber(filter)
        register(subscriber)
        try {
            for (delivery in subscriber.channel) emit(delivery)
        } finally {
            unregister(subscriber)
        }
    }

    /** Events matching [filter]; a resync calls [onResync] instead of emitting. */
    fun events(
        filter: HubFilter = HubFilter.ALL,
        onResync: suspend (reason: String) -> Unit = {},
    ): Flow<HubEvent> = subscribe(filter).transform { delivery ->
        when (delivery) {
            is HubDelivery.Event -> emit(delivery.event)
            is HubDelivery.Resync -> onResync(delivery.reason)
        }
    }

    /** Skip the rest of the backoff: the app came forward or the network came back. */
    fun reconnectNow() {
        wake.trySend(Unit)
    }

    /**
     * Drop the current connection and open a fresh one, for a host or
     * credential change. Every subscriber resyncs: what it holds came from the
     * old connection.
     */
    fun restart() {
        val targets = synchronized(lock) {
            connection?.cancel()
            connection = if (subscribers.isNotEmpty()) scope.launch { run() } else null
            subscribers.toList()
        }
        val resync = HubDelivery.Resync("restart")
        targets.forEach { it.offer(resync) }
    }

    private fun register(subscriber: Subscriber) {
        synchronized(lock) {
            subscribers += subscriber
            linger?.cancel()
            linger = null
            if (connection?.isActive != true) {
                connection = scope.launch { run() }
            }
        }
    }

    private fun unregister(subscriber: Subscriber) {
        subscriber.channel.close()
        synchronized(lock) {
            subscribers -= subscriber
            if (subscribers.isEmpty()) {
                linger?.cancel()
                linger = scope.launch {
                    delay(lingerMs)
                    synchronized(lock) {
                        if (subscribers.isEmpty()) {
                            connection?.cancel()
                            connection = null
                        }
                    }
                }
            }
        }
    }

    /**
     * The connection loop. A fresh [HubProtocol] per run: after an idle stop
     * every new subscriber loads its own state, so there is nothing to resume
     * into and nothing to resync.
     */
    private suspend fun run() {
        val protocol = HubProtocol()
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            var opened = false
            try {
                val host = baseUrl().trim()
                check(host.isNotEmpty()) { "No Magician host configured yet." }
                val url = realtimeHubUrl(host, protocol.resumeCursor())
                connector.connect(url, headers()) { socket ->
                    opened = true
                    attempt = 0
                    wake.tryReceive()
                    _connected.value = true
                    dispatch(protocol.onOpen())
                    coroutineScope {
                        val keepalive = launch {
                            while (isActive) {
                                delay(keepaliveMs)
                                socket.send(PING)
                            }
                        }
                        try {
                            while (true) {
                                val text = socket.receive() ?: break
                                dispatch(protocol.onText(text))
                            }
                        } finally {
                            keepalive.cancel()
                        }
                    }
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Throwable) {
                // A phone drops its socket constantly; a lost connection is
                // the normal case, and the loop below is the handling.
            } finally {
                _connected.value = false
            }
            // A clean close is still a disconnected socket: the pause keeps a
            // proxy that accepts and immediately closes from becoming a hot loop.
            attempt = if (opened) 1 else attempt + 1
            withTimeoutOrNull(backoff(attempt)) { wake.receive() }
        }
    }

    private fun dispatch(outputs: List<HubOutput>) {
        if (outputs.isEmpty()) return
        val targets = synchronized(lock) { subscribers.toList() }
        outputs.forEach { output ->
            when (output) {
                is HubOutput.Deliver -> {
                    runCatching { ai.magicbeans.magdroid.chat.TaskUsageStore.shared.ingest(output.event.root) }
                    val delivery = HubDelivery.Event(output.event)
                    targets.forEach { subscriber ->
                        val wanted = runCatching { subscriber.filter.matches(output.event) }.getOrDefault(false)
                        if (wanted) subscriber.offer(delivery)
                    }
                }
                is HubOutput.Resync -> {
                    val delivery = HubDelivery.Resync(output.reason)
                    targets.forEach { it.offer(delivery) }
                }
            }
        }
    }

    private class Subscriber(val filter: HubFilter) {
        val channel = Channel<HubDelivery>(SUBSCRIBER_BUFFER)

        @Volatile
        private var lagged = false

        /**
         * Never suspends the socket. A full buffer marks this subscriber
         * lagged; the next delivery that fits is preceded by a resync, so the
         * gap is re-read rather than silently lost.
         */
        fun offer(delivery: HubDelivery) {
            if (lagged) {
                if (channel.trySend(HubDelivery.Resync("lagged")).isFailure) return
                lagged = false
            }
            val result = channel.trySend(delivery)
            if (result.isFailure && !result.isClosed) lagged = true
        }
    }

    companion object {
        internal const val PING = """{"type":"ping"}"""
        internal const val KEEPALIVE_MS = 25_000L
        internal const val LINGER_MS = 5_000L
        internal const val SUBSCRIBER_BUFFER = 512

        @Volatile
        private var shared: RealtimeHub? = null

        /** The process's hub. Every surface subscribes here. */
        fun shared(context: Context): RealtimeHub = shared ?: synchronized(this) {
            shared ?: create(context.applicationContext).also { shared = it }
        }

        private fun create(app: Context): RealtimeHub {
            val client = HttpClient(CIO) { install(WebSockets) }
            val hub = RealtimeHub(
                connector = KtorHubConnector(client),
                baseUrl = { MagicianAccess.baseUrl(app) },
                headers = { MagicianAccess.headers(app) },
            )
            HubWakeSignals.install(app, hub::reconnectNow)
            // A machine switch in Settings: drop the old machine's socket and
            // reopen against the newly active one; subscribers resync.
            CoroutineScope(SupervisorJob() + Dispatchers.Default).launch {
                MagicianAccess.activeMachine.collect { hub.restart() }
            }
            return hub
        }
    }
}

/**
 * Foreground and network-available, the two moments a waiting reconnect
 * should not keep waiting.
 */
internal object HubWakeSignals {
    fun install(app: Context, wake: () -> Unit) {
        (app as? Application)?.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
            private var started = 0
            override fun onActivityStarted(activity: Activity) {
                if (started++ == 0) wake()
            }
            override fun onActivityStopped(activity: Activity) {
                started = (started - 1).coerceAtLeast(0)
            }
            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) = Unit
            override fun onActivityResumed(activity: Activity) = Unit
            override fun onActivityPaused(activity: Activity) = Unit
            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) = Unit
            override fun onActivityDestroyed(activity: Activity) = Unit
        })
        runCatching {
            app.getSystemService(ConnectivityManager::class.java)
                ?.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) = wake()
                })
        }
    }
}
