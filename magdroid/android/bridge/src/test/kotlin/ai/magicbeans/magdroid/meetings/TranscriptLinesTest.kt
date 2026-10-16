package ai.magicbeans.magdroid.meetings

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Transcript lines out of a meeting's chat session, against `MeetingsAPI.swift`.
 *
 * The transcript lands in the session as ordinary messages, so the filter is
 * load-bearing: without `source_surface == "meeting-transcript"` an assistant
 * reply in the same session reads back as something somebody said in the room.
 */
class TranscriptLinesTest {

    private fun session(vararg messages: String) =
        """{"messages":[${messages.joinToString(",")}]}"""

    // The live wire shape (verified on-device in pass 6): `content` is the
    // chat message object, not a bare string. This builder used to emit the
    // string form, so the whole suite pinned a frame the server never sends —
    // and the parser that matched it dropped every real line.
    private fun line(id: String, content: String, surface: String = "meeting-transcript") =
        """{"id":"$id","direction":"system","created_at":1,
            "content":{"type":"text","text":"$content"},
            "source_surface":"$surface"}"""

    @Test
    fun `a speaker prefix is split from the line`() {
        val lines = parseTranscriptLines(session(line("m1", "Priya: the tier caps at forty")))
        assertEquals(1, lines.size)
        assertEquals("Priya", lines.single().speaker)
        assertEquals("the tier caps at forty", lines.single().text)
        assertEquals("m1", lines.single().id)
    }

    /**
     * The guard. A colon deep in a sentence is punctuation, not a name — split
     * on it and half the sentence is attributed to a speaker who does not
     * exist.
     */
    @Test
    fun `a colon deep in the sentence is not a speaker`() {
        val long = "There is one thing worth remembering about all of this: the tier caps"
        val lines = parseTranscriptLines(session(line("m1", long)))
        assertNull(lines.single().speaker)
        assertEquals(long, lines.single().text)
    }

    /** No prefix at all is a line with no speaker, not one with an empty name. */
    @Test
    fun `an unattributed line carries no speaker`() {
        val lines = parseTranscriptLines(session(line("m1", "the tier caps at forty")))
        assertNull(lines.single().speaker)
    }

    /**
     * Only what the capture wrote. An assistant reply in the same session is
     * not something that was said in the room.
     */
    @Test
    fun `messages from other surfaces are not transcript`() {
        val lines = parseTranscriptLines(
            session(
                line("m1", "Priya: hello"),
                line("m2", "Here is a summary", surface = "chat"),
                line("m3", "Sam: goodbye"),
            ),
        )
        assertEquals(listOf("m1", "m3"), lines.map { it.id })
    }

    /** An empty body is not a line; a poll should not render a blank row. */
    @Test
    fun `blank bodies are dropped`() {
        val lines = parseTranscriptLines(session(line("m1", ""), line("m2", "   ")))
        assertTrue(lines.isEmpty())
    }

    /**
     * The body stands in as the identity when a message carried none, so a
     * repeated poll de-duplicates by what was said rather than by position.
     */
    @Test
    fun `a message without an id is keyed by its content`() {
        val lines = parseTranscriptLines(
            """{"messages":[{"content":{"type":"text","text":"Priya: hello"},
                "source_surface":"meeting-transcript"}]}""",
        )
        assertEquals("Priya: hello", lines.single().id)
    }

    /** The pre-object string form still parses, in case an old backend sends it. */
    @Test
    fun `a legacy bare-string content still yields a line`() {
        val lines = parseTranscriptLines(
            """{"messages":[{"id":"m1","content":"Sam: hello",
                "source_surface":"meeting-transcript"}]}""",
        )
        assertEquals("Sam", lines.single().speaker)
    }

    @Test
    fun `a session with nothing to show yields nothing rather than throwing`() {
        assertTrue(parseTranscriptLines("""{"messages":[]}""").isEmpty())
        assertTrue(parseTranscriptLines("""{}""").isEmpty())
        assertTrue(parseTranscriptLines("not json").isEmpty())
        assertTrue(parseTranscriptLines("").isEmpty())
    }
}
