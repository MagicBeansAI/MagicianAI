package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.access.PairedMachine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Note links (`/notes?path=…&workspace=…`, the same route on a paired
 * origin, and `magican://notes?…`) open the native Notes screen, honouring the
 * workspace the phone is signed in to.
 */
class NoteLinksTest {
    private val mac = PairedMachine("https://mac.example.com", "mac.example.com", active = true, workspace = "personal")
    private val studio = PairedMachine("https://studio.example.com", "studio.example.com", active = false, workspace = "team")

    @Test
    fun `a relative link decodes its path and workspace`() {
        assertEquals(
            NoteLink("Tasks/2026-10-21/Weekly report+notes.md", "personal"),
            NoteLinks.parse("/notes?path=Tasks%2F2026-10-21%2FWeekly%20report%2Bnotes.md&workspace=personal"),
        )
        assertEquals(NoteLink("a+b.md", null), NoteLinks.parse("/notes?path=a+b.md"))
    }

    @Test
    fun `a link without a workspace opens in the current one`() {
        val link = NoteLinks.parse("/notes?path=Inbox.md")!!
        assertNull(link.workspace)
        assertEquals(NoteLinkResolution.Open("Inbox.md"), NoteLinks.resolve(link, listOf(mac)))
    }

    @Test
    fun `a link in the current workspace opens`() {
        val link = NoteLinks.parse("/notes/?path=a.md&workspace=personal#top")!!
        assertEquals(NoteLinkResolution.Open("a.md"), NoteLinks.resolve(link, listOf(mac, studio)))
    }

    @Test
    fun `an absolute link on the paired origin opens`() {
        val link = NoteLinks.parse("https://MAC.example.com:443/notes?path=a.md&workspace=personal")!!
        assertEquals("https://mac.example.com", link.origin)
        assertEquals(NoteLinkResolution.Open("a.md"), NoteLinks.resolve(link, listOf(mac)))
    }

    @Test
    fun `an absolute link on another paired machine switches to it`() {
        val link = NoteLinks.parse("https://studio.example.com/notes?path=a.md&workspace=team")!!
        assertEquals(
            NoteLinkResolution.SwitchMachine("https://studio.example.com", "a.md"),
            NoteLinks.resolve(link, listOf(mac, studio)),
        )
    }

    @Test
    fun `an absolute link on an unpaired origin is left to the browser`() {
        val link = NoteLinks.parse("https://elsewhere.example.com/notes?path=a.md")!!
        assertEquals(NoteLinkResolution.NotOurs, NoteLinks.resolve(link, listOf(mac)))
    }

    @Test
    fun `the scheme link parses like the relative one`() {
        assertEquals(
            NoteLink("Daily/2026-10-21.md", "personal"),
            NoteLinks.parse("magican://notes?path=Daily%2F2026-10-21.md&workspace=personal"),
        )
        assertEquals("x.md", NoteLinks.parse("magican://notes/?path=x.md")?.path)
    }

    @Test
    fun `another workspace is refused without loading`() {
        val link = NoteLinks.parse("/notes?path=a.md&workspace=team")!!
        assertEquals(NoteLinkResolution.OtherWorkspace("team"), NoteLinks.resolve(link, listOf(mac)))
        val absolute = NoteLinks.parse("https://mac.example.com/notes?path=a.md&workspace=team")!!
        assertEquals(NoteLinkResolution.OtherWorkspace("team"), NoteLinks.resolve(absolute, listOf(mac)))
    }

    @Test
    fun `another workspace on exactly one paired machine switches to it`() {
        val link = NoteLinks.parse("magican://notes?path=a.md&workspace=team")!!
        assertEquals(
            NoteLinkResolution.SwitchMachine("https://studio.example.com", "a.md"),
            NoteLinks.resolve(link, listOf(mac, studio)),
        )
        val office = PairedMachine("https://office.example.com", "office.example.com", active = false, workspace = "team")
        assertEquals(NoteLinkResolution.OtherWorkspace("team"), NoteLinks.resolve(link, listOf(mac, studio, office)))
    }

    @Test
    fun `malformed and foreign links are not note links`() {
        listOf(
            null,
            "",
            "/notes",
            "/notes?path=",
            "/notes?path=%20%20",
            "/notes?workspace=personal",
            "/notesx?path=a.md",
            "/notes/a.md",
            "notes?path=a.md",
            "/tasks?selected=task_1",
            "magican://task/abc?path=a.md",
            "ftp://mac.example.com/notes?path=a.md",
            "https://user:secret@mac.example.com/notes?path=a.md",
            "/notes?path=a b.md",
            "/notes?path=%E0%A4%A",
        ).forEach { assertNull(it, NoteLinks.parse(it)) }
    }
}
