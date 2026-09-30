package ai.magicbeans.magdroid.voice

import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerialName
import kotlinx.serialization.json.*

@Serializable
data class VoiceRequest(
    val id: String,
    @SerialName("parent_session_id") val parentSessionId: String,
    @SerialName("branch_session_id") val branchSessionId: String,
    val title: String,
    @SerialName("work_status") val workStatus: String,
    @SerialName("delivery_status") val deliveryStatus: String,
    @SerialName("speech_text") val speechText: String? = null,
    @SerialName("result_message_id") val resultMessageId: String? = null,
    @SerialName("pending_tasks") val pendingTasks: List<String> = emptyList(),
    @SerialName("task_notification") val taskNotification: Boolean = false,
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("updated_at") val updatedAt: Long = 0,
    val error: String? = null,
    @SerialName("read_at") val readAt: Long? = null,
    @SerialName("ui_thread_id") val uiThreadId: String? = null,
    @SerialName("context_session_id") val contextSessionId: String? = null,
) {
    fun visible(selectedId: String?): Boolean = running || deliveryStatus in listOf("claimed", "playing") ||
        (deliveryStatus != "dismissed" && (id == selectedId || (readAt == null && deliveryStatus != "played")))
    val running: Boolean get() = workStatus in listOf("accepted", "running") ||
        (pendingTasks.isNotEmpty() && workStatus != "cancelled")
    val label: String get() = when {
        pendingTasks.isNotEmpty() && workStatus != "cancelled" -> "Task running"
        workStatus == "accepted" -> "Queued"
        workStatus == "running" -> "Working"
        workStatus == "completed" -> if (deliveryStatus == "played") "Answered" else if (readAt != null) "Read" else "Ready"
        else -> workStatus.replaceFirstChar { it.uppercase() }
    }
}

@Serializable
data class VoiceOutput(
    @SerialName("device_id") val deviceId: String,
    @SerialName("interaction_id") val interactionId: String,
    val epoch: Long,
    @SerialName("expires_at") val expiresAt: Long = 0,
)

@Serializable
data class VoiceRequestSnapshot(
    val revision: Long = -1,
    val requests: List<VoiceRequest> = emptyList(),
    val output: VoiceOutput? = null,
)

data class ConcurrentVoiceState(
    val requests: List<VoiceRequest> = emptyList(),
    val focus: VoiceRequest? = null,
    val speaking: String? = null,
    val error: String? = null,
) {
    val available: List<VoiceRequest> get() = requests.filter { it.visible(focus?.id) }.asReversed()
}

/** A single device output lease; accepted work is never cancelled by capture or teardown. */
class ConcurrentVoiceCoordinator(
    private val scope: CoroutineScope,
    private val request: suspend (String, JsonObject?) -> JsonObject,
    private val eligible: () -> Boolean,
    private val outputBusy: () -> Boolean,
    private val play: (String, () -> Unit, (SpeechOutcome) -> Unit) -> Unit,
    private val stopPlayback: () -> Unit,
    private val focusChanged: (VoiceRequest?) -> Unit,
    private val now: () -> Long = System::currentTimeMillis,
    private val automaticPlayback: () -> Boolean = { true },
) {
    private val json = Json { ignoreUnknownKeys = true }
    private val _state = MutableStateFlow(ConcurrentVoiceState())
    val state = _state.asStateFlow()
    private var snapshot = VoiceRequestSnapshot()
    private val deviceId = UUID.randomUUID().toString()
    private var interactionId = UUID.randomUUID().toString()
    private var epoch = 0L
    private var captureFocus: VoiceRequest? = null
    private var capturing = false
    private var inputPending = false
    private var quietAfter = 0L
    private var active = false
    private var generation = 0L
    private var tickBusy = false
    private var lastLease = 0L
    private val replays = mutableListOf<String>()
    private var monitor: Job? = null
    private data class Playing(val row: VoiceRequest, val attempt: String, val output: Long, val focusEpoch: Long, var started: Boolean = false)
    private var playing: Playing? = null

    fun start() {
        if (monitor != null) return
        monitor = scope.launch { while (isActive) { tick(); delay(1_000) } }
    }
    fun activate() { active = true }
    fun select(row: VoiceRequest?) {
        epoch++
        _state.value = _state.value.copy(focus = row)
        if (!capturing && !inputPending) captureFocus = row
        focusChanged(row)
    }
    fun captureStarted() {
        activate(); captureFocus = _state.value.focus; epoch++; capturing = true; inputPending = true
        if (playing != null) stopPlayback()
    }
    fun captureStopped() { capturing = false }
    fun inputSettled() { inputPending = false; captureFocus = _state.value.focus; quietAfter = now() + 500 }
    fun foregroundStarted() { epoch++; if (playing != null) stopPlayback() }
    fun foregroundStopped() { quietAfter = now() + 500 }
    fun replay(id: String) { activate(); if (id !in replays && playing?.row?.id != id) replays.add(id) }
    fun target(parent: String): Pair<String, String?> = captureFocus?.let { it.parentSessionId to it.branchSessionId } ?: (parent to null)
    fun reset() {
        generation++; active = false; epoch++; stopPlayback(); playing = null
        snapshot = VoiceRequestSnapshot(); _state.value = ConcurrentVoiceState(); captureFocus = null
        capturing = false; inputPending = false; replays.clear(); lastLease = 0
        interactionId = UUID.randomUUID().toString()
        focusChanged(null)
    }
    fun deactivate() {
        active = false; epoch++; capturing = false; inputPending = false
        if (playing != null) stopPlayback()
        snapshot.output?.takeIf { it.deviceId == deviceId && it.interactionId == interactionId }?.let {
            scope.launch { runCatching { command(fields("action" to "release") + identity(it.epoch)) } }
        }
    }
    fun close() { deactivate(); monitor?.cancel(); monitor = null }
    suspend fun cancel(id: String) { update(decode(request("/media/voice/requests/$id/cancel", fields()))) }
    suspend fun dismiss(id: String) { command(fields("action" to "dismiss", "request_id" to id)) }
    suspend fun markRead(id: String) { command(fields("action" to "read", "request_id" to id)) }
    suspend fun result(id: String): String {
        val content = request("/media/voice/requests/$id/result", null)["content"]?.jsonObject
        return content?.get("text")?.jsonPrimitive?.contentOrNull
            ?: content?.get("summary")?.jsonPrimitive?.contentOrNull ?: "Open Review work for the complete result."
    }
    fun action(block: suspend () -> Unit) { scope.launch { try { block() } catch (e: Exception) { report(e) } } }
    fun report(error: Exception) { _state.value = _state.value.copy(error = error.message ?: "Voice requests are unavailable.") }

    suspend fun submit(parent: String, text: String, options: JsonObject, voiceInput: Boolean = false): VoiceRequest? {
        val target = if (voiceInput) target(parent) else parent to null; activate()
        try {
            val cancelCommand = text.trim().trimEnd('.', '!', '?').lowercase().removePrefix("please ")
            val current = cancelCommand in listOf("cancel that request", "cancel this request", "cancel the current request")
            if (current || cancelCommand == "cancel the previous request" || cancelCommand == "cancel all background requests") {
                update(decode(request("/media/voice/requests", null)))
                val all = cancelCommand == "cancel all background requests"
                var rows = snapshot.requests.filter { !it.taskNotification && it.running &&
                    (all || if (current && target.second != null) it.branchSessionId == target.second else it.parentSessionId == target.first) }.sortedByDescending { it.createdAt }
                if (!all) rows = rows.take(1)
                check(rows.isNotEmpty()) { "No matching background request is running." }
                rows.forEach { cancel(it.id) }; return rows.first()
            }
            val body = JsonObject(options + fields("text" to text, "submission_id" to UUID.randomUUID().toString()) +
                (target.second?.let { fields("context_session_id" to it) } ?: fields()))
            val path = "/chat/sessions/${target.first}/voice/requests"
            val response = try { request(path, body) } catch (e: java.io.IOException) { request(path, body) }
            return json.decodeFromJsonElement<VoiceRequest>(response)
        } finally { if (voiceInput) inputSettled() }
    }
    private fun safe(): Boolean = active && !capturing && !inputPending && eligible() &&
        (automaticPlayback() || replays.isNotEmpty()) && !outputBusy() && now() >= quietAfter
    private fun decode(value: JsonObject): VoiceRequestSnapshot = json.decodeFromJsonElement(value)
    private fun update(value: VoiceRequestSnapshot) {
        if (value.revision < snapshot.revision) return
        snapshot = value
        val prior = _state.value.focus
        val current = value.requests.find { it.id == prior?.id && it.deliveryStatus != "dismissed" }
        _state.value = _state.value.copy(requests = value.requests, focus = current, error = null)
        if (prior != null && current == null) select(null)
    }
    private fun identity(output: Long) = fields("device_id" to deviceId, "interaction_id" to interactionId, "epoch" to output)
    private suspend fun command(body: Map<String, JsonElement>) {
        val gen = generation; val value = decode(request("/media/voice/delivery", JsonObject(body)))
        if (gen == generation) update(value)
    }
    private suspend fun receipt(p: Playing, event: String) = command(identity(p.output) + fields("action" to "playback", "request_id" to p.row.id, "attempt_id" to p.attempt, "event" to event))
    suspend fun tick() {
        if (tickBusy) return
        tickBusy = true; val gen = generation
        try {
            val value = decode(request("/media/voice/requests", null)); if (gen != generation) return
            update(value)
            if (!active) return
            val owner = snapshot.output
            if (playing == null && owner != null && owner.expiresAt > now() &&
                (owner.deviceId != deviceId || owner.interactionId != interactionId)) return
            if ((playing != null || safe()) && (now() - lastLease > 8_000 || snapshot.output == null)) {
                command(fields("action" to "acquire", "device_id" to deviceId, "interaction_id" to interactionId)); lastLease = now()
            }
            if (gen != generation) return
            playing?.let { receipt(it, "progress"); return }
            if (!safe()) return
            val replay = replays.firstOrNull()
            val row = if (replay != null) snapshot.requests.find { it.id == replay && it.speechText != null }
                else snapshot.requests.filter { it.deliveryStatus == "pending" && it.readAt == null && it.speechText != null }.minByOrNull { it.updatedAt }
            if (row == null) { if (replay != null) replays.remove(replay); return }
            val output = snapshot.output ?: return
            val p = Playing(row, UUID.randomUUID().toString(), output.epoch, epoch)
            command(identity(output.epoch) + fields("action" to "claim", "request_id" to row.id, "attempt_id" to p.attempt, "focus_epoch" to epoch, "replay" to (replay == row.id)))
            if (gen != generation) return
            if (!safe() || p.focusEpoch != epoch) { receipt(p, "rejected"); return }
            replays.remove(row.id)
            playing = p
            var startedReceipt: Job? = null
            play(row.speechText.orEmpty(), {
                if (!active || capturing || inputPending || epoch != p.focusEpoch || gen != generation) stopPlayback()
                else if (!p.started) {
                    p.started = true; captureFocus = row
                    _state.value = _state.value.copy(focus = row, speaking = row.id); focusChanged(row)
                    startedReceipt = scope.launch { try { receipt(p, "started") } catch (e: Exception) { stopPlayback(); report(e) } }
                }
            }, { outcome -> scope.launch {
                try {
                    startedReceipt?.join()
                    if (gen == generation) receipt(p, if (outcome == SpeechOutcome.Completed && p.started) "completed" else "interrupted")
                } catch (e: Exception) { report(e) }
                finally { if (playing === p) { playing = null; _state.value = _state.value.copy(speaking = null); quietAfter = now() + 500 } }
            } })
        } catch (e: Exception) { if (playing != null) stopPlayback(); if (gen == generation) report(e) }
        finally { tickBusy = false }
    }

    companion object {
        fun fields(vararg values: Pair<String, Any>): JsonObject = buildJsonObject {
            values.forEach { (key, value) -> when (value) {
                is Boolean -> put(key, value)
                is Number -> put(key, value)
                else -> put(key, value.toString())
            } }
        }
    }
}
