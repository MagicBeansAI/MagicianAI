package ai.magicbeans.magdroid.chat

import ai.magicbeans.magdroid.net.Failure
import ai.magicbeans.magdroid.net.FailureKind
import ai.magicbeans.magdroid.net.toFailure
import ai.magicbeans.magdroid.voice.Dictation
import ai.magicbeans.magdroid.voice.Speech
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
    val messages: List<ChatMessage> = emptyList(),
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
    val chatHarnesses: List<ChatHarnessOption> = listOf(ChatHarnessOption("magician", true)),
    val selectedHarnessEngine: String = "magician",
    val selectedHarnessModel: String = "default",
    val attachments: List<StagedAttachment> = emptyList(),
    /** The active Do-or-Plan mode. Dictation uses this too, so it cannot live only in the composer. */
    val composerMode: ChatComposerMode = ChatComposerMode.Ask,
    /** Do's Ask/Accept permission remains parked while Plan is active. */
    val composerDoPermission: ChatDoPermission = ChatDoPermission.Ask,
    val completeResult: CompleteResultViewerState? = null,
)

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
    )

/** Attach canonical Steps to the latest text response for this turn, as iOS does. */
internal fun ChatUiState.withTurnActivity(
    chatTurnId: String,
    rows: List<ActivityRow>,
): ChatUiState {
    val owner = messages.indexOfLast { message ->
        !message.fromUser && message.kind == MessageKind.Text && message.chatTurnId == chatTurnId
    }
    if (owner < 0) return this
    return copy(
        messages = messages.mapIndexed { index, message ->
            if (index == owner) {
                message.copy(activityRows = rows)
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
        sending = false,
        queuedPosition = null,
        error = null,
        sessionActionInFlight = null,
    )

    ChatSessionAction.Archive,
    ChatSessionAction.Delete,
    -> copy(
        messages = emptyList(),
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
            automaticPlayback = { voiceInteracted || realtime.state.value.active || voicePrefs.speakReplies.value },
            outputBusy = { _state.value.sending || _state.value.serverRunning || _state.value.draft.isNotBlank() ||
                _state.value.dictation != DictationState.Idle || voiceInputActive ||
                realtime.concurrentOutputBusy || speech.speaking.value },
            play = speech::speakManaged,
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
        runCatching { repository.queueSnapshot(id) }.onSuccess { queue ->
            if (sessionId == id) _state.value = _state.value.copy(
                queuedMessages = queue.queued, queueSessionId = id, serverRunning = queue.active,
                queuedPosition = null,
            )
        }
    }

    fun actOnQueue(messageId: String, action: String) {
        val id = sessionId ?: return
        if (_state.value.queueMutationInFlight) return
        _state.value = _state.value.copy(queueMutationInFlight = true)
        viewModelScope.launch {
            try {
                if (action == "remove") repository.removeQueued(id, messageId)
                else repository.queueAction(id, messageId, action)
            } catch (error: Exception) {
                if (sessionId == id) _state.value = _state.value.copy(error = error.message)
            } finally { _state.value = _state.value.copy(queueMutationInFlight = false); refreshQueue() }
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

    private var sessionId: String? = null
    private var streamJob: Job? = null
    private var activityJob: Job? = null
    private val activityLoadsInFlight = mutableSetOf<String>()
    private var resultReadJob: Job? = null
    private var startJob: Job? = null
    private var sessionOpenJob: Job? = null
    private var sessionOpenGeneration = 0L

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
        dictation.start(
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
        _state.value = _state.value.copy(selectedProfile = name)
        chatChoice.edit().putString("profile", name).apply()
    }

    fun selectHarnessEngine(name: String) {
        if (_state.value.chatHarnesses.none { it.name == name && it.installed }) return
        if (_state.value.selectedHarnessEngine == name) return
        _state.value = _state.value.copy(selectedHarnessEngine = name, selectedHarnessModel = "default")
        chatChoice.edit().putString("engine", name).putString("model", "default").apply()
    }

    fun selectHarnessModel(model: String) {
        val available = _state.value.chatHarnesses.firstOrNull { it.name == _state.value.selectedHarnessEngine }
        if (available?.models?.contains(model) != true) return
        _state.value = _state.value.copy(selectedHarnessModel = model)
        chatChoice.edit().putString("model", model).apply()
    }

    /**
     * Stage a file for the next turn.
     *
     * Staged immediately and marked uploading, so the owner sees the chip the
     * moment they pick something rather than after a round trip.
     */
    fun stageAttachment(name: String, mime: String, bytes: ByteArray) {
        val staged = StagedAttachment(java.util.UUID.randomUUID().toString(), name)
        _state.value = _state.value.copy(attachments = _state.value.attachments + staged)
        viewModelScope.launch {
            val outcome = runCatching {
                // A file can be picked before the first message, so there may be
                // nothing to attach it to yet. Open the session rather than
                // refusing — the owner did nothing wrong by starting here.
                val sessionId = _state.value.activeSessionId ?: repository.newSession().also { id ->
                    this@ChatViewModel.sessionId = id
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

    private fun applyRealtime(event: ChatRealtimeEvent) {
        val current = sessionId
        when (event) {
            is ChatRealtimeEvent.MessageReceived -> {
                // Another session's message is not ours to show.
                if (event.sessionId != null && event.sessionId != current) return
                val projected = event.message.project(_state.value.messages.size)
                _state.value = _state.value.copy(
                    messages = mergeRealtimeMessage(_state.value.messages, projected),
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
                if (streamJob?.isActive != true) {
                    _state.value = _state.value.copy(
                        messages = _state.value.messages.map {
                            if (it.streaming && it.chatTurnId == event.chatTurnId) it.copy(streaming = false) else it
                        },
                        sending = false,
                    )
                }
                event.chatTurnId?.let(::hydrateTurnActivity)
            }

            is ChatRealtimeEvent.PlanningStarted -> Unit // status only; the card arrives as a message.

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
        sessionId = id
        _state.value = _state.value.copy(
            messages = emptyList(), loading = true, sending = false,
            queuedMessages = emptyList(), queueSessionId = null, serverRunning = false,
            error = null, focusedMessageId = null, activeSessionId = id, completeResult = null,
        )
        sessionOpenJob = viewModelScope.launch {
            runCatching { repository.conversation(id, target) }
                .onSuccess { (metadata, history) ->
                    if (sessionId != id || generation != sessionOpenGeneration) return@onSuccess
                    sessionSelection.remember(id)
                    _state.value = _state.value.copy(
                        messages = history, loading = false, selectedSession = metadata,
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
        sessionId = null
        val previous = _state.value
        _state.value = ChatUiState(
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
        sessionSelection.forget(sessionId)
        sessionId = null
        _state.value = _state.value.copy(
            messages = emptyList(),
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
                val resolved = resolveChatSessionStartup(
                    forceNew = forceNew,
                    currentSessionId = sessionId,
                    rememberedSessionId = sessionSelection.remembered(),
                    findExistingSession = repository::mostRecentActivePersonalSessionId,
                    readHistory = repository::history,
                    createSession = repository::newSession,
                    onRememberedMissing = sessionSelection::forget,
                    onCreated = sessionSelection::remember,
                )
                val id = resolved.sessionId
                val history = resolved.history
                sessionId = id
                sessionSelection.remember(id)
                _state.value = _state.value.copy(
                    messages = history, loading = false, activeSessionId = id,
                )
                history.lastOrNull { m -> !m.fromUser && m.chatTurnId != null }
                    ?.chatTurnId?.let(::hydrateTurnActivity)
                loadCatalog(id)
                refreshSessions()
                runCatching { repository.profiles() }.onSuccess { list ->
                    _state.value = _state.value.copy(
                        profiles = list,
                        selectedProfile = _state.value.selectedProfile.takeIf { selected ->
                            list.any { it.name == selected }
                        } ?: (list.firstOrNull { it.isDefault } ?: list.firstOrNull())?.name.orEmpty(),
                    )
                    chatChoice.edit().putString("profile", _state.value.selectedProfile).apply()
                }
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
                    chatChoice.edit().putString("engine", engine).putString("model", model).apply()
                }
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
        val text = _state.value.draft.trim()
        val mode = _state.value.composerMode
        val harnessEngine = _state.value.selectedHarnessEngine
        val harnessModel = _state.value.selectedHarnessModel
        val profile = _state.value.selectedProfile.takeIf {
            it.isNotBlank() && harnessEngine in setOf("magician", "pi")
        }
        val id = sessionId
        val voiceInput = _state.value.turnFromVoice
        val concurrent = (background || voiceInput) && mode != ChatComposerMode.Plan &&
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
                    val options = ai.magicbeans.magdroid.voice.ConcurrentVoiceCoordinator.fields(
                        "source_surface" to "android",
                        "harness_engine" to harnessEngine, "harness_model" to harnessModel,
                    ).toMutableMap()
                    mode.wire?.let { options["mode"] = kotlinx.serialization.json.JsonPrimitive(it) }
                    profile?.let { options["profile"] = kotlinx.serialization.json.JsonPrimitive(it) }
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
                        text = text, chatTurnId = UUID.randomUUID().toString(), profile = profile,
                        harnessEngine = harnessEngine, harnessModel = harnessModel, attachmentIds = attachmentIds,
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

        streamJob = viewModelScope.launch {
            repository.send(
                sessionId = id,
                text = text,
                chatTurnId = chatTurnId,
                profile = profile,
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
                .catch { error ->
                    updateReply(replyId) {
                        it.copy(
                            streaming = false,
                            failed = true,
                            text = it.text.ifEmpty { error.message ?: "The turn failed." },
                        )
                    }
                }
                .collect { event -> apply(replyId, event) }
            _state.value = _state.value.copy(sending = false)
            updateReply(replyId) { it.copy(streaming = false) }
            activityJob?.cancel()
            activityJob = null
            hydrateTurnActivity(chatTurnId)
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
        concurrentVoice.foregroundStarted()
        _state.value = _state.value.copy(speakingMessageId = messageId)
        speech.speak(text) {
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
                val selected = voicePrefs.realtimeProfile.value
                val valid = catalog.profiles.any { it.id == selected && it.nativeAndAvailable }
                if (!valid) {
                    val fallback = catalog.profiles.firstOrNull {
                        it.id == catalog.defaultProfileId && it.nativeAndAvailable
                    } ?: catalog.profiles.firstOrNull { it.nativeAndAvailable }
                    fallback?.let { voicePrefs.setRealtimeProfile(it.id) }
                }
                val resolvedRealtime = catalog.profiles.firstOrNull {
                    it.id == voicePrefs.realtimeProfile.value && it.nativeAndAvailable
                }
                resolvedRealtime?.let { profile ->
                    val providerPttDefault = profile.mode != "translation" &&
                        profile.turnDetectionMode?.lowercase() == "none"
                    voicePrefs.seedLivePttOn(providerPttDefault)
                    if (profile.mode == "translation" && voicePrefs.livePttOn.value) {
                        voicePrefs.setLivePttOn(false)
                    }
                }
                NativeAudioSurface.entries.forEach { surface ->
                    val candidates = catalog.profilesFor(surface)
                    val current = voicePrefs.audioProfiles.value[surface]
                    val selected = current?.takeIf { id ->
                        candidates.any { it.first == id } && catalog.audioProfileAvailable(surface, id)
                    }
                        ?: catalog.defaultAudioProfiles[surface.wire]?.takeIf { id ->
                            candidates.any { it.first == id } && catalog.audioProfileAvailable(surface, id)
                        }
                        ?: candidates.firstOrNull {
                            catalog.audioProfileAvailable(surface, it.first)
                        }?.first
                    selected?.let { voicePrefs.setAudioProfile(surface, it) }
                    val profile = selected?.let(catalog.audioProfiles::get) ?: return@forEach
                    voicePrefs.audioStageOptions.value
                        .filterKeys { (candidateSurface, _) -> candidateSurface == surface }
                        .forEach { (key, optionId) ->
                            val stage = key.second
                            val option = catalog.optionsFor(stage).firstOrNull { it.id == optionId }
                            if (option == null || !profile.supports(option, stage)) {
                                voicePrefs.setAudioStageOption(surface, stage, null)
                            }
                        }
                }
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
        val liveTurnId = _state.value.messages
            .lastOrNull { !it.fromUser && it.streaming }
            ?.chatTurnId
        activityJob?.cancel()
        activityJob = null
        _state.value = _state.value.copy(
            sending = false,
            messages = _state.value.messages.map {
                if (it.streaming) it.copy(streaming = false) else it
            },
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
     * Android's realtime socket announces completion but does not carry the
     * projection rows. The canonical NDJSON stream keeps the already-mounted
     * response bubble current before the first answer token. The send path
     * cancels this follower and performs one REST reconciliation at settlement.
     */
    private fun followTurnActivity(forSession: String, chatTurnId: String) {
        activityJob?.cancel()
        activityJob = viewModelScope.launch {
            repository.turnActivityStream(forSession, chatTurnId).collect { rows ->
                if (sessionId == forSession && rows.isNotEmpty()) {
                    _state.value = _state.value.withTurnActivity(chatTurnId, rows)
                }
            }
        }
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
    }
}
