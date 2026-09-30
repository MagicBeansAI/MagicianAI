package ai.magicbeans.magdroid.mcp

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*

// JSON-RPC 2.0 error codes
object JsonRpcErrorCodes {
    const val PARSE_ERROR = -32700
    const val INVALID_REQUEST = -32600
    const val METHOD_NOT_FOUND = -32601
    const val INVALID_PARAMS = -32602
    const val INTERNAL_ERROR = -32603
}

@Serializable
data class JsonRpcRequest(
    val jsonrpc: String = "2.0",
    val id: JsonElement? = null,
    val method: String,
    val params: JsonElement? = null
)

@Serializable
data class JsonRpcResponse(
    val jsonrpc: String = "2.0",
    val id: JsonElement? = null,
    val result: JsonElement? = null,
    val error: JsonRpcError? = null
)

@Serializable
data class JsonRpcError(
    val code: Int,
    val message: String,
    val data: JsonElement? = null
)

@Serializable
data class McpToolDefinition(
    val name: String,
    val description: String,
    val inputSchema: JsonObject,
    val annotations: McpToolAnnotations = McpToolAnnotations.forTool(name),
)

/**
 * Standard MCP tool hints. These are descriptive hints, never device-side
 * authorization: Magician's scoped tool policy remains the authority for
 * whether one of the four public Android verbs may be called.
 */
@Serializable
data class McpToolAnnotations(
    val readOnlyHint: Boolean,
    val destructiveHint: Boolean,
    val idempotentHint: Boolean,
    val openWorldHint: Boolean,
) {
    companion object {
        private val readOnly = setOf(
            "android_get_ui_tree",
            "android_screenshot",
            "android_find_elements",
            "android_get_screen_context",
            "android_get_notifications",
            "android_screenshot_diff",
            "android_accessibility_audit",
            "android_get_recent_toasts",
            "android_list_apps",
            "android_wait_for_element",
            "android_wait_for_gone",
            "android_wait_for_idle",
            "android_list_devices",
            "android_search_tools",
            "android_describe_tools",
            "android_get_device_info",
        )

        private val idempotentMutations = setOf(
            "android_close_app",
            "android_set_clipboard",
            "android_select_device",
            "android_enable_events",
            // Answers a pending ask over the device's credential: a write,
            // but first-response-wins makes a repeat a no-op.
            "android_await_otp",
        )

        fun forTool(name: String): McpToolAnnotations {
            val isReadOnly = name in readOnly
            return McpToolAnnotations(
                readOnlyHint = isReadOnly,
                // Unknown and newly added acting tools fail conservative. A
                // maintainer must explicitly prove a mutation reversible before
                // relaxing this hint.
                destructiveHint = !isReadOnly && name !in idempotentMutations,
                idempotentHint = isReadOnly || name in idempotentMutations,
                openWorldHint = !isReadOnly,
            )
        }
    }

    fun toJson(): JsonObject = buildJsonObject {
        put("readOnlyHint", readOnlyHint)
        put("destructiveHint", destructiveHint)
        put("idempotentHint", idempotentHint)
        put("openWorldHint", openWorldHint)
    }
}

@Serializable
data class McpToolCallResult(
    val content: List<McpContentBlock>,
    val isError: Boolean = false,
    /**
     * MCP `structuredContent`: machine-readable results beside the content
     * blocks. The bridge projects it through verbatim, which makes it the
     * channel for facts Magician needs without disturbing the text payloads
     * agents read — the foreground package for the device-action audit, and
     * the `app_protected` refusal verdict.
     */
    val structuredContent: JsonObject? = null
)

@Serializable
data class McpContentBlock(
    val type: String,
    val text: String? = null,
    val data: String? = null,
    val mimeType: String? = null
)

// Helper functions
fun successResponse(id: JsonElement?, result: JsonObject): JsonRpcResponse =
    JsonRpcResponse(id = id, result = result)

fun errorResponse(id: JsonElement?, code: Int, message: String): JsonRpcResponse =
    JsonRpcResponse(id = id, error = JsonRpcError(code = code, message = message))

fun textResult(text: String): McpToolCallResult =
    McpToolCallResult(content = listOf(McpContentBlock(type = "text", text = text)))

fun errorResult(message: String): McpToolCallResult =
    McpToolCallResult(content = listOf(McpContentBlock(type = "text", text = message)), isError = true)

fun imageResult(base64Data: String, mimeType: String = "image/jpeg", metadata: String? = null): McpToolCallResult {
    val blocks = mutableListOf(McpContentBlock(type = "image", data = base64Data, mimeType = mimeType))
    if (metadata != null) blocks.add(McpContentBlock(type = "text", text = metadata))
    return McpToolCallResult(content = blocks)
}
