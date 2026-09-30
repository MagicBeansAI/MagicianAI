package ai.magicbeans.magdroid.voice

import android.content.Context
import android.media.MediaPlayer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.io.File
import java.util.Locale
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class SpeechOutcome { Completed, Cancelled, Failed }

/**
 * Reading replies aloud.
 *
 * On-device, like iOS: free, offline, and it starts speaking immediately
 * instead of waiting on a round trip. The backend has a nicer voice, and that
 * is the fallback worth adding later — not the thing to depend on for the
 * common case.
 *
 * Markdown is flattened before speaking. A reply read literally says "hash
 * hash Summary" and "star star important star star", which is the kind of
 * detail that makes synthesized speech unbearable rather than merely robotic.
 */
class Speech(context: Context) {

    private val app = context.applicationContext
    private val prefs = VoicePrefs.get(app)
    private val backend = BackendVoiceClient(app)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    private val _speaking = MutableStateFlow(false)
    val speaking: StateFlow<Boolean> = _speaking.asStateFlow()

    private var ready = false
    private var backendJob: Job? = null
    private var player: MediaPlayer? = null
    private var playerFile: File? = null
    private val generation = AtomicInteger(0)
    private var completion: (() -> Unit)? = null
    private var managedCompletion: ((SpeechOutcome) -> Unit)? = null
    private var started: (() -> Unit)? = null

    // Declared explicitly: the init callback refers to the engine it is
    // initialising, which an inferred type cannot resolve.
    private val engine: TextToSpeech = TextToSpeech(app) { status ->
        ready = status == TextToSpeech.SUCCESS
        if (ready) engine.language = Locale.getDefault()
    }

    init {
        engine.setOnUtteranceProgressListener(
            object : UtteranceProgressListener() {
                override fun onStart(utteranceId: String?) {
                    val current = utteranceId?.substringAfterLast('-')?.toIntOrNull() ?: return
                    scope.launch { if (current == generation.get()) { _speaking.value = true; didStart() } }
                }
                override fun onDone(utteranceId: String?) { finish(utteranceId) }

                @Deprecated("Required by the platform's abstract class.")
                override fun onError(utteranceId: String?) { finish(utteranceId, SpeechOutcome.Failed) }
                override fun onError(utteranceId: String?, errorCode: Int) { finish(utteranceId, SpeechOutcome.Failed) }
                override fun onStop(utteranceId: String?, interrupted: Boolean) {
                    finish(utteranceId, SpeechOutcome.Cancelled)
                }
            },
        )
    }

    /**
     * Speak a reply.
     *
     * Takes the raw reply and does the extraction itself, so no caller can
     * forget and read protocol markup aloud.
     */
    fun speak(raw: String, onComplete: (() -> Unit)? = null) {
        begin(raw, onComplete, null, null)
    }

    fun speakManaged(raw: String, onStart: () -> Unit, onOutcome: (SpeechOutcome) -> Unit) {
        begin(raw, null, onStart, onOutcome)
    }

    private fun begin(raw: String, onComplete: (() -> Unit)?, onStart: (() -> Unit)?, onOutcome: ((SpeechOutcome) -> Unit)?) {
        val text = plain(SpeechTags.spokenText(raw))
        if (text.isBlank()) {
            onComplete?.invoke()
            onOutcome?.invoke(SpeechOutcome.Failed)
            return
        }
        stop()
        completion = onComplete
        managedCompletion = onOutcome
        started = onStart
        val current = generation.incrementAndGet()
        if (prefs.ttsEngine.value == TtsEngine.Magician) {
            _speaking.value = true
            backendJob = scope.launch {
                val audio = runCatching { withContext(Dispatchers.IO) { backend.synthesize(text) } }
                    .getOrNull()
                if (current != generation.get()) return@launch
                if (audio != null && playBackend(audio, current)) return@launch
                speakOnDevice(text, current)
            }
        } else {
            speakOnDevice(text, current)
        }
    }

    /** Stop mid-sentence. Interrupting is the point; a queue would defeat it. */
    fun stop() {
        val managed = managedCompletion
        managedCompletion = null
        started = null
        generation.incrementAndGet()
        backendJob?.cancel()
        backendJob = null
        if (ready) engine.stop()
        player?.setOnCompletionListener(null)
        player?.setOnErrorListener(null)
        player?.stop()
        player?.release()
        player = null
        playerFile?.delete()
        playerFile = null
        completion = null
        _speaking.value = false
        managed?.invoke(SpeechOutcome.Cancelled)
    }

    fun shutdown() {
        stop()
        backend.close()
        scope.cancel()
        engine.shutdown()
    }

    private fun speakOnDevice(text: String, current: Int) {
        if (current != generation.get()) return
        if (!ready) {
            finishCurrent(current, SpeechOutcome.Failed)
            return
        }
        _speaking.value = true
        if (engine.speak(text, TextToSpeech.QUEUE_FLUSH, null, "$UTTERANCE-$current") == TextToSpeech.ERROR) {
            finishCurrent(current, SpeechOutcome.Failed)
        }
    }

    private fun playBackend(bytes: ByteArray, current: Int): Boolean = runCatching {
        val file = File.createTempFile("magdroid-tts-", ".audio", app.cacheDir)
        file.writeBytes(bytes)
        val created = MediaPlayer().apply {
            setDataSource(file.absolutePath)
            setOnCompletionListener {
                it.release()
                if (player === it) player = null
                file.delete()
                if (playerFile == file) playerFile = null
                finishCurrent(current)
            }
            setOnErrorListener { media, _, _ ->
                media.release()
                if (player === media) player = null
                file.delete()
                if (playerFile == file) playerFile = null
                finishCurrent(current, SpeechOutcome.Failed)
                true
            }
            prepare()
        }
        if (current != generation.get()) {
            created.release()
            file.delete()
            return false
        }
        playerFile = file
        player = created
        _speaking.value = true
        created.start()
        didStart()
        true
    }.getOrElse { false }

    private fun didStart() { val callback = started; started = null; callback?.invoke() }

    private fun finish(utteranceId: String?, outcome: SpeechOutcome = SpeechOutcome.Completed) {
        val current = utteranceId?.substringAfterLast('-')?.toIntOrNull() ?: return
        scope.launch { finishCurrent(current, outcome) }
    }

    private fun finishCurrent(current: Int, outcome: SpeechOutcome = SpeechOutcome.Completed) {
        if (current != generation.get()) return
        _speaking.value = false
        val callback = completion
        completion = null
        val managed = managedCompletion
        managedCompletion = null
        started = null
        callback?.invoke()
        managed?.invoke(outcome)
    }

    private companion object {
        const val UTTERANCE = "magician-reply"

        // Markdown, as spoken prose. Fences and their contents go entirely:
        // code read aloud is noise, and the sentences around it carry the
        // meaning.
        val fence = Regex("""```[\s\S]*?```""")
        val inlineCode = Regex("""`([^`]*)`""")
        val link = Regex("""\[([^\]]+)]\([^)]*\)""")
        val image = Regex("""!\[[^\]]*]\([^)]*\)""")
        val emphasis = Regex("""(\*\*|__|\*|_|~~)""")
        val heading = Regex("""(?m)^\s{0,3}#{1,6}\s*""")
        val quote = Regex("""(?m)^\s{0,3}>\s?""")
        val bullet = Regex("""(?m)^\s{0,3}[-*+]\s+""")
        val rule = Regex("""(?m)^\s{0,3}([-*_])\s*\1\s*\1[-*_\s]*$""")
        val blankLines = Regex("""\n{2,}""")
        val spaces = Regex("""[ \t]{2,}""")
    }

    private fun plain(markdown: String): String = markdown
        .replace(fence, " ")
        .replace(image, " ")
        .replace(link, "$1")
        .replace(inlineCode, "$1")
        .replace(rule, " ")
        .replace(heading, "")
        .replace(quote, "")
        // A bullet becomes a sentence break, so a list is heard as separate
        // items rather than one run-on line.
        .replace(bullet, ". ")
        .replace(emphasis, "")
        .replace(blankLines, ". ")
        .replace("\n", " ")
        .replace(spaces, " ")
        .trim()
}
