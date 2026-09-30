package ai.magicbeans.magdroid.mcp

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.addJsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import java.util.concurrent.atomic.AtomicReference

/**
 * How a tool call reaches the device.
 *
 * A function rather than [McpToolHandler] itself: the handler needs a live
 * accessibility service and its engines, so depending on it directly would make
 * every path through [MagdroidMcpServer] untestable off-device — including the
 * error paths, which are the ones worth testing.
 */
typealias McpInvoke = suspend (name: String, arguments: JsonObject?) -> McpToolCallResult

/**
 * The companion as an MCP server, over the socket it dialled out on.
 *
 * The protocol is transport-agnostic and permits custom transports over any
 * bidirectional channel, so the phone stays the *server* while being the side
 * that connects. Protocol roles do not follow connection direction, which is
 * what lets a handset behind CGNAT be an MCP server at all.
 *
 * Stateless for tool discovery and calls, because `2026-07-28` retired the
 * initialize handshake and session ids. The one bounded exception is an
 * optional `subscriptions/listen` request whose lifetime is exactly the socket
 * lifetime. A dropped socket therefore costs no durable session recovery:
 * reconnect, rediscover, and subscribe again.
 *
 * The surface is deliberately closed — four methods, declared in
 * [discover] and listed in `docs/archive/plans/2026-08-10-android-mcp-migration.md`.
 * Anything else is answered `-32601` rather than ignored, because a client
 * discovers the boundary by being told about it, and silence would leave it
 * waiting on a reply that is never coming.
 */
class MagdroidMcpServer(
    private val tools: () -> List<McpToolDefinition> = McpToolRegistry::getAvailableTools,
) {

    /** What [handle] decided about one incoming frame. */
    sealed class Outcome {
        /** A notification. Correct behaviour is to answer nothing at all. */
        object NoReply : Outcome()

        /** Response text to write back to the socket. */
        data class Reply(val text: String) : Outcome()
    }

    private val toolsSubscriptionId = AtomicReference<JsonElement?>(null)
    private val callLock = Any()
    private val inFlightCalls = mutableMapOf<String, Job>()
    private val earlyCancellations = linkedMapOf<String, Long>()

    companion object {
        const val PROTOCOL_VERSION: String = "2026-07-28"
        const val MAX_REQUEST_BYTES: Int = 4 * 1024 * 1024
        // One admitted 8 MiB Apps result (the screenshot action is the largest)
        // plus a fixed JSON-RPC/content envelope. The server checks the final
        // serialized UTF-8 response so base64/JSON overhead cannot pass silently.
        const val MAX_RESPONSE_FRAME_BYTES: Int = (8 * 1024 * 1024) + (128 * 1024)

        private const val SERVER_NAME = "magdroid"
        // 1.1.0 is the first build with the closed Apps owner claim, exact
        // eight-action point-of-use binding and exact request cancellation.
        private const val SERVER_VERSION = "1.1.0"
        private const val DISCOVER_TTL_MS = 300_000L
        private const val MAX_EARLY_CANCELLATIONS = 128
        private const val EARLY_CANCELLATION_TTL_NANOS = 5_000_000_000L

        private const val PARSE_ERROR = -32700
        private const val INVALID_REQUEST = -32600
        private const val METHOD_NOT_FOUND = -32601
        private const val INVALID_PARAMS = -32602
        private const val INTERNAL_ERROR = -32603
    }

    private val json = Json { ignoreUnknownKeys = true }

    /**
     * Handle one frame.
     *
     * The socket is MCP-only. Malformed or legacy frames receive a JSON-RPC
     * error instead of being passed to a second decoder.
     */
    suspend fun handle(text: String, invoke: McpInvoke): Outcome {
        if (text.toByteArray(Charsets.UTF_8).size > MAX_REQUEST_BYTES) {
            return Outcome.Reply(error(JsonNull, INVALID_REQUEST, "request is too large"))
        }
        val element = try {
            json.parseToJsonElement(text)
        } catch (_: Throwable) {
            return Outcome.Reply(error(JsonNull, PARSE_ERROR, "parse error"))
        }
        val request = element as? JsonObject
            ?: return Outcome.Reply(error(JsonNull, INVALID_REQUEST, "request must be an object"))
        if ((request["jsonrpc"] as? JsonPrimitive)?.contentOrNull != "2.0") {
            return Outcome.Reply(error(request["id"] ?: JsonNull, INVALID_REQUEST, "jsonrpc must be 2.0"))
        }

        val hasId = request.containsKey("id")
        val id = request["id"] ?: JsonNull
        if (!validRequestId(id)) {
            return Outcome.Reply(error(JsonNull, INVALID_REQUEST, "id must be a string, number, or null"))
        }
        // Cast rather than `jsonPrimitive`, which throws on a non-primitive. A
        // peer sending `"method": {}` is malformed, and malformed input must
        // produce an error frame — never an exception that unwinds into the
        // socket loop and drops a connection the phone then has to rebuild.
        val method = (request["method"] as? JsonPrimitive)
            ?.takeIf { it.isString }
            ?.contentOrNull

        // No id is a notification, and a notification must never be answered —
        // replying to one leaves the peer correlating a response to a request
        // that does not exist. This is checked before the method is validated
        // because it holds even for a method we do not know.
        if (!hasId) {
            if (method == "notifications/cancelled") {
                cancelRequest(request)
            }
            return Outcome.NoReply
        }

        if (method == null) {
            return Outcome.Reply(error(id, INVALID_REQUEST, "missing method"))
        }

        val response = when (method) {
                "server/discover" -> result(id, discover())
                "tools/list" -> result(id, toolsList())
                "tools/call" -> toolsCall(id, request, invoke)
                "subscriptions/listen" -> listen(id, request)
                else -> error(id, METHOD_NOT_FOUND, "$method is not supported by this server")
            }
        return Outcome.Reply(
            if (response.toByteArray(Charsets.UTF_8).size <= MAX_RESPONSE_FRAME_BYTES) {
                response
            } else {
                error(id, INTERNAL_ERROR, "serialized response exceeds the bridge frame ceiling")
            }
        )
    }

    /**
     * What this build supports.
     *
     * The stateless replacement for the `initialize` handshake, and what keeps
     * a bounded implementation honest: declaring only `tools` means a client
     * never asks for resources or prompts, rather than asking and being
     * refused. `ttlMs` makes the answer cacheable, so the cost is paid once
     * rather than on every reconnect.
     */
    private fun discover(): JsonObject = buildJsonObject {
        put("resultType", "complete")
        putJsonArray("supportedVersions") { add(PROTOCOL_VERSION) }
        putJsonObject("capabilities") {
            putJsonObject("tools") { put("listChanged", true) }
        }
        put("instructions", "Android companion. Tools only — no resources, prompts or sampling.")
        put("ttlMs", DISCOVER_TTL_MS)
        // Private: the roster reflects one handset's granted permissions, and a
        // shared cache would serve one owner's capabilities to another.
        put("cacheScope", "private")
        putJsonObject("_meta") {
            putJsonObject("io.modelcontextprotocol/serverInfo") {
                put("name", SERVER_NAME)
                put("version", SERVER_VERSION)
            }
        }
    }

    /** The roster, straight from the registry that already describes it. */
    private fun toolsList(): JsonObject = buildJsonObject {
        putJsonArray("tools") {
            tools().forEach { tool ->
                addJsonObject {
                    put("name", tool.name)
                    put("description", tool.description)
                    put("inputSchema", tool.inputSchema)
                    put("annotations", tool.annotations.toJson())
                }
            }
        }
    }

    private suspend fun toolsCall(
        id: JsonElement,
        request: JsonObject,
        invoke: McpInvoke,
    ): String {
        val params = request["params"] as? JsonObject
            ?: return error(id, INVALID_PARAMS, "tools/call requires params")
        val name = (params["name"] as? JsonPrimitive)
            ?.takeIf { it.isString }
            ?.contentOrNull
            ?: return error(id, INVALID_PARAMS, "tools/call requires params.name")
        if (tools().none { it.name == name }) {
            return error(id, INVALID_PARAMS, "unknown or unavailable tool")
        }
        val rawArguments = params["arguments"]
        if (rawArguments != null && rawArguments !is JsonObject) {
            return error(id, INVALID_PARAMS, "tools/call params.arguments must be an object")
        }
        val arguments = rawArguments as? JsonObject

        val callKey = id.toString()
        val callJob = currentCoroutineContext()[Job]
            ?: return error(id, INTERNAL_ERROR, "tool call has no coroutine owner")
        if (!registerCall(callKey, callJob)) {
            throw CancellationException("request was cancelled before dispatch")
        }
        val outcome = try {
            invoke(name, arguments)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (failure: Throwable) {
            return error(id, INTERNAL_ERROR, failure.message ?: failure::class.java.simpleName)
        } finally {
            unregisterCall(callKey, callJob)
        }

        // A tool that ran and failed is a *result* carrying isError, not a
        // JSON-RPC error. Protocol errors mean the call could not be made; a
        // tap that missed was made and did not work, and a caller that cannot
        // tell those apart will retry the wrong one.
        return result(
            id,
            buildJsonObject {
                putJsonArray("content") {
                    outcome.content.forEach { block ->
                        addJsonObject {
                            put("type", block.type)
                            block.text?.let { put("text", it) }
                            block.data?.let { put("data", it) }
                            block.mimeType?.let { put("mimeType", it) }
                        }
                    }
                }
                put("isError", outcome.isError)
                outcome.structuredContent?.let { put("structuredContent", it) }
            },
        )
    }

    /**
     * Open the one bounded notification stream supported by the companion.
     *
     * The request deliberately remains pending. MCP 2026-07-28 acknowledges a
     * subscription with the first notification and sends the final response
     * only when the stream ends; the WebSocket closing is our end condition.
     */
    private fun listen(id: JsonElement, request: JsonObject): String {
        val notifications = (request["params"] as? JsonObject)
            ?.get("notifications") as? JsonObject
            ?: return error(id, INVALID_PARAMS, "subscriptions/listen requires notifications")
        val toolsListChanged = notifications["toolsListChanged"] as? JsonPrimitive
        if (notifications.keys != setOf("toolsListChanged")
            || toolsListChanged == null
            || toolsListChanged.isString
            || toolsListChanged.booleanOrNull != true
        ) {
            return error(id, INVALID_PARAMS, "only toolsListChanged may be subscribed")
        }
        if (!toolsSubscriptionId.compareAndSet(null, id)) {
            return error(id, INVALID_REQUEST, "a tools subscription is already active")
        }
        return buildJsonObject {
            put("jsonrpc", "2.0")
            put("method", "notifications/subscriptions/acknowledged")
            putJsonObject("params") {
                putJsonObject("_meta") {
                    put("io.modelcontextprotocol/subscriptionId", id)
                }
                putJsonObject("notifications") { put("toolsListChanged", true) }
            }
        }.toString()
    }

    /** Return a subscribed, correctly correlated roster-change notification. */
    fun toolsChangedNotification(): String? {
        val id = toolsSubscriptionId.get() ?: return null
        return buildJsonObject {
            put("jsonrpc", "2.0")
            put("method", "notifications/tools/list_changed")
            putJsonObject("params") {
                putJsonObject("_meta") {
                    put("io.modelcontextprotocol/subscriptionId", id)
                }
            }
        }.toString()
    }

    fun close() {
        toolsSubscriptionId.set(null)
        val calls = synchronized(callLock) {
            val snapshot = inFlightCalls.values.toList()
            inFlightCalls.clear()
            earlyCancellations.clear()
            snapshot
        }
        calls.forEach { it.cancel(CancellationException("MCP connection closed")) }
    }

    private fun cancelRequest(request: JsonObject) {
        val requestId = (request["params"] as? JsonObject)?.get("requestId") ?: return
        // AtomicReference.compareAndSet uses object identity. Parsing creates a
        // fresh JsonElement, so CAS-ing `requestId` directly would ignore every
        // valid cancellation even when its JSON value matched.
        val current = toolsSubscriptionId.get()
        if (current == requestId) {
            toolsSubscriptionId.compareAndSet(current, null)
        }
        val callKey = requestId.toString()
        val call = synchronized(callLock) {
            val active = inFlightCalls[callKey]
            if (active == null) {
                val now = System.nanoTime()
                pruneEarlyCancellations(now)
                earlyCancellations[callKey] = now
                while (earlyCancellations.size > MAX_EARLY_CANCELLATIONS) {
                    val oldest = earlyCancellations.entries.firstOrNull()?.key ?: break
                    earlyCancellations.remove(oldest)
                }
            }
            active
        }
        call?.cancel(CancellationException("MCP request cancelled by client"))
    }

    private fun registerCall(callKey: String, callJob: Job): Boolean = synchronized(callLock) {
        val now = System.nanoTime()
        pruneEarlyCancellations(now)
        if (earlyCancellations.remove(callKey) != null || inFlightCalls.containsKey(callKey)) {
            return@synchronized false
        }
        inFlightCalls[callKey] = callJob
        true
    }

    private fun unregisterCall(callKey: String, callJob: Job) {
        synchronized(callLock) {
            if (inFlightCalls[callKey] === callJob) {
                inFlightCalls.remove(callKey)
            }
        }
    }

    private fun pruneEarlyCancellations(now: Long) {
        val iterator = earlyCancellations.entries.iterator()
        while (iterator.hasNext()) {
            val entry = iterator.next()
            if (now - entry.value <= EARLY_CANCELLATION_TTL_NANOS) break
            iterator.remove()
        }
    }

    private fun result(id: JsonElement, value: JsonObject): String = buildJsonObject {
        put("jsonrpc", "2.0")
        put("id", id)
        put("result", value)
    }.toString()

    private fun validRequestId(id: JsonElement): Boolean = when (id) {
        JsonNull -> true
        is JsonPrimitive -> id.isString || id.booleanOrNull == null
        else -> false
    }

    private fun error(id: JsonElement, code: Int, message: String): String = buildJsonObject {
        put("jsonrpc", "2.0")
        put("id", id)
        putJsonObject("error") {
            put("code", code)
            put("message", message)
        }
    }.toString()

}
