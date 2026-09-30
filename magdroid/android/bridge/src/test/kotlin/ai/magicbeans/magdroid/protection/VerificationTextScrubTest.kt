package ai.magicbeans.magdroid.protection

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VerificationTextScrubTest {

    /**
     * The defect this exists for, in the shape a handset produced it: with the
     * notification shade open, `android_get_ui_tree` returned the very message
     * `android_get_notifications` had just withheld. The screen read must hold
     * the same text the notification read holds, or the notification read is
     * a lock on one of two doors.
     */
    @Test
    fun a_ui_tree_node_carrying_a_code_is_held_and_the_rest_of_the_screen_is_not() {
        val tree = """
            {"nodes":[
              {"text":"SecureBank","class":"TextView"},
              {"text":"Your verification code is 728391","class":"TextView"},
              {"text":"Rain expected this afternoon","class":"TextView"},
              {"text":"USB debugging connected","class":"TextView"}
            ]}
        """.trimIndent()
        val result = VerificationTextScrub.scrub(tree)
        assertTrue("a code on screen must be withheld", result.withheld)
        assertFalse("the digits must not survive", result.text.contains("728391"))
        assertTrue(result.text.contains(VerificationTextScrub.WITHHELD))
        assertTrue("an ordinary notification still reads", result.text.contains("Rain expected this afternoon"))
        assertTrue("unrelated screen text is untouched", result.text.contains("USB debugging connected"))
        assertTrue("a label beside the code is not itself a code", result.text.contains("SecureBank"))
    }

    /**
     * The Apps snapshot carries its whole element table in one string. Holding
     * the table because one row carried a code would take the screen away in
     * order to protect one line of it.
     */
    @Test
    fun a_table_in_one_string_is_held_line_by_line() {
        val table = "1 button Settings\n2 text Your login code is 042917\n3 button Cancel"
        val result = VerificationTextScrub.scrub("""{"elements":${quote(table)}}""")
        assertTrue(result.withheld)
        assertFalse(result.text.contains("042917"))
        assertTrue("the rows that carried nothing survive", result.text.contains("button Settings"))
        assertTrue(result.text.contains("button Cancel"))
    }

    @Test
    fun a_screen_with_no_code_is_returned_untouched() {
        val tree = """{"nodes":[{"text":"Balance 1,204.55"},{"text":"Transfer"},{"text":"Order 12345 shipped"}]}"""
        val result = VerificationTextScrub.scrub(tree)
        assertFalse("nothing here is a one-time code", result.withheld)
        assertEquals(tree, result.text)
    }

    @Test
    fun text_that_is_not_json_is_still_judged() {
        val plain = "Your verification code is 519473"
        val result = VerificationTextScrub.scrub(plain)
        assertTrue(result.withheld)
        assertEquals(VerificationTextScrub.WITHHELD, result.text)

        val harmless = VerificationTextScrub.scrub("Settings")
        assertFalse(harmless.withheld)
        assertEquals("Settings", harmless.text)
    }

    /**
     * The same judgement chooses which screen regions to black out of a
     * screenshot, so a capture and a screen read cannot disagree about what a
     * code is. A node with no text at all must not select a region.
     */
    @Test
    fun the_same_judgement_answers_for_pixels() {
        assertTrue(VerificationTextScrub.carriesACode("Your verification code is 728391"))
        assertTrue(VerificationTextScrub.carriesACode("Your login code is 042917"))
        assertFalse(VerificationTextScrub.carriesACode("Order 12345 shipped"))
        assertFalse(VerificationTextScrub.carriesACode("Balance 1,204.55"))
        assertFalse(VerificationTextScrub.carriesACode("Settings"))
        assertFalse("a node with nothing to read selects nothing", VerificationTextScrub.carriesACode(null))
        assertFalse(VerificationTextScrub.carriesACode(""))
    }

    private fun quote(value: String): String =
        "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n") + "\""
}
