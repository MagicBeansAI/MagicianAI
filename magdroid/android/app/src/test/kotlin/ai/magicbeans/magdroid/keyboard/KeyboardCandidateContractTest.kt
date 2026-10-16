package ai.magicbeans.magdroid.keyboard

import android.text.InputType
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyboardCandidateContractTest {
    @Test
    fun `system spelling corrections precede learned completions and duplicates disappear`() {
        assertEquals(
            listOf("hello", "help", "helmet"),
            keyboardCandidates(
                prefix = "helo",
                spelling = listOf("hello", "help", "HELLO"),
                learned = listOf("helmet", "helping"),
            ),
        )
    }

    @Test
    fun `the exact typed word is not offered as its own correction`() {
        assertEquals(
            listOf("writing", "written"),
            keyboardCandidates(
                prefix = "write",
                spelling = listOf("write", "writing"),
                learned = listOf("Write", "written"),
            ),
        )
    }

    @Test
    fun `candidate extraction remains bounded and empty input stays empty`() {
        assertEquals(
            emptyList<String>(),
            keyboardCandidates("", spelling = listOf("one"), learned = listOf("two")),
        )
        assertEquals(
            listOf("one", "two", "three"),
            keyboardCandidates("o", spelling = listOf("one", "two", "three", "four")),
        )
    }

    /**
     * The correction engine outranks the system spell checker, which outranks
     * learned completions. The engine ranks completions and adjacency-verified
     * fixes over the same dictionary iOS ships; the checker only covers what
     * the bundle cannot.
     */
    @Test
    fun `engine candidates lead, and duplicates collapse across lanes`() {
        assertEquals(
            listOf("hello", "helm", "helper"),
            keyboardCandidates(
                "hel",
                engine = listOf("hello", "helm"),
                spelling = listOf("HELLO", "helper"),
                learned = listOf("helm", "helios"),
            ),
        )
    }

    @Test
    fun `secure and numeric fields cannot expose a candidate strip`() {
        assertTrue(isSecureKeyboardField(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD))
        assertTrue(isSecureKeyboardField(InputType.TYPE_CLASS_NUMBER))
        assertFalse(isSecureKeyboardField(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_NORMAL))
        assertFalse(supportsKeyboardCandidates(InputType.TYPE_CLASS_NUMBER))
        assertFalse(supportsKeyboardCandidates(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD))
    }

    @Test
    fun `editor opt out and structured fields suppress spelling candidates`() {
        assertFalse(supportsKeyboardCandidates(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS))
        assertFalse(supportsKeyboardCandidates(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS))
        assertFalse(supportsKeyboardCandidates(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI))
        assertTrue(supportsKeyboardCandidates(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_NORMAL))
    }

    /**
     * Two different questions, and the strip depends on both.
     *
     * "No spelling candidates here" is not "no Magican here". An address bar, a
     * URL field and a filter all decline corrections, and all are ordinary text
     * somebody may want rewritten — so the strip stays and carries the ✦ key.
     * Only a secure or numeric field loses the row outright.
     */
    @Test
    fun `declining candidates is not the same as forbidding the strip`() {
        val declineCandidates = listOf(
            InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI,
            InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_FILTER,
            InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS,
        )
        declineCandidates.forEach { inputType ->
            assertFalse("no corrections", supportsKeyboardCandidates(inputType))
            assertFalse("but the strip stays", isSecureKeyboardField(inputType))
        }

        // The ones that take the whole row with them.
        assertTrue(isSecureKeyboardField(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD))
        assertTrue(isSecureKeyboardField(InputType.TYPE_CLASS_NUMBER))
    }

    /**
     * A variation means nothing without its class.
     *
     * Android reuses the same bit values across classes: a URI text field and a
     * numeric password field both carry variation `0x10`. Reading the variation
     * alone made every address bar a password field — Magican refused to act in
     * one, and the strip carrying it vanished. These two assertions are the
     * same number meaning two different things.
     */
    @Test
    fun `a text variation is not read as the numeric variation that shares its value`() {
        assertEquals(
            "the collision this guards",
            InputType.TYPE_NUMBER_VARIATION_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_URI,
        )
        assertFalse(isSecureKeyboardField(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI))
        assertTrue(isSecureKeyboardField(InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_VARIATION_PASSWORD))
    }

    @Test
    fun `late spell checker results cannot replace a newer word`() {
        assertTrue(candidateResultIsCurrent(4, 4, "write", "write", true))
        assertFalse(candidateResultIsCurrent(3, 4, "write", "write", true))
        assertFalse(candidateResultIsCurrent(4, 4, "write", "writer", true))
        assertFalse(candidateResultIsCurrent(4, 4, "write", "write", false))
    }
}
