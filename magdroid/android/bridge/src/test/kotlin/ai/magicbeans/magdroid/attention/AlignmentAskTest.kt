package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.HitlResponseValue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A preference alignment hold on Android (P6.3): the public projection. On its
 * own it offers the act's typed choices and a reply, never a lasting change;
 * the private detail ([AlignmentDetailTest]) adds the reason and the lasting
 * choices with their wording readback.
 */
class AlignmentAskTest {

    private fun json(text: String): JsonElement = Json.parseToJsonElement(text)

    private val projection = json(
        """
        {"schema_version":"alignment_ask.v1","kind":"preference_alignment","ask_id":"ask-1",
         "purpose":"allow_once",
         "choices":[{"id":"allow_once","label":"Allow this once"},
                    {"id":"change_preference","label":"Change my preference"},
                    {"id":"remember_preference","label":"Remember this as my preference"},
                    {"id":"cancel","label":"Cancel"}],
         "free_text":true,"deadline_at_ms":1000000,"stage":"decide"}
        """,
    )

    private fun item() =
        AttentionItem(id = "i1", itemType = "escalation", title = "T", status = "needs_action")

    @Test
    fun `a hold offers only the act's own choices on this surface`() {
        val request = item().request(
            AttentionMetadata(
                source = "agentic",
                inputSchema = AttentionInputSchema(
                    type = "confirmation",
                    escalationTrigger = "preference_alignment",
                    alignmentRaw = projection,
                ),
            ),
        )
        val alignment = request.alignment!!
        assertEquals("ask-1", alignment.askId)
        assertEquals(listOf("allow_once", "cancel"), alignment.actChoices.map { it.id })
        assertTrue(alignment.freeText)
        assertEquals(1_000_000L, alignment.deadlineAtMs)
    }

    @Test
    fun `a combined ask names the gates it also settles, without their reasons`() {
        val combined = json(
            """
            {"kind":"preference_alignment","ask_id":"ask-2",
             "choices":[{"id":"allow_once","label":"Allow this once and approve it"},
                        {"id":"cancel","label":"Cancel"}],
             "also_covers":["approval_rule",7]}
            """,
        )
        assertEquals(listOf("approval_rule"), parseAlignmentProjection(combined)!!.alsoCovers)
        assertEquals(emptyList<String>(), parseAlignmentProjection(projection)!!.alsoCovers)
    }

    @Test
    fun `a malformed block or another input type is not a hold`() {
        assertNull(parseAlignmentProjection(json("""{"kind":"preference_alignment","choices":"x"}""")))
        assertNull(parseAlignmentProjection(json("""{"kind":"other","ask_id":"a","choices":[]}""")))
        val text = item().request(
            AttentionMetadata(inputSchema = AttentionInputSchema(type = "text", alignmentRaw = projection)),
        )
        assertNull(text.alignment)
    }

    @Test
    fun `a hold announced only inside hitl_request is still read`() {
        val request = item().request(
            AttentionMetadata(
                hitlRequest = HitlRequestPayload(
                    inputType = "confirmation",
                    inputSchema = AttentionInputSchema(type = "confirmation", alignmentRaw = projection),
                ),
            ),
        )
        assertEquals("ask-1", request.alignment?.askId)
    }

    @Test
    fun `answers are typed choices or the owner's words`() {
        assertEquals(HitlResponseValue.Choice(selectedId = "allow_once"), AlignmentAnswers.choice("allow_once"))
        assertEquals(HitlResponseValue.Text("order the paneer one"), AlignmentAnswers.reply("  order the paneer one "))
        assertNull(AlignmentAnswers.reply("   "))
    }
}
