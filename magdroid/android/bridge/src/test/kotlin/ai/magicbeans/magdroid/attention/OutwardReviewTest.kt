package ai.magicbeans.magdroid.attention

import ai.magicbeans.magdroid.chat.HitlResponseValue
import java.time.ZoneOffset
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A captured outward send waiting for review (`escalation_trigger:
 * "outward_review"`): the preview, three choices posted as their ids, and an
 * expiry. Every field is optional on the wire; an older server's plain
 * confirmation reads exactly as before.
 */
class OutwardReviewTest {

    private fun item(schema: String) = attentionJson.decodeFromString(
        AttentionItem.serializer(),
        """{"id":"o1","item_type":"escalation","title":"Review before sending","status":"needs_action",
            "metadata":{"source":"agentic","pause_state_id":"pause-9","input_type":"confirmation",
            "input_schema":$schema}}""",
    )

    private fun request(schema: String) = item(schema).let { it.request(it.metadata) }

    private val review = """{"type":"confirmation","action_type":"gmail.send",
        "action_summary":"gmail send {\"to\":\"a@b.com\"}","reason":"Outward send captured for review",
        "approval_preview":{"title":"ambassador wants to email a@b.com","evidence":[],"warnings":[],"notes":[]},
        "escalation_trigger":"outward_review",
        "options":[{"id":"send","label":"Send"},{"id":"discard","label":"Discard"},
                   {"id":"always_allow_recipient","label":"Always allow ambassador to send to a@b.com"}],
        "outward_review":{"capability":"gmail","action":"send","class":"message",
                          "recipients":["a@b.com"],"agent_id":"ambassador","deadline_at_ms":1800000000000,
                          "future_field":true}}"""

    @Test
    fun `a review decodes its record, preview and three choices`() {
        val request = request(review)
        assertEquals("confirmation", request.inputType)
        assertNotNull(request.outwardReview)
        val outward = request.outwardReview!!
        assertEquals("gmail", outward.capability)
        assertEquals("send", outward.action)
        assertEquals("message", outward.sendClass)
        assertEquals(listOf("a@b.com"), outward.recipients)
        assertEquals("ambassador", outward.agentId)
        assertEquals(1_800_000_000_000L, outward.deadlineAtMs)
        assertEquals("Send", outward.sendLabel)
        assertEquals("Discard", outward.discardLabel)
        assertEquals("Always allow ambassador to send to a@b.com", outward.alwaysAllowLabel)
        assertEquals("To a@b.com", outward.recipientsLine)
        assertEquals("ambassador · gmail send", outward.viaLine)

        // The preview leads, with the raw call behind it.
        assertEquals("ambassador wants to email a@b.com", request.approvalPreview?.title)
        assertEquals("ambassador wants to email a@b.com", request.headline)
        assertEquals("gmail send {\"to\":\"a@b.com\"}\n\nOutward send captured for review", request.approvalRawCall)
        assertNull(request.alignment)
    }

    @Test
    fun `each choice posts its id as a choice value`() {
        val outward = request(review).outwardReview!!
        assertEquals(HitlResponseValue.Choice("send"), outward.sendAnswer())
        assertEquals(HitlResponseValue.Choice("discard"), outward.discardAnswer())
        assertEquals(HitlResponseValue.Choice("always_allow_recipient"), outward.alwaysAllowAnswer())
        assertEquals(
            """{"type":"choice","selected_id":"always_allow_recipient"}""",
            outward.alwaysAllowAnswer()!!.toJson().toString(),
        )
    }

    @Test
    fun `the answer goes to the paused confirmation`() {
        val request = request(review)
        val call = request.respondCall(request.outwardReview!!.sendAnswer().toJson())
        assertEquals("/api/magician/v2/hitl/pause-9/respond", call.path)
        assertEquals("\"agentic\"", call.body["source"].toString())
        assertEquals("\"confirmation\"", call.body["input_type"].toString())
        assertEquals("""{"type":"choice","selected_id":"send"}""", call.body["value"].toString())
    }

    @Test
    fun `without options it falls back to the plain confirmation and offers no grant`() {
        val outward = request("""{"type":"confirmation","escalation_trigger":"outward_review"}""").outwardReview!!
        assertEquals("Send", outward.sendLabel)
        assertEquals("Discard", outward.discardLabel)
        assertEquals(HitlResponseValue.Confirmation(true), outward.sendAnswer())
        assertEquals(HitlResponseValue.Confirmation(false), outward.discardAnswer())
        assertNull(outward.alwaysAllowLabel)
        assertNull(outward.alwaysAllowAnswer())
        assertEquals(emptyList<String>(), outward.recipients)
        assertNull(outward.deadlineAtMs)
        assertNull(outward.recipientsLine)
        assertNull(outward.viaLine)
    }

    @Test
    fun `a malformed record reads as an empty one, not a failed row`() {
        val outward = request(
            """{"type":"confirmation","escalation_trigger":"outward_review",
                "options":[{"id":"send","label":" "},{"id":"always_allow_recipient","label":""}],
                "outward_review":{"capability":7,"recipients":"a@b.com","deadline_at_ms":"soon"}}""",
        ).outwardReview!!
        assertNull(outward.capability)
        assertEquals(emptyList<String>(), outward.recipients)
        assertNull(outward.deadlineAtMs)
        // Blank server labels keep the default wording.
        assertEquals("Send", outward.sendLabel)
        assertEquals("Always allow this agent to send to this recipient", outward.alwaysAllowLabel)
        // Listed ids post as choices; an unlisted Discard is the plain "no".
        assertEquals(HitlResponseValue.Choice("send"), outward.sendAnswer())
        assertEquals(HitlResponseValue.Confirmation(false), outward.discardAnswer())

        val zero = request(
            """{"type":"confirmation","escalation_trigger":"outward_review","outward_review":{"deadline_at_ms":0}}""",
        ).outwardReview!!
        assertNull(zero.deadlineAtMs)
    }

    @Test
    fun `many recipients name three and count the rest`() {
        val outward = parseOutwardReview(
            "confirmation", "outward_review", emptyList(),
            attentionJson.parseToJsonElement("""{"recipients":["a@x","b@x","c@x","d@x","e@x"]}"""),
        )!!
        assertEquals("To a@x, b@x, c@x and 2 more", outward.recipientsLine)
    }

    @Test
    fun `ordinary confirmations and other triggers are not reviews`() {
        val plain = request("""{"type":"confirmation","approval_preview":{"title":"Mira wants to change facts.md"}}""")
        assertNull(plain.outwardReview)
        assertEquals("Mira wants to change facts.md", plain.approvalPreview?.title)
        assertEquals("Approve", plain.confirmLabel)

        assertNull(request("""{"type":"confirmation","escalation_trigger":"criticality"}""").outwardReview)
        // Only a confirmation is a review.
        assertNull(parseOutwardReview("choice", "outward_review", emptyList(), null))
    }

    @Test
    fun `a preference hold stays a hold`() {
        val hold = request(
            """{"type":"confirmation","escalation_trigger":"preference_alignment",
                "alignment":{"kind":"preference_alignment","ask_id":"ask-1",
                  "choices":[{"id":"allow_once","label":"Allow this once"}]},
                "outward_review":{"recipients":["a@b.com"]}}""",
        )
        assertNotNull(hold.alignment)
        assertNull(hold.outwardReview)
        assertNull(hold.approvalPreview)
    }

    @Test
    fun `the expiry line is relative within a day and a date beyond it`() {
        val now = 1_800_000_000_000L
        val utc = ZoneOffset.UTC
        assertNull(outwardReviewExpiresLine(null, now, utc, Locale.US))
        assertNull(outwardReviewExpiresLine(0, now, utc, Locale.US))
        assertEquals("Expired; it will not be sent.", outwardReviewExpiresLine(now - 1, now, utc, Locale.US))
        assertEquals("Expires in 1 min", outwardReviewExpiresLine(now + 10_000, now, utc, Locale.US))
        assertEquals("Expires in 45 min", outwardReviewExpiresLine(now + 45 * 60_000, now, utc, Locale.US))
        assertEquals("Expires in 5 h", outwardReviewExpiresLine(now + 5 * 3_600_000, now, utc, Locale.US))
        // 1_800_000_000_000 is Fri 15 Jan 2027, 08:00 UTC; seven days later is a Friday.
        assertEquals(
            "Expires Fri 22 Jan, 8:00 AM",
            outwardReviewExpiresLine(now + 7 * 86_400_000L, now, utc, Locale.US),
        )
    }
}
