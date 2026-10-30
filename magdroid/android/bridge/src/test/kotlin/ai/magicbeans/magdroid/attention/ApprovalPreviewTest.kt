package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.vibedev.DiffLineKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A confirmation's structured approval card (`input_schema.approval_preview`).
 * Display only and optional on the wire: a feed without it, or with a
 * malformed one, reads exactly as before.
 */
class ApprovalPreviewTest {

    private fun item(schema: String, extraMetadata: String = "") = attentionJson.decodeFromString(
        AttentionItem.serializer(),
        """{"id":"a1","item_type":"escalation","title":"Confirmation required","status":"needs_action",
            "metadata":{"source":"agentic","pause_state_id":"pause-1","input_type":"confirmation",
            "question":"Mira wants to change facts.md (+2 lines, −1 line)"$extraMetadata,
            "input_schema":$schema}}""",
    )

    private val previewed = """{"type":"confirmation","action_summary":"propose_program_revision {\"program\":\"facts.md\"}",
        "reason":"Tool requires approval","future_field":{"x":1},
        "approval_preview":{"title":"Mira wants to change `facts.md`","why_label":"Why (Mira's words)",
          "why":"Pricing changed on Monday.\nThe old line is wrong.",
          "diff":{"path":"facts.md","added":2,"removed":1,"truncated":true,"omitted_changed_lines":7,
            "unified_diff":"@@ -3,3 +3,4 @@\n a\n-b\n+c\n+d\n e\n",
            "hunks":[{"old_start":3,"old_lines":3,"new_start":3,"new_lines":4,"lines":[
              {"kind":"context","text":"  a"},{"kind":"remove","text":"b"},{"kind":"add","text":"c"},
              {"kind":"rename","text":"ignored"},{"kind":"add"},{"kind":"add","text":"d"},{"kind":"context","text":"e"}]}]},
          "evidence":["Invoice #12 shows the new price", 7, ""],
          "warnings":[{"code":"shared_document","message":"Other agents read this document."},{"code":"empty"}],
          "notes":["Saving keeps the current version in history."]}}"""

    @Test
    fun `a previewed confirmation decodes its card and keeps the raw call`() {
        val request = item(previewed).request(item(previewed).metadata)
        assertEquals("confirmation", request.inputType)
        val preview = assertNotNullValue(request.approvalPreview)
        assertEquals("Mira wants to change `facts.md`", preview.title)
        assertEquals("Mira wants to change `facts.md`", request.headline)
        assertEquals("Why (Mira's words)", preview.whyHeading)
        assertEquals("Pricing changed on Monday.\nThe old line is wrong.", preview.why)
        assertEquals(listOf(ApprovalPreviewWarning("shared_document", "Other agents read this document.")), preview.warnings)
        assertEquals(listOf("Invoice #12 shows the new price"), preview.evidence)
        assertEquals(listOf("Saving keeps the current version in history."), preview.notes)

        val diff = assertNotNullValue(preview.diff)
        assertEquals("facts.md", diff.path)
        assertEquals("+2 lines, −1 line", diff.changeLabel)
        assertEquals("7 more changed lines not shown", diff.omittedLabel)
        val hunk = diff.hunks.single()
        assertEquals("@@ -3,3 +3,4 @@", hunk.header)
        // Unknown kinds and lines without text are dropped; text stays verbatim.
        assertEquals(
            listOf(
                ApprovalPreviewLine(ApprovalPreviewLine.Kind.Context, "  a"),
                ApprovalPreviewLine(ApprovalPreviewLine.Kind.Remove, "b"),
                ApprovalPreviewLine(ApprovalPreviewLine.Kind.Add, "c"),
                ApprovalPreviewLine(ApprovalPreviewLine.Kind.Add, "d"),
                ApprovalPreviewLine(ApprovalPreviewLine.Kind.Context, "e"),
            ),
            hunk.lines,
        )

        // Numbered for the shared diff view: context both sides, removals old, additions new.
        val lines = diff.toUnifiedDiff().hunks.single().lines
        assertEquals(listOf(DiffLineKind.Context, DiffLineKind.Removed, DiffLineKind.Added, DiffLineKind.Added, DiffLineKind.Context), lines.map { it.kind })
        assertEquals(listOf(3, 4, null, null, 5), lines.map { it.oldNumber })
        assertEquals(listOf(3, null, 4, 5, 6), lines.map { it.newNumber })

        assertEquals("propose_program_revision {\"program\":\"facts.md\"}\n\nTool requires approval", request.approvalRawCall)
        // Answering is unchanged.
        assertEquals("pause-1", request.correlationId)
        assertEquals("Approve", request.confirmLabel)
    }

    @Test
    fun `a confirmation without a preview reads as before`() {
        val plain = item("""{"type":"confirmation","action_summary":"send_email","reason":"Tool requires approval"}""")
        val request = plain.request(plain.metadata)
        assertNull(request.approvalPreview)
        assertNull(request.approvalRawCall)
        assertEquals("Mira wants to change facts.md (+2 lines, −1 line)", request.headline)
        assertEquals(request.prompt, request.headline)
        assertFalse(AskSurfaces.answersInSheet(request, compact = true))
    }

    @Test
    fun `a malformed preview is no preview and never fails the feed`() {
        listOf(
            """{"type":"confirmation","approval_preview":"not an object"}""",
            """{"type":"confirmation","approval_preview":{"why":"no title"}}""",
            """{"type":"confirmation","approval_preview":{"title":"   "}}""",
            """{"type":"confirmation","approval_preview":null,"action_summary":{"structured":true},"reason":3}""",
        ).forEach { schema ->
            val row = item(schema)
            assertNull(schema, row.request(row.metadata).approvalPreview)
        }
        // A diff without a path is dropped, the rest of the card kept.
        val noPath = item("""{"type":"confirmation","approval_preview":{"title":"T","diff":{"added":1,"hunks":"x"}}}""")
        val request = noPath.request(noPath.metadata)
        assertEquals("T", request.approvalPreview?.title)
        assertNull(request.approvalPreview?.diff)
        // No raw call on the schema: the prompt stands in for it.
        assertEquals("Mira wants to change facts.md (+2 lines, −1 line)", request.approvalRawCall)
    }

    @Test
    fun `the preview rides the hitl payload when the item carries none and goes to the sheet when space is short`() {
        val row = item("null", extraMetadata = """, "hitl_request":{"input_schema":$previewed}""")
        val request = row.request(row.metadata)
        assertEquals("Mira wants to change `facts.md`", request.approvalPreview?.title)
        assertTrue(AskSurfaces.answersInSheet(request, compact = true))
        assertFalse(AskSurfaces.answersInSheet(request, compact = false))
        assertEquals("Review the change", AskSurfaces.sheetLabel(request))
    }

    @Test
    fun `a preference hold never shows a preview`() {
        val hold = item(
            """{"type":"confirmation","escalation_trigger":"preference_alignment",
                "alignment":{"kind":"preference_alignment","ask_id":"ask-1","choices":[{"id":"proceed","label":"Proceed"}]},
                "approval_preview":{"title":"Should not show"}}""",
        )
        val request = hold.request(hold.metadata)
        assertNotNull(request.alignment)
        assertNull(request.approvalPreview)
    }

    private fun <T> assertNotNullValue(value: T?): T {
        assertNotNull(value)
        return value!!
    }
}
