package ai.magicbeans.magdroid.bridge

import ai.magicbeans.magdroid.mcp.MagdroidMcpServer
import ai.magicbeans.magdroid.mcp.McpToolCallResult
import ai.magicbeans.magdroid.mcp.McpToolHandler
import ai.magicbeans.magdroid.log.CommandLog
import ai.magicbeans.magdroid.access.AndroidAutomationIdentityManager
import ai.magicbeans.magdroid.access.AndroidAutomationTrustMode
import ai.magicbeans.magdroid.access.AndroidPlayIntegrity
import android.util.Base64
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.client.request.header
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
import io.ktor.websocket.send
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.math.min
import kotlin.random.Random

/**
 * Holds one outbound connection to Magician and runs what arrives on it.
 *
 * This replaces the inbound MCP listener. The listener bound `0.0.0.0` with no
 * TLS and never enforced its own key, which made every tool — gestures, typing,
 * screenshots — callable by anything on the same network. Dialing out removes
 * that surface rather than defending it: there is no port on the phone to reach.
 * It also means the device works from cellular or a network it does not control,
 * with no inbound reachability and no tunnel terminating on the handset.
 *
 * This socket carries MCP `2026-07-28` exclusively. Connection direction does
 * not change protocol roles: Magdroid dials out but remains the MCP server.
 */
class MagicianBridgeClient(
    private val config: BridgeConfig,
    private val toolHandler: McpToolHandler,
    private val scope: CoroutineScope,
) {
    private val client = HttpClient(CIO) {
        install(WebSockets) {
            // Calls are small JSON objects. Screenshot bytes travel in the
            // opposite direction, so accepting multi-gigabyte inbound frames
            // would add only a memory-exhaustion path, not a capability.
            maxFrameSize = MagdroidMcpServer.MAX_REQUEST_BYTES.toLong()
        }
    }

    /**
     * Serialises writes to the socket.
     *
     * Requests are handled concurrently so a slow screenshot does not stall a
     * queue of taps behind it, which means several coroutines can finish at
     * once. A WebSocket frame must not be interleaved with another, so sends
     * take this lock while the handlers themselves stay parallel.
     */
    private val sendLock = Mutex()
    private val mcpServer = MagdroidMcpServer()
    private val json = Json { ignoreUnknownKeys = false }

    private var runner: Job? = null

    @Volatile
    var isConnected: Boolean = false
        private set

    fun start() {
        if (runner?.isActive == true) return
        runner = scope.launch { runForever() }
    }

    fun stop() {
        runner?.cancel()
        runner = null
        isConnected = false
        client.close()
        mcpServer.close()
    }

    /**
     * Reconnect forever, backing off after failures.
     *
     * A phone loses its connection constantly — the screen sleeps, WiFi hands
     * over to cellular, Doze suspends the radio — so a dropped socket is the
     * normal case and not an error worth surfacing. Backoff is capped and
     * jittered so a Magician that is down does not receive a synchronised
     * retry from every paired device at once.
     */
    private suspend fun runForever() {
        var attempt = 0
        while (scope.isActive) {
            try {
                connectOnce()
                // A clean close still means we need a new socket, but it is not
                // a failure, so the next attempt starts from zero delay.
                attempt = 0
            } catch (cancellation: kotlinx.coroutines.CancellationException) {
                throw cancellation
            } catch (error: Throwable) {
                BridgeLog.warn(TAG, "bridge connection ended: ${error.message}")
            } finally {
                isConnected = false
            }
            if (!scope.isActive) return
            delay(backoffMillis(attempt))
            attempt = min(attempt + 1, MAX_BACKOFF_EXPONENT)
        }
    }

    private fun backoffMillis(attempt: Int): Long {
        val base = BASE_BACKOFF_MS shl attempt
        val capped = min(base, MAX_BACKOFF_MS)
        // Full jitter: a fleet reconnecting in lockstep is its own outage.
        return Random.nextLong(BASE_BACKOFF_MS, capped + 1)
    }

    private suspend fun connectOnce() {
        client.webSocket(
            urlString = config.websocketUrl,
            request = {
                // Identity comes from the client's own Cloudflare Access
                // credentials — the same pair the iOS client uses. The device
                // authenticates to Magician; Magician never authenticates to the
                // device, because the device has no door to knock on.
                config.headers.forEach { (name, value) -> header(name, value) }
            },
        ) {
            currentSession = this
            mcpServer.close()

            try {
                val first = incoming.receive() as? Frame.Text
                    ?: throw IllegalStateException("Magician did not send an Apps socket challenge")
                val challenge = json.decodeFromString(
                    DeviceSocketChallenge.serializer(),
                    first.readText(),
                )
                acceptSocketChallenge(challenge)
                isConnected = true
                BridgeLog.info(TAG, "attested Apps bridge connected to ${config.websocketUrl}")
                for (frame in incoming) {
                    val text = (frame as? Frame.Text)?.readText() ?: continue
                    launch { dispatch(text) }
                }
            } finally {
                mcpServer.close()
                currentSession = null
            }
        }
    }

    private suspend fun acceptSocketChallenge(challenge: DeviceSocketChallenge) {
        if (
            challenge.schema != SOCKET_HANDSHAKE_SCHEMA ||
            challenge.connectionId.length !in 32..64 ||
            challenge.keyId != config.automationKeyId ||
            challenge.apkSha256 != config.automationApkSha256 ||
            challenge.attestationPolicyDigest != config.automationAttestationPolicyDigest ||
            challenge.targetRef.length !in 32..256 ||
            challenge.reviewGeneration < 0 ||
            challenge.protocolVersion != MagdroidMcpServer.PROTOCOL_VERSION
        ) {
            throw IllegalStateException("Magician sent a mismatched Apps socket challenge")
        }
        val nonce = runCatching { Base64.decode(challenge.serverNonceBase64, Base64.DEFAULT) }
            .getOrNull()
            ?.takeIf { it.size == 32 }
            ?: throw IllegalStateException("Magician sent an invalid Apps socket nonce")
        val signature = config.automationIdentity.signSocketProof(
            keyAlias = config.automationKeyAlias,
            connectionId = challenge.connectionId,
            keyId = challenge.keyId,
            targetRef = challenge.targetRef,
            reviewGeneration = challenge.reviewGeneration,
            protocolVersion = challenge.protocolVersion,
            serverNonce = nonce,
            expectedApkSha256 = config.automationApkSha256,
            attestationPolicyDigest = config.automationAttestationPolicyDigest,
            automationTrustMode = config.automationTrustMode,
        )
        val signedMaterial = AndroidAutomationIdentityManager.socketSigningBytes(
            challenge.connectionId,
            challenge.keyId,
            challenge.targetRef,
            challenge.reviewGeneration,
            challenge.protocolVersion,
            nonce,
            challenge.apkSha256,
            challenge.attestationPolicyDigest,
        )
        val playIntegrityToken = if (config.automationTrustMode == AndroidAutomationTrustMode.PlayIntegrity) {
            AndroidPlayIntegrity.token(
                config.automationIdentityContext,
                config.automationPlayIntegrityCloudProjectNumber,
                AndroidPlayIntegrity.requestHash(
                    "magician.android-play-integrity.socket.v1",
                    signedMaterial,
                    Base64.decode(signature, Base64.DEFAULT),
                ),
            )
        } else {
            ""
        }
        sendText(
            json.encodeToString(
                DeviceSocketProof.serializer(),
                DeviceSocketProof(
                    schema = challenge.schema,
                    connectionId = challenge.connectionId,
                    keyId = challenge.keyId,
                    targetRef = challenge.targetRef,
                    reviewGeneration = challenge.reviewGeneration,
                    protocolVersion = challenge.protocolVersion,
                    serverNonceBase64 = challenge.serverNonceBase64,
                    apkSha256 = challenge.apkSha256,
                    attestationPolicyDigest = challenge.attestationPolicyDigest,
                    signatureBase64 = signature,
                    playIntegrityToken = playIntegrityToken,
                ),
            ),
        )
    }

    /**
     * Route one MCP frame without allowing malformed peer input to cancel the
     * WebSocket scope. JSON-RPC errors are returned by the bounded server.
     */
    private suspend fun dispatch(text: String) {
        try {
            route(text)
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (error: Throwable) {
            // Each frame is launched into the socket's scope, so an escaping
            // throw would cancel that scope and take the connection with it.
            // One malformed frame costing a reconnect is the failure this
            // catch exists to prevent.
            BridgeLog.error(TAG, "frame handling threw", error)
        }
    }

    private suspend fun route(text: String) {
        when (val outcome = mcpServer.handle(text, ::handleAndRecordToolCall)) {
            is MagdroidMcpServer.Outcome.Reply -> sendText(outcome.text)
            MagdroidMcpServer.Outcome.NoReply -> Unit
        }
    }

    /** Emit a subscribed, request-bound MCP roster invalidation if one exists. */
    fun notifyToolsChanged() {
        val notification = mcpServer.toolsChangedNotification() ?: return
        scope.launch { sendText(notification) }
    }

    /**
     * Record production MCP calls in the bounded App Pilot history so remote
     * automation never appears idle while it is acting.
     */
    private suspend fun handleAndRecordToolCall(
        action: String,
        arguments: JsonObject?,
    ): McpToolCallResult {
        val started = System.currentTimeMillis()
        return try {
            val result = toolHandler.handleToolCall(action, arguments)
            recordToolCall(action, started, success = !result.isError)
            result
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            recordToolCall(action, started, success = false)
            throw cancellation
        } catch (error: Throwable) {
            recordToolCall(action, started, success = false)
            throw error
        }
    }

    private fun recordToolCall(action: String, started: Long, success: Boolean) {
        CommandLog.add(
            CommandLog.Entry(
                timestamp = started,
                command = action,
                latencyMs = (System.currentTimeMillis() - started).toInt(),
                success = success,
                category = CommandLog.categoryFor(action),
            ),
        )
    }

    /**
     * Write one MCP frame.
     *
     * Both go through the same lock. A WebSocket frame must not interleave with
     * another, and handlers run concurrently so that several can finish at
     * once.
     */
    private suspend fun sendText(text: String) {
        val session = currentSession ?: return
        sendLock.withLock {
            try {
                session.send(text)
            } catch (error: Throwable) {
                BridgeLog.warn(TAG, "bridge send failed: ${error.message}")
            }
        }
    }

    /** The live socket, shared by request handlers and notifications. */
    @Volatile
    private var currentSession: io.ktor.websocket.WebSocketSession? = null

    companion object {
        private const val TAG = "MagdroidBridge"
        private const val BASE_BACKOFF_MS = 1_000L
        private const val MAX_BACKOFF_MS = 60_000L
        private const val MAX_BACKOFF_EXPONENT = 6
        private const val SOCKET_HANDSHAKE_SCHEMA = "magician.android-apps-socket-handshake.v2"

    }
}

/**
 * Where to dial, and the headers that say who we are.
 *
 * Headers rather than named fields: the identity scheme belongs to
 * `MagicianAccess`, and spelling its parts out here would mean changing this
 * file every time that changes.
 */
data class BridgeConfig(
    val websocketUrl: String,
    val headers: Map<String, String>,
    val automationKeyAlias: String,
    val automationKeyId: String,
    val automationApkSha256: String,
    val automationAttestationPolicyDigest: String,
    val automationPlayIntegrityCloudProjectNumber: Long,
    val automationTrustMode: AndroidAutomationTrustMode,
    val automationIdentityContext: android.content.Context,
    val automationIdentity: AndroidAutomationIdentityManager,
)

@Serializable
private data class DeviceSocketChallenge(
    val schema: String,
    @SerialName("connection_id") val connectionId: String,
    @SerialName("key_id") val keyId: String,
    @SerialName("target_ref") val targetRef: String,
    @SerialName("review_generation") val reviewGeneration: Long,
    @SerialName("protocol_version") val protocolVersion: String,
    @SerialName("server_nonce_base64") val serverNonceBase64: String,
    @SerialName("apk_sha256") val apkSha256: String,
    @SerialName("attestation_policy_digest") val attestationPolicyDigest: String,
)

@Serializable
private data class DeviceSocketProof(
    val schema: String,
    @SerialName("connection_id") val connectionId: String,
    @SerialName("key_id") val keyId: String,
    @SerialName("target_ref") val targetRef: String,
    @SerialName("review_generation") val reviewGeneration: Long,
    @SerialName("protocol_version") val protocolVersion: String,
    @SerialName("server_nonce_base64") val serverNonceBase64: String,
    @SerialName("apk_sha256") val apkSha256: String,
    @SerialName("attestation_policy_digest") val attestationPolicyDigest: String,
    @SerialName("signature_base64") val signatureBase64: String,
    @SerialName("play_integrity_token") val playIntegrityToken: String,
)
