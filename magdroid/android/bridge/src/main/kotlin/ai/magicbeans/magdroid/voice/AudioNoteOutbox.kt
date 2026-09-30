package ai.magicbeans.magdroid.voice

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.UUID
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
data class PendingAudioNote(
    val id: String,
    val capturedAt: String,
    val durationMs: Long,
    val sourceSurface: String,
    val transcript: String? = null,
    val attemptCount: Int = 0,
    val lastError: String? = null,
    val failedPermanently: Boolean = false,
)

data class AudioNoteOutboxStatus(
    val record: PendingAudioNote,
    val bytes: Long,
)

/**
 * Durable, idempotent Audio Note delivery.
 *
 * The WAV and metadata are written before transcription/upload begins. A
 * process death therefore leaves a retryable recording rather than silently
 * discarding audio the owner explicitly asked to retain. The stable UUID is
 * also the server's idempotency key.
 */
object AudioNoteOutbox {
    private const val WorkName = "magdroid-audio-note-outbox"
    private const val MaxRecords = 50
    private const val MaxBytes = 256L * 1024L * 1024L
    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }
    private val lock = Mutex()
    private val _changes = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    val changes: SharedFlow<Unit> = _changes.asSharedFlow()

    suspend fun enqueue(
        context: Context,
        wav: ByteArray,
        durationMs: Long,
        sourceSurface: String = "android_chat_dictation",
    ): PendingAudioNote = lock.withLock {
        withContext(Dispatchers.IO) {
            require(wav.size > WavPcm.HeaderBytes) { "An Audio Note cannot be empty." }
            val directory = directory(context)
            val existing = recordsUnlocked(directory)
            val used = existing.sumOf { audioFile(directory, it.id).length() }
            if (existing.size >= MaxRecords || used + wav.size > MaxBytes) {
                throw VoiceMediaError(
                    "The Audio Note outbox is full. Retry or discard a pending recording first.",
                    retryable = false,
                )
            }
            val id = UUID.randomUUID().toString()
            val record = PendingAudioNote(
                id = id,
                capturedAt = nowIso8601(),
                durationMs = durationMs.coerceAtLeast(0),
                sourceSurface = sourceSurface,
            )
            atomicWrite(audioFile(directory, id), wav)
            atomicWrite(metadataFile(directory, id), json.encodeToString(PendingAudioNote.serializer(), record).toByteArray())
            // Give the normal STT path time to attach the transcript. The work
            // is already durable, so a process death still uploads the audio;
            // a healthy capture replaces this delayed job as soon as STT ends.
            schedule(context, initialDelaySeconds = 300)
            _changes.tryEmit(Unit)
            record
        }
    }

    suspend fun attachTranscript(context: Context, id: String, transcript: String?) = lock.withLock {
        withContext(Dispatchers.IO) {
            val directory = directory(context)
            val record = read(metadataFile(directory, id)) ?: return@withContext
            val updated = record.copy(transcript = transcript?.trim()?.takeIf(String::isNotEmpty))
            atomicWrite(metadataFile(directory, id), json.encodeToString(PendingAudioNote.serializer(), updated).toByteArray())
            schedule(context, replace = true)
            _changes.tryEmit(Unit)
        }
    }

    suspend fun statuses(context: Context): List<AudioNoteOutboxStatus> = lock.withLock {
        withContext(Dispatchers.IO) {
            val directory = directory(context)
            recordsUnlocked(directory).map { AudioNoteOutboxStatus(it, audioFile(directory, it.id).length()) }
        }
    }

    suspend fun retry(context: Context, id: String) = lock.withLock {
        withContext(Dispatchers.IO) {
            val directory = directory(context)
            val record = read(metadataFile(directory, id)) ?: return@withContext
            val updated = record.copy(failedPermanently = false, lastError = null)
            atomicWrite(metadataFile(directory, id), json.encodeToString(PendingAudioNote.serializer(), updated).toByteArray())
            schedule(context, replace = true)
            _changes.tryEmit(Unit)
        }
    }

    suspend fun discard(context: Context, id: String) = lock.withLock {
        withContext(Dispatchers.IO) {
            val directory = directory(context)
            metadataFile(directory, id).delete()
            audioFile(directory, id).delete()
            _changes.tryEmit(Unit)
        }
    }

    /** Copy one pending recording without retaining the whole WAV on the heap. */
    suspend fun copyRecording(context: Context, id: String, destination: File): Boolean = lock.withLock {
        withContext(Dispatchers.IO) {
            val source = audioFile(directory(context), id)
            if (!source.isFile || source.length() <= WavPcm.HeaderBytes) return@withContext false
            val temporary = File(destination.parentFile, ".${destination.name}.${UUID.randomUUID()}.tmp")
            temporary.outputStream().use { output ->
                source.inputStream().use { input -> input.copyTo(output, DEFAULT_BUFFER_SIZE) }
                output.fd.sync()
            }
            if (!temporary.renameTo(destination)) {
                temporary.delete()
                return@withContext false
            }
            true
        }
    }

    suspend fun flush(context: Context): Boolean = lock.withLock {
        withContext(Dispatchers.IO) {
            val directory = directory(context)
            val client = BackendVoiceClient(context)
            try {
                var retryNeeded = false
                recordsUnlocked(directory).forEach { record ->
                    if (record.failedPermanently) return@forEach
                    val wavFile = audioFile(directory, record.id)
                    if (!wavFile.isFile || wavFile.length() <= WavPcm.HeaderBytes) {
                        mark(directory, record.copy(
                            attemptCount = record.attemptCount + 1,
                            failedPermanently = true,
                            lastError = "The pending recording is missing or empty.",
                        ))
                        return@forEach
                    }
                    runCatching { client.uploadAudioNote(record, wavFile.readBytes()) }
                        .onSuccess {
                            metadataFile(directory, record.id).delete()
                            wavFile.delete()
                        }
                        .onFailure { error ->
                            val media = error as? VoiceMediaError
                            val permanent = media?.retryable == false
                            mark(directory, record.copy(
                                attemptCount = record.attemptCount + 1,
                                failedPermanently = permanent,
                                lastError = error.message?.take(240) ?: "Upload failed.",
                            ))
                            retryNeeded = retryNeeded || !permanent
                        }
                }
                _changes.tryEmit(Unit)
                retryNeeded
            } finally {
                client.close()
            }
        }
    }

    fun schedule(
        context: Context,
        replace: Boolean = false,
        initialDelaySeconds: Long = 0,
    ) {
        val builder = OneTimeWorkRequestBuilder<AudioNoteUploadWorker>()
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
        if (initialDelaySeconds > 0) builder.setInitialDelay(initialDelaySeconds, TimeUnit.SECONDS)
        val request = builder.build()
        WorkManager.getInstance(context.applicationContext).enqueueUniqueWork(
            WorkName,
            if (replace) ExistingWorkPolicy.REPLACE else ExistingWorkPolicy.KEEP,
            request,
        )
    }

    private fun directory(context: Context): File =
        File(context.applicationContext.filesDir, "audio-note-outbox-v1").also(File::mkdirs)

    private fun recordsUnlocked(directory: File): List<PendingAudioNote> {
        val metadata = directory.listFiles { file -> file.extension == "json" }
            .orEmpty().associateBy { it.nameWithoutExtension }
        val ids = (metadata.keys + directory.listFiles { file -> file.extension == "wav" }
            .orEmpty().map { it.nameWithoutExtension }).distinct()
        return ids.mapNotNull { id ->
            val decoded = metadata[id]?.let(::read)
            if (decoded != null) return@mapNotNull decoded
            val wav = audioFile(directory, id)
            if (!wav.isFile) return@mapNotNull null
            val healthy = wav.length() > WavPcm.HeaderBytes
            PendingAudioNote(
                id = id,
                capturedAt = iso8601(wav.lastModified()),
                durationMs = ((wav.length() - WavPcm.HeaderBytes).coerceAtLeast(0) / 32L),
                sourceSurface = "android_recovered_audio_note",
                lastError = if (healthy) {
                    "Recovered after the pending Audio Note metadata was damaged."
                } else {
                    "The pending recording is missing or empty."
                },
                failedPermanently = !healthy,
            ).also { recovered -> markRecovered(directory, recovered) }
        }.sortedByDescending(PendingAudioNote::capturedAt)
    }

    private fun read(file: File): PendingAudioNote? = runCatching {
        json.decodeFromString(PendingAudioNote.serializer(), file.readText())
    }.getOrNull()

    private fun mark(directory: File, record: PendingAudioNote) {
        atomicWrite(
            metadataFile(directory, record.id),
            json.encodeToString(PendingAudioNote.serializer(), record).toByteArray(),
        )
    }

    private fun markRecovered(directory: File, record: PendingAudioNote) = mark(directory, record)

    private fun metadataFile(directory: File, id: String) = File(directory, "$id.json")
    private fun audioFile(directory: File, id: String) = File(directory, "$id.wav")

    private fun atomicWrite(destination: File, bytes: ByteArray) {
        val temporary = File(destination.parentFile, ".${destination.name}.${UUID.randomUUID()}.tmp")
        temporary.outputStream().use { output ->
            output.write(bytes)
            output.fd.sync()
        }
        if (!temporary.renameTo(destination)) {
            temporary.delete()
            throw VoiceMediaError("The Audio Note could not be staged on this phone.", retryable = false)
        }
    }

    private fun nowIso8601(): String = iso8601(System.currentTimeMillis())

    private fun iso8601(epochMillis: Long): String = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", Locale.US)
        .apply { timeZone = TimeZone.getTimeZone("UTC") }
        .format(Date(epochMillis))
}

class AudioNoteUploadWorker(
    appContext: Context,
    parameters: WorkerParameters,
) : CoroutineWorker(appContext, parameters) {
    override suspend fun doWork(): Result = if (AudioNoteOutbox.flush(applicationContext)) {
        Result.retry()
    } else {
        Result.success()
    }
}
