package ai.magicbeans.magdroid.protection

import ai.magicbeans.magdroid.mcp.McpContentBlock
import ai.magicbeans.magdroid.mcp.McpToolCallResult
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * What protection means for each tool, decided once, as data.
 *
 * Three dispositions, because the tools leak in different ways:
 *
 * - [Disposition.FOREGROUND_GATED] — everything that observes the visible
 *   screen or acts on it. Refused outright while a protected app is
 *   foregrounded: a tap into a banking app is as sensitive as a screenshot
 *   of it.
 * - [Disposition.NOTIFICATION_FILTERED] — the tools that read notifications
 *   and toasts. Not refused: entries whose *source* package is protected are
 *   dropped instead, because an OTP notification arrives while any app is
 *   foregrounded, and refusing the whole tool would just teach callers to
 *   poll harder.
 * - [Disposition.UNGATED] — app launch/close/list, URL opening, clipboard,
 *   device metadata, and global navigation (back/home/recents). Launching a
 *   protected app is the owner's benefit; the agent simply cannot see inside
 *   it once it is up.
 */
object ProtectionGate {

    enum class Disposition { FOREGROUND_GATED, NOTIFICATION_FILTERED, UNGATED }

    fun classify(toolName: String): Disposition = when (toolName) {
        // Observing the visible screen…
        "android_get_ui_tree",
        "android_screenshot",
        "android_find_elements",
        "android_get_screen_context",
        "android_accessibility_audit",
        "android_screenshot_diff",
        // …acting on it…
        "android_tap",
        "android_long_press",
        "android_double_tap",
        "android_swipe",
        "android_pinch",
        "android_drag",
        "android_input_text",
        "android_press_key",
        // …and waiting on its contents, which reads the same tree.
        "android_wait_for_element",
        "android_wait_for_gone",
        "android_wait_for_idle",
        "android_scroll_to_element",
        -> Disposition.FOREGROUND_GATED

        "android_get_notifications",
        "android_await_otp",
        "android_get_recent_toasts",
        -> Disposition.NOTIFICATION_FILTERED

        else -> Disposition.UNGATED
    }

    /**
     * The refusal a gated tool returns while a protected app is foregrounded.
     *
     * Deliberately **not** `isError`: the bridge projects an MCP error into a
     * bare error string, which would flatten the refusal into "device error".
     * A success-shaped result with `app_protected` in `structuredContent`
     * survives projection intact, so Magician can map it to a clear
     * agent-facing refusal and an audit verdict.
     */
    fun refusal(packageName: String): McpToolCallResult = McpToolCallResult(
        content = listOf(
            McpContentBlock(
                type = "text",
                text = "`$packageName` is a protected app on this device. " +
                    "The owner controls the list under Settings > Protected apps.",
            ),
        ),
        isError = false,
        structuredContent = buildJsonObject {
            put("app_protected", true)
            put("foreground_package", packageName)
        },
    )

    /**
     * The refusal for a system surface that shows protected content under an
     * unprotected package name: recents (live thumbnails of every recent app,
     * owned by the launcher) and the notification shade (the text the
     * notification tools filter, owned by systemui). While anything at all is
     * protected, agent-driven opening of these surfaces is refused — a
     * package-keyed gate cannot tell which pixels inside them are whose.
     */
    fun surfaceRefusal(surface: String): McpToolCallResult = McpToolCallResult(
        content = listOf(
            McpContentBlock(
                type = "text",
                text = "Opening `$surface` is refused while protected apps exist on this " +
                    "device: it shows their content under a system package the protection " +
                    "gate cannot attribute. The owner controls the protected list under " +
                    "Settings > Protected apps.",
            ),
        ),
        isError = false,
        structuredContent = buildJsonObject {
            put("app_protected", true)
            put("protected_surface", surface)
        },
    )
}
