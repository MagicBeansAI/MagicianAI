package ai.magicbeans.magdroid.voice

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `<speech>` protocol, shared with the web and iOS.
 *
 * The assistant marks what should be heard; everything else is shown but not
 * read. A table is worth displaying and painful to listen to.
 */
class SpeechTagsTest {

    /** Tagged bodies are what gets read — not the surrounding prose. */
    @Test
    fun `only the tagged parts are spoken`() {
        val raw = "Here is a table:\n\n| a | b |\n\n<speech>Two rows, both fine.</speech>"
        assertEquals("Two rows, both fine.", SpeechTags.spokenText(raw))
    }

    /** Several blocks read as one continuous answer. */
    @Test
    fun `multiple blocks join into one utterance`() {
        val raw = "<speech>First.</speech> noise <speech>Second.</speech>"
        assertEquals("First. Second.", SpeechTags.spokenText(raw))
    }

    /** A reply with nothing marked is still speakable, in full. */
    @Test
    fun `an untagged reply is read whole`() {
        assertEquals("Just a sentence.", SpeechTags.spokenText("  Just a sentence.  "))
    }

    /** Attributes carry delivery hints; they must not break the match. */
    @Test
    fun `attributes on the tag are tolerated`() {
        val raw = """<speech emotion="warm" pace="slow">Hello there.</speech>"""
        assertEquals("Hello there.", SpeechTags.spokenText(raw))
        assertTrue(SpeechTags.hasSpeech(raw))
    }

    /** Line breaks inside a block are prose, not structure. */
    @Test
    fun `whitespace inside a block collapses`() {
        assertEquals("One two three.", SpeechTags.spokenText("<speech>One\n  two\tthree.</speech>"))
    }

    /** An empty block is not something to say. */
    @Test
    fun `an empty block does not count as speech`() {
        assertFalse(SpeechTags.hasSpeech("<speech>   </speech>"))
        assertEquals("<speech>   </speech>", SpeechTags.spokenText("<speech>   </speech>").trim())
    }

    /**
     * Stripping keeps the words and drops the markers. This is where Android
     * departs from iOS and the web, which leave them visible: they are
     * protocol, and nobody reading a reply wants to see them.
     */
    @Test
    fun `stripping keeps the words and drops the markers`() {
        val raw = "Before <speech>the spoken part</speech> after"
        val shown = SpeechTags.stripped(raw)
        assertEquals("Before the spoken part after", shown)
        assertFalse(shown.contains("<speech"))
        assertFalse(shown.contains("</speech>"))
    }

    @Test
    fun `stripping an untagged reply changes nothing`() {
        assertEquals("Nothing to strip.", SpeechTags.stripped("Nothing to strip."))
    }

    /** Case is not part of the protocol. */
    @Test
    fun `tags match regardless of case`() {
        assertEquals("Read me.", SpeechTags.spokenText("<SPEECH>Read me.</SPEECH>"))
        assertEquals("Read me.", SpeechTags.stripped("<Speech>Read me.</speech>"))
    }
}
