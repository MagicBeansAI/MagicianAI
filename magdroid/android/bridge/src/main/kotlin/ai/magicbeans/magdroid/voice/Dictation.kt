package ai.magicbeans.magdroid.voice

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * What dictation is doing, as the composer needs to draw it.
 *
 * `Transcribing` is separate from `Recording` because the two look nothing
 * alike to whoever is holding the phone: one is "keep talking", the other is
 * "stop, I am working on it".
 */
enum class DictationState { Idle, Recording, Transcribing }

/**
 * Voice in, text out.
 *
 * Android's own recogniser does the work, preferring its offline model where
 * the device has one — free, private, and it emits partial results, which is
 * what makes a live transcript above the mic possible at all. iOS makes the
 * same choice for the same reasons, falling back to the backend only when the
 * platform has nothing to offer.
 *
 * The controller is deliberately not a ViewModel: dictation outlives any one
 * screen, and the wake-word and call surfaces will want the same instance.
 */
class Dictation(private val context: Context) {

    private val prefs = VoicePrefs.get(context)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val pcmRecorder = PcmRecorder(context.applicationContext)
    private val backend = BackendVoiceClient(context.applicationContext)

    private val _state = MutableStateFlow(DictationState.Idle)
    val state: StateFlow<DictationState> = _state.asStateFlow()

    /** What has been heard so far this capture. Cleared when a capture starts. */
    private val _partial = MutableStateFlow("")
    val partial: StateFlow<String> = _partial.asStateFlow()

    /** Set when a capture ends badly, so the composer can say why. */
    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private var recognizer: SpeechRecognizer? = null
    private var onFinal: ((String) -> Unit)? = null
    private var onEmpty: (() -> Unit)? = null
    private var cloudCapture = false
    private var retainCurrentRecording = false
    private var transcriptionJob: Job? = null

    /** Whether this device can transcribe at all. */
    fun available(): Boolean = prefs.sttSource.value.allowsCloud ||
        prefs.archiveDictation.value || SpeechRecognizer.isRecognitionAvailable(context)

    /**
     * Begin listening.
     *
     * [onTranscript] receives the finished text once, and only when there is
     * something to receive — an empty capture calls nothing, because inserting
     * an empty string into the composer would look like the app ate the words.
     * [onNoTranscript] settles callers waiting on silence or a capture failure.
     */
    fun start(
        retainRecording: Boolean = prefs.archiveDictation.value,
        onNoTranscript: () -> Unit = {},
        onTranscript: (String) -> Unit,
    ) {
        if (_state.value != DictationState.Idle) return
        if (!available()) {
            _error.value = "This device has no speech recogniser."
            onNoTranscript()
            return
        }
        onFinal = onTranscript
        onEmpty = onNoTranscript
        _partial.value = ""
        _error.value = null

        // Android's SpeechRecognizer owns its microphone and cannot expose the
        // original bytes. Cloud is therefore also the honest path whenever the
        // owner opted to retain the recording: one AudioRecord supplies both
        // STT and the durable Audio Note, with no second hidden microphone.
        retainCurrentRecording = retainRecording
        cloudCapture = prefs.sttSource.value == SttSource.Cloud || retainRecording ||
            (prefs.sttSource.value == SttSource.Auto && !SpeechRecognizer.isRecognitionAvailable(context))
        if (cloudCapture) {
            pcmRecorder.start().fold(
                onSuccess = { _state.value = DictationState.Recording },
                onFailure = { error ->
                    _error.value = error.message ?: "The microphone could not be opened."
                    deliver(null)
                },
            )
            return
        }

        // Built here rather than kept alive: a recogniser held across captures
        // keeps the microphone indicator lit, which reads as the app listening
        // when it is not.
        val speech = SpeechRecognizer.createSpeechRecognizer(context)
        recognizer = speech
        speech.setRecognitionListener(Listener())
        speech.startListening(
            Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(
                    RecognizerIntent.EXTRA_LANGUAGE_MODEL,
                    RecognizerIntent.LANGUAGE_MODEL_FREE_FORM,
                )
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                // Offline where the device has a model: dictation should not
                // depend on the network, and a voice note is not something to
                // send away when it does not have to be.
                // Honours the owner's choice rather than always preferring
                // offline: someone who has picked the backend has usually done
                // so because the on-device model is getting their words wrong.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                    putExtra(
                        RecognizerIntent.EXTRA_PREFER_OFFLINE,
                        prefs.sttSource.value.prefersOnDevice,
                    )
                }
            },
        )
        _state.value = DictationState.Recording
    }

    /**
     * Finish and transcribe what was said.
     *
     * Distinct from [cancel]: this asks the recogniser for its result, which is
     * what a tap on a live mic means. Releasing a held mic means the same.
     */
    fun stop() {
        if (_state.value != DictationState.Recording) return
        _state.value = DictationState.Transcribing
        if (!cloudCapture) {
            recognizer?.stopListening()
            return
        }
        transcriptionJob?.cancel()
        transcriptionJob = scope.launch {
            val outcome = runCatching {
                val recording = pcmRecorder.stop()
                val pending = if (retainCurrentRecording) {
                    AudioNoteOutbox.enqueue(
                        context,
                        recording.wav,
                        recording.durationMs,
                    )
                } else null
                val transcript = runCatching { backend.transcribe(recording.wav) }
                    .onFailure { error ->
                        // The audio remains a valid owner-authorized note even
                        // when STT is temporarily down; its outbox can still
                        // persist the raw recording instead of deleting it.
                        if (pending == null) throw error
                    }
                    .getOrNull()
                if (pending != null) {
                    AudioNoteOutbox.attachTranscript(context, pending.id, transcript)
                    AudioNoteOutbox.schedule(context, replace = true)
                }
                transcript ?: throw VoiceMediaError(
                    "The recording was saved to Audio Notes, but it could not be transcribed.",
                )
            }
            outcome.fold(
                onSuccess = ::deliver,
                onFailure = { error ->
                    _error.value = error.message ?: "Backend transcription failed."
                    deliver(null)
                },
            )
        }
    }

    /** Abandon the capture. Nothing is delivered and nothing is transcribed. */
    fun cancel() {
        transcriptionJob?.cancel()
        transcriptionJob = null
        if (cloudCapture) pcmRecorder.cancel()
        cloudCapture = false
        retainCurrentRecording = false
        onFinal = null
        onEmpty = null
        _partial.value = ""
        _state.value = DictationState.Idle
        release()
    }

    private fun release() {
        recognizer?.let {
            it.setRecognitionListener(null)
            it.destroy()
        }
        recognizer = null
    }

    private fun deliver(text: String?) {
        val trimmed = text?.trim().orEmpty()
        val callback = onFinal
        val emptyCallback = onEmpty
        onFinal = null
        onEmpty = null
        _state.value = DictationState.Idle
        _partial.value = ""
        cloudCapture = false
        retainCurrentRecording = false
        transcriptionJob = null
        release()
        if (trimmed.isNotEmpty()) callback?.invoke(trimmed) else emptyCallback?.invoke()
    }

    private inner class Listener : RecognitionListener {
        override fun onPartialResults(results: Bundle?) {
            results?.transcript()?.let { _partial.value = it }
        }

        override fun onResults(results: Bundle?) = deliver(results?.transcript())

        override fun onError(code: Int) {
            // Silence is not a failure worth reporting: holding the mic and
            // thinking better of it is a normal thing to do.
            val quiet = code == SpeechRecognizer.ERROR_NO_MATCH ||
                code == SpeechRecognizer.ERROR_SPEECH_TIMEOUT
            if (!quiet) _error.value = describe(code)
            deliver(null)
        }

        override fun onReadyForSpeech(params: Bundle?) = Unit
        override fun onBeginningOfSpeech() = Unit
        override fun onRmsChanged(rms: Float) = Unit
        override fun onBufferReceived(buffer: ByteArray?) = Unit
        override fun onEndOfSpeech() {
            // The recogniser has stopped hearing speech but has not answered
            // yet. Saying so is the difference between a considered pause and
            // an app that has hung.
            if (_state.value == DictationState.Recording) {
                _state.value = DictationState.Transcribing
            }
        }

        override fun onEvent(type: Int, params: Bundle?) = Unit
    }

    private fun Bundle.transcript(): String? =
        getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()

    /**
     * A recogniser error, said in terms of what to do about it.
     *
     * The platform's codes are about its own internals; "ERROR_CLIENT" tells
     * whoever is holding the phone nothing they can act on.
     */
    private fun describe(code: Int): String = when (code) {
        SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS ->
            "Magician needs permission to use the microphone."
        SpeechRecognizer.ERROR_NETWORK, SpeechRecognizer.ERROR_NETWORK_TIMEOUT ->
            "Speech recognition could not reach the network."
        SpeechRecognizer.ERROR_RECOGNIZER_BUSY ->
            "The recogniser is busy. Try again in a moment."
        SpeechRecognizer.ERROR_AUDIO -> "The microphone could not be read."
        SpeechRecognizer.ERROR_SERVER -> "The speech service refused the request."
        else -> "Dictation stopped unexpectedly."
    }

    fun close() {
        cancel()
        pcmRecorder.close()
        backend.close()
        scope.cancel()
    }
}
