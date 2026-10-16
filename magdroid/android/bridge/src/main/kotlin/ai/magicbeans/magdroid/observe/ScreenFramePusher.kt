package ai.magicbeans.magdroid.observe

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Pushes screen keyframes, newest first, dropping what it could not keep up with.
 *
 * A port of `ObservationFramePusher` in `ObservationUplink.swift`. One slot, not
 * a queue: a frame that has been superseded is of no use to a narrator, and
 * uploading a backlog of stale screens off a phone would spend a meeting's
 * bandwidth telling the server what was on screen a minute ago.
 *
 * So capture never waits for upload. It overwrites the pending slot and returns;
 * the drain loop takes whatever is there when it comes round. On a slow link
 * that means frames are skipped rather than delayed, which is the behaviour
 * worth having when only the latest matters.
 */
class ScreenFramePusher(
    private val scope: CoroutineScope,
    private val upload: suspend (ByteArray) -> Upload,
) {

    private val lock = Mutex()
    private var pending: ByteArray? = null
    private var draining: Job? = null

    @Volatile
    var stopped: Boolean = false
        private set

    /** Offer a frame. Returns at once; the newest offer wins. */
    suspend fun push(jpeg: ByteArray) {
        if (stopped) return
        val start = lock.withLock {
            pending = jpeg
            // One drain at a time. A second would race the first for the slot
            // and could upload the older of two frames.
            if (draining?.isActive == true) return@withLock false
            true
        }
        if (start) {
            draining = scope.launch { drain() }
        }
    }

    private suspend fun drain() {
        while (true) {
            val next = lock.withLock {
                val frame = pending
                pending = null
                frame
            } ?: return
            // Terminal: the observation is gone and nothing further will land.
            if (upload(next) == Upload.Ended) {
                stopped = true
                lock.withLock { pending = null }
                return
            }
        }
    }
}
