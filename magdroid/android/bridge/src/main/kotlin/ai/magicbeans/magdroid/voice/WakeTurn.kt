package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.ChatStreamEvent
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.dropWhile
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.cancel

/**
 * One whole turn, from a wake word to a spoken answer, with no screen involved.
 *
 * The wake handler used to be a Compose effect on the chat screen, which meant
 * the phone listened while locked and then did nothing with what it heard: the
 * collector stops below STARTED, so the wake fired into an empty room. Speaking
 * to a device that heard you and stayed silent is worse than one that was never
 * listening.
 *
 * Everything here therefore runs off the service: capture, send, and speak. The
 * answer comes back as speech because the screen is usually off when this path
 * is the one being used.
 */
class WakeTurn(
    private val context: Context,
    private val dictation: Dictation,
    private val speech: Speech,
    private val repository: ChatRepository = ChatRepository(context),
    private val prefs: VoicePrefs = VoicePrefs.get(context),
) {
    /** What the turn is doing, so the notification can say. */
    enum class Phase { Idle, Listening, Thinking, Speaking }

    private val _phase = MutableStateFlow(Phase.Idle)
    val phase: StateFlow<Phase> = _phase.asStateFlow()

    private val scope = CoroutineScope(Dispatchers.Main)
    private var sessionId: String? = null
    private var captureRelease: (() -> Unit)? = null
    private var continuous = false

    /**
     * Begin a turn.
     *
     * [onCaptureFinished] fires when the microphone is free again, whether or
     * not anything was said — the wake spotter has to take the microphone back,
     * and it must do so on the failure paths too or the phone stops listening
     * after the first thing it mishears.
     */
    fun begin(onCaptureFinished: () -> Unit) {
        if (_phase.value != Phase.Idle) return
        captureRelease = once(onCaptureFinished)
        continuous = prefs.ambientMode.value == AmbientVoiceMode.HandsFree

        // Anything already being spoken is interrupted: the wake word is a new
        // request, and answering it over the tail of the last answer is
        // unintelligible.
        speech.stop()

        // The recogniser must be created on the main thread, and this is called
        // from Vosk's decoding thread.
        scope.launch { listenForTurn() }
    }

    private fun listenForTurn() {
        _phase.value = Phase.Listening
        dictation.start(retainRecording = false) { transcript ->
            if (!continuous) releaseCapture()
            ask(transcript)
        }
        // A capture that yields nothing never reaches the callback above, so
        // the microphone is handed back when dictation settles. For Hands-free
        // this also ends the open conversation instead of looping forever on
        // an empty room.
        scope.launch {
            dictation.state.dropWhile { it == DictationState.Idle }.first { it == DictationState.Idle }
            if (_phase.value == Phase.Listening) {
                _phase.value = Phase.Idle
                releaseCapture()
            }
        }
    }

    /**
     * Asking about the screen, said aloud.
     *
     * Matched here rather than left to the backend because the capture has to
     * happen on the device before the turn is sent — by the time the backend
     * could tell us it was a screen question, the screen would already be
     * whatever the phone drifted to.
     *
     * Deliberately narrow. "What is on my screen" is a request; "my screen is
     * cracked" is a complaint, and capturing for the second would be a
     * surprise.
     */
    private fun asksAboutScreen(text: String): Boolean {
        val said = text.lowercase()
        val aboutScreen = SCREEN_WORDS.any { said.contains(it) }
        val isAQuestion = QUESTION_OPENERS.any { said.trimStart().startsWith(it) } ||
            said.contains("explain") || said.contains("show me")
        return aboutScreen && isAQuestion
    }

    /** Asking the phone to do it, rather than to explain it. */
    private fun asksToAct(text: String): Boolean =
        ACT_VERBS.any { text.lowercase().contains(it) }

    private fun ask(text: String) {
        _phase.value = Phase.Thinking
        scope.launch {
            // A screen question captures first. The marker is prepended so it
            // reaches the same tutor lane a typed "@tutor" does, rather than a
            // second path that has to be kept correct separately.
            val prompt = if (asksAboutScreen(text)) {
                ai.magicbeans.magdroid.tutor.TutorScreenGrab.grab()
                // "Show me" explains; "do it" acts. The distinction is the
                // owner's word, not a guess — an agent that taps when it was
                // asked to explain is the failure worth avoiding.
                if (asksToAct(text)) "@copilot $text" else "@tutor $text"
            } else {
                text
            }
            val answer = runCatching { send(prompt) }.getOrNull()
            if (answer.isNullOrBlank()) {
                _phase.value = Phase.Idle
                releaseCapture()
                return@launch
            }
            // Spoken unless the owner has muted replies. A locked phone that
            // answers aloud when it was told not to is the worst version of
            // this feature.
            if (continuous || prefs.speakReplies.value) {
                _phase.value = Phase.Speaking
                speech.speak(answer) {
                    if (continuous) listenForTurn() else _phase.value = Phase.Idle
                }
            } else if (continuous) {
                listenForTurn()
            } else {
                _phase.value = Phase.Idle
            }
        }
    }

    private fun releaseCapture() {
        continuous = false
        captureRelease?.invoke()
        captureRelease = null
    }

    private fun once(action: () -> Unit): () -> Unit {
        var called = false
        return {
            if (!called) {
                called = true
                action()
            }
        }
    }

    fun shutdown() {
        continuous = false
        captureRelease = null
        dictation.close()
        speech.shutdown()
        scope.cancel()
        _phase.value = Phase.Idle
    }

    /**
     * Send the turn and wait for what settled.
     *
     * The streamed tokens are ignored: there is nothing to draw them on, and
     * speaking a half-finished sentence then correcting it is worse than a
     * pause. `done` carries the persisted answer, which is what gets read.
     */
    private suspend fun send(text: String): String? = withContext(Dispatchers.IO) {
        val session = sessionId ?: repository.newSession().also { sessionId = it }
        var settled: String? = null
        repository.send(
            sessionId = session,
            text = text,
            chatTurnId = java.util.UUID.randomUUID().toString(),
        )
            .catch { settled = null }
            .collect { event ->
                if (event is ChatStreamEvent.Done) {
                    settled = event.messages
                        .firstOrNull { !it.fromUser && it.text.isNotBlank() }
                        ?.text
                }
            }
        settled
    }

    private companion object {
        val SCREEN_WORDS = listOf("my screen", "this screen", "on screen", "this page", "this app")
        val QUESTION_OPENERS = listOf("what", "how", "why", "who", "where", "which")
        val ACT_VERBS = listOf("do it for me", "do this for me", "tap ", "press ", "fill in", "book ", "send ")
    }
}
