package ai.magicbeans.magdroid.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The handoff between the share target and the composer.
 *
 * Take-once is the property that matters. The composer drains this when it
 * appears, and it appears again every time the app is returned to — a handoff
 * that kept its contents would re-attach the same file on each visit, long
 * after the share it came from.
 */
class ChatShareInboxTest {

    private fun file(name: String, body: String) =
        SharedFile(name, "text/plain", body.toByteArray())

    @Test
    fun `what was handed over comes back once`() {
        ChatShareInbox.hand("a link", listOf(file("notes.txt", "hello")))

        val taken = ChatShareInbox.take()
        assertEquals("a link", taken?.text)
        assertEquals(listOf("notes.txt"), taken?.files?.map { it.name })

        assertNull("a second take must find nothing", ChatShareInbox.take())
    }

    @Test
    fun `an empty inbox yields nothing`() {
        ChatShareInbox.take()
        assertNull(ChatShareInbox.take())
    }

    /** A later share replaces an unclaimed one rather than queuing behind it. */
    @Test
    fun `the most recent share is the one waiting`() {
        ChatShareInbox.hand("first", emptyList())
        ChatShareInbox.hand("second", emptyList())
        assertEquals("second", ChatShareInbox.take()?.text)
        assertNull(ChatShareInbox.take())
    }

    @Test
    fun `text alone and files alone both survive the handoff`() {
        ChatShareInbox.hand("just text", emptyList())
        val text = ChatShareInbox.take()
        assertEquals("just text", text?.text)
        assertTrue(text?.files?.isEmpty() == true)

        ChatShareInbox.hand("", listOf(file("a.txt", "x")))
        val files = ChatShareInbox.take()
        assertEquals("", files?.text)
        assertEquals(1, files?.files?.size)
    }

    /**
     * A shared file is its content. The default `ByteArray` comparison is by
     * reference, which would make two reads of the same bytes unequal — and
     * this type is compared inside a data class the composer holds.
     */
    @Test
    fun `a shared file compares by its content`() {
        assertEquals(file("a.txt", "same"), file("a.txt", "same"))
        assertEquals(file("a.txt", "same").hashCode(), file("a.txt", "same").hashCode())
        assertNotEquals(file("a.txt", "same"), file("a.txt", "different"))
        assertNotEquals(file("a.txt", "same"), file("b.txt", "same"))
    }
}
