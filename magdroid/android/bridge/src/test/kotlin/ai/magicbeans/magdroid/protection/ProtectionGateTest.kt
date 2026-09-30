package ai.magicbeans.magdroid.protection

import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ProtectionGateTest {

    /**
     * The classification table, pinned tool by tool. A new tool that observes
     * or acts on the screen must be added HERE as gated before it is added to
     * the dispatch table — an unlisted tool defaults to UNGATED, which is the
     * safe default for metadata tools and the wrong one for anything that can
     * see a protected app.
     */
    @Test
    fun classification_covers_the_dispatch_table() {
        val gated = listOf(
            "android_get_ui_tree", "android_screenshot", "android_find_elements",
            "android_get_screen_context", "android_accessibility_audit",
            "android_screenshot_diff",
            "android_tap", "android_long_press", "android_double_tap",
            "android_swipe", "android_pinch", "android_drag",
            "android_input_text", "android_press_key",
            "android_wait_for_element", "android_wait_for_gone",
            "android_wait_for_idle", "android_scroll_to_element",
        )
        val filtered = listOf(
            "android_get_notifications", "android_await_otp", "android_get_recent_toasts",
        )
        val ungated = listOf(
            "android_launch_app", "android_close_app", "android_list_apps",
            "android_open_url", "android_set_clipboard", "android_global_action",
            "android_list_devices", "android_select_device",
            "android_search_tools", "android_describe_tools",
            "android_enable_events", "android_get_device_info",
        )
        gated.forEach {
            assertEquals(it, ProtectionGate.Disposition.FOREGROUND_GATED, ProtectionGate.classify(it))
        }
        filtered.forEach {
            assertEquals(it, ProtectionGate.Disposition.NOTIFICATION_FILTERED, ProtectionGate.classify(it))
        }
        ungated.forEach {
            assertEquals(it, ProtectionGate.Disposition.UNGATED, ProtectionGate.classify(it))
        }
    }

    /**
     * The invariant the hardcoded table above cannot hold: every tool the
     * device actually advertises must classify as something DELIBERATE. A
     * new tool added to the registry without a decision here lands in the
     * allowlist assertion below and fails — UNGATED-by-omission is exactly
     * how a new screen-reading tool would ship unprotected.
     */
    @Test
    fun every_registry_tool_has_a_deliberate_classification() {
        val deliberatelyUngated = setOf(
            "android_launch_app", "android_close_app", "android_list_apps",
            "android_open_url", // gated inside the handler by deep-link target
            "android_set_clipboard",
            "android_global_action", // recents/notifications gated in-handler
            "android_list_devices", "android_select_device",
            "android_search_tools", "android_describe_tools",
            "android_enable_events", "android_get_device_info",
        )
        val advertised = ai.magicbeans.magdroid.mcp.McpToolRegistry.getAllTools().map { it.name }
        assertTrue("registry must not be empty", advertised.isNotEmpty())
        advertised.forEach { name ->
            val disposition = ProtectionGate.classify(name)
            if (disposition == ProtectionGate.Disposition.UNGATED) {
                assertTrue(
                    "`$name` is UNGATED but not in the deliberate allowlist — " +
                        "decide its protection disposition before shipping it",
                    name in deliberatelyUngated,
                )
            }
        }
    }

    /**
     * The refusal must be success-shaped: an `isError` result is flattened by
     * the bridge projection into a bare "device error" string, and the
     * structured verdict Magician's audit needs would not survive.
     */
    @Test
    fun refusal_is_success_shaped_and_structured() {
        val refusal = ProtectionGate.refusal("com.bank.app")
        assertFalse(refusal.isError)
        assertTrue(
            refusal.structuredContent?.get("app_protected")?.jsonPrimitive?.booleanOrNull == true
        )
        assertEquals(
            "com.bank.app",
            refusal.structuredContent?.get("foreground_package")?.jsonPrimitive?.contentOrNull
        )
        assertTrue(refusal.content.first().text!!.contains("com.bank.app"))
    }
}
