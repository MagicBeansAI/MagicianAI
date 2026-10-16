package ai.magicbeans.magdroid.realtime

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel

/** A socket the test drives: push frames in, read what the hub sent. */
internal class FakeHubSocket : HubSocket {
    private val inbound = Channel<String>(Channel.UNLIMITED)
    private val sentFrames = mutableListOf<String>()

    val sent: List<String> get() = synchronized(sentFrames) { sentFrames.toList() }

    fun push(text: String) {
        inbound.trySend(text)
    }

    /** The server closes the connection. */
    fun close() {
        inbound.close()
    }

    override suspend fun receive(): String? = inbound.receiveCatching().getOrNull()

    override suspend fun send(text: String) {
        synchronized(sentFrames) { sentFrames += text }
    }
}

/** Hands each connection to the test as a [FakeHubSocket], recording its URL. */
internal class FakeHubConnector : HubConnector {
    private val urlLog = mutableListOf<String>()
    val connections = Channel<FakeHubSocket>(Channel.UNLIMITED)

    val urls: List<String> get() = synchronized(urlLog) { urlLog.toList() }

    override suspend fun connect(
        url: String,
        headers: Map<String, String>,
        session: suspend (HubSocket) -> Unit,
    ) {
        val socket = FakeHubSocket()
        synchronized(urlLog) { urlLog += url }
        connections.send(socket)
        session(socket)
    }
}

/** A hub on [connector] with no backoff, so reconnects are immediate. */
internal fun testHub(
    connector: FakeHubConnector,
    scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default),
    keepaliveMs: Long = 60_000,
) = RealtimeHub(
    connector = connector,
    baseUrl = { "https://mobile.example" },
    headers = { mapOf("Authorization" to "Bearer test-device") },
    scope = scope,
    backoff = { 0L },
    keepaliveMs = keepaliveMs,
    lingerMs = 0,
)
