package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.access.MagicianAccess
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.toFailure
import ai.magicbeans.magdroid.voice.Dictation
import ai.magicbeans.magdroid.voice.LiveCallAudioFocus
import ai.magicbeans.magdroid.voice.Speech
import ai.magicbeans.magdroid.voice.applyCatalog
import ai.magicbeans.magdroid.voice.VoicePrefs
import ai.magicbeans.magdroid.voice.resolveLivePushToTalk
import ai.magicbeans.magdroid.voice.audioProfileAvailable
import ai.magicbeans.magdroid.voice.DictationState
import ai.magicbeans.magdroid.voice.LiveVoiceEngine
import ai.magicbeans.magdroid.voice.NativeAudioStage
import ai.magicbeans.magdroid.voice.NativeAudioSurface
import ai.magicbeans.magdroid.voice.RealtimeVoiceCatalog
import ai.magicbeans.magdroid.voice.RealtimeVoiceSession
import ai.magicbeans.magdroid.voice.RealtimeVoiceState
import ai.magicbeans.magdroid.voice.supports
import android.app.Application
import android.content.Context
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import java.util.UUID

/** Scope-local pointer to the conversation this device last displayed. */
internal class ChatSessionSelectionStore(private val app: Application) {
    private val preferences = app.getSharedPreferences("magdroid.chat.selection", Context.MODE_PRIVATE)

    private fun key(): String {
        val parts = listOf(
            ai.magicbeans.magdroid.access.MagicianAccess.baseUrl(app),
            ai.magicbeans.magdroid.access.MagicianAccess.principal(app),
            ai.magicbeans.magdroid.access.MagicianAccess.workspace(app),
        )
        return parts.joinToString("|") { "${it.toByteArray().size}:$it" }
    }

    fun remembered(): String? = preferences.getString(key(), null)?.trim()?.takeIf { it.isNotEmpty() }

    fun remember(sessionId: String) {
        sessionId.trim().takeIf { it.isNotEmpty() }?.let { preferences.edit().putString(key(), it).apply() }
    }

    fun forget(sessionId: String? = null) {
        val key = key()
        if (sessionId != null && preferences.getString(key, null) != sessionId.trim()) return
        preferences.edit().remove(key).apply()
    }
}

/** Upgrade fallback when no device selection has been recorded yet. */
internal fun mostRecentRestorableSession(sessions: List<SessionSummary>): String? =
    sessions.asSequence()
        .filter { it.status == null || it.status.equals("active", ignoreCase = true) }
        .filter { !it.historyLane.equals("automated", ignoreCase = true) }
        .filter { it.identifier().isNotBlank() }
        .maxWithOrNull(
            compareBy<SessionSummary> { it.updatedAt ?: it.createdAt ?: Long.MIN_VALUE }
                .thenBy { it.identifier() },
        )
        ?.identifier()

internal data class ChatSessionStartupResolution(
    val sessionId: String,
    val history: List<ChatMessage>,
)

/** The startup decision without Android lifecycle state, so absence and outage stay testable. */
internal suspend fun resolveChatSessionStartup(
    forceNew: Boolean,
    currentSessionId: String?,
    rememberedSessionId: String?,
    findExistingSession: suspend () -> String?,
    readHistory: suspend (String) -> List<ChatMessage>,
    createSession: suspend () -> String,
    onRememberedMissing: (String) -> Unit,
    onCreated: (String) -> Unit,
): ChatSessionStartupResolution {
    suspend fun createAndRead(): ChatSessionStartupResolution {
        val created = createSession()
        // The create is authoritative even if the subsequent read loses its
        // connection. Persist before that read so retry cannot duplicate it.
        onCreated(created)
        return ChatSessionStartupResolution(created, readHistory(created))
    }

    if (forceNew) return createAndRead()

    val preferred = currentSessionId ?: rememberedSessionId
    if (preferred != null) {
        return try {
            ChatSessionStartupResolution(preferred, readHistory(preferred))
        } catch (problem: ChatError) {
            if (problem.failure.kind != FailureKind.NotFound) throw problem
            onRememberedMissing(preferred)
            createAndRead()
        }
    }

    val existing = findExistingSession()
    return if (existing != null) {
        ChatSessionStartupResolution(existing, readHistory(existing))
    } else {
        createAndRead()
    }
}

/** Everything the chat screen renders, in one value. */
data class ChatUiState(
    val queuedMessages: List<QueuedTextMessage> = emptyList(),
    val queueSessionId: String? = null,
    val serverRunning: Boolean = false,
    val queueMutationInFlight: Boolean = false,
    /** Why the last queue action failed; shown as a band in the composer, not over the transcript. */
    val queueError: String? = null,
    val messages: List<ChatMessage> = emptyList(),
    /** Older messages exist above the first loaded one. */
    val historyHasMore: Boolean = false,
    /** One older page is being read; the transcript's top row says so. */
    val olderHistoryLoading: Boolean = false,
    /** Why the last older page failed; the top row offers a retry. */
    val olderHistoryError: String? = null,
    /** What dictation is doing, which the composer draws three ways. */
    val dictation: DictationState = DictationState.Idle,
    /** Live transcript while speaking, shown above the mic and never merged early. */
    val partialTranscript: String = "",
    /** Seconds until a dictated draft sends itself, or null when nothing is armed. */
    val autoSendIn: Int? = null,
    /** Whether the pending turn came in by voice, which the backend records. */
    val turnFromVoice: Boolean = false,
    /**
     * Where this send landed in the queue, when the session was mid-turn.
     *
     * Null when the turn ran normally. A queued message produced no reply, and
     * saying nothing would look like the assistant ignored it.
     */
    val queuedPosition: Int? = null,
    val draft: String = "",
    val sending: Boolean = false,
    /** The assistant message currently being read aloud, if any. */
    val speakingMessageId: String? = null,
    val loading: Boolean = true,
    /** A turn that failed to send. The conversation itself is fine. */
    val error: String? = null,
    /**
     * The conversation itself could not be opened, classified.
     *
     * Separate from [error] because it is the whole screen rather than a line
     * over it, and because it is the one that needs a way back — a chat that
     * cannot reach Magician previously offered nothing but the text of the
     * problem.
     */
    val failure: Failure? = null,
    val setupRequired: Boolean = false,
    val sessions: List<SessionSummary> = emptyList(),
    val selectedSession: SessionSummary? = null,
    val focusedMessageId: String? = null,
    val activeSessionId: String? = null,
    /** The session-menu mutation currently holding the controls, if any. */
    val sessionActionInFlight: ChatSessionAction? = null,
    /** Mentions offered while the composer holds an unfinished `@`. */
    val mentions: List<MentionItem> = emptyList(),
    /** The trigger those mentions answer, and the span a pick replaces. */
    val mentionTrigger: MentionTrigger? = null,
    val profiles: List<ChatProfile> = emptyList(),
    val selectedProfile: String = "",
    val modelSelectionIssue: String? = null,
    val modelChoicesLoading: Boolean = true,
    val chatHarnesses: List<ChatHarnessOption> = listOf(ChatHarnessOption("magician", true)),
    val selectedHarnessEngine: String = "magician",
    val selectedHarnessModel: String = "default",
    val attachments: List<StagedAttachment> = emptyList(),
    /** The active Do-or-Plan mode. Dictation uses this too, so it cannot live only in the composer. */
    val composerMode: ChatComposerMode = ChatComposerMode.Ask,
    /** Do's Ask/Accept permission remains parked while Plan is active. */
    val composerDoPermission: ChatDoPermission = ChatDoPermission.Ask,
    val completeResult: CompleteResultViewerState? = null,
    /** Where the `/` command picker is (command list, or a command's options). */
    val commandState: ComposerCommandState = ComposerCommandState(),
    /** The run engine from the roster, which `/run-engine` marks. */
    val runHarnessEngine: String? = null,
    val runHarnessModel: String? = null,
    /** Agentic flows follow the chat engine, and Pi's run model (roster `follows_chat`, `run_model_selection`). */
    val runFollowsChat: Boolean = false,
    val runModelSelection: ChatModelSelection? = null,
    /** The server's chat engine (roster `chat_current`), which `/engine` shows as "everywhere". */
    val serverChatEngine: String? = null,
    /** Catalogs the `/` picker read for its model levels, per engine, and why one could not be read. */
    val commandModelCatalogs: Map<String, List<ChatModelChoice>> = emptyMap(),
    val commandModelErrors: Map<String, String> = emptyMap(),
    /** `/run-engine magician`: the `agentic_decision` operation's models (null = not read yet). */
    val loopModels: List<ChatModelChoice>? = null,
    val loopSelection: ChatModelSelection? = null,
    val loopModelsError: String? = null,
    /** `/budget default`: the caps read from the server, or why they could not be. */
    val budgetDefaults: BudgetDefaults? = null,
    val budgetDefaultsError: String? = null,
    /** The last slash command's confirmation (or failure), shown once as a toast. */
    val commandNotice: ComposerCommandNotice? = null,
)

/** A slash command's short confirmation; [id] makes a repeat show again. */
data class ComposerCommandNotice(val text: String, val isError: Boolean = false, val id: Long = System.nanoTime())

/**
 * What the `/` picker's option rows read, from the chat's own state; [voice] is
 * the composer's mic mode and availability, which the screen holds.
 */
fun ChatUiState.commandContext(voice: ComposerCommandVoice = ComposerCommandVoice()): ComposerCommandContext = ComposerCommandContext(
    engines = chatHarnesses.filter { it.installed }.map { ComposerCommandEngine(it.name, it.models, it.credentials) },
    deviceEngine = selectedHarnessEngine,
    deviceModel = selectedHarnessModel,
    deviceModels = profiles.mapNotNull { it.modelChoice },
    deviceSelection = profiles.firstOrNull { it.name == selectedProfile }?.modelSelection,
    serverChatEngine = serverChatEngine,
    runEngine = runHarnessEngine,
    runModel = runHarnessModel,
    runFollowsChat = runFollowsChat,
    runSelection = runModelSelection,
    modelCatalogs = commandModelCatalogs,
    modelCatalogErrors = commandModelErrors,
    loopModels = loopModels,
    loopSelection = loopSelection,
    loopModelsError = loopModelsError,
    voiceMode = voice.mode,
    liveVoiceAvailable = voice.liveVoiceAvailable,
    handsFreeAvailable = voice.handsFreeAvailable,
    tasks = messages.mapNotNull { message ->
        message.task?.takeIf { message.kind == MessageKind.TaskStatus }
            ?.let { ComposerCommandTask(it.taskId, it.title, it.status) }
    },
    budgetDefaults = budgetDefaults,
    budgetDefaultsError = budgetDefaultsError,
)

/** The rows the `/` picker shows now (empty = hidden). */
fun ChatUiState.commandRows(voice: ComposerCommandVoice = ComposerCommandVoice()): List<ComposerCommandRow> =
    ComposerCommands.rows(draft, commandState, commandContext(voice))

data class CompleteResultViewerState(
    val resultRef: String,
    val title: String,
    val loading: Boolean = true,
    val text: String = "",
    val error: String? = null,
    val contentHash: String? = null,
)

/** Apply a mode selection while retaining Do's permission across Plan. */
internal fun ChatUiState.withComposerMode(mode: ChatComposerMode): ChatUiState = copy(
    composerMode = mode,
    composerDoPermission = ChatDoPermission.fromMode(mode) ?: composerDoPermission,
)

/** Mount the response owner before text or activity has reached the phone. */
internal fun liveAssistantPlaceholder(replyId: String, chatTurnId: String): ChatMessage =
    ChatMessage(
        id = replyId,
        fromUser = false,
        text = "",
        streaming = true,
        chatTurnId = chatTurnId,
        createdAtMs = System.currentTimeMillis(),
        optimistic = true,
    )

/** Attach canonical Steps to the latest text response for this turn, as iOS does. */
internal fun ChatUiState.withTurnActivity(
    chatTurnId: String,
    rows: List<ActivityRow>,
): ChatUiState {
    fun ownsSteps(message: ChatMessage) =
        !message.fromUser && message.kind == MessageKind.Text && message.chatTurnId == chatTurnId
    // A turn's status line owns its steps only when it stands alone; under a
    // partial answer, the answer keeps them.
    val owner = messages.indexOfLast { ownsSteps(it) && !it.isTurnNotice() }
        .takeIf { it >= 0 } ?: messages.indexOfLast(::ownsSteps)
    if (owner < 0) return this
    val settled = settleActivityRows(rows, terminalTaskStatuses(messages))
    return copy(
        messages = messages.mapIndexed { index, message ->
            if (index == owner) {
                message.copy(activityRows = settled)
            } else {
                message
            }
        },
    )
}

/** Destructive scope of a session-menu action. */
enum class ChatSessionAction {
    Clear,
    Archive,
    Delete,
}

/** What the session menu says while its action waits on the server. */
fun ChatSessionAction.progressLabel(): String = when (this) {
    ChatSessionAction.Clear -> "Clearing…"
    ChatSessionAction.Archive -> "Archiving…"
    ChatSessionAction.Delete -> "Deleting…"
}

/**
 * Apply a successful backend mutation to the transcript projection.
 *
 * Clear preserves the session and composer. Archive/delete stop projecting the
 * old owner altogether; [ChatViewModel] then opens a fresh session through the
 * normal creation path.
 */
internal fun ChatUiState.afterSessionAction(
    action: ChatSessionAction,
    sessionId: String,
): ChatUiState = when (action) {
    ChatSessionAction.Clear -> copy(
        messages = emptyList(),
        historyHasMore = false,
        olderHistoryLoading = false,
        olderHistoryError = null,
        sending = false,
        queuedPosition = null,
        error = null,
        sessionActionInFlight = null,
    )

    ChatSessionAction.Archive,
    ChatSessionAction.Delete,
    -> copy(
        messages = emptyList(),
        historyHasMore = false,
        olderHistoryLoading = false,
        olderHistoryError = null,
        draft = "",
        sending = false,
        queuedPosition = null,
        turnFromVoice = false,
        autoSendIn = null,
        error = null,
        activeSessionId = null,
        sessions = sessions.filterNot { it.identifier() == sessionId },
        attachments = emptyList(),
        sessionActionInFlight = null,
    )
}

/**
 * Owns the chat conversation.
 *
 * One immutable [ChatUiState] rather than a bag of observables: a streaming turn
 * changes the last message dozens of times a second, and a screen assembled from
 * several independent signals will render halfway between two of them.
 *
 * The streaming job is held so it can be cancelled. Cancelling collection
 * cancels the request, which is what stopping a turn has to mean — a bubble that
 * stops updating while the backend keeps generating is a lie.
 */
class ChatViewModel(app: Application) : AndroidViewModel(app) {

    private val repository = ChatRepository(app)
    private val dictation = Dictation(app)
    val dictationLevel: StateFlow<Float> = dictation.level
    private val speech = Speech(app)
    private val voicePrefs = VoicePrefs.get(app)
    private val realtime = RealtimeVoiceSession(app)
    private val sessionSelection = ChatSessionSelectionStore(app)
    private val chatChoice = app.getSharedPreferences("magdroid.chat.choice", Context.MODE_PRIVATE)
    val realtimeVoiceState: StateFlow<RealtimeVoiceState> = realtime.state
    private val _voiceCatalog = MutableStateFlow(RealtimeVoiceCatalog())
    val voiceCatalog: StateFlow<RealtimeVoiceCatalog> = _voiceCatalog.asStateFlow()
    private var voiceScreenActive = false
    private var queuePollJob: Job? = null
    private var voiceInputActive = false
    private var voiceInteracted = false
    val concurrentVoice by lazy {
        ai.magicbeans.magdroid.voice.ConcurrentVoiceCoordinator(
            scope = viewModelScope,
            request = repository::voiceRequest,
            eligible = { voiceScreenActive && _tutorRun.value == null },
            // A live call is a reason to stay quiet, not to speak: results
            // wait for the hang-up (`LiveCallAudioFocus`).
            automaticPlayback = { voiceInteracted || voicePrefs.speakReplies.value },
            liveActive = { LiveCallAudioFocus.active.value },
            outputBusy = { _state.value.sending || _state.value.serverRunning || _state.value.draft.isNotBlank() ||
                _state.value.dictation != DictationState.Idle || voiceInputActive ||
                realtime.concurrentOutputBusy || speech.speaking.value },
            play = speech::speakManaged,
            playForChat = speech::speakManagedForChat,
            stopPlayback = speech::stop,
            focusChanged = { realtime.selectConcurrentContext(it?.branchSessionId) },
        )
    }
    private val _voiceResult = MutableStateFlow<Pair<String, String>?>(null)
    val voiceResult = _voiceResult.asStateFlow()
    fun setVoiceScreenActive(active: Boolean) {
        voiceScreenActive = active
        if (active) {
            concurrentVoice.start(); concurrentVoice.activate()
            if (queuePollJob?.isActive != true) queuePollJob = viewModelScope.launch {
                while (kotlinx.coroutines.currentCoroutineContext().isActive) {
                    refreshQueue(); kotlinx.coroutines.delay(2_000)
                }
            }
        } else { concurrentVoice.deactivate(); queuePollJob?.cancel(); queuePollJob = null }
    }
    fun showVoiceResult(id: String) {
        _voiceResult.value = id to "Loading…"
        concurrentVoice.action {
            val result = concurrentVoice.result(id)
            if (_voiceResult.value?.first == id) {
                _voiceResult.value = id to result
                concurrentVoice.markRead(id)
            }
        }
    }
    fun closeVoiceResult() { _voiceResult.value = null }
    fun sendInBackground() = send(background = true)
    fun stopAndSend() = send(stopAndSend = true)

    private suspend fun refreshQueue() {
        val id = sessionId ?: return
        runCatching { repository.queueSnapshot(id) }.onSuccess { queue -> applyQueueSnapshot(id, queue) }
    }

    private fun applyQueueSnapshot(forSession: String, queue: ChatQueueSnapshot) {
        if (sessionId != forSession) return
        _state.value = _state.value.copy(
            queuedMessages = queue.queued, queueSessionId = forSession, serverRunning = queue.active,
            queuedPosition = null,
        )
        applyTurnStateReport(forSession, queue.turnState, TurnStateSource.Queue)
    }

    fun actOnQueue(messageId: String, action: String) {
        val id = sessionId ?: return
        if (_state.value.queueMutationInFlight) return
        _state.value = _state.value.copy(queueMutationInFlight = true)
        viewModelScope.launch {
            try {
                if (action == "remove") repository.removeQueued(id, messageId)
                else repository.queueAction(id, messageId, action)
                if (sessionId == id) _state.value = _state.value.copy(queueError = null)
            } catch (error: Exception) {
                if (sessionId == id) _state.value = _state.value.copy(
                    queueError = error.message?.takeIf { it.isNotBlank() } ?: "The queued message could not be updated.",
                )
            } finally { _state.value = _state.value.copy(queueMutationInFlight = false); refreshQueue() }
        }
    }

    /** Drop every queued message of this conversation (iOS / web "Clear all"). */
    fun clearQueued() {
        val id = sessionId ?: return
        if (_state.value.queueMutationInFlight || _state.value.queuedMessages.isEmpty()) return
        _state.value = _state.value.copy(queueMutationInFlight = true)
        viewModelScope.launch {
            try {
                repository.clearQueue(id)
                if (sessionId == id) _state.value = _state.value.copy(queuedMessages = emptyList(), queueError = null)
            } catch (error: Exception) {
                if (sessionId == id) _state.value = _state.value.copy(
                    queueError = error.message?.takeIf { it.isNotBlank() } ?: "The queue could not be cleared.",
                )
            } finally { _state.value = _state.value.copy(queueMutationInFlight = false); refreshQueue() }
        }
    }

    fun dismissQueueError() {
        _state.value = _state.value.copy(queueError = null)
    }

    /**
     * Leave an archived (read-only) conversation for the most recent active
     * one, else a new one.
     */
    fun returnToActive() {
        viewModelScope.launch {
            val active = runCatching { repository.mostRecentActivePersonalSessionId() }.getOrNull()
            if (active != null && active != sessionId) openSession(active) else newSession()
        }
    }

    /**
     * Where a lesson this turn starts would be drawn.
     *
     * Held here because the send path is the only thing that can announce it,
     * and the router is the only thing that knows it.
     */
    private val tutorRouter = ai.magicbeans.magdroid.tutor.TutorRouter(
        // Both halves are required: a screen to capture, and permission to
        // draw over it. Either missing means the blackboard.
        overlayPermitted = {
            ai.magicbeans.magdroid.tutor.TutorScreenGrab.available() &&
                ai.magicbeans.magdroid.tutor.ScreenCapture.overlayGranted()
        },
    )

    /** The run a tutor turn is feeding, or null when no lesson is live. */
    private val _tutorRun = kotlinx.coroutines.flow.MutableStateFlow<ai.magicbeans.magdroid.tutor.TutorRun?>(null)
    val tutorRun: kotlinx.coroutines.flow.StateFlow<ai.magicbeans.magdroid.tutor.TutorRun?> =
        _tutorRun.asStateFlow()

    private val tutorRealtime = ai.magicbeans.magdroid.tutor.TutorRealtime(app)

    init {
        // The grant seam needs a Context, which this has and the tutor package
        // deliberately does not.
        ai.magicbeans.magdroid.tutor.ScreenCapture.overlayGranted = {
            android.provider.Settings.canDrawOverlays(app)
        }
        ai.magicbeans.magdroid.tutor.ScreenCapture.available =
            ai.magicbeans.magdroid.tutor.TutorScreenGrab::available

        // Primitives before the first shape. The bundled/cached set is read
        // synchronously so a lesson starting immediately still draws, then the
        // backend's set — which carries this scope's own custom primitives —
        // replaces it. A failed fetch leaves the local set in place.
        ai.magicbeans.magdroid.tutor.TutorPrimitiveSource.loadLocal(app)
        viewModelScope.launch {
            ai.magicbeans.magdroid.tutor.TutorPrimitiveSource.refresh(app)
        }
    }
    private val _state = MutableStateFlow(ChatUiState(
        selectedProfile = chatChoice.getString("profile", "").orEmpty(),
        selectedHarnessEngine = chatChoice.getString("engine", "magician") ?: "magician",
        selectedHarnessModel = chatChoice.getString("model", "default") ?: "default",
    ))
    val state: StateFlow<ChatUiState> = _state.asStateFlow()

    private var localChatKey = chatLocalKey(null)
    private fun chatLocalKey(id: String?): String = ChatLocalState.key(
        MagicianAccess.baseUrl(getApplication<Application>()), MagicianAccess.principal(getApplication<Application>()),
        MagicianAccess.workspace(getApplication<Application>()), id)
    private fun choiceKey(suffix: String) = "$localChatKey|$suffix"
    private fun inheritChoicesForCreatedSession(id: String) {
        val target = chatLocalKey(id)
        val edit = chatChoice.edit()
        edit.putString("$target|engine", _state.value.selectedHarnessEngine)
            .putString("$target|model", _state.value.selectedHarnessModel)
            .putString("$target|draft", _state.value.draft)
        chatChoice.getString(choiceKey(ChatModelPreference.storageKey), null)?.let {
            edit.putString("$target|${ChatModelPreference.storageKey}", it)
        }
        edit.apply()
    }

    private var sessionId: String? = null
        set(value) {
            val nextKey = chatLocalKey(value)
            if (localChatKey != nextKey) {
                chatChoice.edit().putString(choiceKey("draft"), _state.value.draft).apply()
                localChatKey = nextKey
                modelCatalogGeneration++
                _state.value = _state.value.copy(
                    draft = chatChoice.getString(choiceKey("draft"), "").orEmpty(),
                    profiles = emptyList(), selectedProfile = chatChoice.getString("profile", "").orEmpty(),
                    selectedHarnessEngine = chatChoice.getString(choiceKey("engine"), chatChoice.getString("engine", "magician")) ?: "magician",
                    selectedHarnessModel = chatChoice.getString(choiceKey("model"), chatChoice.getString("model", "default")) ?: "default")
            }
            field = value
        }
    private var streamJob: Job? = null
    private var activityJob: Job? = null
    /** The turn [activityJob] follows live. */
    private var followedTurnId: String? = null
    /** The turn this device's own send stream is still driving; its stream settles it. */
    private var localStreamTurnId: String? = null
    /** Latest server state per turn of the open conversation (`turn_state`). */
    private val turnStates = LinkedHashMap<String, ChatTurnState>()
    private val activityLoadsInFlight = mutableSetOf<String>()
    private var resultReadJob: Job? = null
    private var startJob: Job? = null
    private var sessionOpenJob: Job? = null
    private var sessionOpenGeneration = 0L
    /** The oldest wire message loaded for [sessionId]: the next older page's `before`. */
    private var historyCursor: String? = null
    private var olderHistoryJob: Job? = null
    private var modelCatalogGeneration = 0L

    /**
     * A seed waiting for the map surface to pick it up.
     *
     * A one-shot handoff rather than a navigation call, so the view model stays
     * unaware of what screen shows a map. Empty string is meaningful: a bare
     * `@brainstorm` opens capture for a fresh map.
     */
    private val _brainstormSeed = kotlinx.coroutines.flow.MutableStateFlow<String?>(null)
    val brainstormSeed: kotlinx.coroutines.flow.StateFlow<String?> = _brainstormSeed

    fun consumeBrainstormSeed() { _brainstormSeed.value = null }
    private var realtimeJob: Job? = null
    private var autoSendJob: Job? = null
    private var configuredVoiceStartJob: Job? = null

    init {
        _state.value = _state.value.copy(draft = chatChoice.getString(choiceKey("draft"), "").orEmpty())
        viewModelScope.launch {
            state.map { localChatKey to it.draft }.distinctUntilChanged().collect { (key, draft) ->
                if (key == localChatKey && draft == _state.value.draft)
                    chatChoice.edit().putString("$key|draft", draft).apply()
            }
        }
        start()
        realtime.concurrentRequests = true
        realtime.onConcurrentEvent = { kind, _ -> viewModelScope.launch {
            when (kind) {
                "session.ready" -> { voiceInteracted = true; concurrentVoice.activate() }
                "speech.started" -> { voiceInputActive = true; concurrentVoice.captureStarted() }
                "speech.stopped" -> { voiceInputActive = false; concurrentVoice.captureStopped() }
                "transcript.user", "transcript.user.ignored" -> if (!voiceInputActive) concurrentVoice.inputSettled()
                "input.cleared" -> { voiceInputActive = false; concurrentVoice.captureStopped(); concurrentVoice.inputSettled() }
                "audio.output.started" -> concurrentVoice.foregroundStarted()
                "audio.output.ended" -> { concurrentVoice.inputSettled(); concurrentVoice.foregroundStopped() }
                "voice.request.accepted" -> { concurrentVoice.inputSettled(); concurrentVoice.foregroundStopped(); refreshSessions() }
            }
        } }
        // A call that starts while a reply is being read stops the reading
        // (Speech stops itself; this clears the message's Stop state too).
        viewModelScope.launch { LiveCallAudioFocus.active.collect { live -> if (live) stopSpeaking() } }
        syncVoicePreferences()
        refreshVoiceCatalog()
        // The composer draws dictation's state, so it has to see it change.
        viewModelScope.launch {
            dictation.state.collect { _state.value = _state.value.copy(dictation = it) }
        }
        viewModelScope.launch {
            dictation.partial.collect { _state.value = _state.value.copy(partialTranscript = it) }
        }
        viewModelScope.launch {
            dictation.error.collect { problem ->
                problem?.let { _state.value = _state.value.copy(error = it) }
            }
        }
    }

    /**
     * Start listening.
     *
     * Both the tap and the hold arrive here, and both finish through
     * [acceptTranscript] — one completion path is what guarantees the countdown
     * and the voice-origin stamp behave the same whichever gesture was used.
     */
    fun startDictation() {
        if (dictation.state.value != DictationState.Idle) return
        cancelAutoSend()
        voiceInteracted = true
        concurrentVoice.captureStarted()
        // Barge-in: speaking over the reply means you want to interrupt it, and
        // dictating into a phone that is still talking transcribes its own
        // voice back at you.
        stopSpeaking()
        dictation.startForChat(
            chatSessionId = sessionId,
            onNoTranscript = {
                concurrentVoice.captureStopped()
                concurrentVoice.inputSettled()
            },
            onTranscript = ::acceptTranscript,
        )
    }

    /** Finish and transcribe: a tap on a live mic, or a released hold. */
    fun stopDictation() { concurrentVoice.captureStopped(); dictation.stop() }

    /** Abandon the capture without transcribing. */
    fun cancelDictation() { dictation.cancel(); concurrentVoice.captureStopped(); concurrentVoice.inputSettled() }

    private fun acceptTranscript(transcript: String) {
        concurrentVoice.captureStopped()
        if (transcript.isBlank()) {
            concurrentVoice.inputSettled()
            return
        }
        // Merged, not replaced: someone who typed half a thought and then spoke
        // the rest means both halves.
        val existing = _state.value.draft
        val merged = if (existing.isBlank()) transcript else "$existing $transcript"
        _state.value = _state.value.copy(draft = merged, turnFromVoice = true)
        armAutoSend()
    }

    /**
     * Send in three seconds unless stopped.
     *
     * Speaking to a phone and then having to find and press a button is the
     * thing that makes voice input feel slower than typing. The countdown is
     * visible and cancelable so it never sends something half-said.
     */
    private fun armAutoSend() {
        autoSendJob?.cancel()
        autoSendJob = viewModelScope.launch {
            for (remaining in 3 downTo 1) {
                _state.value = _state.value.copy(autoSendIn = remaining)
                kotlinx.coroutines.delay(1000)
            }
            _state.value = _state.value.copy(autoSendIn = null)
            if (_state.value.draft.isNotBlank()) send()
        }
    }

    fun cancelAutoSend() {
        val hadPendingDictation = autoSendJob != null || _state.value.autoSendIn != null
        autoSendJob?.cancel()
        autoSendJob = null
        _state.value = _state.value.copy(autoSendIn = null)
        if (hadPendingDictation) concurrentVoice.inputSettled()
    }

    private var mentionItems: List<MentionItem> = MentionCatalog.build()

    fun selectProfile(name: String) {
        if (_state.value.profiles.none { it.name == name }) return
        val resolved = ChatModelPreference.resolve(_state.value.profiles, name, null)
        _state.value = _state.value.copy(selectedProfile = resolved.route, modelSelectionIssue = resolved.issue)
        if (resolved.issue == null) persistModelPreference(resolved)
    }

    private fun persistModelPreference(resolved: ChatModelPreferenceResolution) {
        val selection = resolved.selection ?: return
        chatChoice.edit()
            .putString(choiceKey(ChatModelPreference.storageKey), kotlinx.serialization.json.Json.encodeToString(ChatModelSelection.serializer(), selection))
            .apply()
    }

    private fun refreshModelChoices() {
        val generation = ++modelCatalogGeneration
        val sessionGeneration = sessionOpenGeneration
        val engine = _state.value.selectedHarnessEngine
        if (engine !in setOf("magician", "pi")) {
            _state.value = _state.value.copy(modelChoicesLoading = false)
            return
        }
        _state.value = _state.value.copy(modelChoicesLoading = true)
        viewModelScope.launch {
            try {
                val list = repository.models(engine).rows()
                if (generation != modelCatalogGeneration || sessionGeneration != sessionOpenGeneration) return@launch
                val raw = chatChoice.getString(choiceKey(ChatModelPreference.storageKey), null)
                    ?: chatChoice.getString(ChatModelPreference.storageKey, null)
                val saved = raw?.let { runCatching {
                    kotlinx.serialization.json.Json.decodeFromString(ChatModelSelection.serializer(), it)
                }.getOrNull() }
                val invalid = raw != null && (saved == null || saved.modelId.isBlank() || saved.thinking.isBlank())
                val resolved = ChatModelPreference.resolve(list, _state.value.selectedProfile, saved)
                _state.value = _state.value.copy(profiles = list,
                    selectedProfile = if (invalid) ChatModelPreference.unavailableRoute else resolved.route,
                    modelSelectionIssue = if (invalid) ChatModelPreference.unavailableMessage else resolved.issue,
                    modelChoicesLoading = false)
                if (!invalid && resolved.issue == null) persistModelPreference(resolved)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                if (generation != modelCatalogGeneration || sessionGeneration != sessionOpenGeneration) return@launch
                _state.value = _state.value.copy(modelChoicesLoading = false,
                    modelSelectionIssue = "Model choices could not be loaded. Reconnect and try again.")
            }
        }
    }

    fun selectHarnessEngine(name: String) {
        if (_state.value.chatHarnesses.none { it.name == name && it.installed }) return
        if (_state.value.selectedHarnessEngine == name) return
        _state.value = _state.value.copy(selectedHarnessEngine = name, selectedHarnessModel = "default")
        chatChoice.edit().putString(choiceKey("engine"), name).putString(choiceKey("model"), "default").apply()
        refreshModelChoices()
    }

    fun selectHarnessModel(model: String) {
        val available = _state.value.chatHarnesses.firstOrNull { it.name == _state.value.selectedHarnessEngine }
        if (available?.models?.contains(model) != true) return
        _state.value = _state.value.copy(selectedHarnessModel = model)
        chatChoice.edit().putString(choiceKey("model"), model).apply()
    }

    /**
     * Stage a file for the next turn.
     *
     * Staged immediately and marked uploading, so the owner sees the chip the
     * moment they pick something rather than after a round trip.
     */
    fun stageAttachment(name: String, mime: String, bytes: ByteArray) {
        val staged = StagedAttachment(
            java.util.UUID.randomUUID().toString(), name, mimeType = mime,
            // An image keeps its bytes for the composer's thumbnail (bounded).
            preview = bytes.takeIf { mime.startsWith("image/") && it.size <= 8 * 1024 * 1024 },
        )
        _state.value = _state.value.copy(attachments = _state.value.attachments + staged)
        viewModelScope.launch {
            val outcome = runCatching {
                // A file can be picked before the first message, so there may be
                // nothing to attach it to yet. Open the session rather than
                // refusing — the owner did nothing wrong by starting here.
                val sessionId = _state.value.activeSessionId ?: repository.newSession().also { id ->
                    inheritChoicesForCreatedSession(id)
                    this@ChatViewModel.sessionId = id
                    refreshModelChoices()
                    sessionSelection.remember(id)
                    _state.value = _state.value.copy(activeSessionId = id)
                }
                repository.uploadAttachment(sessionId, name, mime, bytes)
            }
            updateAttachment(staged.localId) {
                outcome.fold(
                    onSuccess = { id -> it.copy(uploading = false, remoteId = id, failed = false) },
                    onFailure = { problem ->
                        it.copy(
                            uploading = false,
                            failed = true,
                            error = problem.message ?: "The upload failed.",
                        )
                    },
                )
            }
        }
    }

    private fun updateAttachment(localId: String, transform: (StagedAttachment) -> StagedAttachment) {
        _state.value = _state.value.copy(
            attachments = _state.value.attachments.map {
                if (it.localId == localId) transform(it) else it
            },
        )
    }

    fun removeAttachment(localId: String) {
        _state.value = _state.value.copy(
            attachments = _state.value.attachments.filterNot { it.localId == localId },
        )
    }

    /**
     * Track the draft, and offer mentions while an `@` is still being typed.
     */
    fun onDraftChange(value: String) {
        // Editing the draft means the owner has taken over from the countdown.
        // Sending anyway, mid-word, is the worst possible moment.
        if (_state.value.autoSendIn != null) cancelAutoSend()

        // The trigger is detected by the same rules the web and iOS use, rather
        // than by looking for the last `@`: that found one in an email address
        // and left the picker open over the rest of the sentence.
        val trigger = MentionCatalog.detectTrigger(value)
        _state.value = _state.value.copy(
            draft = value,
            commandState = ComposerCommands.sync(value, _state.value.commandState),
            mentions = trigger?.let { MentionCatalog.matchesFor(mentionItems, it.query) }.orEmpty(),
            mentionTrigger = trigger,
        )
    }

    /**
     * Replace the half-typed `@…` with the token the backend reads.
     *
     * Only the trigger's own span is replaced, so a mention picked mid-sentence
     * leaves the words on either side of it alone.
     */
    fun pickMention(item: MentionItem) {
        val draft = _state.value.draft
        val trigger = _state.value.mentionTrigger ?: return
        val head = draft.dropLast(trigger.consume.coerceAtMost(draft.length))
        _state.value = _state.value.copy(
            draft = head + item.serialized() + " ",
            mentions = emptyList(),
            mentionTrigger = null,
        )
    }

    // MARK: slash commands (docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md)

    /** Walk the `/` picker, or run a picked option. Commands never reach the model. */
    fun pickCommandRow(row: ComposerCommandRow) {
        val current = _state.value.commandState
        when (val action = row.action) {
            is ComposerCommandAction.Open -> openCommand(action.id)
            ComposerCommandAction.Back -> applyCommandStep(ComposerCommands.back(current))
            ComposerCommandAction.ToggleEngineScope -> _state.value = _state.value.copy(
                commandState = current.copy(engineEverywhere = !current.engineEverywhere),
            )
            is ComposerCommandAction.PickBudgetTask ->
                applyCommandStep(ComposerCommands.pickTask(action.taskId, action.title, current))
            is ComposerCommandAction.OpenModels -> openModels(action.engine, action.run)
            is ComposerCommandAction.OpenThinking ->
                applyCommandStep(ComposerCommands.openThinking(action.engine, action.modelId, current))
            ComposerCommandAction.None, ComposerCommandAction.Unavailable -> Unit
            else -> {
                _state.value = _state.value.copy(
                    draft = "", commandState = ComposerCommandState(),
                    mentions = emptyList(), mentionTrigger = null,
                )
                runCommand(action)
            }
        }
    }

    fun consumeCommandNotice() {
        _state.value = _state.value.copy(commandNotice = null)
    }

    private fun applyCommandStep(step: ComposerCommandStep) {
        _state.value = _state.value.copy(
            draft = step.text, commandState = step.state, mentions = emptyList(), mentionTrigger = null,
        )
    }

    private fun openCommand(id: ComposerCommandId) {
        applyCommandStep(ComposerCommands.open(id))
        // Refresh what the options mark as current.
        when (id) {
            ComposerCommandId.BudgetDefault -> viewModelScope.launch {
                runCatching { repository.commandCall("GET", "/budgets/defaults") }.fold(
                    onSuccess = { body ->
                        val defaults = BudgetDefaults.decode(body)
                        _state.value = _state.value.copy(
                            budgetDefaults = defaults ?: _state.value.budgetDefaults,
                            budgetDefaultsError = if (defaults == null) "The server's budget defaults could not be read." else null,
                        )
                    },
                    onFailure = { problem ->
                        if (problem is CancellationException) throw problem
                        _state.value = _state.value.copy(budgetDefaultsError = problem.message ?: "The defaults could not be read.")
                    },
                )
            }
            ComposerCommandId.Engine -> viewModelScope.launch { refreshCommandRoster() }
            ComposerCommandId.RunEngine -> viewModelScope.launch {
                // The "Now" note names the run engine's model, which lives with that engine.
                when (refreshCommandRoster()?.current ?: _state.value.runHarnessEngine) {
                    "magician" -> loadLoopModels()
                    "pi" -> loadCommandCatalog("pi")
                    else -> Unit
                }
            }
            ComposerCommandId.Budget, ComposerCommandId.VoiceMode -> Unit
        }
    }

    /** `/engine <engine>` or `/run-engine <engine>`: open its models and read them. */
    private fun openModels(engine: String, run: Boolean) {
        applyCommandStep(ComposerCommands.openModels(engine, run, _state.value.commandState))
        when {
            run && engine == "magician" -> loadLoopModels()
            engine in ComposerCommands.catalogEngines -> loadCommandCatalog(engine)
        }
    }

    /** The roster's engines, run engine and chat default, for what `/engine` and `/run-engine` mark. */
    private suspend fun refreshCommandRoster(): ChatHarnessRoster? {
        val roster = repository.engineRoster() ?: return null
        val installed = roster.engines.filter { it.installed }
        _state.value = _state.value.copy(
            chatHarnesses = installed.ifEmpty { _state.value.chatHarnesses },
            runHarnessEngine = roster.current,
            runHarnessModel = roster.runModel,
            runFollowsChat = roster.followsChat,
            runModelSelection = roster.runModelSelection,
            serverChatEngine = roster.chatCurrent ?: _state.value.serverChatEngine,
        )
        return roster
    }

    /** `GET /chat/models?engine=` for a model level, without touching the composer's own choice. */
    private fun loadCommandCatalog(engine: String) {
        _state.value = _state.value.copy(commandModelErrors = _state.value.commandModelErrors - engine)
        viewModelScope.launch {
            runCatching { repository.models(engine).models }.fold(
                onSuccess = { models ->
                    _state.value = _state.value.copy(
                        commandModelCatalogs = _state.value.commandModelCatalogs + (engine to models),
                        commandModelErrors = _state.value.commandModelErrors - engine,
                    )
                },
                onFailure = { problem ->
                    if (problem is CancellationException) throw problem
                    _state.value = _state.value.copy(
                        commandModelErrors = _state.value.commandModelErrors +
                            (engine to (problem.message ?: "Model choices could not be loaded.")),
                    )
                },
            )
        }
    }

    /** `GET /llm/routing`: the `agentic_decision` models and the one mapped now (Settings' loop model). */
    private fun loadLoopModels() {
        _state.value = _state.value.copy(loopModelsError = null)
        viewModelScope.launch {
            runCatching { repository.commandCall("GET", "/llm/routing") }.fold(
                onSuccess = { body ->
                    val routing = LoopModelRouting.decode(body)
                    _state.value = if (routing.error == null) {
                        _state.value.copy(loopModels = routing.models, loopSelection = routing.selection, loopModelsError = null)
                    } else {
                        _state.value.copy(loopModelsError = routing.error)
                    }
                },
                onFailure = { problem ->
                    if (problem is CancellationException) throw problem
                    _state.value = _state.value.copy(loopModelsError = problem.message ?: "Model routing is unavailable.")
                },
            )
        }
    }

    private fun postCommandNotice(text: String, isError: Boolean = false) {
        _state.value = _state.value.copy(commandNotice = ComposerCommandNotice(text, isError))
    }

    /**
     * The composer's engine and model, set the way its own pickers set them:
     * an engine change goes through [selectHarnessEngine] so the model choices
     * load for the new engine, and a catalog [selection] is saved first so that
     * load resolves it (or, on the same engine, picked with [selectProfile]).
     */
    private fun setDeviceEngine(engine: String, model: String, selection: ChatModelSelection?) {
        if (selection != null) {
            chatChoice.edit()
                .putString(choiceKey(ChatModelPreference.storageKey), kotlinx.serialization.json.Json.encodeToString(ChatModelSelection.serializer(), selection))
                .apply()
        }
        if (_state.value.selectedHarnessEngine != engine) {
            selectHarnessEngine(engine)
            if (_state.value.selectedHarnessEngine != engine) {
                throw ChatError("${ComposerCommands.engineDisplayName(engine)} is not installed on this Magician.")
            }
        } else if (selection != null) {
            val route = _state.value.profiles.mapNotNull { it.modelChoice }
                .firstOrNull { it.id == selection.modelId }?.choices?.get(selection.thinking)
            if (route != null && _state.value.profiles.any { it.name == route }) selectProfile(route) else refreshModelChoices()
        }
        if (engine !in ComposerCommands.catalogEngines) selectHarnessModel(model)
    }

    private fun runCommand(action: ComposerCommandAction) {
        viewModelScope.launch {
            try {
                when (action) {
                    is ComposerCommandAction.SetChatEngine -> {
                        // Read before the switch: the catalog's labels name the picked model.
                        val label = action.selection?.let {
                            ComposerCommands.modelLabel(_state.value.commandContext().catalog(action.engine), it)
                        } ?: action.model
                        setDeviceEngine(action.engine, action.model, action.selection)
                        if (action.everywhere) {
                            runCatching {
                                repository.commandCall("PUT", "/plane/chat-engine", ComposerCommands.engineBody(action.engine, action.model))
                            }.onFailure { problem ->
                                if (problem is CancellationException) throw problem
                                postCommandNotice("Set on this device only: ${problem.message.orEmpty()}", isError = true)
                                return@launch
                            }
                            _state.value = _state.value.copy(serverChatEngine = action.engine)
                        }
                        postCommandNotice(
                            ComposerCommands.chatEngineConfirmation(action.engine, label.takeIf { !action.everywhere }, action.everywhere),
                        )
                    }
                    is ComposerCommandAction.SetRunEngine -> {
                        val label = when {
                            action.selection != null ->
                                ComposerCommands.modelLabel(_state.value.commandContext().catalog(action.engine), action.selection)
                            // Pi without a selection is the coding agent's own default.
                            action.engine == "pi" -> "Default"
                            else -> action.model
                        }
                        repository.commandCall(
                            "PUT", "/plane/engine", ComposerCommands.runEngineBody(action.engine, action.model, action.selection),
                        )
                        val followsChat = action.engine == ComposerCommands.sameAsChat
                        _state.value = _state.value.copy(
                            runHarnessEngine = if (followsChat) _state.value.runHarnessEngine else action.engine,
                            runHarnessModel = action.model, runFollowsChat = followsChat,
                            runModelSelection = if (action.engine == "pi") action.selection else _state.value.runModelSelection,
                        )
                        refreshCommandRoster()
                        postCommandNotice(ComposerCommands.runEngineConfirmation(action.engine, label))
                    }
                    is ComposerCommandAction.SetLoopModel -> {
                        val label = ComposerCommands.modelLabel(_state.value.loopModels, action.selection)
                        // Settings' loop model belongs to Magician's own loop: pin the run engine to it first
                        // unless agentic flows already resolve to Magician.
                        refreshCommandRoster()
                        if (!ComposerCommands.runResolvesToMagician(_state.value.commandContext())) {
                            repository.commandCall("PUT", "/plane/engine", ComposerCommands.engineBody("magician", "default"))
                        }
                        repository.commandCall(
                            "PUT", "/llm/routing/${LoopModelRouting.operation}", ComposerCommands.loopModelBody(action.selection),
                        )
                        _state.value = _state.value.copy(loopSelection = action.selection)
                        refreshCommandRoster()
                        postCommandNotice(ComposerCommands.runEngineConfirmation("magician", label))
                    }
                    is ComposerCommandAction.SetVoiceMode -> {
                        voicePrefs.setComposerVoiceMode(action.mode)
                        postCommandNotice(ComposerCommands.voiceModeConfirmation(action.mode))
                    }
                    is ComposerCommandAction.GrantBudget -> {
                        val path = "/tasks/${java.net.URLEncoder.encode(action.taskId, Charsets.UTF_8.name())}/budget"
                        val body = repository.commandCall("POST", path, action.grant.body())
                        postCommandNotice(ComposerCommands.grantConfirmation(action.grant, action.title, ComposerCommands.resumed(body)))
                    }
                    is ComposerCommandAction.SetBudgetDefault -> {
                        val body = repository.commandCall(
                            "PUT", "/budgets/defaults",
                            ComposerCommands.defaultsBody(action.kind, action.cap, _state.value.budgetDefaults),
                        )
                        val known = BudgetDefaults.decode(body) ?: _state.value.budgetDefaults ?: BudgetDefaults()
                        val next = if (action.kind == BudgetRunKind.Harness) {
                            known.copy(harness = known.harness ?: action.cap)
                        } else {
                            known.copy(native = known.native ?: action.cap)
                        }
                        _state.value = _state.value.copy(budgetDefaults = next)
                        postCommandNotice(ComposerCommands.defaultConfirmation(action.kind, action.cap))
                    }
                    else -> Unit
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                postCommandNotice(problem.message ?: "The command failed.", isError = true)
            }
        }
    }

    /**
     * Follow the realtime bus for as long as this screen exists.
     *
     * Started once rather than per session: the socket is shared by every
     * surface and reconnecting it on each conversation switch would drop events
     * during the gap. Frames for other sessions are discarded when they are
     * applied, not when they arrive.
     */
    private fun followRealtime() {
        if (realtimeJob?.isActive == true) return
        realtimeJob = viewModelScope.launch {
            repository.chatEvents().collect(::applyRealtime)
        }
    }

    /**
     * Re-read the open transcript after the realtime hub missed events.
     *
     * Merged, not replaced: a message streaming or sent optimistically on this
     * device is newer than the read and must survive it. Skipped while a
     * session is still opening — that read is already the fresh one.
     */
    private fun resyncTranscript() {
        val forSession = sessionId ?: return
        if (sessionOpenJob?.isActive == true) return
        viewModelScope.launch {
            val snapshot = runCatching { repository.conversationSnapshot(forSession) }.getOrNull() ?: return@launch
            if (sessionId != forSession) return@launch
            val history = snapshot.messages
            var messages = _state.value.messages
            history.forEach { messages = mergeRealtimeMessage(messages, it) }
            _state.value = _state.value.copy(messages = settleTranscriptActivity(messages))
            applyTurnStateReport(forSession, snapshot.turnState, TurnStateSource.Transcript)
            refreshQueue()
            history.lastOrNull { !it.fromUser && it.chatTurnId != null }
                ?.chatTurnId?.let(::hydrateTurnActivity)
        }
    }

    private fun applyRealtime(event: ChatRealtimeEvent) {
        val current = sessionId
        when (event) {
            is ChatRealtimeEvent.MessageReceived -> {
                // Another session's message is not ours to show.
                if (event.sessionId != null && event.sessionId != current) return
                val projected = event.message.project(_state.value.messages.size)
                val merged = mergeRealtimeMessage(_state.value.messages, projected)
                _state.value = _state.value.copy(
                    // A terminal task card settles its task's step strip even
                    // when the strip's own terminal event never arrived.
                    messages = if (projected.task?.terminal == true) settleTranscriptActivity(merged) else merged,
                )
                // A finished assistant message is the moment its steps become
                // worth reading; fetch them from the canonical projection.
                if (!projected.fromUser) projected.chatTurnId?.let(::hydrateTurnActivity)
            }

            is ChatRealtimeEvent.TurnCompleted -> {
                if (event.sessionId != null && event.sessionId != current) return
                // Only clears a *streaming* placeholder. A turn this device is
                // actively streaming ends on its own SSE `done`; this covers
                // the turns nobody here started.
                // A turn the server still reports running keeps its live
                // bubble; its own settled state ends it.
                val stillRunning = event.chatTurnId?.let { turnStates[it]?.running } == true
                if (streamJob?.isActive != true && !stillRunning) {
                    _state.value = _state.value.copy(
                        messages = _state.value.messages.map {
                            if (it.streaming && it.chatTurnId == event.chatTurnId) it.copy(streaming = false) else it
                        },
                        sending = false,
                    )
                }
                event.chatTurnId?.let(::hydrateTurnActivity)
            }

            is ChatRealtimeEvent.TurnStateChanged -> {
                val forSession = current ?: return
                if (event.sessionId != null && event.sessionId != forSession) return
                applyTurnState(forSession, event.state, TurnStateSource.Live)
            }

            is ChatRealtimeEvent.PlanningStarted -> Unit // status only; the card arrives as a message.

            is ChatRealtimeEvent.Resync -> resyncTranscript()

            is ChatRealtimeEvent.ShellOutput -> {
                if (event.lines.isEmpty()) return
                val messages = _state.value.messages
                val index = shellTargetIndex(messages, event.executionId)
                if (index < 0) return
                val card = messages[index].task ?: return
                // Capped from the newest end: the block is a window on a live
                // run, not an archive, and an hour of build output must not
                // become the transcript's largest object.
                _state.value = _state.value.copy(
                    messages = messages.toMutableList().also { list ->
                        list[index] = list[index].copy(
                            task = card.copy(
                                terminalLines = (card.terminalLines + event.lines)
                                    .takeLast(MAX_TERMINAL_LINES),
                            ),
                        )
                    },
                )
            }
        }
    }

    /** Open a different conversation, replacing what is on screen. */
    fun clearMessageFocus() { _state.value = _state.value.copy(focusedMessageId = null) }

    fun openOriginalAnswer(link: OriginalAnswerLink) = openSession(link.origin.sessionId, link)

    fun openSession(id: String, target: OriginalAnswerLink? = null) {
        concurrentVoice.select(null)
        if (id == sessionId && target == null) {
            sessionSelection.remember(id)
            return
        }
        sessionOpenJob?.cancel()
        val generation = ++sessionOpenGeneration
        stopSpeaking()
        streamJob?.cancel()
        activityJob?.cancel()
        resultReadJob?.cancel()
        resetTurnTracking()
        resetOlderHistory()
        sessionId = id
        refreshModelChoices()
        _state.value = _state.value.copy(
            messages = emptyList(), loading = true, sending = false,
            historyHasMore = false, olderHistoryLoading = false, olderHistoryError = null,
            queuedMessages = emptyList(), queueSessionId = null, serverRunning = false,
            error = null, focusedMessageId = null, activeSessionId = id, completeResult = null,
        )
        sessionOpenJob = viewModelScope.launch {
            runCatching { repository.conversationSnapshot(id, target) }
                .onSuccess { snapshot ->
                    val metadata = snapshot.summary
                    val history = snapshot.messages
                    if (sessionId != id || generation != sessionOpenGeneration) return@onSuccess
                    sessionSelection.remember(id)
                    historyCursor = snapshot.oldestMessageId
                    _state.value = _state.value.copy(
                        messages = history, loading = false, selectedSession = metadata,
                        historyHasMore = snapshot.hasMore,
                        focusedMessageId = history.firstOrNull { it.linkedAnswerTarget }?.id,
                        error = if (target != null && history.none { it.linkedAnswerTarget })
                            "The original answer is no longer available in this conversation." else null,
                    )
                    val viewed = history.firstOrNull { it.linkedAnswerTarget }?.id
                    val receipt = concurrentVoice.state.value.requests.firstOrNull {
                        it.branchSessionId == id && viewed != null && it.resultMessageId == viewed
                    }
                    if (receipt != null && receipt.readAt == null) viewModelScope.launch {
                        // A missing/pruned receipt must not break a durable source link.
                        runCatching { concurrentVoice.markRead(receipt.id) }
                    }
                    // The newest turn is the one a reopened chat is read for;
                    // hydrate its steps. Older bubbles keep the collapsed
                    // header and fill in if their turn is refreshed live.
                    history.lastOrNull { m -> !m.fromUser && m.chatTurnId != null }
                        ?.chatTurnId?.let(::hydrateTurnActivity)
                    // A turn still running there shows live, with its steps.
                    applyTurnStateReport(id, snapshot.turnState, TurnStateSource.Transcript)
                }
                .onFailure {
                    if (it is kotlinx.coroutines.CancellationException) return@launch
                    if (sessionId != id || generation != sessionOpenGeneration) return@onFailure
                    if (it is ChatError && it.failure.kind == FailureKind.NotFound) {
                        sessionSelection.forget(id)
                    }
                    _state.value = _state.value.copy(
                        loading = false, error = it.message ?: "Could not open that chat.",
                    )
                }
            loadCatalog(id)
        }
    }

    /**
     * Read one page older than the first loaded message and put it on top.
     *
     * The screen calls this when the reader scrolls the first message into
     * view. One page per call: guarded by [canLoadOlderHistory], and a reply
     * for a chat the screen has since left is dropped.
     */
    fun loadOlderMessages() {
        val forSession = sessionId ?: return
        val before = historyCursor ?: return
        if (!_state.value.canLoadOlderHistory()) return
        val generation = sessionOpenGeneration
        olderHistoryJob?.cancel()
        // A linked answer's focus would otherwise re-scroll to it on prepend.
        _state.value = _state.value.copy(olderHistoryLoading = true, olderHistoryError = null, focusedMessageId = null)
        olderHistoryJob = viewModelScope.launch {
            fun stale() = sessionId != forSession || generation != sessionOpenGeneration || historyCursor != before
            try {
                val page = repository.olderMessages(forSession, before)
                if (stale()) return@launch
                val cursor = page.messages.firstOrNull()?.id
                historyCursor = cursor ?: before
                _state.value = _state.value.copy(
                    messages = settleTranscriptActivity(
                        prependOlderMessages(_state.value.messages, projectOlderPage(page.messages, before)),
                    ),
                    historyHasMore = page.hasMore && cursor != null,
                    olderHistoryLoading = false,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (problem: Throwable) {
                if (stale()) return@launch
                _state.value = _state.value.copy(
                    olderHistoryLoading = false,
                    olderHistoryError = problem.message?.takeIf { it.isNotBlank() } ?: "Could not load earlier messages.",
                )
            }
        }
    }

    /** Forget the open chat's paging; the caller resets the matching state fields. */
    private fun resetOlderHistory() {
        olderHistoryJob?.cancel()
        olderHistoryJob = null
        historyCursor = null
    }

    /** Rebind after pairing; session IDs and captured content belong to the old connection. */
    fun onConnectionChanged() {
        concurrentVoice.reset()
        voiceInteracted = false
        _voiceResult.value = null
        cancelAutoSend()
        cancelDictation()
        stopSpeaking()
        configuredVoiceStartJob?.cancel()
        stopRealtimeVoice()
        streamJob?.cancel()
        activityJob?.cancel()
        resultReadJob?.cancel()
        startJob?.cancel()
        realtimeJob?.cancel()
        realtimeJob = null
        resetTurnTracking()
        resetOlderHistory()
        // The shared socket may still be open to the old host for other
        // surfaces; reopen it against the new pairing.
        repository.restartRealtime()
        sessionId = null
        val previous = _state.value
        _state.value = ChatUiState(
            draft = previous.draft,
            selectedProfile = previous.selectedProfile,
            selectedHarnessEngine = previous.selectedHarnessEngine,
            selectedHarnessModel = previous.selectedHarnessModel,
            composerMode = previous.composerMode,
            composerDoPermission = previous.composerDoPermission,
        )
        start()
        refreshVoiceCatalog()
    }

    /** Begin a new conversation without disturbing the old one. */
    fun newSession() {
        concurrentVoice.select(null)
        cancelAutoSend()
        stopSpeaking()
        streamJob?.cancel()
        activityJob?.cancel()
        resultReadJob?.cancel()
        resetTurnTracking()
        resetOlderHistory()
        sessionSelection.forget(sessionId)
        sessionId = null
        _state.value = _state.value.copy(
            messages = emptyList(),
            historyHasMore = false,
            olderHistoryLoading = false,
            olderHistoryError = null,
            draft = "",
            sending = false,
            error = null,
            failure = null,
            activeSessionId = null,
            completeResult = null,
        )
        start(forceNew = true)
    }

    fun clearSession() = mutateCurrentSession(ChatSessionAction.Clear)

    fun archiveSession() = mutateCurrentSession(ChatSessionAction.Archive)

    fun deleteSession() = mutateCurrentSession(ChatSessionAction.Delete)

    /**
     * Session actions settle on the server before changing local history.
     * Otherwise an offline archive/delete would appear to work until the next
     * refresh, and a failed clear would throw away a transcript that still
     * exists authoritatively.
     */
    private fun mutateCurrentSession(action: ChatSessionAction) {
        val id = sessionId ?: _state.value.activeSessionId ?: return
        if (_state.value.sessionActionInFlight != null) return
        // A dictated draft must not auto-send into a session while that
        // session is being cleared, archived, or deleted. The text itself is
        // retained for Clear and can still be sent deliberately afterwards.
        cancelAutoSend()
        _state.value = _state.value.copy(sessionActionInFlight = action, error = null)

        viewModelScope.launch {
            runCatching {
                when (action) {
                    ChatSessionAction.Clear -> repository.clearSession(id)
                    ChatSessionAction.Archive -> repository.archiveSession(id)
                    ChatSessionAction.Delete -> repository.deleteSession(id)
                }
            }.onSuccess {
                stopSpeaking()
                streamJob?.cancel()
                activityJob?.cancel()
                resultReadJob?.cancel()
                resetTurnTracking()
                resetOlderHistory()
                _state.value = _state.value.afterSessionAction(action, id)
                when (action) {
                    ChatSessionAction.Clear -> refreshSessions()
                    ChatSessionAction.Archive,
                    ChatSessionAction.Delete,
                    -> {
                        sessionSelection.forget(id)
                        sessionId = null
                        start(forceNew = true)
                    }
                }
            }.onFailure { problem ->
                if (problem is kotlinx.coroutines.CancellationException) throw problem
                val fallback = when (action) {
                    ChatSessionAction.Clear -> "Could not clear this chat."
                    ChatSessionAction.Archive -> "Could not archive this session."
                    ChatSessionAction.Delete -> "Could not delete this session."
                }
                _state.value = _state.value.copy(
                    sessionActionInFlight = null,
                    error = problem.message?.takeIf { it.isNotBlank() } ?: fallback,
                )
            }
        }
    }

    private suspend fun loadCatalog(id: String) {
        val catalog = repository.referenceCatalog(id)
        mentionItems = MentionCatalog.build(agents = catalog.agents, skills = catalog.skills)
    }

    private fun refreshSessions() {
        viewModelScope.launch {
            runCatching { repository.sessions() }
                .onSuccess { sessions ->
                    val selectedId = sessionId
                    // A restored voice branch is addressable but absent from
                    // the ordinary list. Recover its saved navigation title.
                    val selected = if (selectedId != null && sessions.none { it.identifier() == selectedId }) {
                        _state.value.selectedSession?.takeIf { it.identifier() == selectedId }
                            ?: runCatching { repository.conversation(selectedId).first }.getOrNull()
                    } else null
                    _state.value = _state.value.copy(
                        sessions = sessions,
                        selectedSession = selected?.takeIf { it.identifier() == sessionId },
                    )
                }
        }
    }

    /**
     * Open the last surviving session and load its transcript.
     *
     * A transient read failure is never treated as absence: creating in that
     * branch was the rebuild/reopen bug, because every offline launch added a
     * conversation. Only a confirmed missing remembered id, an empty
     * authoritative session list, or an explicit New Session action creates.
     */
    fun start(forceNew: Boolean = false) {
        followRealtime()
        startJob?.cancel()
        startJob = viewModelScope.launch {
            _state.value = _state.value.copy(
                loading = true,
                error = null,
                failure = null,
                setupRequired = false,
            )
            try {
                var turnState = ChatTurnStateReport.UNSUPPORTED
                var hasMore = false
                var cursor: String? = null
                val resolved = resolveChatSessionStartup(
                    forceNew = forceNew,
                    currentSessionId = sessionId,
                    rememberedSessionId = sessionSelection.remembered(),
                    findExistingSession = repository::mostRecentActivePersonalSessionId,
                    readHistory = { id ->
                        repository.conversationSnapshot(id).also {
                            turnState = it.turnState
                            hasMore = it.hasMore
                            cursor = it.oldestMessageId
                        }.messages
                    },
                    createSession = repository::newSession,
                    onRememberedMissing = sessionSelection::forget,
                    onCreated = { created -> inheritChoicesForCreatedSession(created); sessionSelection.remember(created) },
                )
                val id = resolved.sessionId
                val history = resolved.history
                sessionId = id
                sessionSelection.remember(id)
                resetOlderHistory()
                historyCursor = cursor
                _state.value = _state.value.copy(
                    messages = history, loading = false, activeSessionId = id,
                    historyHasMore = hasMore, olderHistoryLoading = false, olderHistoryError = null,
                )
                history.lastOrNull { m -> !m.fromUser && m.chatTurnId != null }
                    ?.chatTurnId?.let(::hydrateTurnActivity)
                // Relaunch: a turn the server kept running shows live again.
                applyTurnStateReport(id, turnState, TurnStateSource.Transcript)
                loadCatalog(id)
                refreshSessions()
                val harnesses = repository.chatHarnesses()
                if (harnesses.isNotEmpty()) {
                    val engine = _state.value.selectedHarnessEngine.takeIf { selected ->
                        harnesses.any { it.name == selected }
                    } ?: "magician"
                    val models = harnesses.firstOrNull { it.name == engine }?.models.orEmpty()
                    val model = _state.value.selectedHarnessModel.takeIf { it in models } ?: "default"
                    _state.value = _state.value.copy(
                        chatHarnesses = harnesses,
                        selectedHarnessEngine = engine,
                        selectedHarnessModel = model,
                    )
                    chatChoice.edit().putString(choiceKey("engine"), engine).putString(choiceKey("model"), model).apply()
                }
                refreshModelChoices()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Throwable) {
                val failure = error.toFailure(getApplication(), "this conversation")
                _state.value = _state.value.copy(
                    loading = false,
                    failure = failure,
                    setupRequired = failure.setupRequired,
                )
            }
        }
    }

    fun setComposerMode(mode: ChatComposerMode) {
        if (_state.value.composerMode == mode) return
        _state.value = _state.value.withComposerMode(mode)
    }

    fun openCompleteResult(row: ActivityRow) {
        val resultRef = row.resultRef?.takeIf { it.isNotBlank() } ?: return
        val hostSessionId = sessionId
        resultReadJob?.cancel()
        _state.value = _state.value.copy(
            completeResult = CompleteResultViewerState(
                resultRef = resultRef,
                title = row.label,
                contentHash = row.resultHash,
            ),
        )
        resultReadJob = viewModelScope.launch {
            runCatching { repository.completeResult(hostSessionId, row) }
                .onSuccess { result ->
                    if (_state.value.completeResult?.resultRef == resultRef) {
                        _state.value = _state.value.copy(
                            completeResult = CompleteResultViewerState(
                                resultRef = resultRef,
                                title = row.label,
                                loading = false,
                                text = result.text,
                                contentHash = result.contentHash,
                            ),
                        )
                    }
                }
                .onFailure { problem ->
                    if (problem is CancellationException) throw problem
                    if (_state.value.completeResult?.resultRef == resultRef) {
                        _state.value = _state.value.copy(
                            completeResult = CompleteResultViewerState(
                                resultRef = resultRef,
                                title = row.label,
                                loading = false,
                                error = problem.message ?: "The complete result could not be read.",
                                contentHash = row.resultHash,
                            ),
                        )
                    }
                }
        }
    }

    fun closeCompleteResult() {
        resultReadJob?.cancel()
        resultReadJob = null
        _state.value = _state.value.copy(completeResult = null)
    }

    fun send(background: Boolean = false, stopAndSend: Boolean = false) {
        // A composer holding exactly a command opens its options instead of
        // posting it to the model.
        ComposerCommands.exactCommand(_state.value.draft)?.let { command ->
            openCommand(command)
            return
        }
        // An open `/` level (`/engine pi son`) keeps the picker: never post it.
        if (ComposerCommands.holdsSend(_state.value.draft, _state.value.commandState)) return
        val text = _state.value.draft.trim()
        val mode = _state.value.composerMode
        val harnessEngine = _state.value.selectedHarnessEngine
        val harnessModel = _state.value.selectedHarnessModel
        val modelSelection = _state.value.profiles.firstOrNull { it.name == _state.value.selectedProfile }
            ?.modelSelection.takeIf { harnessEngine in setOf("magician", "pi") }
        val id = sessionId
        val voiceInput = _state.value.turnFromVoice
        // A spoken turn only branches when it would otherwise wait behind a
        // running turn, or when the owner picked a voice topic to continue.
        // An idle session takes it as an ordinary chat turn.
        val voiceBranches = voiceInput && (_state.value.sending || _state.value.serverRunning ||
            concurrentVoice.state.value.focus != null)
        val concurrent = (background || voiceBranches) && mode != ChatComposerMode.Plan &&
            _state.value.attachments.isEmpty() && !startsTutor(text) && !BrainstormInvoke.isInvoke(text)
        if (!concurrent && voiceInput) concurrentVoice.inputSettled()
        if (text.isEmpty() || (_state.value.queueMutationInFlight || (_state.value.sending && mode == ChatComposerMode.Plan)) || _state.value.sessionActionInFlight != null) return
        stopSpeaking()

        // Native lane: @brainstorm opens a thinking map seeded with whatever
        // followed it, and is deliberately not posted as a chat turn — the map
        // owns its own facilitator session, and sending the text as well would
        // start a second conversation about the same thought.
        if (BrainstormInvoke.isInvoke(text)) {
            _state.value = _state.value.copy(draft = "", turnFromVoice = false)
            _brainstormSeed.value = BrainstormInvoke.strip(text)
            return
        }
        if (harnessEngine in setOf("magician", "pi") &&
            (_state.value.modelChoicesLoading || _state.value.modelSelectionIssue != null || modelSelection == null)) {
            _state.value = _state.value.copy(error = _state.value.modelSelectionIssue
                ?: "Model choices are still loading. Try again in a moment.")
            if (voiceInput) concurrentVoice.inputSettled()
            return
        }
        if (id == null) {
            // No session yet — open one, then the owner can send again rather
            // than having the message silently dropped.
            start()
            return
        }

        if (concurrent) {
            voiceInteracted = voiceInteracted || _state.value.turnFromVoice
            _state.value = _state.value.copy(draft = "", turnFromVoice = false)
            concurrentVoice.action {
                try {
                    val options = ChatModelPreference.requestFields(modelSelection, harnessEngine, harnessModel).toMutableMap()
                    options["source_surface"] = kotlinx.serialization.json.JsonPrimitive("android")
                    mode.wire?.let { options["mode"] = kotlinx.serialization.json.JsonPrimitive(it) }
                    concurrentVoice.submit(id, text, kotlinx.serialization.json.JsonObject(options), voiceInput = voiceInput)
                    refreshSessions()
                } catch (error: Exception) {
                    if (sessionId == id && _state.value.draft.isBlank()) _state.value = _state.value.copy(draft = text)
                    throw error
                }
            }
            return
        }

        // Read before the state is cleared below. Only uploaded files travel; a
        // failed chip must not be sent as though it had worked.
        val attachmentIds = _state.value.attachments.mapNotNull { it.remoteId }

        if ((_state.value.sending || _state.value.serverRunning || stopAndSend) && mode != ChatComposerMode.Plan) {
            val attachments = _state.value.attachments
            _state.value = _state.value.copy(draft = "", attachments = emptyList(), turnFromVoice = false, queueMutationInFlight = true)
            viewModelScope.launch {
                var admitted = false
                try {
                    val queuedId = repository.enqueue(id, SendMessageRequest(
                        text = text, chatTurnId = UUID.randomUUID().toString(), modelSelection = modelSelection,
                        harnessEngine = harnessEngine, harnessModel = harnessModel.takeUnless { modelSelection != null }, attachmentIds = attachmentIds,
                        mode = mode.wire, sourceSurface = "android", continueOnDisconnect = true,
                    ))
                    admitted = true
                    if (stopAndSend) repository.queueAction(id, queuedId, "stop_and_send")
                } catch (error: Exception) {
                    if (sessionId == id) _state.value = _state.value.copy(
                        error = error.message, draft = if (!admitted && _state.value.draft.isBlank()) text else _state.value.draft,
                        attachments = if (!admitted) attachments else _state.value.attachments,
                    )
                } finally { _state.value = _state.value.copy(queueMutationInFlight = false); refreshQueue() }
            }
            return
        }

        val spokenTurn = _state.value.turnFromVoice
        val chatTurnId = UUID.randomUUID().toString()
        val userMessage = optimisticUserMessage(
            id = UUID.randomUUID().toString(),
            text = text,
            voiceOrigin = spokenTurn,
            chatTurnId = chatTurnId,
        )
        val replyId = UUID.randomUUID().toString()
        val placeholder = liveAssistantPlaceholder(replyId, chatTurnId)

        _state.value = _state.value.copy(
            // The message appears immediately. Waiting for the server to echo it
            // makes the app feel broken on a slow connection.
            messages = _state.value.messages + userMessage + placeholder,
            draft = "",
            sending = true,
            error = null,
            attachments = emptyList(),
            // Stamp this turn only. Leaving the flag set would make the next
            // typed send look spoken after a failed stream.
            turnFromVoice = false,
        )

        // A lesson begins the moment the turn is sent, not when the first shape
        // arrives. The socket has to be listening before the backend starts
        // drawing, or the opening steps land with nobody watching.
        if (startsTutor(text)) beginLesson()

        followTurnActivity(
            forSession = id,
            chatTurnId = chatTurnId,
        )
        localStreamTurnId = chatTurnId

        streamJob = viewModelScope.launch {
            var streamError: Throwable? = null
            var dropped = false
            repository.send(
                sessionId = id,
                text = text,
                chatTurnId = chatTurnId,
                modelSelection = modelSelection,
                harnessEngine = harnessEngine,
                harnessModel = harnessModel,
                attachmentIds = attachmentIds,
                // A tutor turn announces the canvas that will draw it. Anything
                // else announces the plain surface and draws nothing.
                sourceSurface = if (startsTutor(text)) {
                    tutorRouter.sourceSurface()
                } else {
                    "android"
                },
                // Read before the flag is cleared below, so the stamp belongs
                // to the turn that was actually spoken.
                voiceOrigin = spokenTurn,
                mode = mode.wire,
            )
                // Not yet a failure: the stream is gone, the turn may not be.
                .catch { error -> streamError = error }
                .collect { event ->
                    if (event is ChatStreamEvent.Done && event.dropped) dropped = true else apply(replyId, event)
                }
            if (localStreamTurnId == chatTurnId) localStreamTurnId = null
            // What the server said about this turn while the stream owned it,
            // applied now that the stream is done.
            val heard = turnStates.remove(chatTurnId)
            _state.value = _state.value.copy(sending = false)
            val problem = streamError
            if ((problem != null || dropped) && recoverDroppedTurn(id, replyId, chatTurnId, problem, heard)) {
                return@launch
            }
            updateReply(replyId) { it.copy(streaming = false) }
            activityJob?.cancel()
            activityJob = null
            hydrateTurnActivity(chatTurnId)
            // A turn stopped elsewhere ends this stream `cancelled`, with no reply.
            heard?.takeUnless { it.running }?.let { applyTurnState(id, it, TurnStateSource.Live) }
        }
    }

    /** Stop a turn in flight. Cancelling collection cancels the request. */
    /**
     * Whether this turn starts a lesson.
     *
     * The same marker the backend matches, checked here only to decide which
     * surface to announce — the backend still owns whether a lesson actually
     * starts. Getting this wrong costs a wrongly-announced surface, not a
     * wrongly-started lesson.
     */
    /**
     * Open a run and start listening for its events.
     *
     * The run is published so a surface can show it. Which surface — board or
     * overlay — is the router's answer, and it has already been announced to
     * the backend by the time this is called.
     */
    /**
     * Open the blackboard without a turn to hang it on.
     *
     * The launcher shortcut's entry point, matching iOS's blackboard intent:
     * the board comes up ready for a concept rather than waiting for somebody
     * to type `@tutor` first.
     */
    fun startBlackboard() {
        if (_tutorRun.value == null) beginLesson()
    }

    private fun beginLesson() {
        val run = tutorRouter.begin()
        _tutorRun.value = run
        tutorRealtime.follow(viewModelScope, run)
        viewModelScope.launch {
            // Cleared when the run says it is done, so the board comes down on
            // the backend's word rather than on a guess about timing.
            run.finishedFlow.collect { done ->
                if (done) {
                    _tutorRun.value = null
                    tutorRealtime.stop()
                    tutorRouter.end()
                }
            }
        }
    }

    /**
     * Teach about what is on screen right now.
     *
     * Captures first, then sends — the tutor resolves its targets from the
     * image, so a turn sent before the capture lands would be answered about
     * nothing. A failed capture teaches on a blackboard rather than refusing:
     * the question was still asked.
     */
    fun teachThisScreen(
        question: String = "Explain what is on my screen",
        /** `@copilot` acts on the app; `@tutor` draws over it. */
        act: Boolean = false,
    ) {
        viewModelScope.launch {
            val captured = ai.magicbeans.magdroid.tutor.TutorScreenGrab.grab()
            if (!captured) {
                tutorRouter.choose(ai.magicbeans.magdroid.tutor.TutorSurface.Blackboard)
            }
            // Sent through the draft, so this takes the same path a typed
            // "@tutor …" does — one send to keep correct rather than two.
            // The app is named from the capture rather than asked for. The
            // phone knew which app was in front at the moment of the request,
            // and making somebody type it is a question with an answer already
            // in the room.
            val app = ai.magicbeans.magdroid.tutor.ScreenCapture.latest.value?.packageName
            val marker = if (act) "@copilot" else "@tutor"
            val inApp = app?.let { " in $it" }.orEmpty()
            _state.value = _state.value.copy(draft = "$marker $question$inApp")
            send()
        }
    }

    /** End a lesson early, when the owner closes the board. */
    fun endLesson() {
        _tutorRun.value = null
        tutorRouter.end()
        viewModelScope.launch { tutorRealtime.stop() }
    }

    /**
     * Whether this turn opens a lesson.
     *
     * The rule lives in [TutorInvoke] so the composer, the spoken grammar and
     * the web agree on it. It used to be a bare `contains`, which opened a
     * blackboard for any sentence that mentioned the tutor in passing — and for
     * `@copilot`, which addresses the Pilot agent and draws nothing.
     */
    private fun startsTutor(text: String): Boolean = TutorInvoke.isTutorInvoke(text)

    /** Tell the account this handset changed its mind about spoken replies. */
    fun publishSpeakReplies(enabled: Boolean) {
        viewModelScope.launch { repository.putMediaPreferences(enabled) }
    }

    /** Read one assistant message aloud, or stop it when tapped again. */
    fun toggleMessageSpeech(messageId: String, text: String) {
        if (_state.value.speakingMessageId == messageId) {
            stopSpeaking()
        } else {
            startSpeaking(messageId, text)
        }
    }

    /** Stop speaking a reply, without touching the turn. */
    fun stopSpeaking() {
        speech.stop()
        if (_state.value.speakingMessageId != null) {
            _state.value = _state.value.copy(speakingMessageId = null)
        }
    }

    private fun startSpeaking(messageId: String, text: String) {
        // During a live call the call is the only voice: no spoken reply, no
        // dictation speak-back and no Speak button, as on iOS.
        if (!LiveCallAudioFocus.permitsLocalSpeech()) return
        concurrentVoice.foregroundStarted()
        _state.value = _state.value.copy(speakingMessageId = messageId)
        speech.speakForChat(text, sessionId) {
            if (_state.value.speakingMessageId == messageId) {
                _state.value = _state.value.copy(speakingMessageId = null)
            }
        }
    }

    fun startHandsFree() {
        val catalog = _voiceCatalog.value
        if (!catalog.handsFreeAvailable) {
            refreshVoiceCatalog()
            return
        }
        val selected = voicePrefs.audioProfiles.value[NativeAudioSurface.HandsFree]
        val profile = selected?.takeIf {
            catalog.audioProfileAvailable(NativeAudioSurface.HandsFree, it)
        } ?: catalog.defaultAudioProfiles[NativeAudioSurface.HandsFree.wire]?.takeIf {
            catalog.audioProfileAvailable(NativeAudioSurface.HandsFree, it)
        } ?: catalog.profilesFor(NativeAudioSurface.HandsFree).firstOrNull {
            catalog.audioProfileAvailable(NativeAudioSurface.HandsFree, it.first)
        }?.first
        if (profile == null) {
            refreshVoiceCatalog()
            return
        }
        voicePrefs.setAudioProfile(NativeAudioSurface.HandsFree, profile)
        val stages = voicePrefs.audioStageOptions.value
            .filterKeys { (surface, _) -> surface == NativeAudioSurface.HandsFree }
            .mapKeys { (key, _) -> key.second }
        realtime.start(
            uiThreadId = _state.value.activeSessionId,
            chatSessionId = _state.value.activeSessionId,
            engine = LiveVoiceEngine.HandsFree,
            audioProfile = profile,
            audioStageOptions = stages,
            pushToTalk = voicePrefs.livePttOn.value,
        )
    }

    fun stopHandsFree() = realtime.stop()

    /** Start the selected conversational voice mode, waiting for the catalog
     * on a cold app launch instead of turning the first system/widget tap into
     * a silent catalog refresh. */
    fun startConfiguredVoice() {
        configuredVoiceStartJob?.cancel()
        configuredVoiceStartJob = viewModelScope.launch {
            val selectedEngine = voicePrefs.liveEngine.value
            val supportsSelected: (RealtimeVoiceCatalog) -> Boolean = { catalog ->
                when (selectedEngine) {
                    LiveVoiceEngine.Realtime -> catalog.profiles.any { it.nativeAndAvailable }
                    LiveVoiceEngine.HandsFree -> catalog.handsFreeAvailable
                }
            }
            if (!supportsSelected(_voiceCatalog.value)) {
                refreshVoiceCatalog()
                withTimeoutOrNull(12_000) { _voiceCatalog.first(supportsSelected) }
                    ?: return@launch
            }
            when (selectedEngine) {
                LiveVoiceEngine.Realtime -> startRealtimeVoice()
                LiveVoiceEngine.HandsFree -> startHandsFree()
            }
        }
    }

    fun refreshVoiceCatalog() {
        viewModelScope.launch {
            runCatching { realtime.catalog() }.onSuccess { catalog ->
                _voiceCatalog.value = catalog
                voicePrefs.applyCatalog(catalog)
            }
        }
    }

    fun startRealtimeVoice() {
        val catalog = _voiceCatalog.value
        val selected = voicePrefs.realtimeProfile.value
        val profile = catalog.profiles.firstOrNull { it.id == selected && it.nativeAndAvailable }
            ?: catalog.profiles.firstOrNull { it.nativeAndAvailable }
        if (profile == null) {
            refreshVoiceCatalog()
            return
        }
        realtime.start(
            uiThreadId = _state.value.activeSessionId,
            chatSessionId = _state.value.activeSessionId,
            engine = LiveVoiceEngine.Realtime,
            realtimeProfile = profile,
            pushToTalk = resolveLivePushToTalk(
                voicePrefs.livePttOn.value,
                LiveVoiceEngine.Realtime,
                profile,
            ),
        )
    }

    fun selectRealtimeVoiceProfile(id: String) {
        val profile = _voiceCatalog.value.profiles.firstOrNull { it.id == id } ?: return
        voicePrefs.setRealtimeProfile(profile.id)
        voicePrefs.setLivePttOn(resolveLivePushToTalk(
            voicePrefs.livePttOn.value,
            LiveVoiceEngine.Realtime,
            profile,
        ))
    }

    fun setLiveVoicePushToTalk(enabled: Boolean) {
        val selected = _voiceCatalog.value.profiles.firstOrNull {
            it.id == voicePrefs.realtimeProfile.value
        }
        val resolved = resolveLivePushToTalk(
            enabled,
            voicePrefs.liveEngine.value,
            selected,
        )
        voicePrefs.setLivePttOn(resolved)
        // And the call in progress. This only wrote the preference, so a change
        // took effect on the *next* call: a room that stopped being quiet meant
        // ending this one. Resolved first, so a mode the engine refuses is not
        // applied live either.
        realtime.setPushToTalk(resolved)
    }

    fun toggleLiveVoiceMute() = realtime.toggleMute()
    fun engageLiveVoicePushToTalk() = realtime.engagePushToTalk()
    fun releaseLiveVoicePushToTalk() = realtime.releasePushToTalk()


    fun stopRealtimeVoice() { realtime.stop(); voiceInputActive = false; concurrentVoice.captureStopped(); concurrentVoice.inputSettled() }

    override fun onCleared() {
        concurrentVoice.close()
        super.onCleared()
        configuredVoiceStartJob?.cancel()
        activityJob?.cancel()
        speech.shutdown()
        dictation.close()
        realtime.close()
        repository.close()
    }

    fun stop() {
        stopSpeaking()
        streamJob?.cancel()
        streamJob = null
        localStreamTurnId = null
        val liveTurnId = _state.value.messages
            .lastOrNull { !it.fromUser && it.streaming }
            ?.chatTurnId
        activityJob?.cancel()
        activityJob = null
        val settled = _state.value.messages.map {
            if (it.streaming) it.copy(streaming = false) else it
        }
        // Said at once, as the server will say it: an empty bubble reads
        // "Stopped", a partial answer keeps its words. Recorded as settled so
        // a queue read racing the cancel cannot bring the bubble back; the
        // server's own state (or a reply that beat the Stop) still replaces it.
        val stopped = liveTurnId?.takeUnless(::isRealtimeVoiceTurn)?.let { turn ->
            ChatTurnState(turn, ChatTurnStatus.Stopped, reason = ChatTurnState.USER_STOP_REASON)
                .also { turnStates.remove(turn); turnStates[turn] = it }
        }
        _state.value = _state.value.copy(
            sending = false,
            serverRunning = false,
            messages = stopped?.let { applyChatTurnState(settled, it) } ?: settled,
        )
        liveTurnId?.let(::hydrateTurnActivity)
        // Then tell the server. Cancelling only the local job leaves the turn
        // running there — the words stop arriving but the work does not stop.
        val sessionId = _state.value.activeSessionId ?: return
        viewModelScope.launch { runCatching { repository.cancelRun(sessionId) } }
    }

    /**
     * Reconcile this handset's voice preferences with the account's.
     *
     * Read once at launch and seeded only if this device has never been told
     * otherwise, matching how iOS treats the same record. Failure is silent
     * and leaves local settings alone: a phone that cannot reach Magician
     * should keep the owner's choices, not revert to defaults and start
     * talking again.
     */
    private fun syncVoicePreferences() {
        viewModelScope.launch {
            val account = repository.mediaPreferences() ?: return@launch
            if (!voicePrefs.seedSpeakReplies(account.autoSpeak) &&
                account.autoSpeak != voicePrefs.speakReplies.value
            ) {
                // This device has an explicit choice the account has not heard.
                // Publishing it is what makes the last deliberate choice win
                // rather than whichever device happened to start last.
                repository.putMediaPreferences(voicePrefs.speakReplies.value)
            }
        }
    }

    /**
     * Answer an escalation from its card.
     *
     * The card shows the attempt while it is in flight and keeps its options if
     * it fails, because an escalation that silently refuses an answer leaves the
     * execution paused with nothing on screen saying so.
     */
    fun answerEscalation(
        messageId: String,
        option: EscalationOption? = null,
        text: String = "",
        selectedIds: List<String> = emptyList(),
    ) {
        val message = _state.value.messages.firstOrNull { it.id == messageId } ?: return
        val card = message.escalation ?: return
        val correlationId = card.correlationId
        if (correlationId.isNullOrBlank()) {
            updateEscalation(messageId) { it.copy(error = "This escalation cannot be answered from here.") }
            return
        }
        if (card.answering != null || card.resolved) return
        if (card.requiresReview || card.allowOther) {
            updateEscalation(messageId) { it.copy(error = "Open this request in Attention to answer it.") }
            return
        }

        val value = HitlResponseComposer.compose(
            inputType = card.inputType,
            option = option,
            text = text,
            selectedIds = selectedIds,
            sensitive = card.sensitive != null,
        )
        if (value == null) {
            // The form is not answerable yet. Said out loud rather than
            // swallowed: a submit that does nothing reads as a broken button.
            updateEscalation(messageId) { it.copy(error = "Add an answer before sending.") }
            return
        }

        // A key for the in-flight marker. Option id where there is one, the
        // input type otherwise, so a typed answer can still show it is sending.
        val pendingKey = option?.id ?: card.inputType ?: "answer"
        updateEscalation(messageId) { it.copy(answering = pendingKey, error = null) }
        viewModelScope.launch {
            val problem = runCatching {
                repository.respondToEscalation(
                    correlationId = correlationId,
                    value = value,
                    inputType = card.inputType,
                    executionId = card.executionId,
                )
            }.getOrElse { it.message ?: "Could not send the answer." }
            updateEscalation(messageId) {
                if (problem == null) it.copy(answering = null, resolved = true, error = null)
                else it.copy(answering = null, error = problem)
            }
        }
    }

    private fun updateEscalation(id: String, transform: (EscalationCard) -> EscalationCard) {
        _state.value = _state.value.copy(
            messages = _state.value.messages.map { message ->
                val card = message.escalation
                if (message.id == id && card != null) message.copy(escalation = transform(card))
                else message
            },
        )
    }

    private fun apply(replyId: String, event: ChatStreamEvent) {
        when (event) {
            is ChatStreamEvent.Token -> updateReply(replyId) { it.copy(text = it.text + event.text) }
            is ChatStreamEvent.ReasoningDelta -> updateReply(replyId) {
                // Kept out of `text` on purpose: reasoning is not the answer, and
                // splicing it in would leave it in the transcript afterwards.
                it.copy(reasoning = it.reasoning + event.text)
            }
            ChatStreamEvent.ReasoningEnd -> Unit
            is ChatStreamEvent.ToolCall -> updateReply(replyId) {
                it.copy(activity = it.activity + event.name)
            }
            is ChatStreamEvent.Failed -> updateReply(replyId) {
                it.copy(streaming = false, failed = true, text = it.text.ifEmpty { event.message })
            }
            is ChatStreamEvent.Done -> settle(replyId, event)
            is ChatStreamEvent.Ignored -> Unit
        }
    }

    /**
     * Replace the streamed placeholder with the turn as the backend settled it.
     *
     * The tokens were a preview. What `done` carries is the persisted truth —
     * the composed presentation, and any task or escalation card the turn
     * produced — so the placeholder is swapped for those rows rather than
     * merely stopped. Without this the live transcript disagreed with the same
     * conversation after a reload.
     */
    private fun settle(replyId: String, event: ChatStreamEvent.Done) {
        val current = _state.value
        val settled = event.messages.filter { !it.fromUser }
        val messages = settleChatMessages(current.messages, replyId, event)
        // A session names itself from its first exchange. The title arrives
        // once, on this frame, so dropping it leaves the drawer and the header
        // saying "Untitled" for a conversation that has a name.
        val sessions = event.sessionTitle?.let { title ->
            current.sessions.map { session ->
                if (session.identifier() == current.activeSessionId) session.copy(title = title)
                else session
            }
        } ?: current.sessions

        // Spoken back only when the turn was spoken. Reading a typed
        // conversation aloud unprompted is startling; iOS ties it to the same
        // voice-origin flag.
        // Muting is a standing preference, so it is checked at the moment of
        // speaking rather than baked into the turn.
        val spokenReply = if (current.turnFromVoice && voicePrefs.speakReplies.value) {
            settled.firstOrNull { it.kind == MessageKind.Text && it.text.isNotBlank() }
        } else {
            null
        }

        _state.value = current.copy(
            messages = messages,
            sessions = sessions,
            sending = false,
            turnFromVoice = false,
            // A queued message did not run: say so instead of showing an empty
            // reply as though the assistant had nothing to say.
            queuedPosition = event.queuedPosition,
        )
        spokenReply?.let { startSpeaking(it.id, it.text) }
    }

    private fun updateReply(id: String, transform: (ChatMessage) -> ChatMessage) {
        _state.value = _state.value.copy(
            messages = _state.value.messages.map { if (it.id == id) transform(it) else it },
        )
    }

    /**
     * Follow the durable turn projection while the reply is still running.
     *
     * One REST read of the turn's projection, then the turn's events off the
     * app's realtime hub keep the already-mounted response bubble current
     * before the first answer token. The send path cancels this follower and
     * performs one REST reconciliation at settlement.
     */
    private fun followTurnActivity(forSession: String, chatTurnId: String) {
        activityJob?.cancel()
        followedTurnId = chatTurnId
        activityJob = viewModelScope.launch {
            repository.turnActivityStream(forSession, chatTurnId).collect { rows ->
                if (sessionId == forSession && rows.isNotEmpty()) {
                    _state.value = _state.value.withTurnActivity(chatTurnId, rows)
                }
            }
        }
    }

    /** Where a turn state came from, which decides what else it may conclude. */
    private enum class TurnStateSource {
        /** Read with the transcript (open, relaunch, resync): the reply, if any, is already merged. */
        Transcript,
        /** Read with the queue: the transcript may lag it. */
        Queue,
        /** A `chat.turn.state_changed` event. */
        Live,
    }

    private fun resetTurnTracking() {
        turnStates.clear()
        localStreamTurnId = null
        followedTurnId = null
    }

    /**
     * Apply a snapshot's `turn_state`.
     *
     * The server names its latest turn, so no other turn of this session is
     * still running. `null` from a server that sends the field means it knows
     * no turn — it restarted — and, read with the transcript, a latest user
     * turn left without an answer was interrupted.
     */
    private fun applyTurnStateReport(forSession: String, report: ChatTurnStateReport, source: TurnStateSource) {
        if (!report.supported || sessionId != forSession) return
        val latest = report.state
        turnStates.entries.removeAll { (id, known) -> known.running && id != latest?.chatTurnId }
        if (latest != null) {
            applyTurnState(forSession, latest, source)
            return
        }
        if (source != TurnStateSource.Transcript || localStreamTurnId != null) return
        val current = _state.value
        _state.value = current.copy(messages = markInterruptedTurn(current.messages))
    }

    /**
     * Show one turn's server state.
     *
     * Running: a live bubble with its steps followed from the start, and Stop.
     * Settled: the reply, "Stopped", the watchdog's notice, or a failed or
     * interrupted line ([applyChatTurnState]). The turn this device is still
     * streaming is left to its stream, which settles it with the reply.
     */
    private fun applyTurnState(forSession: String, state: ChatTurnState, source: TurnStateSource) {
        if (sessionId != forSession || isRealtimeVoiceTurn(state.chatTurnId)) return
        val turn = state.chatTurnId
        val previous = turnStates[turn]
        if (previous == state) return
        // A settled turn never runs again: a running snapshot that raced the
        // end event is stale.
        if (previous != null && !previous.running && state.running) return
        turnStates.remove(turn)
        turnStates[turn] = state
        while (turnStates.size > MAX_TRACKED_TURNS) turnStates.remove(turnStates.keys.first())
        // Only noted while this device's stream still drives the turn; the
        // stream's end applies it.
        if (localStreamTurnId == turn) return

        val current = _state.value
        val messages = applyChatTurnState(current.messages, state)
        val anyRunning = turnStates.values.any { it.running }
        _state.value = current.copy(
            messages = messages,
            serverRunning = when {
                anyRunning -> true
                // A snapshot's own `active` also counts tails and queue drains.
                source != TurnStateSource.Live -> current.serverRunning
                else -> false
            },
        )
        if (state.running) {
            if (followedTurnId != turn || activityJob?.isActive != true) followTurnActivity(forSession, turn)
            return
        }
        val wasFollowing = followedTurnId == turn && activityJob?.isActive == true
        if (followedTurnId == turn) {
            activityJob?.cancel()
            activityJob = null
            followedTurnId = null
        }
        // The live follow ends with one canonical read, as a send's does.
        if (wasFollowing) hydrateTurnActivity(turn)
        // The reply (or the watchdog's notice) lands before the settled state;
        // one that has not reached this transcript is read from it.
        val answerExpected = state.status == ChatTurnStatus.Completed || state.stoppedByWatchdog
        if (source != TurnStateSource.Transcript && answerExpected && !hasServerReply(messages, turn)) {
            resyncTranscript()
        }
        if (source == TurnStateSource.Live && current.serverRunning) viewModelScope.launch { refreshQueue() }
    }

    /**
     * The send stream ended without settling its turn — the radio dropped, the
     * app was backgrounded, or the connection closed. The server keeps a turn
     * sent with `continue_on_disconnect`, so it is asked before the turn is
     * called failed. True when the server's state took over the bubble.
     */
    private suspend fun recoverDroppedTurn(
        forSession: String,
        replyId: String,
        chatTurnId: String,
        problem: Throwable?,
        /** The turn's state as last heard during the stream, if any. */
        heard: ChatTurnState?,
    ): Boolean {
        // A dropped radio usually drops this read too; give it a moment back.
        var queue: ChatQueueSnapshot? = null
        for (wait in RECOVERY_RETRY_DELAYS_MS) {
            if (wait > 0) kotlinx.coroutines.delay(wait)
            if (sessionId != forSession) return true
            queue = try {
                repository.queueSnapshot(forSession)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Throwable) {
                null
            }
            if (queue != null) break
        }
        if (sessionId != forSession) return true
        // What was heard during the stream was set aside, so the snapshot is
        // applied fresh: live, settled, or not this turn at all.
        if (queue != null) applyQueueSnapshot(forSession, queue)
        if (queue?.turnState?.state?.chatTurnId == chatTurnId) return true
        // Unreachable, but the turn was heard from during the stream: trust
        // that over calling it failed.
        if (queue == null && heard != null) {
            applyTurnState(forSession, heard, TurnStateSource.Live)
            return true
        }
        // The server cannot say (unreachable, or too old to report turns).
        // A broken stream reads as failed until a transcript read or the
        // turn's reply says otherwise; a clean end just stops the bubble.
        if (problem != null) {
            updateReply(replyId) {
                it.copy(
                    streaming = false,
                    failed = true,
                    text = it.text.ifEmpty { problem.message ?: "The turn failed." },
                )
            }
        }
        return false
    }

    /**
     * Hydrate a historical response when its bubble enters the viewport.
     *
     * iOS uses the same lazy boundary. Loading only the newest turn at session
     * open left every older Android response without its Steps disclosure.
     */
    fun loadActivityIfNeeded(messageId: String, chatTurnId: String) {
        val message = _state.value.messages.firstOrNull { it.id == messageId } ?: return
        if (
            message.fromUser ||
            message.kind != MessageKind.Text ||
            message.chatTurnId != chatTurnId ||
            message.activityRows.isNotEmpty()
        ) return
        hydrateTurnActivity(chatTurnId)
    }

    /**
     * Fetch one turn's steps from the canonical projection and attach them to
     * the turn's messages.
     *
     * This used to be an `ActivityDelta` realtime arm reading `rows` off
     * `ExecutionPanelDelta` — fields that frame has never carried, so the
     * "What happened" section never drew a single row. Rows live behind
     * `GET .../turns/{id}/events`, the same endpoint web and iOS read.
     *
     * Guarded twice against a session switch: the fetch is keyed to the
     * session it started under, and the result is dropped if the screen has
     * moved on — steps from one conversation must not dress another's bubble.
     */
    private fun hydrateTurnActivity(chatTurnId: String) {
        val forSession = sessionId ?: return
        if (!activityLoadsInFlight.add(chatTurnId)) return
        viewModelScope.launch {
            try {
                val rows = runCatching { repository.turnActivity(forSession, chatTurnId) }
                    .getOrDefault(emptyList())
                if (rows.isEmpty() || sessionId != forSession) return@launch
                _state.value = _state.value.withTurnActivity(chatTurnId, rows)
            } finally {
                activityLoadsInFlight.remove(chatTurnId)
            }
        }
    }

    private companion object {
        /**
         * The terminal block keeps this many of the newest lines. Enough to
         * scroll back through what just happened; bounded so a chatty run
         * cannot grow one message without limit.
         */
        const val MAX_TERMINAL_LINES = 500

        /** Turn states kept for the open conversation; only the newest matter. */
        const val MAX_TRACKED_TURNS = 32

        /** Waits before each read of a dropped turn's state. */
        val RECOVERY_RETRY_DELAYS_MS = longArrayOf(0, 2_000, 5_000)
    }
}
