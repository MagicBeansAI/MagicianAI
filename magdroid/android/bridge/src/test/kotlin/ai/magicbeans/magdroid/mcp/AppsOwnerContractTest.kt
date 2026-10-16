package ai.magicbeans.magdroid.mcp

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Test

class AppsOwnerContractTest {
    private val target = "com.example.allowed"
    private val digest = "a".repeat(64)

    @Test
    fun `owner roster is exactly the frozen eight tool action pairs`() {
        assertEquals(
            linkedMapOf(
                "android_get_ui_tree" to AppsOwnerAction.Snapshot,
                "android_screenshot" to AppsOwnerAction.Screenshot,
                "android_launch_app" to AppsOwnerAction.Launch,
                "android_close_app" to AppsOwnerAction.Close,
                "android_tap" to AppsOwnerAction.Tap,
                "android_input_text" to AppsOwnerAction.Type,
                "android_press_key" to AppsOwnerAction.Key,
                "android_swipe" to AppsOwnerAction.Scroll,
            ),
            appsOwnerToolActions,
        )
    }

    @Test
    fun `each frozen tool accepts only its exact claim and arguments`() {
        appsOwnerToolActions.forEach { (tool, action) ->
            val parsed = parseAppsOwnerClaim(tool, exactArguments(action))
            assertNotNull(parsed)
            assertEquals(action, parsed?.action)
            assertEquals(if (action == AppsOwnerAction.Snapshot) null else target, parsed?.targetPackage)
        }
    }

    @Test
    fun `action substitution and owner claim on any other tool fail closed`() {
        val screenshot = exactArguments(AppsOwnerAction.Screenshot)
        val substitutedClaim = JsonObject(
            screenshot.getValue(APPS_OWNER_ARGUMENT).let { it as JsonObject } +
                ("action" to JsonPrimitive("snapshot")),
        )
        val substituted = JsonObject(
            screenshot + (APPS_OWNER_ARGUMENT to substitutedClaim),
        )
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim("android_screenshot", substituted)
        }
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim("android_open_url", exactArguments(AppsOwnerAction.Launch))
        }
    }

    @Test
    fun `legacy selectors extras and partially bound observations fail closed`() {
        val tap = exactArguments(AppsOwnerAction.Tap)
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim(
                "android_tap",
                JsonObject(tap + ("text" to JsonPrimitive("ambient selector"))),
            )
        }

        val type = exactArguments(AppsOwnerAction.Type)
        val claim = type.getValue(APPS_OWNER_ARGUMENT) as JsonObject
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim(
                "android_input_text",
                JsonObject(type + (APPS_OWNER_ARGUMENT to JsonObject(claim + ("observation" to JsonNull)))),
            )
        }
    }

    @Test
    fun `target package and geometry cannot be substituted`() {
        val scroll = exactArguments(AppsOwnerAction.Scroll)
        val claim = scroll.getValue(APPS_OWNER_ARGUMENT) as JsonObject
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim(
                "android_swipe",
                JsonObject(
                    scroll + (
                        APPS_OWNER_ARGUMENT to
                            JsonObject(claim + ("target_package" to JsonPrimitive("com.example.other")))
                    ),
                ),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            parseAppsOwnerClaim(
                "android_swipe",
                JsonObject(scroll + ("end_x" to JsonPrimitive(1080))),
            )
        }
    }

    @Test
    fun `uniform receipt hashes the exact returned payload and has no extra fields`() {
        val claim = requireNotNull(
            parseAppsOwnerClaim(
                "android_get_ui_tree",
                exactArguments(AppsOwnerAction.Snapshot),
            ),
        )
        val receipt = appsOwnerResultReceipt(
            claim = claim,
            exactPayload = "abc".toByteArray(Charsets.UTF_8),
            foregroundPackage = target,
            snapshotSha256 = digest,
            width = 1080,
            height = 2400,
            evidenceNodes = 1,
            truncated = false,
        )
        assertEquals(
            setOf(
                "schema",
                "permit_nonce",
                "action",
                "target_package",
                "foreground_package",
                "snapshot_sha256",
                "width",
                "height",
                "evidence_bytes",
                "evidence_nodes",
                "truncated",
                "outcome",
                "result_sha256",
            ),
            receipt.keys,
        )
        assertEquals(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            receipt.getValue("result_sha256").jsonPrimitive.content,
        )
        assertEquals(JsonNull, receipt.getValue("target_package"))
        assertEquals("settled", receipt.getValue("outcome").jsonPrimitive.content)
    }

    private fun exactArguments(action: AppsOwnerAction): JsonObject {
        val observationRequired = action in setOf(
            AppsOwnerAction.Tap,
            AppsOwnerAction.Type,
            AppsOwnerAction.Key,
            AppsOwnerAction.Scroll,
        )
        val claim = buildJsonObject {
            put("schema", APPS_OWNER_SCHEMA)
            put("permit_nonce", "permit_nonce_1234567890")
            put("action", action.wireName)
            putJsonArray("allowed_packages") { add(JsonPrimitive(target)) }
            put("target_package", if (action == AppsOwnerAction.Snapshot) JsonNull else JsonPrimitive(target))
            if (observationRequired) {
                putJsonObject("observation") {
                    put("foreground_package", target)
                    put("snapshot_sha256", digest)
                    put("width", 1080)
                    put("height", 2400)
                }
            } else {
                put("observation", JsonNull)
            }
            put("max_result_bytes", if (action == AppsOwnerAction.Screenshot) 8 * 1024 * 1024 else 32 * 1024)
            put("max_evidence_nodes", if (action == AppsOwnerAction.Snapshot) 4096 else 1)
        }
        return buildJsonObject {
            when (action) {
                AppsOwnerAction.Snapshot -> {
                    put("filter", "interactive")
                    put("max_depth", 50)
                }
                AppsOwnerAction.Screenshot -> put("quality", "full")
                AppsOwnerAction.Launch -> {
                    put("package_name", target)
                    put("clear_task", false)
                }
                AppsOwnerAction.Close -> {
                    put("package_name", target)
                    put("force", false)
                }
                AppsOwnerAction.Tap -> {
                    put("x", 540)
                    put("y", 1200)
                }
                AppsOwnerAction.Type -> {
                    put("text", "reviewed text")
                    put("append", false)
                }
                AppsOwnerAction.Key -> put("key", "enter")
                AppsOwnerAction.Scroll -> {
                    put("start_x", 540)
                    put("start_y", 1200)
                    put("end_x", 540)
                    put("end_y", 400)
                    put("duration_ms", 300)
                }
            }
            put(APPS_OWNER_ARGUMENT, claim)
        }
    }
}
