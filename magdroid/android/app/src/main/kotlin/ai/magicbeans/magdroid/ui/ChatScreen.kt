package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.OriginalAnswerLink
import ai.magicbeans.magdroid.chat.ChatMessage
import ai.magicbeans.magdroid.chat.ChatComposerMode
import ai.magicbeans.magdroid.chat.ChatDoPermission
import ai.magicbeans.magdroid.chat.ChatProfile
import ai.magicbeans.magdroid.chat.ChatUiState
import ai.magicbeans.magdroid.chat.ChatViewModel
import ai.magicbeans.magdroid.chat.ActivityRow
import ai.magicbeans.magdroid.chat.CompleteResultViewerState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.border
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import ai.magicbeans.magdroid.chat.MentionCatalog
import ai.magicbeans.magdroid.chat.MentionItem
import ai.magicbeans.magdroid.chat.ContentBlockRecord
import ai.magicbeans.magdroid.voice.DictationState
import ai.magicbeans.magdroid.voice.VoicePrefs
import ai.magicbeans.magdroid.voice.PrimaryAgentWakeIdentityStore
import ai.magicbeans.magdroid.identity.ProductIdentity
import ai.magicbeans.magdroid.voice.WakeService
import ai.magicbeans.magdroid.voice.LiveVoiceEngine
import ai.magicbeans.magdroid.voice.RealtimeVoiceState
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.EscalationCard
import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.MessageKind
import ai.magicbeans.magdroid.chat.TaskStatusCard
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.PictureAsPdf
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.UnfoldMore
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material.icons.outlined.Screenshot
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.automirrored.outlined.VolumeOff
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import kotlinx.coroutines.launch
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

// Ambient Aurora, matching the web UI's tokens rather than Material defaults.
/**
 * The palette, by the names the screens already use.
 *
 * Every one resolves through the active theme rather than a literal, so eleven
 * families and their day/night variants reach the whole app without a single
 * call site changing.
 *
 * Backed by snapshot state, not a composition local. A local would force every
 * one of these into a `@Composable` context, and colours are legitimately read
 * from plain helper functions — a rule that says otherwise breaks working code
 * to serve the mechanism rather than the app. Snapshot state reads the same
 * either way, and still subscribes for recomposition when read inside one.
 */
internal val Ground: Color get() = activePalette.background
internal val Panel: Color get() = activePalette.elevated
internal val Control: Color get() = activePalette.control
internal val ControlBorder: Color get() = activePalette.controlBorder
internal val Soft: Color get() = activePalette.soft
internal val Coral: Color get() = activePalette.accent
internal val OnAccent: Color get() = activePalette.onAccent
internal val Danger: Color get() = activePalette.danger
internal val Teal: Color get() = activePalette.info
internal val ChatSuccess: Color get() = if (activePalette.isDark) Color(0xFF68D391) else Color(0xFF16784A)
internal val ChatDiscovery: Color get() = activePalette.discovery
internal val Ink: Color get() = activePalette.text
internal val MWarn: Color get() = activePalette.warning
internal val Muted: Color get() = activePalette.secondaryText
internal val Secondary: Color get() = activePalette.secondaryText
internal val BorderSoft: Color get() = activePalette.cardBorder

internal const val CHAT_BUBBLE_FONT_SP = 13
internal const val CHAT_BUBBLE_LINE_HEIGHT_SP = 18
internal const val CHAT_BUBBLE_CONTENT_PADDING_DP = 11
internal const val CHAT_BUBBLE_CORNER_DP = 12
internal const val MESSAGE_SPEAK_LEADING_PADDING_DP = 0
internal const val MESSAGE_SPEAK_FONT_SP = 10
internal const val MESSAGE_SPEAK_VERTICAL_PADDING_DP = 3
internal const val COMPOSER_CORNER_DP = 12
internal const val COMPOSER_MODE_FONT_SP = 9
internal const val COMPOSER_MODE_MIN_HEIGHT_DP = 24
internal const val COMPOSER_MODE_HORIZONTAL_PADDING_DP = 7
internal const val COMPOSER_MODE_VERTICAL_PADDING_DP = 1
internal const val COMPOSER_TOOL_BUTTON_DP = 30
internal const val COMPOSER_TOOL_ICON_DP = 16
internal const val COMPOSER_PROFILE_VERTICAL_PADDING_DP = 2
internal const val COMPOSER_PROFILE_TAG_CORNER_DP = 4
internal const val COMPOSER_PROFILE_TAG_FONT_SP = 8
internal const val COMPOSER_PROFILE_TAG_HORIZONTAL_PADDING_DP = 4
internal const val COMPOSER_PROFILE_TAG_VERTICAL_PADDING_DP = 1
internal const val COMPOSER_VOICE_DOCK_HEIGHT_DP = 32
internal const val COMPOSER_VOICE_DOCK_PRIMARY_WIDTH_DP = 31
internal const val COMPOSER_VOICE_DOCK_CHEVRON_WIDTH_DP = 22

/** The owner uses the accent; the assistant uses the theme's tinted surface. */
internal fun chatBubbleColor(fromUser: Boolean): Color = if (fromUser) Coral else Control

/** User and assistant messages share one clean, tail-free silhouette. */
internal fun chatBubbleShape(): Shape = RoundedCornerShape(CHAT_BUBBLE_CORNER_DP.dp)

/** The iOS composer sits on the same theme surface as assistant replies. */
internal fun composerSurfaceColor(): Color = Control

internal fun composerModeTrackColor(): Color = Soft

internal fun composerModeForeground(mode: ai.magicbeans.magdroid.chat.ChatComposerMode): Color =
    if (mode == ai.magicbeans.magdroid.chat.ChatComposerMode.Ask) {
        if (activePalette.isDark) Ink else Ground
    } else OnAccent

internal fun composerModeFill(mode: ai.magicbeans.magdroid.chat.ChatComposerMode): Color =
    if (mode == ai.magicbeans.magdroid.chat.ChatComposerMode.Ask) {
        // Text is light in Night mode; using it as the fill creates a glaring
        // white button. Keep selection within the composer's dark surface.
        if (activePalette.isDark) Coral.copy(alpha = 0.18f) else Ink
    } else Coral

/** Mirrors iOS for solid semantic fills such as danger, which has no on-color token. */
internal fun chatContrastingTextColor(color: Color): Color {
    val dark = Color(0xFF111111)
    val luminance = color.luminance()
    val lightContrast = 1.05f / (luminance + 0.05f)
    val darkContrast = (luminance + 0.05f) / (dark.luminance() + 0.05f)
    return if (darkContrast > lightContrast) dark else Color.White
}

/** Profile tier badges use the same semantic palette as iOS. */
internal fun chatProfileTierColor(tier: String?): Color = when (tier?.lowercase()) {
    "instant" -> ChatSuccess
    "normal" -> Teal
    "advanced" -> ChatDiscovery
    else -> Secondary
}

/** Voice origin belongs to the owner's turn, so it follows that bubble's accent. */
internal fun voiceOriginBadgeColor(): Color = chatBubbleColor(fromUser = true)

private enum class MicPermissionAction { Dictation, WakeWord, Live, None }

@Composable
fun ChatScreen(
    viewModel: ChatViewModel = viewModel(),
    onOpenSettings: (() -> Unit)? = null,
    autoStartVoice: Boolean = false,
    onAutoStartVoiceConsumed: () -> Unit = {},
    onOpenAttention: (String?) -> Unit = {},
    onOpenTask: (String) -> Unit = {},
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()
    val context = LocalContext.current
    val voiceLifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current
    DisposableEffect(voiceLifecycle, viewModel) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_RESUME) viewModel.setVoiceScreenActive(true)
            if (event == androidx.lifecycle.Lifecycle.Event.ON_PAUSE) viewModel.setVoiceScreenActive(false)
        }
        voiceLifecycle.lifecycle.addObserver(observer)
        viewModel.setVoiceScreenActive(voiceLifecycle.lifecycle.currentState.isAtLeast(androidx.lifecycle.Lifecycle.State.RESUMED))
        onDispose { voiceLifecycle.lifecycle.removeObserver(observer); viewModel.setVoiceScreenActive(false) }
    }
    val voicePrefs = remember(context) { VoicePrefs.get(context) }

    // Two paths, as iOS has: any file, and an image. A picked file is read to
    // bytes here — the URI is a grant made to this process for this pick, and
    // holding it to upload later is how an attachment becomes unreadable.
    val stage: (android.net.Uri?) -> Unit = { uri ->
        uri?.let {
            readPicked(context, it)?.let { (name, mime, bytes) ->
                viewModel.stageAttachment(name, mime, bytes)
            }
        }
    }
    val pickDocument = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocument(), stage,
    )
    val pickImage = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent(), stage,
    )

    // Asked when the mic is first pressed, not at launch: a permission prompt
    // before anyone has tried to speak is a prompt with no context to judge it
    // by, and is refused more often for it.
    val micGranted = rememberMicPermission()
    var pendingMicAction by remember { mutableStateOf(MicPermissionAction.Dictation) }
    val requestMic = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) when (pendingMicAction) {
            MicPermissionAction.Dictation -> viewModel.startDictation()
            MicPermissionAction.WakeWord -> WakeService.start(context)
            MicPermissionAction.Live -> viewModel.startConfiguredVoice()
            MicPermissionAction.None -> Unit
        }
        pendingMicAction = MicPermissionAction.Dictation
    }
    // A hold that starts without permission asks for it and stops there; the
    // press is over by the time an answer arrives, so starting then would
    // record with nobody holding the button.
    val onHoldStart: () -> Unit = {
        if (micGranted.value) viewModel.startDictation()
        else {
            pendingMicAction = MicPermissionAction.None
            requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
        }
    }
    val onHoldEnd: () -> Unit = { viewModel.stopDictation() }

    // Always-on listening, toggled from the composer's options control. Started
    // from this screen because Android refuses a microphone service started
    // from the background — a restriction worth keeping, not routing around.
    val listening by WakeService.listening.collectAsStateWithLifecycle()
    val speakReplies by voicePrefs.speakReplies.collectAsStateWithLifecycle()
    val onToggleSpeak: () -> Unit = {
        if (speakReplies) viewModel.stopSpeaking()
        voicePrefs.setSpeakReplies(!speakReplies)
        // Published so the account's other devices agree. Muting on one phone
        // and being talked at by another is the whole reason this syncs.
        viewModel.publishSpeakReplies(!speakReplies)
    }
    // Which section the sheet opens on. iOS routes each chevron to its own,
    // because the settings behind the reply toggle and behind Live have
    // nothing to do with each other.
    var voiceSection by remember { mutableStateOf<VoiceSection?>(null) }
    val sttSource by voicePrefs.sttSource.collectAsStateWithLifecycle()
    val ttsEngine by voicePrefs.ttsEngine.collectAsStateWithLifecycle()
    val archiveDictation by voicePrefs.archiveDictation.collectAsStateWithLifecycle()
    val realtimeState by viewModel.realtimeVoiceState.collectAsStateWithLifecycle()
    val voiceCatalog by viewModel.voiceCatalog.collectAsStateWithLifecycle()
    val liveEngine by voicePrefs.liveEngine.collectAsStateWithLifecycle()
    val realtimeProfile by voicePrefs.realtimeProfile.collectAsStateWithLifecycle()
    val livePushToTalk by voicePrefs.livePttOn.collectAsStateWithLifecycle()
    val audioProfiles by voicePrefs.audioProfiles.collectAsStateWithLifecycle()
    val audioStageOptions by voicePrefs.audioStageOptions.collectAsStateWithLifecycle()
    val onToggleWake: () -> Unit = {
        when {
            listening -> WakeService.stop(context)
            micGranted.value -> WakeService.start(context)
            else -> {
                pendingMicAction = MicPermissionAction.WakeWord
                requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
            }
        }
    }

    // The wake word is handled by the service, not here. This effect used to
    // start dictation, and `collectAsStateWithLifecycle` stops collecting below
    // STARTED — so a phone that was listening while locked heard the wake and
    // did nothing with it, which is worse than not listening at all.
    val onMic: () -> Unit = {
        when {
            state.dictation == DictationState.Recording -> viewModel.stopDictation()
            micGranted.value -> viewModel.startDictation()
            else -> {
                pendingMicAction = MicPermissionAction.Dictation
                requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
            }
        }
    }
    val liveActive = realtimeState.active
    var typingDuringCall by remember { mutableStateOf(false) }
    LaunchedEffect(liveActive) { if (!liveActive) typingDuringCall = false }
    val onToggleLive: () -> Unit = {
        when {
            realtimeState.active || realtimeState.phase == ai.magicbeans.magdroid.voice.RealtimeVoiceState.Phase.Failed ->
                viewModel.stopRealtimeVoice()
            micGranted.value -> viewModel.startConfiguredVoice()
            else -> {
                pendingMicAction = MicPermissionAction.Live
                requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
            }
        }
    }

    LaunchedEffect(autoStartVoice) {
        if (!autoStartVoice) return@LaunchedEffect
        onAutoStartVoiceConsumed()
        if (micGranted.value) {
            viewModel.startConfiguredVoice()
        } else {
            pendingMicAction = MicPermissionAction.Live
            requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
        }
    }

    // Follow the stream. Text tokens and canonical activity rows grow the same
    // response bubble independently, so both must keep a reader who is already
    // at the bottom on the live edge. Without the activity key, tool-heavy
    // turns grew below the viewport until the final text token arrived.
    // At the bottom means within a message of the end. Following the stream is
    // right until the owner scrolls back to read something — yanking them
    // forward mid-sentence is the thing that makes a transcript unusable.
    val atBottom by remember {
        derivedStateOf {
            val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            last >= (state.messages.lastIndex - 1).coerceAtLeast(0)
        }
    }
    LaunchedEffect(
        state.messages.size,
        state.messages.lastOrNull()?.text?.length,
        state.messages.lastOrNull()?.activityRows,
    ) {
        if (state.messages.isNotEmpty() && atBottom && state.focusedMessageId == null) {
            listState.animateScrollToItem(state.messages.lastIndex)
        }
    }
    LaunchedEffect(state.focusedMessageId, state.messages.size) {
        val index = state.messages.indexOfFirst { it.id == state.focusedMessageId }
        if (index >= 0) listState.scrollToItem(index)
    }
    val scope = rememberCoroutineScope()

    Column(Modifier.fillMaxSize().background(Ground)) {
        Box(Modifier.weight(1f)) {
            Transcript(
                state, listState, viewModel::answerEscalation, Modifier.fillMaxSize(),
                onToggleSpeak = viewModel::toggleMessageSpeech,
                onRetry = viewModel::start,
                onOpenSettings = onOpenSettings,
                onOpenAttention = onOpenAttention,
                onOpenCompleteResult = viewModel::openCompleteResult,
                onOpenTask = onOpenTask,
                onLoadActivity = viewModel::loadActivityIfNeeded,
                onOpenOriginalAnswer = viewModel::openOriginalAnswer,
            )
            // Jump pill, as iOS has: only while scrolled away, and only when
            // there is something below to jump to.
            if (!atBottom && state.messages.isNotEmpty()) {
                Surface(
                    color = Coral,
                    shape = CircleShape,
                    shadowElevation = 6.dp,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .padding(end = 14.dp, bottom = 12.dp)
                        .size(42.dp)
                        .clickable {
                            viewModel.clearMessageFocus()
                            scope.launch { listState.animateScrollToItem(state.messages.lastIndex) }
                        },
                ) {
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        Text("↓", color = OnAccent, fontSize = 17.sp, fontWeight = FontWeight.Bold)
                    }
                }
            }
        }
        if (state.mentions.isNotEmpty()) {
            MentionPicker(state.mentions, viewModel::pickMention)
        }
        // A dictated draft sends itself in three seconds. Visible and
        // cancelable, because a transcript is often nearly right and the moment
        // to fix it is before it goes.
        state.autoSendIn?.let { seconds ->
            Row(
                Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 12.dp, vertical = 6.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(
                    Icons.Outlined.GraphicEq, contentDescription = null,
                    tint = Coral, modifier = Modifier.size(14.dp),
                )
                Text("Sending in ${seconds}s…", color = Ink, fontSize = 13.sp, fontWeight = FontWeight.Medium)
                Spacer(Modifier.weight(1f))
                Text(
                    "Cancel",
                    color = Coral, fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { viewModel.cancelAutoSend() },
                )
            }
        }

        state.completeResult?.let { result ->
            CompleteResultSheet(result, viewModel::closeCompleteResult)
        }

        voiceSection?.let { section ->
            VoiceSettingsSheet(
                initialSection = section,
                speakReplies = speakReplies,
                listening = listening,
                sttSource = sttSource,
                ttsEngine = ttsEngine,
                archiveDictation = archiveDictation,
                realtimeState = realtimeState,
                handsFreeAvailable = voiceCatalog.handsFreeAvailable,
                liveEngine = liveEngine,
                realtimeProfiles = voiceCatalog.profiles,
                selectedRealtimeProfile = realtimeProfile,
                livePushToTalk = livePushToTalk,
                audioProfileCatalog = voiceCatalog.audioProfiles,
                audioStageCatalog = voiceCatalog.stages,
                selectedAudioProfiles = audioProfiles,
                selectedAudioStageOptions = audioStageOptions,
                onToggleSpeak = onToggleSpeak,
                onToggleWake = onToggleWake,
                onPickStt = voicePrefs::setSttSource,
                onPickTts = voicePrefs::setTtsEngine,
                onToggleArchive = { voicePrefs.setArchiveDictation(!archiveDictation) },
                onPickLiveEngine = voicePrefs::setLiveEngine,
                onPickRealtimeProfile = viewModel::selectRealtimeVoiceProfile,
                onPickLivePushToTalk = viewModel::setLiveVoicePushToTalk,
                onPickAudioProfile = voicePrefs::setAudioProfile,
                onPickAudioStageOption = voicePrefs::setAudioStageOption,
                onRefreshVoiceCatalog = viewModel::refreshVoiceCatalog,
                onToggleLive = onToggleLive,
                onDismiss = { voiceSection = null },
            )
        }
        if (realtimeState.active && !typingDuringCall) {
            LiveVoicePanel(
                state = realtimeState,
                voiceQueue = { ChatComposerTopRows(state, viewModel, topCornerRadius = 20.dp) },
                onMute = viewModel::toggleLiveVoiceMute,
                onHoldStart = viewModel::engageLiveVoicePushToTalk,
                onHoldEnd = viewModel::releaseLiveVoicePushToTalk,
                onSetPushToTalk = viewModel::setLiveVoicePushToTalk,
                onSettings = { voiceSection = VoiceSection.Live },
                onEnd = onToggleLive,
                onTypeMessage = { typingDuringCall = true },
            )
        } else {
            Composer(
                state = state,
                voiceQueue = {
                    ChatComposerTopRows(state, viewModel)
                    if (liveActive) LiveCallComposerStrip(realtimeState,
                        onOpen = { typingDuringCall = false }, onMute = viewModel::toggleLiveVoiceMute, onEnd = onToggleLive)
                },
                onBackground = viewModel::sendInBackground,
                onStopAndSend = viewModel::stopAndSend,
                onDraftChange = viewModel::onDraftChange,
                onSend = { viewModel.send() },
                onStop = viewModel::stop,
                onPickProfile = viewModel::selectProfile,
                onPickEngine = viewModel::selectHarnessEngine,
                onPickModel = viewModel::selectHarnessModel,
                onAttach = { pickDocument.launch(arrayOf("*/*")) },
                onCamera = { pickImage.launch("image/*") },
                // Offered only when the service that reads the screen is
                // running. Absent rather than disabled: a control that explains
                // why it cannot work is worse than one that was never promised.
                onTeachScreen = if (ai.magicbeans.magdroid.tutor.TutorScreenGrab.available()) {
                    { viewModel.teachThisScreen() }
                } else {
                    null
                },
                onMic = onMic,
                onHoldStart = onHoldStart,
                onHoldEnd = onHoldEnd,
                speakReplies = speakReplies,
                onToggleSpeak = onToggleSpeak,
                onOpenReplySettings = { voiceSection = VoiceSection.Replies },
                onOpenLiveSettings = { voiceSection = VoiceSection.Live },
                liveActive = liveActive,
                onToggleLive = onToggleLive,
                onRemoveAttachment = viewModel::removeAttachment,
                onComposerMode = viewModel::setComposerMode,
            )
        }
    }
}

/** Keep queue, background work and provenance inside the same composer surface. */
@Composable
private fun ChatComposerTopRows(state: ChatUiState, viewModel: ChatViewModel, topCornerRadius: androidx.compose.ui.unit.Dp = COMPOSER_CORNER_DP.dp) {
    val background by viewModel.concurrentVoice.state.collectAsStateWithLifecycle()
    val hasQueue = state.queueSessionId == state.activeSessionId && state.queuedMessages.isNotEmpty()
    val metadata = state.selectedSession?.takeIf { it.identifier() == state.activeSessionId }
        ?: state.sessions.firstOrNull { it.identifier() == state.activeSessionId }
    ChatQueueStrip(state, viewModel::actOnQueue, topCornerRadius)
    ConcurrentVoiceStrip(viewModel, topCornerRadius = if (hasQueue) 0.dp else topCornerRadius)
    metadata?.internalVoice?.parentSessionId?.let { parent ->
        ConcurrentOriginStrip(
            topCornerRadius = if (hasQueue || background.available.isNotEmpty()) 0.dp else topCornerRadius,
            onOpenParent = { viewModel.openSession(parent) },
        )
    }
}

/**
 * The full call surface can collapse into the text composer without ending
 * the call. Dictation remains unavailable while realtime owns the microphone.
 */
@Composable
private fun LiveVoicePanel(
    voiceQueue: @Composable () -> Unit = {},
    state: RealtimeVoiceState,
    onMute: () -> Unit,
    onHoldStart: () -> Unit,
    onHoldEnd: () -> Unit,
    onSetPushToTalk: (Boolean) -> Unit,
    onSettings: () -> Unit,
    onEnd: () -> Unit,
    onTypeMessage: () -> Unit,
) {
    Surface(
        color = composerSurfaceColor(),
        shape = RoundedCornerShape(20.dp),
        border = BorderStroke(1.dp, Coral.copy(alpha = 0.35f)),
        modifier = Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        Column(Modifier.fillMaxWidth()) {
            voiceQueue()
            Column(
                Modifier.fillMaxWidth().padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(11.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Box(
                        Modifier.size(9.dp).background(
                            if (state.phase == RealtimeVoiceState.Phase.Ready) Teal else Coral,
                            CircleShape,
                        ),
                    )
                    Text(
                        when (state.phase) {
                            RealtimeVoiceState.Phase.Connecting -> "Connecting voice…"
                            RealtimeVoiceState.Phase.Reconnecting -> "Reconnecting voice…"
                            RealtimeVoiceState.Phase.Ready -> state.profileLabel ?: state.engine.label
                            RealtimeVoiceState.Phase.Ending -> "Ending call…"
                            else -> state.engine.label
                        },
                        color = Ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f),
                    )
                    IconButton(onClick = onTypeMessage) {
                        Icon(Icons.Outlined.Keyboard, "Type a message", tint = Secondary, modifier = Modifier.size(19.dp))
                    }
                    IconButton(onClick = onSettings, enabled = state.phase != RealtimeVoiceState.Phase.Ending) {
                        Icon(Icons.Outlined.Tune, "Voice call settings", tint = Secondary, modifier = Modifier.size(19.dp))
                    }
                }

                if (state.lastUserText.isNotBlank()) {
                    CaptionLine("You", state.lastUserText, Coral)
                }
                if (state.lastAssistantText.isNotBlank()) {
                    CaptionLine("Magican", state.lastAssistantText, Teal)
                }
                // Between "let me check…" and the answer the line is silent; say so
                // rather than inviting the next question.
                val statusLine = when {
                    state.assistantWorking -> "Working…"
                    state.lastUserText.isNotBlank() || state.lastAssistantText.isNotBlank() -> null
                    state.phase == RealtimeVoiceState.Phase.Ready -> "Listening…"
                    else -> "Preparing microphone and voice…"
                }
                statusLine?.let { Text(it, color = Muted, fontSize = 12.sp) }
                state.error?.let { Text(it, color = Danger, fontSize = 11.sp, lineHeight = 15.sp) }

                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // The mode is changeable while the call runs. It was fixed at
                    // connect time, so a room that stopped being quiet meant ending
                    // the call and going to Settings.
                    TextButton(onClick = { onSetPushToTalk(!state.pushToTalk) }) {
                        Text(
                            if (state.pushToTalk) "Open mic" else "Hold to talk",
                            color = Coral, fontSize = 12.sp,
                        )
                    }
                    if (state.pushToTalk) {
                        val pushFill = if (state.pushToTalkHeld) Danger else Coral
                        val pushForeground = if (state.pushToTalkHeld) {
                            chatContrastingTextColor(pushFill)
                        } else {
                            OnAccent
                        }
                        Surface(
                            color = pushFill,
                            shape = RoundedCornerShape(12.dp),
                            modifier = Modifier.weight(1f).height(48.dp)
                                .alpha(if (state.phase == RealtimeVoiceState.Phase.Ready) 1f else 0.5f)
                                .pointerInput(state.phase) {
                                    if (state.phase != RealtimeVoiceState.Phase.Ready) return@pointerInput
                                    detectTapGestures(onPress = {
                                        onHoldStart()
                                        tryAwaitRelease()
                                        onHoldEnd()
                                    })
                                },
                        ) {
                            Row(
                                Modifier.fillMaxSize(),
                                horizontalArrangement = Arrangement.spacedBy(7.dp, Alignment.CenterHorizontally),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Icon(Icons.Filled.Mic, null, tint = pushForeground, modifier = Modifier.size(19.dp))
                                Text(
                                    if (state.pushToTalkHeld) "Release to send" else "Hold to talk",
                                    color = pushForeground, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                                )
                            }
                        }
                    } else {
                        val muteForeground = if (state.muted) Ground else OnAccent
                        Surface(
                            color = if (state.muted) Muted else Coral,
                            shape = RoundedCornerShape(12.dp),
                            modifier = Modifier.weight(1f).height(48.dp)
                                .alpha(if (state.phase == RealtimeVoiceState.Phase.Ready) 1f else 0.5f)
                                .clickable(enabled = state.phase == RealtimeVoiceState.Phase.Ready) { onMute() },
                        ) {
                            Row(
                                Modifier.fillMaxSize(),
                                horizontalArrangement = Arrangement.spacedBy(7.dp, Alignment.CenterHorizontally),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Icon(
                                    if (state.muted) Icons.AutoMirrored.Outlined.VolumeOff else Icons.Filled.Mic,
                                    null, tint = muteForeground, modifier = Modifier.size(19.dp),
                                )
                                Text(
                                    if (state.muted) "Unmute" else "Mute mic",
                                    color = muteForeground, fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
                                )
                            }
                        }
                    }
                    Surface(
                        color = Danger,
                        shape = RoundedCornerShape(12.dp),
                        modifier = Modifier.height(48.dp).clickable { onEnd() },
                    ) {
                        Row(
                            Modifier.padding(horizontal = 15.dp),
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            val dangerForeground = chatContrastingTextColor(Danger)
                            Icon(Icons.Filled.Stop, null, tint = dangerForeground, modifier = Modifier.size(17.dp))
                            Text("End", color = dangerForeground, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                        }
                    }
                }
            }
        }
    }
}

/** Keep call status and immediate mute/end controls reachable beside typed chat. */
@Composable
private fun LiveCallComposerStrip(state: RealtimeVoiceState, onOpen: () -> Unit, onMute: () -> Unit, onEnd: () -> Unit) {
    val label = when (state.phase) {
        RealtimeVoiceState.Phase.Ready -> "Live · ${if (state.pushToTalk) "Hold to talk" else if (state.muted) "Muted" else "Mic on"}"
        RealtimeVoiceState.Phase.Connecting -> "Connecting…"
        RealtimeVoiceState.Phase.Reconnecting -> "Reconnecting…"
        RealtimeVoiceState.Phase.Ending -> "Ending call…"
        else -> "Call ended"
    }
    Column {
        Row(Modifier.fillMaxWidth().height(36.dp).padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(label,
                color = Secondary, fontSize = 12.sp, maxLines = 1,
                modifier = Modifier.weight(1f).fillMaxHeight().wrapContentHeight(Alignment.CenterVertically)
                    .clickable(role = Role.Button, onClickLabel = "Open voice call controls", onClick = onOpen))
            IconButton(onClick = if (state.pushToTalk) onOpen else onMute, modifier = Modifier.size(36.dp)) {
                Icon(if (state.muted || state.pushToTalk) Icons.AutoMirrored.Outlined.VolumeOff else Icons.Filled.Mic,
                    if (state.pushToTalk) "Open voice call controls" else if (state.muted) "Unmute microphone" else "Mute microphone",
                    tint = Secondary, modifier = Modifier.size(17.dp))
            }
            IconButton(onClick = onEnd, modifier = Modifier.size(36.dp)) {
                Icon(Icons.Filled.Stop, "End voice call", tint = Danger, modifier = Modifier.size(16.dp))
            }
        }
        HorizontalDivider(thickness = 0.5.dp, color = Muted.copy(alpha = 0.2f))
    }
}

@Composable
private fun CaptionLine(speaker: String, text: String, color: Color) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(speaker.uppercase(), color = color, fontSize = 9.sp, fontWeight = FontWeight.Bold, letterSpacing = .5.sp)
        Text(text, color = Ink, fontSize = 13.sp, lineHeight = 17.sp, maxLines = 3, overflow = TextOverflow.Ellipsis)
    }
}

/**
 * Read a picked file, and name it as the picker names it.
 *
 * The whole file is read into memory deliberately: the endpoint caps uploads at
 * 20 MB, so streaming would add machinery for a size that already fits, and a
 * `content://` URI has no path to stream from without it.
 *
 * Null when the provider cannot open the URI — a file on a disconnected share,
 * or a grant that has already lapsed.
 */
private fun readPicked(
    context: android.content.Context,
    uri: android.net.Uri,
): Triple<String, String, ByteArray>? {
    val resolver = context.contentResolver
    val mime = resolver.getType(uri) ?: "application/octet-stream"
    val name = resolver.query(uri, null, null, null, null)?.use { cursor ->
        val column = cursor.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
        if (column >= 0 && cursor.moveToFirst()) cursor.getString(column) else null
    } ?: uri.lastPathSegment ?: "attachment"
    val bytes = runCatching {
        resolver.openInputStream(uri)?.use { it.readBytes() }
    }.getOrNull() ?: return null
    return Triple(name, mime, bytes)
}

/** One honest caption for every state of the tap-or-hold dictation control. */
internal fun dictationHint(state: DictationState, holding: Boolean): String = when {
    state == DictationState.Recording && holding -> "Listening — release to send"
    state == DictationState.Recording -> "Listening — tap to finish"
    state == DictationState.Transcribing -> "Working on it"
    else -> "Tap or hold to talk"
}

/** Re-read microphone permission on resume to notice grants from system Settings. */
@Composable
private fun rememberMicPermission(): State<Boolean> {
    val context = LocalContext.current
    val granted = remember { mutableStateOf(micAllowed(context)) }
    val owner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    DisposableEffect(owner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_RESUME) {
                granted.value = micAllowed(context)
            }
        }
        owner.lifecycle.addObserver(observer)
        onDispose { owner.lifecycle.removeObserver(observer) }
    }
    return granted
}

private fun micAllowed(context: android.content.Context): Boolean =
    androidx.core.content.ContextCompat.checkSelfPermission(
        context, android.Manifest.permission.RECORD_AUDIO,
    ) == android.content.pm.PackageManager.PERMISSION_GRANTED

@Composable
private fun Transcript(
    state: ChatUiState,
    listState: androidx.compose.foundation.lazy.LazyListState,
    onAnswer: (String, EscalationOption?, String, List<String>) -> Unit,
    modifier: Modifier,
    onToggleSpeak: (String, String) -> Unit,
    onRetry: () -> Unit = {},
    onOpenSettings: (() -> Unit)? = null,
    onOpenAttention: (String?) -> Unit = {},
    onOpenCompleteResult: (ActivityRow) -> Unit = {},
    onOpenTask: (String) -> Unit = {},
    onLoadActivity: (String, String) -> Unit = { _, _ -> },
    onOpenOriginalAnswer: (OriginalAnswerLink) -> Unit = {},
) {
    Box(modifier.fillMaxWidth()) {
        when {
            state.loading -> Centered("Connecting to Magician…")

            // The conversation could not be opened at all. Named, and with the
            // two things worth doing about it — before this the panel printed
            // the problem and offered nothing.
            state.messages.isEmpty() && state.failure != null -> FailurePane(
                state.failure!!,
                onRetry = onRetry,
                onOpenSettings = onOpenSettings,
            )
            // An error replaced the transcript, so one failed turn hid every
            // message that came before it — and a dictated turn that failed
            // looked like it had never been spoken. The conversation stays;
            // the error is a banner over it.
            state.messages.isEmpty() && state.error != null -> ErrorPanel(state)
            state.messages.isEmpty() -> EmptyChat()
            else -> Column(Modifier.fillMaxSize()) {
                // Messages already on screen stay; the bar says the newest may
                // be missing, and offers the way to ask again.
                state.failure?.let { problem ->
                    FailureBanner(problem, onRetry = onRetry)
                }
                state.error?.let { problem ->
                    Surface(
                        color = Coral.copy(alpha = 0.10f),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            problem,
                            color = Coral, fontSize = 12.sp,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                        )
                    }
                }
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(state.messages, key = { it.id }) { message ->
                        Bubble(
                            message,
                            onAnswer = { option, text, ids -> onAnswer(message.id, option, text, ids) },
                            speaking = state.speakingMessageId == message.id,
                            onToggleSpeak = {
                                onToggleSpeak(message.id, message.text)
                            },
                            onOpenAttention = onOpenAttention,
                            onOpenCompleteResult = onOpenCompleteResult,
                            onOpenTask = onOpenTask,
                            onLoadActivity = onLoadActivity,
                            onOpenOriginalAnswer = onOpenOriginalAnswer,
                        )
                    }
                }
            }
        }
    }
}

@Composable
@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
private fun Bubble(
    message: ChatMessage,
    onAnswer: (EscalationOption?, String, List<String>) -> Unit = { _, _, _ -> },
    speaking: Boolean = false,
    onToggleSpeak: () -> Unit = {},
    onOpenAttention: (String?) -> Unit = {},
    onOpenCompleteResult: (ActivityRow) -> Unit = {},
    onOpenTask: (String) -> Unit = {},
    onLoadActivity: (String, String) -> Unit = { _, _ -> },
    onOpenOriginalAnswer: (OriginalAnswerLink) -> Unit = {},
) {
    val alignment = if (message.fromUser) Alignment.End else Alignment.Start
    val bubbleShape = remember { chatBubbleShape() }
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    val haptics = androidx.compose.ui.platform.LocalHapticFeedback.current
    // Long press copies the text. iOS has this on a context menu and this
    // client had no way to get a message out at all — the single most ordinary
    // thing to want from a reply, and it needed retyping.
    val copyable = Modifier.combinedClickable(
        onClick = {},
        onLongClick = {
            val body = message.text.trim()
            if (body.isNotEmpty()) {
                clipboard.setText(androidx.compose.ui.text.AnnotatedString(body))
                haptics.performHapticFeedback(
                    androidx.compose.ui.hapticfeedback.HapticFeedbackType.LongPress,
                )
            }
        },
    )
    LaunchedEffect(message.id, message.chatTurnId, message.kind) {
        val turnId = message.chatTurnId
        if (!message.fromUser && message.kind == MessageKind.Text && !turnId.isNullOrBlank()) {
            onLoadActivity(message.id, turnId)
        }
    }
    Column(
        Modifier.fillMaxWidth().then(if (message.originalAnswer != null) Modifier.border(1.dp, Coral.copy(alpha = 0.35f), RoundedCornerShape(12.dp)).padding(8.dp) else Modifier),
        horizontalAlignment = alignment,
    ) {
        message.originalAnswer?.let { link ->
            Row(
                Modifier.clip(RoundedCornerShape(6.dp)).clickable { onOpenOriginalAnswer(link) }
                    .padding(horizontal = 6.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Icon(Icons.Outlined.Link, contentDescription = null, tint = Coral, modifier = Modifier.size(14.dp))
                Text("View original answer", color = Coral, fontSize = 11.sp)
            }
        }
        if (message.fromUser && message.voiceOrigin) {
            VoiceOriginBadge()
        }
        // Tools are named while they run, so a long pause has a visible cause
        // rather than looking like the app has stalled.
        if (message.activity.isNotEmpty()) {
            Text(
                text = message.activity.joinToString(" · "),
                color = Muted, fontSize = 11.sp,
                modifier = Modifier.padding(start = 4.dp, bottom = 2.dp),
            )
        }
        // Typed cards outrank the transport direction. Task lifecycle records
        // are system messages, and terminal records often have no structured
        // presentation; treating those as notes first produced an empty gap
        // instead of the completed task card.
        when (message.kind) {
            MessageKind.TaskStatus -> {
                message.task?.let { TaskCard(it, onOpenTask) }
                return@Column
            }
            MessageKind.Escalation -> {
                message.escalation?.let { EscalationCardView(it, onAnswer, onOpenAttention) }
                return@Column
            }
            MessageKind.Attachment -> {
                message.attachment?.let { (name, size) ->
                    AttachmentRow(name, size, fromUser = message.fromUser)
                }
                return@Column
            }
            MessageKind.Text -> Unit
        }
        if (message.system && message.structured == null) {
            SystemNote(message.text)
            return@Column
        }
        val structured = message.structured
        Surface(
            // iOS gives the answer itself the theme's tinted surface. Steps is
            // supporting disclosure and must not be the strongest filled
            // shape underneath an otherwise near-background response.
            color = chatBubbleColor(message.fromUser),
            shape = bubbleShape,
            // Only a structured card carries an edge, tinted by its tone. A
            // plain bubble outlined like a card reads as two kinds of answer
            // where there is one.
            border = if (structured != null) {
                BorderStroke(1.dp, structuredToneColor(structured.tone).copy(alpha = 0.4f))
            } else {
                null
            },
            // The owner's own words are capped so they stay recognisably a
            // message; an answer is not, because a table or a code block that
            // has to wrap at 320dp is unreadable.
            // Copy belongs to the words, not to the row. On the whole column
            // it put a ripple over task and escalation cards — which have their
            // own controls and no text worth copying — and long-pressing one
            // would have lifted its title instead.
            modifier = (if (message.fromUser) Modifier.widthIn(max = 320.dp) else Modifier)
                .then(copyable),
        ) {
            Column(
                Modifier.padding(CHAT_BUBBLE_CONTENT_PADDING_DP.dp),
            ) {
                if (structured != null) {
                    // The backend composed this answer as blocks. Rendering the
                    // flat text instead would collapse a table into a paragraph.
                    StructuredResponseView(structured)
                } else {
                    MarkdownText(
                        markdown = message.text.ifEmpty { if (message.streaming) "…" else "" },
                        color = when {
                            message.fromUser -> OnAccent
                            message.failed -> Coral
                            else -> Ink
                        },
                        fontSize = CHAT_BUBBLE_FONT_SP.sp,
                        lineHeight = CHAT_BUBBLE_LINE_HEIGHT_SP.sp,
                    )
                }
                // A caret only while the turn is live, so a settled transcript
                // does not look like it is still working.
                if (message.streaming) {
                    Spacer(Modifier.height(4.dp))
                    LinearProgressIndicator(
                        color = Teal,
                        trackColor = BorderSoft,
                        modifier = Modifier.fillMaxWidth().height(2.dp),
                    )
                }
            }
        }
        if (!message.fromUser) {
            ActivitySection(
                message.activityRows,
                onOpenAttention = { onOpenAttention(null) },
                isLive = message.streaming,
                onOpenCompleteResult = onOpenCompleteResult,
            )
            if (message.text.isNotBlank() && !message.streaming) {
                MessageSpeakButton(active = speaking, onToggle = onToggleSpeak)
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun CompleteResultSheet(
    state: CompleteResultViewerState,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = Ground,
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .fillMaxHeight(0.92f)
                .padding(horizontal = 16.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    state.title,
                    color = Ink,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    "Done",
                    color = Coral,
                    fontSize = 13.sp,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .clickable(role = Role.Button, onClick = onDismiss)
                        .padding(10.dp),
                )
            }
            state.contentHash?.takeIf { it.isNotBlank() }?.let { hash ->
                Text(
                    "Verified content · ${hash.take(12)}",
                    color = Muted,
                    fontSize = 10.sp,
                    fontFamily = LocalMagicanFontFamilies.current.mono,
                    modifier = Modifier.padding(bottom = 10.dp),
                )
            }
            when {
                state.loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(color = Coral)
                }
                state.error != null -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(
                        state.error.orEmpty(),
                        color = Danger,
                        fontSize = 13.sp,
                        lineHeight = 18.sp,
                        textAlign = TextAlign.Center,
                    )
                }
                else -> SelectionContainer {
                    Text(
                        text = state.text,
                        color = Ink,
                        fontSize = 12.sp,
                        lineHeight = 17.sp,
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        modifier = Modifier
                            .fillMaxSize()
                            .verticalScroll(rememberScrollState())
                            .padding(bottom = 24.dp),
                    )
                }
            }
        }
    }
}

@Composable
private fun VoiceOriginBadge() {
    val color = voiceOriginBadgeColor()
    Row(
        modifier = Modifier.padding(end = 4.dp, bottom = 2.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            Icons.Outlined.GraphicEq,
            contentDescription = null,
            tint = color,
            modifier = Modifier.size(11.dp),
        )
        Text("Voice", color = color, fontSize = 11.sp, fontWeight = FontWeight.Medium)
    }
}

@Composable
private fun MessageSpeakButton(active: Boolean, onToggle: () -> Unit) {
    val color = if (active) Coral else Muted
    Surface(
        // A Material clickable Surface enforces a 48dp control around this
        // tiny secondary action. The plain Surface plus explicit clickable
        // keeps the visible control as compact as iOS while preserving button
        // semantics and the full row as its hit target.
        modifier = Modifier
            .padding(start = MESSAGE_SPEAK_LEADING_PADDING_DP.dp, top = 2.dp)
            .clickable(role = androidx.compose.ui.semantics.Role.Button, onClick = onToggle),
        color = Color.Transparent,
        shape = CircleShape,
        border = BorderStroke(1.dp, color.copy(alpha = 0.30f)),
    ) {
        Row(
            modifier = Modifier.padding(
                horizontal = 7.dp,
                vertical = MESSAGE_SPEAK_VERTICAL_PADDING_DP.dp,
            ),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                if (active) Icons.Filled.Stop else Icons.AutoMirrored.Outlined.VolumeUp,
                contentDescription = null,
                tint = color,
                modifier = Modifier.size(11.dp),
            )
            Text(
                messageSpeechActionLabel(active),
                color = color,
                fontSize = MESSAGE_SPEAK_FONT_SP.sp,
                fontWeight = FontWeight.Medium,
            )
        }
    }
}

internal fun messageSpeechActionLabel(active: Boolean): String =
    if (active) "Stop" else "Speak"

/**
 * The composer, laid out as iOS lays it out.
 *
 * A rounded **card** floating on the background — not a flat bar. Inside, in
 * order: the staged-attachment strip, the field with a single trailing slot, and
 * a horizontally scrollable row of secondary tools.
 *
 * The trailing slot is one control, not three. Mic when the field is empty, Send
 * once there is something to send, Stop while a turn runs. iOS does this because
 * the primary action is always exactly one thing, and offering mic and send side
 * by side asks the owner to choose between them every time.
 *
 * Plan and Accept tint the card, so a turn that will plan or skip in-scope
 * file HITL announces itself before it is sent.
 */
@Composable
private fun Composer(
    voiceQueue: @Composable () -> Unit = {},
    onBackground: () -> Unit = {},
    onStopAndSend: () -> Unit = {},
    state: ChatUiState,
    onDraftChange: (String) -> Unit,
    onSend: () -> Unit,
    onStop: () -> Unit,
    onPickProfile: (String) -> Unit,
    onPickEngine: (String) -> Unit,
    onPickModel: (String) -> Unit,
    onAttach: () -> Unit,
    onCamera: () -> Unit,
    /** Null when the companion cannot read the screen, so no dead button shows. */
    onTeachScreen: (() -> Unit)? = null,
    onMic: () -> Unit,
    onHoldStart: () -> Unit,
    onHoldEnd: () -> Unit,
    speakReplies: Boolean,
    onToggleSpeak: () -> Unit,
    onOpenReplySettings: () -> Unit,
    onOpenLiveSettings: () -> Unit,
    liveActive: Boolean,
    onToggleLive: () -> Unit,
    onRemoveAttachment: (String) -> Unit,
    onComposerMode: (ai.magicbeans.magdroid.chat.ChatComposerMode) -> Unit,
) {
    val composerMode = state.composerMode
    val context = LocalContext.current
    remember(context) { PrimaryAgentWakeIdentityStore.initialize(context) }
    val primaryAgent by PrimaryAgentWakeIdentityStore.identity.collectAsStateWithLifecycle()
    val assistantName = primaryAgent?.let { identity ->
        (listOf(identity.name) + identity.aliases).firstNotNullOfOrNull { name ->
            name.trim().takeIf { it.isNotEmpty() }
        }
    } ?: ProductIdentity.productName
    val identityScope = rememberCoroutineScope()
    androidx.lifecycle.compose.LifecycleResumeEffect(context) {
        val refresh = identityScope.launch { PrimaryAgentWakeIdentityStore.refresh(context) }
        onPauseOrDispose { refresh.cancel() }
    }
    // Voice is the default mode, as on iOS.
    var voiceMode by remember { mutableStateOf(true) }
    var profileMenu by remember { mutableStateOf(false) }
    val acting = composerMode != ai.magicbeans.magdroid.chat.ChatComposerMode.Ask
    val accentEdge = if (acting) Coral.copy(alpha = 0.4f) else Muted.copy(alpha = 0.25f)

    Box(
        Modifier
            .fillMaxWidth()
            .background(if (acting) Coral.copy(alpha = 0.05f) else Ground)
            .padding(horizontal = 10.dp, vertical = 4.dp),
    ) {
        Surface(
            // iOS uses the theme's surface, not the elevated card layer. This
            // keeps the composer related to assistant content in every theme.
            color = composerSurfaceColor(),
            shape = RoundedCornerShape(COMPOSER_CORNER_DP.dp),
            border = BorderStroke(1.dp, accentEdge),
        ) {
            Column(Modifier.fillMaxWidth()) {
                voiceQueue()
                Column(
                    Modifier.padding(horizontal = 12.dp, vertical = 9.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    if (state.attachments.isNotEmpty()) AttachmentStrip(state, onRemoveAttachment)

                    // iOS opens in voice mode: while nothing has been typed the field
                    // is *replaced* by a large centred mic with mute and Live
                    // flanking it. It is a different composer state, not a button,
                    // and starting in text mode was the wrong default.
                    if (voiceMode && state.draft.isEmpty() && !liveActive) {
                        VoiceHero(
                            state = state.dictation,
                            partial = state.partialTranscript,
                            onMic = onMic,
                            onHoldStart = onHoldStart,
                            onHoldEnd = onHoldEnd,
                            speakReplies = speakReplies,
                            onToggleSpeak = onToggleSpeak,
                            onOpenReplySettings = onOpenReplySettings,
                            onOpenLiveSettings = onOpenLiveSettings,
                            liveActive = liveActive,
                            onToggleLive = onToggleLive,
                            onKeyboard = { voiceMode = false },
                        )
                    } else {
                    Row(verticalAlignment = Alignment.Bottom) {
                        BasicComposerField(
                            value = state.draft,
                            placeholder = when (composerMode) {
                                ai.magicbeans.magdroid.chat.ChatComposerMode.Plan -> "Plan with $assistantName…"
                                ai.magicbeans.magdroid.chat.ChatComposerMode.AcceptInScope ->
                                    "Accept in-scope edits…"
                                ai.magicbeans.magdroid.chat.ChatComposerMode.Ask -> "Ask $assistantName…"
                            },
                            onValueChange = onDraftChange,
                            modifier = Modifier.weight(1f),
                        )
                        Spacer(Modifier.width(4.dp))
                        if (state.draft.isNotBlank() && state.composerMode != ChatComposerMode.Plan) {
                            ComposerSendOptions(state, onSend, onStopAndSend, onBackground)
                        }
                        TrailingSlot(
                            state = state,
                            onSend = onSend,
                            onStop = onStop,
                            onVoice = { voiceMode = true },
                            liveActive = liveActive,
                        )
                    }
                    }

                    // One horizontal surface owns every secondary action, matching
                    // iOS. A LazyRow gives long profile labels and narrow phones
                    // real overflow instead of asking a width-filling Row to both
                    // constrain and scroll itself.
                    LazyRow(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        item {
                            ModeToggle(
                                mode = composerMode,
                                permission = state.composerDoPermission,
                                onChange = onComposerMode,
                            )
                        }
                        item {
                            Box(
                                Modifier
                                    .height(16.dp)
                                    .width(1.dp)
                                    .background(Muted.copy(alpha = 0.3f)),
                            )
                        }
                        item { ToolIcon(Icons.Outlined.AttachFile, "Attach a file", onClick = onAttach) }
                        item { ToolIcon(Icons.Outlined.PhotoCamera, "Add a photo", onClick = onCamera) }
                        // Ask about the screen itself. `teachThisScreen` was written
                        // for this and never given a way in: the quick-settings tile
                        // and the assist gesture both reach the tutor from outside
                        // the app, and inside it there was no way to ask at all.
                        //
                        // Android-only by capability rather than by choice — iOS
                        // cannot read another app's screen from inside itself, which
                        // is why its version is a share-sheet extension.
                        if (onTeachScreen != null) {
                            item {
                                ToolIcon(
                                    Icons.Outlined.Screenshot,
                                    "Explain my screen",
                                    onClick = onTeachScreen,
                                )
                            }
                        }
                        item {
                            Box {
                                ProfileChip(state) { profileMenu = true }
                                DropdownMenu(profileMenu, onDismissRequest = { profileMenu = false }) {
                                    Text("ENGINE", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp), color = Muted)
                                    state.chatHarnesses.forEach { engine ->
                                        DropdownMenuItem(
                                            text = { Text("${engine.name.replace('_', ' ')}${if (engine.name == state.selectedHarnessEngine) " ✓" else ""}") },
                                            onClick = { onPickEngine(engine.name) },
                                        )
                                    }
                                    HorizontalDivider()
                                    if (state.selectedHarnessEngine == "magician" || state.selectedHarnessEngine == "pi") {
                                        Text("PROFILE", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp), color = Muted)
                                        state.profiles.forEach { profile ->
                                            ProfileMenuItem(
                                                profile = profile,
                                                selected = profile.name == state.selectedProfile ||
                                                    (state.selectedProfile.isBlank() && profile.isDefault),
                                                onClick = {
                                                    onPickProfile(profile.name)
                                                    profileMenu = false
                                                },
                                            )
                                        }
                                    } else {
                                        Text("MODEL", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp), color = Muted)
                                        (state.chatHarnesses.firstOrNull { it.name == state.selectedHarnessEngine }?.models ?: listOf("default"))
                                            .forEach { model ->
                                                DropdownMenuItem(
                                                    text = { Text("${if (model == "default") "Harness default" else model}${if (model == state.selectedHarnessModel) " ✓" else ""}") },
                                                    onClick = { onPickModel(model); profileMenu = false },
                                                )
                                            }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ComposerSendOptions(state: ChatUiState, onSend: () -> Unit, onStopAndSend: () -> Unit, onBackground: () -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconButton(onClick = { open = true }, enabled = !state.queueMutationInFlight,
            modifier = Modifier.width(32.dp).height(36.dp)) {
            Icon(Icons.Outlined.KeyboardArrowDown, contentDescription = "Send options", tint = Secondary, modifier = Modifier.size(18.dp))
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }, containerColor = Panel) {
            DropdownMenuItem(text = { Text(if (state.sending || state.serverRunning) "Queue message" else "Send message", color = Ink) },
                onClick = { open = false; onSend() })
            if (state.sending || state.serverRunning) {
                DropdownMenuItem(text = { Text("Stop & send", color = Ink) }, onClick = { open = false; onStopAndSend() })
            }
            if (state.attachments.isEmpty()) {
                DropdownMenuItem(text = { Text("Run in parallel", color = Ink) }, onClick = { open = false; onBackground() })
            }
        }
    }
}

internal fun composerDoLabel(permission: ChatDoPermission): String = when (permission) {
    ChatDoPermission.Ask -> "DO · ASK"
    ChatDoPermission.AcceptInScope -> "DO · ACCEPT"
}

internal fun composerDoGuidance(permission: ChatDoPermission): String = when (permission) {
    ChatDoPermission.Ask -> "Prompt before each file edit"
    ChatDoPermission.AcceptInScope ->
        "In-scope file edits, no prompt. Anything outside the workspace still asks."
}

/** Do is split into an activating face and an Ask/Accept permission menu; Plan is its peer. */
@Composable
private fun ModeToggle(
    mode: ChatComposerMode,
    permission: ChatDoPermission,
    onChange: (ChatComposerMode) -> Unit,
) {
    var permissionMenuOpen by remember { mutableStateOf(false) }
    val doSelected = mode != ChatComposerMode.Plan
    val doForeground = if (doSelected) composerModeForeground(permission.mode) else Muted
    val doFill = if (doSelected) composerModeFill(permission.mode) else Color.Transparent

    Surface(color = composerModeTrackColor(), shape = RoundedCornerShape(6.dp)) {
        Row(Modifier.padding(1.dp)) {
            Box {
                Row(Modifier.height(IntrinsicSize.Min)) {
                    Surface(
                        color = doFill,
                        shape = RoundedCornerShape(topStart = 4.dp, bottomStart = 4.dp),
                        modifier = Modifier
                            .heightIn(min = COMPOSER_MODE_MIN_HEIGHT_DP.dp)
                            .clickable(role = Role.Button) { onChange(permission.mode) }
                            .semantics {
                                contentDescription = composerDoLabel(permission)
                                selected = doSelected
                            },
                    ) {
                        Text(
                            composerDoLabel(permission),
                            color = doForeground,
                            fontSize = COMPOSER_MODE_FONT_SP.sp,
                            lineHeight = 12.sp,
                            maxLines = 1,
                            fontWeight = FontWeight.SemiBold,
                            fontFamily = LocalMagicanFontFamilies.current.mono,
                            letterSpacing = 0.4.sp,
                            modifier = Modifier.wrapContentHeight(Alignment.CenterVertically).padding(
                                start = COMPOSER_MODE_HORIZONTAL_PADDING_DP.dp,
                                end = 5.dp,
                                top = COMPOSER_MODE_VERTICAL_PADDING_DP.dp,
                                bottom = COMPOSER_MODE_VERTICAL_PADDING_DP.dp,
                            ),
                        )
                    }

                    Box(
                        Modifier
                            .width(1.dp)
                            .fillMaxHeight()
                            .background(doForeground.copy(alpha = 0.28f)),
                    )

                    Surface(
                        color = doFill,
                        shape = RoundedCornerShape(topEnd = 4.dp, bottomEnd = 4.dp),
                        modifier = Modifier.fillMaxHeight().clickable(role = Role.Button) {
                            permissionMenuOpen = !permissionMenuOpen
                        },
                    ) {
                        Box(Modifier.width(22.dp), contentAlignment = Alignment.Center) {
                            Icon(
                                Icons.Outlined.KeyboardArrowDown,
                                contentDescription = "Choose what Do asks before editing files",
                                tint = doForeground,
                                modifier = Modifier.size(14.dp),
                            )
                        }
                    }
                }

                DropdownMenu(
                    expanded = permissionMenuOpen,
                    onDismissRequest = { permissionMenuOpen = false },
                ) {
                    ChatDoPermission.entries.forEach { choice ->
                        val checked = permission == choice
                        DropdownMenuItem(
                            text = {
                                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                                    Text(
                                        if (choice == ChatDoPermission.Ask) "Ask" else "Accept",
                                        fontWeight = FontWeight.SemiBold,
                                    )
                                    Text(
                                        composerDoGuidance(choice),
                                        color = Secondary,
                                        fontSize = 11.sp,
                                    )
                                }
                            },
                            leadingIcon = {
                                Text(
                                    if (checked) "✓" else "",
                                    color = Coral,
                                    fontWeight = FontWeight.Bold,
                                    modifier = Modifier.width(16.dp),
                                )
                            },
                            onClick = {
                                permissionMenuOpen = false
                                onChange(choice.mode)
                            },
                            modifier = Modifier.semantics { selected = checked },
                        )
                    }
                }
            }

            val planSelected = mode == ChatComposerMode.Plan
            Surface(
                color = if (planSelected) composerModeFill(ChatComposerMode.Plan) else Color.Transparent,
                shape = RoundedCornerShape(4.dp),
                modifier = Modifier
                    .heightIn(min = COMPOSER_MODE_MIN_HEIGHT_DP.dp)
                    .clickable(role = Role.Button) { onChange(ChatComposerMode.Plan) }
                    .semantics { selected = planSelected },
            ) {
                Text(
                    "PLAN",
                    color = if (planSelected) composerModeForeground(ChatComposerMode.Plan) else Muted,
                    fontSize = COMPOSER_MODE_FONT_SP.sp,
                    lineHeight = 12.sp,
                    maxLines = 1,
                    fontWeight = FontWeight.SemiBold,
                    fontFamily = LocalMagicanFontFamilies.current.mono,
                    letterSpacing = 0.4.sp,
                    modifier = Modifier.wrapContentHeight(Alignment.CenterVertically).padding(
                        horizontal = COMPOSER_MODE_HORIZONTAL_PADDING_DP.dp,
                        vertical = COMPOSER_MODE_VERTICAL_PADDING_DP.dp,
                    ),
                )
            }
        }
    }
}

@Composable
private fun ProfileChip(state: ChatUiState, onClick: () -> Unit) {
    val active = state.profiles.firstOrNull { it.name == state.selectedProfile }
        ?: state.profiles.firstOrNull { it.isDefault }
        ?: state.profiles.firstOrNull()
    val usesProfile = state.selectedHarnessEngine == "magician" || state.selectedHarnessEngine == "pi"
    val isDefault = if (usesProfile) active == null || active.isDefault else state.selectedHarnessModel == "default"
    Surface(
        color = composerSurfaceColor(),
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(
            1.dp,
            if (isDefault) Muted.copy(alpha = 0.25f) else Coral.copy(alpha = 0.5f),
        ),
        modifier = Modifier.clickable { onClick() },
    ) {
        Row(
            Modifier.padding(
                horizontal = 8.dp,
                vertical = COMPOSER_PROFILE_VERTICAL_PADDING_DP.dp,
            ),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Icon(
                Icons.Outlined.AutoAwesome,
                contentDescription = null,
                tint = if (isDefault) Ink else Coral,
                modifier = Modifier.size(12.dp),
            )
            if (usesProfile && active?.isAdaptive == true) {
                ChatProfileTag("Adaptive", Coral)
            }
            active?.adaptiveTier?.takeIf { usesProfile && it.isNotBlank() }?.let { tier ->
                ChatProfileTag(tier, chatProfileTierColor(tier))
            }
            Text(
                "${state.selectedHarnessEngine.replace('_', ' ')} · ${if (usesProfile) active?.display() ?: "Default" else state.selectedHarnessModel}",
                color = Ink, fontSize = 10.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = 180.dp),
            )
            Icon(
                Icons.Outlined.UnfoldMore,
                contentDescription = null,
                tint = Muted,
                modifier = Modifier.size(12.dp),
            )
        }
    }
}

@Composable
private fun ProfileMenuItem(
    profile: ChatProfile,
    selected: Boolean,
    onClick: () -> Unit,
) {
    DropdownMenuItem(
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        profile.name,
                        color = Ink,
                        fontSize = 14.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                    )
                    if (profile.isDefault) {
                        Text("★", color = MWarn, fontSize = 11.sp)
                    }
                    if (selected) {
                        Text("✓", color = Coral, fontSize = 13.sp, fontWeight = FontWeight.Bold)
                    }
                }
                if (profile.isAdaptive || !profile.adaptiveTier.isNullOrBlank()) {
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (profile.isAdaptive) ChatProfileTag("Adaptive", Coral)
                        profile.adaptiveTier?.takeIf { it.isNotBlank() }?.let { tier ->
                            ChatProfileTag(tier, chatProfileTierColor(tier))
                        }
                    }
                }
                profile.model?.takeIf { it.isNotBlank() }?.let { model ->
                    Text(
                        model,
                        color = Secondary,
                        fontSize = 11.sp,
                        fontFamily = LocalMagicanFontFamilies.current.mono,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                (profile.adaptiveDescription ?: profile.description)
                    ?.takeIf { it.isNotBlank() }
                    ?.let { description ->
                        Text(
                            description,
                            color = Secondary,
                            fontSize = 11.sp,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
            }
        },
        onClick = onClick,
    )
}

@Composable
private fun ChatProfileTag(text: String, color: Color) {
    Surface(
        color = color.copy(alpha = 0.12f),
        shape = RoundedCornerShape(COMPOSER_PROFILE_TAG_CORNER_DP.dp),
        border = BorderStroke(1.dp, color.copy(alpha = 0.36f)),
    ) {
        Text(
            text.uppercase(),
            color = color,
            fontSize = COMPOSER_PROFILE_TAG_FONT_SP.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            modifier = Modifier.padding(
                horizontal = COMPOSER_PROFILE_TAG_HORIZONTAL_PADDING_DP.dp,
                vertical = COMPOSER_PROFILE_TAG_VERTICAL_PADDING_DP.dp,
            ),
        )
    }
}

/**
 * The single trailing slot, drawn as iOS draws it.
 *
 * A 36dp rounded square at radius 12 — not a circle — and always filled: danger
 * red for Stop, accent for Send, accent for the mic. Never a ghost outline. The
 * primary action is never ambiguous about being the primary action, which is why
 * the empty state is a solid mic rather than a hint of one.
 */
@Composable
private fun TrailingSlot(
    state: ChatUiState,
    onSend: () -> Unit,
    onStop: () -> Unit,
    onVoice: () -> Unit,
    liveActive: Boolean = false,
) {
    val hasText = state.draft.isNotBlank()
    val stopping = (state.sending || state.serverRunning) && !hasText
    if (liveActive && !hasText && !stopping) return
    val fill = if (stopping) Danger else Coral
    Surface(
        color = fill,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier
            .size(36.dp)
            .clickable(enabled = !state.queueMutationInFlight) {
                when {
                    stopping -> onStop()
                    hasText -> onSend()
                    else -> onVoice()
                }
            },
    ) {
        Box(contentAlignment = Alignment.Center) {
            if (!stopping && !hasText) {
                Icon(
                    Icons.Filled.Mic,
                    contentDescription = "Switch to voice",
                    tint = OnAccent,
                    modifier = Modifier.size(20.dp),
                )
            } else {
                Text(
                    text = if (stopping) "■" else "↑",
                    color = if (stopping) chatContrastingTextColor(fill) else OnAccent,
                    fontSize = if (stopping) 13.sp else 17.sp,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@Composable
private fun VoiceHero(
    state: DictationState,
    partial: String,
    onMic: () -> Unit,
    onHoldStart: () -> Unit,
    onHoldEnd: () -> Unit,
    speakReplies: Boolean,
    onToggleSpeak: () -> Unit,
    onOpenReplySettings: () -> Unit,
    onOpenLiveSettings: () -> Unit,
    liveActive: Boolean,
    onToggleLive: () -> Unit,
    onKeyboard: () -> Unit,
) {
    val recording = state == DictationState.Recording
    val transcribing = state == DictationState.Transcribing
    // A held capture ends on release; a tapped one ends on the next tap. The
    // hint has to say which, or the gesture that works is guesswork.
    var holding by remember { mutableStateOf(false) }
    val currentRecording by rememberUpdatedState(recording)
    val currentTranscribing by rememberUpdatedState(transcribing)
    val currentOnMic by rememberUpdatedState(onMic)
    val currentOnHoldStart by rememberUpdatedState(onHoldStart)
    val currentOnHoldEnd by rememberUpdatedState(onHoldEnd)
    Column(
        Modifier.fillMaxWidth().padding(vertical = 2.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // What was heard so far, while it is being heard. Showing it is the
        // difference between dictation you can trust and dictation you have to
        // re-read afterwards.
        if (recording && partial.isNotBlank()) {
            Text(
                partial,
                color = Ink, fontSize = 15.sp,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                maxLines = 3, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        } else if (transcribing) {
            Text("Transcribing…", color = Coral, fontSize = 13.sp, fontWeight = FontWeight.Medium)
        }

        // The mic sits centred with the flanking controls overlaid rather than
        // laid out beside it, so their differing widths cannot shift it
        // off-centre — the same reason iOS uses a ZStack here.
        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            // Recording turns the mic to the danger colour, because while it
            // is red the room is being listened to and that should never be
            // something you have to infer.
            val micFill = if (recording) Danger else Coral
            val micForeground = if (recording) chatContrastingTextColor(micFill) else OnAccent
            Surface(
                color = micFill,
                shape = CircleShape,
                shadowElevation = 10.dp,
                modifier = Modifier
                    .size(64.dp)
                    // Tap and hold are one gesture detector, not two controls.
                    // A tap toggles; a hold records for exactly as long as it
                    // is held. Holding is what you do when you know what you
                    // want to say and want it gone the moment you stop.
                    // Keep the detector alive when recording changes. Keying
                    // it to that state cancels the gesture it just started.
                    .pointerInput(Unit) {
                        detectTapGestures(
                            onTap = { if (!currentTranscribing) currentOnMic() },
                            onLongPress = {
                                if (!currentRecording && !currentTranscribing) {
                                    holding = true
                                    currentOnHoldStart()
                                }
                            },
                            onPress = {
                                // Only a recognized long press owns a held
                                // capture. A short tap must not start and stop
                                // a recording before onTap gets to toggle it.
                                try {
                                    tryAwaitRelease()
                                } finally {
                                    if (holding) {
                                        holding = false
                                        currentOnHoldEnd()
                                    }
                                }
                            },
                        )
                    },
            ) {
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    if (transcribing) {
                        CircularProgressIndicator(
                            color = micForeground, strokeWidth = 2.dp,
                            modifier = Modifier.size(24.dp),
                        )
                    } else {
                        Icon(
                            if (recording) Icons.Filled.Stop else Icons.Filled.Mic,
                            contentDescription = if (recording) "Stop dictation" else "Talk",
                            tint = micForeground,
                            modifier = Modifier.size(if (recording) 26.dp else 30.dp),
                        )
                    }
                }
            }
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // Replies. Two segments, as iOS builds it: the toggle you
                // reach for, and a chevron to everything else about voice.
                DockPill(
                    active = speakReplies,
                    onChevron = onOpenReplySettings,
                    chevronLabel = "More voice settings",
                ) {
                    Box(
                        Modifier
                            .width(COMPOSER_VOICE_DOCK_PRIMARY_WIDTH_DP.dp)
                            .fillMaxHeight()
                            .clickable { onToggleSpeak() },
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            if (speakReplies) Icons.AutoMirrored.Outlined.VolumeUp
                            else Icons.AutoMirrored.Outlined.VolumeOff,
                            contentDescription = if (speakReplies) "Mute spoken replies" else "Speak replies",
                            tint = if (speakReplies) Coral else Secondary,
                            modifier = Modifier.size(16.dp),
                        )
                    }
                }

                DockPill(
                    active = liveActive,
                    onChevron = onOpenLiveSettings,
                    chevronLabel = "Live call settings",
                ) {
                    Row(
                        Modifier
                            .fillMaxHeight()
                            .clickable { onToggleLive() }
                            .padding(start = 7.dp, end = 5.dp),
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(
                            Icons.Outlined.GraphicEq, contentDescription = null,
                            tint = Secondary, modifier = Modifier.size(12.dp),
                        )
                        Text(
                            if (liveActive) "Stop" else "Live",
                            color = if (liveActive) Coral else Secondary,
                            fontSize = 10.sp, fontWeight = FontWeight.SemiBold,
                        )
                    }
                }
            }
        }
        // Both gestures named, and the way out to typing, because a mic with no
        // caption leaves the keyboard undiscoverable.
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                dictationHint(state, holding),
                color = Secondary, fontSize = 12.sp, fontWeight = FontWeight.Medium,
            )
            if (!recording && !transcribing) {
                Text("·", color = Secondary, fontSize = 12.sp)
                Text(
                    "type instead",
                    color = Coral, fontSize = 12.sp, fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.clickable { onKeyboard() },
                )
            }
        }
    }
}

@Composable
private fun BasicComposerField(
    value: String,
    placeholder: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(modifier.heightIn(min = 36.dp), contentAlignment = Alignment.CenterStart) {
        if (value.isEmpty()) {
            Text(placeholder, color = Muted, fontSize = 16.sp, modifier = Modifier.padding(horizontal = 10.dp))
        }
        androidx.compose.foundation.text.BasicTextField(
            value = value,
            onValueChange = onValueChange,
            textStyle = androidx.compose.ui.text.TextStyle(color = Ink, fontSize = 16.sp),
            cursorBrush = androidx.compose.ui.graphics.SolidColor(Coral),
            maxLines = 5,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 10.dp),
        )
    }
}

/**
 * Staged files as 54dp tiles, matching iOS.
 *
 * A tile rather than a horizontal pill: iOS shows a thumbnail when it has one,
 * and a row of pills cannot become a row of thumbnails later without rebuilding
 * the strip. State is an overlay on the tile and the remove control sits in its
 * top corner, so the tile itself stays the file rather than becoming a control.
 */
@Composable
private fun AttachmentStrip(state: ChatUiState, onRemove: (String) -> Unit) {
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        state.attachments.forEach { file ->
            Box(Modifier.size(60.dp), contentAlignment = Alignment.TopEnd) {
                Surface(
                    color = Ground,
                    shape = RoundedCornerShape(10.dp),
                    border = BorderStroke(1.dp, Muted.copy(alpha = 0.2f)),
                    modifier = Modifier.size(54.dp).align(Alignment.BottomStart),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Column(
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(3.dp),
                        ) {
                            Icon(
                                Icons.Outlined.Description, contentDescription = null,
                                tint = Secondary, modifier = Modifier.size(18.dp),
                            )
                            Text(
                                file.shortName(),
                                color = Secondary, fontSize = 8.sp, maxLines = 1,
                            )
                        }
                        // State sits over the tile, as on iOS, so the file stays
                        // recognisable underneath whatever is happening to it.
                        if (file.uploading || file.failed) {
                            Box(
                                Modifier
                                    .matchParentSize()
                                    .background(Ink.copy(alpha = if (file.failed) 0.35f else 0.25f)),
                                contentAlignment = Alignment.Center,
                            ) {
                                if (file.failed) {
                                    Icon(
                                        Icons.Outlined.ErrorOutline,
                                        contentDescription = "Upload failed",
                                        tint = Color(0xFFFFE66D),
                                        modifier = Modifier.size(18.dp),
                                    )
                                } else {
                                    // A real indeterminate spinner: the static
                                    // glyph here looked like a file that had
                                    // stalled rather than one in flight.
                                    CircularProgressIndicator(
                                        color = Color.White,
                                        strokeWidth = 2.dp,
                                        modifier = Modifier.size(18.dp),
                                    )
                                }
                            }
                        }
                    }
                }
                Text(
                    "✕",
                    color = Color.White,
                    fontSize = 11.sp,
                    modifier = Modifier
                        .background(Ink.copy(alpha = 0.7f), RoundedCornerShape(9.dp))
                        .clickable { onRemove(file.localId) }
                        .padding(horizontal = 5.dp, vertical = 1.dp),
                )
            }
        }
    }
}


/**
 * A task spawned from chat, as its own card.
 *
 * Bordered in its status colour and never shaped like a bubble: a task is
 * something running on your behalf, not something the assistant said. The verb
 * leads because "what is it doing" is the question being asked of this card.
 */
@Composable
private fun TaskCard(task: TaskStatusCard, onOpenTask: (String) -> Unit) {
    val edge = when {
        task.failed -> Coral
        task.terminal -> Teal
        else -> Muted
    }
    Surface(
        color = Panel,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, edge.copy(alpha = 0.35f)),
    ) {
        Column(
            Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "${task.verb()}: ${task.title}",
                color = Ink, fontSize = 15.sp, fontWeight = FontWeight.SemiBold,
            )
            // The summary when there is one; a task with neither summary nor
            // output is still worth showing, because its status is the news.
            task.summary?.takeIf { it.isNotBlank() }?.let {
                MarkdownText(
                    markdown = it,
                    color = Ink,
                    fontSize = 13.sp,
                    lineHeight = 18.sp,
                )
            }
            if (task.running) {
                LinearProgressIndicator(
                    color = Teal,
                    trackColor = BorderSoft,
                    modifier = Modifier.fillMaxWidth().height(2.dp),
                )
            }
            if (task.terminalLines.isNotEmpty()) {
                TerminalBlock(task.terminalLines)
            }
            task.outputs.forEach { OutputRow(it, task.taskId) }
            task.taskId.takeIf { it.isNotBlank() && it != "unknown-task" }?.let { taskId ->
                HorizontalDivider(color = BorderSoft)
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable(
                            role = Role.Button,
                            onClickLabel = "Inspect run",
                        ) { onOpenTask(taskId) }
                        .padding(vertical = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Inspect run",
                        color = Coral,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.weight(1f),
                    )
                    Icon(
                        Icons.Outlined.ChevronRight,
                        contentDescription = null,
                        tint = Coral,
                        modifier = Modifier.size(16.dp),
                    )
                }
            }
        }
    }
}

/**
 * The run's shell output, streaming — what `ShellOutputChunk` was waiting on.
 *
 * A window, not a pane: dark ground, monospace, bounded height so a chatty
 * build cannot push the card's own controls off screen, stuck to the newest
 * line because live output is read at its end. Scrolling back is still there
 * for the lines the cap kept.
 */
@Composable
private fun TerminalBlock(lines: List<String>) {
    val listState = rememberLazyListState()
    LaunchedEffect(lines.size) {
        if (lines.isNotEmpty()) listState.scrollToItem(lines.lastIndex)
    }
    Surface(
        color = Color(0xFF10141A),
        shape = RoundedCornerShape(6.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        LazyColumn(
            state = listState,
            modifier = Modifier.heightIn(max = 180.dp).padding(horizontal = 10.dp, vertical = 8.dp),
        ) {
            items(lines.size) { index ->
                Text(
                    // A blank line of output still occupies its row; an empty
                    // Text would collapse it.
                    lines[index].ifEmpty { " " },
                    color = Color(0xFFB9C4CE),
                    fontSize = 11.sp,
                    lineHeight = 15.sp,
                    fontFamily = LocalMagicanFontFamilies.current.mono,
                )
            }
        }
    }
}

/**
 * A paused execution asking for a decision.
 *
 * The options stay visible after it is answered, greyed rather than removed —
 * the history has to keep saying what was asked and what was chosen.
 */
@Composable
private fun EscalationCardView(
    card: EscalationCard,
    onAnswer: (EscalationOption?, String, List<String>) -> Unit,
    onOpenAttention: (String?) -> Unit = {},
) {
    // Held per card, not hoisted: a half-typed password belongs to the card
    // that asked for it and must not survive it scrolling away and back.
    var typed by remember(card.correlationId) { mutableStateOf("") }
    val picked = remember(card.correlationId) { mutableStateListOf<String>() }

    Surface(
        color = Panel,
        shape = RoundedCornerShape(8.dp),
        border = BorderStroke(1.dp, Coral.copy(alpha = if (card.resolved) 0.2f else 0.5f)),
    ) {
        Column(
            Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                if (card.resolved) "Answered" else "Action required",
                color = if (card.resolved) Muted else Coral,
                fontSize = 11.sp, fontWeight = FontWeight.SemiBold,
            )
            Text(card.question, color = Ink, fontSize = 14.sp)
            val busy = card.answering != null
            val locked = card.resolved || busy

            when (card.inputType) {
                "form" -> {
                    Text(
                        "Open this request in Attention to answer every field.",
                        color = Secondary,
                        fontSize = 12.sp,
                    )
                    EscalationSubmit(
                        enabled = !locked && !card.correlationId.isNullOrBlank(),
                        label = "Open in Attention",
                    ) {
                        onOpenAttention(card.correlationId)
                    }
                }

                // The typed kinds. Without these an escalation that wants a
                // string renders a question and nothing to answer it with, and
                // the execution stays paused with no way out of it.
                "text", "guidance", "file_path", "password", "otp" -> {
                    card.sensitive?.let { spec ->
                        Text(
                            if (spec.oneTime == true) "Used once, then discarded. Never shown to the assistant."
                            else "Held privately for this run. Never shown to the assistant.",
                            color = Secondary,
                            fontSize = 12.sp,
                        )
                    }
                    EscalationTextAnswer(
                        inputType = card.inputType,
                        value = typed,
                        onValue = { typed = it },
                        enabled = !locked && card.sensitive?.isExpired() != true,
                        onSubmit = { onAnswer(null, typed, emptyList()) },
                        renderKind = ai.magicbeans.magdroid.chat.hitlRenderKind(card.inputType, card.sensitive?.kind),
                        placeholder = card.placeholder,
                    )
                }

                "multi_choice" -> {
                    card.options.forEach { option ->
                        val on = picked.contains(option.id)
                        EscalationOptionRow(
                            option = option,
                            selected = on,
                            dimmed = card.resolved,
                            enabled = !locked,
                            onClick = { if (on) picked.remove(option.id) else picked.add(option.id) },
                        )
                    }
                    EscalationSubmit(enabled = !locked && picked.isNotEmpty()) {
                        onAnswer(null, "", picked.toList())
                    }
                }

                // Single choice, and the shapes that are choice-flavoured —
                // confirmation and external_action both answer by picking one.
                else -> card.options.forEach { option ->
                    EscalationOptionRow(
                        option = option,
                        selected = card.answering == option.id,
                        dimmed = card.resolved,
                        enabled = !locked,
                        onClick = { onAnswer(option, typed, emptyList()) },
                    )
                }
            }

            // A refused answer leaves the execution paused, so it has to say so
            // rather than quietly doing nothing.
            card.error?.let {
                Text(it, color = Coral, fontSize = 11.sp)
            }
        }
    }
}

/** One option, as a button or as read-only history. */
@Composable
internal fun EscalationOptionRow(
    option: EscalationOption,
    selected: Boolean,
    dimmed: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    Surface(
        color = if (selected) Coral.copy(alpha = 0.12f) else Ground,
        shape = RoundedCornerShape(6.dp),
        border = if (selected) BorderStroke(1.dp, Coral.copy(alpha = 0.5f)) else null,
        modifier = Modifier
            .fillMaxWidth()
            // Answered or mid-flight: no longer a control. The options stay
            // legible so the history still reads.
            .then(if (enabled) Modifier.clickable { onClick() } else Modifier),
    ) {
        Column(Modifier.padding(horizontal = 10.dp, vertical = 8.dp)) {
            Text(
                option.label,
                color = if (dimmed) Muted else Ink,
                fontSize = 13.sp, fontWeight = FontWeight.Medium,
            )
            option.description?.takeIf { it.isNotBlank() }?.let {
                Text(it, color = Secondary, fontSize = 11.sp)
            }
        }
    }
}

/**
 * The typed answer forms.
 *
 * `password` masks and gets its own keyboard hint; `file_path` says it takes
 * more than one, because the composer splits on commas and newlines and an
 * owner who cannot see that will only ever send one path.
 */
@Composable
internal fun EscalationTextAnswer(
    inputType: String?,
    value: String,
    onValue: (String) -> Unit,
    enabled: Boolean,
    onSubmit: () -> Unit,
    /**
     * How the field renders once the backend's sensitivity spec is applied
     * (`hitlRenderKind`): `otp` and `password` mask; the value posted still
     * follows [inputType]. Defaults to the widget type.
     */
    renderKind: String? = inputType,
    placeholder: String? = null,
) {
    val code = renderKind == "otp"
    val secret = code || renderKind == "password"
    MagicianTextField(
        value = value,
        onValueChange = onValue,
        enabled = enabled,
        singleLine = inputType != "file_path",
        maxLines = if (secret) 1 else 4,
        placeholder = {
            Text(
                placeholder ?: when (renderKind) {
                    "file_path" -> "Enter path(s), separated by commas"
                    "otp" -> "Enter the code…"
                    "password" -> "Enter securely…"
                    "guidance" -> "Add guidance…"
                    else -> "Type your response…"
                },
                color = Muted, fontSize = 13.sp,
            )
        },
        visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None,
        // A code is an exact string, not a number: some services issue
        // alphanumeric codes, so the digits-only keyboard would lock them out.
        keyboardOptions = when {
            secret -> KeyboardOptions(keyboardType = KeyboardType.Password)
            else -> KeyboardOptions.Default
        },
        textStyle = LocalTextStyle.current.copy(color = Ink, fontSize = 13.sp),
        modifier = Modifier.fillMaxWidth(),
    )
    EscalationSubmit(enabled = enabled && value.isNotBlank(), onClick = onSubmit)
}

@Composable
internal fun EscalationSubmit(
    enabled: Boolean,
    label: String = "Submit",
    onClick: () -> Unit,
) {
    Surface(
        color = if (enabled) Coral.copy(alpha = 0.14f) else Ground,
        shape = RoundedCornerShape(6.dp),
        modifier = Modifier
            .fillMaxWidth()
            .then(if (enabled) Modifier.clickable { onClick() } else Modifier),
    ) {
        Text(
            label,
            color = if (enabled) Coral else Muted,
            fontSize = 13.sp, fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(vertical = 9.dp),
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun AttachmentRow(filename: String, size: String?, fromUser: Boolean = false) {
    // A file the owner sent reads as theirs, the way their text does: accent
    // fill, accent-contrast label. Rendering both sides identically made an
    // attachment the one thing in the transcript with no visible sender.
    Surface(
        color = if (fromUser) Coral else Panel,
        shape = RoundedCornerShape(10.dp),
        border = if (fromUser) null else BorderStroke(1.dp, BorderSoft),
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                Icons.Outlined.AttachFile, contentDescription = null,
                tint = if (fromUser) OnAccent else Secondary,
                modifier = Modifier.size(16.dp),
            )
            Text(filename, color = if (fromUser) OnAccent else Ink, fontSize = 13.sp)
            size?.let {
                Text(
                    it,
                    color = if (fromUser) OnAccent.copy(alpha = 0.8f) else Muted,
                    fontSize = 11.sp,
                )
            }
        }
    }
}

/** A file a task or tool produced. */
@OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)
@Composable
private fun OutputRow(block: ContentBlockRecord, taskId: String? = null) {
    val context = LocalContext.current
    val artifactScope = rememberCoroutineScope()
    // A file a task produced was a label and nothing else — nothing on this
    // client could open one. The link builder already existed and matched iOS
    // exactly; it simply had no caller.
    val target = remember(block, taskId) {
        block.url?.takeIf { it.isNotBlank() }
            ?: taskId?.let { id ->
                block.filename?.takeIf { it.isNotBlank() }?.let { name ->
                    TaskArtifactLinks.url(MagicianAccess.baseUrl(context), id, name)
                }
            }
    }
    Row(
        Modifier.then(
            // Not every block resolves to something openable — a text block has
            // no file behind it — and a row that looks tappable and is not is
            // the promise this client keeps removing.
            if (target != null) {
                Modifier.combinedClickable(
                    onClick = {
                        artifactScope.launch {
                            AuthenticatedArtifacts.open(context, target, block.filename, block.mimeType)
                        }
                    },
                    onLongClick = {
                        artifactScope.launch {
                            AuthenticatedArtifacts.share(context, target, block.filename, block.mimeType)
                        }
                    },
                )
            } else {
                Modifier
            },
        ),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The kind, rather than one icon for everything. The mime type was
        // already decoded and never read, so a chart, a recording and a
        // spreadsheet all read as the same generic document.
        val kind = ai.magicbeans.magdroid.chat.ArtifactKind.from(block.mimeType, block.filename)
        Icon(
            kind.icon, contentDescription = kind.label,
            tint = if (target != null) Coral else Secondary, modifier = Modifier.size(14.dp),
        )
        Text(
            block.display(),
            color = if (target != null) Coral else Secondary,
            fontSize = 12.sp,
        )
    }
}

/**
 * One of the two controls flanking the mic.
 *
 * iOS builds both the same way and it is the shape that carries the meaning: a
 * primary segment you press, a hairline, and a chevron to the settings behind
 * it. Drawn as a bare icon — which is what stood here — they read as
 * decoration rather than as controls with more inside them.
 */
@Composable
private fun DockPill(
    active: Boolean,
    onChevron: () -> Unit,
    chevronLabel: String,
    dimmed: Boolean = false,
    content: @Composable () -> Unit,
) {
    val edge = if (active) Coral.copy(alpha = 0.4f) else Secondary.copy(alpha = 0.25f)
    val fill = if (active) Coral.copy(alpha = 0.12f) else Secondary.copy(alpha = 0.10f)
    Surface(
        color = fill,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, edge),
        modifier = Modifier
            .height(COMPOSER_VOICE_DOCK_HEIGHT_DP.dp)
            .alpha(if (dimmed) 0.55f else 1f),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            content()
            Box(
                Modifier
                    .width(1.dp)
                    .height(if (active) 16.dp else 14.dp)
                    .background(Secondary.copy(alpha = 0.22f)),
            )
            Box(
                Modifier
                    .width(COMPOSER_VOICE_DOCK_CHEVRON_WIDTH_DP.dp)
                    .fillMaxHeight()
                    .clickable { onChevron() },
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Outlined.KeyboardArrowDown,
                    contentDescription = chevronLabel,
                    tint = if (active) Coral else Secondary,
                    modifier = Modifier.size(13.dp),
                )
            }
        }
    }
}

/**
 * A system note — quieter than a bubble and shaped differently, so it never
 * reads as something the assistant said.
 */
@Composable
private fun SystemNote(text: String) {
    Surface(color = Ground, shape = RoundedCornerShape(16.dp)) {
        Text(
            text,
            color = Secondary, fontSize = 12.sp,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
    }
}

@Composable
private fun ToolIcon(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    tint: Color = Secondary,
    onClick: () -> Unit,
) {
    IconButton(onClick = onClick, modifier = Modifier.size(COMPOSER_TOOL_BUTTON_DP.dp)) {
        Icon(
            icon,
            contentDescription = label,
            tint = tint,
            modifier = Modifier.size(COMPOSER_TOOL_ICON_DP.dp),
        )
    }
}

/**
 * References offered while an `@` is still being typed.
 *
 * Each row leads with the reference's KIND as an accent-tinted badge, then its
 * label and detail — as iOS does. The badge matters: `@sam` could be an agent, a
 * file or a task, and picking the wrong kind produces a reference that resolves
 * to something the owner did not mean.
 *
 * It floats above the composer card rather than over the transcript: it belongs
 * to what is being written, and covering the conversation to finish a word is
 * the wrong trade.
 */
@Composable
private fun MentionPicker(
    items: List<MentionItem>,
    onPick: (MentionItem) -> Unit,
) {
    Surface(
        color = Panel,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, Muted.copy(alpha = 0.2f)),
        shadowElevation = 6.dp,
        modifier = Modifier.padding(horizontal = 10.dp),
    ) {
        Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
            items.forEach { item ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { onPick(item) }
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Surface(
                        color = Coral.copy(alpha = 0.15f),
                        shape = RoundedCornerShape(5.dp),
                    ) {
                        Text(
                            MentionCatalog.kindLabel(item.kind).uppercase(),
                            color = Coral,
                            fontSize = 9.sp,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(horizontal = 6.dp, vertical = 3.dp),
                        )
                    }
                    Column(Modifier.weight(1f)) {
                        Text(
                            item.label,
                            color = Ink, fontSize = 14.sp,
                            fontWeight = FontWeight.Medium, maxLines = 1,
                        )
                        item.detail?.takeIf { it.isNotBlank() }?.let {
                            Text(it, color = Secondary, fontSize = 11.sp, maxLines = 1)
                        }
                    }
                }
            }
        }
    }
}

/**
 * The empty transcript, as iOS composes it: a mark, the question, and a pill
 * pointing at the mic.
 *
 * Voice-forward on purpose — the pill names the primary way in rather than
 * leaving a new owner staring at a blank screen wondering what this is. A bare
 * "Ask for anything", which is what stood here, told them nothing about how.
 */
@Composable
private fun EmptyChat() {
    Column(
        Modifier.fillMaxSize().padding(top = 40.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("✦", color = Coral, fontSize = 40.sp)
        Text(
            "How can I help you today?",
            color = Ink, fontSize = 22.sp, fontWeight = FontWeight.Bold,
        )
        Surface(
            color = Panel,
            shape = RoundedCornerShape(50),
            border = BorderStroke(1.dp, BorderSoft),
            modifier = Modifier.padding(top = 20.dp),
        ) {
            Row(
                Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text("◉", color = Coral, fontSize = 14.sp)
                Text(
                    "Tap or hold to dictate",
                    color = Secondary, fontSize = 15.sp, fontWeight = FontWeight.Medium,
                )
            }
        }
    }
}

@Composable
private fun Centered(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(text, color = Muted, fontSize = 14.sp)
    }
}

@Composable
private fun ErrorPanel(state: ChatUiState) {
    Column(
        Modifier.fillMaxSize().padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            state.error.orEmpty(),
            color = Secondary, fontSize = 14.sp,
            textAlign = TextAlign.Center, lineHeight = 20.sp,
        )
        if (state.setupRequired) {
            Spacer(Modifier.height(10.dp))
            Text(
                "Open Setup and add your Magician host and Cloudflare Access credentials.",
                color = Muted, fontSize = 12.sp, textAlign = TextAlign.Center,
            )
        }
    }
}


/** Material's nearest glyph for each artifact kind. */
private val ai.magicbeans.magdroid.chat.ArtifactKind.icon: androidx.compose.ui.graphics.vector.ImageVector
    get() = when (this) {
        ai.magicbeans.magdroid.chat.ArtifactKind.Image -> Icons.Outlined.Image
        ai.magicbeans.magdroid.chat.ArtifactKind.Pdf -> Icons.Outlined.PictureAsPdf
        ai.magicbeans.magdroid.chat.ArtifactKind.Html -> Icons.Outlined.Code
        ai.magicbeans.magdroid.chat.ArtifactKind.Video -> Icons.Outlined.Movie
        ai.magicbeans.magdroid.chat.ArtifactKind.Audio -> Icons.Outlined.GraphicEq
        ai.magicbeans.magdroid.chat.ArtifactKind.Markdown,
        ai.magicbeans.magdroid.chat.ArtifactKind.Json,
        ai.magicbeans.magdroid.chat.ArtifactKind.Text -> Icons.Outlined.Description
        ai.magicbeans.magdroid.chat.ArtifactKind.Other -> Icons.AutoMirrored.Outlined.InsertDriveFile
    }
