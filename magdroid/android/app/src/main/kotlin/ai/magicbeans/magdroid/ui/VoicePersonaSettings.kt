package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.MediaPreferences
import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import ai.magicbeans.magdroid.voice.RealtimeVoiceChoice
import ai.magicbeans.magdroid.voice.VoicePrefs
import android.speech.tts.TextToSpeech
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException

internal data class VoicePersonaBinding(
    val key: String,
    val realtime: Boolean,
    val label: String,
    val defaultVoice: String?,
    val choices: List<RealtimeVoiceChoice>,
    val available: Boolean,
)

internal fun voicePersonaBindings(catalog: RealtimeVoiceCatalog): List<VoicePersonaBinding> =
    catalog.stages["tts"].orEmpty().filter { it.capabilities.voices.isNotEmpty() }.map {
        VoicePersonaBinding(it.providerId, false, it.label, it.variant,
            it.capabilities.voices.map { id -> RealtimeVoiceChoice(id, id.replace('_', ' ')) }, it.available)
    } + catalog.profiles.filter { it.mode != "translation" && it.voices.isNotEmpty() }.map {
        VoicePersonaBinding(it.id, true, it.label, it.voice, it.voices, it.available)
    }

/**
 * Account-scoped voice choices for the composer's voice panels. The full
 * list of shared speaking voices is edited in the embedded web Settings.
 */
@Composable
fun VoicePersonaControls(
    showTts: Boolean = true,
    showRealtime: Boolean = true,
    realtimeProfileId: String? = null,
    ttsProviderId: String? = null,
    enabled: Boolean = true,
) {
    val context = LocalContext.current
    val repository = remember(context) { ChatRepository(context) }
    DisposableEffect(repository) { onDispose(repository::close) }
    var catalog by remember { mutableStateOf(RealtimeVoiceCatalog()) }
    var preferences by remember { mutableStateOf(MediaPreferences()) }
    var loading by remember { mutableStateOf(true) }
    var saving by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var refresh by remember { mutableStateOf(0) }
    LaunchedEffect(repository, refresh) {
        loading = true
        error = null
        try {
            val loadedCatalog = repository.voicePersonaCatalog()
            val loadedPreferences = repository.mediaPreferences()
                ?: error("Could not load voice preferences. Check your connection and retry.")
            catalog = loadedCatalog
            preferences = loadedPreferences
            error = null
        } catch (cancel: CancellationException) { throw cancel }
        catch (_: Exception) { error = "Could not load voices. Check your connection and retry." }
        finally { loading = false }
    }
    fun selection(binding: VoicePersonaBinding): String =
        (if (binding.realtime) preferences.realtimeVoices else preferences.ttsVoices)[binding.key].orEmpty()
    val bindings = voicePersonaBindings(catalog).filter {
        if (it.realtime) showRealtime && (realtimeProfileId == null || it.key == realtimeProfileId) else showTts && (ttsProviderId == null || it.key == ttsProviderId)
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (ttsProviderId == null) {
            Text("Speaking voices", color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
            Text("Shared across your devices. Live voices apply to the next call.", color = Muted, fontSize = 11.sp)
        }
        if (loading) CircularProgressIndicator()
        bindings.forEach { binding ->
            VoiceChoicePicker(if (ttsProviderId == null) binding.label else "Speaking voice", selection(binding),
                listOf(VoiceChoice("", "Provider default", binding.defaultVoice)) + binding.choices.map { VoiceChoice(it.id, it.label) },
                enabled = enabled && !saving && !loading && binding.available) { voice ->
                saving = true
                try {
                    preferences = repository.putVoicePreference(binding.realtime, binding.key, voice)
                    error = null
                } finally { saving = false }
            }
        }
        error?.let { Text(it, color = Danger, fontSize = 12.sp) }
        if (!loading && error != null) TextButton(onClick = { refresh++ }) { Text("Retry") }
        if (!loading && bindings.isEmpty() && error == null) Text(if (ttsProviderId == null) "No selectable voices are available for these models." else "Uses the provider’s default voice. No voice choices are listed by this server.", color = Muted, fontSize = 12.sp)
    }
}

@Composable
fun DeviceSpeechVoicePicker() {
    val context = LocalContext.current
    val prefs = remember(context) { VoicePrefs.get(context) }
    val selected by prefs.deviceVoice.collectAsState()
    var voices by remember { mutableStateOf<List<android.speech.tts.Voice>>(emptyList()) }
    DisposableEffect(context) {
        var disposed = false
        var engine: TextToSpeech? = null
        engine = TextToSpeech(context.applicationContext) { status ->
            if (!disposed && status == TextToSpeech.SUCCESS) {
                voices = engine?.voices.orEmpty().sortedBy { it.name }
            }
        }
        onDispose { disposed = true; engine?.shutdown() }
    }
    VoiceChoicePicker("This phone · speaking voice", selected,
        listOf(VoiceChoice("", "System default")) + voices.map { voice ->
            VoiceChoice(voice.name, voice.name, "${voice.locale.toLanguageTag()}${if (voice.isNetworkConnectionRequired) " · online" else ""}")
        }) { voice -> prefs.setDeviceVoice(voice) }
}
