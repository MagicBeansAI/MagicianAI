package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.chat.splitAnswerVerification
import ai.magicbeans.magdroid.chat.withoutAnswerVerification
import ai.magicbeans.magdroid.chat.OriginalAnswerLink
import ai.magicbeans.magdroid.chat.ChatMessage
import ai.magicbeans.magdroid.chat.ChatDateDividers
import ai.magicbeans.magdroid.chat.ChatComposerMode
import ai.magicbeans.magdroid.chat.ChatDoPermission
import ai.magicbeans.magdroid.chat.ChatProfile
import ai.magicbeans.magdroid.chat.thinkingLevelLabel
import ai.magicbeans.magdroid.chat.chatComposerThinkingLabel
import ai.magicbeans.magdroid.chat.modelChipAccessibilityLabel
import ai.magicbeans.magdroid.chat.ChatUiState
import ai.magicbeans.magdroid.chat.SessionSummary
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
import androidx.compose.foundation.LocalIndication
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import ai.magicbeans.magdroid.chat.MentionCatalog
import ai.magicbeans.magdroid.chat.ComposerCommandAction
import ai.magicbeans.magdroid.chat.ComposerCommandRow
import ai.magicbeans.magdroid.chat.ComposerCommandVoice
import ai.magicbeans.magdroid.chat.ComposerCommands
import ai.magicbeans.magdroid.chat.commandRows
import ai.magicbeans.magdroid.chat.MentionItem
import ai.magicbeans.magdroid.chat.ContentBlockRecord
import ai.magicbeans.magdroid.voice.DictationState
import ai.magicbeans.magdroid.voice.VoicePrefs
import ai.magicbeans.magdroid.voice.PrimaryAgentWakeIdentityStore
import ai.magicbeans.magdroid.voice.primaryAgentDisplayName
import ai.magicbeans.magdroid.voice.WakeService
import ai.magicbeans.magdroid.voice.LiveVoiceEngine
import ai.magicbeans.magdroid.preferences.HarnessCredentials
import ai.magicbeans.magdroid.voice.RealtimeVoiceState
import ai.magicbeans.magdroid.voice.liveMicLevel
import ai.magicbeans.magdroid.voice.livePlaybackLevel
import ai.magicbeans.magdroid.voice.withoutMicLevel
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.expandVertically
import androidx.compose.animation.core.tween
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.chat.EscalationCard
import ai.magicbeans.magdroid.chat.EscalationOption
import ai.magicbeans.magdroid.chat.MessageKind
import ai.magicbeans.magdroid.chat.TaskStatusCard
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.FlightTakeoff
import androidx.compose.material.icons.outlined.CalendarToday
import androidx.compose.material.icons.outlined.ShoppingBasket
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
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
import androidx.compose.material.icons.outlined.Headphones
import ai.magicbeans.magdroid.voice.ComposerVoiceMode
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material.icons.outlined.Screenshot
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.automirrored.outlined.VolumeOff
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Sensors
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.*
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.compose.ui.draw.clip
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.material.icons.outlined.Close
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
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
/** The fill behind [OnAccent] content; the bright [Coral] stays for tints and strokes. */
internal val AccentFill: Color get() = activePalette.accentFill
internal val AccentFillPressed: Color get() = activePalette.accentFillPressed
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
/**
 * Assistant bubbles are compact and share their leading edge and inner
 * inset (12) with the task cards; the owner's own bubbles keep more air
 * (iOS `plainTextMessageBubble`).
 */
internal const val CHAT_BUBBLE_ASSISTANT_PADDING_H_DP = 12
internal const val CHAT_BUBBLE_ASSISTANT_PADDING_V_DP = 9
internal const val CHAT_BUBBLE_USER_PADDING_H_DP = 14
internal const val CHAT_BUBBLE_USER_PADDING_V_DP = 12
/** The bubble radius is the lively token ([Lively.bubbleRadius]); no tails. */
internal const val CHAT_BUBBLE_CORNER_DP = 16
internal const val MESSAGE_SPEAK_LEADING_PADDING_DP = 0
internal const val MESSAGE_SPEAK_FONT_SP = 10
internal const val MESSAGE_SPEAK_VERTICAL_PADDING_DP = 3
/** The composer card's radius, [Lively.composerRadius] (10 on every platform). */
internal const val COMPOSER_CORNER_DP = 10

/** Space under the composer card: it sits just above the tab bar. */
internal const val COMPOSER_BOTTOM_GAP_DP = 2
internal const val COMPOSER_MODE_FONT_SP = 9
internal const val COMPOSER_MODE_MIN_HEIGHT_DP = 24
internal const val COMPOSER_MODE_HORIZONTAL_PADDING_DP = 7
internal const val COMPOSER_MODE_VERTICAL_PADDING_DP = 1
internal const val COMPOSER_TOOL_BUTTON_DP = 30
internal const val COMPOSER_TOOL_ICON_DP = 16
internal const val COMPOSER_PROFILE_VERTICAL_PADDING_DP = 2
internal const val COMPOSER_VOICE_DOCK_HEIGHT_DP = 32
internal const val COMPOSER_VOICE_DOCK_PRIMARY_WIDTH_DP = 31
internal const val COMPOSER_VOICE_DOCK_CHEVRON_WIDTH_DP = 22

/** The owner uses the accent; the assistant uses the theme's tinted surface. */
internal fun chatBubbleColor(fromUser: Boolean): Color = if (fromUser) AccentFill else Control

/** User and assistant messages share one clean, tail-free silhouette. */
internal fun chatBubbleShape(): Shape = Lively.bubbleShape

/** The iOS composer sits on the same theme surface as assistant replies. */
internal fun composerSurfaceColor(): Color = Control

internal fun composerModeTrackColor(): Color = Soft

/**
 * A selected mode segment is one accent rounded square with on-accent text,
 * whichever mode it is (iOS `ComposerView.modeToggle`); the mode itself is
 * carried by the label and the card's border.
 */
@Suppress("UNUSED_PARAMETER")
internal fun composerModeForeground(mode: ai.magicbeans.magdroid.chat.ChatComposerMode): Color {
    // The segment fills with AccentFill, which every palette pairs with a
    // readable OnAccent; the guard keeps a future palette from regressing.
    val fill = AccentFill.luminance()
    val text = OnAccent.luminance()
    val contrast = (maxOf(fill, text) + 0.05f) / (minOf(fill, text) + 0.05f)
    return if (contrast >= 3f) OnAccent else chatContrastingTextColor(AccentFill)
}

@Suppress("UNUSED_PARAMETER")
internal fun composerModeFill(mode: ai.magicbeans.magdroid.chat.ChatComposerMode): Color = AccentFill

/** Segments sit inside the 6 dp track, so their corner is a hair tighter: never capsules. */
internal const val COMPOSER_MODE_SEGMENT_CORNER_DP = 4.5f

/** Mirrors iOS for solid semantic fills such as danger, which has no on-color token. */
internal fun chatContrastingTextColor(color: Color): Color {
    val dark = Color(0xFF111111)
    val luminance = color.luminance()
    val lightContrast = 1.05f / (luminance + 0.05f)
    val darkContrast = (luminance + 0.05f) / (dark.luminance() + 0.05f)
    return if (darkContrast > lightContrast) dark else Color.White
}

/**
 * Voice origin belongs to the owner's turn, so it takes the accent. It is a
 * tint on the page, not a fill, so it keeps the bright accent rather than the
 * bubble's deeper [AccentFill].
 */
internal fun voiceOriginBadgeColor(): Color = Coral

private enum class MicPermissionAction { Dictation, WakeWord, Live, None }

@Composable
fun ChatScreen(
    viewModel: ChatViewModel = viewModel(),
    onOpenSettings: (() -> Unit)? = null,
    autoStartVoice: Boolean = false,
    onAutoStartVoiceConsumed: () -> Unit = {},
    onOpenAttention: (String?) -> Unit = {},
    onOpenTask: (String) -> Unit = {},
    /**
     * The task detail entry: task, run (when known), and whether to open on
     * its plan. Null until the host wires it; the card falls back to [onOpenTask].
     */
    onOpenTaskDetail: ((taskId: String, executionId: String?, focusPlan: Boolean) -> Unit)? = null,
    /** The Plan Inspector for a task's plan. Null falls back to the detail on its plan. */
    onOpenPlanInspector: ((taskId: String) -> Unit)? = null,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // The shell's detail push (LocalOpenTaskDetail) carries the run; onOpenTask
    // is the id-only fallback outside the shell.
    val shellTaskDetail = LocalOpenTaskDetail.current
    val onInspectTask: (String, String?) -> Unit = { taskId, executionId ->
        onOpenTaskDetail?.invoke(taskId, executionId, false)
            ?: shellTaskDetail?.open(taskId, executionId, false)
            ?: onOpenTask(taskId)
    }
    // Review and Plan graph open the Plan Inspector (its Graph tab carries
    // Reject · Approve · Approve & Run on a draft); without an inspector host,
    // the detail screen's Plan act.
    val openPlanInspector = LocalOpenPlanInspector.current
    val onReviewPlan: (String) -> Unit = { taskId ->
        val title = state.messages.lastOrNull { it.task?.taskId == taskId }?.task?.let {
            ai.magicbeans.magdroid.chat.TaskTitleRules.labelOrShortIdTitle(it.title, it.taskId)
        }
        onOpenPlanInspector?.invoke(taskId)
            ?: openPlanInspector?.invoke(
                ai.magicbeans.magdroid.tasks.PlanInspectorRoute(
                    taskId = taskId,
                    title = title,
                    tab = ai.magicbeans.magdroid.tasks.PlanInspectorTab.Graph,
                ),
            )
            ?: onOpenTaskDetail?.invoke(taskId, null, true)
            ?: shellTaskDetail?.open(taskId, null, true)
            ?: onOpenTask(taskId)
    }
    val listState = rememberLazyListState()
    val context = LocalContext.current
    // A slash command's confirmation (or the server's refusal), once.
    LaunchedEffect(state.commandNotice?.id) {
        val notice = state.commandNotice ?: return@LaunchedEffect
        android.widget.Toast.makeText(
            context, notice.text,
            if (notice.isError) android.widget.Toast.LENGTH_LONG else android.widget.Toast.LENGTH_SHORT,
        ).show()
        viewModel.consumeCommandNotice()
    }
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

    val attachmentPicker = rememberComposerAttachments(viewModel::stageAttachment)

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
    val voiceInputMode by voicePrefs.composerVoiceMode.collectAsStateWithLifecycle()
    // The call state without its ~16 Hz mic level, so the screen does not
    // recompose at the meter's rate; the level is read only where it is drawn.
    val realtimeFlow = viewModel.realtimeVoiceState
    val dictationLevel by viewModel.dictationLevel.collectAsStateWithLifecycle()
    val realtimeState by remember(realtimeFlow) { realtimeFlow.withoutMicLevel() }
        .collectAsStateWithLifecycle(initialValue = realtimeFlow.value.copy(micLevel = 0f))
    val liveMicLevel by remember(realtimeFlow) { realtimeFlow.liveMicLevel() }
        .collectAsStateWithLifecycle(initialValue = 0f)
    val livePlaybackLevel by remember(realtimeFlow) { realtimeFlow.livePlaybackLevel() }
        .collectAsStateWithLifecycle(initialValue = 0f)
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
            state.dictation == DictationState.Transcribing -> Unit
            voiceInputMode != ComposerVoiceMode.Dictation -> {
                voicePrefs.setLiveEngine(if (voiceInputMode == ComposerVoiceMode.Live) LiveVoiceEngine.Realtime else LiveVoiceEngine.HandsFree)
                if (micGranted.value) viewModel.startConfiguredVoice()
                else {
                    pendingMicAction = MicPermissionAction.Live
                    requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
                }
            }
            micGranted.value -> viewModel.startDictation()
            else -> {
                pendingMicAction = MicPermissionAction.Dictation
                requestMic.launch(android.Manifest.permission.RECORD_AUDIO)
            }
        }
    }
    val liveActive = realtimeState.active
    var suggestionFocusRequest by remember { mutableIntStateOf(0) }
    val composerVoiceMode = rememberComposerVoiceMode(liveActive, suggestionFocusRequest)
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

    // No fill of its own: the shell paints the theme wash behind the whole
    // chat (bars included), so the transcript and composer float on it.
    // The transcript's top edge: the background-requests list in the composer
    // sizes itself against the room between it and its own bottom edge.
    var composerRoomTop by remember { mutableStateOf<Float?>(null) }
    val outputLinks = rememberChatOutputHandler(androidx.compose.ui.platform.LocalUriHandler.current, state.activeSessionId)
    CompositionLocalProvider(LocalComposerRoomTop provides composerRoomTop, LocalChatOutputSession provides state.activeSessionId,
        androidx.compose.ui.platform.LocalUriHandler provides outputLinks) {
    Column(Modifier.fillMaxSize()) {
        Box(Modifier.weight(1f).onGloballyPositioned { composerRoomTop = it.boundsInWindow().top }) {
            Transcript(
                state, listState, viewModel::answerEscalation, Modifier.fillMaxSize(),
                onToggleSpeak = viewModel::toggleMessageSpeech,
                onRetry = viewModel::start,
                onOpenSettings = onOpenSettings,
                onOpenAttention = onOpenAttention,
                onOpenCompleteResult = viewModel::openCompleteResult,
                onOpenTask = onOpenTask,
                onLoadActivity = viewModel::loadActivityIfNeeded,
                onLoadOlder = viewModel::loadOlderMessages,
                onOpenOriginalAnswer = viewModel::openOriginalAnswer,
                onInspectTask = onInspectTask,
                onReviewPlan = onReviewPlan,
                onSuggest = { prompt ->
                    viewModel.onDraftChange(prompt)
                    if (liveActive) typingDuringCall = true
                    suggestionFocusRequest++
                },
                onOpenSession = { id ->
                    viewModel.cancelAutoSend()
                    viewModel.openSession(id)
                },
            )
            // Jump pill, as iOS has: only while scrolled away, and only when
            // there is something below to jump to.
            if (!atBottom && state.messages.isNotEmpty()) {
                Surface(
                    color = AccentFill,
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
        // Above the composer, web order: the ask dock, the planner dock, then
        // the mention list right over the card. An archived conversation is
        // read-only: no docks, and a band in place of the composer.
        val activeSession = state.selectedSession?.takeIf { it.identifier() == state.activeSessionId }
            ?: state.sessions.firstOrNull { it.identifier() == state.activeSessionId }
        val readOnly = ai.magicbeans.magdroid.chat.ChatReadOnly.isArchived(activeSession)
        // The dock also answers every open ask from the Attention feed, this
        // conversation's first — by thread, or by a task it started (Relentless
        // and quality asks carry only a task); the rest show where they came
        // from. Shared with a task detail pushed over chat.
        val askSurface: ai.magicbeans.magdroid.attention.AskSurfaceViewModel = viewModel()
        val askState by askSurface.state.collectAsStateWithLifecycle()
        FollowAskSurface(askSurface)
        val askThreadIds = setOfNotNull(
            state.activeSessionId,
            activeSession?.identifier(),
            activeSession?.id?.takeIf { it.isNotBlank() },
            // The default thread is shared by every untitled chat; matching on
            // it would put one chat's asks in all of them.
            activeSession?.uiThreadId?.takeIf { it.isNotBlank() && it != "general" },
        )
        // A named thread can hold several chats: an ask that names the chat
        // session raising it is here only in that session.
        val askSessionIds = setOfNotNull(
            state.activeSessionId,
            activeSession?.identifier(),
            activeSession?.id?.takeIf { it.isNotBlank() },
        )
        val askTaskIds = remember(state.messages) { ai.magicbeans.magdroid.chat.ComposerAskDock.taskIds(state.messages) }
        val feedAsks = remember(askState.feed, askState.settled, askThreadIds, askTaskIds, askSessionIds) {
            ai.magicbeans.magdroid.attention.AskSurfaces.forComposer(
                askState.feed, askThreadIds, askTaskIds, askState.settled, chatSessionIds = askSessionIds,
            )
        }
        AskSurfaceSheet(
            viewModel = askSurface,
            state = askState,
            owner = ASK_SHEET_DOCK,
            onOpenTask = { taskId -> onInspectTask(taskId, null) },
            onReviewPlan = onReviewPlan,
        )
        if (!readOnly) {
            ComposerAskDockView(
                messages = state.messages,
                enabled = !readOnly,
                onAnswer = { messageId, option, text, ids -> viewModel.answerEscalation(messageId, option, text, ids) },
                onOpenAttention = onOpenAttention,
                feedAsks = feedAsks.items,
                feedHere = feedAsks.here,
                originChatTitles = state.sessions.associate { it.identifier() to (it.title?.takeIf(String::isNotBlank) ?: "Chat ${it.identifier().take(8)}") },
                askState = askState,
                onAnswerFeed = askSurface::answer,
                loadAlignmentDetail = askSurface::alignmentDetail,
                onPlanChoice = askSurface::answerPlan,
                onOpenSheet = { item -> askSurface.openSheet(item, ASK_SHEET_DOCK) },
                onReviewPlan = onReviewPlan,
            )
            PlannerDockView(messages = state.messages, enabled = !readOnly, onReviewPlan = onReviewPlan)
            // The `/` command picker takes the mention picker's place.
            // `/voice-mode` reads the composer mic's mode and the same availability its menu uses.
            val commandRows = state.commandRows(
                ComposerCommandVoice(
                    mode = voiceInputMode,
                    liveVoiceAvailable = voiceCatalog.profiles.any { it.nativeAndAvailable },
                    handsFreeAvailable = voiceCatalog.handsFreeAvailable,
                ),
            )
            if (commandRows.isNotEmpty()) {
                CommandPicker(commandRows, viewModel::pickCommandRow)
            } else if (state.mentions.isNotEmpty()) {
                MentionPicker(state.mentions, viewModel::pickMention)
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
        val livePanelVisible = realtimeState.active && !typingDuringCall
        AnimatedVisibility(
            visible = livePanelVisible,
            enter = slideInVertically { it } + fadeIn(),
            exit = slideOutVertically { it } + fadeOut() + shrinkVertically(),
        ) {
            LiveVoicePanel(
                state = realtimeState,
                micLevel = { liveMicLevel },
                playbackLevel = { livePlaybackLevel },
                voiceQueue = { ChatComposerTopRows(state, viewModel, topCornerRadius = Lively.composerRadius) },
                onMute = viewModel::toggleLiveVoiceMute,
                onHoldStart = viewModel::engageLiveVoicePushToTalk,
                onHoldEnd = viewModel::releaseLiveVoicePushToTalk,
                onSetPushToTalk = viewModel::setLiveVoicePushToTalk,
                onEnd = onToggleLive,
                onTypeMessage = { typingDuringCall = true },
            )
        }
        if (livePanelVisible) {
            // The call panel above holds the composer's place while it shows.
        } else if (readOnly) {
            ArchivedComposerBand(onReturn = viewModel::returnToActive)
        } else {
            Composer(
                voiceModeState = composerVoiceMode,
                dictationLevel = { dictationLevel },
                state = state,
                voiceQueue = {
                    ChatComposerTopRows(state, viewModel, liveStrip = {
                        if (liveActive) LiveCallComposerStrip(realtimeState,
                            onOpen = { typingDuringCall = false }, onMute = viewModel::toggleLiveVoiceMute, onEnd = onToggleLive)
                    })
                },
                onBackground = viewModel::sendInBackground,
                onStopAndSend = viewModel::stopAndSend,
                onDraftChange = viewModel::onDraftChange,
                onSend = { viewModel.send() },
                onStop = viewModel::stop,
                onPickProfile = viewModel::selectProfile,
                onPickEngine = viewModel::selectHarnessEngine,
                onPickModel = viewModel::selectHarnessModel,
                onAttach = attachmentPicker.files,
                onCamera = attachmentPicker.photos,
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
                liveVoiceAvailable = voiceCatalog.profiles.any { it.nativeAndAvailable },
                handsFreeAvailable = voiceCatalog.handsFreeAvailable,
                onToggleLive = onToggleLive,
                onRemoveAttachment = viewModel::removeAttachment,
                onComposerMode = viewModel::setComposerMode,
                focusRequest = suggestionFocusRequest,
            )
        }
    }
    }
}

/**
 * The rows inside the composer card, above the input, in web order: queue →
 * its error → concurrent voice → concurrent origin → banners (live call,
 * auto-send) → the plan-mode band. Attachments, input and tools follow in
 * the card itself.
 */
@Composable
private fun ChatComposerTopRows(
    state: ChatUiState,
    viewModel: ChatViewModel,
    topCornerRadius: androidx.compose.ui.unit.Dp = COMPOSER_CORNER_DP.dp,
    liveStrip: @Composable () -> Unit = {},
) {
    val background by viewModel.concurrentVoice.state.collectAsStateWithLifecycle()
    val hasQueue = state.queueSessionId == state.activeSessionId && state.queuedMessages.isNotEmpty()
    val metadata = state.selectedSession?.takeIf { it.identifier() == state.activeSessionId }
        ?: state.sessions.firstOrNull { it.identifier() == state.activeSessionId }
    ChatQueueStrip(
        state,
        viewModel::actOnQueue,
        topCornerRadius,
        onClearAll = viewModel::clearQueued,
        onStopTurn = viewModel::stop,
    )
    state.queueError?.let { QueueErrorBand(it, viewModel::dismissQueueError) }
    if (state.selectedHarnessEngine in setOf("magician", "pi")) {
        state.modelSelectionIssue?.let {
            Text(it, color = Danger, fontSize = 11.sp, lineHeight = 15.sp,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp))
        }
    }
    ConcurrentVoiceStrip(viewModel, topCornerRadius = if (hasQueue) 0.dp else topCornerRadius)
    metadata?.internalVoice?.parentSessionId?.let { parent ->
        ConcurrentOriginStrip(
            topCornerRadius = if (hasQueue || background.available.isNotEmpty()) 0.dp else topCornerRadius,
            onOpenParent = { viewModel.openSession(parent) },
        )
    }
    liveStrip()
    // A dictated draft sends itself in three seconds. Visible and cancelable,
    // because a transcript is often nearly right and the moment to fix it is
    // before it goes.
    AutoSendBand(state.autoSendIn, viewModel::cancelAutoSend)
    if (state.composerMode == ChatComposerMode.Plan) PlanModeBand()
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
internal fun readPicked(
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
        resolver.openInputStream(uri)?.use { input ->
            val output = java.io.ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                if (output.size() + count > 20 * 1024 * 1024) return null
                output.write(buffer, 0, count)
            }
            output.toByteArray()
        }
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
    onLoadOlder: () -> Unit = {},
    onOpenOriginalAnswer: (OriginalAnswerLink) -> Unit = {},
    onInspectTask: (String, String?) -> Unit = { taskId, _ -> onOpenTask(taskId) },
    onReviewPlan: (String) -> Unit = onOpenTask,
    onSuggest: (String) -> Unit = {},
    onOpenSession: (String) -> Unit = {},
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
            state.messages.isEmpty() -> {
                val session = state.selectedSession?.takeIf { it.identifier() == state.activeSessionId }
                    ?: state.sessions.firstOrNull { it.identifier() == state.activeSessionId }
                EmptyChat(
                    readOnly = ai.magicbeans.magdroid.chat.ChatReadOnly.isArchived(session),
                    suggestionsEnabled = state.dictation != DictationState.Recording && state.dictation != DictationState.Transcribing,
                    recentSessions = state.sessions.filter {
                        it.identifier().isNotBlank() && it.identifier() != state.activeSessionId && it.internalVoice == null
                    }.distinctBy { it.identifier() }.sortedByDescending { it.updatedAt ?: it.createdAt ?: 0 }.take(3),
                    onSuggest = onSuggest,
                    onOpenSession = onOpenSession,
                )
            }
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
                val timestamps = remember(state.messages) { state.messages.map { it.createdAtMs } }
                val dayLabels = remember(timestamps) { ChatDateDividers.labels(timestamps) }
                val messageIndexById = remember(state.messages) {
                    state.messages.withIndex().associate { (index, message) -> message.id to index }
                }
                val showOlderRow = state.historyHasMore || state.olderHistoryLoading || state.olderHistoryError != null
                // Each message's position among the list's items, in the order
                // they are emitted below: the older-history row, then each
                // message behind its day divider.
                val lazyIndexById = remember(state.messages, dayLabels, showOlderRow) {
                    var next = if (showOlderRow) 1 else 0
                    buildMap {
                        state.messages.forEachIndexed { index, message ->
                            if (dayLabels[index] != null) next++
                            put(message.id, next++)
                        }
                    }
                }
                OlderHistoryPaging(state, listState, messageIndexById, lazyIndexById, onLoadOlder)
                Box(Modifier.fillMaxSize()) {
                LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    if (showOlderRow) {
                        item(key = OLDER_HISTORY_ROW_KEY, contentType = "older-history") {
                            OlderHistoryRow(
                                loading = state.olderHistoryLoading,
                                error = state.olderHistoryError,
                                onLoad = onLoadOlder,
                            )
                        }
                    }
                    state.messages.forEachIndexed { index, message ->
                        dayLabels[index]?.let { label ->
                            item(key = "${DAY_DIVIDER_KEY_PREFIX}${message.id}", contentType = "day-divider") {
                                ChatDayDivider(label)
                            }
                        }
                        item(key = message.id, contentType = message.kind) {
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
                            sessionId = state.activeSessionId,
                            onInspectTask = onInspectTask,
                            onReviewPlan = onReviewPlan,
                        )
                        }
                    }
                }
                FloatingDatePill(
                    listState = listState,
                    timestamps = timestamps,
                    messageIndexById = messageIndexById,
                    modifier = Modifier.align(Alignment.TopCenter).padding(top = 8.dp),
                )
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
    sessionId: String? = null,
    onInspectTask: (String, String?) -> Unit = { taskId, _ -> onOpenTask(taskId) },
    onReviewPlan: (String) -> Unit = onOpenTask,
) {
    val alignment = if (message.fromUser) Alignment.End else Alignment.Start
    val answerDisplay = remember(message.text, message.fromUser) {
        if (message.fromUser) ai.magicbeans.magdroid.chat.AnswerVerificationDisplay(message.text, null)
        else splitAnswerVerification(message.text)
    }
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
                message.task?.let {
                    LiveTaskCard(
                        messageId = message.id,
                        task = it,
                        sessionId = sessionId ?: it.uiThreadId,
                        onInspect = onInspectTask,
                        onReviewPlan = onReviewPlan,
                    )
                }
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
            // A stopped turn's "Stopped" line keeps the steps it ran.
            if (!message.fromUser && message.activityRows.isNotEmpty()) {
                ActivitySection(
                    message.activityRows,
                    onOpenAttention = { onOpenAttention(null) },
                    onOpenCompleteResult = onOpenCompleteResult,
                    sessionId = sessionId,
                    onInspectRun = { taskId, executionId -> onInspectTask(taskId, executionId) },
                )
            }
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
                Modifier.padding(
                    horizontal = (if (message.fromUser) CHAT_BUBBLE_USER_PADDING_H_DP else CHAT_BUBBLE_ASSISTANT_PADDING_H_DP).dp,
                    vertical = (if (message.fromUser) CHAT_BUBBLE_USER_PADDING_V_DP else CHAT_BUBBLE_ASSISTANT_PADDING_V_DP).dp,
                ),
            ) {
                if (structured != null) {
                    // The backend composed this answer as blocks. Rendering the
                    // flat text instead would collapse a table into a paragraph.
                    StructuredResponseView(if (answerDisplay.verification != null) withoutAnswerVerification(structured) else structured)
                } else {
                    MarkdownText(
                        markdown = answerDisplay.body.ifEmpty { if (message.streaming) "…" else "" },
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
                sessionId = sessionId,
                onInspectRun = { taskId, executionId -> onInspectTask(taskId, executionId) },
            )
            if (message.text.isNotBlank() && !message.streaming) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    MessageSpeakButton(active = speaking, onToggle = onToggleSpeak)
                    answerDisplay.verification?.let { AnswerVerificationInfo(it) }
                }
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
        shape = Lively.chipShape,
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

/** Owned above the call/composer switch, so hiding the composer cannot reset it. */
@Composable
internal fun rememberComposerVoiceMode(liveActive: Boolean, focusRequest: Int = 0): MutableState<Boolean> {
    val mode = rememberSaveable { mutableStateOf(true) }
    var beforeCall by rememberSaveable { mutableStateOf<Boolean?>(null) }
    var handledFocusRequest by rememberSaveable { mutableIntStateOf(0) }
    LaunchedEffect(liveActive) {
        if (liveActive) {
            if (beforeCall == null) beforeCall = mode.value
        } else {
            beforeCall?.let { mode.value = it }
            beforeCall = null
        }
    }
    LaunchedEffect(focusRequest) {
        if (focusRequest > 0 && focusRequest != handledFocusRequest) mode.value = false
        handledFocusRequest = focusRequest
    }
    return mode
}

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
internal fun Composer(
    voiceModeState: MutableState<Boolean>,
    dictationLevel: () -> Float = { 0f },
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
    liveVoiceAvailable: Boolean = true,
    handsFreeAvailable: Boolean = true,
    onToggleLive: () -> Unit,
    onRemoveAttachment: (String) -> Unit,
    onComposerMode: (ai.magicbeans.magdroid.chat.ChatComposerMode) -> Unit,
    topPanel: (@Composable () -> Unit)? = null,
    modeControl: (@Composable () -> Unit)? = null,
    modelControl: (@Composable () -> Unit)? = null,
    inputPlaceholder: String? = null,
    showChatGodMode: Boolean = true,
    holdToSend: Boolean = true,
    focusRequest: Int = 0,
) {
    val composerMode = state.composerMode
    var privilegeLabel by remember(state.activeSessionId) { mutableStateOf<String?>(null) }
    val context = LocalContext.current
    val voicePrefs = remember(context) { VoicePrefs.get(context) }
    val voiceInputMode by voicePrefs.composerVoiceMode.collectAsStateWithLifecycle()
    remember(context) { PrimaryAgentWakeIdentityStore.initialize(context) }
    val primaryAgent by PrimaryAgentWakeIdentityStore.identity.collectAsStateWithLifecycle()
    val assistantName = primaryAgentDisplayName(primaryAgent)
    val identityScope = rememberCoroutineScope()
    androidx.lifecycle.compose.LifecycleResumeEffect(context) {
        val refresh = identityScope.launch { PrimaryAgentWakeIdentityStore.refresh(context) }
        onPauseOrDispose { refresh.cancel() }
    }
    var voiceMode by voiceModeState
    val inputFocus = remember { FocusRequester() }
    val keyboard = LocalSoftwareKeyboardController.current
    LaunchedEffect(voiceMode, focusRequest) {
        if (!voiceMode) {
            androidx.compose.runtime.withFrameNanos { }
            inputFocus.requestFocus()
            keyboard?.show()
        }
    }
    var profileMenu by remember { mutableStateOf(false) }
    val hasModelCatalog = state.profiles.any { it.modelChoice != null }
    if (profileMenu && hasModelCatalog) {
        CatalogModelSheet(state, onPickProfile, onPickEngine, onPickModel) { profileMenu = false }
    }
    val acting = composerMode != ai.magicbeans.magdroid.chat.ChatComposerMode.Ask

    // No band behind the card: the chat's wash shows around it, and the card's
    // own border carries the mode (iOS `ComposerView`).
    ComposerCard(accented = acting, privilegeLabel = privilegeLabel) {
        if (topPanel != null) Column(Modifier.padding(start = 12.dp, end = 12.dp, top = 8.dp, bottom = 6.dp)) { topPanel() }
        voiceQueue()
        Column(
            Modifier.padding(start = 12.dp, end = 12.dp, top = 9.dp, bottom = 6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (state.attachments.isNotEmpty()) AttachmentStrip(state, onRemoveAttachment)
            AnimatedVisibility(
                visible = state.dictation == DictationState.Recording,
                enter = fadeIn(tween(160)) + expandVertically(tween(240)),
                exit = fadeOut(tween(120)) + shrinkVertically(tween(220)),
            ) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
                    Text("Listening", color = Secondary, fontSize = 12.sp, modifier = Modifier.padding(end = 10.dp))
                    VoiceActivityWaveform(active = true, muted = false, animate = rememberAnimatorDurationScale() > 0f,
                        micLevel = dictationLevel, playbackLevel = { 0f })
                }
            }

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
                    inputMode = voiceInputMode,
                    onSelectInputMode = voicePrefs::setComposerVoiceMode,
                    liveVoiceAvailable = liveVoiceAvailable,
                    handsFreeAvailable = handsFreeAvailable,
                    onToggleLive = onToggleLive,
                    onKeyboard = { voiceMode = false }, holdToSend = holdToSend,
                )
            } else {
            Row(verticalAlignment = Alignment.Bottom) {
                BasicComposerField(
                    value = state.draft,
                    placeholder = inputPlaceholder ?: when (composerMode) {
                        ai.magicbeans.magdroid.chat.ChatComposerMode.Plan -> "Plan with $assistantName…"
                        ai.magicbeans.magdroid.chat.ChatComposerMode.AcceptInScope ->
                            "Accept in-scope edits…"
                        ai.magicbeans.magdroid.chat.ChatComposerMode.Ask -> "Ask $assistantName…"
                    },
                    onValueChange = onDraftChange,
                    modifier = Modifier.weight(1f).focusRequester(inputFocus),
                )
                Spacer(Modifier.width(4.dp))
                ComposerKeyboardDismiss()
                if (state.draft.isNotBlank() && state.composerMode != ChatComposerMode.Plan) {
                    ComposerSendOptions(state, onSend, onStopAndSend, onBackground)
                }
                TrailingSlot(
                    state = state,
                    onSend = onSend,
                    onStop = onStop,
                    onVoice = { voiceMode = true },
                    liveActive = liveActive,
                    inputMode = voiceInputMode,
                )
            }
            }

            // Keep this small tool row composed while it scrolls, so God-mode
            // status and the composer's aura remain live even offscreen.
            Row(
                modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                key("composer-tool-1") {
                    if (modeControl != null) modeControl() else ModeToggle(
                        mode = composerMode,
                        permission = state.composerDoPermission,
                        onChange = onComposerMode,
                    )
                }
                key("composer-tool-2") {
                    Box(
                        Modifier
                            .height(16.dp)
                            .width(1.dp)
                            .background(Muted.copy(alpha = 0.3f)),
                    )
                }
                key("composer-tool-3") { ComposerExecutionModes(state.activeSessionId, showChatGodMode) { privilegeLabel = it } }
                key("composer-tool-4") { ToolIcon(Icons.Outlined.AttachFile, "Attach a file", onClick = onAttach) }
                key("composer-tool-5") { ToolIcon(Icons.Outlined.PhotoCamera, "Add a photo", onClick = onCamera) }
                // Ask about the screen itself. `teachThisScreen` was written
                // for this and never given a way in: the quick-settings tile
                // and the assist gesture both reach the tutor from outside
                // the app, and inside it there was no way to ask at all.
                //
                // Android-only by capability rather than by choice — iOS
                // cannot read another app's screen from inside itself, which
                // is why its version is a share-sheet extension.
                if (onTeachScreen != null) {
                    key("composer-tool-6") {
                        ToolIcon(
                            Icons.Outlined.Screenshot,
                            "Explain my screen",
                            onClick = onTeachScreen,
                        )
                    }
                }
                key("composer-tool-7") {
                    Box {
                        if (modelControl != null) modelControl() else ProfileChip(state) { profileMenu = true }
                        DropdownMenu(profileMenu && !hasModelCatalog, onDismissRequest = { profileMenu = false }) {
                            Text("ENGINE", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp), color = Muted)
                            state.chatHarnesses.forEach { engine ->
                                DropdownMenuItem(
                                    text = { Text("${HarnessCredentials.engineName(engine.name, engine.name.replace('_', ' '), engine.credentials)}${if (engine.name == state.selectedHarnessEngine) " ✓" else ""}") },
                                    onClick = { onPickEngine(engine.name) },
                                )
                            }
                            HorizontalDivider()
                            if (state.selectedHarnessEngine == "magician" || state.selectedHarnessEngine == "pi") {
                                Text("Model choices are unavailable.", modifier = Modifier.padding(16.dp), color = Muted)
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

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun CatalogModelSheet(
    state: ChatUiState,
    onPickProfile: (String) -> Unit,
    onPickEngine: (String) -> Unit,
    onPickModel: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val models = state.profiles.mapNotNull { it.modelChoice }.distinctBy { it.id }
    val selection = state.profiles.firstOrNull { it.name == state.selectedProfile }?.modelSelection
        ?: state.profiles.firstOrNull { it.isDefault }?.modelSelection
    val usesCatalog = state.selectedHarnessEngine in listOf("magician", "pi")
    ComposerModelPickerSheet(
        selectedEngine = state.selectedHarnessEngine,
        engines = state.chatHarnesses.map { CatalogChoiceOption(it.name, catalogEngineLabel(it.name, it.credentials), enabled = it.installed) },
        onPickEngine = onPickEngine, onDismiss = onDismiss,
    ) {
        if (usesCatalog) ComposerModelCatalogControls(models, selection, onPickProfile)
        else {
            Text("Model", color = Muted, fontSize = 12.sp)
            CatalogChoiceDropdown("Model", state.selectedHarnessModel,
                (state.chatHarnesses.firstOrNull { it.name == state.selectedHarnessEngine }?.models ?: listOf("default"))
                    .map { CatalogChoiceOption(it, if (it == "default") "Engine default" else it) }, onSelect = onPickModel)
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
internal fun ComposerModelPickerSheet(
    selectedEngine: String,
    engines: List<CatalogChoiceOption>,
    onPickEngine: (String) -> Unit,
    onDismiss: () -> Unit,
    content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = Ground,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Model and thinking", color = Ink, fontSize = 18.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                TextButton(onClick = onDismiss) { Text("Done", color = Coral) }
            }
            Text("Engine", color = Muted, fontSize = 12.sp)
            CatalogChoiceDropdown("Engine", selectedEngine, engines, onSelect = onPickEngine)
            content()
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
internal fun ComposerModelCatalogControls(
    models: List<ai.magicbeans.magdroid.chat.ChatModelChoice>,
    selection: ai.magicbeans.magdroid.chat.ChatModelSelection?,
    onPickRoute: (String) -> Unit,
) {
    val active = models.firstOrNull { it.id == selection?.modelId }
    Text("Model", color = Muted, fontSize = 12.sp)
    CatalogChoiceDropdown("Model", active?.id.orEmpty(), models.map { CatalogChoiceOption(it.id, it.label, it.providerDisplayName, enabled = it.installed) }) { id ->
        models.firstOrNull { it.id == id }?.let { model ->
            val level = selection?.thinking?.takeIf { model.choices.containsKey(it) } ?: "auto"
            model.choices[level]?.let(onPickRoute)
        }
    }
    active?.let { model ->
        Text(model.providerDisplayName + (model.limits?.let { " · ${it.contextWindowTokens / 1000}K context" } ?: ""), color = Muted, fontSize = 12.sp)
        ModelThinkingControl(model.thinking, selection?.thinking ?: "auto") { level -> model.choices[level]?.let(onPickRoute) }
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

    val segmentShape = RoundedCornerShape(COMPOSER_MODE_SEGMENT_CORNER_DP.dp)
    Surface(color = composerModeTrackColor(), shape = Lively.chipShape) {
        Row(Modifier.padding(1.dp)) {
            Box {
                // Selected, the face and its caret are ONE accent rounded
                // square (one fill, no box per half).
                Row(
                    Modifier
                        .height(IntrinsicSize.Min)
                        .background(doFill, segmentShape),
                ) {
                    Surface(
                        color = Color.Transparent,
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
                        color = Color.Transparent,
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
            ComposerModeSegment("PLAN", planSelected) { onChange(ChatComposerMode.Plan) }
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
    val engineName = state.selectedHarnessEngine.replace('_', ' ')
    val selectionName = if (usesProfile) active?.compactDisplay() ?: "Default" else state.selectedHarnessModel
    // Web parity (`ChatPanel` `profileThinkingLabel`): the chip carries the
    // selected thinking level for a catalog model that has thinking.
    val selection = active?.modelSelection
    val model = state.profiles.mapNotNull { it.modelChoice }.firstOrNull { it.id == selection?.modelId }
    val thinking = chatComposerThinkingLabel(state.selectedHarnessEngine, model, selection)
    ComposerSelectorChip(
        label = "$engineName · $selectionName",
        onClick = onClick,
        isDefault = isDefault,
        thinking = thinking,
        accessibilityLabel = modelChipAccessibilityLabel("Chat engine: $engineName, selection: $selectionName", thinking),
    )
}

@Composable
internal fun ComposerSelectorChip(
    label: String,
    onClick: (() -> Unit)? = null,
    icon: androidx.compose.ui.graphics.vector.ImageVector = Icons.Outlined.AutoAwesome,
    isDefault: Boolean = true,
    maxWidth: androidx.compose.ui.unit.Dp = 180.dp,
    /** Selected thinking level tag (web `pp-badge`), e.g. "Auto" or "High"; null hides it. */
    thinking: String? = null,
    /** Replaces the chip's merged text for screen readers when set. */
    accessibilityLabel: String? = null,
) {
    val tap = if (onClick != null) Modifier.clickable(role = androidx.compose.ui.semantics.Role.Button, onClick = onClick) else Modifier
    Surface(
        color = composerSurfaceColor(),
        shape = Lively.chipShape,
        border = BorderStroke(
            1.dp,
            if (isDefault) Muted.copy(alpha = 0.25f) else Coral.copy(alpha = 0.5f),
        ),
        modifier = if (accessibilityLabel != null) tap.semantics { contentDescription = accessibilityLabel } else tap,
    ) {
        Row(
            // With an explicit label the visible texts stay out of the merged node.
            (if (accessibilityLabel != null) Modifier.clearAndSetSemantics { } else Modifier).padding(
                horizontal = 8.dp,
                vertical = COMPOSER_PROFILE_VERTICAL_PADDING_DP.dp,
            ),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Icon(
                icon,
                contentDescription = null,
                tint = if (isDefault) Ink else Coral,
                modifier = Modifier.size(12.dp),
            )
            Text(
                label,
                color = Ink, fontSize = 10.sp,
                fontFamily = LocalMagicanFontFamilies.current.mono,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.widthIn(max = maxWidth),
            )
            if (thinking != null) Text(
                thinking.uppercase(),
                color = Coral, fontSize = 8.sp, fontWeight = FontWeight.SemiBold,
                letterSpacing = 0.3.sp, maxLines = 1,
                modifier = Modifier
                    .background(Coral.copy(alpha = 0.12f), RoundedCornerShape(3.dp))
                    .border(1.dp, Coral.copy(alpha = 0.28f), RoundedCornerShape(3.dp))
                    .padding(horizontal = 4.dp),
            )
            if (onClick != null) Icon(
                Icons.Outlined.UnfoldMore,
                contentDescription = null,
                tint = Muted,
                modifier = Modifier.size(12.dp),
            )
        }
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
    inputMode: ComposerVoiceMode = ComposerVoiceMode.Dictation,
) {
    val hasText = state.draft.isNotBlank() || state.attachments.any { it.remoteId != null }
    val stopping = (state.sending || state.serverRunning) && !hasText
    val interaction = remember { MutableInteractionSource() }
    val pressed by interaction.collectIsPressedAsState()
    if (liveActive && !hasText && !stopping) return
    val fill = when {
        stopping -> Danger
        pressed -> AccentFillPressed
        else -> AccentFill
    }
    Surface(
        color = fill,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier
            .size(36.dp)
            .clickable(
                interactionSource = interaction,
                indication = LocalIndication.current,
                enabled = !state.queueMutationInFlight,
            ) {
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
                    when (inputMode) {
                        ComposerVoiceMode.Dictation -> Icons.Filled.Mic
                        ComposerVoiceMode.Live -> Icons.Outlined.GraphicEq
                        ComposerVoiceMode.HandsFree -> Icons.Outlined.Headphones
                    },
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
internal fun VoiceHero(
    state: DictationState,
    partial: String,
    onMic: () -> Unit,
    onHoldStart: () -> Unit,
    onHoldEnd: () -> Unit,
    speakReplies: Boolean = false,
    onToggleSpeak: () -> Unit = {},
    onOpenReplySettings: () -> Unit = {},
    onOpenLiveSettings: () -> Unit = {},
    liveActive: Boolean = false,
    inputMode: ComposerVoiceMode = ComposerVoiceMode.Dictation,
    onSelectInputMode: (ComposerVoiceMode) -> Unit = {},
    liveVoiceAvailable: Boolean = true,
    handsFreeAvailable: Boolean = true,
    onToggleLive: () -> Unit = {},
    onKeyboard: () -> Unit,
    holdToSend: Boolean = true,
    sideControls: (@Composable androidx.compose.foundation.layout.RowScope.() -> Unit)? = null,
) {
    val recording = state == DictationState.Recording
    val transcribing = state == DictationState.Transcribing
    val available = when (inputMode) {
        ComposerVoiceMode.Dictation -> true
        ComposerVoiceMode.Live -> liveVoiceAvailable
        ComposerVoiceMode.HandsFree -> handsFreeAvailable
    }
    val modeIcon = when (inputMode) {
        ComposerVoiceMode.Dictation -> Icons.Filled.Mic
        ComposerVoiceMode.Live -> Icons.Outlined.GraphicEq
        ComposerVoiceMode.HandsFree -> Icons.Outlined.Headphones
    }
    var modeMenu by remember { mutableStateOf(false) }
    // A held capture ends on release; a tapped one ends on the next tap. The
    // hint has to say which, or the gesture that works is guesswork.
    var holding by remember { mutableStateOf(false) }
    val currentRecording by rememberUpdatedState(recording)
    val currentTranscribing by rememberUpdatedState(transcribing)
    val currentOnMic by rememberUpdatedState(onMic)
    val currentOnHoldStart by rememberUpdatedState(onHoldStart)
    val currentOnHoldEnd by rememberUpdatedState(onHoldEnd)
    val currentMode by rememberUpdatedState(inputMode)
    val currentAvailable by rememberUpdatedState(available)
    Column(
        Modifier.fillMaxWidth().padding(vertical = 2.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        // What was heard so far, while it is being heard. Showing it is the
        // difference between dictation you can trust and dictation you have to
        // re-read afterwards.
        AnimatedVisibility(
            visible = recording && partial.isNotBlank(),
            enter = fadeIn(tween(160)) + expandVertically(tween(240)),
            exit = fadeOut(tween(120)) + shrinkVertically(tween(220)),
        ) {
            Text(
                partial,
                color = Ink, fontSize = 15.sp,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                maxLines = 3, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        if (transcribing) {
            Text("Transcribing…", color = Coral, fontSize = 13.sp, fontWeight = FontWeight.Medium)
        }

        // The mic sits centred with the flanking controls overlaid rather than
        // laid out beside it, so their differing widths cannot shift it
        // off-centre — the same reason iOS uses a ZStack here.
        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            // Recording turns the mic to the danger colour, because while it
            // is red the room is being listened to and that should never be
            // something you have to infer.
            val micFill = if (recording) Danger else AccentFill
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
                            onTap = { if (!currentTranscribing && (currentAvailable || currentRecording)) currentOnMic() },
                            onLongPress = {
                                if (!currentRecording && !currentTranscribing && currentAvailable) {
                                    if (currentMode == ComposerVoiceMode.Dictation) {
                                        holding = true
                                        currentOnHoldStart()
                                    } else currentOnMic()
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
                            if (recording) Icons.Filled.Stop else modeIcon,
                            contentDescription = if (recording) "Stop dictation" else if (inputMode == ComposerVoiceMode.Dictation) "Talk" else "Start ${inputMode.label}",
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
                if (sideControls != null) sideControls() else {
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

                Box {
                    DockPill(active = inputMode != ComposerVoiceMode.Dictation,
                        onChevron = { if (!recording && !transcribing) modeMenu = true },
                        chevronLabel = "Choose voice mode or settings") {
                        Row(Modifier.fillMaxHeight().clickable(enabled = !recording && !transcribing) { modeMenu = true }
                            .padding(start = 7.dp, end = 5.dp),
                            horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                            Icon(modeIcon, null, tint = Secondary, modifier = Modifier.size(12.dp))
                            Text(inputMode.label, color = Secondary, fontSize = 10.sp, fontWeight = FontWeight.SemiBold)
                        }
                    }
                    DropdownMenu(modeMenu, onDismissRequest = { modeMenu = false }, containerColor = Panel) {
                        ComposerVoiceMode.entries.forEach { mode ->
                            val ready = when (mode) {
                                ComposerVoiceMode.Dictation -> true
                                ComposerVoiceMode.Live -> liveVoiceAvailable
                                ComposerVoiceMode.HandsFree -> handsFreeAvailable
                            }
                            DropdownMenuItem(text = { Text(mode.label + if (inputMode == mode) " ✓" else "", color = Ink) },
                                enabled = ready && !recording && !transcribing,
                                onClick = { modeMenu = false; onSelectInputMode(mode) })
                        }
                        HorizontalDivider()
                        DropdownMenuItem(text = { Text("Voice settings…", color = Ink) },
                            onClick = { modeMenu = false; onOpenLiveSettings() })
                    }
                }
                }
            }
        }
        // Both gestures named, and the way out to typing, because a mic with no
        // caption leaves the keyboard undiscoverable.
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                when {
                    recording || transcribing || holding -> if (holding && !holdToSend) "Listening — release to finish" else dictationHint(state, holding)
                    !available -> "${inputMode.label} is unavailable"
                    inputMode != ComposerVoiceMode.Dictation -> "Tap to start ${inputMode.label}"
                    else -> dictationHint(state, holding)
                },
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
internal fun BasicComposerField(
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
            keyboardOptions = KeyboardOptions(capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.Sentences, keyboardType = KeyboardType.Text),
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
            // An image shows itself (iOS: 54 dp, 6 dp radius); other files a tile.
            val thumbnail = remember(file.localId, file.preview) {
                file.preview?.let { bytes ->
                    runCatching {
                        val bounds = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
                        android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
                        var sample = 1
                        while (bounds.outWidth / (sample * 2) >= 162) sample *= 2
                        android.graphics.BitmapFactory.decodeByteArray(
                            bytes, 0, bytes.size,
                            android.graphics.BitmapFactory.Options().apply { inSampleSize = sample },
                        )?.asImageBitmap()
                    }.getOrNull()
                }
            }
            Box(Modifier.size(62.dp), contentAlignment = Alignment.TopEnd) {
                Surface(
                    color = Ground,
                    shape = RoundedCornerShape(ATTACHMENT_THUMB_CORNER_DP.dp),
                    border = BorderStroke(1.dp, Muted.copy(alpha = 0.2f)),
                    modifier = Modifier.size(ATTACHMENT_THUMB_DP.dp).align(Alignment.BottomStart),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        if (thumbnail != null) {
                            androidx.compose.foundation.Image(
                                thumbnail,
                                contentDescription = file.name,
                                contentScale = androidx.compose.ui.layout.ContentScale.Crop,
                                modifier = Modifier.matchParentSize(),
                            )
                        } else {
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
                        }
                        // State sits over the tile, as on iOS, so the file stays
                        // recognisable underneath whatever is happening to it.
                        if (file.uploading || file.failed) {
                            Box(
                                Modifier
                                    .matchParentSize()
                                    .background(Ground.copy(alpha = 0.72f)),
                                contentAlignment = Alignment.Center,
                            ) {
                                if (file.failed) {
                                    Icon(
                                        Icons.Outlined.ErrorOutline,
                                        contentDescription = "Upload failed",
                                        tint = Danger,
                                        modifier = Modifier.size(18.dp),
                                    )
                                } else {
                                    // A real indeterminate spinner: the static
                                    // glyph here looked like a file that had
                                    // stalled rather than one in flight.
                                    CircularProgressIndicator(
                                        color = Coral,
                                        strokeWidth = 2.dp,
                                        modifier = Modifier.size(18.dp),
                                    )
                                }
                            }
                        }
                    }
                }
                // Remove: a rounded-square chip with a 48 dp hit area around it.
                Box(
                    Modifier
                        .size(Lively.tapMin)
                        .clickable(role = Role.Button, onClickLabel = "Remove ${file.name}") { onRemove(file.localId) },
                    contentAlignment = Alignment.TopEnd,
                ) {
                    Box(
                        Modifier
                            .size(18.dp)
                            .clip(Lively.chipShape)
                            .background(Panel)
                            .border(1.dp, ControlBorder, Lively.chipShape),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(Icons.Outlined.Close, contentDescription = "Remove ${file.name}", tint = Ink, modifier = Modifier.size(11.dp))
                    }
                }
            }
        }
    }
}

internal const val ATTACHMENT_THUMB_DP = 54
internal const val ATTACHMENT_THUMB_CORNER_DP = 6

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

            if ((card.requiresReview || card.allowOther) && !card.resolved) {
                EscalationSubmit(enabled = !locked && !card.correlationId.isNullOrBlank(),
                    label = if (card.requiresReview) "Review in Attention" else "Answer in Attention") {
                    onOpenAttention(card.correlationId)
                }
            } else when (card.inputType) {
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
        color = if (fromUser) AccentFill else Panel,
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

/**
 * One of the two controls flanking the mic.
 *
 * iOS builds both the same way and it is the shape that carries the meaning: a
 * primary segment you press, a hairline, and a chevron to the settings behind
 * it. Drawn as a bare icon — which is what stood here — they read as
 * decoration rather than as controls with more inside them.
 */
@Composable
internal fun DockPill(
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
    // Same width and left edge as the composer card; flat, like the card.
    Surface(
        color = Panel,
        shape = Lively.composerShape,
        border = BorderStroke(1.dp, Muted.copy(alpha = 0.2f)),
        modifier = Modifier.fillMaxWidth().padding(start = COMPOSER_DOCK_INSET, end = COMPOSER_DOCK_INSET, bottom = 4.dp),
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
 * The `/` command picker: commands, then a command's options with a back row.
 *
 * Same card, badge and rows as [MentionPicker]; the current value carries a
 * check. Picking an option runs it on the client — it is never sent as a
 * chat message.
 */
@Composable
private fun CommandPicker(
    rows: List<ComposerCommandRow>,
    onPick: (ComposerCommandRow) -> Unit,
) {
    Surface(
        color = Panel,
        shape = Lively.composerShape,
        border = BorderStroke(1.dp, Muted.copy(alpha = 0.2f)),
        modifier = Modifier.fillMaxWidth().padding(start = COMPOSER_DOCK_INSET, end = COMPOSER_DOCK_INSET, bottom = 4.dp),
    ) {
        Column(
            Modifier
                .fillMaxWidth()
                .heightIn(max = 300.dp)
                .verticalScroll(rememberScrollState())
                .padding(vertical = 4.dp),
        ) {
            rows.forEach { row ->
                val inert = row.inert
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable(enabled = !inert) { onPick(row) }
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    if (row.action == ComposerCommandAction.Back) {
                        Text("‹", color = Coral, fontSize = 18.sp, fontWeight = FontWeight.Bold)
                    }
                    Surface(
                        color = Coral.copy(alpha = 0.15f),
                        shape = RoundedCornerShape(5.dp),
                    ) {
                        Text(
                            row.badge.uppercase(),
                            color = Coral,
                            fontSize = 9.sp,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(horizontal = 6.dp, vertical = 3.dp),
                        )
                    }
                    Column(Modifier.weight(1f)) {
                        // A note ("Now: …") may run to a second line rather than lose its end.
                        Text(
                            row.label,
                            color = if (inert) Secondary else Ink, fontSize = 14.sp,
                            fontWeight = FontWeight.Medium, maxLines = if (row.action == ComposerCommandAction.None) 2 else 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        row.detail?.takeIf { it.isNotBlank() }?.let {
                            Text(it, color = Secondary, fontSize = 11.sp, maxLines = 1)
                        }
                    }
                    if (row.current) {
                        Text(
                            "✓", color = Coral, fontSize = 15.sp, fontWeight = FontWeight.Bold,
                            modifier = Modifier.semantics { contentDescription = "Current" },
                        )
                    }
                }
            }
        }
    }
}

internal const val DAY_DIVIDER_KEY_PREFIX = "day-divider:"

/** A day change in the transcript: hairline · label · hairline (iOS `ChatDayDivider`). */
@Composable
private fun ChatDayDivider(label: String) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .semantics(mergeDescendants = true) { heading() },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Box(Modifier.weight(1f).height(1.dp).background(BorderSoft))
        Text(
            label,
            color = Secondary,
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            modifier = Modifier
                .clip(RoundedCornerShape(50))
                .background(activePalette.surface.copy(alpha = 0.85f))
                .padding(horizontal = 10.dp, vertical = 3.dp),
        )
        Box(Modifier.weight(1f).height(1.dp).background(BorderSoft))
    }
}

/**
 * The day under the top of the transcript while the reader scrolls history:
 * its own capsule, shown while scrolling away from the tail, fading ~1.2 s
 * after scrolling stops, and never when a visible divider already says the
 * same day ([ChatDateDividers.floatingLabel]).
 */
@Composable
private fun FloatingDatePill(
    listState: androidx.compose.foundation.lazy.LazyListState,
    timestamps: List<Long?>,
    messageIndexById: Map<String, Int>,
    modifier: Modifier = Modifier,
) {
    val atBottom by remember(listState) { derivedStateOf { !listState.canScrollForward } }
    val scrolling = listState.isScrollInProgress
    var shown by remember { mutableStateOf(false) }
    LaunchedEffect(scrolling, atBottom) {
        when {
            atBottom -> shown = false
            scrolling -> shown = true
            else -> {
                delay(FLOATING_DATE_FADE_MS)
                shown = false
            }
        }
    }
    val label by remember(listState, timestamps, messageIndexById) {
        derivedStateOf {
            val visible = listState.layoutInfo.visibleItemsInfo.mapNotNull { info ->
                val key = info.key as? String ?: return@mapNotNull null
                messageIndexById[key.removePrefix(DAY_DIVIDER_KEY_PREFIX)]
            }.toSet()
            ChatDateDividers.floatingLabel(timestamps, visible)
        }
    }
    androidx.compose.animation.AnimatedVisibility(
        visible = shown && !atBottom && label != null,
        enter = androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(150)),
        exit = androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(300)),
        modifier = modifier,
    ) {
        Text(
            label.orEmpty(),
            color = Ink,
            fontSize = 12.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier
                .shadow(2.dp, RoundedCornerShape(50))
                .clip(RoundedCornerShape(50))
                .background(Panel.copy(alpha = 0.94f))
                .border(1.dp, BorderSoft, RoundedCornerShape(50))
                .padding(horizontal = 12.dp, vertical = 6.dp)
                .clearAndSetSemantics {},
        )
    }
}

internal const val FLOATING_DATE_FADE_MS = 1_200L

private const val OLDER_HISTORY_ROW_KEY = "older-history"

/**
 * Older history, one page per scroll to the top.
 *
 * Armed only by the reader's own drag, so the open and its programmatic
 * scroll to the newest message never fetch. A page is requested when the
 * first loaded message comes into view; the view model refuses while one is
 * in flight or nothing older exists. While it loads, the first visible
 * message is remembered and, once the page is prepended, put back exactly
 * where it was, so reading continues instead of jumping.
 */
@Composable
private fun OlderHistoryPaging(
    state: ChatUiState,
    listState: androidx.compose.foundation.lazy.LazyListState,
    messageIndexById: Map<String, Int>,
    lazyIndexById: Map<String, Int>,
    onLoadOlder: () -> Unit,
) {
    var armed by remember(state.activeSessionId) { mutableStateOf(false) }
    LaunchedEffect(listState, state.activeSessionId) {
        listState.interactionSource.interactions.collect {
            if (it is androidx.compose.foundation.interaction.DragInteraction.Start) armed = true
        }
    }
    val firstId by rememberUpdatedState(state.messages.firstOrNull()?.id)
    val topMessageInView by remember(listState) {
        derivedStateOf {
            firstId?.takeIf { id -> listState.layoutInfo.visibleItemsInfo.any { it.key == id } }
        }
    }
    var anchor by remember(state.activeSessionId) { mutableStateOf<Pair<String, Int>?>(null) }
    val settled = anchor == null && !state.olderHistoryLoading
    // A failure waits for Retry rather than re-asking on every frame; until
    // the prepended page is anchored, the new first message only looks visible.
    LaunchedEffect(topMessageInView, armed, state.historyHasMore, settled) {
        if (topMessageInView != null && armed && settled && state.historyHasMore && state.olderHistoryError == null) {
            onLoadOlder()
        }
    }
    LaunchedEffect(state.olderHistoryLoading) {
        if (state.olderHistoryLoading) {
            anchor = listState.layoutInfo.visibleItemsInfo
                .firstOrNull { (it.key as? String) in messageIndexById }
                ?.let { (it.key as String) to it.offset }
            return@LaunchedEffect
        }
        val (key, offset) = anchor ?: return@LaunchedEffect
        try {
            val index = lazyIndexById[key] ?: return@LaunchedEffect
            listState.scrollToItem(index)
            if (offset != 0) listState.scrollBy(-offset.toFloat())
        } finally {
            anchor = null
        }
    }
}

/** The transcript's top row: a way to read further back, its progress, or its failure. */
@Composable
private fun OlderHistoryRow(loading: Boolean, error: String?, onLoad: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .then(if (loading) Modifier else Modifier.clickable(onClick = onLoad))
            .padding(vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        when {
            loading -> {
                CircularProgressIndicator(color = Muted, strokeWidth = 1.5.dp, modifier = Modifier.size(12.dp))
                Text("Loading earlier messages…", color = Muted, fontSize = 12.sp)
            }
            error != null -> {
                Text(
                    "Couldn't load earlier messages.",
                    color = Coral, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Text("Retry", color = Coral, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
            else -> Text("Load earlier messages", color = Muted, fontSize = 12.sp)
        }
    }
}

/** Same starters and draft-only interaction as Web, sized for a phone. */
private data class ChatStarter(val label: String, val hint: String, val prompt: String, val icon: ImageVector)

private val chatStarters = listOf(
    ChatStarter("Research flights", "Compare fares", "Find the cheapest Kyoto flights for next month and shortlist the three best options.", Icons.Outlined.FlightTakeoff),
    ChatStarter("Catch up on a meeting", "Decisions & next steps", "What did I miss in yesterday’s design sync? Decisions, and anything I owe people.", Icons.Outlined.CalendarToday),
    ChatStarter("Restock the groceries", "Stay within budget", "We’re out of groceries. Order my usual list and stay within my spending limit.", Icons.Outlined.ShoppingBasket),
    ChatStarter("Review my screen time", "See where time went", "Look at my recent screen observations and summarize where my time went today.", Icons.Outlined.Computer),
)

@Composable
private fun EmptyChat(
    readOnly: Boolean,
    suggestionsEnabled: Boolean,
    recentSessions: List<SessionSummary>,
    onSuggest: (String) -> Unit,
    onOpenSession: (String) -> Unit,
) {
    val hour = java.util.Calendar.getInstance().get(java.util.Calendar.HOUR_OF_DAY)
    val greeting = when {
        readOnly -> "Nothing in this session"
        hour < 12 -> "Good morning"
        hour < 18 -> "Good afternoon"
        else -> "Good evening"
    }
    val fontScale = LocalDensity.current.fontScale
    val colors = listOf(Coral, Teal, ChatSuccess, ChatDiscovery)
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val columns = if (maxWidth < 330.dp || fontScale >= 1.3f) 1 else 2
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(greeting, color = Ink, style = MaterialTheme.typography.headlineMedium,
                        fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center,
                        modifier = Modifier.semantics { heading() })
                if (!readOnly) {
                    Text("Pick a start, or say it your way.", color = Muted, fontSize = 12.sp, textAlign = TextAlign.Center)
                }
            }
            if (!readOnly) {
                Column(Modifier.widthIn(max = 560.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    chatStarters.chunked(columns).forEachIndexed { rowIndex, row ->
                        Row(Modifier.fillMaxWidth().height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            row.forEachIndexed { columnIndex, starter ->
                                val tint = colors[rowIndex * columns + columnIndex]
                                Surface(
                                    color = Panel, shape = RoundedCornerShape(16.dp), border = BorderStroke(1.dp, BorderSoft),
                                    modifier = Modifier.weight(1f).fillMaxHeight().clickable(enabled = suggestionsEnabled, role = Role.Button) { onSuggest(starter.prompt) }
                                        .semantics(mergeDescendants = true) { contentDescription = "${starter.label}. ${starter.prompt}" },
                                ) {
                                    Row(Modifier.padding(12.dp).heightIn(min = 64.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                        Box(Modifier.size(28.dp).background(tint.copy(alpha = 0.12f), RoundedCornerShape(10.dp)), contentAlignment = Alignment.Center) {
                                            Icon(starter.icon, null, tint = tint, modifier = Modifier.size(17.dp))
                                        }
                                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                                            Text(starter.label, color = Ink, fontSize = 14.sp, lineHeight = 18.sp, fontWeight = FontWeight.SemiBold, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                            Text(starter.hint, color = Muted, fontSize = 12.sp, lineHeight = 16.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if (recentSessions.isNotEmpty()) {
                Column(Modifier.widthIn(max = 560.dp).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("PICK UP WHERE YOU LEFT OFF", color = Muted, fontSize = 10.sp, letterSpacing = 1.sp,
                        fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(bottom = 4.dp).semantics { heading() })
                    recentSessions.forEach { session ->
                        Surface(color = Panel.copy(alpha = 0.70f), shape = RoundedCornerShape(12.dp),
                            modifier = Modifier.fillMaxWidth().clickable(enabled = suggestionsEnabled, role = Role.Button) { onOpenSession(session.identifier()) }) {
                            Row(Modifier.heightIn(min = 44.dp).padding(horizontal = 12.dp, vertical = 9.dp),
                                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                                Icon(Icons.Outlined.Schedule, null, tint = Muted, modifier = Modifier.size(15.dp))
                                Text(session.label(), color = Ink, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                                (session.updatedAt ?: session.createdAt)?.let { timestamp ->
                                    Text(android.text.format.DateUtils.getRelativeTimeSpanString(timestamp, System.currentTimeMillis(), android.text.format.DateUtils.MINUTE_IN_MILLIS,
                                        android.text.format.DateUtils.FORMAT_ABBREV_RELATIVE).toString(), color = Muted, fontSize = 10.sp)
                                }
                                Icon(Icons.Outlined.ChevronRight, null, tint = Muted, modifier = Modifier.size(14.dp))
                            }
                        }
                    }
                }
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

@Composable
internal fun ModelThinkingControl(control: ai.magicbeans.magdroid.chat.ChatThinkingControl, level: String, enabled: Boolean = true, onSelect: (String) -> Unit) {
                    var previewIndex by remember(control.levels, control.defaultLevel, level) { mutableStateOf<Float?>(null) }
                    val displayedLevel = previewIndex?.let { control.levels.getOrNull(it.toInt()) } ?: level
                    if (control.mode != "none" && control.levels.isNotEmpty()) {
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            Text("Thinking · ${if (displayedLevel == "auto" && control.defaultLevel == null) "Engine default" else thinkingLevelLabel(if (displayedLevel == "auto") control.defaultLevel ?: "auto" else displayedLevel)}", color = Ink, modifier = Modifier.weight(1f))
                            FilterChip(selected = displayedLevel == "auto", enabled = enabled, onClick = { previewIndex = null; onSelect("auto") }, label = { Text("Auto") },
                                shape = RoundedCornerShape(6.dp),
                                border = BorderStroke(1.dp, if (displayedLevel == "auto") Coral else ControlBorder),
                                colors = FilterChipDefaults.filterChipColors(containerColor = Control, labelColor = Ink,
                                    selectedContainerColor = Soft, selectedLabelColor = Coral))
                        }
                        if (control.mode == "toggle") {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text("Thinking enabled", color = Ink, modifier = Modifier.weight(1f))
                                Switch(checked = (if (level == "auto") control.defaultLevel else level) != "none", enabled = enabled,
                                    colors = SwitchDefaults.colors(checkedTrackColor = AccentFill, checkedThumbColor = OnAccent),
                                    onCheckedChange = { checked -> onSelect(if (checked) "on" else "none") })
                            }
                        } else if (control.levels.size > 1) {
                            val current = control.levels.indexOf(if (level == "auto") control.defaultLevel else level).coerceAtLeast(0)
                            Slider(value = previewIndex ?: current.toFloat(), enabled = enabled, onValueChange = { previewIndex = it },
                                onValueChangeFinished = {
                                    previewIndex?.let { onSelect(control.levels[it.toInt().coerceIn(control.levels.indices)]) }
                                    previewIndex = null
                                }, valueRange = 0f..(control.levels.size - 1).toFloat(), steps = (control.levels.size - 2).coerceAtLeast(0),
                                colors = SliderDefaults.colors(thumbColor = Coral, activeTrackColor = Coral,
                                    inactiveTrackColor = ControlBorder, activeTickColor = OnAccent, inactiveTickColor = Muted),
                                modifier = Modifier.semantics { contentDescription = "Thinking level" })
                            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                                control.levels.forEach { option ->
                                    Box(Modifier.weight(1f).clip(RoundedCornerShape(6.dp))
                                        .background(if (displayedLevel == option) Soft else Color.Transparent)
                                        .clickable(enabled = enabled, role = Role.Button) { previewIndex = null; onSelect(option) }
                                        .semantics { selected = displayedLevel == option; contentDescription = "Set thinking to ${thinkingLevelLabel(option)}" }
                                        .heightIn(min = 36.dp).padding(horizontal = 1.dp), contentAlignment = Alignment.Center) {
                                        Text(thinkingLevelLabel(option), color = if (displayedLevel == option) Coral else Muted,
                                            fontSize = 10.sp, textAlign = TextAlign.Center, maxLines = 2,
                                            fontWeight = if (displayedLevel == option) FontWeight.SemiBold else FontWeight.Normal)
                                    }
                                }
                            }
                        }
                        Text("Higher thinking can improve difficult answers and take more time.", color = Muted, fontSize = 12.sp)
                    }
}

internal data class CatalogChoiceOption(val id: String, val label: String, val detail: String? = null, val enabled: Boolean = true)

internal fun catalogEngineLabel(id: String): String = ComposerCommands.engineDisplayName(id)

/** The engine's name plus whose credentials it runs on (`Codex · your login`). */
internal fun catalogEngineLabel(id: String, credentials: String?): String =
    HarnessCredentials.engineName(id, catalogEngineLabel(id), credentials)

/** The same themed box and stable, bounded scrolling menu in Chat and VibeDev. */
@Composable
internal fun CatalogChoiceDropdown(label: String, selectedId: String, options: List<CatalogChoiceOption>, enabled: Boolean = true, onSelect: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val scrollState = rememberScrollState()
    Box(Modifier.fillMaxWidth()) {
        OutlinedButton(onClick = { expanded = true }, enabled = enabled && options.isNotEmpty(),
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
            shape = RoundedCornerShape(8.dp), border = BorderStroke(1.dp, ControlBorder),
            colors = ButtonDefaults.outlinedButtonColors(containerColor = Control, contentColor = Ink),
            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 10.dp)) {
            Text(options.firstOrNull { it.id == selectedId }?.label ?: "Choose a model",
                modifier = Modifier.weight(1f), textAlign = TextAlign.Start, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Icon(Icons.Outlined.KeyboardArrowDown, contentDescription = null, tint = Muted, modifier = Modifier.size(18.dp))
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false },
            containerColor = Panel, scrollState = scrollState, modifier = Modifier.heightIn(max = 360.dp).widthIn(min = 240.dp, max = 360.dp)) {
            options.forEach { option -> key(option.id) {
                DropdownMenuItem(text = {
                    Column {
                        Text(option.label, color = Ink, fontSize = 14.sp)
                        option.detail?.let { Text(it, color = Muted, fontSize = 11.sp) }
                    }
                }, enabled = enabled && option.enabled,
                    trailingIcon = { if (option.id == selectedId) Icon(Icons.Outlined.Check, contentDescription = "Selected", tint = Coral) },
                    modifier = Modifier.semantics { selected = option.id == selectedId },
                    onClick = { onSelect(option.id); expanded = false })
            } }
        }
    }
}

@Composable
internal fun ComposerModeSegment(label: String, selected: Boolean, onSelect: () -> Unit) {
    Surface(color = if (selected) AccentFill else Color.Transparent,
        shape = RoundedCornerShape(COMPOSER_MODE_SEGMENT_CORNER_DP.dp),
        modifier = Modifier.heightIn(min = COMPOSER_MODE_MIN_HEIGHT_DP.dp)
            .clickable(role = Role.Button, onClick = onSelect).semantics { this.selected = selected }) {
        Text(label, color = if (selected) OnAccent else Muted, fontSize = COMPOSER_MODE_FONT_SP.sp,
            lineHeight = 12.sp, maxLines = 1, fontWeight = FontWeight.SemiBold,
            fontFamily = LocalMagicanFontFamilies.current.mono, letterSpacing = 0.4.sp,
            modifier = Modifier.wrapContentHeight(Alignment.CenterVertically).padding(
                horizontal = COMPOSER_MODE_HORIZONTAL_PADDING_DP.dp, vertical = COMPOSER_MODE_VERTICAL_PADDING_DP.dp))
    }
}
