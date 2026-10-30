package ai.magicbeans.magdroid.voice

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlin.math.log10
import kotlin.math.sqrt

/**
 * One line of the live call's rolling captions. A partial line ([final] false)
 * is still being spoken or streamed and is replaced in place as it grows.
 *
 * [sourceId] is the server's grouping key: `response_id` for the assistant,
 * `item_id` (or the turn generation) for the speaker.
 */
data class RealtimeVoiceCaption(
    val id: Long,
    val fromUser: Boolean,
    val text: String,
    val final: Boolean,
    val sourceId: String? = null,
)

/**
 * The caption reducer, ported from iOS `RealtimeVoiceCaptionState`: a partial
 * grows its own line, the final replaces it in place, and a new utterance
 * appends. Bounded to [Max] lines — the durable transcript is the chat behind
 * the panel, so the call surface only needs a glance's worth.
 */
object RealtimeVoiceCaptions {
    const val Max = 6

    fun apply(
        captions: List<RealtimeVoiceCaption>,
        fromUser: Boolean,
        text: String,
        final: Boolean,
        sourceId: String?,
    ): List<RealtimeVoiceCaption> {
        val clean = text.trim()
        if (clean.isEmpty()) return captions
        val index = captions.indexOfLast { caption ->
            // The speaker has one unfinished line at a time, so its final
            // closes it even when the local partial and the provider final
            // carry different item ids. Assistant lines group by response.
            !caption.final && caption.fromUser == fromUser && (fromUser || caption.sourceId == sourceId)
        }
        val next = if (index >= 0) {
            captions.toMutableList().also {
                it[index] = it[index].copy(
                    text = clean,
                    final = final,
                    sourceId = sourceId ?: it[index].sourceId,
                )
            }
        } else {
            val nextId = (captions.maxOfOrNull { it.id } ?: 0L) + 1L
            captions + RealtimeVoiceCaption(nextId, fromUser, clean, final, sourceId)
        }
        return if (next.size > Max) next.takeLast(Max) else next
    }

    /** `transcript.user.cleared` / `.ignored`: the speaker's unfinished line was not a turn. */
    fun clearUnfinishedUser(captions: List<RealtimeVoiceCaption>): List<RealtimeVoiceCaption> =
        if (captions.none { it.fromUser && !it.final }) captions
        else captions.filterNot { it.fromUser && !it.final }
}

/**
 * Turns 20 ms PCM16 capture chunks into a smoothed 0..1 microphone level for
 * the call orb, published at most every [intervalNanos] (~16 Hz) so a 50 Hz
 * capture loop cannot drive a 50 Hz recomposition.
 *
 * Not thread-safe: one instance per capture loop.
 */
class MicLevelMeter(private val intervalNanos: Long = 60_000_000L) {
    private var level = 0f
    private var published = 0f
    private var lastPublishedAt = 0L

    /**
     * Feed one chunk. [gateOpen] false (muted, or hold-to-talk not held) reads
     * as silence, so the orb rests while nothing leaves the phone. Returns the
     * level to publish, or null when nothing should be published yet.
     */
    fun feed(buffer: ByteArray, count: Int, gateOpen: Boolean, nowNanos: Long = System.nanoTime()): Float? {
        val target = if (gateOpen) levelOf(buffer, count) else 0f
        // Fast attack, slower release: speech onsets show at once, and the orb
        // does not flicker between syllables.
        level += (target - level) * (if (target > level) 0.6f else 0.25f)
        if (level < 0.01f) level = 0f
        if (nowNanos - lastPublishedAt < intervalNanos) return null
        if (kotlin.math.abs(level - published) < 0.01f && !(level == 0f && published != 0f)) return null
        lastPublishedAt = nowNanos
        published = level
        return level
    }

    companion object {
        /** RMS of little-endian PCM16, mapped from -50..0 dBFS onto 0..1. */
        fun levelOf(buffer: ByteArray, count: Int): Float {
            val samples = minOf(count, buffer.size) / 2
            if (samples <= 0) return 0f
            var sum = 0.0
            for (index in 0 until samples) {
                val low = buffer[index * 2].toInt() and 0xff
                val high = buffer[index * 2 + 1].toInt()
                val sample = ((high shl 8) or low).toShort().toDouble() / 32768.0
                sum += sample * sample
            }
            val rms = sqrt(sum / samples)
            if (rms <= 1e-5) return 0f
            val decibels = 20.0 * log10(rms)
            return ((decibels + 50.0) / 50.0).coerceIn(0.0, 1.0).toFloat()
        }
    }
}

/**
 * The call state without its hot field. [RealtimeVoiceState.micLevel] changes
 * ~16 times a second; a screen that collects the whole state would recompose
 * at that rate. Collect this for the screen and [liveMicLevel] for the orb,
 * read only in draw lambdas.
 */
fun StateFlow<RealtimeVoiceState>.withoutMicLevel(): Flow<RealtimeVoiceState> =
    map { it.copy(micLevel = 0f, playbackLevel = 0f) }.distinctUntilChanged()

fun StateFlow<RealtimeVoiceState>.liveMicLevel(): Flow<Float> =
    map { it.micLevel }.distinctUntilChanged()

fun StateFlow<RealtimeVoiceState>.livePlaybackLevel(): Flow<Float> =
    map { it.playbackLevel }.distinctUntilChanged()

/** Meter the PCM at the speaker's playback head, not its network arrival time. */
class PlaybackLevelMeter {
    private data class Window(val startFrame: Long, val endFrame: Long, val level: Float)
    private val windows = ArrayDeque<Window>()

    @Synchronized fun queue(bytes: ByteArray, startFrame: Long, sampleRate: Int) {
        val step = (sampleRate / 20).coerceAtLeast(1) * 2
        var offset = 0
        while (offset < bytes.size) {
            val end = minOf(bytes.size, offset + step)
            val slice = bytes.copyOfRange(offset, end)
            windows.addLast(Window(startFrame + offset / 2, startFrame + end / 2, MicLevelMeter.levelOf(slice, slice.size)))
            offset = end
        }
        while (windows.size > 600) windows.removeFirst()
    }

    @Synchronized fun levelAt(frame: Long): Float {
        while (windows.isNotEmpty() && frame >= windows.first().endFrame) windows.removeFirst()
        return windows.firstOrNull()?.takeIf { frame >= it.startFrame }?.level ?: 0f
    }

    @Synchronized fun reset() = windows.clear()
}
