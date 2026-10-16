package ai.magicbeans.magdroid.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Uploading a file, against `upload_attachment_handler` in `chat_api.rs`.
 *
 * The handler reads one multipart `file` part and answers
 * `{attachment_id, filename, mime_type, size}` flat, capping the body at 20 MB
 * with a 413.
 */
class AttachmentUploadTest {
    private val json = Json { ignoreUnknownKeys = true }

    @Test
    fun `the upload response yields the id a send refers to`() {
        val uploaded = json.decodeFromString(
            AttachmentUploaded.serializer(),
            """{"attachment_id": "att-7", "filename": "notes.pdf",
                "mime_type": "application/pdf", "size": 2048}""",
        )
        assertEquals("att-7", uploaded.attachmentId)
        assertEquals("notes.pdf", uploaded.filename)
        assertEquals(2048L, uploaded.size)
    }

    /**
     * A staged file only travels once it has an id. Sending a failed chip's
     * name would tell the assistant about a file it cannot open.
     */
    @Test
    fun `only uploaded attachments have an id to send`() {
        val staged = listOf(
            StagedAttachment("l1", "done.pdf", remoteId = "att-1", uploading = false),
            StagedAttachment("l2", "inflight.pdf", uploading = true),
            StagedAttachment("l3", "broken.pdf", uploading = false, failed = true, error = "too large"),
        )
        assertEquals(listOf("att-1"), staged.mapNotNull { it.remoteId })
    }

    /** A failed chip keeps the reason, so it can say more than that it failed. */
    @Test
    fun `a failed attachment carries why`() {
        val staged = StagedAttachment("l1", "big.mov", uploading = false, failed = true)
            .copy(error = "That file is too large. The limit is 20 MB.")
        assertTrue(staged.failed)
        assertEquals("That file is too large. The limit is 20 MB.", staged.error)
    }

    /**
     * The ids ride on the send body under `attachment_ids`, and an empty list
     * is omitted rather than sent as `[]`.
     */
    @Test
    fun `attachment ids travel on the send body`() {
        val withFiles = chatRequestJson.encodeToString(
            SendMessageRequest.serializer(),
            SendMessageRequest(text = "look at these", chatTurnId = "t1", attachmentIds = listOf("att-1", "att-2")),
        )
        assertTrue(withFiles.contains(""""attachment_ids":["att-1","att-2"]"""))

        val parsed = chatJson.decodeFromString(
            SendMessageRequest.serializer(),
            chatRequestJson.encodeToString(
                SendMessageRequest.serializer(),
                SendMessageRequest(text = "no files", chatTurnId = "t2"),
            ),
        )
        assertTrue(parsed.attachmentIds.isEmpty())
    }

    /** An id-less success is a failure: nothing could reference the file. */
    @Test
    fun `a response without an id is not a usable upload`() {
        val uploaded = json.decodeFromString(
            AttachmentUploaded.serializer(),
            """{"filename": "notes.pdf"}""",
        )
        assertTrue(uploaded.attachmentId.isBlank())
    }

    /**
     * Camera filenames differ only in their tail. Truncating the end makes
     * every photo on the strip read the same.
     */
    @Test
    fun `a long name is shortened from the middle`() {
        val staged = StagedAttachment("l1", "IMG_20260718_151524_9.jpg")
        val short = staged.shortName()
        assertTrue("kept the head", short.startsWith("IMG"))
        assertTrue("kept the tail", short.endsWith(".jpg"))
        assertTrue("shortened", short.length <= 13)
    }

    /** A name that already fits is left alone. */
    @Test
    fun `a short name is untouched`() {
        assertEquals("notes.pdf", StagedAttachment("l1", "notes.pdf").shortName())
    }
}
