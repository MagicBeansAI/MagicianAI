package ai.magicbeans.magdroid.observe

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * What the server said about one upload.
 *
 * The 410 is the load-bearing case. It is how a stop made anywhere else — the
 * web, another device, the server itself — reaches this one: iOS documents it
 * as the cascade that ends in-app capture. Collapsed into "failed", the
 * microphone carried on recording and uploading into a session that had already
 * ended, and the remote stop silently did nothing here.
 */
class UploadTest {

    @Test
    fun `410 is terminal and nothing else is`() {
        assertEquals(Upload.Ended, Upload.of(410))
        // Neighbours in the 4xx range are not the cascade and must not stop a
        // capture the owner is still in.
        assertEquals(Upload.Transient, Upload.of(409))
        assertEquals(Upload.Transient, Upload.of(411))
    }

    @Test
    fun `success is success across the range`() {
        listOf(200, 201, 202, 204, 299).forEach {
            assertEquals("$it should land", Upload.Landed, Upload.of(it))
        }
    }

    /**
     * Everything else is worth retrying. A 5xx, or auth that briefly lapsed, is
     * the server having a bad moment rather than the session being over.
     */
    @Test
    fun `failures that are not 410 keep the capture running`() {
        listOf(400, 401, 403, 404, 429, 500, 502, 503).forEach {
            assertEquals("$it should be transient", Upload.Transient, Upload.of(it))
        }
    }
}
