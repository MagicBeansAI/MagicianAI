package ai.magicbeans.magdroid.attention

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The owner-private read of a hold on Android (2026-10-11): the reason, the
 * act's choices kept apart from the lasting ones, and the wording readback.
 */
class AlignmentDetailTest {

    private val decide = """
        {"private":true,"prompt":"Review","execution_id":"exec-1",
         "alignment":{"stage":"decide","explanation":"You asked to be checked before ordering meat.",
          "act_summary":"Order chicken biryani from Spice Route",
          "intentions":[{"id":"i-meat","statement":"Check with me before ordering meat for me.","assertion_status":"direct_owner_statement"},
                        {"id":"i-curry","statement":"Usually orders curry on Fridays","assertion_status":"inferred"},
                        {"statement":"no id, skipped"}],
          "choices":[{"id":"allow_once","label":"Allow this once","consent":true},
                     {"id":"use_alternative","label":"Do something else"},
                     {"id":"change_preference","label":"Change my preference","lasting":true,"needs_wording":true},
                     {"id":"remember_preference","label":"Remember this","lasting":true},
                     {"id":"keep_as_is","label":"Keep it"},
                     {"id":"cancel","label":"Cancel"}],
          "remember":{"id":"i-curry","statement":"Usually orders curry on Fridays"},
          "deadline_at_ms":1000000,
          "also_covers":[{"gate":"approval_rule","reason":""}]}}
    """

    @Test
    fun `a private read shows why and keeps the lasting choices apart from the act`() {
        val detail = parseAlignmentDetail(decide)
        assertNotNull(detail)
        detail!!
        assertTrue(detail.isPrivate)
        val alignment = detail.alignment
        assertEquals("You asked to be checked before ordering meat.", alignment.explanation)
        assertEquals(listOf("You said", "Magician noticed"), alignment.intentions.map { it.sourceLabel })
        assertEquals(listOf("allow_once", "use_alternative", "cancel"), alignment.actChoices.map { it.id })
        assertEquals(listOf("change_preference", "remember_preference"), alignment.lastingChoices.map { it.id })
        assertTrue(alignment.lastingChoices.first().needsWording)
        assertEquals("One of your approval rules matched it.", alignment.alsoCovers.single().line)
        assertFalse(alignment.isWordingStage)
    }

    @Test
    fun `the wording stage carries the exact readback and its token`() {
        val body = """
            {"private":true,"alignment":{"stage":"confirm_wording",
             "choices":[{"id":"confirm_wording","label":"Save","lasting":true},{"id":"keep_as_is","label":"Keep"},{"id":"cancel","label":"Cancel"}],
             "readback":{"kind":"preference","wording":"Check with me before ordering red meat.","superseded":"Check with me before ordering meat for me.","token":"tok-123"}}}
        """
        val alignment = parseAlignmentDetail(body)!!.alignment
        assertTrue(alignment.isWordingStage)
        assertEquals("tok-123", alignment.readback?.token)
        assertEquals("Check with me before ordering meat for me.", alignment.readback?.superseded)
    }

    @Test
    fun `an unreadable body is no detail rather than a crash`() {
        assertNull(parseAlignmentDetail("not json"))
        assertNull(parseAlignmentDetail("""{"private":true}"""))
    }

    @Test
    fun `the deadline line counts minutes then hours and closes`() {
        val now = 1_000_000L
        assertEquals("Answer within 5 min; after that it won't run.", alignmentDeadlineLine(now + 5 * 60_000, now))
        assertEquals("Answer within 2 h; after that it won't run.", alignmentDeadlineLine(now + 2 * 3_600_000, now))
        assertEquals("The answer window has closed; a late answer runs nothing.", alignmentDeadlineLine(now - 1, now))
        assertNull(alignmentDeadlineLine(null, now))
    }
}
