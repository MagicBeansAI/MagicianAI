package ai.magicbeans.magdroid.voice

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import androidx.core.content.ContextCompat
import java.io.ByteArrayOutputStream
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class PcmRecording(val wav: ByteArray, val durationMs: Long)

/** One bounded 16 kHz mono PCM capture, with no networking or UI state. */
class PcmRecorder(private val context: Context) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val recording = AtomicBoolean(false)
    private var audioRecord: AudioRecord? = null
    private var readJob: Job? = null
    private var output: ByteArrayOutputStream? = null
    private var startedAtMs = 0L
    @Volatile private var failure: String? = null

    fun start(): Result<Unit> = runCatching {
        if (recording.get()) return Result.success(Unit)
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            throw VoiceMediaError("Magician needs permission to use the microphone.", retryable = false)
        }
        val minimum = AudioRecord.getMinBufferSize(
            SampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        if (minimum <= 0) throw VoiceMediaError("This phone could not open the microphone.")
        val bufferSize = maxOf(minimum, SampleRate / 5 * 2)
        val recorder = AudioRecord(
            MediaRecorder.AudioSource.VOICE_RECOGNITION,
            SampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            bufferSize,
        )
        if (recorder.state != AudioRecord.STATE_INITIALIZED) {
            recorder.release()
            throw VoiceMediaError("This phone could not initialize voice recording.")
        }
        output = ByteArrayOutputStream(minOf(MaxPcmBytes, SampleRate * 2 * 30))
        failure = null
        recorder.startRecording()
        audioRecord = recorder
        startedAtMs = android.os.SystemClock.elapsedRealtime()
        recording.set(true)
        readJob = scope.launch {
            val buffer = ByteArray(bufferSize)
            while (recording.get()) {
                val count = recorder.read(buffer, 0, buffer.size)
                when {
                    count > 0 -> {
                        val sink = output ?: break
                        if (sink.size() + count > MaxPcmBytes) {
                            failure = "Recording stopped at the 10-minute safety limit."
                            recording.set(false)
                        } else {
                            sink.write(buffer, 0, count - (count % 2))
                        }
                    }
                    count == AudioRecord.ERROR_INVALID_OPERATION || count == AudioRecord.ERROR_BAD_VALUE -> {
                        failure = "The microphone stopped unexpectedly."
                        recording.set(false)
                    }
                }
            }
        }
    }

    suspend fun stop(): PcmRecording = withContext(Dispatchers.IO) {
        val recorder = audioRecord ?: throw VoiceMediaError("No recording is active.", retryable = false)
        recording.set(false)
        runCatching { recorder.stop() }
        readJob?.join()
        recorder.release()
        audioRecord = null
        readJob = null
        failure?.let { throw VoiceMediaError(it, retryable = false) }
        val pcm = output?.toByteArray() ?: ByteArray(0)
        output = null
        if (pcm.isEmpty()) throw VoiceMediaError("No speech was recorded.", retryable = false)
        PcmRecording(
            wav = WavPcm.encode(pcm, SampleRate),
            durationMs = (android.os.SystemClock.elapsedRealtime() - startedAtMs).coerceAtLeast(0),
        )
    }

    fun cancel() {
        recording.set(false)
        runCatching { audioRecord?.stop() }
        audioRecord?.release()
        audioRecord = null
        readJob?.cancel()
        readJob = null
        output = null
        failure = null
    }

    fun close() {
        cancel()
        scope.cancel()
    }

    companion object {
        const val SampleRate = 16_000
        private const val MaxPcmBytes = SampleRate * 2 * 60 * 10
    }
}
