package ai.magicbeans.magdroid.observe

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Newest frame wins, and the ones it overtook are dropped.
 *
 * A port of iOS's `ObservationFramePusher`. One slot rather than a queue: a
 * superseded screen is of no use to a narrator, and uploading a backlog off a
 * phone spends a meeting's bandwidth describing what was on screen a minute ago.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ScreenFramePusherTest {

    private fun frame(n: Int) = byteArrayOf(n.toByte())

    @Test
    fun `a single frame is uploaded`() = runTest {
        val sent = mutableListOf<Int>()
        val pusher = ScreenFramePusher(this) { sent += it.first().toInt(); Upload.Landed }
        pusher.push(frame(1))
        testScheduler.advanceUntilIdle()
        assertEquals(listOf(1), sent)
    }

    /**
     * The point of the whole class. While one upload is in flight, later frames
     * overwrite each other, and the drain takes only the last.
     */
    @Test
    fun `frames offered during an upload collapse to the newest`() = runTest {
        val gate = CompletableDeferred<Unit>()
        val sent = mutableListOf<Int>()
        val pusher = ScreenFramePusher(this) { jpeg ->
            if (jpeg.first().toInt() == 1) gate.await()
            sent += jpeg.first().toInt()
            Upload.Landed
        }

        pusher.push(frame(1))
        testScheduler.advanceUntilIdle()
        // Three more while the first is still uploading. Two of them are stale
        // the moment the next arrives.
        pusher.push(frame(2))
        pusher.push(frame(3))
        pusher.push(frame(4))
        gate.complete(Unit)
        testScheduler.advanceUntilIdle()

        assertEquals("only the first and the newest should land", listOf(1, 4), sent)
    }

    /** A 410 stops the pusher, and nothing offered afterwards is sent. */
    @Test
    fun `an ended session stops the pusher for good`() = runTest {
        val sent = mutableListOf<Int>()
        val pusher = ScreenFramePusher(this) { jpeg ->
            sent += jpeg.first().toInt()
            Upload.Ended
        }
        pusher.push(frame(1))
        testScheduler.advanceUntilIdle()
        assertTrue(pusher.stopped)

        pusher.push(frame(2))
        testScheduler.advanceUntilIdle()
        assertEquals("nothing after the session ended", listOf(1), sent)
    }

    /** A transient failure is not terminal: the next frame still goes. */
    @Test
    fun `a transient failure keeps the pusher running`() = runTest {
        val sent = mutableListOf<Int>()
        val pusher = ScreenFramePusher(this) { jpeg ->
            sent += jpeg.first().toInt()
            if (jpeg.first().toInt() == 1) Upload.Transient else Upload.Landed
        }
        pusher.push(frame(1))
        testScheduler.advanceUntilIdle()
        pusher.push(frame(2))
        testScheduler.advanceUntilIdle()

        assertEquals(listOf(1, 2), sent)
        assertTrue(!pusher.stopped)
    }
}
