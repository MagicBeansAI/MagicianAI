package ai.magicbeans.magdroid.apps

import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class AppSlotSettingsModelsTest {
    private val digest = "blake3:" + "0".repeat(64)
    private val homeSlot = AppSlotId.forPageRegion("/", "primary")
    private val observeSlot = AppSlotId.forPageRegion("/observe", "reviews")

    @Test
    fun `entity route matches the shared canonical app-surface page`() {
        assertEquals("/apps/installation-1", appSurfaceSlotPage("installation-1"))
        assertEquals("/apps/installation-1/review", appSurfaceSlotPage("installation-1", "review"))
        assertNotNull(
            appSurfaceSlotPage("installation-1")
                ?.let { AppSlotId.forPageRegion(it, AppSlotContextualRegion) },
        )
        assertNull(appSurfaceSlotPage("bad id"))
        assertNull(appSurfaceSlotPage("installation-1", ".."))
        assertNull(appSurfaceSlotPage("installation-1", "a/b?c"))
    }

    @Test
    fun `only a round-tripping slot identity is canonical`() {
        assertEquals(observeSlot, canonicalAppSlotId(observeSlot.value))
        assertNull(canonicalAppSlotId("page:2F:primary"))
        assertNull(canonicalAppSlotId("page:2f6:primary"))
        assertNull(canonicalAppSlotId("page::primary"))
        assertNull(canonicalAppSlotId("page:2f:primary:extra"))
    }

    @Test
    fun `settings page decodes its head picker and assignments`() {
        val page = settings(picker = "[${candidateJson()}]")

        assertEquals(AppSlotWriteHead(7, 4), page.head)
        assertEquals(digest, page.inventoryRevision)
        val candidate = page.picker.single()
        assertEquals("Review queue", candidate.title)
        assertEquals(setOf(homeSlot.value), candidate.suggestedSlotIds)
        assertFalse(candidate.systemClass)
    }

    @Test
    fun `a settings page that cannot be mutated from is refused`() {
        // The host's initial fence is zero and it rejects a write presenting it.
        assertThrows(IllegalArgumentException::class.java) { settings(fence = 0) }
        // Truncation and its cursor must agree in both directions.
        assertThrows(IllegalArgumentException::class.java) { settings(pickerTruncated = true) }
        assertThrows(IllegalArgumentException::class.java) {
            settings(pickerCursor = "cursor-1")
        }
        assertThrows(IllegalArgumentException::class.java) { settings(inventory = "not-a-digest") }
    }

    @Test
    fun `picker rows cannot forge provenance identity or duplicates`() {
        // An untrusted package may not claim a system-default slot.
        assertThrows(IllegalArgumentException::class.java) {
            settings(picker = "[${candidateJson(systemDefault = true, systemClass = false)}]")
        }
        // A suggestion whose declared page disagrees with its encoded slot id.
        assertThrows(IllegalArgumentException::class.java) {
            settings(picker = "[${candidateJson(suggestedPage = "/observe")}]")
        }
        assertThrows(IllegalArgumentException::class.java) {
            settings(picker = "[${candidateJson()},${candidateJson()}]")
        }
    }

    @Test
    fun `each command sends exactly the members its variant declares`() {
        val candidate = settings(picker = "[${candidateJson()}]").picker.single()

        val assign = command(AppSlotAssignmentCommand.Assign(homeSlot, candidate))
        assertEquals(
            setOf("command", "slot_id", "installation_id", "widget_id", "expected_candidate"),
            assign.keys,
        )
        // The host denies unknown fields per variant, so a null carried along
        // by a flattened command would be a rejected request, not a tolerated
        // one. This is the pin on that encoder configuration.
        assertEquals(setOf("command", "slot_id"), command(AppSlotAssignmentCommand.OptOut(homeSlot)).keys)
        assertEquals(
            setOf("command", "slot_id"),
            command(AppSlotAssignmentCommand.RestoreWorkspaceDefault(homeSlot)).keys,
        )
        val envelope = appSurfacingJson
            .parseToJsonElement(slotMutationRequestJson(request(AppSlotAssignmentCommand.OptOut(homeSlot))))
            .jsonObject
        assertEquals(setOf("expected_revision", "write_fence", "mutation_id", "command"), envelope.keys)
    }

    @Test
    fun `a mutation id is minted once and is a valid reference`() {
        val minted = newAppSlotMutationId()

        assertTrue(minted.startsWith("slot-mutation:"))
        assertTrue(minted.isReference())
        assertThrows(IllegalArgumentException::class.java) {
            slotMutationRequestJson(request(AppSlotAssignmentCommand.OptOut(homeSlot), mutationId = "other:1"))
        }
        // A fence that never came from a settings read cannot be presented.
        assertThrows(IllegalArgumentException::class.java) {
            slotMutationRequestJson(request(AppSlotAssignmentCommand.OptOut(homeSlot), fence = 0))
        }
    }

    @Test
    fun `an assign receipt must describe this write and this exact binding`() {
        val candidate = settings(picker = "[${candidateJson()}]").picker.single()
        val write = request(AppSlotAssignmentCommand.Assign(homeSlot, candidate))

        val receipt = receipt(assignedAssignmentJson()).checked(write)

        assertEquals(AppSlotWriteHead(8, 4), receipt.head)
        assertEquals("user", receipt.assignment.source)
        assertEquals(AppWidgetRenderTarget("installation-1", "review_queue"), receipt.assignment.target)
        // One revision under the presented fence, or it is not this write.
        assertThrows(IllegalArgumentException::class.java) {
            receipt(assignedAssignmentJson(), revision = 9).checked(write)
        }
        assertThrows(IllegalArgumentException::class.java) {
            receipt(assignedAssignmentJson(), fence = 5).checked(write)
        }
        // A receipt for a different binding never confirms this command.
        assertThrows(IllegalArgumentException::class.java) {
            receipt(assignedAssignmentJson(generation = 4)).checked(write)
        }
        assertThrows(IllegalArgumentException::class.java) {
            receipt(optedOutAssignmentJson()).checked(write)
        }
    }

    @Test
    fun `an opt-out receipt must show the slot actually opted out`() {
        val write = request(AppSlotAssignmentCommand.OptOut(homeSlot))

        val receipt = receipt(optedOutAssignmentJson()).checked(write)

        assertTrue(receipt.assignment.optedOut)
        assertNull(receipt.assignment.source)
        assertThrows(IllegalArgumentException::class.java) {
            receipt(assignedAssignmentJson()).checked(write)
        }
        assertThrows(IllegalArgumentException::class.java) {
            receipt(emptyAssignmentJson()).checked(write)
        }
    }

    @Test
    fun `a restore receipt must leave the slot uncustomized`() {
        val write = request(AppSlotAssignmentCommand.RestoreWorkspaceDefault(homeSlot))

        val receipt = receipt(emptyAssignmentJson()).checked(write)

        assertNull(receipt.assignment.source)
        assertThrows(IllegalArgumentException::class.java) {
            receipt(optedOutAssignmentJson()).checked(write)
        }
        assertThrows(IllegalArgumentException::class.java) {
            receipt(assignedAssignmentJson()).checked(write)
        }
    }

    @Test
    fun `settings must describe the same authority the visible card was drawn from`() {
        val untouched = resolved(emptyAssignmentJson())
        val assigned = resolved(assignedAssignmentJson())

        // The host omits a slot nobody has touched: absent is the empty
        // assignment, not a mismatch.
        assertTrue(settings().agreesWith(homeSlot, untouched))
        assertFalse(settings().agreesWith(homeSlot, assigned))
        assertTrue(
            settings(assignments = "[${assignedAssignmentJson()}]").agreesWith(homeSlot, assigned),
        )
        // A field this client never renders still refuses the write.
        assertFalse(
            settings(assignments = "[${assignedAssignmentJson(generation = 4)}]")
                .agreesWith(homeSlot, assigned),
        )
        assertFalse(
            settings(assignments = "[${optedOutAssignmentJson()}]").agreesWith(homeSlot, untouched),
        )
    }

    @Test
    fun `occupancy decides which slot control the owner is offered`() {
        // Occupied means some authority owns the slot, so it can be removed.
        assertTrue(resolved(assignedAssignmentJson()).occupied)
        // An opt-out owns nothing: it offers the picker and the default again.
        assertFalse(resolved(optedOutAssignmentJson()).occupied)
        assertTrue(resolved(optedOutAssignmentJson()).optedOut)
        assertFalse(resolved(emptyAssignmentJson()).occupied)
        assertEquals("user", resolved(assignedAssignmentJson()).source)
        assertNull(resolved(emptyAssignmentJson()).source)
    }

    @Test
    fun `picker pagination appends only a trustworthy continuation`() {
        val first = settings(
            picker = "[${candidateJson()}]",
            pickerTruncated = true,
            pickerCursor = "cursor-1",
        )
        val second = settings(
            fence = 5,
            picker = "[${candidateJson(installationId = "installation-2", title = "Backlog")}]",
        )

        val merged = first.mergedPicker(second, "cursor-1", emptySet())

        assertEquals(2, merged?.picker?.size)
        assertEquals(AppSlotWriteHead(7, 5), merged?.head)
        assertFalse(merged?.pickerTruncated ?: true)
        // A layout revision change, a fence that did not advance, or a
        // different inventory all mean this is not the same read.
        assertNull(first.mergedPicker(second.copy(head = AppSlotWriteHead(8, 5)), "cursor-1", emptySet()))
        assertNull(first.mergedPicker(second.copy(head = AppSlotWriteHead(7, 4)), "cursor-1", emptySet()))
        assertNull(
            first.mergedPicker(
                second.copy(inventoryRevision = "blake3:" + "1".repeat(64)),
                "cursor-1",
                emptySet(),
            ),
        )
        // A repeated row or a cursor that loops can never terminate.
        assertNull(first.mergedPicker(second.copy(picker = first.picker), "cursor-1", emptySet()))
        val looping = settings(
            fence = 5,
            picker = "[${candidateJson(installationId = "installation-2", title = "Backlog")}]",
            pickerTruncated = true,
            pickerCursor = "cursor-1",
        )
        assertNull(first.mergedPicker(looping, "cursor-1", emptySet()))
        assertNull(first.mergedPicker(looping, "cursor-0", setOf("cursor-1")))
    }

    @Test
    fun `rows suggested for the slot are offered first`() {
        val page = settings(
            picker = "[" +
                candidateJson(installationId = "installation-2", title = "Alpha", suggested = false) +
                "," + candidateJson(title = "Zulu") + "]",
        )

        val ordered = page.picker.orderedForSlot(homeSlot)

        assertEquals(listOf("Zulu", "Alpha"), ordered.map(AppSlotPickerCandidate::title))
        assertEquals(
            listOf("Alpha", "Zulu"),
            page.picker.orderedForSlot(observeSlot).map(AppSlotPickerCandidate::title),
        )
    }

    private fun settings(
        revision: Long = 7,
        fence: Long = 4,
        inventory: String = digest,
        assignments: String = "[]",
        assignmentsTruncated: Boolean = false,
        assignmentCursor: String? = null,
        picker: String = "[]",
        pickerTruncated: Boolean = false,
        pickerCursor: String? = null,
    ): AppSlotSettingsPage = appSurfacingJson.decodeFromString(
        AppSlotSettingsPageWire.serializer(),
        buildString {
            append("""{"head":{"revision":$revision,"fence":$fence},""")
            append(""""inventory_revision":"$inventory",""")
            append(""""assignments":$assignments,""")
            assignmentCursor?.let { append(""""next_assignment_cursor":"$it",""") }
            append(""""assignments_truncated":$assignmentsTruncated,""")
            append(""""picker":$picker,""")
            pickerCursor?.let { append(""""next_picker_cursor":"$it",""") }
            append(""""picker_truncated":$pickerTruncated}""")
        },
    ).checked()

    private fun bindingJson(
        installationId: String = "installation-1",
        widgetId: String = "review_queue",
        generation: Long = 3,
    ) = """{
      "package":{
        "installation_id":"$installationId",
        "package_id":"package:claims",
        "package_revision_ref":"revision:claims-1",
        "package_content_digest":"$digest",
        "installation_generation":$generation
      },
      "widget_id":"$widgetId"
    }"""

    private fun candidateJson(
        installationId: String = "installation-1",
        title: String = "Review queue",
        suggested: Boolean = true,
        suggestedPage: String = "/",
        systemDefault: Boolean = false,
        systemClass: Boolean = false,
    ): String {
        val suggestions = if (suggested) {
            """[{"page":"$suggestedPage","region":"primary","slot_id":"${homeSlot.value}",""" +
                """"system_default":$systemDefault}]"""
        } else {
            "[]"
        }
        return """{
          "widget":${bindingJson(installationId = installationId)},
          "title":"$title",
          "suggested_slots":$suggestions,
          "system_class":$systemClass
        }"""
    }

    private fun emptyAssignmentJson() = """{
      "slot_id":"${homeSlot.value}",
      "pinned_system_default":false,
      "opted_out":false
    }"""

    private fun optedOutAssignmentJson() = """{
      "slot_id":"${homeSlot.value}",
      "pinned_system_default":false,
      "opted_out":true
    }"""

    private fun assignedAssignmentJson(generation: Long = 3) = """{
      "slot_id":"${homeSlot.value}",
      "source":"user",
      "pinned_system_default":false,
      "opted_out":false,
      "widget":{
        "pinned":${bindingJson(generation = generation)},
        "current":${bindingJson(generation = generation)},
        "restored_across_generation":false,
        "assignment_compatibility":"exact_digest_only"
      }
    }"""

    private fun resolved(json: String): AppResolvedSlotAssignment = appSurfacingJson
        .decodeFromString(AppResolvedSlotAssignmentWire.serializer(), json)
        .checked(homeSlot)

    private fun receipt(
        assignment: String,
        revision: Long = 8,
        fence: Long = 4,
    ): AppSlotAssignmentMutationReceiptWire = appSurfacingJson.decodeFromString(
        AppSlotAssignmentMutationReceiptWire.serializer(),
        """{
          "mutation_id":"slot-mutation:fixture",
          "head":{"revision":$revision,"fence":$fence},
          "assignment":$assignment
        }""",
    )

    private fun request(
        command: AppSlotAssignmentCommand,
        mutationId: String = "slot-mutation:fixture",
        fence: Long = 4,
    ) = AppSlotAssignmentWriteRequest(
        expectedRevision = 7,
        writeFence = fence,
        mutationId = mutationId,
        command = command,
    )

    private fun command(command: AppSlotAssignmentCommand) = appSurfacingJson
        .parseToJsonElement(slotMutationRequestJson(request(command)))
        .jsonObject["command"]!!
        .jsonObject
}
