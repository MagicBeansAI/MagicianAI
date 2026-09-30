package ai.magicbeans.magdroid.voice

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

enum class NativeAudioSurface(val wire: String, val label: String) {
    Dictation("dictation", "Dictation"),
    HandsFree("hands_free", "Hands-free"),
}

enum class NativeAudioStage(val wire: String, val label: String) {
    Vad("vad", "Voice activity"),
    RecordingStt("recording_stt", "Transcription"),
    StreamingStt("streaming_stt", "Live transcription"),
    Diarization("diarization", "Speakers"),
    Tts("tts", "Voice"),
}

@Serializable
data class AudioStageProfile(
    val enabled: Boolean = false,
    val providers: List<String> = emptyList(),
)

@Serializable
data class AudioSurfaceProfile(
    val surface: String,
    @SerialName("turn_boundary") val turnBoundary: String? = null,
    val vad: AudioStageProfile = AudioStageProfile(),
    @SerialName("recording_stt") val recordingStt: AudioStageProfile = AudioStageProfile(),
    @SerialName("streaming_stt") val streamingStt: AudioStageProfile = AudioStageProfile(),
    val diarization: AudioStageProfile = AudioStageProfile(),
    val tts: AudioStageProfile = AudioStageProfile(),
) {
    fun stage(value: NativeAudioStage): AudioStageProfile = when (value) {
        NativeAudioStage.Vad -> vad
        NativeAudioStage.RecordingStt -> recordingStt
        NativeAudioStage.StreamingStt -> streamingStt
        NativeAudioStage.Diarization -> diarization
        NativeAudioStage.Tts -> tts
    }
}

@Serializable
data class AudioStageOption(
    @SerialName("option_id") val id: String,
    val stage: String,
    @SerialName("provider_id") val providerId: String,
    @SerialName("engine_id") val engineId: String,
    @SerialName("model_id") val modelId: String,
    val label: String,
    val availability: String,
    @SerialName("unavailable_reason") val unavailableReason: String? = null,
) {
    val available: Boolean get() = availability == "available"
    val displayLabel: String get() = when (engineId.lowercase()) {
        "macos_system", "fluid_audio" -> "Mac host · $label"
        "online" -> "Online · $label"
        else -> "Backend host · $label"
    }
}

fun audioProfileLabel(id: String): String = id
    .removePrefix("compat-")
    .replace(Regex("-v[0-9]+$"), "")
    .split('-', '_')
    .filter(String::isNotBlank)
    .joinToString(" ") { word -> word.replaceFirstChar(Char::uppercase) }

fun AudioSurfaceProfile.supports(option: AudioStageOption, stage: NativeAudioStage): Boolean {
    val configured = stage(stage).providers
    return stage(stage).enabled && configured.any {
        it.equals(option.providerId, ignoreCase = true) || it.equals(option.id, ignoreCase = true)
    }
}

/**
 * A Hands-free pipeline is usable only when every stage needed to keep the
 * conversation open has an available configured implementation. Dictation can
 * use its ordered fallbacks, matching the admission rule used by iOS.
 */
fun AudioSurfaceProfile.usable(
    requestedSurface: NativeAudioSurface,
    options: Map<String, List<AudioStageOption>>,
): Boolean {
    if (surface != requestedSurface.wire) return false
    if (requestedSurface == NativeAudioSurface.Dictation) return true
    return listOf(
        NativeAudioStage.Vad,
        NativeAudioStage.StreamingStt,
        NativeAudioStage.Tts,
    ).all { stage ->
        stage(stage).enabled && options[stage.wire].orEmpty().any {
            it.available && supports(it, stage)
        }
    }
}

fun RealtimeVoiceCatalog.audioProfileAvailable(surface: NativeAudioSurface, id: String): Boolean =
    audioProfiles[id]?.usable(surface, stages) == true
