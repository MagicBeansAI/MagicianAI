package ai.magicbeans.magdroid.bridge

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * What the companion has been doing, readable on the phone.
 *
 * This exists because the alternative is a USB cable. A phone failing to
 * connect, or spending a second on every screenshot, should be able to say so
 * to the person holding it — and the person holding it is usually nowhere near
 * a terminal.
 *
 * A bounded ring in memory, not a file. These lines are for diagnosing what is
 * happening now; keeping them across restarts would mean a growing file, a
 * retention policy, and somewhere for a stray token to end up on disk.
 */
object BridgeLog {

    enum class Level { Info, Warn, Error }

    data class Line(
        val level: Level,
        val tag: String,
        val message: String,
        val atMs: Long,
    )

    private const val CAPACITY = 300

    private val _lines = MutableStateFlow<List<Line>>(emptyList())

    /** Chronological and bounded; presentation surfaces choose their ordering. */
    val lines: StateFlow<List<Line>> = _lines.asStateFlow()

    fun info(tag: String, message: String) = record(Level.Info, tag, message)
    fun warn(tag: String, message: String) = record(Level.Warn, tag, message)
    fun error(tag: String, message: String) = record(Level.Error, tag, message)
    fun error(tag: String, message: String, cause: Throwable) =
        record(Level.Error, tag, message, cause)

    /**
     * Record a line here and in logcat.
     *
     * Both, deliberately. The on-device view is for whoever is holding the
     * phone; logcat is for whoever has it plugged in. Writing to one and not
     * the other means the two disagree about what happened, and the one you are
     * not looking at is always the one with the answer.
     */
    @Synchronized
    private fun record(level: Level, tag: String, message: String, cause: Throwable? = null) {
        when (level) {
            Level.Info -> Log.i(tag, message)
            Level.Warn -> Log.w(tag, message)
            Level.Error -> if (cause == null) Log.e(tag, message) else Log.e(tag, message, cause)
        }
        val next = _lines.value + Line(level, tag, message, System.currentTimeMillis())
        // Oldest dropped rather than newest refused: a bridge that has been up
        // for a day should still be able to say what it did a minute ago.
        _lines.value = if (next.size > CAPACITY) next.takeLast(CAPACITY) else next
    }

    fun clear() {
        _lines.value = emptyList()
    }

    /**
     * How long something took, recorded once it has.
     *
     * Timing is the half of this that matters for performance: "screenshot
     * captured" says nothing, "screenshot captured in 1400ms" says where the
     * second went.
     */
    inline fun <T> timed(tag: String, what: String, block: () -> T): T {
        val started = System.currentTimeMillis()
        return try {
            block()
        } finally {
            info(tag, "$what took ${System.currentTimeMillis() - started}ms")
        }
    }
}
