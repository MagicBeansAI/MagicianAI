package ai.magicbeans.magdroid.voice

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * While a live call is up, only the call talks.
 *
 * Every realtime call holds this from the moment it starts connecting until
 * it has torn down, and every app text-to-speech path asks it first: spoken
 * replies, dictation speak-back, the per-message Speak button, the tutor's
 * captions and completed concurrent-voice results (replays included). A
 * result that finishes during a call waits for the hang-up instead of being
 * read over the provider's voice; a reply or a Speak tap during a call is not
 * spoken, as on iOS (`VoiceCallAudioFocus` / `SpeechSynthesizer.permitsSpeech`).
 *
 * Process-wide, like the microphone and the speaker it arbitrates: the chat
 * screen's call and the wake service's call are different sessions, and
 * neither may be talked over by the other's app voice.
 */
open class CallAudioFocus {
    private val owners = mutableSetOf<Any>()
    private val _active = MutableStateFlow(false)

    /** True while at least one live call holds focus. */
    val active: StateFlow<Boolean> = _active.asStateFlow()

    /** Hold focus for [owner]; holding it twice is one hold. */
    fun acquire(owner: Any) = synchronized(owners) {
        owners.add(owner)
        _active.value = true
    }

    /** Give [owner]'s hold back; releasing what is not held is a no-op. */
    fun release(owner: Any) = synchronized(owners) {
        owners.remove(owner)
        _active.value = owners.isNotEmpty()
    }

    /** May the app speak through its own TTS right now? */
    fun permitsLocalSpeech(): Boolean = !_active.value
}

object LiveCallAudioFocus : CallAudioFocus()
