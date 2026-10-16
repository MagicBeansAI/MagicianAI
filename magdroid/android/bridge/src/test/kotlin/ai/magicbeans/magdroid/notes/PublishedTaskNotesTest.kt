package ai.magicbeans.magdroid.notes

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Published task notes, against `notes_api.rs`.
 *
 * The note itself lives in the notes store and is reached by URL. This client
 * lists them and hands off, so the only contract that matters here is the page
 * envelope and whether a row has somewhere to go.
 */
class PublishedTaskNotesTest {

    private fun page(json: String) =
        notesJson.decodeFromString(PublishedTaskNotePage.serializer(), json)

    @Test
    fun `a page decodes with its paging fields`() {
        val p = page(
            """{"items":[{"projection_id":"p1","task_id":"t1","title":"Weekly digest",
                          "status":"done","agent_id":"presto","mode":"auto",
                          "source_updated_at":"2026-08-10T10:00:00Z",
                          "published_at":"2026-08-10T10:05:00Z",
                          "tags":["digest","weekly"],"note_path":"Tasks/Weekly.md",
                          "open_url":"https://notes.example/Tasks/Weekly"}],
                "offset":0,"limit":10,"total":31,"has_more":true}""",
        )
        val note = p.items.single()
        assertEquals("p1", note.id)
        assertEquals("Weekly digest", note.title)
        assertEquals(listOf("digest", "weekly"), note.tags)
        assertTrue(note.isOpenable)
        assertEquals(31, p.total)
        assertTrue(p.hasMore)
    }

    /**
     * A note with no URL is listed but not tappable.
     *
     * A row that opens nothing is the same broken promise as a button that does
     * nothing — which is the failure this whole sweep started by removing.
     */
    @Test
    fun `a note with no url is not openable`() {
        assertFalse(PublishedTaskNote(title = "No link").isOpenable)
        assertFalse(PublishedTaskNote(title = "Blank", notePath = " ").isOpenable)
        assertTrue(PublishedTaskNote(title = "Fine", notePath = "Inbox/a.md").isOpenable)
    }

    @Test
    fun `an empty page is empty, not an error`() {
        val p = page("""{"items":[],"offset":0,"limit":10,"total":0,"has_more":false}""")
        assertTrue(p.items.isEmpty())
        assertFalse(p.hasMore)
    }

    @Test
    fun `unknown fields and a sparse note do not break the decode`() {
        val p = page(
            """{"items":[{"projection_id":"p","task_id":"t","title":"Bare",
                          "brand_new_field":{"x":1}}],"total":1}""",
        )
        assertEquals("Bare", p.items.single().title)
        // Assets and timestamps are absent here and must not be required.
        assertTrue(p.items.single().tags.isEmpty())
    }
}

