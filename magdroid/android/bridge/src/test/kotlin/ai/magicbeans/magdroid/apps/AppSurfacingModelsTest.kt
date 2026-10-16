package ai.magicbeans.magdroid.apps

import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import io.ktor.utils.io.ByteReadChannel
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

class AppSurfacingModelsTest {
    private val digest = "blake3:" + "0".repeat(64)

    @Test
    fun `slot identity includes the full page and cannot collide by region`() {
        val home = AppSlotId.forPageRegion("/", "primary")
        val observe = AppSlotId.forPageRegion("/observe", "primary")

        assertEquals("page:2f:primary", home.value)
        assertEquals("page:2f6f627365727665:primary", observe.value)
        assertNotEquals(home, observe)
        assertThrows(IllegalArgumentException::class.java) { AppSlotId.forPageRegion("/t/:name", "primary") }
    }

    @Test
    fun `widget response closes tags targets and unknown members`() {
        val target = AppWidgetRenderTarget("installation-1", "summary")
        val response = appSurfacingJson.decodeFromString(
            AppWidgetRenderBatchResponse.serializer(),
            """{
              "schema_version":1,
              "revision":"$digest",
              "etag":"$digest",
              "rendered_at":"2026-09-02T00:00:00Z",
              "refresh_after":"2026-09-02T00:00:30Z",
              "widgets":[{
                "installation_id":"installation-1",
                "widget_id":"summary",
                "title":"Summary",
                "installation_generation":2,
                "revision":"$digest",
                "rendered_at":"2026-09-02T00:00:00Z",
                "refresh_after":"2026-09-02T00:00:30Z",
                "state":"ready",
                "model":{
                  "model":"list",
                  "rows":[{
                    "entity":"item",
                    "record_id":"record-1",
                    "record_revision":1,
                    "fields":{"title":"Ready"}
                  }],
                  "hints":{"display_field":"title"},
                  "actions":[{"action_id":"confirm","label":"Confirm"}]
                }
              }]
            }""",
        ).checked(listOf(target))

        assertEquals("Ready", response.widgets.single().model?.rows?.single()?.fields?.get("title")?.appWidgetDisplay())
        assertThrows(IllegalArgumentException::class.java) { response.checked(listOf(target.copy(widgetId = "other"))) }
        assertThrows(SerializationException::class.java) {
            appSurfacingJson.decodeFromString(
                AppIndicatorListResponse.serializer(),
                """{"schema_version":1,"revision":"$digest","etag":"e","generated_at":"now","indicators":[],"extra":true}""",
            )
        }
    }

    @Test
    fun `empty hidden slot does not manufacture a widget target`() {
        val slot = AppSlotId.forPageRegion("/", "primary")
        val assignment = appSurfacingJson.decodeFromString(
            AppResolvedSlotAssignmentWire.serializer(),
            """{
              "slot_id":"${slot.value}",
              "pinned_system_default":false,
              "opted_out":false
            }""",
        ).checked(slot)

        assertNull(assignment.target)
    }

    @Test
    fun `resolved slot uses only the current exact widget binding`() {
        val slot = AppSlotId.forPageRegion("/", "primary")
        val binding = """{
          "package":{
            "installation_id":"installation-1",
            "package_id":"package:claims",
            "package_revision_ref":"revision:claims-1",
            "package_content_digest":"$digest",
            "installation_generation":3
          },
          "widget_id":"review_queue"
        }"""
        val assignment = appSurfacingJson.decodeFromString(
            AppResolvedSlotAssignmentWire.serializer(),
            """{
              "slot_id":"${slot.value}",
              "source":"user",
              "pinned_system_default":false,
              "opted_out":false,
              "widget":{
                "pinned":$binding,
                "current":$binding,
                "restored_across_generation":false,
                "assignment_compatibility":"exact_digest_only"
              }
            }""",
        ).checked(slot)

        assertEquals(AppWidgetRenderTarget("installation-1", "review_queue"), assignment.target)
        assertEquals("revision:claims-1", assignment.packageRevisionRef)
        assertEquals(digest, assignment.packageContentDigest)
        assertEquals(3L, assignment.installationGeneration)
        val item = AppWidgetRenderItem(
            installationId = "installation-1",
            widgetId = "review_queue",
            installationGeneration = 3,
            revision = digest,
            renderedAt = "2026-09-02T00:00:00Z",
            refreshAfter = "2026-09-02T00:00:30Z",
            state = "ready",
            model = AppWidgetNativeModel(
                model = "detail",
                hints = AppWidgetRenderHints(),
                actions = listOf(AppWidgetGovernedAction("confirm", "Confirm")),
            ),
        )
        val actionKey = AppSurfacingViewModel.actionKey("owner-a", assignment, item, "confirm")
        assertNotEquals(actionKey, AppSurfacingViewModel.actionKey("owner-b", assignment, item, "confirm"))
        assertNull(AppSurfacingViewModel.actionKey("owner-a", assignment, item.copy(installationGeneration = 4), "confirm"))
    }

    @Test
    fun `slot batch is bound one for one in requested order`() {
        val primary = AppSlotId.forPageRegion("/", "primary")
        val secondary = AppSlotId.forPageRegion("/", "secondary")
        val response = AppSlotResolutionBatchResponse(listOf(
            AppResolvedSlotAssignmentWire(primary.value, pinnedSystemDefault = false, optedOut = false),
            AppResolvedSlotAssignmentWire(secondary.value, pinnedSystemDefault = false, optedOut = false),
        ))

        assertEquals(listOf(primary, secondary), response.checked(listOf(primary, secondary)).map { it.slotId })
        assertThrows(IllegalArgumentException::class.java) {
            response.checked(listOf(secondary, primary))
        }
    }

    @Test
    fun `indicator badge and scalar values stay bounded`() {
        AppIndicatorModel(kind = "badge", count = 9_999).checked()
        assertThrows(IllegalArgumentException::class.java) {
            AppIndicatorModel(kind = "badge", count = 10_000).checked()
        }
        assertEquals("2 items", JsonPrimitive("2 items").appWidgetDisplay())
        assertEquals(
            "title Ready",
            JsonObject(mapOf("title" to JsonPrimitive("Ready"))).appWidgetDisplay(),
        )
    }

    @Test
    fun `foreground cadence honors widget deadline without a retry loop`() {
        val now = Instant.parse("2026-09-02T00:00:00Z")
        assertEquals(250L, AppSurfacingViewModel.nextForegroundDelay(now, now.minusSeconds(1)))
        assertEquals(12_000L, AppSurfacingViewModel.nextForegroundDelay(now, now.plusSeconds(12)))
        assertEquals(30_000L, AppSurfacingViewModel.nextForegroundDelay(now, now.plusSeconds(90)))
        assertEquals(30_000L, AppSurfacingViewModel.nextForegroundDelay(now, null))
        assertNotEquals(
            AppSurfacingViewModel.pageCacheKey("/", listOf(AppSlotRegionSpec("/", "primary"))),
            AppSurfacingViewModel.pageCacheKey("/observe", listOf(AppSlotRegionSpec("/observe", "primary"))),
        )
        assertEquals(digest, normalizedAppEtag("W/\"$digest\""))
        assertNull(normalizedAppEtag("\"$digest\", \"$digest\""))
    }

    @Test
    fun `indicator cadence wakes at the first materialization expiry`() {
        val now = Instant.parse("2026-09-02T00:00:00Z")
        val indicator = AppMaterializedIndicator(
            installationId = "installation-1",
            installationGeneration = 1,
            indicatorId = "due",
            title = "Due",
            revision = digest,
            evaluatedAt = now.toString(),
            expiresAt = now.plusSeconds(7).toString(),
            model = AppIndicatorModel(kind = "badge", count = 1),
        )

        assertEquals(now.plusSeconds(7), AppSurfacingViewModel.indicatorRefreshDeadline(now, listOf(indicator)))
        assertEquals(now.plusSeconds(30), AppSurfacingViewModel.indicatorRefreshDeadline(now, emptyList()))
    }

    @Test
    fun `native governed launch has exact empty input`() {
        val body = appSurfacingJson.parseToJsonElement(
            emptyActionRequestJson(
                "android-widget-00000000-0000-0000-0000-000000000000",
                expectedInstallationGeneration = 7,
                expectedPackageRevisionRef = "revision:claims-7",
            ),
        ).jsonObject

        assertEquals(setOf("idempotency_key", "input", "expected_installation_binding"), body.keys)
        assertEquals(JsonObject(emptyMap()), body["input"])
        assertEquals(
            JsonObject(mapOf(
                "generation" to JsonPrimitive(7),
                "package_revision_ref" to JsonPrimitive("revision:claims-7"),
            )),
            body["expected_installation_binding"],
        )
    }

    @Test
    fun `public action receipt binds the exact installation and action`() {
        val receipt = appSurfacingJson.decodeFromString(
            AppActionLaunchReceipt.serializer(),
            """{
              "run_handle":{
                "protocol_version":"1",
                "run_ref":"run:app-action:fixture",
                "installation_id":"installation-1",
                "action_id":"confirm"
              }
            }""",
        ).checked("installation-1", "confirm")

        assertEquals("run:app-action:fixture", receipt.runHandle.runRef)
        assertThrows(IllegalArgumentException::class.java) {
            receipt.checked("installation-2", "confirm")
        }
    }

    @Test
    fun `chunked response reader rejects before buffering beyond its ceiling`() = runTest {
        assertEquals(
            "four",
            ByteReadChannel("four".encodeToByteArray()).readBoundedUtf8(4, "load widgets", 200),
        )
        val failure = runCatching {
            ByteReadChannel("five!".encodeToByteArray()).readBoundedUtf8(4, "load widgets", 200)
        }.exceptionOrNull()

        assertTrue(failure is AppSurfacingApiException)
        assertEquals(200, (failure as AppSurfacingApiException).status)
    }

    /**
     * The host half of gate S4 is unbuilt, so no render item names a frame
     * today. The pin is the decoder, not the frame: the batch is decoded as one
     * response, so an unknown `mini_frame` member would blank every pinned
     * widget on the page — not only the escalating one — the day the host emits
     * its first. Web and iOS already accept the member; this keeps the three
     * clients on one contract while Android still hosts nothing.
     */
    @Test
    fun `declared mini frame is accepted bounded and never hosted`() {
        val target = AppWidgetRenderTarget("installation-1", "summary")
        val batch = appSurfacingJson.decodeFromString(
            AppWidgetRenderBatchResponse.serializer(),
            readyBatchJson(""","mini_frame":{"entry_point":"/canvas","max_height_px":240}"""),
        ).checked(listOf(target))
        val widget = batch.widgets.single()

        assertEquals("/canvas", widget.miniFrame?.entryPoint)
        assertEquals(240, widget.miniFrame?.maxHeightPx)
        // The native model is what this client draws either way, frame or none.
        assertEquals("Ready", widget.model?.rows?.single()?.fields?.get("title")?.appWidgetDisplay())
        assertFalse(CustomSurfaceSupport.supported)

        // Absence stays the ordinary case rather than becoming a decode error.
        assertNull(
            appSurfacingJson.decodeFromString(
                AppWidgetRenderBatchResponse.serializer(),
                readyBatchJson(""),
            ).checked(listOf(target)).widgets.single().miniFrame,
        )
    }

    @Test
    fun `mini frame declaration outside its bounds refuses the item`() {
        val target = listOf(AppWidgetRenderTarget("installation-1", "summary"))
        val refused = listOf(
            """{"entry_point":"/canvas/:id","max_height_px":240}""",
            """{"entry_point":"/canvas/../../etc","max_height_px":240}""",
            """{"entry_point":"/","max_height_px":240}""",
            """{"entry_point":"canvas","max_height_px":240}""",
            """{"entry_point":"/canvas","max_height_px":0}""",
            """{"entry_point":"/canvas","max_height_px":481}""",
        )

        refused.forEach { declaration ->
            assertThrows(IllegalArgumentException::class.java) {
                appSurfacingJson.decodeFromString(
                    AppWidgetRenderBatchResponse.serializer(),
                    readyBatchJson(""","mini_frame":$declaration"""),
                ).checked(target)
            }
        }
        // The member set is exact in both directions, as it is on web and iOS.
        assertThrows(SerializationException::class.java) {
            appSurfacingJson.decodeFromString(
                AppWidgetRenderBatchResponse.serializer(),
                readyBatchJson(""","mini_frame":{"entry_point":"/canvas","max_height_px":240,"origin":"*"}"""),
            )
        }
    }

    @Test
    fun `only a rendered widget may claim a mini frame`() {
        val declaration = AppWidgetMiniFrameDeclaration(entryPoint = "/canvas", maxHeightPx = 240)
        val unavailable = AppWidgetRenderItem(
            installationId = "installation-1",
            widgetId = "summary",
            revision = digest,
            renderedAt = "2026-09-02T00:00:00Z",
            refreshAfter = "2026-09-02T00:00:30Z",
            state = "unavailable",
        )

        unavailable.checked()
        assertThrows(IllegalArgumentException::class.java) {
            unavailable.copy(miniFrame = declaration).checked()
        }
        assertThrows(IllegalArgumentException::class.java) {
            unavailable.copy(
                state = "unsupported",
                fallback = AppWidgetFallback(kind = "hide"),
                miniFrame = declaration,
            ).checked()
        }
    }

    /** One ready widget, with whatever extra members a case needs appended. */
    private fun readyBatchJson(extraItemMembers: String): String = """{
      "schema_version":1,
      "revision":"$digest",
      "etag":"$digest",
      "rendered_at":"2026-09-02T00:00:00Z",
      "refresh_after":"2026-09-02T00:00:30Z",
      "widgets":[{
        "installation_id":"installation-1",
        "widget_id":"summary",
        "installation_generation":2,
        "revision":"$digest",
        "rendered_at":"2026-09-02T00:00:00Z",
        "refresh_after":"2026-09-02T00:00:30Z",
        "state":"ready",
        "model":{
          "model":"list",
          "rows":[{
            "entity":"item",
            "record_id":"record-1",
            "record_revision":1,
            "fields":{"title":"Ready"}
          }],
          "hints":{"display_field":"title"},
          "actions":[]
        }$extraItemMembers
      }]
    }"""

}
