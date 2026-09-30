package ai.magicbeans.magdroid.voice

import ai.magicbeans.magdroid.access.MagicianAccess
import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.app.KeyguardManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import androidx.core.content.ContextCompat
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.HttpTimeoutConfig
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsText
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.websocket.Frame
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.websocket.readBytes
import io.ktor.websocket.readText
import java.net.URLEncoder
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

@Serializable
data class RealtimeVoiceProfile(
    @SerialName("profile_id") val id: String,
    val label: String,
    val provider: String,
    val model: String,
    val topology: String,
    val mode: String,
    @SerialName("turn_detection_mode") val turnDetectionMode: String? = null,
    val available: Boolean = false,
    @SerialName("unavailable_reason") val unavailableReason: String? = null,
) {
    val native: Boolean get() = topology == "backend_proxied"
    val nativeAndAvailable: Boolean get() = topology == "backend_proxied" && available
}

@Serializable
data class RealtimeVoiceCatalog(
    @SerialName("realtime_voice_profiles") val profiles: List<RealtimeVoiceProfile> = emptyList(),
    @SerialName("realtime_voice_default_profile") val defaultProfileId: String? = null,
    @SerialName("hands_free_voice") val handsFreeAvailable: Boolean = false,
    @SerialName("surface_profiles") val audioProfiles: Map<String, AudioSurfaceProfile> = emptyMap(),
    @SerialName("default_surface_profiles") val defaultAudioProfiles: Map<String, String> = emptyMap(),
    val stages: Map<String, List<AudioStageOption>> = emptyMap(),
) {
    fun profilesFor(surface: NativeAudioSurface): List<Pair<String, AudioSurfaceProfile>> =
        audioProfiles.entries.filter { it.value.surface == surface.wire }.map { it.toPair() }

    fun optionsFor(stage: NativeAudioStage): List<AudioStageOption> = stages[stage.wire].orEmpty()
}

data class RealtimeVoiceState(
    val phase: Phase = Phase.Idle,
    val engine: LiveVoiceEngine = LiveVoiceEngine.Realtime,
    val profileLabel: String? = null,
    val lastUserText: String = "",
    val lastAssistantText: String = "",
    val error: String? = null,
    val pushToTalk: Boolean = false,
    val pushToTalkHeld: Boolean = false,
    val muted: Boolean = false,
    /**
     * The assistant finished an utterance but is still on the request:
     * Gemini 3.8 Live Extended Thinking says "let me check…", runs a tool
     * without blocking, then answers, and that gap is silent. Driven by the
     * `interaction.status` envelope; engines without the signal never set it.
     */
    val assistantWorking: Boolean = false,
) {
    enum class Phase { Idle, Connecting, Reconnecting, Ready, Ending, Failed }
    val active: Boolean get() = phase == Phase.Connecting || phase == Phase.Reconnecting ||
        phase == Phase.Ready || phase == Phase.Ending

    /**
     * Apply an `interaction.status` payload. Only `in_progress` means working;
     * `idle` and anything unrecognised clear it, so a status the client does
     * not understand can never leave "Working…" on screen.
     */
    fun withInteractionStatus(status: String?): RealtimeVoiceState {
        val working = status?.trim()?.lowercase() == "in_progress"
        return if (working == assistantWorking) this else copy(assistantWorking = working)
    }
}

/**
 * The same two recovery budgets used by iOS: bootstrap gets more chances than
 * a call that was already live. Keeping the choice pure makes the most
 * important part of reconnect behaviour testable without an Android socket.
 */
internal object RealtimeReconnectPolicy {
    private val bootstrapDelays = longArrayOf(500, 1_500, 3_000, 5_000)
    private val liveDelays = longArrayOf(250, 750, 1_500)

    fun delayMillis(hasBeenReady: Boolean, attempt: Int): Long? =
        (if (hasBeenReady) liveDelays else bootstrapDelays).getOrNull(attempt)
}

/** Build the long-lived websocket client without using Ktor's invalid `0` timeout. */
internal fun createRealtimeVoiceHttpClient(): HttpClient = HttpClient(CIO) {
    install(WebSockets) { pingIntervalMillis = 15_000 }
    install(HttpTimeout) {
        connectTimeoutMillis = 20_000
        requestTimeoutMillis = 60_000
        socketTimeoutMillis = HttpTimeoutConfig.INFINITE_TIMEOUT_MS
    }
}

/**
 * Android's backend-proxied realtime voice transport.
 *
 * Text frames are control envelopes; binary frames are 24 kHz mono PCM16 in
 * both directions, exactly matching iOS and the web control socket. Audio is
 * bounded to one AudioRecord and one streaming AudioTrack. No full recording
 * or response is accumulated in memory.
 */
class RealtimeVoiceSession(private val context: Context) {
    var concurrentRequests = false
    var onConcurrentEvent: ((String, JsonObject) -> Unit)? = null
    @Volatile private var responsePending = false
    @Volatile private var playbackFrames = 0L
    @Volatile private var selectedContext: String? = null
    val concurrentOutputBusy: Boolean get() = responsePending || playback?.let {
        playbackFrames > (it.playbackHeadPosition.toLong() and 0xffffffffL)
    } == true

    fun selectConcurrentContext(id: String?) {
        selectedContext = id
        scope.launch { runCatching { socket?.send(Frame.Text(envelope("voice.context", buildJsonObject {
            put("context_session_id", id?.let { kotlinx.serialization.json.JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
        }))) } }
    }
    private val app = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val client = createRealtimeVoiceHttpClient()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val _state = MutableStateFlow(RealtimeVoiceState())
    val state: StateFlow<RealtimeVoiceState> = _state.asStateFlow()
    private var callJob: Job? = null
    @Volatile private var ending = false
    private var capture: AudioRecord? = null
    private var playback: AudioTrack? = null
    private var echoCanceler: AcousticEchoCanceler? = null
    private var noiseSuppressor: NoiseSuppressor? = null
    private var audioFocusRequest: AudioFocusRequest? = null
    private var previousAudioMode: Int? = null
    @Volatile private var socket: DefaultClientWebSocketSession? = null
    @Volatile private var pushToTalkEngaged = false
    @Volatile private var pushToTalkDesired = false
    private var pushToTalkStartedAt = 0L
    private val pttControl = Mutex()
    @Volatile private var microphoneMuted = false
    @Volatile private var hasBeenReadyThisCall = false
    private var reconnectAttempt = 0

    suspend fun catalog(): RealtimeVoiceCatalog {
        val response = client.get("${base()}/media/providers") { authorize() }
        if (!response.status.isSuccess()) throw VoiceMediaError("Voice providers are unavailable.")
        return runCatching {
            json.decodeFromString(RealtimeVoiceCatalog.serializer(), response.bodyAsText())
        }.getOrElse { throw VoiceMediaError("Voice providers returned an unreadable catalog.") }
    }

    fun start(
        uiThreadId: String?,
        engine: LiveVoiceEngine,
        realtimeProfile: RealtimeVoiceProfile? = null,
        audioProfile: String? = null,
        audioStageOptions: Map<NativeAudioStage, String> = emptyMap(),
        pushToTalk: Boolean = false,
    ) {
        if (_state.value.active) return
        if (engine == LiveVoiceEngine.Realtime && realtimeProfile?.nativeAndAvailable != true) {
            _state.value = RealtimeVoiceState(
                phase = RealtimeVoiceState.Phase.Failed,
                engine = engine,
                profileLabel = realtimeProfile?.label,
                error = realtimeProfile?.unavailableReason
                    ?: "This realtime profile is unavailable on Android.",
            )
            return
        }
        if (ContextCompat.checkSelfPermission(app, Manifest.permission.RECORD_AUDIO) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            _state.value = RealtimeVoiceState(
                phase = RealtimeVoiceState.Phase.Failed,
                engine = engine,
                error = "Magician needs permission to use the microphone.",
            )
            return
        }
        ending = false
        pushToTalkEngaged = false
        pushToTalkDesired = false
        microphoneMuted = false
        hasBeenReadyThisCall = false
        reconnectAttempt = 0
        _state.value = RealtimeVoiceState(
            phase = RealtimeVoiceState.Phase.Connecting,
            engine = engine,
            profileLabel = realtimeProfile?.label ?: audioProfile?.let(::audioProfileLabel),
            pushToTalk = pushToTalk,
        )
        callJob = scope.launch {
            var registeredSessionId: String? = null
            try {
                val id = registerSession(engine, audioProfile, audioStageOptions)
                registeredSessionId = id
                openControlWithRecovery(id, uiThreadId.orEmpty(), engine, realtimeProfile, pushToTalk)
                if (!ending && _state.value.phase != RealtimeVoiceState.Phase.Failed) {
                    _state.value = _state.value.copy(phase = RealtimeVoiceState.Phase.Idle)
                }
            } catch (_: CancellationException) {
                // Stop is an expected terminal path.
            } catch (error: Throwable) {
                if (!ending) {
                    _state.value = _state.value.copy(
                        phase = RealtimeVoiceState.Phase.Failed,
                        error = error.message ?: "The realtime call ended unexpectedly.",
                    )
                }
            } finally {
                releaseAudio()
                registeredSessionId?.let { id ->
                    withContext(NonCancellable) { disconnectSession(id) }
                }
                if (ending || _state.value.active) {
                    _state.value = RealtimeVoiceState(phase = RealtimeVoiceState.Phase.Idle)
                }
                callJob = null
            }
        }
    }

    /**
     * Re-open only the control transport. The media registration is the call's
     * durable identity and remains valid while a transient socket is restored;
     * registering again would leak sessions and split one call into several.
     */
    private suspend fun openControlWithRecovery(
        sessionId: String,
        uiThreadId: String,
        engine: LiveVoiceEngine,
        realtimeProfile: RealtimeVoiceProfile?,
        pushToTalk: Boolean,
    ) {
        while (!ending) {
            try {
                val serverEnded = openControl(
                    sessionId,
                    uiThreadId,
                    engine,
                    realtimeProfile,
                    pushToTalk,
                )
                if (serverEnded || ending) return
                throw VoiceMediaError("Voice control connection closed unexpectedly.")
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Throwable) {
                if (ending) return
                val wait = RealtimeReconnectPolicy.delayMillis(hasBeenReadyThisCall, reconnectAttempt)
                    ?: throw VoiceMediaError("Voice control connection could not be restored.")
                reconnectAttempt += 1
                // Re-open both audio endpoints only after the new socket says
                // ready. Until then no stale recorder can write into a dead
                // transport and no old provider audio remains queued.
                releaseAudio()
                _state.value = _state.value.copy(
                    phase = RealtimeVoiceState.Phase.Reconnecting,
                    error = "Connection interrupted. Reconnecting…",
                    pushToTalkHeld = false,
                    muted = false,
                )
                delay(wait)
            }
        }
    }

    fun stop() {
        if (!_state.value.active && _state.value.phase != RealtimeVoiceState.Phase.Failed) return
        ending = true
        val job = callJob
        if (job == null || job.isCompleted) {
            releaseAudio()
            callJob = null
            _state.value = RealtimeVoiceState(phase = RealtimeVoiceState.Phase.Idle)
            return
        }
        _state.value = _state.value.copy(phase = RealtimeVoiceState.Phase.Ending)
        job.cancel()
        releaseAudio()
    }

    private suspend fun openControl(
        sessionId: String,
        uiThreadId: String,
        engine: LiveVoiceEngine,
        realtimeProfile: RealtimeVoiceProfile?,
        pushToTalk: Boolean,
    ): Boolean {
        var serverEnded = false
        client.webSocket(
            urlString = controlUrl(sessionId),
            // Headers belong to the handshake, not to the open session: inside
            // the session lambda the receiver is the socket, and `header` there
            // resolves to nothing. Same shape as `TutorRealtime` and
            // `MagicianBridgeClient`, which is where this was drifting from.
            request = {
                MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
                val protocols = MagicianAccess.webSocketProtocols(
                    app,
                    listOf("magician-voice-control-v1"),
                )
                if (protocols.isNotEmpty()) {
                    header("Sec-WebSocket-Protocol", protocols.joinToString(", "))
                }
            },
        ) {
            socket = this
            val ready = CompletableDeferred<Unit>()
            val readyWatchdog = launch {
                delay(ReadyTimeoutMillis)
                if (!ready.isCompleted) {
                    throw VoiceMediaError("Magician opened the call but voice did not become ready in time.")
                }
            }
            val payload = buildJsonObject {
                put("ui_thread_id", uiThreadId.trim())
                realtimeProfile?.let { put("realtime_profile", it.id) }
                put("voice_mode", engine.wire)
                put("turn_boundary", if (pushToTalk) "push_to_talk" else "server_vad")
                put("echo_cancellation", true)
                put("screen_locked", (app.getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager)
                    ?.isDeviceLocked == true)
                put("require_voice_prefix", false)
                put("concurrent_requests", concurrentRequests)
            }
            send(Frame.Text(envelope("session.start", payload)))
            if (concurrentRequests) send(Frame.Text(envelope("voice.context", buildJsonObject {
                put("context_session_id", selectedContext?.let { kotlinx.serialization.json.JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
            })))
            var captureJob: Job? = null
            var assistantResponseId: String? = null
            try {
                for (frame in incoming) {
                    when (frame) {
                        is Frame.Binary -> writePlayback(frame.readBytes())
                        is Frame.Text -> {
                            val event = runCatching { json.parseToJsonElement(frame.readText()).jsonObject }
                                .getOrNull() ?: continue
                            val kind = event["kind"]?.jsonPrimitive?.contentOrNull.orEmpty()
                            val body = event["payload"] as? JsonObject ?: JsonObject(emptyMap())
                            if (concurrentRequests) {
                                when (kind) {
                                    "speech.started" -> responsePending = false
                                    "speech.stopped" -> responsePending = engine != LiveVoiceEngine.HandsFree
                                    "audio.output.ended", "response.interrupted", "voice.request.accepted" -> responsePending = false
                                    "interaction.status" -> responsePending = body["status"]?.jsonPrimitive?.contentOrNull == "in_progress"
                                }
                                onConcurrentEvent?.invoke(kind, body)
                            }
                            when (kind) {
                                "session.ready" -> if (captureJob == null) {
                                    ready.complete(Unit)
                                    openAudio()
                                    hasBeenReadyThisCall = true
                                    reconnectAttempt = 0
                                    // A fresh upstream session starts idle; a
                                    // "working" flag carried over from a rotated
                                    // session would never be cleared by it.
                                    _state.value = _state.value.copy(
                                        phase = RealtimeVoiceState.Phase.Ready,
                                        error = null,
                                        assistantWorking = false,
                                    )
                                    captureJob = launch(Dispatchers.IO) {
                                        val recorder = capture ?: return@launch
                                        val buffer = ByteArray(CaptureChunkBytes)
                                        while (isActive && recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING) {
                                            val count = recorder.read(buffer, 0, buffer.size)
                                            if (count > 0 && !microphoneMuted && (!_state.value.pushToTalk || pushToTalkEngaged)) {
                                                pttControl.withLock {
                                                    if (!_state.value.pushToTalk || pushToTalkEngaged) {
                                                        send(Frame.Binary(true, buffer.copyOf(count - count % 2)))
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                "transcript.user", "transcript.user.partial" -> {
                                    val text = body["text"]?.jsonPrimitive?.contentOrNull.orEmpty().trim()
                                    if (text.isNotEmpty()) _state.value = _state.value.copy(lastUserText = text)
                                }
                                "transcript.assistant", "transcript.assistant.delta" -> {
                                    val text = body["text"]?.jsonPrimitive?.contentOrNull.orEmpty().trim()
                                    if (text.isEmpty()) continue
                                    val responseId = body["response_id"]?.jsonPrimitive?.contentOrNull
                                    val merged = if (responseId != null && responseId == assistantResponseId) {
                                        mergeStreamingCaption(_state.value.lastAssistantText, text)
                                    } else {
                                        text
                                    }
                                    assistantResponseId = responseId ?: assistantResponseId
                                    _state.value = _state.value.copy(lastAssistantText = merged)
                                }
                                "response.interrupted" -> {
                                    flushPlayback()
                                    _state.value = _state.value.copy(assistantWorking = false)
                                }
                                "audio.output.ended" -> if (
                                    body["interrupted"]?.jsonPrimitive?.booleanOrNull == true
                                ) flushPlayback()
                                "interaction.status" -> _state.value = _state.value.withInteractionStatus(
                                    body["status"]?.jsonPrimitive?.contentOrNull,
                                )
                                "session.error" -> {
                                    val message = body["message"]?.jsonPrimitive?.contentOrNull
                                        ?: "The realtime provider reported an error."
                                    if (body["recoverable"]?.jsonPrimitive?.booleanOrNull == true) {
                                        _state.value = _state.value.copy(error = message)
                                    } else {
                                        throw VoiceMediaError(message)
                                    }
                                }
                                "session.ended" -> {
                                    serverEnded = true
                                    return@webSocket
                                }
                            }
                        }
                        else -> Unit
                    }
                }
            } finally {
                readyWatchdog.cancel()
                withContext(NonCancellable) {
                    runCatching {
                        withTimeout(1_000) {
                            send(Frame.Text(envelope("session.end", JsonObject(emptyMap()))))
                        }
                    }
                }
                captureJob?.cancelAndJoin()
                socket = null
            }
        }
        return serverEnded
    }

    /** Mute is a local PCM gate; no microphone bytes leave the phone. */
    fun toggleMute() {
        if (!_state.value.active || _state.value.pushToTalk) return
        microphoneMuted = !microphoneMuted
        _state.value = _state.value.copy(muted = microphoneMuted)
    }

    /** Send the PTT authority frame before opening the local PCM gate. */
    /**
     * Switch between open mic and hold-to-talk without leaving the call.
     *
     * The mode was fixed for the duration: it came from the preference at
     * connect time, and somebody who started open-mic in a quiet room had to
     * end the call and go to Settings when it stopped being quiet.
     *
     * Leaving push-to-talk releases a held mic first. Without that the capture
     * stays engaged in a mode that has no release gesture, and the microphone
     * is open with nothing on screen saying so.
     */
    fun setPushToTalk(enabled: Boolean) {
        if (_state.value.phase != RealtimeVoiceState.Phase.Ready) return
        if (_state.value.pushToTalk == enabled) return
        if (!enabled && pushToTalkDesired) releasePushToTalk()
        pushToTalkDesired = false
        _state.value = _state.value.copy(pushToTalk = enabled, pushToTalkHeld = false)
        if (enabled) {
            responsePending = false
            if (concurrentRequests) onConcurrentEvent?.invoke("input.cleared", JsonObject(emptyMap()))
        }
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            pttControl.withLock { runCatching {
                if (enabled) socket?.send(Frame.Text(envelope("input.clear", JsonObject(emptyMap()))))
                socket?.send(Frame.Text(envelope("session.turn_boundary", buildJsonObject {
                    put("turn_boundary", if (enabled) "push_to_talk" else "server_vad")
                })))
            }.onFailure { error -> _state.value = _state.value.copy(error = error.message) } }
        }
    }

    fun engagePushToTalk() {
        if (_state.value.phase != RealtimeVoiceState.Phase.Ready || !_state.value.pushToTalk || pushToTalkDesired) return
        pushToTalkDesired = true
        pushToTalkStartedAt = System.nanoTime()
        if (concurrentRequests) onConcurrentEvent?.invoke("speech.started", JsonObject(emptyMap()))
        _state.value = _state.value.copy(pushToTalkHeld = true)
        // Acquire ordering before returning to the caller. A quick release must
        // follow engage exactly once, including when the socket send suspends.
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            pttControl.withLock {
                runCatching {
                    val activeSocket = socket ?: throw VoiceMediaError("Voice connection is unavailable.")
                    activeSocket.send(Frame.Text(envelope("ptt.engage", JsonObject(emptyMap()))))
                    if (concurrentRequests) activeSocket.send(Frame.Text(envelope("speech.started", JsonObject(emptyMap()))))
                    pushToTalkEngaged = pushToTalkDesired
                }.onFailure { error ->
                    pushToTalkDesired = false; pushToTalkEngaged = false
                    if (concurrentRequests) onConcurrentEvent?.invoke("input.cleared", JsonObject(emptyMap()))
                    _state.value = _state.value.copy(pushToTalkHeld = false, error = error.message ?: "Hold to talk could not start.")
                }
            }
        }
    }

    /** Close the local gate before committing; discarded taps release the queue gate too. */
    fun releasePushToTalk() {
        if (!_state.value.pushToTalk || !pushToTalkDesired) return
        pushToTalkDesired = false; pushToTalkEngaged = false
        val discarded = concurrentRequests && System.nanoTime() - pushToTalkStartedAt < 180_000_000
        responsePending = !discarded && _state.value.engine != LiveVoiceEngine.HandsFree
        if (concurrentRequests) onConcurrentEvent?.invoke(if (discarded) "input.cleared" else "speech.stopped", JsonObject(emptyMap()))
        _state.value = _state.value.copy(pushToTalkHeld = false)
        scope.launch(start = CoroutineStart.UNDISPATCHED) {
            pttControl.withLock {
                runCatching {
                    socket?.send(Frame.Text(envelope(if (discarded) "input.clear" else "ptt.release", JsonObject(emptyMap()))))
                }.onFailure { error ->
                    responsePending = false
                    if (concurrentRequests) onConcurrentEvent?.invoke("input.cleared", JsonObject(emptyMap()))
                    _state.value = _state.value.copy(error = error.message ?: "The spoken turn could not be released.")
                }
            }
        }
    }

    private suspend fun registerSession(
        engine: LiveVoiceEngine,
        audioProfile: String?,
        audioStageOptions: Map<NativeAudioStage, String>,
    ): String {
        val body = buildJsonObject {
            put("surface_type", "web_mobile")
            put("transport", "websocket")
            put("display_label", "Magdroid Voice")
            put("user_agent", "Magdroid-Android")
            put("capabilities", buildJsonObject {
                put("mic", true); put("realtime_voice", true); put("text_bubble", true)
            })
            put("permissions", buildJsonObject { put("mic", "granted") })
            if (engine == LiveVoiceEngine.HandsFree) {
                put("audio_surface", NativeAudioSurface.HandsFree.wire)
                audioProfile?.let { put("audio_profile", it) }
                put("audio_stage_options", buildJsonObject {
                    audioStageOptions.forEach { (stage, option) -> put(stage.wire, option) }
                })
            }
        }
        val response = client.post("${base()}/media/sessions") {
            authorize()
            contentType(ContentType.Application.Json)
            setBody(body.toString())
        }
        if (!response.status.isSuccess()) {
            throw VoiceMediaError("Media session registration failed (HTTP ${response.status.value}).")
        }
        val decoded = json.parseToJsonElement(response.bodyAsText()).jsonObject
        return decoded["session"]?.jsonObject?.get("session_id")?.jsonPrimitive?.contentOrNull
            ?: decoded["session_id"]?.jsonPrimitive?.contentOrNull
            ?: throw VoiceMediaError("Media session registration returned no session id.")
    }

    private fun openAudio() {
        val inputMinimum = AudioRecord.getMinBufferSize(
            SampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val recorder = AudioRecord(
            MediaRecorder.AudioSource.VOICE_COMMUNICATION,
            SampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
            maxOf(inputMinimum, CaptureChunkBytes * 4),
        )
        if (recorder.state != AudioRecord.STATE_INITIALIZED) {
            recorder.release()
            throw VoiceMediaError("This phone could not open realtime microphone audio.")
        }
        echoCanceler = if (AcousticEchoCanceler.isAvailable()) {
            AcousticEchoCanceler.create(recorder.audioSessionId)?.apply { enabled = true }
        } else null
        noiseSuppressor = if (NoiseSuppressor.isAvailable()) {
            NoiseSuppressor.create(recorder.audioSessionId)?.apply { enabled = true }
        } else null
        val outputMinimum = AudioTrack.getMinBufferSize(
            SampleRate,
            AudioFormat.CHANNEL_OUT_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val track = AudioTrack.Builder()
            .setAudioAttributes(AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                .build())
            .setAudioFormat(AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SampleRate)
                .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                .build())
            .setBufferSizeInBytes(maxOf(outputMinimum, CaptureChunkBytes * 8))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        if (track.state != AudioTrack.STATE_INITIALIZED) {
            recorder.release()
            track.release()
            throw VoiceMediaError("This phone could not open realtime voice playback.")
        }
        val manager = app.getSystemService(Context.AUDIO_SERVICE) as? AudioManager
        previousAudioMode = manager?.mode
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        val focusGranted = if (android.os.Build.VERSION.SDK_INT >= 26) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes)
                .setAcceptsDelayedFocusGain(false)
                .build()
            audioFocusRequest = request
            manager?.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        } else {
            @Suppress("DEPRECATION")
            manager?.requestAudioFocus(null, AudioManager.STREAM_VOICE_CALL, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT) ==
                AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        }
        if (!focusGranted) {
            recorder.release()
            track.release()
            echoCanceler?.release(); echoCanceler = null
            noiseSuppressor?.release(); noiseSuppressor = null
            throw VoiceMediaError("Another app is using audio. End it before starting a voice call.", false)
        }
        manager?.mode = AudioManager.MODE_IN_COMMUNICATION
        capture = recorder
        playback = track
        track.play()
        recorder.startRecording()
    }

    private fun writePlayback(bytes: ByteArray) {
        if (bytes.isEmpty()) return
        responsePending = true
        if (concurrentRequests) onConcurrentEvent?.invoke("audio.output.started", JsonObject(emptyMap()))
        playback?.let { track ->
            val written = track.write(bytes, 0, bytes.size, AudioTrack.WRITE_BLOCKING)
            if (written > 0) playbackFrames += written / 2
        }
    }

    private fun flushPlayback() {
        playback?.let { track ->
            runCatching {
                track.pause()
                track.flush()
                playbackFrames = 0
                track.play()
            }
        }
    }

    @Synchronized
    private fun releaseAudio() {
        if (concurrentRequests) onConcurrentEvent?.invoke("input.cleared", JsonObject(emptyMap()))
        responsePending = false
        playbackFrames = 0
        pushToTalkEngaged = false
        pushToTalkDesired = false
        microphoneMuted = false
        runCatching { capture?.stop() }
        capture?.release()
        capture = null
        echoCanceler?.release(); echoCanceler = null
        noiseSuppressor?.release(); noiseSuppressor = null
        runCatching { playback?.stop() }
        playback?.release()
        playback = null
        (app.getSystemService(Context.AUDIO_SERVICE) as? AudioManager)?.let { manager ->
            if (android.os.Build.VERSION.SDK_INT >= 26) {
                audioFocusRequest?.let(manager::abandonAudioFocusRequest)
            } else {
                @Suppress("DEPRECATION")
                manager.abandonAudioFocus(null)
            }
            audioFocusRequest = null
            previousAudioMode?.let { manager.mode = it }
            previousAudioMode = null
        }
    }

    private suspend fun disconnectSession(id: String) {
        runCatching {
            withTimeout(5_000) {
                client.delete("${base()}/media/sessions/${encoded(id)}?revoke=false") {
                    authorize()
                }
            }
        }
    }

    private fun base(): String {
        val host = MagicianAccess.baseUrl(app)
        if (host.isBlank()) throw VoiceMediaError("No Magician host is configured.", retryable = false)
        return "$host/api/magician/v2"
    }

    private fun controlUrl(id: String): String {
        val base = MagicianAccess.baseUrl(app)
        val socket = when {
            base.startsWith("https://") -> "wss://${base.removePrefix("https://")}"
            base.startsWith("http://") -> "ws://${base.removePrefix("http://")}"
            else -> "wss://$base"
        }
        return "$socket/api/magician/v2/media/voice/${encoded(id)}/control"
    }

    private fun io.ktor.client.request.HttpRequestBuilder.authorize() {
        MagicianAccess.headers(app).forEach { (name, value) -> header(name, value) }
    }

    private fun envelope(kind: String, payload: JsonObject): String = buildJsonObject {
        put("kind", kind)
        put("payload", payload)
    }.toString()

    private fun encoded(value: String): String = URLEncoder.encode(value, Charsets.UTF_8.name())

    fun close() {
        stop()
        scope.cancel()
        client.close()
    }

    companion object {
        const val SampleRate = 24_000
        private const val CaptureChunkBytes = 960 // 20 ms PCM16 mono
        private const val ReadyTimeoutMillis = 45_000L

        private fun mergeStreamingCaption(existing: String, incoming: String): String {
            if (incoming.isEmpty()) return existing
            if (existing.isEmpty()) return incoming
            if (incoming.startsWith(existing)) return incoming
            if (existing.startsWith(incoming)) return existing
            return existing + incoming
        }
    }
}
