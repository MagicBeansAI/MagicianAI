package ai.magicbeans.magdroid.voice

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Voice preferences that outlive a screen.
 *
 * Whether replies are spoken is a standing choice, not a per-session one:
 * somebody who muted the assistant on the bus does not want it talking again
 * because they reopened the app.
 */
class VoicePrefs private constructor(context: Context) {

    private val store = context.applicationContext
        .getSharedPreferences("magdroid.voice", Context.MODE_PRIVATE)

    private val _speakReplies = MutableStateFlow(store.getBoolean(SPEAK, true))
    val speakReplies: StateFlow<Boolean> = _speakReplies.asStateFlow()

    private val _stt = MutableStateFlow(SttSource.from(store.getString(STT, null)))
    val sttSource: StateFlow<SttSource> = _stt.asStateFlow()

    private val _tts = MutableStateFlow(TtsEngine.from(store.getString(TTS, null)))
    val ttsEngine: StateFlow<TtsEngine> = _tts.asStateFlow()

    private val _archiveDictation = MutableStateFlow(store.getBoolean(ARCHIVE_DICTATION, false))
    val archiveDictation: StateFlow<Boolean> = _archiveDictation.asStateFlow()

    private val _liveEngine = MutableStateFlow(LiveVoiceEngine.from(store.getString(LIVE_ENGINE, null)))
    val liveEngine: StateFlow<LiveVoiceEngine> = _liveEngine.asStateFlow()

    private val _realtimeProfile = MutableStateFlow(store.getString(REALTIME_PROFILE, null))
    val realtimeProfile: StateFlow<String?> = _realtimeProfile.asStateFlow()

    private val _observeScreen = MutableStateFlow(store.getBoolean(OBSERVE_SCREEN, false))
    val observeScreen: StateFlow<Boolean> = _observeScreen.asStateFlow()

    private val _livePttOn = MutableStateFlow(store.getBoolean(LIVE_PTT, false))
    val livePttOn: StateFlow<Boolean> = _livePttOn.asStateFlow()

    private val _audioProfiles = MutableStateFlow(
        NativeAudioSurface.entries.mapNotNull { surface ->
            store.getString(audioProfileKey(surface), null)?.let { surface to it }
        }.toMap(),
    )
    val audioProfiles: StateFlow<Map<NativeAudioSurface, String>> = _audioProfiles.asStateFlow()

    private val _audioStageOptions = MutableStateFlow(
        NativeAudioSurface.entries.flatMap { surface ->
            NativeAudioStage.entries.mapNotNull { stage ->
                store.getString(audioStageKey(surface, stage), null)?.let { (surface to stage) to it }
            }
        }.toMap(),
    )
    val audioStageOptions: StateFlow<Map<Pair<NativeAudioSurface, NativeAudioStage>, String>> =
        _audioStageOptions.asStateFlow()

    fun setSpeakReplies(enabled: Boolean) {
        _speakReplies.value = enabled
        // Marked chosen: from here the account's value no longer overrides
        // this handset's, because the owner has said what they want on it.
        store.edit().putBoolean(SPEAK, enabled).putBoolean(SEEDED, true).apply()
    }

    /**
     * Adopt the account's value, once.
     *
     * A device that has never been told anything takes what the account says,
     * so a new phone arrives already muted if the others are. After the owner
     * chooses here, their choice stands until they change it — a later sync
     * silently flipping a switch they set is worse than the two disagreeing.
     *
     * Returns true when the value was taken.
     */
    fun seedSpeakReplies(fromAccount: Boolean): Boolean {
        if (store.getBoolean(SEEDED, false)) return false
        _speakReplies.value = fromAccount
        store.edit().putBoolean(SPEAK, fromAccount).putBoolean(SEEDED, true).apply()
        return true
    }

    fun setSttSource(source: SttSource) {
        _stt.value = source
        store.edit().putString(STT, source.wire).apply()
    }

    private val _leash = MutableStateFlow(AmbientLeash.from(store.getString(LEASH, null)))
    val leash: StateFlow<AmbientLeash> = _leash.asStateFlow()

    private val _ambientMode = MutableStateFlow(AmbientVoiceMode.from(store.getString(AMBIENT, null)))
    val ambientMode: StateFlow<AmbientVoiceMode> = _ambientMode.asStateFlow()

    fun setLeash(value: AmbientLeash) {
        _leash.value = value
        store.edit().putString(LEASH, value.wire).apply()
    }

    fun setAmbientMode(value: AmbientVoiceMode) {
        _ambientMode.value = value
        store.edit().putString(AMBIENT, value.wire).apply()
    }

    fun setTtsEngine(engine: TtsEngine) {
        _tts.value = engine
        store.edit().putString(TTS, engine.wire).apply()
    }

    /** Explicit device-local consent; ordinary dictation is ephemeral. */
    fun setArchiveDictation(enabled: Boolean) {
        _archiveDictation.value = enabled
        store.edit().putBoolean(ARCHIVE_DICTATION, enabled).apply()
    }

    fun setLiveEngine(engine: LiveVoiceEngine) {
        _liveEngine.value = engine
        store.edit().putString(LIVE_ENGINE, engine.wire).apply()
    }

    fun setRealtimeProfile(id: String) {
        val clean = id.trim().takeIf(String::isNotEmpty) ?: return
        _realtimeProfile.value = clean
        store.edit().putString(REALTIME_PROFILE, clean).apply()
    }

    /**
     * Whether an observation also captures the screen.
     *
     * Off unless asked for. Recording the room and recording the screen are
     * different consents, and one must not be taken as the other.
     */
    fun setObserveScreen(enabled: Boolean) {
        _observeScreen.value = enabled
        store.edit().putBoolean(OBSERVE_SCREEN, enabled).apply()
    }

    fun setLivePttOn(enabled: Boolean) {
        _livePttOn.value = enabled
        store.edit().putBoolean(LIVE_PTT, enabled).apply()
    }

    /** Seed once from the selected provider; explicit device choices always win. */
    fun seedLivePttOn(enabled: Boolean) {
        if (store.contains(LIVE_PTT)) return
        setLivePttOn(enabled)
    }

    fun setAudioProfile(surface: NativeAudioSurface, id: String) {
        val clean = id.trim().takeIf(String::isNotEmpty) ?: return
        _audioProfiles.value = _audioProfiles.value + (surface to clean)
        store.edit().putString(audioProfileKey(surface), clean).apply()
    }

    fun setAudioStageOption(surface: NativeAudioSurface, stage: NativeAudioStage, id: String?) {
        val key = surface to stage
        val clean = id?.trim()?.takeIf(String::isNotEmpty)
        _audioStageOptions.value = if (clean == null) _audioStageOptions.value - key
        else _audioStageOptions.value + (key to clean)
        store.edit().let { editor ->
            if (clean == null) editor.remove(audioStageKey(surface, stage))
            else editor.putString(audioStageKey(surface, stage), clean)
        }.apply()
    }

    companion object {
        private const val SPEAK = "speak_replies"
        private const val SEEDED = "speak_replies_seeded"
        private const val STT = "stt_source"
        private const val TTS = "tts_engine"
        private const val LEASH = "ambient_leash"
        private const val AMBIENT = "ambient_mode"
        private const val ARCHIVE_DICTATION = "archive_chat_dictation"
        private const val LIVE_ENGINE = "live_voice_engine"
        private const val REALTIME_PROFILE = "realtime_voice_profile"
        private const val LIVE_PTT = "live_voice_ptt_on"
        private const val OBSERVE_SCREEN = "observe_screen"

        @Volatile
        private var instance: VoicePrefs? = null

        /**
         * One instance for the process.
         *
         * The composer, the settings sheet and whatever speaks have to agree,
         * and two readers of the same file would drift the moment one wrote.
         */
        fun get(context: Context): VoicePrefs =
            instance ?: synchronized(this) {
                instance ?: VoicePrefs(context).also { instance = it }
            }

        private fun audioProfileKey(surface: NativeAudioSurface) = "audio_profile.${surface.wire}"
        private fun audioStageKey(surface: NativeAudioSurface, stage: NativeAudioStage) =
            "audio_stage.${surface.wire}.${stage.wire}"
    }
}

enum class LiveVoiceEngine(val wire: String, val label: String) {
    Realtime("realtime", "Live"),
    HandsFree("hands_free", "Hands-free"),
    ;

    companion object {
        fun from(wire: String?): LiveVoiceEngine = entries.firstOrNull { it.wire == wire } ?: Realtime
    }
}

/**
 * Translation is continuous two-way audio and therefore cannot be push to
 * talk. This is a protocol rule shared by selection, call startup, and tests —
 * keeping three ad-hoc checks is how a stored setting becomes a different
 * runtime setting.
 */
internal fun resolveLivePushToTalk(
    requested: Boolean,
    engine: LiveVoiceEngine,
    profile: RealtimeVoiceProfile?,
): Boolean = requested && !(engine == LiveVoiceEngine.Realtime && profile?.mode == "translation")

/**
 * Where dictation is transcribed.
 *
 * The wire values match iOS so the same choice means the same thing on both,
 * and a backend that starts honouring a per-device preference does not have to
 * learn two vocabularies.
 */
enum class SttSource(val wire: String, val label: String) {
    Auto("auto", "Auto (this phone, backend fallback)"),
    OnDevice("on_device", "This phone · Android Speech"),
    Cloud("cloud", "Backend host"),
    ;

    /** Whether to ask the recogniser for its offline model first. */
    val prefersOnDevice: Boolean get() = this != Cloud

    /** Whether the backend may be used, as primary or as fallback. */
    val allowsCloud: Boolean get() = this != OnDevice

    companion object {
        fun from(wire: String?): SttSource =
            entries.firstOrNull { it.wire == wire } ?: Auto
    }
}

/** Which voice reads replies aloud. */
enum class TtsEngine(val wire: String, val label: String) {
    OnDevice("on_device", "This phone · Android Voice"),
    Magician("magician", "Backend host"),
    ;

    companion object {
        fun from(wire: String?): TtsEngine =
            entries.firstOrNull { it.wire == wire } ?: OnDevice
    }
}

/**
 * How long listening stays open.
 *
 * The control exists because an always-on microphone with no end is the thing
 * people rightly refuse. Every option here is an upper bound, including the
 * open-ended one: iOS caps that at eight hours because its Orb control cannot
 * outlive that, and the same cap is kept here so a phone left listening
 * overnight stops on its own rather than because the battery ran out.
 */
enum class AmbientLeash(val wire: String, val label: String, val minutes: Long, val detail: String) {
    ThirtyMinutes("30m", "30 min", 30, "Magician stops listening after 30 minutes."),
    TwoHours("2h", "2 hours", 120, "Magician stops listening after 2 hours."),
    UntilStopped(
        "until_stopped", "Until I stop", 8 * 60,
        "Magician keeps listening until you stop it — and stops after 8 hours regardless.",
    ),
    ;

    companion object {
        fun from(wire: String?): AmbientLeash = entries.firstOrNull { it.wire == wire } ?: TwoHours
    }
}

/**
 * What a wake word starts.
 *
 * Same three lanes iOS offers, with the same wire values. Availability remains
 * explicit because a disabled transport must never be selected and then
 * silently run the dictation lane instead.
 */
enum class AmbientVoiceMode(val wire: String, val label: String, val built: Boolean, val detail: String) {
    Dictation(
        "dictation", "Dictation", true,
        "A wake word records the turn, transcribes it, speaks the answer, then listens again.",
    ),
    HandsFree(
        "hands_free", "Hands-free", true,
        "Keeps the conversation open through transcription, the agent, and spoken replies.",
    ),
    Realtime(
        "realtime", "Live", true,
        "Uses the selected backend-proxied low-latency realtime provider.",
    ),
    ;

    companion object {
        fun from(wire: String?): AmbientVoiceMode = entries.firstOrNull { it.wire == wire } ?: Dictation
    }
}
