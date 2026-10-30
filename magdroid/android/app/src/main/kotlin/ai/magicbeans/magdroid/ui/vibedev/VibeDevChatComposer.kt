package ai.magicbeans.magdroid.ui.vibedev

import ai.magicbeans.magdroid.ui.*
import ai.magicbeans.magdroid.chat.ChatRepository
import ai.magicbeans.magdroid.chat.ChatUiState
import ai.magicbeans.magdroid.chat.StagedAttachment
import ai.magicbeans.magdroid.voice.*
import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.unit.dp
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import java.util.UUID
import ai.magicbeans.magdroid.vibedev.VibeDevProject

/** VibeDev adapts run transport to the actual Chat composer; it does not redraw it. */
@Composable
internal fun VibeDevChatComposer(
    prompt: String, onPrompt: (String) -> Unit, project: VibeDevProject?,
    attachments: List<StagedAttachment>, onAttachments: ((List<StagedAttachment>) -> List<StagedAttachment>) -> Unit,
    submitting: Boolean, canSend: Boolean, onSend: () -> Unit, placeholder: String, focusRequest: Int = 0,
    askPanel: @Composable () -> Unit = {},
    topPanel: @Composable () -> Unit, modePanel: @Composable () -> Unit, modeControl: @Composable () -> Unit, modelControl: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val prefs = remember { VoicePrefs.get(context) }
    val repository = remember { ChatRepository(context.applicationContext) }
    val dictation = remember { Dictation(context.applicationContext) }
    val live = remember { RealtimeVoiceSession(context.applicationContext) }
    DisposableEffect(Unit) { onDispose { dictation.close(); live.close(); repository.close() } }
    val recording by dictation.state.collectAsStateWithLifecycle()
    val dictationLevel by dictation.level.collectAsStateWithLifecycle()
    val partial by dictation.partial.collectAsStateWithLifecycle()
    val dictationError by dictation.error.collectAsStateWithLifecycle()
    // Without the ~16 Hz mic level: only the call orb and ring read it, in draw.
    val liveState by remember(live) { live.state.withoutMicLevel() }
        .collectAsStateWithLifecycle(initialValue = live.state.value.copy(micLevel = 0f))
    val liveMicLevel by remember(live) { live.state.liveMicLevel() }.collectAsStateWithLifecycle(initialValue = 0f)
    val livePlaybackLevel by remember(live) { live.state.livePlaybackLevel() }.collectAsStateWithLifecycle(initialValue = 0f)
    val composerVoiceMode = rememberComposerVoiceMode(liveState.active, focusRequest)
    val speak by prefs.speakReplies.collectAsStateWithLifecycle()
    val listening by WakeService.listening.collectAsStateWithLifecycle()
    val stt by prefs.sttSource.collectAsStateWithLifecycle()
    val tts by prefs.ttsEngine.collectAsStateWithLifecycle()
    val archive by prefs.archiveDictation.collectAsStateWithLifecycle()
    val engine by prefs.liveEngine.collectAsStateWithLifecycle()
    val voiceInputMode by prefs.composerVoiceMode.collectAsStateWithLifecycle()
    val realtimeProfile by prefs.realtimeProfile.collectAsStateWithLifecycle()
    val ptt by prefs.livePttOn.collectAsStateWithLifecycle()
    val audioProfiles by prefs.audioProfiles.collectAsStateWithLifecycle()
    val audioStages by prefs.audioStageOptions.collectAsStateWithLifecycle()
    var catalog by remember { mutableStateOf(RealtimeVoiceCatalog()) }
    var section by remember { mutableStateOf<VoiceSection?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var typingDuringCall by remember { mutableStateOf(false) }
    LaunchedEffect(liveState.active) { if (!liveState.active) typingDuringCall = false }
    val refresh = { scope.launch {
        runCatching { live.catalog() }.onSuccess { loaded ->
            catalog = loaded
            prefs.applyCatalog(loaded)
        }.onFailure { error = it.message }
    }; Unit }
    LaunchedEffect(Unit) { refresh() }
    val latestPrompt by rememberUpdatedState(prompt)
    val latestProject by rememberUpdatedState(project?.projectId)
    val dictate = { dictation.start(onTranscript = { heard -> onPrompt(if (latestPrompt.isBlank()) heard else "$latestPrompt $heard") }) }
    val toggleLive = {
        if (liveState.active || liveState.phase == RealtimeVoiceState.Phase.Failed) live.stop()
        else if (project?.chatSessionId == null) error = "This project's voice session is unavailable. Refresh and try again."
        else live.start(project.chatSessionId, prefs.liveEngine.value,
            realtimeProfile = catalog.profiles.firstOrNull { it.id == realtimeProfile },
            audioProfile = audioProfiles[NativeAudioSurface.HandsFree],
            audioStageOptions = audioStages.filterKeys { it.first == NativeAudioSurface.HandsFree }.mapKeys { it.key.second },
            pushToTalk = ptt)
        Unit
    }
    var pending by remember { mutableStateOf<(() -> Unit)?>(null) }
    val micPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { allowed ->
        val action = pending; pending = null
        if (allowed) action?.invoke() else error = "Microphone permission was not granted."
    }
    val withMic: (() -> Unit) -> Unit = { action ->
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) action()
        else { pending = action; micPermission.launch(Manifest.permission.RECORD_AUDIO) }
    }
    val pick = rememberComposerAttachments(onPicked = { name, mime, bytes ->
        val session = project?.chatSessionId
        val projectID = project?.projectId
        if (session == null) error = "This project's attachment session is unavailable. Refresh and try again."
        else {
            val item = StagedAttachment(UUID.randomUUID().toString(), name, mimeType = mime, preview = bytes.takeIf { mime.startsWith("image/") })
            onAttachments { it + item }
            scope.launch {
                val uploaded = runCatching { repository.uploadAttachment(session, name, mime, bytes) }
                if (latestProject == projectID) onAttachments { current -> current.map {
                    if (it.localId == item.localId) it.copy(remoteId = uploaded.getOrNull(), uploading = false,
                        failed = uploaded.isFailure, error = uploaded.exceptionOrNull()?.message) else it
                } }
            }
        }
    }, onError = { error = it })
    val ready = canSend && prompt.isNotBlank() && !submitting && attachments.none { it.uploading || it.failed }
    val livePanelVisible = liveState.active && !typingDuringCall
    Column(Modifier.imePadding().navigationBarsPadding().padding(bottom = 12.dp)) {
        askPanel()
        AnimatedVisibility(
            visible = livePanelVisible,
            enter = slideInVertically { it } + fadeIn(),
            exit = slideOutVertically { it } + fadeOut() + shrinkVertically(),
        ) {
            LiveVoicePanel(state = liveState, micLevel = { liveMicLevel }, playbackLevel = { livePlaybackLevel },
                onMute = live::toggleMute, onHoldStart = live::engagePushToTalk, onHoldEnd = live::releasePushToTalk,
                onSetPushToTalk = { prefs.setLivePttOn(it); live.setPushToTalk(it) },
                onEnd = live::stop, onTypeMessage = { typingDuringCall = true })
        }
        if (!livePanelVisible) Composer(voiceModeState = composerVoiceMode, dictationLevel = { dictationLevel }, voiceQueue = {
            modePanel()
            if (liveState.active) LiveCallComposerStrip(liveState, onOpen = { typingDuringCall = false }, onMute = live::toggleMute, onEnd = live::stop)
        }, state = ChatUiState(draft = prompt, attachments = attachments, dictation = recording, partialTranscript = partial,
            queueMutationInFlight = submitting || (prompt.isNotBlank() && !ready)),
            onDraftChange = onPrompt, onSend = { if (ready) onSend() }, onStop = {},
            onPickProfile = {}, onPickEngine = {}, onPickModel = {}, onAttach = pick.files, onCamera = pick.photos,
            onMic = { if (recording == DictationState.Recording) dictation.stop()
                else withMic { if (voiceInputMode == ComposerVoiceMode.Dictation) dictate() else toggleLive() } },
            onHoldStart = { withMic(dictate) }, onHoldEnd = dictation::stop,
            speakReplies = speak && !liveState.active, onToggleSpeak = { prefs.setSpeakReplies(!speak) },
            onOpenReplySettings = { section = VoiceSection.Replies }, onOpenLiveSettings = { section = VoiceSection.Live },
            liveActive = liveState.active, liveVoiceAvailable = project?.chatSessionId != null && catalog.profiles.any { it.nativeAndAvailable },
            handsFreeAvailable = project?.chatSessionId != null && catalog.handsFreeAvailable, onToggleLive = { withMic(toggleLive) },
            onRemoveAttachment = { id -> onAttachments { current -> current.filterNot { it.localId == id } } }, onComposerMode = {},
            topPanel = { topPanel(); (error ?: dictationError ?: liveState.error)?.let { Text(it, color = activePalette.danger) } },
            modeControl = modeControl, modelControl = modelControl, inputPlaceholder = placeholder, showChatGodMode = false, holdToSend = false, focusRequest = focusRequest)
    }
    section?.let { current -> VoiceSettingsSheet(current, speak, listening, stt, tts, archive, liveState,
        catalog.handsFreeAvailable, engine, catalog.profiles, realtimeProfile, ptt, catalog.audioProfiles, catalog.stages, audioProfiles, audioStages,
        onToggleSpeak = { prefs.setSpeakReplies(!speak) },
        onToggleWake = { if (listening) WakeService.stop(context) else withMic { WakeService.start(context) } },
        onPickStt = prefs::setSttSource, onPickTts = prefs::setTtsEngine, onToggleArchive = { prefs.setArchiveDictation(!archive) },
        onPickLiveEngine = prefs::setLiveEngine, onPickRealtimeProfile = prefs::setRealtimeProfile,
        onPickLivePushToTalk = { prefs.setLivePttOn(it); live.setPushToTalk(it) },
        onPickAudioProfile = prefs::setAudioProfile, onPickAudioStageOption = prefs::setAudioStageOption,
        onRefreshVoiceCatalog = refresh, onToggleLive = { withMic(toggleLive) }, onDismiss = { section = null }) }
}
