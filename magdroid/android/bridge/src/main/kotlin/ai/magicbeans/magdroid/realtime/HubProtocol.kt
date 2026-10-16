package ai.magicbeans.magdroid.realtime

import java.net.URLEncoder
import kotlin.math.min
import kotlin.random.Random

/** The last event this client holds: where a reconnect resumes from. */
data class HubCursor(val epoch: String, val seq: Long)

/** What the protocol asks the hub to do with one input. */
sealed class HubOutput {
    data class Deliver(val event: HubEvent) : HubOutput()
    data class Resync(val reason: String) : HubOutput()
}

/**
 * The hub's connection rules, free of the socket so they can be tested.
 *
 * Two servers have to work: one that stamps `seq`/`epoch` and greets each
 * connection with `hub.hello`, and the current one, which sends neither.
 *
 * - The first connection never resyncs: every subscriber loads its own state
 *   by REST when it mounts.
 * - A reconnect to a hub-aware server resumes from [cursor]. `hub.hello` with
 *   `resumed=false`, or an explicit `hub.resync`, resyncs.
 * - A reconnect to a server that has never said hello resyncs at once — there
 *   is no resume, so the gap is unknowable. A reconnect that expected a hello
 *   and got an event first resyncs on that event (the server was downgraded).
 * - Events are deduped by `(epoch, seq)`: a replayed event this client already
 *   delivered is dropped. Unstamped events always pass.
 * - One resync per gap: a second resync with no event in between (the hello
 *   and the `hub.resync` that follows it) is not repeated.
 */
internal class HubProtocol {
    var cursor: HubCursor? = null
        private set

    /** True once this server has greeted us; then reconnects wait for its hello. */
    var serverSpeaksHub: Boolean = false
        private set

    private var connections = 0
    private var awaitingHello = false
    private var resyncedSinceEvent = false

    private val reconnect: Boolean get() = connections > 1

    /** The resume point to put on the next connection's URL. */
    fun resumeCursor(): HubCursor? = cursor.takeIf { serverSpeaksHub }

    /** A socket opened. */
    fun onOpen(): List<HubOutput> {
        connections++
        resyncedSinceEvent = false
        awaitingHello = serverSpeaksHub
        if (!reconnect || serverSpeaksHub) return emptyList()
        return resync("reconnect")
    }

    fun onText(text: String): List<HubOutput> = when (val frame = parseHubFrame(text)) {
        null, is HubFrame.Control -> emptyList()
        is HubFrame.Hello -> onHello(frame)
        is HubFrame.Resync -> resync(frame.reason)
        is HubFrame.Event -> onEvent(frame.event)
    }

    private fun onHello(hello: HubFrame.Hello): List<HubOutput> {
        serverSpeaksHub = true
        awaitingHello = false
        val previous = cursor
        if (hello.resumed && previous != null && previous.epoch == hello.epoch) {
            // The replay that follows advances the cursor itself.
            return emptyList()
        }
        // A fresh seq space, or one this client cannot resume in: anything
        // after the hello is live, everything before it is the resync's job.
        cursor = HubCursor(hello.epoch, hello.seq)
        return if (reconnect) resync(if (previous != null && previous.epoch != hello.epoch) "epoch" else "reconnect")
        else emptyList()
    }

    private fun onEvent(event: HubEvent): List<HubOutput> {
        val out = mutableListOf<HubOutput>()
        if (awaitingHello) {
            awaitingHello = false
            serverSpeaksHub = false
            if (reconnect) out += resync("reconnect")
        }
        val epoch = event.epoch
        val seq = event.seq
        if (epoch != null && seq != null) {
            val current = cursor
            if (current != null && current.epoch == epoch && seq <= current.seq) return out
            cursor = HubCursor(epoch, seq)
        }
        resyncedSinceEvent = false
        out += HubOutput.Deliver(event)
        return out
    }

    private fun resync(reason: String): List<HubOutput> {
        if (resyncedSinceEvent) return emptyList()
        resyncedSinceEvent = true
        return listOf(HubOutput.Resync(reason))
    }
}

/**
 * The realtime socket URL for [base], resuming from [cursor] when there is one.
 *
 * A bare host means TLS: guessing plaintext for a credential-bearing socket is
 * the wrong way to be wrong.
 */
fun realtimeHubUrl(base: String, cursor: HubCursor?): String {
    val host = base.trim().trimEnd('/')
    val socket = when {
        host.startsWith("https://") -> "wss://" + host.removePrefix("https://")
        host.startsWith("http://") -> "ws://" + host.removePrefix("http://")
        host.startsWith("wss://") || host.startsWith("ws://") -> host
        else -> "wss://$host"
    }
    val url = "$socket$REALTIME_PATH"
    if (cursor == null || cursor.epoch.isBlank()) return url
    val epoch = URLEncoder.encode(cursor.epoch, Charsets.UTF_8.name())
    return "$url?resume_epoch=$epoch&resume_seq=${cursor.seq}"
}

/**
 * Capped full jitter, the shape every per-feature socket used: at least 750 ms
 * so a proxy that accepts and closes is never a hot loop, at most 30 s.
 */
fun hubBackoffMs(attempt: Int, random: Random = Random.Default): Long {
    val cap = min(MAX_BACKOFF_MS, 1_000L shl attempt.coerceIn(0, 5))
    return random.nextLong(MIN_BACKOFF_MS, cap + 1)
}

internal const val REALTIME_PATH = "/api/magician/v2/realtime/ws"
internal const val MIN_BACKOFF_MS = 750L
internal const val MAX_BACKOFF_MS = 30_000L
