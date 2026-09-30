package ai.magicbeans.magdroid.mcp

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The wire contract, checked without a phone.
 *
 * These assert the shapes a real MCP client parses, which is why they read
 * fields out of raw JSON rather than round-tripping through our own types — a
 * serializer and its matching deserializer agree with each other even when both
 * are wrong about the protocol.
 */
class MagdroidMcpServerTest {

    private val json = Json { ignoreUnknownKeys = true }

    // Constructed per test since the server became a class with an injectable
    // tool source; it holds subscription state and sharing one across tests
    // would let a `tools/list_changed` subscription leak between them.
    private val server = MagdroidMcpServer()

    /** Records what the server asked the device to do. */
    private class Recorder(
        private val result: McpToolCallResult = McpToolCallResult(
            content = listOf(McpContentBlock(type = "text", text = "ok")),
        ),
        private val boom: Throwable? = null,
    ) {
        var calls = 0
            private set
        var lastName: String? = null
            private set
        var lastArguments: JsonObject? = null
            private set

        val invoke: McpInvoke = { name, arguments ->
            calls += 1
            lastName = name
            lastArguments = arguments
            boom?.let { throw it }
            result
        }
    }

    private fun reply(text: String, recorder: Recorder = Recorder()): JsonObject {
        val outcome = runBlocking { server.handle(text, recorder.invoke) }
        assertTrue("expected a reply, got $outcome", outcome is MagdroidMcpServer.Outcome.Reply)
        return json.parseToJsonElement((outcome as MagdroidMcpServer.Outcome.Reply).text).jsonObject
    }

    private fun outcome(text: String, recorder: Recorder = Recorder()) =
        runBlocking { server.handle(text, recorder.invoke) }

    // ── Frames that are not requests ─────────────────────────────────────────

    /**
     * The socket is MCP-only now.
     *
     * This file previously asserted that a bridge envelope came back as
     * `NotMcp`, so the two dialects could share the socket while the fleet
     * updated. That fallback has since been removed deliberately — the envelope
     * is gone and unparseable input is answered as a parse error rather than
     * handed to a second decoder.
     */
    @Test
    fun `unparseable input is answered, not ignored`() {
        val body = reply("not json at all")
        assertEquals(-32700, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
    }

    @Test
    fun `a notification is answered with nothing`() {
        val notification = """{"jsonrpc":"2.0","method":"notifications/something"}"""
        assertEquals(MagdroidMcpServer.Outcome.NoReply, outcome(notification))
    }

    @Test
    fun `an explicit null id is still a request and receives a response`() {
        val body = reply("""{"jsonrpc":"2.0","id":null,"method":"tools/list"}""")
        assertTrue(body.containsKey("result"))
        assertTrue(body["id"] is kotlinx.serialization.json.JsonNull)
    }

    @Test
    fun `boolean and object request ids are rejected`() {
        listOf("true", "{}", "[]").forEach { id ->
            val body = reply("""{"jsonrpc":"2.0","id":$id,"method":"tools/list"}""")
            assertEquals(-32600, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
        }
    }

    // ── server/discover ──────────────────────────────────────────────────────

    @Test
    fun `discover declares the version we implement`() {
        val body = reply("""{"jsonrpc":"2.0","id":1,"method":"server/discover"}""")
        val result = body["result"]!!.jsonObject

        assertEquals("2.0", body["jsonrpc"]!!.jsonPrimitive.content)
        assertEquals(1, body["id"]!!.jsonPrimitive.intOrNull)
        assertEquals("complete", result["resultType"]!!.jsonPrimitive.content)
        assertEquals(
            listOf("2026-07-28"),
            result["supportedVersions"]!!.jsonArray.map { it.jsonPrimitive.content },
        )
    }

    @Test
    fun `discover declares tools and nothing else`() {
        val result = reply("""{"jsonrpc":"2.0","id":1,"method":"server/discover"}""")["result"]!!
            .jsonObject
        val capabilities = result["capabilities"]!!.jsonObject

        // The bounded surface, asserted. A capability appearing here that the
        // server does not serve is a promise a client will act on.
        assertEquals(setOf("tools"), capabilities.keys)
    }

    /**
     * `listChanged` is claimed, and now it is true.
     *
     * This asserted `false` while the notification did not exist, because
     * declaring a capability nothing emits is a promise a client acts on. The
     * notification has since been implemented, so the declaration flipped with
     * it — which is the order that keeps it honest.
     */
    @Test
    fun `discover claims listChanged now that it is emitted`() {
        val result = reply("""{"jsonrpc":"2.0","id":1,"method":"server/discover"}""")["result"]!!
            .jsonObject
        val tools = result["capabilities"]!!.jsonObject["tools"]!!.jsonObject
        assertTrue(tools["listChanged"]!!.jsonPrimitive.booleanOrNull!!)
    }

    @Test
    fun `discover is cacheable and scoped to one owner`() {
        val result = reply("""{"jsonrpc":"2.0","id":1,"method":"server/discover"}""")["result"]!!
            .jsonObject

        assertTrue((result["ttlMs"]!!.jsonPrimitive.longOrNull ?: 0) > 0)
        // Public would let one handset's granted permissions be served as
        // another's, since the roster reflects what this owner has allowed.
        assertEquals("private", result["cacheScope"]!!.jsonPrimitive.content)
    }

    // ── tools/list ───────────────────────────────────────────────────────────

    /**
     * The roster is what this handset can actually do, not everything compiled in.
     *
     * `getAvailableTools` withholds the notification tools when no listener is
     * bound. Asserting against `getAllTools` — as this did — would demand the
     * server advertise capabilities the phone cannot honour, which is the same
     * lie in the other direction.
     */
    @Test
    fun `tools list carries the available registry with schemas`() {
        val result = reply("""{"jsonrpc":"2.0","id":2,"method":"tools/list"}""")["result"]!!
            .jsonObject
        val tools = result["tools"]!!.jsonArray

        assertEquals(McpToolRegistry.getAvailableTools().size, tools.size)
        assertTrue("the roster must not be empty", tools.isNotEmpty())
        tools.forEach { entry ->
            val tool = entry.jsonObject
            assertNotNull(tool["name"]!!.jsonPrimitive.contentOrNull)
            assertNotNull(tool["description"]!!.jsonPrimitive.contentOrNull)
            // Without a schema a client cannot construct a call at all.
            assertNotNull(tool["inputSchema"]!!.jsonObject)
            val annotations = tool["annotations"]!!.jsonObject
            assertNotNull(annotations["readOnlyHint"]!!.jsonPrimitive.booleanOrNull)
            assertNotNull(annotations["destructiveHint"]!!.jsonPrimitive.booleanOrNull)
            assertNotNull(annotations["idempotentHint"]!!.jsonPrimitive.booleanOrNull)
            assertNotNull(annotations["openWorldHint"]!!.jsonPrimitive.booleanOrNull)
        }
    }

    // ── subscriptions/listen ─────────────────────────────────────────────

    @Test
    fun `tools subscription is acknowledged with the originating request id`() {
        val body = reply(
            """{"jsonrpc":"2.0","id":"sub-1","method":"subscriptions/listen",
               "params":{"notifications":{"toolsListChanged":true}}}""",
        )

        assertEquals("notifications/subscriptions/acknowledged", body["method"]!!.jsonPrimitive.content)
        val params = body["params"]!!.jsonObject
        assertEquals(
            "sub-1",
            params["_meta"]!!.jsonObject["io.modelcontextprotocol/subscriptionId"]!!
                .jsonPrimitive.content,
        )
        assertTrue(
            params["notifications"]!!.jsonObject["toolsListChanged"]!!
                .jsonPrimitive.booleanOrNull!!,
        )
    }

    @Test
    fun `tools change notification stays correlated to the subscription`() {
        reply(
            """{"jsonrpc":"2.0","id":17,"method":"subscriptions/listen",
               "params":{"notifications":{"toolsListChanged":true}}}""",
        )

        val notification = json.parseToJsonElement(server.toolsChangedNotification()!!).jsonObject
        assertEquals("notifications/tools/list_changed", notification["method"]!!.jsonPrimitive.content)
        assertEquals(
            17,
            notification["params"]!!.jsonObject["_meta"]!!.jsonObject
                ["io.modelcontextprotocol/subscriptionId"]!!.jsonPrimitive.intOrNull,
        )
    }

    @Test
    fun `matching cancelled notification clears the subscription by value`() {
        reply(
            """{"jsonrpc":"2.0","id":"sub-cancel","method":"subscriptions/listen",
               "params":{"notifications":{"toolsListChanged":true}}}""",
        )
        assertNotNull(server.toolsChangedNotification())

        assertEquals(
            MagdroidMcpServer.Outcome.NoReply,
            outcome(
                """{"jsonrpc":"2.0","method":"notifications/cancelled",
                   "params":{"requestId":"sub-cancel","reason":"done"}}""",
            ),
        )
        assertEquals(null, server.toolsChangedNotification())
    }

    @Test
    fun `unsupported subscription filter is rejected without reserving the stream`() {
        val rejected = reply(
            """{"jsonrpc":"2.0","id":18,"method":"subscriptions/listen",
               "params":{"notifications":{"resourcesListChanged":true}}}""",
        )
        assertEquals(-32602, rejected["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)

        val accepted = reply(
            """{"jsonrpc":"2.0","id":19,"method":"subscriptions/listen",
               "params":{"notifications":{"toolsListChanged":true}}}""",
        )
        assertEquals("notifications/subscriptions/acknowledged", accepted["method"]!!.jsonPrimitive.content)
    }

    @Test
    fun `subscription requires a boolean and exact bounded filter`() {
        listOf(
            """{"toolsListChanged":"true"}""",
            """{"toolsListChanged":true,"resourcesListChanged":true}""",
        ).forEachIndexed { index, filter ->
            val body = reply(
                """{"jsonrpc":"2.0","id":${30 + index},"method":"subscriptions/listen",
                   "params":{"notifications":$filter}}""",
            )
            assertEquals(-32602, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
        }
    }

    @Test
    fun `oversized input is rejected before JSON parsing`() {
        val body = reply(" ".repeat(MagdroidMcpServer.MAX_REQUEST_BYTES + 1))
        assertEquals(-32600, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
    }

    // ── tools/call ───────────────────────────────────────────────────────────

    @Test
    fun `tools call reaches the device with its arguments`() {
        val recorder = Recorder()
        val body = reply(
            """{"jsonrpc":"2.0","id":3,"method":"tools/call",
               "params":{"name":"android_tap","arguments":{"x":10,"y":20}}}""",
            recorder,
        )

        assertEquals(1, recorder.calls)
        assertEquals("android_tap", recorder.lastName)
        assertEquals(10, recorder.lastArguments!!["x"]!!.jsonPrimitive.intOrNull)
        assertFalse(body["result"]!!.jsonObject["isError"]!!.jsonPrimitive.booleanOrNull!!)
    }

    @Test
    fun `a tool without arguments still runs`() {
        val recorder = Recorder()
        reply(
            """{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"android_screenshot"}}""",
            recorder,
        )
        assertEquals(1, recorder.calls)
    }

    /**
     * A tool that ran and failed is a result, not a protocol error.
     *
     * The distinction is the whole reason a caller can tell "the phone refused"
     * from "the tap missed", and only one of those is worth retrying.
     */
    @Test
    fun `a failed tool is a result carrying isError`() {
        val recorder = Recorder(
            result = McpToolCallResult(
                content = listOf(McpContentBlock(type = "text", text = "no such element")),
                isError = true,
            ),
        )
        val body = reply(
            """{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"android_tap"}}""",
            recorder,
        )

        assertTrue(body["error"] == null)
        assertTrue(body["result"]!!.jsonObject["isError"]!!.jsonPrimitive.booleanOrNull!!)
    }

    @Test
    fun `a throwing tool becomes an internal error, not a crash`() {
        val recorder = Recorder(boom = IllegalStateException("accessibility is off"))
        val body = reply(
            """{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"android_tap"}}""",
            recorder,
        )

        assertEquals(-32603, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
        assertTrue(
            body["error"]!!.jsonObject["message"]!!.jsonPrimitive.content
                .contains("accessibility is off"),
        )
    }

    @Test
    fun `tools call without a name is refused before reaching the device`() {
        val recorder = Recorder()
        val body = reply(
            """{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{}}""",
            recorder,
        )

        assertEquals(-32602, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
        assertEquals("nothing should have been dispatched", 0, recorder.calls)
    }

    @Test
    fun `tools call refuses non-object arguments before reaching the device`() {
        val recorder = Recorder()
        val body = reply(
            """{"jsonrpc":"2.0","id":71,"method":"tools/call",
               "params":{"name":"android_tap","arguments":"x=10"}}""",
            recorder,
        )

        assertEquals(-32602, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
        assertEquals(0, recorder.calls)
    }

    // ── The closed boundary ──────────────────────────────────────────────────

    @Test
    fun `everything outside the surface is refused explicitly`() {
        // `initialize` is in this list on purpose: answering it would let a
        // client fall back to the stateful model the migration exists to leave.
        listOf("initialize", "resources/list", "prompts/list", "sampling/createMessage")
            .forEach { method ->
                val body = reply("""{"jsonrpc":"2.0","id":8,"method":"$method"}""")
                assertEquals(
                    "$method should be refused",
                    -32601,
                    body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull,
                )
            }
    }

    @Test
    fun `a request without a method is an invalid request`() {
        val body = reply("""{"jsonrpc":"2.0","id":9}""")
        assertEquals(-32600, body["error"]!!.jsonObject["code"]!!.jsonPrimitive.intOrNull)
    }

    @Test
    fun `a string id is echoed as a string`() {
        // Ids are opaque: a client using uuids must get its own id back in the
        // same type, or it cannot correlate the response.
        val body = reply("""{"jsonrpc":"2.0","id":"abc-123","method":"tools/list"}""")
        assertEquals("abc-123", body["id"]!!.jsonPrimitive.content)
        assertTrue(body["id"]!!.jsonPrimitive.isString)
    }
}
