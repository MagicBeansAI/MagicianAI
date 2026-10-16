package ai.magicbeans.magdroid.mcp

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class McpToolRegistryTest {

    private val tools = McpToolRegistry.getAllTools()

    @Test
    fun `registry returns at least 30 tools`() {
        assertTrue("Expected ≥30 tools, got ${tools.size}", tools.size >= 30)
    }

    @Test
    fun `all tool names have android_ prefix`() {
        tools.forEach { tool ->
            assertTrue(
                "Tool '${tool.name}' missing android_ prefix",
                tool.name.startsWith("android_")
            )
        }
    }

    @Test
    fun `no duplicate tool names`() {
        val names = tools.map { it.name }
        val unique = names.toSet()
        assertEquals("Duplicate tool names found: ${names.groupBy { it }.filter { it.value.size > 1 }.keys}",
            unique.size, names.size)
    }

    @Test
    fun `all tools have non-empty descriptions`() {
        tools.forEach { tool ->
            assertTrue("Tool '${tool.name}' has empty description", tool.description.isNotBlank())
        }
    }

    @Test
    fun `all tools have valid inputSchema`() {
        tools.forEach { tool ->
            val schema = tool.inputSchema
            assertEquals("Tool '${tool.name}' inputSchema missing type:object",
                "object", schema["type"]?.jsonPrimitive?.content)
            assertNotNull("Tool '${tool.name}' inputSchema missing properties",
                schema["properties"])
        }
    }

    @Test
    fun `every tool has a conservative complete annotation set`() {
        tools.forEach { tool ->
            val hints = tool.annotations
            if (hints.readOnlyHint) {
                assertFalse("read-only '${tool.name}' cannot be destructive", hints.destructiveHint)
                assertTrue("read-only '${tool.name}' must be safe to repeat", hints.idempotentHint)
                assertFalse("read-only '${tool.name}' should not claim open-world mutation", hints.openWorldHint)
            } else {
                assertTrue("acting '${tool.name}' must conservatively claim open-world effects", hints.openWorldHint)
            }
        }
    }

    @Test
    fun `representative observe and act annotations do not drift`() {
        val screenshot = requireNotNull(McpToolRegistry.getTool("android_screenshot"))
        assertTrue(screenshot.annotations.readOnlyHint)
        assertFalse(screenshot.annotations.destructiveHint)
        assertTrue(screenshot.annotations.idempotentHint)

        val tap = requireNotNull(McpToolRegistry.getTool("android_tap"))
        assertFalse(tap.annotations.readOnlyHint)
        assertTrue(tap.annotations.destructiveHint)
        assertFalse(tap.annotations.idempotentHint)
        assertTrue(tap.annotations.openWorldHint)

        val close = requireNotNull(McpToolRegistry.getTool("android_close_app"))
        assertFalse(close.annotations.readOnlyHint)
        assertFalse(close.annotations.destructiveHint)
        assertTrue(close.annotations.idempotentHint)

        // Answers the runtime's pending ask over the device's own credential:
        // a write (never read-only), harmless to repeat.
        val awaitOtp = requireNotNull(McpToolRegistry.getTool("android_await_otp"))
        assertFalse(awaitOtp.annotations.readOnlyHint)
        assertFalse(awaitOtp.annotations.destructiveHint)
        assertTrue(awaitOtp.annotations.idempotentHint)
        assertTrue(awaitOtp.annotations.openWorldHint)
    }

    @Test
    fun `getTool returns correct tool by name`() {
        val tap = McpToolRegistry.getTool("android_tap")
        assertNotNull(tap)
        assertEquals("android_tap", tap!!.name)
    }

    @Test
    fun `getTool returns null for unknown name`() {
        assertNull(McpToolRegistry.getTool("android_nonexistent_tool"))
    }

    @Test
    fun `core gesture tools are present`() {
        val coreTools = listOf(
            "android_tap", "android_swipe", "android_long_press", "android_double_tap",
            "android_pinch", "android_drag"
        )
        coreTools.forEach { name ->
            assertNotNull("Missing core tool: $name", McpToolRegistry.getTool(name))
        }
    }

    @Test
    fun `core observe tools are present`() {
        val observeTools = listOf(
            "android_get_ui_tree", "android_screenshot", "android_find_elements",
            "android_get_screen_context", "android_get_notifications"
        )
        observeTools.forEach { name ->
            assertNotNull("Missing observe tool: $name", McpToolRegistry.getTool(name))
        }
    }

    @Test
    fun `core wait tools are present`() {
        val waitTools = listOf(
            "android_wait_for_element", "android_wait_for_gone", "android_wait_for_idle"
        )
        waitTools.forEach { name ->
            assertNotNull("Missing wait tool: $name", McpToolRegistry.getTool(name))
        }
    }

    @Test
    fun `tools with required params have required array`() {
        // android_swipe requires start_x, start_y, end_x, end_y
        val swipe = McpToolRegistry.getTool("android_swipe")
        assertNotNull(swipe)
        val required = swipe!!.inputSchema["required"]?.jsonArray
        assertNotNull("android_swipe should have required array", required)
        val requiredNames = required!!.map { it.jsonPrimitive.content }
        assertTrue("start_x required", "start_x" in requiredNames)
        assertTrue("start_y required", "start_y" in requiredNames)
        assertTrue("end_x required", "end_x" in requiredNames)
        assertTrue("end_y required", "end_y" in requiredNames)
    }

    @Test
    fun `android_input_text has required text field`() {
        val inputText = McpToolRegistry.getTool("android_input_text")
        assertNotNull(inputText)
        val required = inputText!!.inputSchema["required"]?.jsonArray
            ?.map { it.jsonPrimitive.content } ?: emptyList()
        assertTrue("text should be required for android_input_text", "text" in required)
    }
}
