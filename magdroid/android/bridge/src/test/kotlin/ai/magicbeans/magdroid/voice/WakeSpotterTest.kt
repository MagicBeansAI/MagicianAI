package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Whether a wake phrase was actually said.
 *
 * The decision is the part that goes subtly wrong — a spotter that fires on
 * ordinary conversation is worse than no spotter, because the only fix anyone
 * reaches for is turning the microphone off.
 */
class WakeSpotterTest {
    private val phrases = listOf("magician")

    /**
     * The unknown token is not optional. Without an alternative for ordinary
     * speech the decoder must spend every utterance on the phrases, which makes
     * every conversation in the room a wake word.
     */
    @Test
    fun `the grammar always admits unknown speech`() {
        assertTrue(WakeSpotter.grammar(phrases).contains(WakeSpotter.UNKNOWN))
        assertTrue(WakeSpotter.grammar(emptyList()).contains(WakeSpotter.UNKNOWN))
    }

    @Test
    fun `the grammar is a sorted json array of phrases`() {
        assertEquals("""["alpha","beta","[unk]"]""", WakeSpotter.grammar(listOf("Beta", "Alpha")))
    }

    /** Duplicates and empties would only widen what can be mistaken for a wake. */
    @Test
    fun `the grammar drops duplicates and blanks`() {
        assertEquals("""["magician","[unk]"]""", WakeSpotter.grammar(listOf("Magician", "magician", "  ")))
    }

    /** A decoder emits lower-case words; punctuation could never match. */
    @Test
    fun `phrases normalise to what a decoder can emit`() {
        assertEquals("hey magician", WakeSpotter.normalise("  Hey, Magician!  "))
        assertEquals("hey magician", WakeSpotter.normalise("HEY   MAGICIAN"))
    }

    @Test
    fun `the phrase is spotted in an utterance`() {
        assertEquals("magician", WakeSpotter.matches("magician", phrases))
        assertEquals("magician", WakeSpotter.matches("ok magician what is next", phrases))
    }

    /**
     * The classic failure: a substring test fires "hey" inside "heyday" and
     * "sam" inside "Samantha". Matching is on word boundaries.
     */
    @Test
    fun `a phrase inside a longer word does not wake`() {
        assertNull(WakeSpotter.matches("magicians assemble", listOf("magician")))
        assertNull(WakeSpotter.matches("heyday", listOf("hey")))
        assertNull(WakeSpotter.matches("samantha called", listOf("sam")))
    }

    /** A multi-word phrase must appear together and in order. */
    @Test
    fun `a multi-word phrase must be contiguous and in order`() {
        val hey = listOf("hey magician")
        assertEquals("hey magician", WakeSpotter.matches("hey magician are you there", hey))
        assertNull(WakeSpotter.matches("magician hey", hey))
        assertNull(WakeSpotter.matches("hey there magician", hey))
    }

    @Test
    fun `ordinary speech does not wake`() {
        assertNull(WakeSpotter.matches("what time is the meeting", phrases))
        assertNull(WakeSpotter.matches("", phrases))
        assertNull(WakeSpotter.matches("   ", phrases))
    }

    /** Finals arrive as `text`, partials as `partial`. Both are worth checking. */
    @Test
    fun `a decoded result reads either shape`() {
        assertEquals("magician", WakeSpotter.decoded("""{"text": "magician"}"""))
        assertEquals("magi", WakeSpotter.decoded("""{"partial": "magi"}"""))
    }

    /** An empty or unreadable result is silence, not a crash. */
    @Test
    fun `an unusable result decodes to nothing`() {
        assertNull(WakeSpotter.decoded(null))
        assertNull(WakeSpotter.decoded(""))
        assertNull(WakeSpotter.decoded("not json"))
        assertNull(WakeSpotter.decoded("""{"text": ""}"""))
    }

    /** Punctuation from a decoder must not prevent a match. */
    @Test
    fun `a decoded phrase matches through punctuation and case`() {
        assertEquals("magician", WakeSpotter.matches("Magician, hello.", phrases))
    }
}
